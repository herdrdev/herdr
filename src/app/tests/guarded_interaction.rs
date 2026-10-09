use super::*;
use crate::api::{interaction_journal as journal, schema::*};

#[tokio::test]
async fn interaction_actual_handler_binds_exact_terminal_and_rejects_wrong_session_owner() {
    let _lock = config_env_lock().lock().unwrap();
    let root = unique_temp_path("guarded-handler");
    crate::platform::create_private_state_directory(&root).unwrap();
    crate::platform::create_private_state_directory(&root.join(crate::config::app_dir_name()))
        .unwrap();
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
    let _restore = Restore(
        [
            "XDG_CONFIG_HOME",
            "HERDR_GUARDED_CLAUDE_PROFILE",
            "HERDR_SESSION",
        ]
        .into_iter()
        .map(|name| (name, std::env::var_os(name)))
        .collect(),
    );
    std::env::set_var("XDG_CONFIG_HOME", &root);
    std::env::set_var(
        "HERDR_GUARDED_CLAUDE_PROFILE",
        "2.1.284-custom-v1-experimental",
    );
    std::env::remove_var("HERDR_SESSION");
    let mut app = test_app();
    app.state.workspaces = vec![Workspace::test_new("owned"), Workspace::test_new("other")];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    let capture=format!("Claude Code v2.1.284\r\n{}\r\n ☐ Format\r\n\r\nWhat format?\r\n\r\n❯ 1. Café\r\n     Description\r\n  2. Video\r\n     Description\r\n  3. Async\r\n     Description\r\n  4. Type something.\r\n{}\r\n  5. Chat about this\r\n\r\nEnter to select · ↑/↓ to navigate · Esc to cancel\r\n","─".repeat(80),"─".repeat(80));
    let capture = capture.replace(
        "  4. Type something.",
        "  \x1b[38;2;153;153;153m4. Type something.\x1b[0m",
    );
    let mut ids = Vec::new();
    let mut receivers = Vec::new();
    for i in 0..2 {
        let pane = app.state.workspaces[i].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[i].terminal_id(pane).unwrap().clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Blocked,
        );
        terminal.last_agent_state_change_seq = Some(7);
        terminal.set_agent_name(format!("worker-{i}"));
        terminal.set_agent_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id(format!("session-{i}")),
            Some(7),
        );
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(120, 40, 4);
        // TestChannel never spawns a process and preserves processes on drop. This synthetic
        // identity exceeds supported PID ranges; it cannot name another user's live process.
        runtime.test_set_child_pid(1_000_000_001 + i as u32);
        runtime.test_process_pty_bytes(capture.as_bytes());
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        ids.push(terminal_id.to_string());
        receivers.push(rx);
    }
    let get = |app: &mut App, target: &str| -> serde_json::Value {
        serde_json::from_str(&app.handle_api_request(Request {
            id: "get".into(),
            method: Method::AgentInteractionGet(AgentTarget {
                target: target.into(),
            }),
        }))
        .unwrap()
    };
    std::env::set_var("HERDR_GUARDED_CLAUDE_PROFILE", "2.1.284-experimental");
    let disabled = get(&mut app, &ids[0]);
    assert_eq!(
        disabled["result"]["supported"], false,
        "old token must not enroll the failed compiler"
    );
    std::env::set_var(
        "HERDR_GUARDED_CLAUDE_PROFILE",
        "2.1.284-custom-v1-experimental",
    );
    let observed = get(&mut app, &ids[0]);
    assert_eq!(observed["result"]["supported"], true, "{observed}");
    let expected: InteractionObservation =
        serde_json::from_value(observed["result"]["observation"].clone()).unwrap();
    assert_eq!(expected.terminal_id, ids[0]);
    assert_eq!(expected.agent_session.value, "session-0");
    let submit = |app: &mut App,
                  operation_id: &str,
                  expected: InteractionObservation,
                  action: InteractionAction|
     -> serde_json::Value {
        let mut params = InteractionSubmitParams {
            operation_id: operation_id.into(),
            payload_digest: String::new(),
            expected,
            action,
        };
        params.payload_digest = journal::payload_digest(&params).unwrap();
        serde_json::from_str(&app.handle_api_request(Request {
            id: operation_id.into(),
            method: Method::AgentInteractionSubmit(params),
        }))
        .unwrap()
    };
    let begin = InteractionAction::BeginCustom {
        option_id: "choice-4".into(),
    };
    let accepted = submit(&mut app, "owned", expected.clone(), begin.clone());
    assert_eq!(
        accepted["result"]["receipt"]["outcome"], "enqueued",
        "{accepted}"
    );
    assert_eq!(receivers[0].try_recv().unwrap().as_ref(), b"4");
    let mut wrong_owner: InteractionObservation =
        serde_json::from_value(get(&mut app, &ids[1])["result"]["observation"].clone()).unwrap();
    wrong_owner.agent_session = expected.agent_session.clone();
    let rejected = submit(&mut app, "wrong-owner", wrong_owner, begin.clone());
    assert_eq!(rejected["result"]["receipt"]["outcome"], "rejected");
    assert_eq!(rejected["result"]["receipt"]["code"], "stale_identity");
    assert!(receivers[1].try_recv().is_err());
    let journal_root = crate::session::data_dir().join("interaction-operations-v1");
    let lineage = journal::validated_lineage(&journal_root, "owned")
        .unwrap()
        .unwrap();
    assert_eq!(lineage.request.expected, expected);
    assert_eq!(
        lineage.request.action,
        InteractionAction::BeginCustom {
            option_id: "choice-4".into()
        }
    );
    assert_eq!(lineage.dialog.question, "What format?");
    assert_eq!(lineage.encoded_input_digest, journal::digest(b"4"));
    assert!(journal::validated_lineage(&journal_root, "wrong-owner")
        .unwrap()
        .is_none());
    let retry = submit(&mut app, "owned", expected.clone(), begin.clone());
    assert_eq!(retry, accepted);
    assert!(receivers[0].try_recv().is_err());
    let mut negatives = Vec::new();
    let mut stale = expected.clone();
    stale.runtime_pid += 1;
    negatives.push(("process", stale, begin.clone(), "stale_identity"));
    let mut stale = expected.clone();
    stale.state_change_seq += 1;
    negatives.push(("sequence", stale, begin.clone(), "stale_state"));
    let mut stale = expected.clone();
    stale.content_digest = journal::digest(b"different");
    negatives.push(("content", stale, begin.clone(), "stale_content"));
    let mut stale = expected.clone();
    stale.style_digest = Some(journal::digest(b"different style"));
    negatives.push(("style", stale, begin.clone(), "stale_style"));
    negatives.push((
        "choose",
        expected.clone(),
        InteractionAction::Choose {
            option_id: "choice-1".into(),
        },
        "unsupported_interaction_phase",
    ));
    negatives.push((
        "free-text",
        expected.clone(),
        InteractionAction::FreeText {
            text: "answer".into(),
        },
        "unsupported_interaction_phase",
    ));
    negatives.push((
        "submit-custom",
        expected.clone(),
        InteractionAction::SubmitCustom {
            text: "answer".into(),
            parent_operation_id: "owned".into(),
        },
        "unsupported_interaction_phase",
    ));
    for (id, observation, action, code) in negatives {
        let result = submit(&mut app, id, observation, action);
        assert_eq!(
            result["result"]["receipt"]["outcome"], "rejected",
            "{result}"
        );
        assert_eq!(result["result"]["receipt"]["code"], code, "{result}");
        assert!(receivers[0].try_recv().is_err());
        assert!(journal::validated_lineage(&journal_root, id)
            .unwrap()
            .is_none());
    }
    std::env::set_var("HERDR_GUARDED_CLAUDE_PROFILE", "2.1.284-experimental");
    let result = submit(&mut app, "old-token", expected.clone(), begin.clone());
    assert_eq!(
        result["result"]["receipt"]["code"],
        "unsupported_interaction_profile"
    );
    assert!(receivers[0].try_recv().is_err());
    assert!(journal::validated_lineage(&journal_root, "old-token")
        .unwrap()
        .is_none());
    std::env::set_var(
        "HERDR_GUARDED_CLAUDE_PROFILE",
        "2.1.284-custom-v1-experimental",
    );
    for (id, altered) in [
        ("wrong-version", capture.replace("2.1.284", "2.1.285")),
        (
            "custom-focused",
            capture.replace("❯ 1.", "  1.").replace("  4.", "❯ 4."),
        ),
    ] {
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(120, 40, 4);
        runtime.test_set_child_pid(1_000_000_001);
        runtime.test_process_pty_bytes(altered.as_bytes());
        let terminal_id = app.state.workspaces[0]
            .terminal_id(app.state.workspaces[0].tabs[0].root_pane)
            .unwrap()
            .clone();
        app.terminal_runtimes.insert(terminal_id, runtime);
        receivers[0] = rx;
        let observation =
            serde_json::from_value(get(&mut app, &ids[0])["result"]["observation"].clone())
                .unwrap();
        let result = submit(&mut app, id, observation, begin.clone());
        assert_eq!(
            result["result"]["receipt"]["code"], "unsupported_interaction_profile",
            "{result}"
        );
        assert!(receivers[0].try_recv().is_err());
        assert!(journal::validated_lineage(&journal_root, id)
            .unwrap()
            .is_none());
    }
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn interaction_actual_handler_submits_custom_once_after_bound_parent_and_empty_styled_phase()
{
    let _lock = config_env_lock().lock().unwrap();
    let root = unique_temp_path("guarded-handler");
    crate::platform::create_private_state_directory(&root).unwrap();
    crate::platform::create_private_state_directory(&root.join(crate::config::app_dir_name()))
        .unwrap();
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
    let _restore = Restore(
        [
            "XDG_CONFIG_HOME",
            "HERDR_GUARDED_CLAUDE_PROFILE",
            "HERDR_SESSION",
        ]
        .into_iter()
        .map(|name| (name, std::env::var_os(name)))
        .collect(),
    );
    std::env::set_var("XDG_CONFIG_HOME", &root);
    std::env::set_var(
        "HERDR_GUARDED_CLAUDE_PROFILE",
        "2.1.284-custom-v1-experimental",
    );
    std::env::remove_var("HERDR_SESSION");
    let mut app = test_app();
    app.state.workspaces = vec![Workspace::test_new("owned"), Workspace::test_new("other")];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    let capture=format!("Claude Code v2.1.284\r\n{}\r\n ☐ Format\r\n\r\nWhat format?\r\n\r\n❯ 1. Café\r\n     Description\r\n  2. Video\r\n     Description\r\n  3. Async\r\n     Description\r\n  4. Type something.\r\n{}\r\n  5. Chat about this\r\n\r\nEnter to select · ↑/↓ to navigate · Esc to cancel\r\n","─".repeat(80),"─".repeat(80));
    let capture = capture.replace(
        "  4. Type something.",
        "  \x1b[38;2;153;153;153m4. Type something.\x1b[0m",
    );
    let mut ids = Vec::new();
    let mut receivers = Vec::new();
    for i in 0..2 {
        let pane = app.state.workspaces[i].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[i].terminal_id(pane).unwrap().clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Blocked,
        );
        terminal.last_agent_state_change_seq = Some(7);
        terminal.set_agent_name(format!("worker-{i}"));
        terminal.set_agent_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id(format!("session-{i}")),
            Some(7),
        );
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(120, 40, 4);
        // TestChannel never spawns a process and preserves processes on drop. This synthetic
        // identity exceeds supported PID ranges; it cannot name another user's live process.
        runtime.test_set_child_pid(1_000_000_001 + i as u32);
        runtime.test_process_pty_bytes(capture.as_bytes());
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        ids.push(terminal_id.to_string());
        receivers.push(rx);
    }
    let get = |app: &mut App| -> serde_json::Value {
        serde_json::from_str(&app.handle_api_request(Request {
            id: "get".into(),
            method: Method::AgentInteractionGet(AgentTarget {
                target: ids[0].clone(),
            }),
        }))
        .unwrap()
    };
    let submit = |app: &mut App,
                  operation: &str,
                  expected: InteractionObservation,
                  action: InteractionAction|
     -> serde_json::Value {
        let mut params = InteractionSubmitParams {
            operation_id: operation.into(),
            payload_digest: String::new(),
            expected,
            action,
        };
        params.payload_digest = journal::payload_digest(&params).unwrap();
        serde_json::from_str(&app.handle_api_request(Request {
            id: operation.into(),
            method: Method::AgentInteractionSubmit(params),
        }))
        .unwrap()
    };
    let initial = get(&mut app);
    assert_eq!(initial["result"]["supported"], true, "{initial}");
    let parent_expected: InteractionObservation =
        serde_json::from_value(initial["result"]["observation"].clone()).unwrap();
    let parent_dialog: InteractionDialog =
        serde_json::from_value(initial["result"]["dialog"].clone()).unwrap();
    let parent = submit(
        &mut app,
        "custom-parent",
        parent_expected.clone(),
        InteractionAction::BeginCustom {
            option_id: "choice-4".into(),
        },
    );
    assert_eq!(parent["result"]["receipt"]["outcome"], "enqueued");
    assert_eq!(receivers[0].try_recv().unwrap().as_ref(), b"4");
    let journal_root = crate::session::data_dir().join("interaction-operations-v1");
    let custom = capture
        .replace("❯ 1.", "  1.")
        .replace(
            "  \x1b[38;2;153;153;153m4. Type something.\x1b[0m",
            "❯ 4. \x1b[7mT\x1b[0m\x1b[2mype something.\x1b[0m",
        )
        .replace(
            "Enter to select · ↑/↓ to navigate · Esc to cancel",
            "Enter to select · ↑/↓ to navigate · ctrl+g to edit in nano · Esc to cancel",
        );
    let terminal = app.state.workspaces[0]
        .terminal_id(app.state.workspaces[0].tabs[0].root_pane)
        .unwrap()
        .clone();
    let redraw = |app: &App, text: &str, bracketed: bool| {
        let runtime = app.terminal_runtimes.get(&terminal).unwrap();
        runtime.test_process_pty_bytes(b"\x1bc");
        runtime.test_process_pty_bytes(text.as_bytes());
        runtime.test_process_pty_bytes(if bracketed {
            b"\x1b[?2004h"
        } else {
            b"\x1b[?2004l"
        });
    };
    redraw(&app, &custom, true);
    let observed = get(&mut app);
    assert_eq!(observed["result"]["supported"], true, "{observed}");
    assert_eq!(observed["result"]["dialog"]["phase"], "custom_entry");
    let expected: InteractionObservation =
        serde_json::from_value(observed["result"]["observation"].clone()).unwrap();
    let answer = "A library circle with an optional video dial-in.";
    let action = InteractionAction::SubmitCustom {
        text: answer.into(),
        parent_operation_id: "custom-parent".into(),
    };
    let mut negatives = Vec::new();
    let mut wrong = expected.clone();
    wrong.agent_session.value = "session-other".into();
    negatives.push(("custom-wrong-owner", wrong, "stale_identity"));
    let mut wrong = expected.clone();
    wrong.runtime_pid += 1;
    negatives.push(("custom-wrong-process", wrong, "stale_identity"));
    let mut wrong = expected.clone();
    wrong.state_change_seq += 1;
    negatives.push(("custom-wrong-seq", wrong, "stale_state"));
    let mut wrong = expected.clone();
    wrong.content_digest = journal::digest(b"other");
    negatives.push(("custom-wrong-content", wrong, "stale_content"));
    let mut wrong = expected.clone();
    wrong.style_digest = None;
    negatives.push(("custom-missing-style", wrong, "stale_style"));
    for (op, wrong, code) in negatives {
        let result = submit(&mut app, op, wrong, action.clone());
        assert_eq!(result["result"]["receipt"]["code"], code, "{result}");
        assert!(receivers[0].try_recv().is_err());
        assert!(journal::validated_lineage(&journal_root, op)
            .unwrap()
            .is_none());
    }
    // Identical plain text with actual draft styling must not pass the empty-phase guard.
    let filled = custom.replace("\x1b[2mype something.", "ype something.");
    redraw(&app, &filled, true);
    let filled_observation = get(&mut app);
    assert_eq!(filled_observation["result"]["supported"], false);
    assert_eq!(
        filled_observation["result"]["observation"]["content_digest"],
        observed["result"]["observation"]["content_digest"]
    );
    let result = submit(
        &mut app,
        "custom-stale-filled",
        expected.clone(),
        action.clone(),
    );
    assert_eq!(result["result"]["receipt"]["code"], "stale_style");
    assert!(receivers[0].try_recv().is_err());
    redraw(&app, &custom.replace("2.1.284", "2.1.285"), true);
    let wrong_version = get(&mut app);
    let wrong = serde_json::from_value(wrong_version["result"]["observation"].clone()).unwrap();
    let result = submit(&mut app, "custom-wrong-version", wrong, action.clone());
    assert_eq!(
        result["result"]["receipt"]["code"],
        "unsupported_interaction_profile"
    );
    assert!(receivers[0].try_recv().is_err());
    redraw(&app, &custom, false);
    let current = serde_json::from_value(get(&mut app)["result"]["observation"].clone()).unwrap();
    let result = submit(&mut app, "custom-unbracketed", current, action.clone());
    assert_eq!(
        result["result"]["receipt"]["code"],
        "bracketed_paste_required"
    );
    assert!(receivers[0].try_recv().is_err());
    redraw(&app, &custom, true);
    let expected: InteractionObservation =
        serde_json::from_value(get(&mut app)["result"]["observation"].clone()).unwrap();
    // Pure journal fixtures establish actual handler's parent outcome/dialog/identity guards.
    for (parent_id, kind) in [
        ("custom-parent-unknown", 0),
        ("custom-parent-other-dialog", 1),
        ("custom-parent-other-owner", 2),
        ("custom-parent-legacy", 3),
        ("custom-parent-other-episode", 4),
    ] {
        let mut request = InteractionSubmitParams {
            operation_id: parent_id.into(),
            payload_digest: String::new(),
            expected: parent_expected.clone(),
            action: InteractionAction::BeginCustom {
                option_id: "choice-4".into(),
            },
        };
        let mut dialog = parent_dialog.clone();
        if kind == 1 {
            dialog.question = "Different question?".into();
        }
        if kind == 2 {
            request.expected.agent_session.value = "other-owner".into();
        }
        if kind == 4 {
            request.expected.state_change_seq -= 1;
        }
        request.payload_digest = journal::payload_digest(&request).unwrap();
        if kind == 3 {
            journal::dispatch(&journal_root, &request, || Ok(b"4".to_vec()), |_| Ok(())).unwrap();
        } else {
            journal::dispatch_guarded(
                &journal_root,
                &request,
                || Ok((b"4".to_vec(), dialog)),
                |_| {
                    if kind == 0 {
                        Err(std::io::Error::other("unknown"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap();
        }
        let result = submit(
            &mut app,
            &format!("child-{kind}"),
            expected.clone(),
            InteractionAction::SubmitCustom {
                text: answer.into(),
                parent_operation_id: parent_id.into(),
            },
        );
        let code = match kind {
            0 => "unresolved_custom_parent",
            1 => "custom_parent_dialog_mismatch",
            2 => "stale_custom_parent_identity",
            4 => "stale_custom_parent_state",
            _ => "invalid_custom_parent",
        };
        assert_eq!(result["result"]["receipt"]["code"], code, "{result}");
        assert!(receivers[0].try_recv().is_err());
    }
    let accepted = submit(&mut app, "custom-child", expected.clone(), action.clone());
    assert_eq!(
        accepted["result"]["receipt"]["outcome"], "enqueued",
        "{accepted}"
    );
    let bytes = receivers[0].try_recv().unwrap();
    assert_eq!(
        bytes.as_ref(),
        format!("\x1b[200~{answer}\x1b[201~\r").as_bytes()
    );
    let lineage = journal::validated_lineage(&journal_root, "custom-child")
        .unwrap()
        .unwrap();
    assert_eq!(lineage.request.action, action);
    assert_eq!(lineage.encoded_input_digest, journal::digest(&bytes));
    let parent_dir = journal_root.join(journal::digest(b"custom-parent"));
    let claim: InteractionSubmitParams =
        serde_json::from_slice(&std::fs::read(parent_dir.join("custom-child.json")).unwrap())
            .unwrap();
    assert_eq!(claim, lineage.request);
    let retry = submit(&mut app, "custom-child", expected.clone(), action.clone());
    assert_eq!(retry, accepted);
    assert!(receivers[0].try_recv().is_err());
    let another = submit(&mut app, "custom-second-child", expected, action);
    assert_eq!(
        another["result"]["receipt"]["code"],
        "custom_parent_consumed"
    );
    assert!(receivers[0].try_recv().is_err());
    assert!(
        journal::validated_lineage(&journal_root, "custom-second-child")
            .unwrap()
            .is_none()
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}
