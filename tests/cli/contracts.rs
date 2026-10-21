use super::harness::*;
use serde_json::{json, Value};

struct Capture {
    output: std::process::Output,
    requests: Vec<Value>,
}

fn capture(args: &[&str], response: Value) -> Capture {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket = base.join("contract.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(args)
        .env("HERDR_SOCKET_PATH", &socket)
        .env("XDG_CONFIG_HOME", &base)
        .env("XDG_RUNTIME_DIR", &base)
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_SESSION")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut requests = Vec::new();
    loop {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            drop(listener);
            cleanup_test_base(&base);
            panic!("CLI timed out: {args:?}; output: {output:?}; requests: {requests:?}");
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                // BSD-derived platforms inherit the listener's non-blocking flag.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                if request["method"] == "ping" {
                    write_fake_pong(&mut stream, &request, "contract-baseline", CURRENT_PROTOCOL);
                } else {
                    writeln!(stream, "{response}").unwrap();
                    stream.flush().unwrap();
                }
                requests.push(request);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("contract listener failed: {error}"),
        }
    }
    let output = child.wait_with_output().unwrap();
    drop(listener);
    cleanup_test_base(&base);
    Capture { output, requests }
}

fn assert_request(args: &[&str], expected: Value) {
    let response = json!({"id": expected["id"], "result": {"type": "ok"}});
    let captured = capture(args, response.clone());
    assert_eq!(
        captured.output.status.code(),
        Some(0),
        "{args:?}: {:?}",
        captured.output
    );
    assert!(
        captured.output.stderr.is_empty(),
        "{args:?}: {:?}",
        captured.output
    );
    assert_eq!(
        captured.requests.len(),
        2,
        "{args:?}: {:?}",
        captured.requests
    );
    assert_eq!(captured.requests[0]["method"], "ping", "{args:?}");
    assert_eq!(captured.requests[1], expected, "{args:?}");
    assert_eq!(
        captured.output.stdout,
        format!("{response}\n").as_bytes(),
        "{args:?}"
    );
}

#[test]
fn workspace_cli_request_contract_matrix() {
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec!["workspace", "list"],
            json!({"id":"cli:workspace:list","method":"workspace.list","params":{}}),
        ),
        (
            vec!["workspace", "get", "opaque:w-7:p.2"],
            json!({"id":"cli:workspace:get","method":"workspace.get","params":{"workspace_id":"opaque:w-7:p.2"}}),
        ),
        (
            vec!["workspace", "focus", "007"],
            json!({"id":"cli:workspace:focus","method":"workspace.focus","params":{"workspace_id":"007"}}),
        ),
        (
            vec!["workspace", "rename", "w:7", "two", "words", "--literal"],
            json!({"id":"cli:workspace:rename","method":"workspace.rename","params":{"workspace_id":"w:7","label":"two words --literal"}}),
        ),
        (
            vec!["workspace", "rename", "w:7", ""],
            json!({"id":"cli:workspace:rename","method":"workspace.rename","params":{"workspace_id":"w:7","label":""}}),
        ),
        (
            vec!["workspace", "create"],
            json!({"id":"cli:workspace:create","method":"workspace.create","params":{"focus":false}}),
        ),
        (
            vec!["workspace", "create", "--focus", "--no-focus", "--focus"],
            json!({"id":"cli:workspace:create","method":"workspace.create","params":{"focus":true}}),
        ),
        (
            vec!["workspace", "create", "--no-focus", "--focus", "--no-focus"],
            json!({"id":"cli:workspace:create","method":"workspace.create","params":{"focus":false}}),
        ),
        (
            vec![
                "workspace",
                "create",
                "--cwd",
                "relative/first",
                "--cwd",
                "../last",
                "--label",
                "first",
                "--label",
                "--dash-value",
                "--env",
                "KEY=first",
                "--env",
                "KEY=last=value",
                "--env",
                "EMPTY=",
            ],
            json!({"id":"cli:workspace:create","method":"workspace.create","params":{"cwd":"../last","label":"--dash-value","focus":false,"env":{"KEY":"last=value","EMPTY":""}}}),
        ),
        (
            vec!["workspace", "close", "w:7"],
            json!({"id":"cli:workspace:close","method":"workspace.close","params":{"workspace_id":"w:7"}}),
        ),
        (
            vec!["workspace", "close", "w:7", "--group"],
            json!({"id":"cli:workspace:close","method":"workspace.close","params":{"workspace_id":"w:7","close_group":true}}),
        ),
        (
            vec!["workspace", "get", "--literal"],
            json!({"id":"cli:workspace:get","method":"workspace.get","params":{"workspace_id":"--literal"}}),
        ),
    ];
    for (args, request) in cases {
        assert_request(&args, request);
    }
}

