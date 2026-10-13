use super::bindings::{self, Operation};
use crate::api::schema::{Method, Request};
use crate::cli::projection::{self, ParseError};

fn parse(args: &[&str]) -> Result<Operation, ParseError> {
    projection::parse(
        super::command(),
        &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        bindings::from_matches,
    )
}

fn params(args: &[&str]) -> serde_json::Value {
    let method = match parse(args).unwrap() {
        Operation::Get(params) => Method::AgentGet(params),
        Operation::Focus(params) => Method::AgentFocus(params),
        Operation::Read(params) => Method::AgentRead(params),
        Operation::SendKeys(params) => Method::AgentSendKeys(params),
        Operation::Rename(params) => Method::AgentRename(params),
        Operation::Wait(params) => Method::AgentWait(params),
        Operation::Prompt(params) => Method::AgentPrompt(params),
        Operation::Start(params) => Method::AgentStart(params),
        _ => panic!("expected API params"),
    };
    serde_json::to_value(Request {
        id: "test".into(),
        method,
    })
    .unwrap()["params"]
        .clone()
}

#[test]
fn agent_projection_defaults_and_ordered_read_policy() {
    for (args, expected) in [
        (
            vec!["read", "opaque"],
            serde_json::json!({"target":"opaque", "source":"recent", "format":"text", "strip_ansi":true}),
        ),
        (
            vec!["read", "opaque", "--format", "ansi"],
            serde_json::json!({"target":"opaque", "source":"recent", "format":"ansi", "strip_ansi":false}),
        ),
        (
            vec!["read", "opaque", "--ansi", "--format", "text"],
            serde_json::json!({"target":"opaque", "source":"recent", "format":"text", "strip_ansi":true}),
        ),
        (
            vec!["read", "opaque", "--format", "text", "--ansi"],
            serde_json::json!({"target":"opaque", "source":"recent", "format":"ansi", "strip_ansi":false}),
        ),
        (
            vec!["read", "opaque", "--source=recent_unwrapped", "--lines=0"],
            serde_json::json!({"target":"opaque", "source":"recent_unwrapped", "lines":0, "format":"text", "strip_ansi":true}),
        ),
    ] {
        assert_eq!(params(&args), expected, "{args:?}");
    }
    assert_eq!(
        params(&["read", "--format=ansi", "opaque"]),
        serde_json::json!({"target":"opaque", "source":"recent", "format":"ansi", "strip_ansi":false})
    );
    assert_eq!(
        params(&["wait", "opaque"]),
        serde_json::json!({"target":"opaque"})
    );
}

#[test]
fn agent_projection_nested_wait_omission_and_repeated_statuses() {
    for text in ["--wait", "--until", "--timeout", "--help", ""] {
        assert_eq!(
            params(&["prompt", "opaque", text]),
            serde_json::json!({"target":"opaque", "text":text})
        );
        assert_eq!(
            params(&["prompt", "opaque", text, "--wait"]),
            serde_json::json!({"target":"opaque", "text":text, "wait":{}})
        );
    }
    assert_eq!(
        params(&["prompt", "opaque", "exact text"]),
        serde_json::json!({"target":"opaque", "text":"exact text"})
    );
    assert_eq!(
        params(&["prompt", "opaque", "exact text", "--wait"]),
        serde_json::json!({"target":"opaque", "text":"exact text", "wait":{}})
    );
    assert_eq!(
        params(&[
            "prompt",
            "opaque",
            "exact text",
            "--until",
            "idle",
            "--until",
            "idle",
            "--until",
            "blocked",
            "--timeout=0",
            "--wait"
        ]),
        serde_json::json!({"target":"opaque", "text":"exact text", "wait":{"until":["idle","idle","blocked"], "timeout_ms":0}})
    );
    assert_eq!(
        params(&[
            "wait",
            "opaque",
            "--until",
            "done",
            "--until",
            "done",
            "--timeout",
            "10",
            "--timeout",
            "0"
        ]),
        serde_json::json!({"target":"opaque", "until":["done","done"], "timeout_ms":0})
    );
}

#[test]
fn agent_projection_raw_vectors_and_start_defaults() {
    assert_eq!(
        params(&["send-keys", "opaque", "esc", "--literal", "two words", ""]),
        serde_json::json!({"target":"opaque", "keys":["esc","--literal","two words",""]})
    );
    assert_eq!(
        params(&["start", "name", "--kind", "codex", "--pane", "1"]),
        serde_json::json!({"name":"name", "kind":"codex", "pane_id":"1"})
    );
    assert_eq!(
        params(&["start", "name", "--kind=claude-code", "--pane=1"]),
        serde_json::json!({"name":"name", "kind":"claude-code", "pane_id":"1"})
    );
    assert_eq!(
        params(&[
            "start",
            "name",
            "--kind=codex",
            "--pane=1",
            "--timeout=0",
            "--",
            "--help",
            "two words",
            "",
            "--"
        ]),
        serde_json::json!({"name":"name", "kind":"codex", "pane_id":"1", "timeout_ms":0, "args":["--help","two words","","--"]})
    );
}

#[test]
fn agent_projection_rename_and_local_adapters() {
    assert_eq!(
        params(&["rename", "opaque", "name"]),
        serde_json::json!({"target":"opaque", "name":"name"})
    );
    assert_eq!(
        params(&["rename", "opaque", "--clear"]),
        serde_json::json!({"target":"opaque"})
    );
    let Operation::Attach(params) = parse(&["attach", "opaque", "--takeover"]).unwrap() else {
        panic!("attach")
    };
    assert_eq!(params.target, "opaque");
    assert!(params.takeover);
    let Operation::Explain(params) = parse(&[
        "explain",
        "--file",
        "file",
        "--agent",
        "codex",
        "--json",
        "--format=text",
        "-v",
    ])
    .unwrap() else {
        panic!("explain")
    };
    assert_eq!(params.file.as_deref(), Some("file"));
    assert_eq!(params.agent.as_deref(), Some("codex"));
    assert!(!params.json);
    assert!(params.verbose);
}

#[test]
fn agent_projection_clap_relations_and_help_before_transport() {
    super::command().debug_assert();
    for args in [
        vec!["get"],
        vec!["rename", "opaque"],
        vec!["rename", "opaque", "name", "--clear"],
        vec!["prompt", "opaque", "text", "--until=idle"],
        vec!["prompt", "opaque", "text", "--timeout=0"],
        vec!["wait", "opaque", "--timeout=bad"],
        vec!["wait", "opaque", "--until=invalid"],
        vec!["start", "name", "--kind=codex"],
        vec!["explain", "--file=file"],
        vec!["explain", "opaque", "--agent=codex"],
    ] {
        assert!(
            matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2),
            "{args:?}"
        );
    }
    for leaf in super::command().get_subcommands() {
        assert!(
            matches!(parse(&[leaf.get_name(), "--help"]), Err(ParseError::Clap(error)) if error.exit_code() == 0),
            "{}",
            leaf.get_name()
        );
    }
}
