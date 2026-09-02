use super::*;

#[path = "pane_graphics.rs"]
mod pane_graphics_tests;

fn client_shell_snapshot(message: ServerMessage) -> Box<crate::protocol::ClientShellSnapshot> {
    let ServerMessage::EndpointControl { kind, data } = message else {
        panic!("expected client shell snapshot");
    };
    assert_eq!(kind, crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND);
    Box::new(serde_json::from_str(&data).expect("decode client shell snapshot"))
}

fn test_headless_server() -> HeadlessServer {
    test_headless_server_with_event_hub(api::EventHub::default())
}

fn test_headless_server_with_event_hub(event_hub: api::EventHub) -> HeadlessServer {
    let config = crate::config::Config::default();
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let app = crate::app::App::new(
        &config,
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        event_hub,
    );

    let dir = std::env::temp_dir().join(format!(
        "hh-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::create_dir_all(&dir);
    let socket_path = dir.join("client.sock");
    let _ = fs::remove_file(&socket_path);
    let listener = bind_local_listener(&socket_path).expect("bind test listener");
    let client_socket_identity =
        socket_file_identity(&socket_path).expect("test listener socket identity");
    #[cfg(unix)]
    listener
        .set_nonblocking(ListenerNonblockingMode::Accept)
        .expect("set listener nonblocking");
    let (server_event_tx, server_event_rx) = mpsc::channel(64);
    let should_quit = Arc::new(AtomicBool::new(false));
    #[cfg(windows)]
    spawn_windows_client_accept_thread(listener, should_quit.clone(), server_event_tx.clone());
    let server_keybindings = app_keybindings(&app);
    let headless_size = app.state.headless_size;

    HeadlessServer {
        app,
        #[cfg(unix)]
        api_tx: None,
        api_server: None,
        #[cfg(unix)]
        client_listener: listener,
        client_socket_path: socket_path,
        client_socket_identity,
        clients: HashMap::new(),
        #[cfg(unix)]
        next_client_id: 1,
        foreground_client_id: None,
        tab_geometry_controllers: HashMap::new(),
        popup_owner_tab_id: None,
        client_shell_boot_id: "test-boot".into(),
        sent_window_title: None,
        api_window_title: None,
        server_keybindings,
        server_config_diagnostic: None,
        server_config_diagnostic_without_keybindings: None,
        terminal_attach_owners: HashMap::new(),
        pending_alt_screen_reads: Vec::new(),
        deferred_alt_screen_reads: Vec::new(),
        next_activity_stamp: 1,
        headless_size,
        effective_size: headless_size,
        shutting_down: false,
        handoff_in_progress: false,
        #[cfg(unix)]
        pending_handoff_repaint_nudge: false,
        should_quit,
        server_event_rx,
        server_event_tx,
    }
}

fn shutdown_test_runtimes(server: &mut HeadlessServer) {
    for (_, runtime) in server.app.terminal_runtimes.drain() {
        runtime.shutdown();
    }
}

fn read_server_message(bytes: Vec<u8>) -> ServerMessage {
    let mut cursor = std::io::Cursor::new(bytes);
    protocol::read_message(&mut cursor, MAX_FRAME_SIZE).expect("decode server message")
}

fn frame_text(frame: &FrameData) -> String {
    frame
        .cells
        .chunks(usize::from(frame.width))
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_server_shutdown_reason(bytes: Vec<u8>) -> Option<String> {
    match read_server_message(bytes) {
        ServerMessage::ServerShutdown { reason } => reason,
        other => panic!("expected shutdown, got {other:?}"),
    }
}

#[test]
fn completed_handoff_disables_only_old_server_session_persistence() {
    let mut server = test_headless_server();
    server.app.policy = crate::app::AppPolicy::PRODUCTION;

    server.finish_live_handoff_shutdown();

    assert!(!server.app.policy.persist_session);
    assert!(server.app.policy.restore_session);
    assert!(server.app.policy.persist_plugin_registry);
    assert!(server.app.policy.background_updates);
}

#[test]
fn default_headless_size_is_effective_without_clients() {
    let server = test_headless_server();

    assert_eq!(
        server.headless_size,
        (
            crate::config::DEFAULT_HEADLESS_COLS,
            crate::config::DEFAULT_HEADLESS_ROWS
        )
    );
    assert_eq!(server.effective_size, server.headless_size);
}

#[tokio::test]
async fn headless_api_reads_latest_title_without_spinner_event_flooding() {
    let event_hub = api::EventHub::default();
    let mut server = test_headless_server_with_event_hub(event_hub.clone());
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.sidebar_agents.rows = vec![vec![
        crate::config::AgentSidebarToken::TerminalTitleStripped,
    ]];
    let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
    let terminal_id = server.app.state.workspaces[0].tabs[0].panes[&pane_id]
        .attached_terminal_id
        .clone();
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .detected_agent = Some(crate::detect::Agent::Claude);
    let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
    runtime.test_process_pty_bytes(b"\x1b]0;\xe2\xa0\x8b task\x07");
    server
        .app
        .terminal_runtimes
        .insert(terminal_id.clone(), runtime);
    server.app.render_dirty.request_terminal_title(pane_id);

    let first = headless_pane_list(&mut server).pop().unwrap();
    assert_eq!(first.terminal_title.as_deref(), Some("⠋ task"));
    assert_eq!(first.terminal_title_stripped.as_deref(), Some("task"));
    assert_eq!(pane_updated_events(&event_hub), 1);
    server
        .app
        .terminal_runtimes
        .get(&terminal_id)
        .unwrap()
        .test_process_pty_bytes(b"\x1b]2;\xe2\xa0\x99 task\x1b\\");
    server.app.render_dirty.request_terminal_title(pane_id);
    let second = headless_pane_list(&mut server).pop().unwrap();
    assert_eq!(second.terminal_title.as_deref(), Some("⠙ task"));
    assert_eq!(second.terminal_title_stripped.as_deref(), Some("task"));
    assert_eq!(pane_updated_events(&event_hub), 1);
}

fn headless_pane_list(server: &mut HeadlessServer) -> Vec<api::schema::PaneInfo> {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "list-titles".into(),
            method: api::schema::Method::PaneList(api::schema::PaneListParams::default()),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    let response: api::schema::SuccessResponse =
        serde_json::from_str(&response_rx.recv().unwrap()).unwrap();
    let api::schema::ResponseResult::PaneList { panes } = response.result else {
        panic!("expected pane list");
    };
    panes
}

fn pane_updated_events(event_hub: &api::EventHub) -> usize {
    event_hub
        .events_after(0)
        .iter()
        .filter(|(_, event)| event.event == api::schema::EventKind::PaneUpdated)
        .count()
}

#[test]
fn server_stop_interrupts_server_event_backlog() {
    let mut server = test_headless_server();
    for client_id in 1..=64 {
        server
            .server_event_tx
            .try_send(ServerEvent::ClientDisconnected { client_id })
            .unwrap();
    }

    server.should_quit.store(true, Ordering::Release);

    assert!(!server.drain_server_events());
    assert!(server.server_event_rx.try_recv().is_ok());
    shutdown_test_runtimes(&mut server);
}

#[test]
fn headless_api_request_drains_all_pending_internal_events_before_reading_state() {
    let mut server = test_headless_server();
    for i in 0..=crate::app::APP_EVENT_DRAIN_LIMIT {
        server
            .app
            .event_tx
            .try_send(AppEvent::UpdateReady {
                version: format!("4.0.{i}"),
                install_command: "herdr install".into(),
            })
            .unwrap();
    }

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    assert!(
        server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "headless_stop_after_events".into(),
                method: api::schema::Method::ServerStop(api::schema::EmptyParams::default()),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        })
    );
    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();

    assert_eq!(response["result"]["type"], "ok");
    let expected_version = format!("4.0.{}", crate::app::APP_EVENT_DRAIN_LIMIT);
    assert_eq!(
        server.app.state.update_available.as_deref(),
        Some(expected_version.as_str())
    );
    assert!(server.app.event_rx.try_recv().is_err());
}

fn window_title_test_server() -> (HeadlessServer, std::sync::mpsc::Receiver<Vec<u8>>) {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("herd")];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;

    let (client_tx, control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.promote_client_to_foreground(1);
    drain_window_titles(&control_rx);
    (server, control_rx)
}

/// The test client writer drains its queue on a background thread, so
/// reading a pushed message needs a timeout rather than `try_recv`.
fn next_window_title(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Option<Option<String>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok(bytes) = control_rx.recv_timeout(remaining) else {
            return None;
        };
        if let ServerMessage::WindowTitle { title } = read_server_message(bytes) {
            return Some(title);
        }
    }
    None
}

fn drain_window_titles(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>) {
    while control_rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
}

fn no_window_title(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> bool {
    while let Ok(bytes) = control_rx.recv_timeout(Duration::from_millis(200)) {
        if let ServerMessage::WindowTitle { .. } = read_server_message(bytes) {
            return false;
        }
    }
    true
}

#[test]
fn window_title_waits_for_a_foreground_client_to_exist() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("herd")];
    server.app.state.active = Some(0);
    server.app.configure_window_title("{workspace}");

    // The server renders before the first client attaches. Nothing was
    // delivered, so nothing may be recorded as delivered either.
    server.sync_window_title();
    assert_eq!(server.sent_window_title, None);

    let (client_tx, control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.promote_client_to_foreground(1);
    server.sync_window_title();

    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("herd".to_string()))
    );
    shutdown_test_runtimes(&mut server);
}

#[test]
fn an_attaching_client_gets_the_title_even_when_it_has_not_changed() {
    let (mut server, first_control_rx) = window_title_test_server();
    server.app.configure_window_title("{workspace}");
    server.sync_window_title();
    assert_eq!(
        next_window_title(&first_control_rx),
        Some(Some("herd".to_string()))
    );

    // ClientConnected assigns the foreground client directly rather than
    // going through promote_client_to_foreground, so the cache must notice
    // the new client on its own.
    let (client_tx, second_control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(2);
    server.sync_window_title();

    assert_eq!(
        next_window_title(&second_control_rx),
        Some(Some("herd".to_string()))
    );
    shutdown_test_runtimes(&mut server);
}

#[test]
fn configured_window_title_reaches_the_foreground_client_once_per_change() {
    let (mut server, control_rx) = window_title_test_server();
    server.app.configure_window_title("{workspace}/{tab}");

    server.sync_window_title();
    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("herd/1".to_string()))
    );

    // An unchanged title must not re-emit an OSC on every render.
    server.sync_window_title();
    assert!(no_window_title(&control_rx));

    server.app.state.workspaces[0].tabs[0].custom_name = Some("build".into());
    server.sync_window_title();
    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("herd/build".to_string()))
    );

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn focused_terminal_title_syncs_without_requesting_a_sidebar_render() {
    let (mut server, control_rx) = window_title_test_server();
    server.app.configure_window_title("{terminal_title}");
    server.app.state.ensure_test_terminals();
    let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
    let terminal_id = server.app.state.workspaces[0]
        .terminal_id(pane_id)
        .expect("terminal")
        .clone();
    let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
    runtime.test_process_pty_bytes("\x1b]0;⠋ building\x07".as_bytes());
    server
        .app
        .terminal_runtimes
        .insert(terminal_id.clone(), runtime);

    assert_eq!(
        server.sync_terminal_title_sources(&HashSet::from([pane_id])),
        (false, true)
    );
    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("building".to_string()))
    );

    server
        .app
        .terminal_runtimes
        .get(&terminal_id)
        .expect("runtime")
        .test_process_pty_bytes("\x1b]0;⠙ building\x07".as_bytes());
    assert_eq!(
        server.sync_terminal_title_sources(&HashSet::from([pane_id])),
        (false, true)
    );
    assert!(no_window_title(&control_rx));

    shutdown_test_runtimes(&mut server);
}

#[test]
fn a_foreground_client_without_a_writer_does_not_cache_the_window_title() {
    let (mut server, _control_rx) = window_title_test_server();
    server.app.configure_window_title("{workspace}");

    // A detached client keeps its entry but loses its writer, so nothing
    // reaches a terminal even though the targeted send reports success.
    if let Some(client) = server.clients.get_mut(&1) {
        client.writer = None;
    }
    server.sync_window_title();
    assert!(server.sent_window_title.is_none());

    // Attaching again has to deliver the title rather than skip it as sent.
    let (client_tx, control_rx, _render_rx) = test_client_writer();
    if let Some(client) = server.clients.get_mut(&1) {
        client.writer = Some(client_tx);
    }
    server.sync_window_title();
    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("herd".to_string()))
    );

    shutdown_test_runtimes(&mut server);
}

#[test]
fn empty_window_title_config_leaves_the_outer_title_alone() {
    let (mut server, control_rx) = window_title_test_server();
    server.app.configure_window_title("");

    server.sync_window_title();

    assert!(no_window_title(&control_rx));
    shutdown_test_runtimes(&mut server);
}

#[test]
fn api_window_title_wins_until_it_is_cleared() {
    let (mut server, control_rx) = window_title_test_server();
    server.app.configure_window_title("{workspace}");

    server.handle_client_window_title_api("set".into(), Some("herdr api".into()));
    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("herdr api".to_string()))
    );

    server.app.state.workspaces[0].custom_name = Some("ops".into());
    server.sync_window_title();
    assert!(no_window_title(&control_rx));

    // Clearing hands the title back to ui.window_title, not to "herdr".
    server.handle_client_window_title_api("clear".into(), None);
    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("ops".to_string()))
    );

    shutdown_test_runtimes(&mut server);
}

#[test]
fn clearing_the_api_title_falls_back_to_herdr_when_window_titles_are_disabled() {
    let (mut server, control_rx) = window_title_test_server();
    server.app.configure_window_title("");

    server.handle_client_window_title_api("set".into(), Some("herdr api".into()));
    assert_eq!(
        next_window_title(&control_rx),
        Some(Some("herdr api".to_string()))
    );

    server.handle_client_window_title_api("clear".into(), None);
    assert_eq!(next_window_title(&control_rx), Some(None));

    shutdown_test_runtimes(&mut server);
}

#[test]
fn a_newly_promoted_client_gets_the_window_title_again() {
    let (mut server, first_control_rx) = window_title_test_server();
    server.app.configure_window_title("{workspace}");
    server.sync_window_title();
    assert_eq!(
        next_window_title(&first_control_rx),
        Some(Some("herd".to_string()))
    );

    // A second terminal starts on whatever its shell or ssh left behind.
    let (client_tx, second_control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.promote_client_to_foreground(2);
    server.sync_window_title();

    assert_eq!(
        next_window_title(&second_control_rx),
        Some(Some("herd".to_string()))
    );
    shutdown_test_runtimes(&mut server);
}

fn test_client_writer() -> (
    ClientWriter,
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    let (control_tx, control_rx) = std::sync::mpsc::channel();
    let (render_tx, render_rx) = std::sync::mpsc::sync_channel(1);
    (
        ClientWriter::test_channel(control_tx, render_tx),
        control_rx,
        render_rx,
    )
}

#[tokio::test]
async fn client_shell_attach_seeds_workspace() {
    let mut server = test_headless_server();
    server.app.state.workspaces.clear();
    server.app.state.active = None;
    server.app.state.mode = crate::app::Mode::Navigate;
    let (writer, _control_rx, _render_rx) = test_client_writer();

    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: 6,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            writer,
        })
    );

    assert_eq!(server.app.state.mode, crate::app::Mode::Terminal);
    assert_eq!(server.app.state.workspaces.len(), 1);
    assert_eq!(server.app.state.active, Some(0));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_endpoint_request_uses_the_selected_connection() {
    let mut server = test_headless_server();
    let (writer, control_rx, _render_rx) = test_client_writer();
    let client_id = 41;
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            writer,
        })
    );
    let _initial_snapshot = control_rx.recv().expect("initial shell snapshot");
    let boot_id = server.client_shell_boot_id.clone();

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id: boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: "client-shell:1".into(),
                method: api::schema::Method::IntegrationList(api::schema::EmptyParams::default(),),
            }),
        })
    );
    assert!(server.clients[&client_id].shell_endpoint_command_in_flight);

    let response_ready = server
        .server_event_rx
        .recv()
        .await
        .expect("endpoint response ready");
    assert!(!server.handle_server_event(response_ready));
    assert!(!server.clients[&client_id].shell_endpoint_command_in_flight);

    match read_server_message(control_rx.recv().expect("endpoint response")) {
        ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: response_boot_id,
            request_id,
            final_chunk,
            data,
        } => {
            assert_eq!(response_boot_id, boot_id);
            assert_eq!(request_id, "client-shell:1");
            assert!(final_chunk);
            let response = serde_json::from_slice::<api::schema::SuccessResponse>(&data)
                .expect("success response");
            assert_eq!(response.id, "client-shell:1");
            assert!(matches!(
                response.result,
                api::schema::ResponseResult::IntegrationList { .. }
            ));
        }
        other => panic!("expected client shell endpoint response, got {other:?}"),
    }
    shutdown_test_runtimes(&mut server);
}

