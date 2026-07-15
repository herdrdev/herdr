use super::*;

#[test]
fn frames_preserve_cursor_without_changing_ordinary_messages() {
    assert_eq!(frame_pane_graphics_for_client(Vec::new()), Vec::<u8>::new());
    assert_eq!(
        frame_pane_graphics_for_client(b"graphics".to_vec()),
        b"\x1b7graphics\x1b8"
    );
}

fn enable_graphics_and_render(
    server: &mut HeadlessServer,
    client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> FrameData {
    server.app.state.kitty_graphics_enabled = true;
    server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    server.render_and_stream();
    read_server_frame(
        client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame"),
    )
}

fn set_graphics_layer(server: &mut HeadlessServer, pane_id: crate::layout::PaneId, data: Vec<u8>) {
    server.app.state.pane_graphics_layers.insert(
        pane_id,
        crate::app::state::PaneGraphicsLayer::new(
            api::schema::PaneGraphicsFormat::Png,
            1,
            1,
            data,
            api::schema::PaneGraphicsPlacementParams::default(),
        ),
    );
}

fn fill_render_lane(server: &HeadlessServer) {
    let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
        .expect("dummy frame");
    server
        .clients
        .get(&1)
        .unwrap()
        .writer
        .as_ref()
        .unwrap()
        .render
        .try_send(queued)
        .expect("pre-fill render lane");
}

fn stream_set_message(
    id: &str,
    pane_id: &str,
    owner: &str,
    data: Vec<u8>,
) -> (api::ApiRequestMessage, std::sync::mpsc::Receiver<String>) {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    (
        api::ApiRequestMessage {
            request: api::schema::Request {
                id: id.into(),
                method: api::schema::Method::PaneGraphicsStreamSet(
                    api::schema::PaneGraphicsSetParams {
                        pane_id: pane_id.into(),
                        owner: owner.into(),
                        format: api::schema::PaneGraphicsFormat::Png,
                        image_width: 1,
                        image_height: 1,
                        data: Some(data),
                        data_base64: String::new(),
                        placement: api::schema::PaneGraphicsPlacementParams::default(),
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
        },
        response_rx,
    )
}

#[tokio::test]
async fn retained_update_sends_only_graphics_message() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let baseline = enable_graphics_and_render(&mut server, &client_rx);
    set_graphics_layer(&mut server, pane_id, vec![1, 2, 3]);

    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    match read_server_message(
        client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("graphics-only update"),
    ) {
        ServerMessage::Graphics { bytes } => {
            assert!(bytes.windows(3).any(|window| window == b"\x1b_G"));
        }
        other => panic!("expected graphics-only message, got {other:?}"),
    }
    assert_frame_data_eq(
        server
            .clients
            .get(&1)
            .unwrap()
            .render_state
            .last_frame()
            .expect("semantic baseline"),
        &baseline,
    );
}

#[tokio::test]
async fn retained_update_defers_on_full_render_lane() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &client_rx);
    fill_render_lane(&server);
    set_graphics_layer(&mut server, pane_id, vec![4, 5, 6]);

    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Deferred
    );
    let client = server.clients.get(&1).unwrap();
    assert_eq!(client.deferred_render(), DeferredRender::Graphics);
    assert!(matches!(
        read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
        ServerMessage::ReloadSoundConfig
    ));
    assert_eq!(
        server.handle_server_event_with_render_impact(ServerEvent::ClientWriterDrained {
            client_id: 1
        }),
        RenderImpact::Graphics
    );
}

#[tokio::test]
async fn retained_update_does_not_downgrade_pending_full_render() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &client_rx);
    fill_render_lane(&server);
    let client = server.clients.get_mut(&1).unwrap();
    client.request_full_redraw();
    server.render_and_stream();
    assert_eq!(
        server.clients.get(&1).unwrap().deferred_render(),
        DeferredRender::Full
    );

    set_graphics_layer(&mut server, pane_id, vec![7, 8, 9]);
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Deferred
    );
    assert_eq!(
        server.clients.get(&1).unwrap().deferred_render(),
        DeferredRender::Full
    );
    assert_eq!(
        server.handle_server_event_with_render_impact(ServerEvent::ClientWriterDrained {
            client_id: 1
        }),
        RenderImpact::Full
    );
}