#[test]
fn workspace_metadata_presence_and_order_contract() {
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec![
                "workspace",
                "report-metadata",
                "w:7",
                "--source",
                "first",
                "--source",
                "user:final",
                "--token",
                "set=a=b",
                "--token",
                "empty=",
                "--clear-token",
                "cleared",
                "--seq",
                "1",
                "--seq",
                "0",
                "--ttl-ms",
                "5",
                "--ttl-ms",
                "0",
            ],
            json!({"id":"cli:request","method":"workspace.report_metadata","params":{"workspace_id":"w:7","source":"user:final","tokens":{"set":"a=b","empty":"","cleared":null},"seq":0,"ttl_ms":0}}),
        ),
        (
            vec![
                "workspace",
                "report-metadata",
                "w:7",
                "--source",
                "user",
                "--token",
                "key=first",
                "--clear-token",
                "key",
            ],
            json!({"id":"cli:request","method":"workspace.report_metadata","params":{"workspace_id":"w:7","source":"user","tokens":{"key":null}}}),
        ),
        (
            vec![
                "workspace",
                "report-metadata",
                "w:7",
                "--source",
                "user",
                "--clear-token",
                "key",
                "--token",
                "key=",
            ],
            json!({"id":"cli:request","method":"workspace.report_metadata","params":{"workspace_id":"w:7","source":"user","tokens":{"key":""}}}),
        ),
    ];
    for (args, expected) in cases {
        let captured = capture(&args, json!({"id":"cli:request","result":{"type":"ok"}}));
        assert_eq!(
            captured.output.status.code(),
            Some(0),
            "{args:?}: {:?}",
            captured.output
        );
        assert!(captured.output.stdout.is_empty(), "{args:?}");
        assert!(captured.output.stderr.is_empty(), "{args:?}");
        assert_eq!(captured.requests.len(), 2, "{args:?}");
        assert_eq!(captured.requests[0]["method"], "ping");
        assert_eq!(captured.requests[1], expected, "{args:?}");
    }
}