#[test]
fn terminal_client_endpoint_request_error_removes_client() {
    let mut server = test_headless_server();
    let (writer, _control_rx, _render_rx) = test_client_writer();
    let client_id = 42;
    assert!(!server.handle_server_event(ServerEvent::ClientConnected {
        client_id,
        cols: 80,
        rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        writer,
    }));

    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequestError {
            client_id,
            boot_id: "boot".into(),
            request_id: "request".into(),
            code: "unsupported_method",
            message: "unsupported".into(),
        })
    );
    assert!(!server.clients.contains_key(&client_id));
}

#[tokio::test]
async fn client_shell_receives_metadata_then_shell_free_pane_surface() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("shell-only-label");
    let pane_id = workspace.focused_pane_id().expect("focused pane");
    workspace.insert_test_runtime(
        pane_id,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(
            80,
            23,
            b"\x1b[?1003h\x1b[?1006h\x1b[?1016hCLIENT_SHELL_LIVE",
        ),
    );
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.product_announcement = Some(crate::app::state::ProductAnnouncementState {
        version: "0.8.2".into(),
        id: "client-shell".into(),
        title: "Client shell".into(),
        body: "announcement".into(),
        scroll: 0,
        preview: true,
    });
    server.server_config_diagnostic_without_keybindings = Some("endpoint config warning".into());

    let (writer, control_rx, render_rx) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: 7,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 10,
            cell_height_px: 20,
            pixel_mouse: true,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            writer,
        })
    );
    let snapshot = client_shell_snapshot(read_server_message(
        control_rx.recv().expect("shell snapshot"),
    ));
    assert_eq!(snapshot.workspaces.len(), 1);
    assert_eq!(snapshot.workspaces[0].label, "shell-only-label");
    assert_eq!(
        snapshot.config_diagnostic.as_deref(),
        Some("endpoint config warning")
    );
    assert_eq!(
        snapshot.product_announcement.as_ref().map(|announcement| (
            announcement.version.as_str(),
            announcement.id.as_str(),
            announcement.preview,
        )),
        Some(("0.8.2", "client-shell", true))
    );

    server.render_and_stream();
    let initial_surface = match read_server_message(render_rx.recv().expect("pane surface")) {
        ServerMessage::PaneSurface(surface) => {
            assert_eq!((surface.frame.width, surface.frame.height), (80, 23));
            let text = frame_text(&surface.frame);
            assert!(text.contains("CLIENT_SHELL_LIVE"), "surface: {text:?}");
            assert!(!text.contains("shell-only-label"), "surface: {text:?}");
            assert_eq!(surface.panes.len(), 1);
            assert_eq!(surface.panes[0].rect.x, 0);
            assert_eq!(surface.panes[0].rect.y, 0);
            assert!(surface.panes[0].sgr_pixel_mouse);
            assert_eq!(
                surface.panes[0].pixel_width,
                u32::from(surface.panes[0].inner_rect.width) * 10
            );
            assert_eq!(
                surface.panes[0].pixel_height,
                u32::from(surface.panes[0].inner_rect.height) * 20
            );
            surface
        }
        other => panic!("expected pane surface, got {other:?}"),
    };

    server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .expect("pane runtime")
        .test_process_pty_bytes(b"\rPATCHED");
    let sources = std::collections::HashSet::from([pane_id]);
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    match read_server_message(render_rx.recv().expect("pane surface patch")) {
        ServerMessage::PaneSurfacePatch(patch) => {
            assert_eq!(
                patch.base_surface_revision,
                initial_surface.surface_revision
            );
            assert_eq!(patch.surface_revision, initial_surface.surface_revision + 1);
            assert_eq!(patch.panes.len(), 1);
            assert!(!patch.rows.is_empty());
            assert!(patch
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.symbol == "P"));
        }
        other => panic!("expected pane surface patch, got {other:?}"),
    }
    server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .expect("pane runtime")
        .test_process_pty_bytes(b"\x1b[?1003l\x1b[?1006l\x1b[?1016l");
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    match read_server_message(render_rx.recv().expect("metadata-only pane surface patch")) {
        ServerMessage::PaneSurfacePatch(patch) => {
            assert_eq!(patch.panes.len(), 1);
            assert!(!patch.panes[0].mouse_reporting);
            assert!(!patch.panes[0].sgr_pixel_mouse);
        }
        other => panic!("expected metadata-only pane surface patch, got {other:?}"),
    }
    let retained = server.clients[&7]
        .render_state
        .last_pane_surface()
        .expect("committed retained surface")
        .clone();
    server
        .clients
        .get_mut(&7)
        .unwrap()
        .render_state
        .request_repaint();
    server.render_and_stream();
    let full = match read_server_message(render_rx.recv().expect("full comparison surface")) {
        ServerMessage::PaneSurface(surface) => surface,
        other => panic!("expected full comparison surface, got {other:?}"),
    };
    assert!(full.surface_revision > retained.surface_revision);
    assert_eq!(retained.frame, full.frame);
    assert_eq!(retained.panes, full.panes);
    assert_eq!(retained.splits, full.splits);
    shutdown_test_runtimes(&mut server);
}

fn install_shared_view_test_runtime(server: &mut HeadlessServer) -> crate::layout::PaneId {
    let mut workspace = crate::workspace::Workspace::test_new("shared-view");
    let pane_id = workspace.focused_pane_id().expect("focused pane");
    workspace.insert_test_runtime(
        pane_id,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 23, b"BASE"),
    );
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    pane_id
}

fn connect_test_shell(
    server: &mut HeadlessServer,
    client_id: u64,
    surface_cols: u16,
    surface_rows: u16,
) -> (
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    let (writer, control, render) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id,
            surface_cols,
            surface_rows,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            writer,
        })
    );
    (control, render)
}

fn connect_matching_test_shell(
    server: &mut HeadlessServer,
    client_id: u64,
) -> (
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    connect_test_shell(server, client_id, 80, 23)
}

fn write_shared_test_pane(
    server: &mut HeadlessServer,
    pane_id: crate::layout::PaneId,
    bytes: &[u8],
) {
    server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .expect("pane runtime")
        .test_process_pty_bytes(bytes);
}

fn recv_pane_surface(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    context: &str,
) -> crate::protocol::PaneSurfaceFrame {
    match read_server_message(
        receiver
            .recv()
            .unwrap_or_else(|error| panic!("{context}: {error}")),
    ) {
        ServerMessage::PaneSurface(surface) => surface,
        other => panic!("{context}: expected pane surface, got {other:?}"),
    }
}

fn recv_pane_surface_patch(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    context: &str,
) -> crate::protocol::PaneSurfacePatch {
    match read_server_message(
        receiver
            .recv()
            .unwrap_or_else(|error| panic!("{context}: {error}")),
    ) {
        ServerMessage::PaneSurfacePatch(patch) => patch,
        other => panic!("{context}: expected pane surface patch, got {other:?}"),
    }
}

