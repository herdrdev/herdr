use super::*;

fn snapshot(
    server: &mut HeadlessServer,
    pane_id: &str,
) -> Result<api::schema::PaneAgentStatusChangedEvent, api::schema::ErrorBody> {
    let (respond_to, response) = std::sync::mpsc::channel();
    server.handle_api_message(api::ApiMessage::PaneAgentStatus {
        pane_id: pane_id.into(),
        respond_to,
    });
    response.try_recv().unwrap()
}

#[test]
fn status_snapshot_reads_current_presentation_seen_state_and_expiry() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("status")];
    server.app.state.ensure_test_terminals();
    let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let terminal_id = server.app.state.workspaces[0].tabs[0].panes[&pane_id]
        .attached_terminal_id
        .clone();
    let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.set_detected_state(
        Some(crate::detect::Agent::Codex),
        crate::detect::AgentState::Idle,
    );
    let report = crate::terminal::AgentMetadataReport {
        source: "test".into(),
        agent_label: None,
        applies_to_source: None,
        title: Some("Reviewing changes".into()),
        display_agent: Some("reviewer".into()),
        state_labels: [("idle".into(), "review complete".into())].into(),
        clear_title: false,
        clear_display_agent: false,
        clear_state_labels: false,
        ttl: None,
        seq: None,
    };
    terminal.set_agent_metadata(report.clone());

    for (seen, expected) in [
        (false, api::schema::AgentStatus::Done),
        (true, api::schema::AgentStatus::Idle),
    ] {
        server.app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = seen;
        let current = snapshot(&mut server, &public_pane_id).unwrap();
        assert_eq!(current.pane_id, public_pane_id);
        assert_eq!(current.agent_status, expected);
        assert_eq!(current.agent.as_deref(), Some("codex"));
        assert_eq!(current.title.as_deref(), Some("Reviewing changes"));
        assert_eq!(current.display_agent.as_deref(), Some("reviewer"));
        assert_eq!(current.state_labels.get("idle").unwrap(), "review complete");
    }
    assert_eq!(server.app.terminal_runtimes.len(), 0);

    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_agent_metadata(crate::terminal::AgentMetadataReport {
            ttl: Some(Duration::ZERO),
            ..report
        });
    server.app.sync_agent_metadata_deadline();
    let expired = snapshot(&mut server, &public_pane_id).unwrap();
    assert_eq!(expired.title, None);
    assert_eq!(expired.display_agent, None);
    assert!(expired.state_labels.is_empty());
    assert_eq!(server.app.agent_metadata_deadline, None);
}

#[tokio::test]
async fn status_snapshot_refreshes_pending_terminal_titles() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("status")];
    server.app.state.ensure_test_terminals();
    let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let terminal_id = server.app.state.workspaces[0].tabs[0].panes[&pane_id]
        .attached_terminal_id
        .clone();
    let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
    runtime.test_process_pty_bytes(b"\x1b]0;Current task\x07");
    server
        .app
        .terminal_runtimes
        .insert(terminal_id.clone(), runtime);
    server.app.render_dirty.request_terminal_title(pane_id);

    snapshot(&mut server, &public_pane_id).unwrap();
    assert_eq!(
        server.app.state.terminals[&terminal_id]
            .terminal_title
            .as_deref(),
        Some("Current task"),
    );
    shutdown_test_runtimes(&mut server);
}

#[test]
fn status_snapshot_drains_agent_changes_before_reading_state() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("status")];
    server.app.state.ensure_test_terminals();
    let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    server
        .app
        .event_tx
        .try_send(AppEvent::StateChanged {
            pane_id,
            agent: Some(crate::detect::Agent::Codex),
            state: crate::detect::AgentState::Working,
            visible_blocker: false,
            visible_working: true,
            process_exited: false,
            observed_at: Instant::now(),
        })
        .unwrap();

    let current = snapshot(&mut server, &public_pane_id).unwrap();
    assert_eq!(current.agent_status, api::schema::AgentStatus::Working);
    assert_eq!(current.agent.as_deref(), Some("codex"));
    assert!(server.app.event_rx.try_recv().is_err());
}

