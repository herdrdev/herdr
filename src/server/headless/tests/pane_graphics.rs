use super::*;

fn receive_render(receiver: &std::sync::mpsc::Receiver<Vec<u8>>, timeout: Duration) -> Vec<u8> {
    receiver.recv_timeout(timeout).unwrap()
}

#[tokio::test]
async fn first_kitty_image_updates_retained_surface_without_full_redraw() {
    let (mut server, _control_rx, client_rx, pane_id) =
        retained_test_server_with_control(b"text before image");
    let client = server.clients.get_mut(&1).unwrap();
    client.mode = ClientConnectionMode::ClientShell;
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    server.render_and_stream();
    let ServerMessage::PaneSurface(initial) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("expected text-only baseline");
    };
    assert!(initial.graphics.placements.is_empty());

    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
    );
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let ServerMessage::PaneSurface(repainted) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("expected image update without a full redraw");
    };
    assert_eq!(repainted.graphics.placements.len(), 1);
    assert_eq!(repainted.graphics.assets[0].data, [255, 0, 0, 255]);
    assert_eq!(repainted.panes[0].inner_rect, initial.panes[0].inner_rect);

    // Text changes while an image is visible must reuse its uploaded pixels.
    write_shared_test_pane(&mut server, pane_id, b"\rupdated text");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let ServerMessage::PaneSurface(text_update) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("expected retained text and image scene");
    };
    assert_eq!(
        text_update.graphics.placements,
        repainted.graphics.placements
    );
    assert!(text_update.graphics.assets.is_empty());
    assert!(frame_text(&text_update.frame).contains("updated text"));

    write_shared_test_pane(&mut server, pane_id, b"\x1b_Ga=d,d=A\x1b\\");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let ServerMessage::PaneSurface(deleted) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("expected image removal");
    };
    assert!(deleted.graphics.placements.is_empty());
    assert!(deleted.graphics.assets.is_empty());

    write_shared_test_pane(&mut server, pane_id, b"\rtext only again");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    assert!(matches!(
        read_server_message(receive_render(&client_rx, Duration::from_millis(100))),
        ServerMessage::PaneSurfacePatch(_)
    ));

    // A full output queue must not mark unsent pixels as delivered.
    fill_render_lane(&server);
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
    );
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    assert!(server.clients[&1]
        .render_state
        .last_pane_surface()
        .unwrap()
        .graphics
        .placements
        .is_empty());
    let _ = receive_render(&client_rx, Duration::from_millis(100));
    server.render_and_stream();
    let ServerMessage::PaneSurface(recovered) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("expected deferred graphics recovery");
    };
    assert_eq!(recovered.graphics.assets[0].data, [255, 0, 0, 255]);
}

#[tokio::test]
async fn retained_unicode_image_arrives_after_fragmented_upload_without_reupload() {
    let (mut server, _control_rx, client_rx, pane_id) =
        retained_test_server_with_control(b"\x1b[?1049h");
    let client = server.clients.get_mut(&1).unwrap();
    client.mode = ClientConnectionMode::ClientShell;
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    server.render_and_stream();
    let _ = receive_render(&client_rx, Duration::from_millis(100));
    let sources = HashSet::from([pane_id]);
    // Yazi-style virtual placement: uploading the image and drawing its Unicode cell
    // can happen in separate PTY reads, with no text dirty rows when upload completes.
    for bytes in [
        b"\x1b_Ga=t,f=32,t=d,i=1193046,s=1,v=1,q=2;/wAA/w".as_slice(),
        b"==\x1b\\",
        b"\x1b_Ga=p,U=1,i=1193046,c=1,r=1,q=2\x1b\\",
    ] {
        write_shared_test_pane(&mut server, pane_id, bytes);
        assert!(server.render_retained_pane_surface_and_stream(&sources));
        for frame in client_rx.try_iter() {
            if let ServerMessage::PaneSurface(surface) = read_server_message(frame) {
                assert!(surface.graphics.placements.is_empty());
            }
        }
    }
    write_shared_test_pane(
        &mut server,
        pane_id,
        "\x1b[2;3H\x1b[38;2;18;52;86m\u{10eeee}\u{0305}\u{0305}\x1b[0m".as_bytes(),
    );
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    let ServerMessage::PaneSurface(surface) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("virtual image must arrive without a tab switch");
    };
    assert_eq!(surface.graphics.placements.len(), 1);
    assert_eq!(surface.graphics.assets[0].data, [255, 0, 0, 255]);
    // Retransmission removes placements; recreating the virtual placement must
    // invalidate the delivered asset without needing another text update.
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\x1b_Ga=t,f=32,t=d,i=1193046,s=1,v=1,q=2;AP8A/w==\x1b\\",
    );
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    let ServerMessage::PaneSurface(removed) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("retransmission must remove the virtual placement");
    };
    assert!(removed.graphics.placements.is_empty());
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\x1b_Ga=p,U=1,i=1193046,c=1,r=1,q=2\x1b\\",
    );
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    let ServerMessage::PaneSurface(replaced) =
        read_server_message(receive_render(&client_rx, Duration::from_millis(100)))
    else {
        panic!("updated image pixels must arrive");
    };
    assert_eq!(replaced.graphics.assets[0].data, [0, 255, 0, 255]);
    assert_ne!(
        surface.graphics.assets[0].key,
        replaced.graphics.assets[0].key
    );
}