#[tokio::test]
async fn different_size_shells_receive_geometry_specific_patches_from_one_dirty_collection() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (large_control, large_render) = connect_test_shell(&mut server, 7, 80, 23);
    let (small_control, small_render) = connect_test_shell(&mut server, 8, 68, 17);
    let _ = large_control.recv().expect("large snapshot");
    let _ = small_control.recv().expect("small snapshot");
    server.render_and_stream();
    let large_initial = recv_pane_surface(&large_render, "large initial surface");
    let small_initial = recv_pane_surface(&small_render, "small initial surface");
    assert_eq!(
        (large_initial.frame.width, large_initial.frame.height),
        (80, 23)
    );
    assert_eq!(
        (small_initial.frame.width, small_initial.frame.height),
        (68, 17)
    );
    assert_ne!(
        large_initial.panes[0].inner_rect,
        small_initial.panes[0].inner_rect
    );

    write_shared_test_pane(&mut server, pane_id, b"\rMIXED");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));

    let large_patch = recv_pane_surface_patch(&large_render, "large retained patch");
    let small_patch = recv_pane_surface_patch(&small_render, "small retained patch");
    assert_eq!(
        large_patch.base_surface_revision,
        large_initial.surface_revision
    );
    assert_eq!(
        small_patch.base_surface_revision,
        small_initial.surface_revision
    );
    assert!(large_patch.rows.iter().all(|row| {
        row.x
            .saturating_add(u16::try_from(row.cells.len()).unwrap_or(u16::MAX))
            <= large_initial.frame.width
            && row.y < large_initial.frame.height
    }));
    assert!(small_patch.rows.iter().all(|row| {
        row.x
            .saturating_add(u16::try_from(row.cells.len()).unwrap_or(u16::MAX))
            <= small_initial.frame.width
            && row.y < small_initial.frame.height
    }));
    assert_eq!(large_patch.rows, small_patch.rows);
    assert_ne!(
        large_patch.panes[0].inner_rect,
        small_patch.panes[0].inner_rect
    );
    assert!(frame_text(
        &server.clients[&7]
            .render_state
            .last_pane_surface()
            .expect("large retained surface")
            .frame
    )
    .contains("MIXED"));
    assert!(frame_text(
        &server.clients[&8]
            .render_state
            .last_pane_surface()
            .expect("small retained surface")
            .frame
    )
    .contains("MIXED"));

    write_shared_test_pane(&mut server, pane_id, b"\x1b[?1049hALT");
    assert!(!server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    server.render_and_stream();
    let large_alt = recv_pane_surface(&large_render, "large alternate-screen surface");
    let small_alt = recv_pane_surface(&small_render, "small alternate-screen surface");
    assert!(large_alt.panes[0].alternate_screen_active);
    assert!(small_alt.panes[0].alternate_screen_active);
    assert_eq!(
        large_alt.panes[0].inner_rect.width,
        large_initial.panes[0].inner_rect.width + 1
    );
    assert_eq!(
        small_alt.panes[0].inner_rect.width,
        small_initial.panes[0].inner_rect.width + 1
    );

    write_shared_test_pane(&mut server, pane_id, b"\x1b[?1049l");
    assert!(!server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    server.render_and_stream();
    let large_main = recv_pane_surface(&large_render, "large restored main-screen surface");
    let small_main = recv_pane_surface(&small_render, "small restored main-screen surface");
    assert!(!large_main.panes[0].alternate_screen_active);
    assert!(!small_main.panes[0].alternate_screen_active);
    assert_eq!(
        large_main.panes[0].inner_rect,
        large_initial.panes[0].inner_rect
    );
    assert_eq!(
        small_main.panes[0].inner_rect,
        small_initial.panes[0].inner_rect
    );

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn retained_patches_only_reach_shells_viewing_the_dirty_tab() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("divergent-retained");
    let first_pane = workspace.tabs[0].root_pane;
    let second_tab = workspace.test_add_tab(Some("second"));
    let second_pane = workspace.tabs[second_tab].root_pane;
    workspace.insert_test_runtime(
        first_pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 23, b"FIRST"),
    );
    workspace.insert_test_runtime(
        second_pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 23, b"SECOND"),
    );
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let second_tab_id = server.app.public_tab_id(0, second_tab).unwrap();

    let (first_control, first_render) = connect_matching_test_shell(&mut server, 7);
    let (second_control, second_render) = connect_matching_test_shell(&mut server, 8);
    let _ = first_control.recv().expect("first snapshot");
    let _ = second_control.recv().expect("second snapshot");
    assert!(server.focus_shell_client_on_tab(8, &second_tab_id));
    assert!(server.claim_shell_tab_geometry(8, false));
    assert!(
        server.pty_sources_visible_to_any_render_target(&HashSet::from([first_pane, second_pane,]))
    );
    server.render_and_stream();
    let _ = recv_pane_surface(&first_render, "first baseline");
    let _ = recv_pane_surface(&second_render, "second baseline");

    server.app.state.workspaces[0].test_runtimes[&first_pane]
        .test_process_pty_bytes(b"\rFIRST_PATCH");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([first_pane])));
    let first_patch = recv_pane_surface_patch(&first_render, "first patch");
    assert_eq!(first_patch.panes.len(), 1);
    assert!(second_render.try_recv().is_err());

    server.app.state.workspaces[0].test_runtimes[&second_pane]
        .test_process_pty_bytes(b"\rSECOND_PATCH");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([second_pane])));
    let second_patch = recv_pane_surface_patch(&second_render, "second patch");
    assert_eq!(second_patch.panes.len(), 1);
    assert!(first_render.try_recv().is_err());

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn backpressured_shell_does_not_disable_retained_patches_for_responsive_peer() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (responsive_control, responsive_render) = connect_matching_test_shell(&mut server, 7);
    let (slow_control, slow_render) = connect_matching_test_shell(&mut server, 8);
    let _ = responsive_control.recv().expect("responsive snapshot");
    let _ = slow_control.recv().expect("slow snapshot");
    server.render_and_stream();
    let _ = responsive_render
        .recv()
        .expect("responsive initial surface");
    let _ = slow_render.recv().expect("slow initial surface");

    let sources = HashSet::from([pane_id]);
    write_shared_test_pane(&mut server, pane_id, b"\rONE");
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    assert!(matches!(
        read_server_message(responsive_render.recv().expect("responsive first patch")),
        ServerMessage::PaneSurfacePatch(_)
    ));

    write_shared_test_pane(&mut server, pane_id, b"\rTWO");
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    assert!(matches!(
        read_server_message(responsive_render.recv().expect("responsive second patch")),
        ServerMessage::PaneSurfacePatch(_)
    ));
    assert_eq!(server.clients[&8].deferred_render(), DeferredRender::Full);

    write_shared_test_pane(&mut server, pane_id, b"\rTHREE");
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    assert!(matches!(
        read_server_message(responsive_render.recv().expect("responsive third patch")),
        ServerMessage::PaneSurfacePatch(_)
    ));

    assert!(matches!(
        read_server_message(slow_render.recv().expect("slow queued first patch")),
        ServerMessage::PaneSurfacePatch(_)
    ));
    assert!(server.handle_server_event(ServerEvent::ClientWriterDrained { client_id: 8 }));
    server.render_and_stream();
    assert!(matches!(
        read_server_message(slow_render.recv().expect("slow full recovery surface")),
        ServerMessage::PaneSurface(_)
    ));

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn full_render_backpressure_does_not_disable_responsive_peer_patches() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (responsive_control, responsive_render) = connect_matching_test_shell(&mut server, 7);
    let (slow_control, slow_render) = connect_matching_test_shell(&mut server, 8);
    let _ = responsive_control.recv().expect("responsive snapshot");
    let _ = slow_control.recv().expect("slow snapshot");
    server.render_and_stream();
    let _ = responsive_render
        .recv()
        .expect("responsive initial surface");
    // Keep the slow client's initial surface queued, then force another full
    // replacement for both clients.
    server.clients.get_mut(&7).unwrap().request_repaint();
    server.clients.get_mut(&8).unwrap().request_repaint();
    server.app.full_redraw_pending = true;
    server.render_and_stream();
    let _ = responsive_render
        .recv()
        .expect("responsive full replacement");
    assert_eq!(server.clients[&8].deferred_render(), DeferredRender::Full);
    assert!(!server.app.full_redraw_pending);

    write_shared_test_pane(&mut server, pane_id, b"\rPATCH");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    assert!(matches!(
        read_server_message(responsive_render.recv().expect("responsive retained patch")),
        ServerMessage::PaneSurfacePatch(_)
    ));

    let _ = slow_render.recv().expect("slow queued initial surface");
    assert!(server.handle_server_event(ServerEvent::ClientWriterDrained { client_id: 8 }));
    server.render_and_stream();
    assert!(matches!(
        read_server_message(slow_render.recv().expect("slow full recovery surface")),
        ServerMessage::PaneSurface(_)
    ));

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_config_diagnostics_follow_keybinding_ownership() {
    let mut server = test_headless_server();
    server.server_config_diagnostic = Some("server keybinding warning\ntheme warning".into());
    server.server_config_diagnostic_without_keybindings = Some("theme warning".into());

    let (local_writer, local_control, _local_render) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: 13,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            writer: local_writer,
        })
    );
    let local_snapshot = client_shell_snapshot(read_server_message(
        local_control.recv().expect("local shell snapshot"),
    ));
    assert_eq!(
        local_snapshot.config_diagnostic.as_deref(),
        Some("theme warning")
    );

    let (endpoint_writer, endpoint_control, _endpoint_render) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: 14,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: true,
            mouse_capture: false,
            writer: endpoint_writer,
        })
    );
    let endpoint_snapshot = client_shell_snapshot(read_server_message(
        endpoint_control.recv().expect("endpoint shell snapshot"),
    ));
    assert_eq!(
        endpoint_snapshot.config_diagnostic.as_deref(),
        Some("server keybinding warning\ntheme warning")
    );

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_tab_focus_changes_only_the_source_connection() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("independent-tabs");
    let second_tab = workspace.test_add_tab(Some("second"));
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let tab_ids = server
        .app
        .session_snapshot()
        .tabs
        .into_iter()
        .map(|tab| tab.tab_id)
        .collect::<Vec<_>>();
    let first_tab_id = tab_ids[0].clone();
    let second_tab_id = tab_ids[second_tab].clone();

    let (first_control, _first_render) = connect_test_shell(&mut server, 7, 100, 30);
    let (second_control, _second_render) = connect_test_shell(&mut server, 8, 80, 24);
    let first_initial = client_shell_snapshot(read_server_message(
        first_control.recv().expect("first snapshot"),
    ));
    let second_initial = client_shell_snapshot(read_server_message(
        second_control.recv().expect("second snapshot"),
    ));
    assert_eq!(
        first_initial.focused_tab_id.as_deref(),
        Some(first_tab_id.as_str())
    );
    assert_eq!(
        second_initial.focused_tab_id.as_deref(),
        Some(first_tab_id.as_str())
    );

    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id: 8,
            boot_id: server.client_shell_boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: "focus-second".into(),
                method: api::schema::Method::TabFocus(api::schema::TabTarget {
                    tab_id: second_tab_id.clone(),
                }),
            }),
        })
    );
    let response_ready = server
        .server_event_rx
        .recv()
        .await
        .expect("focus response ready");
    assert!(!server.handle_server_event(response_ready));
    let _ = second_control.recv().expect("focus response");

    server.render_and_stream();

    assert!(
        first_control.try_recv().is_err(),
        "another shell must not receive a navigation replacement"
    );
    let second_replacement = client_shell_snapshot(read_server_message(
        second_control.recv().expect("second replacement snapshot"),
    ));
    assert_eq!(
        second_replacement.focused_tab_id.as_deref(),
        Some(second_tab_id.as_str())
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn deferred_worktree_response_moves_only_its_source_client() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("deferred-worktree");
    let created_tab = workspace.test_add_tab(Some("created"));
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let created_tab_id = server.app.public_tab_id(0, created_tab).unwrap();

    let (source_control, _) = connect_matching_test_shell(&mut server, 51);
    let (other_control, _) = connect_matching_test_shell(&mut server, 52);
    let _ = source_control.recv().expect("source snapshot");
    let _ = other_control.recv().expect("other snapshot");
    let original_tab_id = server.shell_tab_id_for_client(51).unwrap();
    server
        .clients
        .get_mut(&51)
        .unwrap()
        .shell_endpoint_command_in_flight = true;
    server
        .clients
        .get_mut(&51)
        .unwrap()
        .shell_deferred_navigation_response = Some(Vec::new());
    let response = serde_json::json!({
        "id": "create-worktree",
        "result": {
            "type": "worktree_created",
            "tab": { "tab_id": created_tab_id }
        }
    });

    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointResponseChunkReady {
            client_id: 51,
            boot_id: server.client_shell_boot_id.clone(),
            request_id: "create-worktree".into(),
            final_chunk: true,
            data: serde_json::to_vec(&response).unwrap(),
        })
    );
    assert_eq!(
        server.shell_tab_id_for_client(51).as_deref(),
        response
            .pointer("/result/tab/tab_id")
            .and_then(serde_json::Value::as_str)
    );
    assert_eq!(
        server.shell_tab_id_for_client(52).as_deref(),
        Some(original_tab_id.as_str())
    );

    assert!(server.focus_shell_client_on_tab(51, &original_tab_id));
    server
        .clients
        .get_mut(&51)
        .unwrap()
        .shell_endpoint_command_in_flight = true;
    server
        .clients
        .get_mut(&51)
        .unwrap()
        .shell_deferred_navigation_request_id = Some("background-worktree".into());
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellEndpointResponseChunkReady {
            client_id: 51,
            boot_id: server.client_shell_boot_id.clone(),
            request_id: "background-worktree".into(),
            final_chunk: true,
            data: serde_json::to_vec(&response).unwrap(),
        })
    );
    assert_eq!(
        server.shell_tab_id_for_client(51).as_deref(),
        Some(original_tab_id.as_str())
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_local_navigation_does_not_emit_global_focus_transitions() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("independent-focus");
    let first_pane = workspace.tabs[0].root_pane;
    let second_tab = workspace.test_add_tab(Some("second"));
    let second_pane = workspace.tabs[second_tab].root_pane;
    let (first_runtime, mut first_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[?1004h",
            4,
        );
    let (second_runtime, mut second_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[?1004h",
            4,
        );
    workspace.insert_test_runtime(first_pane, first_runtime);
    workspace.insert_test_runtime(second_pane, second_runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let second_tab_id = server.app.public_tab_id(0, second_tab).unwrap();

    let (first_control, _) = connect_matching_test_shell(&mut server, 61);
    let (second_control, _) = connect_matching_test_shell(&mut server, 62);
    let _ = first_control.recv().expect("first snapshot");
    let _ = second_control.recv().expect("second snapshot");
    assert!(server.focus_shell_client_on_tab(62, &second_tab_id));
    server.clients.get_mut(&61).unwrap().outer_terminal_focus = Some(true);
    server.clients.get_mut(&62).unwrap().outer_terminal_focus = Some(true);

    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request(
        62,
        crate::api::ApiRequestMessage {
            request: crate::api::schema::Request {
                id: "focus-own-tab".into(),
                method: crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                    tab_id: second_tab_id,
                }),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        },
    );
    server.app.sync_focus_events();
    assert!(first_input.try_recv().is_err());
    assert!(second_input.try_recv().is_err());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn public_focus_moves_shell_focus_between_tabs() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("public-focus-events");
    let first_pane = workspace.tabs[0].root_pane;
    let second_tab = workspace.test_add_tab(Some("second"));
    let second_pane = workspace.tabs[second_tab].root_pane;
    let (first_runtime, mut first_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[?1004h",
            4,
        );
    let (second_runtime, mut second_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[?1004h",
            4,
        );
    workspace.insert_test_runtime(first_pane, first_runtime);
    workspace.insert_test_runtime(second_pane, second_runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let second_tab_id = server
        .app
        .public_tab_id(0, second_tab)
        .expect("second tab id");

    let (first_control, _) = connect_matching_test_shell(&mut server, 63);
    let (second_control, _) = connect_matching_test_shell(&mut server, 64);
    let _ = first_control.recv().expect("first snapshot");
    let _ = second_control.recv().expect("second snapshot");
    assert!(server.focus_shell_client_on_tab(64, &second_tab_id));
    server.clients.get_mut(&63).unwrap().outer_terminal_focus = Some(true);
    server.clients.get_mut(&64).unwrap().outer_terminal_focus = Some(true);
    assert!(server.app.state.switch_workspace_tab(0, second_tab));

    server.focus_all_shell_clients_on_default_target();

    assert_eq!(
        server.shell_tab_id_for_client(63).as_deref(),
        Some(second_tab_id.as_str())
    );
    assert_eq!(
        first_input.try_recv().expect("previous tab focus lost"),
        Bytes::from_static(b"\x1b[O")
    );
    assert!(
        second_input.try_recv().is_err(),
        "focus gain was duplicated"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn repeated_layout_action_reapplies_controller_geometry() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("layout-geometry");
    let first_pane = workspace.tabs[0].root_pane;
    let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
    workspace.insert_test_runtime(
        first_pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
    );
    workspace.insert_test_runtime(
        second_pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
    );
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let tab_id = server.app.public_tab_id(0, 0).expect("tab id");

    let (control, _) = connect_test_shell(&mut server, 65, 100, 30);
    let _ = control.recv().expect("snapshot");
    let before = server.app.state.workspaces[0].test_runtimes[&first_pane].current_size();
    let (respond_to, _response_rx) = std::sync::mpsc::channel();

    assert!(server.handle_client_shell_api_request(
        65,
        crate::api::ApiRequestMessage {
            request: crate::api::schema::Request {
                id: "resize-layout".into(),
                method: crate::api::schema::Method::LayoutSetSplitRatio(
                    crate::api::schema::LayoutSetSplitRatioParams {
                        tab_id: Some(tab_id),
                        pane_id: None,
                        path: Vec::new(),
                        ratio: 0.8,
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        },
    ));

    let after = server.app.state.workspaces[0].test_runtimes[&first_pane].current_size();
    assert_ne!(after, before);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_tabs_render_accept_input_and_resize_independently() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("independent-geometry");
    let first_pane = workspace.tabs[0].root_pane;
    let second_tab = workspace.test_add_tab(Some("second"));
    let second_pane = workspace.tabs[second_tab].root_pane;
    workspace.insert_test_runtime(
        first_pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"FIRST_TAB"),
    );
    let (second_runtime, mut second_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"SECOND_TAB",
            4,
        );
    workspace.insert_test_runtime(second_pane, second_runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let second_tab_id = server.app.public_tab_id(0, second_tab).unwrap();
    let second_pane_id = server.app.public_pane_id(0, second_pane).unwrap();
    let initial_second_size =
        server.app.state.workspaces[0].test_runtimes[&second_pane].current_size();

    let (first_control, first_render) = connect_test_shell(&mut server, 21, 100, 30);
    let _ = first_control.recv().expect("first snapshot");
    let first_size = server.app.state.workspaces[0].test_runtimes[&first_pane].current_size();
    let singleton_second_size =
        server.app.state.workspaces[0].test_runtimes[&second_pane].current_size();
    assert_ne!(singleton_second_size, initial_second_size);
    assert_eq!(singleton_second_size, first_size);

    let (second_control, second_render) = connect_test_shell(&mut server, 22, 70, 20);
    let _ = second_control.recv().expect("second snapshot");

    assert!(server.focus_shell_client_on_tab(22, &second_tab_id));
    assert!(server.claim_shell_tab_geometry(22, false));
    let second_size = server.app.state.workspaces[0].test_runtimes[&second_pane].current_size();
    assert_ne!(first_size, second_size);
    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&first_pane].current_size(),
        first_size
    );

    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 22,
        pane_id: second_pane_id,
        events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
            "typed".into(),
        )],
    });
    assert_eq!(
        second_input.try_recv().expect("second tab input"),
        Bytes::from_static(b"typed")
    );

    server.render_and_stream();
    let first_surface = match read_server_message(first_render.recv().expect("first surface")) {
        ServerMessage::PaneSurface(surface) => surface,
        other => panic!("expected first pane surface, got {other:?}"),
    };
    let second_surface = match read_server_message(second_render.recv().expect("second surface")) {
        ServerMessage::PaneSurface(surface) => surface,
        other => panic!("expected second pane surface, got {other:?}"),
    };
    assert!(frame_text(&first_surface.frame).contains("FIRST_TAB"));
    assert!(frame_text(&second_surface.frame).contains("SECOND_TAB"));

    assert!(server.handle_server_event(ServerEvent::ClientShellResize {
        client_id: 22,
        surface_cols: 60,
        surface_rows: 16,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
    }));
    let resized_second = server.app.state.workspaces[0].test_runtimes[&second_pane].current_size();
    assert_ne!(resized_second, second_size);
    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&first_pane].current_size(),
        first_size
    );

    assert!(server.focus_shell_client_on_tab(21, &second_tab_id));
    assert!(server.claim_shell_tab_geometry(21, false));
    assert_ne!(
        server.app.state.workspaces[0].test_runtimes[&second_pane].current_size(),
        resized_second
    );

    server.remove_client_and_resize_if_needed(21);
    let singleton_first = server.app.state.workspaces[0].test_runtimes[&first_pane].current_size();
    let singleton_second =
        server.app.state.workspaces[0].test_runtimes[&second_pane].current_size();
    assert_ne!(singleton_first, first_size);
    assert_eq!(singleton_first, singleton_second);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn public_background_tab_create_preserves_client_locations() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("background-create");
    let second_tab = workspace.test_add_tab(Some("second"));
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let workspace_id = server.app.public_workspace_id(0);
    let first_tab_id = server.app.public_tab_id(0, 0).unwrap();
    let second_tab_id = server.app.public_tab_id(0, second_tab).unwrap();

    let (first_control, _) = connect_matching_test_shell(&mut server, 71);
    let (second_control, _) = connect_matching_test_shell(&mut server, 72);
    let _ = first_control.recv().expect("first snapshot");
    let _ = second_control.recv().expect("second snapshot");
    assert!(server.focus_shell_client_on_tab(71, &second_tab_id));

    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: crate::api::schema::Request {
            id: "create-background-tab".into(),
            method: crate::api::schema::Method::TabCreate(crate::api::schema::TabCreateParams {
                workspace_id: Some(workspace_id),
                cwd: None,
                focus: false,
                label: Some("background".into()),
                env: std::collections::HashMap::new(),
            }),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });

    assert_eq!(
        server.shell_tab_id_for_client(71).as_deref(),
        Some(second_tab_id.as_str())
    );
    assert_eq!(
        server.shell_tab_id_for_client(72).as_deref(),
        Some(first_tab_id.as_str())
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn public_workspace_focus_preserves_each_clients_remembered_tabs() {
    let mut server = test_headless_server();
    let mut first = crate::workspace::Workspace::test_new("first");
    let second_tab = first.test_add_tab(Some("second"));
    let second = crate::workspace::Workspace::test_new("second");
    server.app.state.workspaces = vec![first, second];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let first_workspace_id = server.app.public_workspace_id(0);
    let second_workspace_id = server.app.public_workspace_id(1);
    let first_tab_id = server.app.public_tab_id(0, 0).unwrap();
    let second_tab_id = server.app.public_tab_id(0, second_tab).unwrap();

    let (first_control, _) = connect_test_shell(&mut server, 41, 100, 30);
    let (second_control, _) = connect_test_shell(&mut server, 42, 80, 24);
    let _ = first_control.recv().expect("first snapshot");
    let _ = second_control.recv().expect("second snapshot");
    assert!(server.focus_shell_client_on_tab(41, &second_tab_id));

    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: crate::api::schema::Request {
            id: "focus-second-workspace".into(),
            method: crate::api::schema::Method::WorkspaceFocus(
                crate::api::schema::WorkspaceTarget {
                    workspace_id: second_workspace_id.clone(),
                },
            ),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });

    let first_location = server.clients[&41].shell_location.as_ref().unwrap();
    let second_location = server.clients[&42].shell_location.as_ref().unwrap();
    assert_eq!(
        first_location.focused_workspace_id.as_deref(),
        Some(second_workspace_id.as_str())
    );
    assert_eq!(
        second_location.focused_workspace_id.as_deref(),
        Some(second_workspace_id.as_str())
    );
    assert_eq!(
        first_location.active_tab_ids[&first_workspace_id],
        second_tab_id
    );
    assert_eq!(
        second_location.active_tab_ids[&first_workspace_id],
        first_tab_id
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn public_api_focus_replaces_every_client_shell_projection() {
    let mut server = test_headless_server();
    let first = crate::workspace::Workspace::test_new("first");
    let second = crate::workspace::Workspace::test_new("second");
    server.app.state.workspaces = vec![first, second];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let second_id = server.app.session_snapshot().workspaces[1]
        .workspace_id
        .clone();

    let (writer, control_rx, render_rx) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: 9,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            writer,
        })
    );
    let initial_revision = client_shell_snapshot(read_server_message(
        control_rx.recv().expect("initial snapshot"),
    ))
    .revision;

    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: crate::api::schema::Request {
            id: "test.client.shell.workspace.focus".into(),
            method: crate::api::schema::Method::WorkspaceFocus(
                crate::api::schema::WorkspaceTarget {
                    workspace_id: second_id.clone(),
                },
            ),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    assert_eq!(server.app.state.active, Some(1));
    server.render_and_stream();

    let replacement = client_shell_snapshot(read_server_message(
        control_rx.recv().expect("replacement snapshot"),
    ));
    assert!(replacement.revision > initial_revision);
    assert_eq!(
        replacement.focused_workspace_id.as_deref(),
        Some(second_id.as_str())
    );
    match read_server_message(render_rx.recv().expect("replacement pane surface")) {
        ServerMessage::PaneSurface(surface) => {
            assert_eq!(surface.projection_revision, replacement.revision);
        }
        other => panic!("expected replacement pane surface, got {other:?}"),
    }
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_input_targets_runtime_without_server_shell_classification() {
    let mut server = test_headless_server();
    let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1000h\x1b[?1006h");
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    server.clients.insert(
        11,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );

    assert!(
        server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id,
            events: vec![
                crate::protocol::ClientPaneInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('c'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                    repeat_count: 1,
                    shifted_codepoint: None,
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: None,
                    windows_record: None,
                },
                crate::protocol::ClientPaneInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('c'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Release,
                    repeat_count: 1,
                    shifted_codepoint: None,
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: None,
                    windows_record: None,
                },
                crate::protocol::ClientPaneInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('x'),
                    modifiers: crossterm::event::KeyModifiers::ALT.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                    repeat_count: 1,
                    shifted_codepoint: None,
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: None,
                    windows_record: None,
                },
                crate::protocol::ClientPaneInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Down(
                        crate::protocol::ClientMouseButton::Left,
                    ),
                    position: crate::protocol::ClientMousePosition::Cell { column: 2, row: 1 },
                    geometry: None,
                    modifiers: 0,
                    lines: 3,
                },
            ],
        })
    );
    assert_eq!(
        input_rx.try_recv().expect("targeted pane interrupt"),
        Bytes::from_static(&[0x03])
    );
    assert_eq!(
        input_rx.try_recv().expect("targeted pane alt key"),
        Bytes::from_static(b"\x1bx")
    );
    assert_eq!(
        input_rx.try_recv().expect("targeted pane mouse click"),
        Bytes::from_static(b"\x1b[<0;3;2M")
    );
    assert_eq!(server.foreground_client_id, Some(11));
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    assert!(server.paste_client_clipboard_image_path(
        11,
        crate::protocol::ClientClipboardImageTarget::Pane(pane_id.clone()),
        "/tmp/client-image.png".into(),
    ));
    assert_eq!(
        input_rx.try_recv().expect("targeted clipboard image path"),
        Bytes::from_static(b"/tmp/client-image.png")
    );
    assert!(!server.paste_client_clipboard_image_path(
        11,
        crate::protocol::ClientClipboardImageTarget::DirectTerminal,
        "/tmp/wrong-target.png".into(),
    ));
    assert!(!server.paste_client_clipboard_image_path(
        11,
        crate::protocol::ClientClipboardImageTarget::Popup("missing-popup".into()),
        "/tmp/wrong-target.png".into(),
    ));
    assert!(input_rx.try_recv().is_err());

    let (workspace_index, runtime_pane_id) = server
        .app
        .parse_pane_id(&pane_id)
        .expect("runtime pane target");
    let runtime = server
        .app
        .state
        .runtime_for_pane_in_workspace(
            &server.app.terminal_runtimes,
            workspace_index,
            runtime_pane_id,
        )
        .expect("focused runtime");
    assert_eq!(runtime.current_size(), (24, 79));
    assert!(input_rx.try_recv().is_err(), "legacy release emitted bytes");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_hidden_pane_rejects_presses_but_accepts_releases() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("hidden-input");
    let hidden_tab = workspace.test_add_tab(Some("hidden"));
    let hidden_pane = workspace.tabs[hidden_tab].root_pane;
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[>3u",
            4,
        );
    workspace.insert_test_runtime(hidden_pane, runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    let pane_id = server.app.public_pane_id(0, hidden_pane).unwrap();
    server.clients.insert(
        11,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    let key = |kind| crate::protocol::ClientPaneInputEvent::Key {
        code: crate::protocol::ClientKeyCode::Char('x'),
        modifiers: 0,
        kind,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: None,
        tracks_release: true,
        physical_key_id: Some(0x2d),
        windows_record: None,
    };

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id: pane_id.clone(),
            events: vec![key(crate::protocol::ClientKeyKind::Press)],
        })
    );
    assert!(input_rx.try_recv().is_err());
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id,
            events: vec![key(crate::protocol::ClientKeyKind::Release)],
        })
    );
    assert!(!input_rx.recv().await.expect("encoded release").is_empty());
    assert_eq!(server.foreground_client_id, None);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_streams_and_targets_popup_terminal_content() {
    let mut server = test_headless_server();
    let mut pane_input = install_focused_test_runtime(&mut server, b"base-pane");
    let (popup_runtime, mut popup_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            40,
            12,
            0,
            b"POPUP_SHELL_LIVE\x1b_Ga=T,f=32,t=d,i=9,p=4,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
            4,
        );
    let (_, popup_terminal_id) = server.app.install_test_popup_runtime(popup_runtime);

    let (writer, control_rx, render_rx) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: 12,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 10,
            cell_height_px: 20,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            writer,
        })
    );
    let _snapshot = client_shell_snapshot(read_server_message(
        control_rx.recv().expect("shell snapshot"),
    ));

    server.render_and_stream();
    let ServerMessage::PaneSurface(surface) =
        read_server_message(render_rx.recv().expect("popup surface"))
    else {
        panic!("expected pane surface");
    };
    let popup = surface.popup.as_deref().expect("popup terminal surface");
    assert_eq!(popup.terminal_id, popup_terminal_id.as_str());
    assert!(frame_text(&popup.frame).contains("POPUP_SHELL_LIVE"));
    assert_eq!((popup.frame.width, popup.frame.height), (37, 9));
    assert!(popup.frame.cursor.is_some());
    assert_eq!(surface.graphics.assets.len(), 1);
    assert_eq!(surface.graphics.placements.len(), 1);
    assert!(matches!(
        surface.graphics.placements[0].asset.source,
        crate::protocol::SurfaceGraphicsSource::Terminal {
            target: crate::protocol::SurfaceGraphicsTarget::Popup { .. },
            image_id: 9,
        }
    ));

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 12,
            pane_id: server.app.session_snapshot().focused_pane_id.unwrap(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                "must-not-leak".into(),
            )],
        })
    );
    assert!(pane_input.try_recv().is_err());

    assert!(server.handle_server_event(ServerEvent::ClientShellResize {
        client_id: 12,
        surface_cols: 60,
        surface_rows: 15,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
    }));
    assert_eq!(
        server
            .app
            .terminal_runtimes
            .get(&popup_terminal_id)
            .expect("popup runtime")
            .current_size(),
        (5, 27)
    );

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPopupInput {
            client_id: 12,
            terminal_id: popup_terminal_id.to_string(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                "typed".into()
            )],
        })
    );
    assert_eq!(
        popup_input.try_recv().expect("popup input"),
        Bytes::from_static(b"typed")
    );
    assert!(server.paste_client_clipboard_image_path(
        12,
        crate::protocol::ClientClipboardImageTarget::Popup(popup_terminal_id.to_string()),
        "/tmp/popup-image.png".into(),
    ));
    assert_eq!(
        popup_input.try_recv().expect("popup clipboard image path"),
        Bytes::from_static(b"/tmp/popup-image.png")
    );
    assert!(!server.paste_client_clipboard_image_path(
        12,
        crate::protocol::ClientClipboardImageTarget::Popup("stale-popup".into()),
        "/tmp/wrong-popup.png".into(),
    ));
    assert!(popup_input.try_recv().is_err());

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPopupInput {
            client_id: 12,
            terminal_id: "stale-popup".into(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                "wrong".into()
            )],
        })
    );
    assert!(popup_input.try_recv().is_err());

    assert!(server.app.close_popup_pane());
    server.render_and_stream();
    let ServerMessage::PaneSurface(surface) =
        read_server_message(render_rx.recv().expect("popup close surface"))
    else {
        panic!("expected pane surface after popup close");
    };
    assert!(surface.popup.is_none());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn terminal_popup_is_visible_and_modal_only_on_its_owning_tab() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("tab-popup");
    let first_pane = workspace.tabs[0].root_pane;
    let second_tab = workspace.test_add_tab(Some("second"));
    let second_pane = workspace.tabs[second_tab].root_pane;
    workspace.insert_test_runtime(
        first_pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 23, b"FIRST"),
    );
    let (second_runtime, mut second_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80, 23, 0, b"SECOND", 4,
        );
    workspace.insert_test_runtime(second_pane, second_runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let first_tab_id = server.app.public_tab_id(0, 0).unwrap();
    let second_tab_id = server.app.public_tab_id(0, second_tab).unwrap();
    let second_pane_id = server.app.public_pane_id(0, second_pane).unwrap();

    let (first_control, first_render) = connect_matching_test_shell(&mut server, 31);
    let (second_control, second_render) = connect_matching_test_shell(&mut server, 32);
    let _ = first_control.recv().expect("first snapshot");
    let _ = second_control.recv().expect("second snapshot");
    assert!(server.focus_shell_client_on_tab(32, &second_tab_id));
    assert!(server.claim_shell_tab_geometry(32, false));

    let (popup_runtime, mut popup_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            40,
            12,
            0,
            b"POPUP\x1b[>3u",
            4,
        );
    let (_, popup_terminal_id) = server.app.install_test_popup_runtime(popup_runtime);
    server.popup_owner_tab_id = Some(first_tab_id);
    assert!(server.apply_shell_tab_geometry(31, false));
    let popup_size = server
        .app
        .terminal_runtimes
        .get(&popup_terminal_id)
        .unwrap()
        .current_size();
    connect_pending_terminal_client(&mut server, 33);
    assert!(
        server.handle_server_event(ServerEvent::ClientAttachTerminal {
            client_id: 33,
            terminal_id: popup_terminal_id.to_string(),
            takeover: false,
        })
    );
    assert_ne!(
        server
            .app
            .terminal_runtimes
            .get(&popup_terminal_id)
            .unwrap()
            .current_size(),
        popup_size
    );
    assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 33 }));
    assert_eq!(
        server
            .app
            .terminal_runtimes
            .get(&popup_terminal_id)
            .unwrap()
            .current_size(),
        popup_size
    );
    server.render_and_stream();
    assert!(recv_pane_surface(&first_render, "popup owner surface")
        .popup
        .is_some());
    assert!(recv_pane_surface(&second_render, "other tab surface")
        .popup
        .is_none());

    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 32,
        pane_id: second_pane_id,
        events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
            "typed".into(),
        )],
    });
    assert_eq!(
        second_input.try_recv().expect("other tab input"),
        Bytes::from_static(b"typed")
    );

    let popup_key = |kind| crate::protocol::ClientPaneInputEvent::Key {
        code: crate::protocol::ClientKeyCode::Char('x'),
        modifiers: 0,
        kind,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: (kind == crate::protocol::ClientKeyKind::Press).then(|| "x".into()),
        tracks_release: true,
        physical_key_id: Some(0x2d),
        windows_record: None,
    };
    server.handle_server_event(ServerEvent::ClientShellPopupInput {
        client_id: 31,
        terminal_id: popup_terminal_id.to_string(),
        events: vec![popup_key(crate::protocol::ClientKeyKind::Press)],
    });
    assert!(
        !tokio::time::timeout(Duration::from_secs(1), popup_input.recv())
            .await
            .expect("popup press timed out")
            .expect("popup press")
            .is_empty()
    );
    assert!(server.focus_shell_client_on_tab(31, &second_tab_id));
    server.handle_server_event(ServerEvent::ClientShellPopupInput {
        client_id: 31,
        terminal_id: popup_terminal_id.to_string(),
        events: vec![popup_key(crate::protocol::ClientKeyKind::Release)],
    });
    assert!(
        !tokio::time::timeout(Duration::from_secs(1), popup_input.recv())
            .await
            .expect("popup release timed out")
            .expect("popup release after navigation")
            .is_empty()
    );

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPopupInput {
            client_id: 32,
            terminal_id: popup_terminal_id.to_string(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                "wrong".into()
            )],
        })
    );
    assert!(popup_input.try_recv().is_err());

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_release_under_popup_renders_when_it_resets_scrollback() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("popup-release-scroll");
    let pane_id = workspace.tabs[0].root_pane;
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            2,
            10_000,
            b"one\r\ntwo\r\nthree\r\n\x1b[>3u",
            4,
        );
    runtime.scroll_up(1);
    assert!(runtime
        .scroll_metrics()
        .is_some_and(|metrics| metrics.offset_from_bottom > 0));
    workspace.insert_test_runtime(pane_id, runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let popup_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"");
    server.app.install_test_popup_runtime(popup_runtime);
    server.clients.insert(
        11,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.foreground_client_id = Some(11);

    let render_impact =
        server.handle_server_event_with_render_impact(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id: public_pane_id,
            events: vec![crate::protocol::ClientPaneInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('x'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Release,
                repeat_count: 1,
                shifted_codepoint: None,
                generated_text: None,
                tracks_release: true,
                physical_key_id: Some(0x2d),
                windows_record: None,
            }],
        });

    assert_eq!(render_impact, RenderImpact::Full);
    assert!(!input_rx.recv().await.expect("encoded release").is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_text_input_renders_only_when_resetting_scrollback() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("scrolled-input");
    let pane_id = workspace.tabs[0].root_pane;
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            2,
            10_000,
            b"one\r\ntwo\r\nthree\r\n",
            4,
        );
    runtime.scroll_up(1);
    assert!(runtime
        .scroll_metrics()
        .is_some_and(|metrics| metrics.offset_from_bottom > 0));
    workspace.insert_test_runtime(pane_id, runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    server.clients.insert(
        11,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.foreground_client_id = Some(11);

    let render_impact =
        server.handle_server_event_with_render_impact(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id: public_pane_id.clone(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                "x".to_owned(),
            )],
        });

    assert_eq!(render_impact, RenderImpact::Full);
    assert_eq!(
        input_rx.try_recv().expect("text must reach the PTY"),
        Bytes::from_static(b"x")
    );
    assert_eq!(
        server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .and_then(|runtime| runtime.scroll_metrics())
            .map(|metrics| metrics.offset_from_bottom),
        Some(0)
    );

    let render_impact =
        server.handle_server_event_with_render_impact(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id: public_pane_id,
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                "y".to_owned(),
            )],
        });
    assert_eq!(render_impact, RenderImpact::None);
    assert_eq!(
        input_rx.try_recv().expect("second text must reach the PTY"),
        Bytes::from_static(b"y")
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_mouse_motion_delivers_without_render_when_foreground() {
    let mut server = test_headless_server();
    let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1003h\x1b[?1006h");
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    server.clients.insert(
        11,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.foreground_client_id = Some(11);
    assert!(server.claim_unowned_shell_tab_geometry(11, false));

    let render_impact =
        server.handle_server_event_with_render_impact(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id,
            events: vec![crate::protocol::ClientPaneInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                position: crate::protocol::ClientMousePosition::Cell { column: 2, row: 1 },
                geometry: None,
                modifiers: 0,
                lines: 0,
            }],
        });

    assert_eq!(render_impact, RenderImpact::None);
    assert!(
        input_rx.try_recv().is_ok(),
        "motion must still reach the PTY"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_mouse_motion_promotes_and_requests_render() {
    let mut server = test_headless_server();
    let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1003h\x1b[?1006h");
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    server.clients.insert(
        11,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );

    let render_impact =
        server.handle_server_event_with_render_impact(ServerEvent::ClientShellPaneInput {
            client_id: 11,
            pane_id,
            events: vec![crate::protocol::ClientPaneInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                position: crate::protocol::ClientMousePosition::Cell { column: 2, row: 1 },
                geometry: None,
                modifiers: 0,
                lines: 0,
            }],
        });

    assert_eq!(render_impact, RenderImpact::Full);
    assert_eq!(server.foreground_client_id, Some(11));
    assert!(
        input_rx.try_recv().is_ok(),
        "motion must still reach the PTY"
    );
    shutdown_test_runtimes(&mut server);
}

fn install_focused_test_runtime(
    server: &mut HeadlessServer,
    terminal_bytes: &[u8],
) -> tokio::sync::mpsc::Receiver<Bytes> {
    let mut workspace = crate::workspace::Workspace::test_new("focus-reporting");
    let pane_id = workspace.tabs[0].root_pane;
    let (runtime, input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            terminal_bytes,
            4,
        );
    workspace.insert_test_runtime(pane_id, runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    input_rx
}

fn retained_test_server(
    initial_screen: &[u8],
) -> (
    HeadlessServer,
    std::sync::mpsc::Receiver<Vec<u8>>,
    crate::layout::PaneId,
) {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("test");
    let pane_id = workspace.focused_pane_id().expect("focused pane");
    workspace.insert_test_runtime(
        pane_id,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, initial_screen),
    );
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;

    let (client_tx, _client_control_rx, client_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.sync_foreground_client_state();
    assert!(server.claim_unowned_shell_tab_geometry(1, true));

    (server, client_rx, pane_id)
}

#[test]
fn server_keybinding_filter_keeps_whole_config_failures() {
    assert!(!config::is_keybinding_config_diagnostic(
        "config parse error: invalid value at `keys.new_tab = @`; using defaults"
    ));
    assert!(!config::is_keybinding_config_diagnostic(
        "config read error: permission denied at keys.toml; using defaults"
    ));
    assert!(config::is_keybinding_config_diagnostic(
        "unsafe direct keybinding: keys.close_pane would intercept typing"
    ));
}

#[test]
fn client_shell_host_theme_follows_foreground_client() {
    let mut server = test_headless_server();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.foreground_client_id = Some(1);

    let dark = protocol::ClientHostColor {
        r: 20,
        g: 30,
        b: 40,
    };
    let blue = protocol::ClientHostColor {
        r: 10,
        g: 20,
        b: 200,
    };
    assert!(
        server.handle_server_event(ServerEvent::ClientShellHostTheme {
            client_id: 1,
            update: protocol::ClientHostThemeUpdate::DefaultColor {
                kind: protocol::ClientHostDefaultColorKind::Background,
                color: dark,
            },
        })
    );
    assert!(
        server.handle_server_event(ServerEvent::ClientShellHostTheme {
            client_id: 1,
            update: protocol::ClientHostThemeUpdate::PaletteColors(vec![(4, blue)]),
        })
    );
    server.handle_server_event(ServerEvent::ClientShellHostTheme {
        client_id: 1,
        update: protocol::ClientHostThemeUpdate::Appearance(protocol::ClientHostAppearance::Dark),
    });
    assert_eq!(
        server.app.state.host_terminal_theme.background,
        Some(dark.into())
    );
    assert_eq!(
        server.app.state.host_terminal_theme.palette[4],
        Some(blue.into())
    );
    assert_eq!(
        server.app.state.host_terminal_appearance,
        Some(crate::terminal_theme::HostAppearance::Dark)
    );
    assert!(server.app.state.host_terminal_appearance_explicit);

    let light = protocol::ClientHostColor {
        r: 240,
        g: 230,
        b: 220,
    };
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellHostTheme {
            client_id: 2,
            update: protocol::ClientHostThemeUpdate::DefaultColor {
                kind: protocol::ClientHostDefaultColorKind::Background,
                color: light,
            },
        })
    );
    assert_eq!(
        server.app.state.host_terminal_theme.background,
        Some(dark.into())
    );

    server.foreground_client_id = Some(2);
    server.sync_foreground_client_state();
    assert_eq!(
        server.app.state.host_terminal_theme.background,
        Some(light.into())
    );
    assert_eq!(
        server.app.state.host_terminal_appearance,
        Some(crate::terminal_theme::HostAppearance::Light)
    );
    assert!(!server.app.state.host_terminal_appearance_explicit);
}

#[test]
fn terminal_clients_store_known_cell_geometry_independently_of_pixel_mouse() {
    let mut server = test_headless_server();

    let (writer, _control_rx, _render_rx) = test_client_writer();
    assert!(!server.handle_server_event(ServerEvent::ClientConnected {
        client_id: 7,
        cols: 80,
        rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: true,
        writer,
    }));
    assert!(!server.clients[&7].pixel_mouse);
    assert_eq!(
        server.clients[&7].cell_size,
        crate::kitty_graphics::HostCellSize::default()
    );

    let (writer, _control_rx, _render_rx) = test_client_writer();
    assert!(!server.handle_server_event(ServerEvent::ClientConnected {
        client_id: 8,
        cols: 80,
        rows: 24,
        cell_width_px: 10,
        cell_height_px: 20,
        pixel_mouse: false,
        writer,
    }));
    assert!(!server.clients[&8].pixel_mouse);
    assert_eq!(
        server.clients[&8].cell_size,
        crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        }
    );
}

#[test]
fn terminal_attach_rejects_missing_terminal_and_removes_client() {
    let mut server = test_headless_server();
    let (writer, control_rx, _render_rx) = test_client_writer();

    assert!(!server.handle_server_event(ServerEvent::ClientConnected {
        client_id: 7,
        cols: 80,
        rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        writer,
    }));
    assert!(matches!(
        server.clients.get(&7).map(|client| &client.mode),
        Some(ClientConnectionMode::TerminalPending)
    ));

    assert!(
        !server.handle_server_event(ServerEvent::ClientAttachTerminal {
            client_id: 7,
            terminal_id: "term_missing".to_owned(),
            takeover: false,
        })
    );
    assert!(!server.clients.contains_key(&7));
    let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
    assert_eq!(
        reason,
        Some("terminal attach failed: terminal term_missing not found".to_owned())
    );
}

fn with_terminal_session_test_server(
    test: impl FnOnce(&mut HeadlessServer, crate::terminal::TerminalId, String, String),
) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("test");
    let pane_id = workspace.tabs[0].root_pane;
    let terminal_id = workspace.terminal_id(pane_id).expect("terminal id").clone();
    let terminal_id_string = terminal_id.to_string();
    let public_pane_id = format!("{}:p1", workspace.id);
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.terminal_runtimes.insert(
        terminal_id.clone(),
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
    );

    test(&mut server, terminal_id, terminal_id_string, public_pane_id);

    drop(server);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

fn connect_pending_terminal_client(server: &mut HeadlessServer, client_id: u64) {
    let _control_rx = connect_pending_terminal_client_with_control_rx(server, client_id);
}

fn connect_pending_terminal_client_with_control_rx(
    server: &mut HeadlessServer,
    client_id: u64,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (writer, control_rx, _render_rx) = test_client_writer();
    assert!(!server.handle_server_event(ServerEvent::ClientConnected {
        client_id,
        cols: 100,
        rows: 30,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        writer,
    }));
    assert!(matches!(
        server.clients.get(&client_id).map(|client| &client.mode),
        Some(ClientConnectionMode::TerminalPending)
    ));
    control_rx
}

#[test]
fn explicit_agent_history_read_requires_idle_on_alternate_screen() {
    with_terminal_session_test_server(
        |server, terminal_id, _terminal_id_string, public_pane_id| {
            let terminal = server
                .app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal");
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.state = crate::detect::AgentState::Working;
            server.app.terminal_runtimes.insert(
                terminal_id,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(
                    80,
                    24,
                    b"\x1b[?1049hworking",
                ),
            );
            let request = api::schema::Request {
                id: "read".into(),
                method: api::schema::Method::AgentRead(api::schema::AgentReadParams {
                    target: public_pane_id.clone(),
                    source: api::schema::ReadSource::Recent,
                    lines: Some(200),
                    format: api::schema::ReadFormat::Text,
                    strip_ansi: true,
                }),
            };

            assert_eq!(
                    server.agent_read_not_idle_error(&request),
                    Some(api::schema::ErrorBody {
                        code: "agent_not_idle".into(),
                        message: format!(
                            "cannot read 200 lines while {public_pane_id} is working: its alternate-screen history can only be captured by scrolling while idle. Wait and retry, or use --source visible"
                        ),
                    })
                );

            let mut default_request = request.clone();
            let api::schema::Method::AgentRead(params) = &mut default_request.method else {
                unreachable!();
            };
            params.lines = None;
            assert_eq!(server.agent_read_not_idle_error(&default_request), None);

            let mut visible_request = request;
            let api::schema::Method::AgentRead(params) = &mut visible_request.method else {
                unreachable!();
            };
            params.source = api::schema::ReadSource::Visible;
            assert_eq!(server.agent_read_not_idle_error(&visible_request), None);
        },
    );
}

#[test]
fn terminal_attach_disconnect_restores_client_shell_pane_size() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("test");
    let second_tab = workspace.test_add_tab(Some("second"));
    let pane_id = workspace.tabs[0].root_pane;
    let terminal_id = workspace.terminal_id(pane_id).expect("terminal id").clone();
    let terminal_id_string = terminal_id.to_string();
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    let second_tab_id = server
        .app
        .public_tab_id(0, second_tab)
        .expect("second tab id");
    server.app.terminal_runtimes.insert(
        terminal_id.clone(),
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
    );
    server.clients.insert(
        1,
        ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.foreground_client_id = Some(1);
    server.sync_foreground_client_state();
    server.reconcile_client_shell_locations();
    assert!(server.claim_unowned_shell_tab_geometry(1, true));
    let expected_shell_size = server
        .app
        .terminal_runtimes
        .get(&terminal_id)
        .expect("runtime")
        .current_size();

    connect_pending_terminal_client(&mut server, 2);
    assert!(
        server.handle_server_event(ServerEvent::ClientAttachTerminal {
            client_id: 2,
            terminal_id: terminal_id_string,
            takeover: false,
        })
    );
    assert_eq!(server.foreground_client_id, Some(1));
    assert!(server
        .app
        .state
        .direct_attach_resize_locks
        .contains(&terminal_id));
    assert_eq!(
        server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .current_size(),
        (30, 100)
    );

    assert!(server.focus_shell_client_on_tab(1, &second_tab_id));
    assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 2 }));
    assert!(!server
        .app
        .state
        .direct_attach_resize_locks
        .contains(&terminal_id));
    assert_eq!(
        server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .current_size(),
        expected_shell_size
    );

    drop(server);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