#[test]
fn workspace_legacy_rejections_do_not_connect() {
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["workspace", "list", "extra"],
            "error: unexpected argument 'extra' found",
        ),
        (vec!["workspace", "get"], "<workspace_id>"),
        (
            vec!["workspace", "get", "w1", "extra"],
            "error: unexpected argument 'extra' found",
        ),
        (vec!["workspace", "focus"], "<workspace_id>"),
        (vec!["workspace", "rename", "w1"], "<LABEL>"),
        (
            vec!["workspace", "create", "--label"],
            "error: a value is required for '--label <TEXT>' but none was supplied",
        ),
        (
            vec!["workspace", "create", "--cwd"],
            "error: a value is required for '--cwd <PATH>' but none was supplied",
        ),
        (
            vec!["workspace", "create", "--env"],
            "error: a value is required for '--env <KEY=VALUE>' but none was supplied",
        ),
        (
            vec!["workspace", "create", "--env", "=value"],
            "env key must not be empty\n",
        ),
        (
            vec!["workspace", "create", "--env", "KEY"],
            "env must use KEY=VALUE\n",
        ),
        (
            vec!["workspace", "create", "--focus=true"],
            "error: unexpected value 'true' for '--focus'",
        ),
        (
            vec!["workspace", "create", "--unknown"],
            "error: unexpected argument '--unknown' found",
        ),
        (
            vec!["workspace", "close", "w1", "extra"],
            "error: unexpected argument 'extra' found",
        ),
        (
            vec!["workspace", "report-metadata", "w1", "--source", "user"],
            "missing token to set or clear\n",
        ),
        (
            vec!["workspace", "report-metadata", "w1", "--token", "key=value"],
            "--source <ID>",
        ),
        (
            vec![
                "workspace",
                "report-metadata",
                "w1",
                "--source",
                "user",
                "--token",
                "=value",
            ],
            "token name must not be empty\n",
        ),
        (
            vec![
                "workspace",
                "report-metadata",
                "w1",
                "--source",
                "user",
                "--token",
                "key",
            ],
            "token must use NAME=VALUE\n",
        ),
        (
            vec![
                "workspace",
                "report-metadata",
                "w1",
                "--source",
                "user",
                "--token",
            ],
            "error: a value is required for '--token <NAME=VALUE>' but none was supplied",
        ),
    ];
    for (args, stderr) in cases {
        let captured = capture(&args, json!({"id":"unexpected","result":{"type":"ok"}}));
        assert_eq!(
            captured.output.status.code(),
            Some(2),
            "{args:?}: {:?}",
            captured.output
        );
        assert!(captured.output.stdout.is_empty(), "{args:?}");
        if stderr.ends_with('\n') {
            assert_eq!(captured.output.stderr, stderr.as_bytes(), "{args:?}");
        } else {
            let diagnostic = String::from_utf8_lossy(&captured.output.stderr);
            assert!(diagnostic.contains(stderr), "{args:?}: {diagnostic}");
            if !stderr.starts_with("error:") {
                assert!(
                    diagnostic.contains("required arguments were not provided"),
                    "{args:?}: {diagnostic}"
                );
            }
        }
        assert!(
            captured.requests.is_empty(),
            "{args:?}: {:?}",
            captured.requests
        );
    }
}

#[test]
fn workspace_normalized_syntax_request_contract() {
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec!["workspace", "create", "--label=value"],
            json!({"id":"cli:workspace:create","method":"workspace.create","params":{"label":"value","focus":false}}),
        ),
        (
            vec!["workspace", "create", "--cwd=path"],
            json!({"id":"cli:workspace:create","method":"workspace.create","params":{"cwd":"path","focus":false}}),
        ),
        (
            vec!["workspace", "create", "--env=KEY=value"],
            json!({"id":"cli:workspace:create","method":"workspace.create","params":{"env":{"KEY":"value"},"focus":false}}),
        ),
        (
            vec!["workspace", "close", "--group", "w1"],
            json!({"id":"cli:workspace:close","method":"workspace.close","params":{"workspace_id":"w1","close_group":true}}),
        ),
        (
            vec!["workspace", "close", "w1", "--group", "--group"],
            json!({"id":"cli:workspace:close","method":"workspace.close","params":{"workspace_id":"w1","close_group":true}}),
        ),
    ];
    for (args, expected) in cases {
        assert_request(&args, expected);
    }
    for args in [
        vec![
            "workspace",
            "report-metadata",
            "--source",
            "user",
            "w1",
            "--token",
            "key=value",
        ],
        vec![
            "workspace",
            "report-metadata",
            "w1",
            "--source=user",
            "--token",
            "key=value",
        ],
    ] {
        let captured = capture(&args, json!({"id":"cli:request","result":{"type":"ok"}}));
        assert_eq!(
            captured.output.status.code(),
            Some(0),
            "{args:?}: {:?}",
            captured.output
        );
        assert!(captured.output.stdout.is_empty(), "{args:?}");
        assert!(captured.output.stderr.is_empty(), "{args:?}");
        assert_eq!(captured.requests.len(), 2, "{args:?}");
        assert_eq!(captured.requests[0]["method"], "ping", "{args:?}");
        assert_eq!(
            captured.requests[1],
            json!({"id":"cli:request","method":"workspace.report_metadata","params":{"workspace_id":"w1","source":"user","tokens":{"key":"value"}}}),
            "{args:?}"
        );
    }
}

