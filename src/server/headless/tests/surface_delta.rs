use super::*;

fn receive_message(receiver: &std::sync::mpsc::Receiver<Vec<u8>>) -> (Vec<u8>, ServerMessage) {
    let bytes = receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("render message");
    let message = read_server_message(bytes.clone());
    (bytes, message)
}

fn decode_surface_message(
    decoder: &mut protocol::surface_reuse::Decoder,
    message: ServerMessage,
) -> crate::protocol::PaneSurfaceFrame {
    match decoder.decode(message).expect("decode surface message") {
        ServerMessage::PaneSurface(surface) => surface,
        other => panic!("expected decoded pane surface, got {other:?}"),
    }
}

fn without_asset_payload(
    mut surface: crate::protocol::PaneSurfaceFrame,
) -> crate::protocol::PaneSurfaceFrame {
    surface.graphics.assets.clear();
    surface
}

fn enable_delta(server: &mut HeadlessServer, client_id: u64) {
    server
        .clients
        .get_mut(&client_id)
        .expect("delta client")
        .render_state
        .enable_surface_delta(true);
}

fn fill_render_lane(server: &HeadlessServer, client_id: u64) {
    let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
        .expect("dummy frame");
    server.clients[&client_id]
        .writer
        .as_ref()
        .expect("client writer")
        .test_fill_render(queued);
}

#[tokio::test]
async fn surface_delta_reconstructs_metadata_text_and_hyperlinks() {
    let (mut server, _control_rx, render_rx, pane_id) =
        retained_test_server_with_control(b"initial text");
    enable_delta(&mut server, 1);
    server.render_and_stream();
    let (initial_bytes, initial_message) = receive_message(&render_rx);
    let mut decoder = protocol::surface_reuse::Decoder::new(true);
    let initial = decode_surface_message(&mut decoder, initial_message);
    assert_eq!(
        without_asset_payload(initial.clone()),
        without_asset_payload(
            server.clients[&1]
                .render_state
                .last_pane_surface()
                .expect("initial baseline")
                .clone()
        )
    );

    let initial_projection_revision = initial.projection_revision;
    server.app.state.workspaces[0].custom_name = Some("renamed workspace".into());
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\rupdated text \x1b]8;;https://example.test/path\x1b\\linked\x1b]8;;\x1b\\",
    );
    server.clients.get_mut(&1).unwrap().request_recompute();
    assert!(!server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    server.render_and_stream();
    let (delta_bytes, delta_message) = receive_message(&render_rx);
    assert!(delta_bytes.len() < initial_bytes.len());
    assert!(matches!(
        &delta_message,
        ServerMessage::EndpointControl { kind, .. }
            if kind == protocol::surface_delta::MESSAGE_KIND
    ));
    let decoded = decode_surface_message(&mut decoder, delta_message);
    let committed = server.clients[&1]
        .render_state
        .last_pane_surface()
        .expect("updated baseline")
        .clone();
    assert_eq!(
        without_asset_payload(decoded.clone()),
        without_asset_payload(committed)
    );
    assert!(decoded.projection_revision > initial_projection_revision);
    assert!(frame_text(&decoded.frame).contains("updated text"));
    assert_eq!(decoded.frame.hyperlinks, vec!["https://example.test/path"]);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn surface_delta_updates_global_popup_without_affecting_legacy_peer() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("popup-delta");
    let pane_id = workspace.focused_pane_id().expect("focused pane");
    workspace.insert_test_runtime(
        pane_id,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 23, b"base"),
    );
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let popup_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 12, b"POPUP");
    let (_, popup_terminal_id) = server.app.install_test_popup_runtime(popup_runtime);

    let (_delta_control, delta_render) = connect_matching_test_shell(&mut server, 1);
    let (_legacy_control, legacy_render) = connect_matching_test_shell(&mut server, 2);
    enable_delta(&mut server, 1);
    server.render_and_stream();
    let (initial_bytes, initial_message) = receive_message(&delta_render);
    let (_, _legacy_initial) = receive_message(&legacy_render);
    let mut decoder = protocol::surface_reuse::Decoder::new(true);
    let _ = decode_surface_message(&mut decoder, initial_message);

    server
        .app
        .terminal_runtimes
        .get(&popup_terminal_id)
        .expect("popup runtime")
        .test_process_pty_bytes(b"\rPOPUP updated");
    server.clients.get_mut(&1).unwrap().request_recompute();
    server.render_and_stream();

    let (delta_bytes, delta_message) = receive_message(&delta_render);
    assert!(delta_bytes.len() * 4 < initial_bytes.len());
    assert!(matches!(
        &delta_message,
        ServerMessage::EndpointControl { kind, .. }
            if kind == protocol::surface_delta::MESSAGE_KIND
    ));
    let decoded = decode_surface_message(&mut decoder, delta_message);
    assert!(decoded
        .popup
        .as_ref()
        .is_some_and(|popup| frame_text(&popup.frame).contains("POPUP updated")));

    let (_, legacy_message) = receive_message(&legacy_render);
    assert!(matches!(legacy_message, ServerMessage::PaneSurface(_)));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn surface_delta_preserves_graphics_baseline_through_queue_recovery() {
    let (mut server, _control_rx, render_rx, pane_id) =
        retained_test_server_with_control(b"text before image");
    server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    enable_delta(&mut server, 1);
    server.render_and_stream();
    let (initial_bytes, initial_message) = receive_message(&render_rx);
    let mut decoder = protocol::surface_reuse::Decoder::new(true);
    let _ = decode_surface_message(&mut decoder, initial_message);

    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
    );
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let (image_bytes, image_message) = receive_message(&render_rx);
    assert!(image_bytes.len() * 4 < initial_bytes.len());
    assert!(matches!(
        &image_message,
        ServerMessage::EndpointControl { kind, .. }
            if kind == protocol::surface_delta::MESSAGE_KIND
    ));
    let image = decode_surface_message(&mut decoder, image_message);
    assert!(!image.graphics.placements.is_empty());

    write_shared_test_pane(&mut server, pane_id, b"\rupdated text");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let (text_bytes, text_message) = receive_message(&render_rx);
    assert!(text_bytes.len() * 4 < initial_bytes.len());
    assert!(matches!(
        &text_message,
        ServerMessage::EndpointControl { kind, .. }
            if kind == protocol::surface_delta::MESSAGE_KIND
    ));
    let text_update = decode_surface_message(&mut decoder, text_message);
    assert_eq!(text_update.graphics.placements, image.graphics.placements);
    assert!(text_update.graphics.assets.is_empty());

    let baseline_before_queue = server.clients[&1]
        .render_state
        .last_pane_surface()
        .expect("graphics baseline")
        .clone();
    fill_render_lane(&server, 1);
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\x1b_Ga=T,f=32,t=d,i=8,p=3,s=1,v=1,c=1,r=1,q=2;AP8A/w==\x1b\\",
    );
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    assert_eq!(
        without_asset_payload(
            server.clients[&1]
                .render_state
                .last_pane_surface()
                .expect("baseline after full queue")
                .clone()
        ),
        without_asset_payload(baseline_before_queue)
    );
    let _ = render_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("drain full render lane");

    server.render_and_stream();
    let (_, recovered_message) = receive_message(&render_rx);
    let recovered = decode_surface_message(&mut decoder, recovered_message);
    assert!(!recovered.graphics.placements.is_empty());
    assert_eq!(recovered.graphics.assets.len(), 1);
    assert_eq!(recovered.graphics.assets[0].data, [0, 255, 0, 255]);
    shutdown_test_runtimes(&mut server);
}