#[test]
fn terminal_observe_allows_multiple_clients_without_attach_ownership() {
    with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
        let initial_size = server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .current_size();

        for client_id in [7, 8] {
            connect_pending_terminal_client(server, client_id);
            assert!(
                server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id,
                    target: terminal_id_string.clone(),
                })
            );
        }

        assert!(server.terminal_attach_owners.is_empty());
        assert!(!server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size(),
            initial_size
        );
        assert_eq!(
            terminal_stream_client_ids(&server.clients, &terminal_id_string).len(),
            2
        );
    });
}

#[test]
fn direct_terminal_observer_keeps_hidden_pty_source_renderable_with_client_shell() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("test");
    let background_tab = workspace.test_add_tab(Some("background"));
    let background_pane = workspace.tabs[background_tab].root_pane;
    let hidden_pane = workspace.tabs[0].root_pane;
    let terminal_id = workspace
        .terminal_id(background_pane)
        .expect("background terminal id")
        .to_string();
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.ensure_test_terminals();

    let (shell_writer, _shell_control_rx, _shell_render_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(shell_writer),
        ),
    );
    assert!(!server.pty_sources_visible_to_any_render_target(&HashSet::from([background_pane])));

    let (observer_writer, _observer_control_rx, _observer_render_rx) = test_client_writer();
    server.clients.insert(
        2,
        ClientConnection::new_with_mode(
            ClientConnectionMode::TerminalObserve { terminal_id },
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(observer_writer),
        ),
    );

    assert!(server.pty_sources_visible_to_any_render_target(&HashSet::from([background_pane])));
    server.sync_immediate_pty_sources();
    assert!(server.app.render_dirty.request_pty(background_pane));
    assert!(server.has_pending_presentation_work(false, false));
    assert!(server.app.render_dirty.request_pty(hidden_pane));
}