#[test]
fn workspace_help_preserves_public_and_runtime_grammar_split() {
    let cases = [
        ("list", vec![]),
        ("get", vec!["<workspace_id>"]),
        ("focus", vec!["<workspace_id>"]),
        ("rename", vec!["<WORKSPACE_ID>", "<LABEL>..."]),
        (
            "create",
            vec![
                "--cwd <PATH>",
                "--label <TEXT>",
                "--env <KEY=VALUE>",
                "--focus",
                "--no-focus",
            ],
        ),
        ("close", vec!["<workspace_id>"]),
        (
            "report-metadata",
            vec![
                "<WORKSPACE_ID>",
                "--source <ID>",
                "--token <NAME=VALUE>",
                "--clear-token <NAME>",
                "--seq <N>",
                "--ttl-ms <N>",
            ],
        ),
    ];
    for (leaf, fragments) in cases {
        let captured = capture(&["workspace", leaf, "--help"], json!({}));
        assert_eq!(
            captured.output.status.code(),
            Some(0),
            "{leaf}: {:?}",
            captured.output
        );
        assert!(captured.requests.is_empty(), "{leaf}");
        assert!(captured.output.stderr.is_empty(), "{leaf}");
        let help = String::from_utf8(captured.output.stdout).unwrap();
        assert!(help.contains(&format!("herdr workspace {leaf}")), "{help}");
        for fragment in fragments {
            assert!(help.contains(fragment), "missing {fragment:?}: {help}");
        }
        if leaf == "close" {
            assert!(!help.contains("--group"), "{help}");
        }
    }
}

#[test]
fn workspace_json_output_and_error_envelope_contract() {
    for response in [
        json!({"id":"cli:workspace:get","result":{"type":"workspace","custom":"preserve","workspace":{"workspace_id":"w:7"}}}),
        json!({"id":"server:literal-id","error":{"code":"literal_error","message":"exact message"}}),
    ] {
        let captured = capture(&["workspace", "get", "w:7"], response.clone());
        let is_error = response.get("error").is_some();
        assert_eq!(
            captured.output.status.code(),
            Some(if is_error { 1 } else { 0 })
        );
        let expected = format!("{response}\n");
        if is_error {
            assert!(captured.output.stdout.is_empty());
            assert_eq!(captured.output.stderr, expected.as_bytes());
        } else {
            assert_eq!(captured.output.stdout, expected.as_bytes());
            assert!(captured.output.stderr.is_empty());
        }
        assert_eq!(captured.requests.len(), 2);
        assert_eq!(
            captured.requests[1],
            json!({"id":"cli:workspace:get","method":"workspace.get","params":{"workspace_id":"w:7"}})
        );
    }
}

