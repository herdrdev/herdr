use super::*;

fn receive_render(receiver: &std::sync::mpsc::Receiver<Vec<u8>>, timeout: Duration) -> Vec<u8> {
    receiver.recv_timeout(timeout).unwrap()
}

#[tokio::test]
async fn cold_redraw_advances_one_bounded_layer_after_each_send() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"cold redraw");
    server.app.state.kitty_graphics_enabled = true;
    server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    const LAYERS: usize = 8;
    for index in 0..LAYERS {
        set_named_graphics_layer(
            &mut server,
            pane_id,
            &format!("layer-{index:02}"),
            vec![index as u8; 1024 * 1024],
            index as i32,
        );
    }

    fill_render_lane(&server);
    server.render_and_stream();
    assert!(server.clients[&1].graphics_cache.is_empty());
    let _older = client_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    for expected in 1..=LAYERS {
        server.render_and_stream();
        let bytes = client_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(bytes.len() <= MAX_GRAPHICS_FRAME_SIZE + 4);
        let frame = read_server_frame(bytes);
        assert_eq!(
            frame
                .graphics
                .windows(4)
                .filter(|part| *part == b"a=t,")
                .count(),
            1
        );
        assert_eq!(
            server.clients[&1].graphics_cache.test_image_count(),
            expected
        );
    }
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
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
    read_server_frame(receive_render(client_rx, Duration::from_millis(100)))
}

fn graphics_key(pane_id: crate::layout::PaneId) -> crate::app::pane_graphics::Key {
    (pane_id, api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.into())
}

fn active_gate() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true))
}

fn set_graphics_layer(server: &mut HeadlessServer, pane_id: crate::layout::PaneId, data: Vec<u8>) {
    set_named_graphics_layer(
        server,
        pane_id,
        api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID,
        data,
        0,
    );
}

fn set_named_graphics_layer(
    server: &mut HeadlessServer,
    pane_id: crate::layout::PaneId,
    layer_id: &str,
    data: Vec<u8>,
    z_index: i32,
) {
    let key = (pane_id, layer_id.into());
    let host_image_id = server.app.pane_graphics.reserve_image_id(&key).unwrap();
    let layer = crate::app::pane_graphics::Layer::inline(
        api::schema::PaneGraphicsFormat::Png,
        1,
        1,
        data,
        Default::default(),
        z_index,
    );
    server.app.pane_graphics.slots.insert(
        key,
        crate::app::pane_graphics::Slot::test(host_image_id, Some(layer)),
    );
}

fn set_stream_owner(server: &mut HeadlessServer, pane_id: crate::layout::PaneId, owner: &str) {
    let key = graphics_key(pane_id);
    if let Some(slot) = server.app.pane_graphics.slots.get_mut(&key) {
        slot.stream_owner = Some(owner.into());
        slot.stream_active = Some(active_gate());
    } else {
        let host_image_id = server.app.pane_graphics.reserve_image_id(&key).unwrap();
        let mut slot = crate::app::pane_graphics::Slot::test(host_image_id, None);
        slot.stream_owner = Some(owner.into());
        slot.stream_active = Some(active_gate());
        server.app.pane_graphics.slots.insert(key, slot);
    }
}

fn fill_render_lane(server: &HeadlessServer) {
    let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
        .expect("dummy frame");
    server.clients[&1]
        .writer
        .as_ref()
        .unwrap()
        .test_fill_render(queued);
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
                        layer_id: None,
                        z_index: 0,
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
            stream_active: None,
        },
        response_rx,
    )
}