#[test]
fn terminal_observe_resolves_public_pane_id() {
    with_terminal_session_test_server(|server, terminal_id, _, public_pane_id| {
        connect_pending_terminal_client(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientObserveTerminal {
                client_id: 7,
                target: public_pane_id,
            })
        );

        assert!(matches!(
            server.clients.get(&7).map(|client| &client.mode),
            Some(ClientConnectionMode::TerminalObserve { terminal_id: observed })
                if observed == &terminal_id.to_string()
        ));
    });
}

#[test]
fn terminal_control_resolves_public_pane_id_and_takes_ownership() {
    with_terminal_session_test_server(|server, terminal_id, terminal_id_string, public_pane_id| {
        connect_pending_terminal_client(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 7,
                target: public_pane_id,
                takeover: false,
            })
        );

        assert!(matches!(
            server.clients.get(&7).map(|client| &client.mode),
            Some(ClientConnectionMode::TerminalAttach { terminal_id: attached })
                if attached == &terminal_id_string
        ));
        assert_eq!(
            server.terminal_attach_owners.get(&terminal_id_string),
            Some(&7)
        );
        assert!(server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
    });
}

#[test]
fn terminal_control_rejects_attach_during_alt_screen_read() {
    with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
        let (respond_to, _response_rx) = std::sync::mpsc::channel();
        server.pending_alt_screen_reads.push(
            crate::server::alt_screen_read::PendingAltScreenRead::start(
                terminal_id,
                "read".into(),
                respond_to,
                "fallback".into(),
                api::schema::PaneReadResult {
                    pane_id: "w1:p1".into(),
                    workspace_id: "w1".into(),
                    tab_id: "w1:t1".into(),
                    source: api::schema::ReadSource::Recent,
                    format: api::schema::ReadFormat::Text,
                    text: String::new(),
                    revision: 0,
                    truncated: false,
                },
                120,
                false,
                crate::terminal::ScreenSnapshot {
                    cols: 80,
                    rows: Vec::new(),
                },
                0,
                Instant::now(),
            ),
        );
        let control_rx = connect_pending_terminal_client_with_control_rx(server, 7);

        assert!(
            !server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 7,
                target: terminal_id_string.clone(),
                takeover: false,
            })
        );
        assert!(!server.clients.contains_key(&7));
        assert!(!server
            .terminal_attach_owners
            .contains_key(&terminal_id_string));
        let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
        assert_eq!(
                reason,
                Some(format!(
                    "terminal attach failed: terminal {terminal_id_string} has a read in progress; retry"
                ))
            );
    });
}

#[test]
fn terminal_control_rejects_second_controller_without_takeover() {
    with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
        connect_pending_terminal_client(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 7,
                target: terminal_id_string.clone(),
                takeover: false,
            })
        );

        connect_pending_terminal_client(server, 8);
        assert!(
            !server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 8,
                target: terminal_id_string.clone(),
                takeover: false,
            })
        );

        assert!(server.clients.contains_key(&7));
        assert!(!server.clients.contains_key(&8));
        assert_eq!(
            server.terminal_attach_owners.get(&terminal_id_string),
            Some(&7)
        );
    });
}

#[test]
fn terminal_control_takeover_replaces_existing_controller() {
    with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
        connect_pending_terminal_client(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 7,
                target: terminal_id_string.clone(),
                takeover: false,
            })
        );

        connect_pending_terminal_client(server, 8);
        assert!(
            server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 8,
                target: terminal_id_string.clone(),
                takeover: true,
            })
        );

        assert!(!server.clients.contains_key(&7));
        assert!(server.clients.contains_key(&8));
        assert_eq!(
            server.terminal_attach_owners.get(&terminal_id_string),
            Some(&8)
        );
    });
}

#[test]
fn terminal_observe_can_coexist_with_terminal_control() {
    with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
        connect_pending_terminal_client(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 7,
                target: terminal_id_string.clone(),
                takeover: false,
            })
        );

        connect_pending_terminal_client(server, 8);
        assert!(
            server.handle_server_event(ServerEvent::ClientObserveTerminal {
                client_id: 8,
                target: terminal_id_string.clone(),
            })
        );

        assert_eq!(
            server.terminal_attach_owners.get(&terminal_id_string),
            Some(&7)
        );
        assert!(matches!(
            server.clients.get(&8).map(|client| &client.mode),
            Some(ClientConnectionMode::TerminalObserve { terminal_id })
                if terminal_id == &terminal_id_string
        ));
        assert_eq!(
            terminal_stream_client_ids(&server.clients, &terminal_id_string).len(),
            2
        );
    });
}

#[test]
fn terminal_control_detach_sends_shutdown_before_removal() {
    with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
        let control_rx = connect_pending_terminal_client_with_control_rx(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientControlTerminal {
                client_id: 7,
                target: terminal_id_string.clone(),
                takeover: false,
            })
        );

        assert!(server.handle_server_event(ServerEvent::ClientDetach { client_id: 7 }));

        assert!(!server.clients.contains_key(&7));
        assert!(!server
            .terminal_attach_owners
            .contains_key(&terminal_id_string));
        let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
        assert_eq!(reason, Some("detached".to_owned()));
    });
}

#[test]
fn terminal_observe_rejects_later_attach_upgrade() {
    with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
        connect_pending_terminal_client(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientObserveTerminal {
                client_id: 7,
                target: terminal_id_string.clone(),
            })
        );
        assert!(
            !server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 7,
                terminal_id: terminal_id_string,
                takeover: true,
            })
        );

        assert!(!server.clients.contains_key(&7));
        assert!(server.terminal_attach_owners.is_empty());
        assert!(!server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
    });
}

#[test]
fn terminal_attach_rejects_later_observe_and_clears_ownership() {
    with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
        connect_pending_terminal_client(server, 7);
        assert!(
            server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 7,
                terminal_id: terminal_id_string.clone(),
                takeover: false,
            })
        );
        assert_eq!(
            server.terminal_attach_owners.get(&terminal_id_string),
            Some(&7)
        );
        assert!(server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));

        assert!(
            !server.handle_server_event(ServerEvent::ClientObserveTerminal {
                client_id: 7,
                target: terminal_id_string.clone(),
            })
        );

        assert!(!server.clients.contains_key(&7));
        assert!(server.terminal_attach_owners.is_empty());
        assert!(!server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
    });
}

#[test]
fn unchanged_git_refresh_does_not_request_headless_render() {
    let mut server = test_headless_server();
    server.app.git_refresh_in_flight = true;
    let mut workspace = crate::workspace::Workspace::test_new("one");
    let workspace_id = workspace.id.clone();
    let cwd = workspace.identity_cwd.clone();
    workspace.cached_auto_label = "cached".into();
    workspace.cached_git_status_key = cwd.clone();
    workspace.cached_git_branch = None;
    server.app.state.workspaces.push(workspace);

    let changed = server.handle_internal_event_with_forwarding(AppEvent::GitStatusRefreshed {
        results: vec![crate::workspace::WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd: cwd.clone(),
            status_cache_key: cwd,
            demand: crate::workspace::GitStatusRefreshDemand::ALL,
            auto_label: "cached".into(),
            branch: None,
            ahead_behind: None,
            space: None,
        }],
        cache_updates: Vec::new(),
    });

    assert!(!changed);
    assert!(!server.app.git_refresh_in_flight);
}

#[test]
fn changed_git_refresh_requests_headless_render() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("one");
    let workspace_id = workspace.id.clone();
    let cwd = workspace.identity_cwd.clone();
    server.app.state.workspaces.push(workspace);

    let changed = server.handle_internal_event_with_forwarding(AppEvent::GitStatusRefreshed {
        results: vec![crate::workspace::WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd: cwd.clone(),
            status_cache_key: cwd,
            demand: crate::workspace::GitStatusRefreshDemand::ALL,
            auto_label: "one".into(),
            branch: Some("changed".into()),
            ahead_behind: None,
            space: None,
        }],
        cache_updates: Vec::new(),
    });

    assert!(changed);
}