#[test]
fn pane_and_agent_read_distinct_ansi_contract() {
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec!["pane", "read", "p:7"],
            json!({"id":"cli:pane:read","method":"pane.read","params":{"pane_id":"p:7","source":"recent","format":"text","strip_ansi":true}}),
        ),
        (
            vec!["pane", "read", "p:7", "--ansi"],
            json!({"id":"cli:pane:read","method":"pane.read","params":{"pane_id":"p:7","source":"recent","format":"ansi","strip_ansi":true}}),
        ),
        (
            vec!["pane", "read", "p:7", "--format", "ansi"],
            json!({"id":"cli:pane:read","method":"pane.read","params":{"pane_id":"p:7","source":"recent","format":"ansi","strip_ansi":true}}),
        ),
        (
            vec!["pane", "read", "p:7", "--raw", "--format", "text"],
            json!({"id":"cli:pane:read","method":"pane.read","params":{"pane_id":"p:7","source":"recent","format":"text","strip_ansi":false}}),
        ),
        (
            vec![
                "pane",
                "read",
                "--source=recent_unwrapped",
                "--lines=0",
                "p:7",
                "--raw",
            ],
            json!({"id":"cli:pane:read","method":"pane.read","params":{"pane_id":"p:7","source":"recent_unwrapped","lines":0,"format":"ansi","strip_ansi":false}}),
        ),
        (
            vec!["agent", "read", "named-agent"],
            json!({"id":"cli:agent:read","method":"agent.read","params":{"target":"named-agent","source":"recent","format":"text","strip_ansi":true}}),
        ),
        (
            vec!["agent", "read", "named-agent", "--ansi"],
            json!({"id":"cli:agent:read","method":"agent.read","params":{"target":"named-agent","source":"recent","format":"ansi","strip_ansi":false}}),
        ),
        (
            vec![
                "agent",
                "read",
                "named-agent",
                "--format",
                "ansi",
                "--format",
                "text",
            ],
            json!({"id":"cli:agent:read","method":"agent.read","params":{"target":"named-agent","source":"recent","format":"text","strip_ansi":true}}),
        ),
    ];
    for (args, expected) in cases {
        let text = "\u{1b}[31mexact text\u{1b}[0m";
        let captured = capture(
            &args,
            json!({"id":expected["id"],"result":{"type":"read","read":{"text":text}}}),
        );
        assert_eq!(
            captured.output.status.code(),
            Some(0),
            "{args:?}: {:?}",
            captured.output
        );
        assert_eq!(captured.output.stdout, text.as_bytes(), "{args:?}");
        assert!(captured.output.stderr.is_empty(), "{args:?}");
        assert_eq!(captured.requests.len(), 2, "{args:?}");
        assert_eq!(captured.requests[1], expected, "{args:?}");
    }
}

#[test]
fn plugin_popup_dimensions_and_log_alias_contract() {
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec![
                "plugin",
                "pane",
                "open",
                "--plugin",
                "plugin:7",
                "--entrypoint",
                "main",
            ],
            json!({"id":"cli:plugin","method":"plugin.pane.open","params":{"plugin_id":"plugin:7","entrypoint":"main","focus":true}}),
        ),
        (
            vec![
                "plugin",
                "pane",
                "open",
                "--plugin",
                "plugin:7",
                "--entrypoint",
                "main",
                "--placement",
                "popup",
                "--width",
                "80%",
                "--height",
                "24",
                "--no-focus",
            ],
            json!({"id":"cli:plugin","method":"plugin.pane.open","params":{"plugin_id":"plugin:7","entrypoint":"main","placement":"popup","width":"80%","height":24,"focus":false}}),
        ),
        (
            vec![
                "plugin",
                "pane",
                "open",
                "--plugin",
                "plugin:7",
                "--entrypoint",
                "main",
                "--placement",
                "fullscreen",
                "--width",
                "0",
                "--height",
                "100%",
                "--no-focus",
                "--focus",
            ],
            json!({"id":"cli:plugin","method":"plugin.pane.open","params":{"plugin_id":"plugin:7","entrypoint":"main","placement":"zoomed","width":0,"height":"100%","focus":true}}),
        ),
        (
            vec!["plugin", "log"],
            json!({"id":"cli:plugin","method":"plugin.log.list","params":{}}),
        ),
        (
            vec![
                "plugin", "logs", "list", "--plugin", "plugin:7", "--limit", "0",
            ],
            json!({"id":"cli:plugin","method":"plugin.log.list","params":{"plugin_id":"plugin:7","limit":0}}),
        ),
        (
            vec!["plugin", "log", "list", "--limit", "2", "--limit", "7"],
            json!({"id":"cli:plugin","method":"plugin.log.list","params":{"limit":7}}),
        ),
    ];
    for (args, expected) in cases {
        assert_request(&args, expected);
    }
}