#[cfg(unix)]
fn sparse_direct_frame(
    server: &HeadlessServer,
    name: &str,
    image_width: u32,
    image_height: u32,
) -> String {
    use std::os::unix::fs::OpenOptionsExt as _;

    let path = server
        .app
        .pane_graphics_files
        .source_directory()
        .unwrap()
        .join(name);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.set_len(u64::from(image_width) * u64::from(image_height) * 4)
        .unwrap();
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn direct_stream_message(
    id: &str,
    pane_id: &str,
    owner: &str,
    path: String,
    image_width: u32,
    image_height: u32,
) -> (api::ApiRequestMessage, std::sync::mpsc::Receiver<String>) {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    (
        api::ApiRequestMessage {
            request: api::schema::Request {
                id: id.into(),
                method: api::schema::Method::PaneGraphicsStreamDirect(
                    api::schema::PaneGraphicsDirectParams {
                        pane_id: pane_id.into(),
                        layer_id: None,
                        z_index: 0,
                        owner: owner.into(),
                        image_width,
                        image_height,
                        format: api::schema::PaneGraphicsFormat::Rgba,
                        path,
                        sequence: 1,
                        revision: 1,
                        placement: Default::default(),
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        },
        response_rx,
    )
}

#[tokio::test]
async fn pixel_mouse_activation_requires_graphics_demand_not_direct_transport() {
    let (mut server, _client_rx, pane_id) =
        retained_test_server(b"\x1b[?1003h\x1b[?1006h\x1b[?1016h");
    let (writer, control_rx, _render_rx) = test_client_writer();
    let client = server.clients.get_mut(&1).unwrap();
    client.writer = Some(writer);
    client.direct_graphics = false;
    client.pixel_mouse = true;
    client.host_mouse_capture_active = None;
    client.host_sgr_pixels_active = None;
    server.app.direct_graphics_available = false;

    server.stream_host_mouse_capture_mode();
    assert!(matches!(
        read_server_message(control_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
        ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: false
        }
    ));

    set_graphics_layer(&mut server, pane_id, vec![1, 2, 3]);
    server.stream_host_mouse_capture_mode();
    assert!(matches!(
        read_server_message(control_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
        ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: true
        }
    ));
}

#[tokio::test]
async fn pixel_input_metadata_cannot_resize_authoritative_client_state() {
    let (mut server, _client_rx, pane_id) =
        retained_test_server(b"\x1b[?1003h\x1b[?1006h\x1b[?1016h");
    set_graphics_layer(&mut server, pane_id, vec![1]);
    let client = server.clients.get_mut(&1).unwrap();
    client.pixel_mouse = true;
    client.host_sgr_pixels_active = Some(true);
    server.foreground_client_id = None;
    assert!(!server.handle_server_event(ServerEvent::ClientInputPixels {
        client_id: 1,
        data: b"\x1b[<0;500;300M".to_vec(),
        geometry: crate::input::mouse::HostGeometry::new(80, 24, 800, 480).unwrap(),
    }));
    server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    for (geometry, data) in [
        (
            crate::input::mouse::HostGeometry::new(100, 30, 1_000, 600).unwrap(),
            b"\x1b[<0;500;300M".as_slice(),
        ),
        (
            crate::input::mouse::HostGeometry::new(80, 24, 960, 480).unwrap(),
            b"\x1b[<0;500;300M",
        ),
        (
            crate::input::mouse::HostGeometry::new(80, 24, 800, 480).unwrap(),
            b"\x1b[<0;0;1M",
        ),
    ] {
        assert!(!server.handle_server_event(ServerEvent::ClientInputPixels {
            client_id: 1,
            data: data.to_vec(),
            geometry,
        }));
    }
    assert_eq!(server.clients[&1].terminal_size, (80, 24));
    assert_eq!(
        (server.effective_size, server.foreground_client_id),
        ((80, 24), None)
    );
}

#[test]
fn direct_eligibility_is_installed_with_the_client_connection() {
    let mut server = test_headless_server();
    let (writer, _control_rx, _render_rx) = test_client_writer();

    assert!(server.handle_server_event(ServerEvent::ClientConnected {
        client_id: 7,
        cols: 80,
        rows: 24,
        cell_width_px: 10,
        cell_height_px: 20,
        render_encoding: RenderEncoding::SemanticFrame,
        keybindings: None,
        direct_attach_requested: false,
        direct_graphics: true,
        writer,
    }));

    let client = server.clients.get(&7).expect("connected client");
    assert!(client.direct_graphics);
    assert_eq!(server.foreground_client_id, Some(7));
    assert!(server.app.direct_graphics_available);
}

#[tokio::test]
async fn focus_repaint_preserves_uploaded_graphics() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let (client_2_writer, _client_2_control_rx, client_2_rx) = test_client_writer();
    server.clients.insert(
        2,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize {
                width_px: 10,
                height_px: 20,
            },
            crate::terminal_theme::TerminalTheme::default(),
            Some(false),
            0,
            RenderEncoding::SemanticFrame,
            Some(client_2_writer),
        ),
    );
    set_graphics_layer(&mut server, pane_id, vec![1, 2, 3]);
    let initial = enable_graphics_and_render(&mut server, &client_rx);
    let initial_graphics = String::from_utf8_lossy(&initial.graphics);
    assert!(initial_graphics.contains("a=t"));
    assert!(initial_graphics.contains("a=p"));
    let client_2_initial =
        read_server_frame(receive_render(&client_2_rx, Duration::from_millis(100)));
    assert!(String::from_utf8_lossy(&client_2_initial.graphics).contains("a=t"));

    assert!(server.handle_server_event(ServerEvent::ClientInput {
        client_id: 2,
        data: b"\x1b[I".to_vec(),
    }));
    assert_eq!(server.foreground_client_id, Some(2));
    server.render_and_stream();

    let focused = read_server_frame(receive_render(&client_2_rx, Duration::from_millis(100)));
    let focused_graphics = String::from_utf8_lossy(&focused.graphics);
    assert!(focused_graphics.contains("a=p"));
    assert!(!focused_graphics.contains("a=t"));
}

#[tokio::test]
async fn resize_replays_placement_without_retransmitting_or_closing_stream() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    set_graphics_layer(&mut server, pane_id, vec![1, 2, 3]);
    set_stream_owner(&mut server, pane_id, "owner-resize");
    let initial = enable_graphics_and_render(&mut server, &client_rx);
    assert!(String::from_utf8_lossy(&initial.graphics).contains("a=t"));

    for (cols, rows, cell_width_px, cell_height_px) in
        [(100, 30, 10, 20), (100, 30, 12, 24), (100, 30, 12, 24)]
    {
        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 1,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
        }));
        server.render_and_stream();
        let frame = read_server_frame(receive_render(&client_rx, Duration::from_millis(100)));
        let graphics = String::from_utf8_lossy(&frame.graphics);
        assert!(!graphics.contains("a=t"));
        assert!(graphics.contains("a=p"));
    }
    assert_eq!(
        server
            .app
            .pane_graphics
            .slots
            .get(&graphics_key(pane_id))
            .and_then(|slot| slot.stream_owner.as_deref()),
        Some("owner-resize")
    );
}