#[tokio::test]
async fn pane_death_reconciles_each_client_view_and_focus() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("pane-death-views");
    let dead_pane = workspace.tabs[0].root_pane;
    let second_tab = workspace.test_add_tab(Some("second"));
    let second_pane = workspace.tabs[second_tab].root_pane;
    let (second_runtime, mut second_input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[?1004h",
            4,
        );
    workspace.insert_test_runtime(second_pane, second_runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let second_tab_id = server
        .app
        .public_tab_id(0, second_tab)
        .expect("second tab id");

    let (first_control, _) = connect_matching_test_shell(&mut server, 71);
    let (second_control, _) = connect_matching_test_shell(&mut server, 72);
    let _ = first_control.recv().expect("first snapshot");
    let _ = second_control.recv().expect("second snapshot");
    assert!(server.focus_shell_client_on_tab(72, &second_tab_id));
    server.clients.get_mut(&71).unwrap().outer_terminal_focus = Some(true);
    server.clients.get_mut(&72).unwrap().outer_terminal_focus = Some(false);

    assert!(server.handle_internal_event_with_forwarding(AppEvent::PaneDied { pane_id: dead_pane }));

    assert_eq!(
        server.shell_tab_id_for_client(71).as_deref(),
        Some(second_tab_id.as_str())
    );
    assert_eq!(
        server.shell_tab_id_for_client(72).as_deref(),
        Some(second_tab_id.as_str())
    );
    assert_eq!(
        second_input
            .try_recv()
            .expect("fallback focus gained input"),
        Bytes::from_static(b"\x1b[I")
    );
    assert!(
        second_input.try_recv().is_err(),
        "focus gain was duplicated"
    );
    shutdown_test_runtimes(&mut server);
}

#[test]
fn terminal_attach_client_exits_when_attached_pane_dies() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("attached");
    let pane_id = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    let terminal_id = server.app.state.workspaces[0]
        .pane_state(pane_id)
        .expect("pane")
        .attached_terminal_id
        .to_string();
    let (writer, control_rx, _render_rx) = test_client_writer();

    assert!(!server.handle_server_event(ServerEvent::ClientConnected {
        client_id: 7,
        cols: 80,
        rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        writer,
    }));
    assert!(
        server.handle_server_event(ServerEvent::ClientAttachTerminal {
            client_id: 7,
            terminal_id: terminal_id.clone(),
            takeover: false,
        })
    );
    assert_eq!(server.terminal_attach_owners.get(&terminal_id), Some(&7));

    assert!(server.handle_internal_event_with_forwarding(AppEvent::PaneDied { pane_id }));

    assert!(!server.clients.contains_key(&7));
    assert!(!server.terminal_attach_owners.contains_key(&terminal_id));
    let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
    assert_eq!(reason, Some(format!("terminal {terminal_id} exited")));
}

#[test]
fn terminal_attach_scroll_moves_attached_runtime_viewport() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let mut bytes = Vec::new();
    for line in 0..80 {
        bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
    }
    let runtime = crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 4096, &bytes);

    apply_terminal_attach_scroll(
        &runtime,
        AttachScrollSource::Wheel,
        AttachScrollDirection::Up,
        3,
        None,
        None,
        0,
    )
    .expect("scroll up");
    let metrics = runtime.scroll_metrics().expect("scroll metrics");
    assert_eq!(metrics.offset_from_bottom, 3);

    apply_terminal_attach_scroll(
        &runtime,
        AttachScrollSource::Wheel,
        AttachScrollDirection::Down,
        2,
        None,
        None,
        0,
    )
    .expect("scroll down");
    let metrics = runtime.scroll_metrics().expect("scroll metrics");
    assert_eq!(metrics.offset_from_bottom, 1);
    drop(runtime);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

#[test]
fn client_pane_pixel_mouse_uses_runtime_pixel_encoding() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            20,
            5,
            0,
            b"\x1b[?1003h\x1b[?1006h\x1b[?1016h",
            4,
        );
    runtime.resize(5, 20, 10, 20);

    apply_client_pane_input_events(
        &runtime,
        &[crate::protocol::ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Moved,
            position: crate::protocol::ClientMousePosition::Pixels {
                x: 21,
                y: 22,
                column: 2,
                row: 1,
            },
            geometry: None,
            modifiers: 0,
            lines: 3,
        }],
    )
    .expect("pixel mouse input");
    assert_eq!(
        input_rx.try_recv().expect("encoded pixel mouse"),
        Bytes::from_static(b"\x1b[<35;21;22M")
    );
    drop(runtime);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

#[test]
fn client_pane_pixel_mouse_falls_back_to_canonical_cell_position() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            20,
            5,
            0,
            b"\x1b[?1003h\x1b[?1006h",
            4,
        );
    runtime.resize(5, 20, 10, 20);

    apply_client_pane_input_events(
        &runtime,
        &[crate::protocol::ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Moved,
            position: crate::protocol::ClientMousePosition::Pixels {
                x: 21,
                y: 22,
                column: 2,
                row: 1,
            },
            geometry: None,
            modifiers: 0,
            lines: 3,
        }],
    )
    .expect("cell mouse fallback");
    assert_eq!(
        input_rx.try_recv().expect("encoded cell mouse"),
        Bytes::from_static(b"\x1b[<35;3;2M")
    );
    drop(runtime);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

#[test]
fn client_pane_wheel_input_accumulates_scrollback_offset() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let mut bytes = Vec::new();
    for line in 0..80 {
        bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
    }
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            20, 5, 4096, &bytes, 4,
        );
    let scroll = |kind| crate::protocol::ClientPaneInputEvent::Mouse {
        kind,
        position: crate::protocol::ClientMousePosition::Cell { column: 2, row: 1 },
        geometry: None,
        modifiers: 0,
        lines: 3,
    };

    apply_client_pane_input_events(
        &runtime,
        &[scroll(crate::protocol::ClientMouseKind::ScrollUp)],
    )
    .expect("first scroll up");
    apply_client_pane_input_events(
        &runtime,
        &[scroll(crate::protocol::ClientMouseKind::ScrollUp)],
    )
    .expect("second scroll up");
    assert_eq!(
        runtime
            .scroll_metrics()
            .expect("scroll metrics")
            .offset_from_bottom,
        6
    );

    apply_client_pane_input_events(
        &runtime,
        &[scroll(crate::protocol::ClientMouseKind::ScrollDown)],
    )
    .expect("scroll down");
    assert_eq!(
        runtime
            .scroll_metrics()
            .expect("scroll metrics")
            .offset_from_bottom,
        3
    );

    runtime.test_process_pty_bytes(b"\x1b[?1003h\x1b[?1006h");
    apply_client_pane_input_events(
        &runtime,
        &[crate::protocol::ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Moved,
            position: crate::protocol::ClientMousePosition::Cell { column: 2, row: 1 },
            geometry: None,
            modifiers: 0,
            lines: 3,
        }],
    )
    .expect("reported mouse motion");
    assert_eq!(
        runtime
            .scroll_metrics()
            .expect("scroll metrics")
            .offset_from_bottom,
        3
    );
    assert_eq!(
        input_rx.try_recv().expect("reported mouse motion"),
        Bytes::from_static(b"\x1b[<35;3;2M")
    );

    apply_client_pane_input_events(
        &runtime,
        &[crate::protocol::ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Down(crate::protocol::ClientMouseButton::Left),
            position: crate::protocol::ClientMousePosition::Cell { column: 2, row: 1 },
            geometry: None,
            modifiers: 0,
            lines: 3,
        }],
    )
    .expect("mouse button");
    assert_eq!(
        runtime
            .scroll_metrics()
            .expect("scroll metrics")
            .offset_from_bottom,
        0
    );
    assert_eq!(
        input_rx.try_recv().expect("reported mouse button"),
        Bytes::from_static(b"\x1b[<0;3;2M")
    );
    drop(runtime);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

#[test]
fn terminal_attach_input_resets_scrolled_viewport() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let mut bytes = Vec::new();
    for line in 0..80 {
        bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
    }
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            20, 5, 4096, &bytes, 4,
        );

    runtime.scroll_up(4);
    assert_eq!(
        runtime
            .scroll_metrics()
            .expect("scroll metrics")
            .offset_from_bottom,
        4
    );

    apply_terminal_attach_input(&runtime, b"x".to_vec()).expect("attach input");
    assert_eq!(
        runtime
            .scroll_metrics()
            .expect("scroll metrics")
            .offset_from_bottom,
        0
    );
    assert_eq!(
        input_rx.try_recv().expect("forwarded input"),
        Bytes::from("x")
    );

    drop(runtime);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

fn with_terminal_attach_runtime(
    initial_bytes: &[u8],
    initial_scroll: usize,
    test: impl FnOnce(&crate::terminal::TerminalRuntime, &mut mpsc::Receiver<Bytes>),
) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _runtime_guard = rt.enter();
    let mut bytes = initial_bytes.to_vec();
    for line in 0..80 {
        bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
    }
    let (runtime, mut input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            20, 5, 4096, &bytes, 4,
        );
    if initial_scroll > 0 {
        runtime.scroll_up(initial_scroll);
    }

    test(&runtime, &mut input_rx);

    drop(runtime);
    drop(_runtime_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
}

fn apply_terminal_attach_page_up(runtime: &crate::terminal::TerminalRuntime) {
    apply_terminal_attach_scroll(
        runtime,
        AttachScrollSource::PageKey {
            input: b"\x1b[5~".to_vec(),
        },
        AttachScrollDirection::Up,
        4,
        None,
        None,
        0,
    )
    .expect("page key");
}

fn client_page_key(
    code: crate::protocol::ClientKeyCode,
    modifiers: crossterm::event::KeyModifiers,
    kind: crate::protocol::ClientKeyKind,
) -> crate::protocol::ClientPaneInputEvent {
    crate::protocol::ClientPaneInputEvent::Key {
        code,
        modifiers: modifiers.bits(),
        kind,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: None,
        tracks_release: true,
        physical_key_id: None,
        windows_record: None,
    }
}

#[test]
fn client_plain_page_keys_scroll_shell_transcript_by_pane_height() {
    with_terminal_attach_runtime(b"", 0, |runtime, input_rx| {
        apply_client_pane_input_events(
            runtime,
            &[client_page_key(
                crate::protocol::ClientKeyCode::PageUp,
                crossterm::event::KeyModifiers::empty(),
                crate::protocol::ClientKeyKind::Press,
            )],
        )
        .expect("pane PageUp");
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            5
        );

        apply_client_pane_input_events(
            runtime,
            &[client_page_key(
                crate::protocol::ClientKeyCode::PageUp,
                crossterm::event::KeyModifiers::empty(),
                crate::protocol::ClientKeyKind::Release,
            )],
        )
        .expect("pane PageUp release");
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            5
        );

        apply_client_pane_input_events(
            runtime,
            &[client_page_key(
                crate::protocol::ClientKeyCode::PageDown,
                crossterm::event::KeyModifiers::empty(),
                crate::protocol::ClientKeyKind::Press,
            )],
        )
        .expect("pane PageDown");
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert!(input_rx.try_recv().is_err(), "page keys reached the shell");
    });
}

#[test]
fn client_page_keys_forward_when_modified_or_owned_by_application() {
    with_terminal_attach_runtime(b"", 0, |runtime, input_rx| {
        apply_client_pane_input_events(
            runtime,
            &[client_page_key(
                crate::protocol::ClientKeyCode::PageUp,
                crossterm::event::KeyModifiers::CONTROL,
                crate::protocol::ClientKeyKind::Press,
            )],
        )
        .expect("modified pane PageUp");
        assert!(
            input_rx.try_recv().is_ok(),
            "modified PageUp was not forwarded"
        );
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
    });

    with_terminal_attach_runtime(b"\x1b[?1h", 0, |runtime, input_rx| {
        apply_client_pane_input_events(
            runtime,
            &[client_page_key(
                crate::protocol::ClientKeyCode::PageUp,
                crossterm::event::KeyModifiers::empty(),
                crate::protocol::ClientKeyKind::Press,
            )],
        )
        .expect("application PageUp");
        assert_eq!(
            input_rx.try_recv().expect("forwarded application PageUp"),
            Bytes::from_static(b"\x1b[5~")
        );
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
    });
}

#[test]
fn client_popup_plain_page_key_remains_popup_input() {
    with_terminal_attach_runtime(b"", 0, |runtime, input_rx| {
        apply_client_popup_input_events(
            runtime,
            &[client_page_key(
                crate::protocol::ClientKeyCode::PageUp,
                crossterm::event::KeyModifiers::empty(),
                crate::protocol::ClientKeyKind::Press,
            )],
        )
        .expect("popup PageUp");
        assert_eq!(
            input_rx.try_recv().expect("forwarded popup PageUp"),
            Bytes::from_static(b"\x1b[5~")
        );
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
    });
}

#[test]
fn terminal_attach_paste_uses_plain_text_when_runtime_did_not_enable_brackets() {
    with_terminal_attach_runtime(b"", 0, |runtime, input_rx| {
        apply_terminal_attach_input(runtime, b"\x1b[200~line one\nline two\x1b[201~".to_vec())
            .expect("attach paste");

        assert_eq!(
            input_rx.try_recv().expect("forwarded paste"),
            Bytes::from_static(b"line one\nline two")
        );
    });
}

#[test]
fn terminal_attach_paste_preserves_brackets_when_runtime_enabled_them() {
    with_terminal_attach_runtime(b"\x1b[?2004h", 0, |runtime, input_rx| {
        apply_terminal_attach_input(runtime, b"\x1b[200~line one\nline two\x1b[201~".to_vec())
            .expect("attach paste");

        assert_eq!(
            input_rx.try_recv().expect("forwarded paste"),
            Bytes::from_static(b"\x1b[200~line one\nline two\x1b[201~")
        );
    });
}

#[test]
fn terminal_attach_page_key_host_scrolls_plain_terminal() {
    with_terminal_attach_runtime(b"", 0, |runtime, input_rx| {
        apply_terminal_attach_page_up(runtime);

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            4
        );
        assert!(input_rx.try_recv().is_err());
    });
}

#[test]
fn terminal_attach_page_key_forwards_when_mouse_reporting() {
    with_terminal_attach_runtime(b"\x1b[?1000h", 3, |runtime, input_rx| {
        apply_terminal_attach_page_up(runtime);

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded page key"),
            Bytes::from_static(b"\x1b[5~")
        );
    });
}

#[test]
fn terminal_attach_page_key_forwards_when_application_cursor() {
    with_terminal_attach_runtime(b"\x1b[?1h", 3, |runtime, input_rx| {
        apply_terminal_attach_page_up(runtime);

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded page key"),
            Bytes::from_static(b"\x1b[5~")
        );
    });
}

#[test]
fn terminal_attach_page_key_host_scrolls_shell_like_decckm_with_bracketed_paste() {
    with_terminal_attach_runtime(b"\x1b[?1h\x1b[?2004h", 0, |runtime, input_rx| {
        apply_terminal_attach_page_up(runtime);

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            4
        );
        assert!(input_rx.try_recv().is_err());
    });
}

#[test]
fn terminal_attach_page_key_forwards_in_alternate_screen_without_mouse_reporting() {
    with_terminal_attach_runtime(b"\x1b[?1049h", 3, |runtime, input_rx| {
        apply_terminal_attach_page_up(runtime);

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded page key"),
            Bytes::from_static(b"\x1b[5~")
        );
    });
}

#[test]
fn headless_scheduled_tasks_expire_agent_metadata() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("metadata");
    let pane_id = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();

    assert!(
        server.handle_internal_event_with_forwarding(AppEvent::HookStateReported {
            pane_id,
            source: "custom:pi".into(),
            agent_label: "pi".into(),
            state: crate::detect::AgentState::Working,
            message: None,
            seq: None,
            session_ref: None,
        })
    );
    assert!(
        server.handle_internal_event_with_forwarding(AppEvent::HookMetadataReported {
            pane_id,
            source: "user:pi-display".into(),
            agent_label: Some("pi".into()),
            applies_to_source: Some("custom:pi".into()),
            title: Some("short lived".into()),
            display_agent: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            seq: None,
            // Expiry is advanced with the captured deadline below; keep the
            // pre-expiry assertion independent of wall-clock scheduling.
            ttl: Some(Duration::from_secs(60)),
        })
    );

    let deadline = server
        .app
        .agent_metadata_deadline
        .expect("metadata deadline");
    let terminal_id = server.app.state.workspaces[0]
        .pane_state(pane_id)
        .expect("pane")
        .attached_terminal_id
        .clone();
    assert_eq!(
        server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal")
            .effective_title()
            .as_deref(),
        Some("short lived")
    );

    assert!(server.handle_scheduled_tasks_headless(deadline + Duration::from_millis(1), false));

    assert_eq!(server.app.agent_metadata_deadline, None);
    assert_eq!(
        server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal")
            .effective_title(),
        None
    );
    assert!(server
        .app
        .event_hub
        .events_after(0)
        .iter()
        .any(|(_, event)| {
            event.event == crate::api::schema::EventKind::PaneAgentStatusChanged
                && matches!(
                    &event.data,
                    crate::api::schema::EventData::PaneAgentStatusChanged {
                        title,
                        ..
                    } if title.is_none()
                )
        }));
}

