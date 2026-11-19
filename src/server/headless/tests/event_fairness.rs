use super::*;

fn queue_requests(server: &mut HeadlessServer, count: usize) -> std::sync::mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::unbounded_channel();
    server.app.api_rx = receiver;
    let (respond_to, responses) = std::sync::mpsc::channel();
    for index in 0..count {
        sender
            .send(api::ServerRequest::Api(Box::new(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: index.to_string(),
                    method: api::schema::Method::WorkspaceList(api::schema::EmptyParams::default()),
                },
                respond_to: respond_to.clone(),
                response_write_complete: None,
            })))
            .unwrap();
    }
    responses
}

#[test]
fn external_api_batches_preserve_fifo_and_leave_work_for_the_next_turn() {
    let mut server = test_headless_server();
    let count = EXTERNAL_EVENT_DRAIN_LIMIT * 2 + 1;
    let responses = queue_requests(&mut server, count);
    let mut processed = 0;
    while !server.app.api_rx.is_empty() {
        server.drain_api_requests_with_shutdown_check();
        let batch: Vec<_> = responses.try_iter().collect();
        assert_eq!(
            batch.len(),
            EXTERNAL_EVENT_DRAIN_LIMIT.min(count - processed)
        );
        for response in batch {
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["id"], processed.to_string());
            assert!(response.get("result").is_some());
            processed += 1;
        }
        assert_eq!(server.app.api_rx.len(), count - processed);
    }
    assert_eq!(processed, count);
}

#[test]
fn external_server_events_yield_with_backlog_and_preserve_fifo() {
    let mut server = test_headless_server();
    let count = EXTERNAL_EVENT_DRAIN_LIMIT * 2 + 1;
    let (sender, receiver) = mpsc::channel(count);
    server.server_event_rx = receiver;
    server.server_event_tx = sender.clone();
    for client_id in 0..count as u64 {
        sender
            .try_send(ServerEvent::ClientDisconnected { client_id })
            .unwrap();
    }
    server.drain_server_events();
    assert_eq!(
        server.server_event_rx.len(),
        count - EXTERNAL_EVENT_DRAIN_LIMIT
    );
    let ServerEvent::ClientDisconnected { client_id } = server.server_event_rx.try_recv().unwrap()
    else {
        panic!("expected the next disconnect");
    };
    assert_eq!(client_id, EXTERNAL_EVENT_DRAIN_LIMIT as u64);
    server.drain_server_events();
    assert!(server.server_event_rx.is_empty());
}

#[test]
fn external_api_batch_stops_immediately_when_shutdown_is_requested() {
    let mut server = test_headless_server();
    let responses = queue_requests(&mut server, EXTERNAL_EVENT_DRAIN_LIMIT + 1);
    server.should_quit.store(true, Ordering::Release);
    assert!(!server.drain_api_requests_with_shutdown_check());
    assert_eq!(server.app.api_rx.len(), EXTERNAL_EVENT_DRAIN_LIMIT + 1);
    assert_eq!(responses.try_iter().count(), 0);
}

#[test]
fn scheduled_work_runs_between_external_api_batches() {
    let mut server = test_headless_server();
    let responses = queue_requests(&mut server, EXTERNAL_EVENT_DRAIN_LIMIT + 1);
    let now = Instant::now();
    server.app.config_diagnostic_deadline = Some(now);
    server.app.state.config_diagnostic = Some("expired diagnostic".into());

    server.drain_api_requests_with_shutdown_check();
    assert_eq!(responses.try_iter().count(), EXTERNAL_EVENT_DRAIN_LIMIT);
    assert_eq!(server.app.api_rx.len(), 1);
    assert!(server.handle_scheduled_tasks_headless(now, false));
    assert!(server.app.state.config_diagnostic.is_none());
    assert!(server.app.config_diagnostic_deadline.is_none());
    assert_eq!(server.app.api_rx.len(), 1);
}

#[tokio::test]
async fn server_loop_drains_api_backlog_and_runs_scheduled_work() {
    let mut server = test_headless_server();
    let (sender, receiver) = mpsc::unbounded_channel();
    server.app.api_rx = receiver;
    let (respond_to, responses) = std::sync::mpsc::channel();
    let count = EXTERNAL_EVENT_DRAIN_LIMIT * 2 + 1;
    for index in 0..=count {
        // A queued stop terminates the real loop without a concurrent producer
        // or a timing assertion. Earlier requests must all receive responses.
        let method = if index == count {
            api::schema::Method::ServerStop(api::schema::EmptyParams::default())
        } else {
            api::schema::Method::WorkspaceList(api::schema::EmptyParams::default())
        };
        sender
            .send(api::ServerRequest::Api(Box::new(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: index.to_string(),
                    method,
                },
                respond_to: respond_to.clone(),
                response_write_complete: None,
            })))
            .unwrap();
    }
    server.app.config_diagnostic_deadline = Some(Instant::now());
    server.app.state.config_diagnostic = Some("expired diagnostic".into());

    tokio::time::timeout(Duration::from_secs(5), server.run())
        .await
        .expect("queued API requests must wake the server loop")
        .expect("server loop shuts down cleanly");

    let responses: Vec<_> = responses.try_iter().collect();
    assert_eq!(responses.len(), count + 1);
    for (index, response) in responses.iter().enumerate() {
        let response: serde_json::Value = serde_json::from_str(response).unwrap();
        assert_eq!(response["id"], index.to_string());
        assert!(response.get("result").is_some());
    }
    assert!(server.app.api_rx.is_empty());
    assert!(server.app.state.config_diagnostic.is_none());
    assert!(server.app.config_diagnostic_deadline.is_none());
    assert_eq!(server.app.terminal_runtimes.len(), 0);
}