#[tokio::test]
async fn graphics_pruning_preserves_live_panes_and_removes_closed_panes() {
    let (mut server, _client_rx, pane_id) = retained_test_server(b"aaaa");
    set_graphics_layer(&mut server, pane_id, vec![1, 2, 3]);

    assert!(!server
        .app
        .pane_graphics
        .retain_live_panes(&server.app.state));
    assert!(server
        .app
        .pane_graphics
        .slots
        .contains_key(&graphics_key(pane_id)));

    server.app.state.workspaces.clear();
    assert!(server
        .app
        .pane_graphics
        .retain_live_panes(&server.app.state));
    assert!(server.app.pane_graphics.slots.is_empty());
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
async fn retained_graphics_stays_ordered_after_an_older_render() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &client_rx);
    fill_render_lane(&server);
    set_graphics_layer(&mut server, pane_id, vec![4, 5, 6]);
    let older = client_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    let graphics = client_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        read_server_message(older),
        ServerMessage::ReloadSoundConfig
    ));
    assert!(matches!(
        read_server_message(graphics),
        ServerMessage::Graphics { .. }
    ));
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
fn stream_open_gate_is_owned_by_the_layer_and_cancels_on_removal() {
    let mut server = test_headless_server();
    server.app.state.kitty_graphics_enabled = true;
    let workspace = crate::workspace::Workspace::test_new("gated");
    let pane_id = workspace.tabs[0].root_pane;
    let public = format!("{}:p1", workspace.id);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    let active = active_gate();
    let (respond_to, response_rx) = std::sync::mpsc::channel();

    server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "open-gated".into(),
            method: api::schema::Method::PaneGraphicsStreamOpen(
                api::schema::PaneGraphicsStreamParams {
                    pane_id: public.clone(),
                    layer_id: None,
                    z_index: 0,
                    owner: "worker-1".into(),
                },
            ),
        },
        respond_to,
        response_write_complete: None,
        stream_active: Some(active.clone()),
    });
    assert!(
        serde_json::from_str::<api::schema::SuccessResponse>(&response_rx.recv().unwrap()).is_ok()
    );
    let (frame, frame_response) =
        stream_set_message("gated-frame", &public, "worker-1", vec![1, 2, 3]);
    assert_eq!(
        server.handle_api_request_with_render_impact(frame),
        RenderImpact::Graphics
    );
    assert!(frame_response.recv().is_ok());
    assert!(active.load(std::sync::atomic::Ordering::Acquire));
    active.store(false, std::sync::atomic::Ordering::Release);
    let (delayed, delayed_response) =
        stream_set_message("delayed-frame", &public, "worker-1", vec![4, 5, 6]);
    assert_eq!(
        server.handle_api_request_with_render_impact(delayed),
        RenderImpact::None
    );
    let error: api::schema::ErrorResponse =
        serde_json::from_str(&delayed_response.recv().unwrap()).unwrap();
    assert_eq!(error.error.code, "stream_closed");
    assert!(server
        .app
        .pane_graphics
        .slots
        .remove(&graphics_key(pane_id))
        .is_some());
    assert!(!active.load(std::sync::atomic::Ordering::Acquire));
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
    set_stream_owner(&mut server, pane_id, "owner-a");

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

    server.app.pane_graphics.clear();
    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    let impact = server.handle_api_request_with_render_impact(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "direct-frame".into(),
            method: api::schema::Method::PaneGraphicsSet(api::schema::PaneGraphicsSetParams {
                pane_id: public_pane_id,
                layer_id: None,
                z_index: 0,
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
        stream_active: None,
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
                layer_id: None,
                z_index: 0,
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
        stream_active: None,
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
    set_stream_owner(&mut server, pane_id, "current-owner");
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let impact = server.handle_api_request_with_render_impact(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "stale-close".into(),
            method: api::schema::Method::PaneGraphicsStreamClose(
                api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id,
                    layer_id: None,
                    z_index: 0,
                    owner: "stale-owner".into(),
                },
            ),
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    assert_eq!(impact, RenderImpact::None);
    assert_eq!(
        server
            .app
            .pane_graphics
            .slots
            .get(&graphics_key(pane_id))
            .and_then(|slot| slot.stream_owner.as_deref()),
        Some("current-owner")
    );
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(
        &response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
    )
    .is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn hidden_large_direct_frame_uploads_then_replays_placement_without_closing_stream() {
    let (mut server, client_rx, _) = retained_test_server(b"active");
    enable_graphics_and_render(&mut server, &client_rx);
    let background_tab = server.app.state.workspaces[0].test_add_tab(Some("browser"));
    let pane_id = server.app.state.workspaces[0].tabs[background_tab].root_pane;
    let pane_number = server.app.state.workspaces[0]
        .public_pane_number(pane_id)
        .unwrap();
    let public_pane_id = crate::workspace::public_pane_id_for_number(
        &server.app.state.workspaces[0].id,
        pane_number,
    );
    server.clients.get_mut(&1).unwrap().direct_graphics = true;
    server.app.direct_graphics_available = true;
    set_stream_owner(&mut server, pane_id, "browser");

    let image_width = 2_048;
    let image_height = 2_049;
    let expected_len = u64::from(image_width) * u64::from(image_height) * 4;
    assert!(expected_len > api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES as u64);
    let path = sparse_direct_frame(
        &server,
        "hidden-large-frame.rgba",
        image_width,
        image_height,
    );
    let (message, response_rx) = direct_stream_message(
        "hidden-frame",
        &public_pane_id,
        "browser",
        path,
        image_width,
        image_height,
    );

    assert_eq!(
        server.handle_pane_graphics_stream_frame(message),
        RenderImpact::None
    );
    let (transfer_id, image_id, control, leading) = match read_server_message(
        client_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("hidden direct upload"),
    ) {
        ServerMessage::GraphicsFile {
            transfer_id,
            image_id,
            control,
            leading,
            expected_len: sent_len,
            ..
        } => {
            assert_eq!(sent_len, expected_len);
            (transfer_id, image_id, control, leading)
        }
        other => panic!("expected graphics file, got {other:?}"),
    };
    assert!(leading.is_empty());
    assert!(control.starts_with("a=t,"), "{control}");
    assert!(!control.contains("p="), "{control}");
    assert!(response_rx.try_recv().is_err());

    server.app.state.workspaces[0].switch_tab(background_tab);
    server.render_and_stream();
    let frame = read_server_frame(
        client_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("frame while upload is pending"),
    );
    assert!(!frame.graphics.windows(4).any(|bytes| bytes == b"a=p,"));

    server.app.state.workspaces[0].switch_tab(0);
    server.render_and_stream();
    let _hidden_again = read_server_frame(
        client_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("frame after hiding pending upload"),
    );
    server.start_direct_graphics_response(1, transfer_id, image_id);
    assert!(server.complete_direct_graphics(1, transfer_id, image_id, true));
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(
        &response_rx.recv_timeout(Duration::from_secs(1)).unwrap()
    )
    .is_ok());
    let slot = &server.app.pane_graphics.slots[&graphics_key(pane_id)];
    assert!(slot.stream_is_active());
    assert!(slot.layer.as_ref().unwrap().terminal_only());

    server.app.state.workspaces[0].switch_tab(background_tab);
    server.render_and_stream();
    let frame = read_server_frame(
        client_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("placement replay after tab switch"),
    );
    let graphics = String::from_utf8_lossy(&frame.graphics);
    assert!(graphics.contains("a=p,"), "{graphics:?}");
    assert!(graphics.contains(&format!("i={image_id}")), "{graphics:?}");
    assert!(!graphics.contains("a=t,"), "{graphics:?}");

    let next_path = sparse_direct_frame(
        &server,
        "visible-next-frame.rgba",
        image_width,
        image_height,
    );
    let (message, next_response_rx) = direct_stream_message(
        "visible-frame",
        &public_pane_id,
        "browser",
        next_path,
        image_width,
        image_height,
    );
    assert_eq!(
        server.handle_pane_graphics_stream_frame(message),
        RenderImpact::None
    );
    match read_server_message(
        client_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("next visible direct frame"),
    ) {
        ServerMessage::GraphicsFile { control, .. } => {
            assert!(control.starts_with("a=T,"), "{control}");
        }
        other => panic!("expected graphics file, got {other:?}"),
    }
    assert!(next_response_rx.try_recv().is_err());
    assert!(server.app.pane_graphics.slots[&graphics_key(pane_id)].stream_is_active());
}

#[cfg(unix)]
#[tokio::test]
async fn hidden_small_direct_frame_preserves_owned_inline_fallback() {
    let (mut server, client_rx, _) = retained_test_server(b"active");
    enable_graphics_and_render(&mut server, &client_rx);
    let background_tab = server.app.state.workspaces[0].test_add_tab(Some("browser"));
    let pane_id = server.app.state.workspaces[0].tabs[background_tab].root_pane;
    let pane_number = server.app.state.workspaces[0]
        .public_pane_number(pane_id)
        .unwrap();
    let public_pane_id = crate::workspace::public_pane_id_for_number(
        &server.app.state.workspaces[0].id,
        pane_number,
    );
    server.clients.get_mut(&1).unwrap().direct_graphics = true;
    server.app.direct_graphics_available = true;
    set_stream_owner(&mut server, pane_id, "browser");

    let path = sparse_direct_frame(&server, "hidden-small-frame.rgba", 1, 1);
    let (message, response_rx) =
        direct_stream_message("hidden-small", &public_pane_id, "browser", path, 1, 1);
    assert_eq!(
        server.handle_pane_graphics_stream_frame(message),
        RenderImpact::Graphics
    );
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(
        &response_rx.recv_timeout(Duration::from_secs(1)).unwrap()
    )
    .is_ok());
    assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    let slot = &server.app.pane_graphics.slots[&graphics_key(pane_id)];
    assert!(slot.stream_is_active());
    assert_eq!(
        slot.layer.as_ref().unwrap().inline_data(),
        Some([0; 4].as_slice())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn direct_frame_during_internal_redraw_uploads_without_placement() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"active");
    enable_graphics_and_render(&mut server, &client_rx);
    let pane_number = server.app.state.workspaces[0]
        .public_pane_number(pane_id)
        .unwrap();
    let public_pane_id = crate::workspace::public_pane_id_for_number(
        &server.app.state.workspaces[0].id,
        pane_number,
    );
    server.clients.get_mut(&1).unwrap().direct_graphics = true;
    server.app.direct_graphics_available = true;
    set_stream_owner(&mut server, pane_id, "browser");
    server
        .app
        .event_tx
        .try_send(AppEvent::UpdateReady {
            version: "9.9.9".into(),
            install_command: "herdr update".into(),
        })
        .unwrap();

    let image_width = 2_048;
    let image_height = 2_049;
    let path = sparse_direct_frame(&server, "redraw-frame.rgba", image_width, image_height);
    let (message, response_rx) = direct_stream_message(
        "redraw",
        &public_pane_id,
        "browser",
        path,
        image_width,
        image_height,
    );
    assert_eq!(
        server.handle_pane_graphics_stream_frame(message),
        RenderImpact::Full
    );
    let (transfer_id, image_id) = match read_server_message(
        client_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("direct upload during redraw"),
    ) {
        ServerMessage::GraphicsFile {
            control,
            leading,
            transfer_id,
            image_id,
            ..
        } => {
            assert!(leading.is_empty());
            assert!(control.starts_with("a=t,"), "{control}");
            (transfer_id, image_id)
        }
        other => panic!("expected graphics file, got {other:?}"),
    };
    server.start_direct_graphics_response(1, transfer_id, image_id);
    assert!(server.complete_direct_graphics(1, transfer_id, image_id, true));
    assert!(response_rx.recv_timeout(Duration::from_secs(1)).is_ok());
    assert!(server.app.pane_graphics.slots[&graphics_key(pane_id)].stream_is_active());

    server.render_and_stream();
    let frame = read_server_frame(
        client_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("placement after redraw upload acknowledgement"),
    );
    let graphics = String::from_utf8_lossy(&frame.graphics);
    assert!(graphics.contains("a=p,"), "{graphics:?}");
    assert!(graphics.contains(&format!("i={image_id}")), "{graphics:?}");
    assert!(!graphics.contains("a=t,"), "{graphics:?}");
}

#[cfg(unix)]
fn direct_gate_server(
    data: &[u8],
) -> (
    HeadlessServer,
    crate::app::pane_graphics::Key,
    std::sync::mpsc::Receiver<String>,
) {
    direct_gate_server_with_file(data.len(), Some(data))
}

#[cfg(unix)]
fn direct_gate_server_with_file(
    len: usize,
    data: Option<&[u8]>,
) -> (
    HeadlessServer,
    crate::app::pane_graphics::Key,
    std::sync::mpsc::Receiver<String>,
) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("direct-gate");
    let pane_id = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    let key = graphics_key(pane_id);
    let path = server
        .app
        .pane_graphics_files
        .source_directory()
        .unwrap()
        .join("gate-frame");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    if let Some(data) = data {
        file.write_all(data).unwrap();
    } else {
        file.set_len(len as u64).unwrap();
    }
    drop(file);
    let lease = server.app.pane_graphics_files.lease(&path, len).unwrap();
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let layer =
        crate::app::pane_graphics::Layer::direct(1, 1, lease.clone(), Default::default(), 0);
    let mut slot = crate::app::pane_graphics::Slot::test((1 << 31) | 900, Some(layer));
    slot.stream_owner = Some("owner".into());
    slot.stream_active = Some(active_gate());
    slot.direct_gate = Some(crate::app::pane_graphics::DirectGate {
        transfer_id: lease.fingerprint(),
        client_id: 7,
        deadline: std::time::Instant::now() + Duration::from_secs(1),
        written: true,
        success_response: "ack".into(),
        respond_to,
    });
    server.app.pane_graphics.slots.insert(key.clone(), slot);
    (server, key, response_rx)
}