#[test]
fn status_snapshot_reports_missing_panes_without_creating_a_workspace() {
    let mut server = test_headless_server();
    let error = snapshot(&mut server, "missing").unwrap_err();
    assert_eq!(error.code, "pane_not_found");
    assert_eq!(error.message, "pane missing not found");
    assert!(server.app.state.workspaces.is_empty());
}

#[tokio::test]
async fn status_snapshots_preserve_api_queue_order_and_shutdown_rejection() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("status")];
    server.app.state.ensure_test_terminals();
    let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let (sender, receiver) = mpsc::unbounded_channel();
    server.app.api_rx = receiver;
    let (before_tx, before) = std::sync::mpsc::channel();
    let (after_tx, after) = std::sync::mpsc::channel();
    let (mutation_tx, mutation) = std::sync::mpsc::channel();
    let request = |title| {
        serde_json::from_value(serde_json::json!({
            "id": "metadata",
            "method": "pane.report_metadata",
            "params": {
                "pane_id": public_pane_id,
                "source": "test",
                "title": title,
            },
        }))
        .unwrap()
    };
    sender
        .send(api::ApiMessage::PaneAgentStatus {
            pane_id: public_pane_id.clone(),
            respond_to: before_tx,
        })
        .unwrap();
    sender
        .send(api::ApiMessage::Request(Box::new(api::ApiRequestMessage {
            request: request("Updated task"),
            respond_to: mutation_tx.clone(),
            response_write_complete: None,
        })))
        .unwrap();
    sender
        .send(api::ApiMessage::PaneAgentStatus {
            pane_id: public_pane_id.clone(),
            respond_to: after_tx,
        })
        .unwrap();

    server.drain_api_requests_with_shutdown_check();
    assert_eq!(before.try_recv().unwrap().unwrap().title, None);
    let mutation_response: serde_json::Value =
        serde_json::from_str(&mutation.try_recv().unwrap()).unwrap();
    assert_eq!(mutation_response["result"]["type"], "ok");
    assert_eq!(
        after.try_recv().unwrap().unwrap().title.as_deref(),
        Some("Updated task")
    );

    let (shutdown_tx, shutdown) = std::sync::mpsc::channel();
    sender
        .send(api::ApiMessage::Request(Box::new(api::ApiRequestMessage {
            request: request("Must not replace the task"),
            respond_to: mutation_tx,
            response_write_complete: None,
        })))
        .unwrap();
    sender
        .send(api::ApiMessage::PaneAgentStatus {
            pane_id: public_pane_id.clone(),
            respond_to: shutdown_tx,
        })
        .unwrap();
    server.shutting_down = true;
    server.reject_queued_api_requests_for_shutdown();

    let mutation_response: serde_json::Value =
        serde_json::from_str(&mutation.try_recv().unwrap()).unwrap();
    assert_eq!(mutation_response["error"]["code"], "server_unavailable");
    assert_eq!(
        shutdown.try_recv().unwrap().unwrap_err().code,
        "server_unavailable"
    );
    assert_eq!(
        server
            .app
            .pane_agent_status_snapshot(&public_pane_id)
            .unwrap()
            .title
            .as_deref(),
        Some("Updated task")
    );
    assert!(server.app.api_rx.is_empty());
}

#[test]
fn status_snapshot_rejects_shutdown_before_and_after_event_drain() {
    thread_local! {
        static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    fn shutdown_on_second_check() -> bool {
        CHECKS.with(|checks| {
            let previous = checks.get();
            checks.set(previous + 1);
            previous > 0
        })
    }

    for after_drain in [false, true] {
        let mut server = test_headless_server();
        CHECKS.with(|checks| checks.set(0));
        server.host_shutdown_probe = if after_drain {
            shutdown_on_second_check
        } else {
            || true
        };
        let error = snapshot(&mut server, "missing").unwrap_err();
        assert_eq!(error.code, "server_unavailable");
        assert!(server.host_shutdown_requested.load(Ordering::Acquire));
    }
}