#[tokio::test]
#[ignore = "manual retained text/image scaling profile"]
async fn render_scale_profile_retained_graphics() {
    use ratatui::layout::Direction;
    for retained in [false, true] {
        for with_image in [false, true] {
            for count in [1, 15] {
                let (mut server, _control_rx, client_rx, root) =
                    retained_test_server_with_control(b"populated terminal\r\n");
                let mut pane_ids = vec![root];
                for index in 1..count {
                    let workspace = &mut server.app.state.workspaces[0];
                    workspace.tabs[0]
                        .layout
                        .focus_pane(pane_ids[(index - 1) / 2]);
                    let id = workspace.test_split(if index % 2 == 0 {
                        Direction::Vertical
                    } else {
                        Direction::Horizontal
                    });
                    workspace.insert_test_runtime(
                        id,
                        crate::terminal::TerminalRuntime::test_with_screen_bytes(
                            80,
                            24,
                            b"populated terminal\r\n",
                        ),
                    );
                    pane_ids.push(id);
                }
                let client = server.clients.get_mut(&1).unwrap();
                client.mode = ClientConnectionMode::ClientShell;
                client.cell_size = crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                };
                if with_image {
                    write_shared_test_pane(
                        &mut server,
                        root,
                        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
                    );
                }
                server.render_and_stream();
                let _ = receive_render(&client_rx, Duration::from_millis(100));
                let sources = pane_ids.iter().copied().collect();
                let mut samples = Vec::new();
                for sample in 0..110 {
                    for id in &pane_ids {
                        write_shared_test_pane(
                            &mut server,
                            *id,
                            format!("\x1b[H{sample:03}").as_bytes(),
                        );
                    }
                    let started = Instant::now();
                    if retained {
                        assert!(server.render_retained_pane_surface_and_stream(&sources));
                    } else {
                        server.render_and_stream();
                    }
                    let elapsed = started.elapsed();
                    for frame in client_rx.try_iter() {
                        if let ServerMessage::PaneSurface(surface) = read_server_message(frame) {
                            assert!(surface.graphics.assets.is_empty());
                        }
                    }
                    if sample >= 10 {
                        samples.push(elapsed);
                    }
                }
                samples.sort_unstable();
                println!(
                "retained={retained} 80x24 panes={count} image={with_image} median_us={} p95_us={}",
                samples[50].as_micros(),
                samples[94].as_micros()
            );
            }
        }
    }
}

#[tokio::test]
async fn client_shell_surface_projects_terminal_kitty_images_from_authoritative_runtime() {
    let (mut server, _control_rx, client_rx, _pane_id) = retained_test_server_with_control(
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
    );
    let client = server.clients.get_mut(&1).unwrap();
    client.mode = ClientConnectionMode::ClientShell;
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };

    server.render_and_stream();
    let message = read_server_message(receive_render(&client_rx, Duration::from_millis(100)));
    let ServerMessage::PaneSurface(surface) = message else {
        panic!("expected client shell pane surface");
    };
    assert_eq!(surface.graphics.placements.len(), 1);
    assert_eq!(surface.graphics.assets.len(), 1);
    assert!(matches!(
        surface.graphics.placements[0].asset.source,
        crate::protocol::SurfaceGraphicsSource::Terminal {
            target: crate::protocol::SurfaceGraphicsTarget::Pane { .. },
            image_id: 7,
        }
    ));
    assert_eq!(surface.graphics.assets[0].data, vec![255, 0, 0, 255]);
}

#[tokio::test]
async fn client_shell_delivers_equal_pixels_for_distinct_terminal_image_ids() {
    let (mut server, _control_rx, client_rx, _pane_id) = retained_test_server_with_control(
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\\x1b_Ga=T,f=32,t=d,i=8,p=4,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
    );
    let client = server.clients.get_mut(&1).unwrap();
    client.mode = ClientConnectionMode::ClientShell;
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };

    server.render_and_stream();
    let message = read_server_message(receive_render(&client_rx, Duration::from_millis(100)));
    let ServerMessage::PaneSurface(surface) = message else {
        panic!("expected client shell pane surface");
    };
    assert_eq!(surface.graphics.placements.len(), 2);
    assert_eq!(surface.graphics.assets.len(), 2);
    assert_ne!(
        surface.graphics.assets[0].key,
        surface.graphics.assets[1].key
    );
    assert_eq!(
        surface.graphics.assets[0].data,
        surface.graphics.assets[1].data
    );

    server.clients.get_mut(&1).unwrap().request_repaint();
    server.render_and_stream();
    let message = read_server_message(receive_render(&client_rx, Duration::from_millis(100)));
    let ServerMessage::PaneSurface(surface) = message else {
        panic!("expected replacement client shell pane surface");
    };
    assert_eq!(surface.graphics.placements.len(), 2);
    assert!(surface.graphics.assets.is_empty());
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

#[tokio::test]
async fn pixel_mouse_activation_follows_child_1016_without_graphics_demand() {
    let (mut server, _client_rx, _pane_id) =
        retained_test_server(b"\x1b[?1003h\x1b[?1006h\x1b[?1016h");
    let (writer, control_rx, _render_rx) = test_client_writer();
    let client = server.clients.get_mut(&1).unwrap();
    client.writer = Some(writer);
    client.direct_graphics = false;
    client.pixel_mouse = true;
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    client.host_mouse_capture_active = None;
    client.host_sgr_pixels_active = None;

    server.stream_host_mouse_capture_mode();
    assert!(matches!(
        read_server_message(control_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
        ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: true
        }
    ));
}