#[test]
fn headless_scheduled_tasks_clears_disabled_agent_manifest_update_deadline() {
    let mut server = test_headless_server();
    let now = Instant::now();
    server.app.next_agent_manifest_update_check = Some(now - Duration::from_millis(1));

    assert!(!server.handle_scheduled_tasks_headless(now, false));
    assert_eq!(server.app.next_agent_manifest_update_check, None);
}

#[cfg(unix)]
#[tokio::test]
async fn headless_scheduled_tasks_start_pending_agent_resume_without_foreground_client() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("restored");
    let pane_id = workspace.tabs[0].root_pane;
    let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .expect("test terminal should exist")
        .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
        agent: "codex".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
        dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
    });

    server.render_and_stream();
    assert_ne!(server.app.state.view.terminal_area, Rect::default());

    let now = Instant::now();
    assert!(!server.handle_scheduled_tasks_headless(now, false));
    assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());
    let deadline = server
        .app
        .pending_agent_resume_deadline
        .expect("clientless resume should wait briefly for a host theme");

    assert!(server.handle_scheduled_tasks_headless(deadline, false));
    assert!(server.app.terminal_runtimes.get(&terminal_id).is_some());
    assert!(server
        .app
        .state
        .terminals
        .get(&terminal_id)
        .expect("test terminal should still exist")
        .pending_agent_resume_plan
        .is_none());
    shutdown_test_runtimes(&mut server);
}

#[test]
fn terminal_attach_resize_uses_known_cell_geometry_without_pixel_mouse() {
    with_terminal_session_test_server(|server, _other_terminal_id, terminal_id, _pane_id| {
        let mut client = ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        );
        client.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: terminal_id.clone(),
        };
        server.clients.insert(1, client);

        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 1,
            cols: 100,
            rows: 30,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: false,
        }));
        assert_eq!(
            server
                .runtime_for_terminal_id_string(&terminal_id)
                .unwrap()
                .pixel_size(),
            Some((800, 480))
        );
        assert_eq!(
            server.clients[&1].cell_size,
            crate::kitty_graphics::HostCellSize {
                width_px: 8,
                height_px: 16,
            }
        );
        assert!(!server.clients[&1].pixel_mouse);

        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 1,
            cols: 100,
            rows: 30,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
        }));
        assert_eq!(
            server
                .runtime_for_terminal_id_string(&terminal_id)
                .unwrap()
                .pixel_size(),
            None
        );
        assert_eq!(
            server.clients[&1].cell_size,
            crate::kitty_graphics::HostCellSize::default()
        );
        assert!(!server.clients[&1].pixel_mouse);
    });
}

#[test]
fn pending_terminal_resize_does_not_take_shell_foreground_or_geometry() {
    let mut server = test_headless_server();
    server.clients.insert(
        1,
        ClientConnection::new(
            (100, 30),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.clients.insert(
        2,
        ClientConnection::new_with_mode(
            ClientConnectionMode::TerminalPending,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::TerminalAnsi,
            None,
        ),
    );
    server.foreground_client_id = Some(1);
    server.sync_foreground_client_state();
    let shell_size = server.effective_size;

    assert!(server.handle_server_event(ServerEvent::ClientResize {
        client_id: 2,
        cols: 200,
        rows: 60,
        cell_width_px: 10,
        cell_height_px: 20,
        pixel_mouse: false,
    }));

    assert_eq!(server.foreground_client_id, Some(1));
    assert_eq!(server.effective_size, shell_size);
    assert_eq!(server.clients[&2].terminal_size, (200, 60));
}

#[test]
fn client_shell_streams_focused_pane_report_all_demand() {
    with_terminal_session_test_server(|server, _other_terminal_id, terminal_id, _pane_id| {
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.app.state.active = Some(0);
        server
            .runtime_for_terminal_id_string(&terminal_id)
            .expect("focused runtime")
            .test_process_pty_bytes(b"\x1b[>15u");

        server.stream_direct_terminal_keyboard_mode();

        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("shell keyboard mode message")
            ),
            ServerMessage::ClientShellKeyboardReportAll { enabled: true }
        ));
    });
}

#[tokio::test]
async fn client_shell_release_cleanup_does_not_promote_and_survives_disconnect() {
    let mut server = test_headless_server();
    let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[>3u");
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    for client_id in [1, 2] {
        server.clients.insert(
            client_id,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                client_id,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
    }
    let key = |kind| crate::protocol::ClientPaneInputEvent::Key {
        code: crate::protocol::ClientKeyCode::Char('x'),
        modifiers: 0,
        kind,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: (kind == crate::protocol::ClientKeyKind::Press).then(|| "x".to_owned()),
        tracks_release: true,
        physical_key_id: Some(0x2d),
        windows_record: None,
    };

    assert!(
        server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 1,
            pane_id: pane_id.clone(),
            events: vec![key(crate::protocol::ClientKeyKind::Press)],
        })
    );
    assert!(!input_rx.recv().await.expect("encoded press").is_empty());
    assert!(server.promote_client_to_foreground(2));

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 1,
            pane_id: pane_id.clone(),
            events: vec![key(crate::protocol::ClientKeyKind::Release)],
        })
    );
    assert!(!input_rx.recv().await.expect("encoded release").is_empty());
    assert_eq!(server.foreground_client_id, Some(2));

    assert!(
        server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 1,
            pane_id,
            events: vec![key(crate::protocol::ClientKeyKind::Press)],
        })
    );
    assert!(!input_rx
        .recv()
        .await
        .expect("second encoded press")
        .is_empty());
    assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 1 }));
    assert!(!input_rx
        .recv()
        .await
        .expect("disconnect synthesized release")
        .is_empty());
    shutdown_test_runtimes(&mut server);
}

#[test]
fn client_shell_mouse_capture_combines_local_preference_with_endpoint_demand() {
    let mut server = test_headless_server();
    let (writer, control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(writer),
        ),
    );

    server.stream_host_mouse_capture_mode();
    assert!(matches!(
        read_server_message(control_rx.recv().expect("initial mouse mode")),
        ServerMessage::MouseCapture {
            enabled: false,
            sgr_pixels: false
        }
    ));
    assert!(
        server.handle_server_event(ServerEvent::ClientShellMouseCapture {
            client_id: 1,
            enabled: true,
        })
    );
    server.stream_host_mouse_capture_mode();
    assert!(matches!(
        read_server_message(control_rx.recv().expect("preferred mouse mode")),
        ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: false
        }
    ));
}

#[test]
fn client_shell_focus_promotes_and_reaches_reporting_pane() {
    with_terminal_session_test_server(|server, terminal_id, _other_terminal_id, _pane_id| {
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[?1004h",
                4,
            );
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        server.app.state.active = Some(0);
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();
        assert!(server.claim_unowned_shell_tab_geometry(2, true));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("focused runtime")
                .current_size(),
            (30, 99)
        );

        assert!(server.handle_server_event(ServerEvent::ClientShellFocus {
            client_id: 1,
            focused: true,
        }));
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("focused runtime")
                .current_size(),
            (24, 79)
        );
        assert_eq!(
            input_rx.try_recv().expect("focus gained input"),
            Bytes::from_static(b"\x1b[I")
        );

        assert!(server.handle_server_event(ServerEvent::ClientShellFocus {
            client_id: 2,
            focused: true,
        }));
        assert!(
            input_rx.try_recv().is_err(),
            "second viewer duplicated focus gain"
        );
        assert!(server.handle_server_event(ServerEvent::ClientShellFocus {
            client_id: 1,
            focused: false,
        }));
        assert!(
            input_rx.try_recv().is_err(),
            "remaining viewer lost tab focus"
        );
        assert!(server.handle_server_event(ServerEvent::ClientShellFocus {
            client_id: 2,
            focused: false,
        }));
        assert_eq!(server.app.state.outer_terminal_focus, Some(false));
        assert_eq!(
            input_rx.try_recv().expect("last viewer focus lost input"),
            Bytes::from_static(b"\x1b[O")
        );
    });
}

#[test]
fn direct_terminal_streams_child_keyboard_and_mouse_modes() {
    with_terminal_session_test_server(|server, _other_terminal_id, terminal_id, _pane_id| {
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new_with_mode(
                ClientConnectionMode::TerminalAttach {
                    terminal_id: terminal_id.clone(),
                },
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server
            .clients
            .get_mut(&1)
            .expect("direct attach client")
            .pixel_mouse = true;
        server
            .runtime_for_terminal_id_string(&terminal_id)
            .expect("attached runtime")
            .test_process_pty_bytes(b"\x1b[>15u\x1b[?1000h");

        server.stream_direct_terminal_keyboard_mode();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("keyboard mode message")
            ),
            ServerMessage::DirectTerminalKeyboardProtocol {
                flags: 15,
                modify_other_keys_level: 0
            }
        ));

        server
            .runtime_for_terminal_id_string(&terminal_id)
            .expect("attached runtime")
            .test_process_pty_bytes(b"\x1b[<u\x1b[>3u\x1b[>4;1m");
        server.stream_direct_terminal_keyboard_mode();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("modifyOtherKeys mode-one keyboard message")
            ),
            ServerMessage::DirectTerminalKeyboardProtocol {
                flags: 3,
                modify_other_keys_level: 1
            }
        ));

        server
            .runtime_for_terminal_id_string(&terminal_id)
            .expect("attached runtime")
            .test_process_pty_bytes(b"\x1b[>4;2m");
        server.stream_direct_terminal_keyboard_mode();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("modifyOtherKeys mode-two keyboard message")
            ),
            ServerMessage::DirectTerminalKeyboardProtocol {
                flags: 3,
                modify_other_keys_level: 2
            }
        ));

        server
            .runtime_for_terminal_id_string(&terminal_id)
            .expect("attached runtime")
            .test_process_pty_bytes(b"\x1b[<u");
        server.stream_direct_terminal_keyboard_mode();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("modifyOtherKeys-only keyboard mode message")
            ),
            ServerMessage::DirectTerminalKeyboardProtocol {
                flags: 0,
                modify_other_keys_level: 2
            }
        ));

        server.stream_host_mouse_capture_mode();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("mouse capture message")
            ),
            ServerMessage::MouseCapture {
                enabled: true,
                sgr_pixels: false
            }
        ));

        server
            .runtime_for_terminal_id_string(&terminal_id)
            .expect("attached runtime")
            .test_process_pty_bytes(b"\x1b[?1016h");
        server.stream_host_mouse_capture_mode();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("pixel mouse capture message")
            ),
            ServerMessage::MouseCapture {
                enabled: true,
                sgr_pixels: true
            }
        ));

        server
            .runtime_for_terminal_id_string(&terminal_id)
            .expect("attached runtime")
            .test_process_pty_bytes(b"\x1b[?1000l\x1b[?1016l");
        server.stream_host_mouse_capture_mode();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("child mouse disable message")
            ),
            ServerMessage::MouseCapture {
                enabled: false,
                sgr_pixels: false
            }
        ));
    });
}

#[test]
fn direct_terminal_mouse_uses_runtime_protocol_encoding() {
    with_terminal_session_test_server(|server, runtime_terminal_id, terminal_id, _pane_id| {
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[?1000h\x1b[?1006h",
                4,
            );
        server
            .app
            .terminal_runtimes
            .insert(runtime_terminal_id, runtime);
        server.clients.insert(
            1,
            ClientConnection::new_with_mode(
                ClientConnectionMode::TerminalAttach {
                    terminal_id: terminal_id.clone(),
                },
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                1,
                RenderEncoding::TerminalAnsi,
                None,
            ),
        );

        assert!(server.handle_server_event(ServerEvent::ClientAttachMouse {
            client_id: 1,
            kind: protocol::ClientMouseKind::Down(protocol::ClientMouseButton::Left),
            position: protocol::ClientMousePosition::Cell { column: 10, row: 5 },
            geometry: None,
            modifiers: 0,
            lines: 1,
        }));
        assert_eq!(
            input_rx.try_recv().expect("encoded direct mouse input"),
            Bytes::from_static(b"\x1b[<0;11;6M")
        );
    });
}

#[test]
fn direct_terminal_pixel_mouse_uses_runtime_tracking_and_coordinates() {
    with_terminal_session_test_server(|server, runtime_terminal_id, terminal_id, _pane_id| {
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[?1000h\x1b[?1006h\x1b[?1016h",
                4,
            );
        runtime.resize(24, 80, 10, 20);
        server
            .app
            .terminal_runtimes
            .insert(runtime_terminal_id, runtime);
        server.clients.insert(
            1,
            ClientConnection::new_with_mode(
                ClientConnectionMode::TerminalAttach {
                    terminal_id: terminal_id.clone(),
                },
                (80, 24),
                crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                },
                1,
                RenderEncoding::TerminalAnsi,
                None,
            ),
        );
        let client = server.clients.get_mut(&1).expect("direct attach client");
        client.pixel_mouse = true;
        client.host_sgr_pixels_active = Some(true);

        assert!(!server.handle_server_event(ServerEvent::ClientAttachMouse {
            client_id: 1,
            kind: protocol::ClientMouseKind::Down(protocol::ClientMouseButton::Left),
            position: protocol::ClientMousePosition::Pixels {
                x: 21,
                y: 22,
                column: 3,
                row: 1,
            },
            geometry: Some(protocol::ClientMouseGeometry {
                cols: 80,
                rows: 24,
                width_px: 800,
                height_px: 480,
            }),
            modifiers: 0,
            lines: 1,
        }));
        assert!(input_rx.try_recv().is_err());

        assert!(server.handle_server_event(ServerEvent::ClientAttachMouse {
            client_id: 1,
            kind: protocol::ClientMouseKind::Down(protocol::ClientMouseButton::Left),
            position: protocol::ClientMousePosition::Pixels {
                x: 21,
                y: 22,
                column: 2,
                row: 1,
            },
            geometry: Some(protocol::ClientMouseGeometry {
                cols: 80,
                rows: 24,
                width_px: 805,
                height_px: 485,
            }),
            modifiers: 0,
            lines: 1,
        }));
        assert_eq!(
            input_rx
                .try_recv()
                .expect("proportionally mapped direct pixel mouse input"),
            Bytes::from_static(b"\x1b[<0;21;22M")
        );

        assert!(server.handle_server_event(ServerEvent::ClientAttachMouse {
            client_id: 1,
            kind: protocol::ClientMouseKind::Moved,
            position: protocol::ClientMousePosition::Pixels {
                x: 21,
                y: 22,
                column: 2,
                row: 1,
            },
            geometry: Some(protocol::ClientMouseGeometry {
                cols: 80,
                rows: 24,
                width_px: 800,
                height_px: 480,
            }),
            modifiers: 0,
            lines: 1,
        }));
        assert!(input_rx.try_recv().is_err());

        assert!(server.handle_server_event(ServerEvent::ClientAttachMouse {
            client_id: 1,
            kind: protocol::ClientMouseKind::Down(protocol::ClientMouseButton::Left),
            position: protocol::ClientMousePosition::Pixels {
                x: 21,
                y: 22,
                column: 2,
                row: 1,
            },
            geometry: Some(protocol::ClientMouseGeometry {
                cols: 80,
                rows: 24,
                width_px: 800,
                height_px: 480,
            }),
            modifiers: 0,
            lines: 1,
        }));
        assert_eq!(
            input_rx
                .try_recv()
                .expect("encoded direct pixel mouse input"),
            Bytes::from_static(b"\x1b[<0;21;22M")
        );
    });
}

#[test]
fn client_config_reload_request_refreshes_attached_clients() {
    let mut server = test_headless_server();
    let (client_tx, client_control_rx, _client_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.app.state.request_client_config_reload = true;

    server.drain_client_config_reload_request();

    match read_server_message(
        client_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("client config reload message"),
    ) {
        ServerMessage::ReloadSoundConfig => {}
        other => panic!("expected ReloadSoundConfig, got {other:?}"),
    }
    assert!(!server.app.state.request_client_config_reload);
}

#[test]
fn terminal_bell_targets_foreground_client_only() {
    let mut server = test_headless_server();
    let (background_tx, background_control_rx, _background_rx) = test_client_writer();
    let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(background_tx),
        ),
    );
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(foreground_tx),
        ),
    );
    server.foreground_client_id = Some(2);

    let changed = server.handle_internal_event_with_forwarding(AppEvent::TerminalBell {
        pane_id: crate::layout::PaneId::from_raw(1),
        count: 3,
    });

    assert!(!changed);
    match read_server_message(
        foreground_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("foreground terminal bell message"),
    ) {
        ServerMessage::TerminalBell { count } => assert_eq!(count, 3),
        other => panic!("expected terminal bell message, got {other:?}"),
    }
    assert!(
        background_control_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "background client should not receive terminal bells"
    );

    server.foreground_client_id = None;
    server.handle_internal_event_with_forwarding(AppEvent::TerminalBell {
        pane_id: crate::layout::PaneId::from_raw(1),
        count: 1,
    });
    assert!(
        foreground_control_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "bells without a foreground client must not be retained"
    );
}