#[test]
fn agent_status_request_expires_metadata_and_preserves_errors() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("status-read");
    let pane_id = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let terminal_id = server.app.state.workspaces[0]
        .pane_state(pane_id)
        .unwrap()
        .attached_terminal_id
        .clone();
    let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.set_detected_state(
        Some(crate::detect::Agent::Pi),
        crate::detect::AgentState::Working,
    );
    terminal.set_agent_metadata(crate::terminal::AgentMetadataReport {
        source: "status-test".into(),
        agent_label: None,
        applies_to_source: None,
        title: Some("expired".into()),
        display_agent: None,
        state_labels: HashMap::new(),
        clear_title: false,
        clear_display_agent: false,
        clear_state_labels: false,
        ttl: Some(Duration::ZERO),
        seq: None,
    });
    server.app.sync_agent_metadata_deadline();
    let sequence = server.app.event_hub.current_sequence();
    let (respond_to, response) = std::sync::mpsc::channel();
    assert!(
        server.handle_server_request(api::ServerRequest::AgentStatus {
            id: "status".into(),
            pane_id: public_pane_id.clone(),
            respond_to,
        })
    );
    let snapshot = response.recv().unwrap().unwrap();
    assert_eq!(snapshot.pane_id, public_pane_id);
    assert_eq!(snapshot.agent_status, api::schema::AgentStatus::Working);
    assert_eq!(snapshot.title, None);
    assert!(server.app.event_hub.current_sequence() > sequence);
    assert!(server.app.agent_metadata_deadline.is_none());

    for shutting_down in [false, true] {
        server.shutting_down = shutting_down;
        let (respond_to, response) = std::sync::mpsc::channel();
        server.handle_server_request(api::ServerRequest::AgentStatus {
            id: "missing".into(),
            pane_id: "missing".into(),
            respond_to,
        });
        let error = response.recv().unwrap().unwrap_err();
        assert_eq!(error.id, "missing");
        assert_eq!(
            error.error.code,
            if shutting_down {
                "server_unavailable"
            } else {
                "pane_not_found"
            }
        );
        assert_eq!(
            error.error.message,
            if shutting_down {
                "server is shutting down"
            } else {
                "pane missing not found"
            }
        );
    }
}

#[test]
fn agent_status_request_preserves_foreground_seen_projection() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("status-seen");
    let pane_id = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let terminal_id = server.app.state.workspaces[0]
        .pane_state(pane_id)
        .unwrap()
        .attached_terminal_id
        .clone();
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
    let (_control, _render) = connect_test_shell(&mut server, 5289, 120, 40);
    for focused in [false, true] {
        server.clients.get_mut(&5289).unwrap().outer_terminal_focus = Some(focused);
        server.foreground_client_id = Some(5289);
        server.app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .unwrap()
            .seen = false;
        let (respond_to, response) = std::sync::mpsc::channel();
        server.handle_server_request(api::ServerRequest::AgentStatus {
            id: "seen".into(),
            pane_id: public_pane_id.clone(),
            respond_to,
        });
        let snapshot = response.recv().unwrap().unwrap();
        assert_eq!(
            snapshot.agent_status,
            if focused {
                api::schema::AgentStatus::Idle
            } else {
                api::schema::AgentStatus::Done
            }
        );
    }
}

#[test]
#[ignore = "manual external API burst scheduling profile"]
fn external_api_burst_profile() {
    for pane_count in [1, 15] {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("api-profile");
        for _ in 1..pane_count {
            workspace.test_split(ratatui::layout::Direction::Horizontal);
        }
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        for count in [64, 512, 4096] {
            let mut first_samples = Vec::new();
            let mut total_samples = Vec::new();
            let mut first_count = 0;
            for sample in 0..10 {
                let responses = queue_requests(&mut server, count);
                let start = Instant::now();
                server.drain_api_requests_with_shutdown_check();
                let first = start.elapsed();
                first_count = count - server.app.api_rx.len();
                while !server.app.api_rx.is_empty() {
                    server.drain_api_requests_with_shutdown_check();
                }
                let total = start.elapsed();
                assert_eq!(responses.try_iter().count(), count);
                if sample >= 3 {
                    first_samples.push(first);
                    total_samples.push(total);
                }
            }
            first_samples.sort_unstable();
            total_samples.sort_unstable();
            println!("api-burst panes={pane_count} requests={count} first_count={first_count} first_us={:.3} total_us={:.3}",
                first_samples[3].as_secs_f64() * 1e6, total_samples[3].as_secs_f64() * 1e6);
        }
    }
}