#[test]
fn agent_prompt_missing_text_rejects_before_connecting() {
    let captured = capture(
        &["agent", "prompt", "main"],
        json!({"id":"unexpected","result":{"type":"ok"}}),
    );
    assert_eq!(captured.output.status.code(), Some(2));
    assert!(captured.output.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&captured.output.stderr);
    assert!(
        diagnostic.contains("required arguments were not provided"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("<TEXT>"), "{diagnostic}");
    assert!(captured.requests.is_empty(), "{:?}", captured.requests);
}

#[test]
fn agent_prompt_nested_wait_presence_contract() {
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec!["agent", "prompt", "named-agent", "literal - text"],
            json!({"id":"cli:agent:prompt","method":"agent.prompt","params":{"target":"named-agent","text":"literal - text"}}),
        ),
        (
            vec!["agent", "prompt", "named-agent", "literal - text", "--wait"],
            json!({"id":"cli:agent:prompt","method":"agent.prompt","params":{"target":"named-agent","text":"literal - text","wait":{}}}),
        ),
        (
            vec![
                "agent",
                "prompt",
                "named-agent",
                "literal - text",
                "--wait",
                "--until",
                "idle",
                "--until",
                "done",
                "--until",
                "idle",
                "--timeout",
                "0",
            ],
            json!({"id":"cli:agent:prompt","method":"agent.prompt","params":{"target":"named-agent","text":"literal - text","wait":{"until":["idle","done","idle"],"timeout_ms":0}}}),
        ),
    ];
    for (args, expected) in cases {
        assert_request(&args, expected);
    }
}

#[test]
fn text_operands_keep_flag_shaped_words_like_the_legacy_cli() {
    for (args, expected) in [
        (
            vec!["agent", "prompt", "main", "--wait"],
            json!({"id":"cli:agent:prompt","method":"agent.prompt","params":{"target":"main","text":"--wait"}}),
        ),
        (
            vec!["agent", "prompt", "-agent", "--wait"],
            json!({"id":"cli:agent:prompt","method":"agent.prompt","params":{"target":"-agent","text":"--wait"}}),
        ),
        (
            vec!["pane", "send-text", "p1", "--help", "more"],
            json!({"id":"cli:request","method":"pane.send_text","params":{"pane_id":"p1","text":"--help more"}}),
        ),
        (
            vec!["pane", "run", "p1", "ls", "-h"],
            json!({"id":"cli:request","method":"pane.send_input","params":{"pane_id":"p1","text":"ls -h","keys":["Enter"]}}),
        ),
        (
            vec!["pane", "send-keys", "p1", "-h"],
            json!({"id":"cli:request","method":"pane.send_keys","params":{"pane_id":"p1","keys":["-h"]}}),
        ),
        (
            vec!["pane", "rename", "p1", "--clear", "x"],
            json!({"id":"cli:pane:rename","method":"pane.rename","params":{"pane_id":"p1","label":"--clear x"}}),
        ),
        (
            vec!["pane", "rename", "p1", "--clear"],
            json!({"id":"cli:pane:rename","method":"pane.rename","params":{"pane_id":"p1"}}),
        ),
        (
            vec!["tab", "rename", "t1", "--help", "x"],
            json!({"id":"cli:tab:rename","method":"tab.rename","params":{"tab_id":"t1","label":"--help x"}}),
        ),
        (
            vec!["agent", "rename", "main", "--help"],
            json!({"id":"cli:agent:rename","method":"agent.rename","params":{"target":"main","name":"--help"}}),
        ),
        (
            vec!["agent", "rename", "main", "--clear"],
            json!({"id":"cli:agent:rename","method":"agent.rename","params":{"target":"main"}}),
        ),
        (
            vec!["notification", "show", "--body"],
            json!({"id":"cli:notification:show","method":"notification.show","params":{"title":"--body"}}),
        ),
    ] {
        let captured = capture(
            &args,
            json!({"id": expected["id"], "result": {"type": "ok"}}),
        );
        assert_eq!(
            captured.output.status.code(),
            Some(0),
            "{args:?}: {:?}",
            captured.output
        );
        assert_eq!(captured.requests.get(1), Some(&expected), "{args:?}");
    }
}