#[test]
fn clipboard_write_targets_foreground_client_only() {
    let mut server = test_headless_server();
    let (background_tx, background_control_rx, _background_rx) = test_client_writer();
    let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(background_tx),
        ),
    );
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(foreground_tx),
        ),
    );
    server.foreground_client_id = Some(2);
    server.sync_foreground_client_state();

    let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
        content: b"test".to_vec(),
    });

    assert!(!changed);
    match read_server_message(
        foreground_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("foreground clipboard message"),
    ) {
        ServerMessage::Clipboard { data } => assert_eq!(data, "dGVzdA=="),
        other => panic!("expected clipboard message, got {other:?}"),
    }
    assert!(
        background_control_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "background client should not receive clipboard writes"
    );
}

#[test]
fn clipboard_write_without_foreground_client_does_not_change_visual_state() {
    let mut server = test_headless_server();
    server.foreground_client_id = None;

    let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
        content: b"test".to_vec(),
    });

    assert!(!changed);
}

#[test]
fn clipboard_write_failed_foreground_send_removes_client_without_visual_change() {
    let mut server = test_headless_server();
    let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();
    drop(foreground_control_rx);
    foreground_tx.test_close();

    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(foreground_tx),
        ),
    );
    server.foreground_client_id = Some(1);

    let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
        content: b"test".to_vec(),
    });

    assert!(!changed);
    assert!(
        !server.clients.contains_key(&1),
        "failed targeted send should remove the broken foreground client"
    );
}

#[test]
fn semantic_notifications_broadcast_only_to_client_shells() {
    let mut server = test_headless_server();
    let (shell_one_tx, shell_one_control, _shell_one_frames) = test_client_writer();
    let (shell_two_tx, shell_two_control, _shell_two_frames) = test_client_writer();
    let (terminal_tx, terminal_control, _terminal_frames) = test_client_writer();
    for (client_id, writer) in [(1, shell_one_tx), (2, shell_two_tx)] {
        server.clients.insert(
            client_id,
            ClientConnection::new_with_mode(
                ClientConnectionMode::ClientShell,
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                client_id,
                RenderEncoding::SemanticFrame,
                Some(writer),
            ),
        );
    }
    server.clients.insert(
        3,
        ClientConnection::new_with_mode(
            ClientConnectionMode::TerminalPending,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            3,
            RenderEncoding::TerminalAnsi,
            Some(terminal_tx),
        ),
    );
    let event = protocol::SemanticNotification {
        kind: protocol::SemanticNotificationKind::Custom,
        title: "hello".into(),
        body: None,
        sound: None,
        agent: None,
        workspace_id: None,
        tab_id: None,
        pane_id: None,
        position: None,
    };
    assert!(server.send_to_client_shells(ServerMessage::SemanticNotification(event.clone())));
    for receiver in [shell_one_control, shell_two_control] {
        assert_eq!(
            read_server_message(
                receiver
                    .recv_timeout(Duration::from_millis(100))
                    .expect("semantic notification")
            ),
            ServerMessage::SemanticNotification(event.clone())
        );
    }
    assert!(terminal_control
        .recv_timeout(Duration::from_millis(50))
        .is_err());
}

#[test]
fn notification_show_uses_client_shell_policy_independent_of_server_delivery() {
    let mut server = test_headless_server();
    server.app.state.toast_config.delivery = config::ToastDelivery::Off;
    let (shell_tx, shell_control, _shell_frames) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(shell_tx),
        ),
    );
    let response = server.handle_notification_show_api(
        "notify-shell".into(),
        api::schema::NotificationShowParams {
            title: "plugin title".into(),
            body: Some("plugin body".into()),
            position: Some(crate::config::ToastHerdrPosition::TopLeft),
            sound: api::schema::NotificationShowSound::Done,
        },
    );
    let response: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
    assert!(matches!(
        response.result,
        api::schema::ResponseResult::NotificationShow { shown: true, .. }
    ));
    assert_eq!(
        read_server_message(
            shell_control
                .recv_timeout(Duration::from_millis(100))
                .expect("semantic plugin notification")
        ),
        ServerMessage::SemanticNotification(protocol::SemanticNotification {
            kind: protocol::SemanticNotificationKind::Custom,
            title: "plugin title".into(),
            body: Some("plugin body".into()),
            sound: Some(protocol::SemanticNotificationSound::Done),
            agent: None,
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            position: Some(crate::config::ToastHerdrPosition::TopLeft),
        })
    );
}

#[test]
fn client_local_notifications_target_foreground_client_only() {
    let mut server = test_headless_server();
    let (background_tx, background_control_rx, _background_rx) = test_client_writer();
    let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(background_tx),
        ),
    );
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(foreground_tx),
        ),
    );
    server.foreground_client_id = Some(2);
    server.sync_foreground_client_state();

    assert!(server.send_to_foreground_client(ServerMessage::Notify {
        kind: protocol::NotifyKind::Toast,
        message: "pi finished".to_string(),
        body: Some("workspace 1".to_string()),
    }));

    match read_server_message(
        foreground_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("foreground toast message"),
    ) {
        ServerMessage::Notify {
            kind,
            message,
            body,
        } => {
            assert_eq!(kind, protocol::NotifyKind::Toast);
            assert_eq!(message, "pi finished");
            assert_eq!(body.as_deref(), Some("workspace 1"));
        }
        other => panic!("expected toast notify, got {other:?}"),
    }
    assert!(
        background_control_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "background client should not receive client-local notifications"
    );
}

#[test]
fn oversized_paste_rejection_notifies_only_the_sending_client() {
    let mut server = test_headless_server();
    let (sender_writer, sender_control_rx, _sender_render_rx) = test_client_writer();
    let (foreground_writer, foreground_control_rx, _foreground_render_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(sender_writer),
        ),
    );
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(foreground_writer),
        ),
    );
    server.foreground_client_id = Some(2);
    server.sync_foreground_client_state();

    assert!(
        !server.handle_server_event(ServerEvent::ClientPasteRejected {
            client_id: 1,
            size: 5_000_012,
            max: 1_048_576,
        })
    );

    match read_server_message(
        sender_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("sending client rejection notification"),
    ) {
        ServerMessage::ClientShellError { message } => assert_eq!(
            message,
            "Paste rejected: Input message is 5000012 bytes; Herdr's limit is 1048576 bytes"
        ),
        other => panic!("expected client shell paste error, got {other:?}"),
    }
    let (shell_writer, shell_control_rx, _shell_render_rx) = test_client_writer();
    server.clients.insert(
        3,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (100, 30),
            crate::kitty_graphics::HostCellSize::default(),
            3,
            RenderEncoding::SemanticFrame,
            Some(shell_writer),
        ),
    );
    assert!(
        !server.handle_server_event(ServerEvent::ClientPasteRejected {
            client_id: 3,
            size: 7_000_000,
            max: 1_048_576,
        })
    );
    match read_server_message(
        shell_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("client shell rejection error"),
    ) {
        ServerMessage::ClientShellError { message } => assert_eq!(
            message,
            "Paste rejected: Input message is 7000000 bytes; Herdr's limit is 1048576 bytes"
        ),
        other => panic!("expected client shell paste error, got {other:?}"),
    }
    assert!(
        foreground_control_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "foreground client must not receive another client's rejection"
    );
    assert_eq!(server.foreground_client_id, Some(2));
    assert_eq!(server.clients.len(), 3);
    assert!(server.app.state.toast.is_none());
}

#[test]
fn update_notification_reaches_client_shell_independent_of_delivery() {
    let mut server = test_headless_server();
    let (client_tx, client_control_rx, _client_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

    let changed = server.handle_internal_event_with_forwarding(AppEvent::UpdateReady {
        version: "9.9.9".to_string(),
        install_command: "herdr update".into(),
    });

    assert!(changed);
    assert!(matches!(
        read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("semantic update notification")
        ),
        ServerMessage::SemanticNotification(protocol::SemanticNotification {
            kind: protocol::SemanticNotificationKind::UpdateInstalled,
            ..
        })
    ));
}

#[test]
fn update_notification_is_semantic_for_system_delivery() {
    let mut server = test_headless_server();
    let (client_tx, client_control_rx, _client_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

    let changed = server.handle_internal_event_with_forwarding(AppEvent::UpdateReady {
        version: "9.9.9".to_string(),
        install_command: "herdr update".into(),
    });

    assert!(changed);
    match read_server_message(
        client_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("semantic update notification"),
    ) {
        ServerMessage::SemanticNotification(notification) => {
            assert_eq!(
                notification.kind,
                protocol::SemanticNotificationKind::UpdateInstalled
            );
            assert_eq!(notification.title, "Herdr v9.9.9 available");
            assert_eq!(
                notification.body.as_deref(),
                Some("detach, run `herdr update`, then run Herdr again to reconnect")
            );
        }
        other => panic!("expected semantic update notification, got {other:?}"),
    }
}

#[test]
fn notification_show_api_forwards_one_semantic_client_notification() {
    let mut server = test_headless_server();
    let (client_tx, client_control_rx, _client_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "notify".into(),
            method: api::schema::Method::NotificationShow(api::schema::NotificationShowParams {
                title: "build failed".into(),
                body: Some("api workspace".into()),
                position: Some(crate::config::ToastHerdrPosition::TopLeft),
                sound: api::schema::NotificationShowSound::Request,
            }),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });

    assert!(changed);
    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(
        parsed.result,
        api::schema::ResponseResult::NotificationShow {
            shown: true,
            reason: api::schema::NotificationShowReason::Shown,
        }
    );
    match read_server_message(
        client_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("semantic api notification"),
    ) {
        ServerMessage::SemanticNotification(notification) => {
            assert_eq!(notification.title, "build failed");
            assert_eq!(notification.body.as_deref(), Some("api workspace"));
            assert_eq!(
                notification.sound,
                Some(protocol::SemanticNotificationSound::Request)
            );
        }
        other => panic!("expected semantic api notification, got {other:?}"),
    }
}

#[test]
fn notification_show_api_preserves_colon_in_forwarded_title() {
    let mut server = test_headless_server();
    let (client_tx, client_control_rx, _client_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "notify".into(),
            method: api::schema::Method::NotificationShow(api::schema::NotificationShowParams {
                title: "build: failed".into(),
                body: Some("api workspace".into()),
                position: None,
                sound: api::schema::NotificationShowSound::None,
            }),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });

    assert!(changed);
    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(
        parsed.result,
        api::schema::ResponseResult::NotificationShow {
            shown: true,
            reason: api::schema::NotificationShowReason::Shown,
        }
    );
    match read_server_message(
        client_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("semantic api notification"),
    ) {
        ServerMessage::SemanticNotification(notification) => {
            assert_eq!(notification.title, "build: failed");
            assert_eq!(notification.body.as_deref(), Some("api workspace"));
        }
        other => panic!("expected semantic api notification, got {other:?}"),
    }
}

#[test]
fn notification_show_api_validates_empty_title_before_disabled_delivery() {
    let mut server = test_headless_server();
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::Off;

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "notify".into(),
            method: api::schema::Method::NotificationShow(api::schema::NotificationShowParams {
                title: "\n\t".into(),
                body: None,
                position: None,
                sound: api::schema::NotificationShowSound::None,
            }),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });

    assert!(changed);
    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    let parsed: api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(parsed.error.code, "invalid_params");
    assert_eq!(parsed.error.message, "notification title is empty");
}

#[test]
fn notification_show_api_reports_no_foreground_client() {
    let mut server = test_headless_server();
    server.foreground_client_id = None;
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "notify".into(),
            method: api::schema::Method::NotificationShow(api::schema::NotificationShowParams {
                title: "build failed".into(),
                body: None,
                position: None,
                sound: api::schema::NotificationShowSound::Request,
            }),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });

    assert!(changed);
    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(
        parsed.result,
        api::schema::ResponseResult::NotificationShow {
            shown: false,
            reason: api::schema::NotificationShowReason::NoForegroundClient,
        }
    );
}

#[test]
fn notification_show_api_includes_sound_in_semantic_event() {
    let mut server = test_headless_server();
    let (client_tx, client_control_rx, _client_rx) = test_client_writer();

    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    assert!(
        server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: api::schema::NotificationShowSound::Done,
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        })
    );

    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(
        parsed.result,
        api::schema::ResponseResult::NotificationShow {
            shown: true,
            reason: api::schema::NotificationShowReason::Shown,
        }
    );
    match read_server_message(
        client_control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("semantic api notification"),
    ) {
        ServerMessage::SemanticNotification(notification) => {
            assert_eq!(notification.title, "build failed");
            assert_eq!(
                notification.sound,
                Some(protocol::SemanticNotificationSound::Done)
            );
        }
        other => panic!("expected semantic api notification, got {other:?}"),
    }
}

#[test]
fn startup_idle_does_not_forward_completion() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("active");
    let pane_id = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;
    server.app.state.toast_config.delay_seconds = 0;
    server.app.state.sound.enabled = true;

    assert!(
        server.handle_internal_event_with_forwarding(AppEvent::AgentProcessDetected {
            pane_id,
            agent: crate::detect::Agent::Pi,
            observed_at: Instant::now(),
        })
    );

    let (client_tx, client_control_rx, _client_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.sync_foreground_client_state();
    while client_control_rx
        .recv_timeout(Duration::from_millis(20))
        .is_ok()
    {}

    assert!(
        server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
            pane_id,
            agent: Some(crate::detect::Agent::Pi),
            state: crate::detect::AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: Instant::now(),
        })
    );
    assert!(
        client_control_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "startup readiness should not forward a completion notification"
    );
}

#[test]
fn stale_api_agent_report_does_not_forward_done_sound() {
    let mut server = test_headless_server();
    let background = crate::workspace::Workspace::test_new("background");
    let pane_id = background.tabs[0].root_pane;
    let public_pane_id = format!("{}:p1", background.id);
    let foreground = crate::workspace::Workspace::test_new("foreground");
    server.app.state.workspaces = vec![background, foreground];
    server.app.state.ensure_test_terminals();
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
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(
                std::env::current_dir()
                    .unwrap()
                    .join("headless-pi-session.jsonl")
                    .display()
                    .to_string(),
            )
            .unwrap(),
        });
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            crate::detect::AgentState::Working,
            None,
            Some(20),
        );
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.mode = crate::app::Mode::Terminal;

    let (client_tx, client_control_rx, _client_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(client_tx),
        ),
    );
    server.foreground_client_id = Some(1);
    server.sync_foreground_client_state();

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "stale".into(),
            method: api::schema::Method::PaneReportAgent(api::schema::PaneReportAgentParams {
                pane_id: public_pane_id,
                source: "herdr:pi".into(),
                agent: "pi".into(),
                state: api::schema::PaneAgentState::Idle,
                message: None,
                seq: Some(19),
                agent_session_id: None,
                agent_session_path: None,
            }),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });

    assert!(changed);
    assert!(response_rx.recv_timeout(Duration::from_millis(100)).is_ok());
    assert_eq!(
        server.app.state.terminals.get(&terminal_id).unwrap().state,
        crate::detect::AgentState::Working
    );
    assert!(
        client_control_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "stale idle report must not forward a done sound"
    );
}

/// Verify that calls to the app's internal-event methods only occur inside
/// `handle_internal_event_with_forwarding`. This ensures the forwarding
/// bypass cannot be reintroduced.
#[test]
fn no_handle_internal_event_bypass_in_module() {
    let source = include_str!("../../headless.rs");

    // Find all lines containing handle_internal_event
    let mut bypass_lines: Vec<String> = Vec::new();
    let mut inside_forwarding_method = false;
    let mut forwarding_method_brace_depth = 0u32;

    for (i, line) in source.lines().enumerate() {
        let line_num = i + 1;

        // Track when we're inside handle_internal_event_with_forwarding
        if line.contains("fn handle_internal_event_with_forwarding") {
            inside_forwarding_method = true;
            forwarding_method_brace_depth = 0;
        }

        if inside_forwarding_method {
            // Count braces to track when we exit the method
            for ch in line.chars() {
                match ch {
                    '{' => forwarding_method_brace_depth += 1,
                    '}' => {
                        forwarding_method_brace_depth =
                            forwarding_method_brace_depth.saturating_sub(1);
                        if forwarding_method_brace_depth == 0 {
                            inside_forwarding_method = false;
                        }
                    }
                    _ => {}
                }
            }
        } else if (line.contains("self.app.handle_internal_event(")
            || line.contains("self.app.handle_internal_event_with_render_impact("))
            && !line.trim().starts_with("///")
            && !line.contains("contains(")
        {
            // Internal-event call outside the forwarding method.
            bypass_lines.push(format!("line {}: {}", line_num, line.trim()));
        }
    }

    assert!(
        bypass_lines.is_empty(),
        "Found direct calls to self.app.handle_internal_event outside \
             handle_internal_event_with_forwarding (bypass risk):\n  {}",
        bypass_lines.join("\n  ")
    );
}