#[cfg(unix)]
fn direct_ids(server: &HeadlessServer, key: &crate::app::pane_graphics::Key) -> (u64, u32) {
    let slot = &server.app.pane_graphics.slots[key];
    (
        slot.direct_gate.as_ref().unwrap().transfer_id,
        slot.host_image_id,
    )
}

#[cfg(unix)]
fn add_direct_client(server: &mut HeadlessServer, client_id: u64) {
    let (writer, control_rx, render_rx) = test_client_writer();
    std::mem::forget((control_rx, render_rx));
    let mut client = ClientConnection::new(
        (80, 24),
        crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        },
        crate::terminal_theme::TerminalTheme::default(),
        None,
        1,
        RenderEncoding::SemanticFrame,
        Some(writer),
    );
    client.direct_graphics = true;
    client.pixel_mouse = true;
    server.clients.insert(client_id, client);
}

#[cfg(unix)]
#[test]
fn terminal_response_deadline_starts_only_after_client_flush() {
    let (mut server, key, _response_rx) = direct_gate_server(&[1, 2, 3, 4]);
    let slot = server.app.pane_graphics.slots.get_mut(&key).unwrap();
    let gate = slot.direct_gate.as_mut().unwrap();
    gate.written = false;
    let (transfer_id, image_id) = (gate.transfer_id, slot.host_image_id);
    assert!(!server.complete_direct_graphics(7, transfer_id, image_id, true));
    assert!(!server.start_direct_graphics_response(7, transfer_id, image_id));
    let gate = server.app.pane_graphics.slots[&key]
        .direct_gate
        .as_ref()
        .unwrap();
    assert!(gate.written && gate.deadline > std::time::Instant::now());
}