#[tokio::test]
async fn retained_update_falls_back_for_mixed_app_geometry() {
    let (mut server, client_rx, _pane_id) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &client_rx);

    let (writer, _control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        2,
        ClientConnection::new(
            (60, 20),
            crate::kitty_graphics::HostCellSize {
                width_px: 10,
                height_px: 20,
            },
            crate::terminal_theme::TerminalTheme::default(),
            None,
            2,
            RenderEncoding::SemanticFrame,
            Some(writer),
        ),
    );

    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Fallback
    );
}

#[test]
fn stream_set_has_graphics_only_render_impact() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("graphics");
    let pane_id = workspace.tabs[0].root_pane;
    let public_pane_id = format!("{}:p1", workspace.id);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.kitty_graphics_enabled = true;
    server
        .app
        .state
        .pane_graphics_streams
        .insert(pane_id, "owner-a".into());

    let (request, response_rx) =
        stream_set_message("wrong-owner", &public_pane_id, "owner-b", vec![1, 2, 3]);
    assert_eq!(
        server.handle_api_request_with_render_impact(request),
        RenderImpact::None
    );
    assert!(serde_json::from_str::<api::schema::ErrorResponse>(
        &response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
    )
    .is_ok());

    let (request, response_rx) =
        stream_set_message("stream-frame", &public_pane_id, "owner-a", vec![1, 2, 3]);
    assert_eq!(
        server.handle_api_request_with_render_impact(request),
        RenderImpact::Graphics
    );
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(
        &response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
    )
    .is_ok());

    server
        .app
        .event_tx
        .try_send(AppEvent::UpdateReady {
            version: "9.9.9".into(),
            install_command: "herdr update".into(),
        })
        .unwrap();
    let (request, _response_rx) = stream_set_message(
        "stream-frame-with-internal-event",
        &public_pane_id,
        "owner-a",
        vec![4, 5, 6],
    );
    assert_eq!(
        server.handle_api_request_with_render_impact(request),
        RenderImpact::Full
    );

    server.app.state.pane_graphics_streams.clear();
    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    let impact = server.handle_api_request_with_render_impact(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "direct-frame".into(),
            method: api::schema::Method::PaneGraphicsSet(api::schema::PaneGraphicsSetParams {
                pane_id: public_pane_id,
                owner: String::new(),
                format: api::schema::PaneGraphicsFormat::Png,
                image_width: 1,
                image_height: 1,
                data: Some(vec![1, 2, 3]),
                data_base64: String::new(),
                placement: api::schema::PaneGraphicsPlacementParams::default(),
            }),
        },
        respond_to,
        response_write_complete: None,
    });
    assert_eq!(impact, RenderImpact::Full);
}

#[test]
fn rejected_or_stale_requests_do_not_schedule_rendering() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("graphics");
    let pane_id = workspace.tabs[0].root_pane;
    let public_pane_id = format!("{}:p1", workspace.id);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "disabled-set".into(),
            method: api::schema::Method::PaneGraphicsSet(api::schema::PaneGraphicsSetParams {
                pane_id: public_pane_id.clone(),
                owner: String::new(),
                format: api::schema::PaneGraphicsFormat::Png,
                image_width: 1,
                image_height: 1,
                data: Some(vec![1, 2, 3]),
                data_base64: String::new(),
                placement: api::schema::PaneGraphicsPlacementParams::default(),
            }),
        },
        respond_to,
        response_write_complete: None,
    });
    assert!(!changed);
    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<api::schema::ErrorResponse>(&response)
            .unwrap()
            .error
            .code,
        "feature_disabled"
    );

    server.app.state.kitty_graphics_enabled = true;
    server
        .app
        .state
        .pane_graphics_streams
        .insert(pane_id, "current-owner".into());
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let impact = server.handle_api_request_with_render_impact(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "stale-close".into(),
            method: api::schema::Method::PaneGraphicsStreamClose(
                api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id,
                    owner: "stale-owner".into(),
                },
            ),
        },
        respond_to,
        response_write_complete: None,
    });
    assert_eq!(impact, RenderImpact::None);
    assert_eq!(
        server.app.state.pane_graphics_streams.get(&pane_id),
        Some(&"current-owner".to_string())
    );
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(
        &response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
    )
    .is_ok());
}