#[cfg(unix)]
#[test]
fn outer_timeout_covers_both_direct_phases_and_cancellation_blocks_late_results() {
    assert!(
        crate::app::pane_graphics::DIRECT_OUTER_TIMEOUT
            > crate::app::pane_graphics::DIRECT_DELIVERY_TIMEOUT
                + crate::app::pane_graphics::DIRECT_RESPONSE_TIMEOUT
    );
    let (mut server, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
    let slot = server.app.pane_graphics.slots.get_mut(&key).unwrap();
    slot.stream_active
        .as_ref()
        .unwrap()
        .store(false, std::sync::atomic::Ordering::Release);
    let (transfer_id, image_id) = (
        slot.direct_gate.as_ref().unwrap().transfer_id,
        slot.host_image_id,
    );

    assert!(!server.complete_direct_graphics(7, transfer_id, image_id, true));
    assert!(response_rx.try_recv().is_err());
    assert!(server.app.pane_graphics.slots[&key]
        .layer
        .as_ref()
        .unwrap()
        .direct_lease()
        .is_some());
}

#[cfg(unix)]
#[test]
fn matching_terminal_ok_releases_producer_and_acknowledges() {
    let (mut server, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
    let (transfer_id, image_id) = direct_ids(&server, &key);

    assert!(server.complete_direct_graphics(7, transfer_id, image_id, true));

    assert_eq!(response_rx.recv().unwrap(), "ack");
    let layer = server.app.pane_graphics.slots[&key].layer.as_ref().unwrap();
    assert!(layer.terminal_only());
    assert!(layer.direct_lease().is_none());
}

#[cfg(unix)]
#[test]
fn explicit_terminal_error_acks_only_after_owned_inline_fallback() {
    let (mut server, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
    add_direct_client(&mut server, 7);
    let (transfer_id, image_id) = direct_ids(&server, &key);
    let layer = server.app.pane_graphics.slots[&key].layer.as_ref().unwrap();
    server
        .clients
        .get_mut(&7)
        .unwrap()
        .graphics_cache
        .trust_pane_layer(&key, image_id, layer);
    assert!(server.complete_direct_graphics(7, transfer_id, image_id, false));

    let layer = server.app.pane_graphics.slots[&key].layer.as_ref().unwrap();
    assert_eq!(
        (
            response_rx.recv().unwrap(),
            layer.inline_data(),
            server.clients[&7].direct_graphics,
            server.clients[&7].pixel_mouse,
        ),
        ("ack".into(), Some([1, 2, 3, 4].as_slice()), false, true)
    );
    assert!(server.clients[&7].graphics_cache.is_empty());
}

#[cfg(unix)]
#[test]
fn large_direct_terminal_error_closes_without_acknowledging_or_copying() {
    let len = crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES + 4;
    let (mut server, key, response_rx) = direct_gate_server_with_file(len, None);
    add_direct_client(&mut server, 7);
    let (transfer_id, image_id) = direct_ids(&server, &key);

    assert!(server.complete_direct_graphics(7, transfer_id, image_id, false));
    assert!(!server.app.pane_graphics.slots.contains_key(&key));
    assert!(matches!(
        response_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected)
    ));
}

#[cfg(unix)]
#[test]
fn unwritten_direct_full_falls_back_without_stickiness_but_disconnect_retires() {
    for error in [
        std::sync::mpsc::TrySendError::Full(Vec::new()),
        std::sync::mpsc::TrySendError::Disconnected(Vec::new()),
    ] {
        let should_ack = matches!(error, std::sync::mpsc::TrySendError::Full(_));
        let (mut server, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
        add_direct_client(&mut server, 7);
        let gate = server
            .app
            .pane_graphics
            .slots
            .get_mut(&key)
            .and_then(|slot| slot.direct_gate.take())
            .unwrap();
        let result = server.handle_unwritten_direct_failure(
            &key,
            gate.success_response,
            gate.respond_to,
            error,
        );
        let inline = server
            .app
            .pane_graphics
            .slots
            .get(&key)
            .and_then(|slot| slot.layer.as_ref()?.inline_data())
            .is_some();
        assert_eq!(
            (
                result,
                response_rx.try_recv().ok().as_deref() == Some("ack"),
                inline,
                server.clients[&7].direct_graphics,
            ),
            (should_ack, should_ack, should_ack, true)
        );
    }
}

#[cfg(unix)]
#[test]
fn client_loss_retires_only_its_direct_stream() {
    let (mut pending, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
    pending.retire_direct_graphics_for_client(8);
    assert!(pending.app.pane_graphics.slots.contains_key(&key));
    pending.retire_direct_graphics_for_client(7);
    assert!(!pending.app.pane_graphics.slots.contains_key(&key));
    assert!(response_rx.recv().is_err());

    let (mut resident, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
    let slot = resident.app.pane_graphics.slots.get(&key).unwrap();
    assert!(resident.complete_direct_graphics(
        7,
        slot.direct_gate.as_ref().unwrap().transfer_id,
        slot.host_image_id,
        true,
    ));
    assert_eq!(response_rx.recv().unwrap(), "ack");
    resident.retire_direct_graphics_for_client(8);
    assert!(resident.app.pane_graphics.slots.contains_key(&key));
    resident.retire_direct_graphics_for_client(7);
    assert!(!resident.app.pane_graphics.slots.contains_key(&key));
}

#[cfg(unix)]
#[test]
fn pane_removal_and_shutdown_drop_direct_without_ack() {
    let setups: [fn(&mut HeadlessServer); 2] = [
        |server| server.app.state.workspaces.clear(),
        |server| server.shutting_down = true,
    ];
    for setup in setups {
        let (mut server, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
        let (transfer_id, image_id) = direct_ids(&server, &key);
        setup(&mut server);
        assert!(!server.complete_direct_graphics(7, transfer_id, image_id, true));
        assert!(response_rx.recv().is_err());
        assert!(!server.app.pane_graphics.slots.contains_key(&key));
    }
}

#[cfg(unix)]
#[test]
fn timeout_retires_stream_without_producer_ack() {
    let (mut server, key, response_rx) = direct_gate_server(&[1, 2, 3, 4]);
    add_direct_client(&mut server, 7);
    server
        .app
        .pane_graphics
        .slots
        .get_mut(&key)
        .unwrap()
        .direct_gate
        .as_mut()
        .unwrap()
        .deadline = std::time::Instant::now() - Duration::from_millis(1);

    assert!(server.expire_direct_graphics(std::time::Instant::now()));

    assert!(response_rx.recv().is_err());
    assert!(!server.app.pane_graphics.slots.contains_key(&key));
    assert!(!server.clients[&7].direct_graphics);
    assert!(server.clients[&7].pixel_mouse);
}
