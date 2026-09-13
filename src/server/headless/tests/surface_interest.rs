use super::*;

use std::sync::{Arc, Mutex};

use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, EndpointNegotiation, EndpointRegistry,
    EndpointTransport, ProfileId, SavedSshEndpoint,
};

#[derive(Clone)]
struct CapturingEndpointTransport(Arc<Mutex<Vec<crate::protocol::ClientMessage>>>);

impl EndpointTransport for CapturingEndpointTransport {
    fn send(&mut self, message: &crate::protocol::ClientMessage) -> std::io::Result<()> {
        self.0.lock().unwrap().push(message.clone());
        Ok(())
    }
}

fn lifecycle_negotiation() -> EndpointNegotiation {
    EndpointNegotiation::new(
        vec!["client_shell.surface.set".into()],
        vec![
            crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
            crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
        ],
    )
}

fn lifecycle_resize() -> crate::protocol::ClientMessage {
    crate::protocol::ClientMessage::ClientShellResize {
        cell_width_px: 8,
        cell_height_px: 16,
        surface_size: crate::protocol::ClientSurfaceSize { cols: 80, rows: 24 },
        pixel_mouse: false,
    }
}

fn request_active_surface(server: &mut HeadlessServer, client_id: u64, request_id: &str) {
    let boot_id = server.client_shell_boot_id.clone();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id,
            request: Box::new(api::schema::Request {
                id: request_id.into(),
                method: api::schema::Method::ClientShellSurfaceSet(
                    api::schema::ClientShellSurfaceSetParams { active: true },
                ),
            }),
        })
    );
}

#[tokio::test]
async fn metadata_only_shell_is_isolated_until_surface_activation() {
    let mut server = test_headless_server();
    let mut input_rx = install_focused_test_runtime(&mut server, b"");
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    let workspace_id = server.app.session_snapshot().focused_workspace_id.unwrap();
    let original_size = server.effective_size;
    let (writer, control_rx, render_rx) = test_client_writer();
    let client_id = 52;

    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id,
            surface_cols: 101,
            surface_rows: 37,
            cell_width_px: 9,
            cell_height_px: 18,
            pixel_mouse: true,
            direct_graphics: false,
            endpoint_keybindings: true,
            mouse_capture: true,
            surface_active: false,
            writer,
        })
    );
    assert!(matches!(
        read_server_message(control_rx.recv().expect("metadata snapshot")),
        ServerMessage::EndpointControl { .. }
    ));
    assert_eq!(server.foreground_client_id, None);
    assert_eq!(server.effective_size, original_size);

    server.render_and_stream();
    assert!(render_rx.try_recv().is_err());
    assert!(server.clients[&client_id]
        .render_state
        .last_pane_surface()
        .is_none());

    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id,
            pane_id,
            events: vec![crate::protocol::ClientPaneInputEvent::Paste(
                "blocked".into()
            )],
        })
    );
    assert!(input_rx.try_recv().is_err());

    let boot_id = server.client_shell_boot_id.clone();
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id: boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: "inactive-mutation".into(),
                method: api::schema::Method::WorkspaceFocus(api::schema::WorkspaceTarget {
                    workspace_id,
                }),
            }),
        })
    );
    let ServerMessage::ClientShellEndpointResponseChunk { data, .. } =
        read_server_message(control_rx.recv().expect("inactive mutation response"))
    else {
        panic!("expected endpoint response");
    };
    let error = serde_json::from_slice::<api::schema::ErrorResponse>(&data).unwrap();
    assert_eq!(error.error.code, "surface_inactive");

    assert!(
        server.send_to_client_shells(ServerMessage::SemanticNotification(
            crate::protocol::SemanticNotification {
                kind: crate::protocol::SemanticNotificationKind::Custom,
                title: "metadata event".into(),
                body: None,
                sound: None,
                agent: None,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                position: None,
            },
        ))
    );
    assert!(matches!(
        read_server_message(control_rx.recv().expect("metadata notification")),
        ServerMessage::SemanticNotification(_)
    ));

    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id: boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: "activate-surface".into(),
                method: api::schema::Method::ClientShellSurfaceSet(
                    api::schema::ClientShellSurfaceSetParams { active: true },
                ),
            }),
        })
    );
    let ServerMessage::ClientShellEndpointResponseChunk { data, .. } =
        read_server_message(control_rx.recv().expect("surface activation response"))
    else {
        panic!("expected typed surface activation response");
    };
    let activation_ack = serde_json::from_slice::<api::schema::SuccessResponse>(&data).unwrap();
    let api::schema::ResponseResult::ClientShellSurfaceSet {
        active: true,
        projection_revision: activation_floor,
    } = activation_ack.result
    else {
        panic!("expected typed surface activation result");
    };
    assert_eq!(server.foreground_client_id, Some(client_id));
    assert_eq!(server.effective_size, (101, 37));

    server.render_and_stream();
    let ServerMessage::PaneSurface(surface) =
        read_server_message(render_rx.recv().expect("activated surface"))
    else {
        panic!("expected pane surface");
    };
    assert_eq!((surface.frame.width, surface.frame.height), (101, 37));
    assert!(surface.projection_revision >= activation_floor);
    assert_eq!(surface.surface_revision, 1);
    server
        .clients
        .get_mut(&client_id)
        .expect("surface client")
        .shell_endpoint_command_in_flight = true;

    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id: boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: "deactivate-surface".into(),
                method: api::schema::Method::ClientShellSurfaceSet(
                    api::schema::ClientShellSurfaceSetParams { active: false },
                ),
            }),
        })
    );
    let _ = control_rx.recv().expect("surface deactivation response");
    assert!(server.clients.contains_key(&client_id));
    let (_, runtime_pane_id) = server.app.parse_pane_id(&surface.panes[0].pane_id).unwrap();
    server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, runtime_pane_id)
        .unwrap()
        .test_process_pty_bytes(b"REACTIVATED");
    assert!(
        server.render_retained_pane_surface_and_stream(&std::collections::HashSet::from([
            runtime_pane_id
        ]))
    );
    assert!(render_rx.try_recv().is_err());

    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id,
            request: Box::new(api::schema::Request {
                id: "reactivate-surface".into(),
                method: api::schema::Method::ClientShellSurfaceSet(
                    api::schema::ClientShellSurfaceSetParams { active: true },
                ),
            }),
        })
    );
    let data = loop {
        let message =
            read_server_message(control_rx.recv().expect("surface reactivation response"));
        match message {
            ServerMessage::ClientShellEndpointResponseChunk {
                request_id, data, ..
            } if request_id == "reactivate-surface" => break data,
            ServerMessage::EndpointControl { .. }
            | ServerMessage::ClientShellEndpointResponseChunk { .. } => continue,
            other => panic!("unexpected surface reactivation message: {other:?}"),
        }
    };
    let reactivation_ack = serde_json::from_slice::<api::schema::SuccessResponse>(&data).unwrap();
    let api::schema::ResponseResult::ClientShellSurfaceSet {
        active: true,
        projection_revision: reactivation_floor,
    } = reactivation_ack.result
    else {
        panic!("expected typed surface reactivation result");
    };
    assert!(reactivation_floor > activation_floor);
    server.render_and_stream();
    let ServerMessage::PaneSurface(surface) =
        read_server_message(render_rx.recv().expect("reactivated surface"))
    else {
        panic!("expected replacement pane surface");
    };
    assert!(frame_text(&surface.frame).contains("REACTIVATED"));
    assert!(surface.projection_revision >= reactivation_floor);
    assert_eq!(surface.surface_revision, 2);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn background_surface_activation_preserves_focused_viewer_geometry() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (focused_control, _) = connect_test_shell(&mut server, 7, 68, 17);
    let _ = focused_control.recv().expect("focused client snapshot");
    assert!(server.handle_server_event(ServerEvent::ClientShellFocus {
        client_id: 7,
        focused: true,
    }));
    let focused_size = server.app.state.workspaces[0].test_runtimes[&pane_id].current_size();
    assert_eq!(focused_size, (17, 67));
    let shared_tab_id = server.shell_tab_id_for_client(7).expect("focused tab");
    assert_eq!(
        server.tab_geometry_controllers.get(&shared_tab_id),
        Some(&7)
    );

    let (writer, background_control, _) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: 8,
            surface_cols: 100,
            surface_rows: 35,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            surface_active: false,
            writer,
        })
    );
    let _ = background_control
        .recv()
        .expect("background client snapshot");

    request_active_surface(&mut server, 8, "activate-background-surface");
    let _ = background_control
        .recv()
        .expect("background surface activation response");
    assert_eq!(
        server.shell_tab_id_for_client(8).as_deref(),
        Some(shared_tab_id.as_str())
    );
    assert_eq!(server.clients[&7].outer_terminal_focus, Some(true));
    assert_eq!(server.clients[&8].outer_terminal_focus, None);
    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&pane_id].current_size(),
        focused_size,
        "surface activation must not transiently resize a focused viewer's tab"
    );
    assert_eq!(
        server.tab_geometry_controllers.get(&shared_tab_id),
        Some(&7)
    );

    assert!(server.handle_server_event(ServerEvent::ClientShellFocus {
        client_id: 8,
        focused: false,
    }));
    assert_eq!(server.clients[&7].outer_terminal_focus, Some(true));
    assert_eq!(server.clients[&8].outer_terminal_focus, Some(false));
    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&pane_id].current_size(),
        focused_size
    );
    assert_eq!(
        server.tab_geometry_controllers.get(&shared_tab_id),
        Some(&7)
    );

    request_active_surface(&mut server, 8, "synchronize-background-surface");
    let _ = background_control
        .recv()
        .expect("background presentation synchronization response");
    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&pane_id].current_size(),
        focused_size
    );
    assert_eq!(
        server.tab_geometry_controllers.get(&shared_tab_id),
        Some(&7)
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn focused_surface_reassertion_reclaims_tab_geometry() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (focused_control, _) = connect_test_shell(&mut server, 8, 100, 35);
    let _ = focused_control.recv().expect("focused client snapshot");
    assert!(server.handle_server_event(ServerEvent::ClientShellFocus {
        client_id: 8,
        focused: true,
    }));
    let shared_tab_id = server.shell_tab_id_for_client(8).expect("focused tab");

    let (other_control, _) = connect_test_shell(&mut server, 7, 68, 17);
    let _ = other_control.recv().expect("other client snapshot");
    assert!(server.claim_shell_tab_geometry(7, false));
    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&pane_id].current_size(),
        (17, 67)
    );

    request_active_surface(&mut server, 8, "reassert-focused-surface");
    let _ = focused_control
        .recv()
        .expect("focused surface reassertion response");

    assert_eq!(server.clients[&8].outer_terminal_focus, Some(true));
    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&pane_id].current_size(),
        (35, 99)
    );
    assert_eq!(
        server.tab_geometry_controllers.get(&shared_tab_id),
        Some(&8)
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn presentation_sync_epoch_replays_modes_and_title() {
    let mut server = test_headless_server();
    let (writer, control_rx, _render_rx) = test_client_writer();
    let client_id = 63;
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id,
            surface_cols: 80,
            surface_rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: true,
            mouse_capture: true,
            surface_active: true,
            writer,
        })
    );
    let _ = control_rx.recv().expect("initial snapshot");
    server.api_window_title = Some("target title".into());
    {
        let client = server.clients.get_mut(&client_id).unwrap();
        client.host_mouse_capture_active = Some(false);
        client.host_sgr_pixels_active = Some(false);
        client.host_keyboard_report_all_active = Some(false);
    }

    let boot_id = server.client_shell_boot_id.clone();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id,
            request: Box::new(api::schema::Request {
                id: "post-commit-reassert".into(),
                method: api::schema::Method::ClientShellSurfaceSet(
                    api::schema::ClientShellSurfaceSetParams { active: true },
                ),
            }),
        })
    );
    let _ = control_rx
        .recv()
        .expect("typed surface reassertion acknowledgement");
    server.stream_host_mouse_capture_mode();
    server.stream_direct_terminal_keyboard_mode();
    server.sync_window_title();
    assert_eq!(
        server.clients[&client_id].host_mouse_capture_active,
        Some(true),
        "the target mode is sent after, not during, the frozen handoff"
    );
    assert_eq!(
        server.clients[&client_id].host_keyboard_report_all_active,
        Some(false)
    );
    let messages = (0..3)
        .map(|_| read_server_message(control_rx.recv().expect("reassertion effect")))
        .collect::<Vec<_>>();
    assert!(messages
        .iter()
        .any(|message| matches!(message, ServerMessage::MouseCapture { enabled: true, .. })));
    assert!(messages.iter().any(|message| matches!(
        message,
        ServerMessage::ClientShellKeyboardReportAll { enabled: false }
    )));
    assert!(messages.iter().any(|message| matches!(
        message,
        ServerMessage::WindowTitle { title: Some(title) } if title == "target title"
    )));
    shutdown_test_runtimes(&mut server);
}

fn dispatch_lifecycle_messages(
    server: &mut HeadlessServer,
    client_id: u64,
    messages: Vec<crate::protocol::ClientMessage>,
) {
    for message in messages {
        let event = match message {
            crate::protocol::ClientMessage::ClientShellResize {
                cell_width_px,
                cell_height_px,
                surface_size,
                pixel_mouse,
            } => ServerEvent::ClientShellResize {
                client_id,
                cell_width_px,
                cell_height_px,
                surface_cols: surface_size.cols,
                surface_rows: surface_size.rows,
                pixel_mouse,
            },
            crate::protocol::ClientMessage::ClientShellFocus { focused } => {
                ServerEvent::ClientShellFocus { client_id, focused }
            }
            crate::protocol::ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                ServerEvent::ClientShellEndpointRequest {
                    client_id,
                    boot_id,
                    request: Box::new(serde_json::from_str(&request).unwrap()),
                }
            }
            other => panic!("unhandled lifecycle message: {other:?}"),
        };
        server.handle_server_event(event);
    }
}

/// A real two-server/client lifecycle harness. Both source-off and target-on traverse the
/// production HeadlessServer endpoint request path; the client test only routes its emitted wire
/// messages and never authors an acknowledgement, snapshot, or surface response.
#[tokio::test]
async fn two_headless_servers_drive_atomic_endpoint_handoff() {
    let mut source_server = test_headless_server();
    let _source_input = install_focused_test_runtime(&mut source_server, b"local source");
    let (source_writer, source_control, _source_render) = test_client_writer();
    let source_client_id = 78;
    assert!(
        source_server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: source_client_id,
            surface_cols: 80,
            surface_rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: true,
            mouse_capture: true,
            surface_active: true,
            writer: source_writer,
        })
    );
    let source_snapshot = client_shell_snapshot(read_server_message(
        source_control
            .recv()
            .expect("real source metadata snapshot"),
    ));

    let mut target_server = test_headless_server();
    let _target_input = install_focused_test_runtime(&mut target_server, b"remote target");
    let (target_writer, target_control, target_render) = test_client_writer();
    let target_client_id = 79;
    assert!(
        target_server.handle_server_event(ServerEvent::ClientShellConnected {
            client_id: target_client_id,
            surface_cols: 80,
            surface_rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: true,
            mouse_capture: true,
            surface_active: false,
            writer: target_writer,
        })
    );
    let remote_snapshot = client_shell_snapshot(read_server_message(
        target_control
            .recv()
            .expect("real target metadata snapshot"),
    ));

    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Remote".into(),
        target: "dev@example.com".into(),
        session: "main".into(),
        enabled: true,
    };
    let target_id = ClientEndpointId::Ssh(profile.id.clone());
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    shell.set_endpoint_catalog(&[profile]);
    shell.set_snapshot(source_snapshot);
    shell.set_endpoint_status(&target_id, ClientEndpointStatus::Online);
    shell.set_endpoint_snapshot(&target_id, remote_snapshot);

    let source_sent = Arc::new(Mutex::new(Vec::new()));
    let target_sent = Arc::new(Mutex::new(Vec::new()));
    let mut endpoints = EndpointRegistry::new(
        CapturingEndpointTransport(source_sent.clone()),
        1,
        lifecycle_negotiation(),
    );
    endpoints.insert(
        target_id.clone(),
        CapturingEndpointTransport(target_sent.clone()),
        7,
        lifecycle_negotiation(),
        false,
    );
    let mut activation = crate::client::endpoint::PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target_id.clone(),
        None,
        lifecycle_resize(),
        41,
        std::time::Instant::now(),
    )
    .unwrap();

    // Route the source-off-first client messages through a second real HeadlessServer. Its
    // typed response is the only source acknowledgement supplied to the activation state.
    let mut source_release_request_id = None;
    for message in std::mem::take(&mut *source_sent.lock().unwrap()) {
        match message {
            crate::protocol::ClientMessage::ClientShellFocus { focused } => {
                assert!(
                    source_server.handle_server_event(ServerEvent::ClientShellFocus {
                        client_id: source_client_id,
                        focused,
                    })
                );
            }
            crate::protocol::ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                let request = serde_json::from_str::<api::schema::Request>(&request).unwrap();
                if matches!(
                    request.method,
                    api::schema::Method::ClientShellSurfaceSet(
                        api::schema::ClientShellSurfaceSetParams { active: false }
                    )
                ) {
                    source_release_request_id = Some(request.id.clone());
                }
                assert!(source_server.handle_server_event(
                    ServerEvent::ClientShellEndpointRequest {
                        client_id: source_client_id,
                        boot_id,
                        request: Box::new(request),
                    }
                ));
            }
            other => panic!("unexpected source lifecycle message: {other:?}"),
        }
    }
    let source_release_request_id = source_release_request_id.expect("client source-off request");
    let source_release_data = loop {
        let message = read_server_message(source_control.recv().expect("source typed release ack"));
        match message {
            ServerMessage::ClientShellEndpointResponseChunk {
                request_id, data, ..
            } if request_id == source_release_request_id => break data,
            ServerMessage::EndpointControl { .. }
            | ServerMessage::MouseCapture { .. }
            | ServerMessage::ClientShellKeyboardReportAll { .. }
            | ServerMessage::WindowTitle { .. }
            | ServerMessage::ClientShellEndpointResponseChunk { .. } => continue,
            other => panic!("unexpected source release message: {other:?}"),
        }
    };
    assert_eq!(
        activation.receive_response(
            &ClientEndpointId::Local,
            1,
            &source_release_request_id,
            &source_release_data,
            &mut endpoints,
        ),
        crate::client::endpoint::SurfaceActivationProgress::Pending
    );

    dispatch_lifecycle_messages(
        &mut target_server,
        target_client_id,
        std::mem::take(&mut *target_sent.lock().unwrap()),
    );
    assert_eq!(
        target_server.clients[&target_client_id].outer_terminal_focus,
        Some(true)
    );
    assert_eq!(
        source_server.clients[&source_client_id].outer_terminal_focus,
        Some(false)
    );
    let ServerMessage::ClientShellEndpointResponseChunk {
        request_id, data, ..
    } = read_server_message(target_control.recv().expect("target typed activation ack"))
    else {
        panic!("expected target activation acknowledgement");
    };
    assert_eq!(
        activation.receive_response(&target_id, 7, &request_id, &data, &mut endpoints),
        crate::client::endpoint::SurfaceActivationProgress::Pending
    );

    target_server.render_and_stream();
    let coherent_snapshot = client_shell_snapshot(read_server_message(
        target_control.recv().expect("target replacement snapshot"),
    ));
    let snapshot_progress = activation.receive_snapshot(&target_id, 7, &coherent_snapshot);
    shell.set_endpoint_snapshot(&target_id, coherent_snapshot);
    assert_eq!(
        snapshot_progress,
        crate::client::endpoint::SurfaceActivationProgress::Pending
    );
    let ServerMessage::PaneSurface(coherent_surface) =
        read_server_message(target_render.recv().expect("target replacement surface"))
    else {
        panic!("expected target pane surface");
    };
    assert_eq!(
        activation.receive_surface(&target_id, 7, coherent_surface),
        crate::client::endpoint::SurfaceActivationProgress::Ready
    );

    assert!(matches!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(crate::client::endpoint::ActivationCompletion::AwaitingPresentationSync {
            endpoint,
            ..
        }) if endpoint == target_id
    ));
    assert!(
        !endpoints.active_surface_available(),
        "target input remains fenced while presentation effects resynchronize"
    );

    let sync_request = target_sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|message| match message {
            crate::protocol::ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                let request = serde_json::from_str::<api::schema::Request>(request).ok()?;
                request
                    .id
                    .ends_with(":presentation-sync")
                    .then(|| (boot_id.clone(), Box::new(request)))
            }
            _ => None,
        })
        .expect("client presentation synchronization request");
    assert!(
        target_server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id: target_client_id,
            boot_id: sync_request.0,
            request: sync_request.1,
        })
    );
    let (sync_request_id, sync_data) = loop {
        let message = read_server_message(target_control.recv().expect("presentation sync ack"));
        if let ServerMessage::ClientShellEndpointResponseChunk {
            request_id, data, ..
        } = message
        {
            if request_id.ends_with(":presentation-sync") {
                break (request_id, data);
            }
        }
    };
    assert_eq!(
        activation.receive_response(&target_id, 7, &sync_request_id, &sync_data, &mut endpoints,),
        crate::client::endpoint::SurfaceActivationProgress::Pending
    );
    target_server.render_and_stream();
    let sync_snapshot = loop {
        let message =
            read_server_message(target_control.recv().expect("presentation sync snapshot"));
        if let ServerMessage::EndpointControl { kind, data } = message {
            if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND {
                break serde_json::from_str::<crate::protocol::ClientShellSnapshot>(&data).unwrap();
            }
        }
    };
    let sync_progress = activation.receive_snapshot(&target_id, 7, &sync_snapshot);
    shell.set_endpoint_snapshot_for_generation(&target_id, 7, Box::new(sync_snapshot));
    assert_eq!(
        sync_progress,
        crate::client::endpoint::SurfaceActivationProgress::Pending
    );
    let ServerMessage::PaneSurface(sync_surface) =
        read_server_message(target_render.recv().expect("presentation sync surface"))
    else {
        panic!("expected synchronized target surface");
    };
    assert_eq!(
        activation.receive_surface(&target_id, 7, sync_surface),
        crate::client::endpoint::SurfaceActivationProgress::Ready
    );
    assert_eq!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(crate::client::endpoint::ActivationCompletion::AwaitingPresentationEffects)
    );
    let effects_token = target_sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|message| match message {
            crate::protocol::ClientMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND =>
            {
                Some(data.clone())
            }
            _ => None,
        })
        .expect("client presentation effects fence");
    assert!(
        target_server.handle_server_event(ServerEvent::ClientShellPresentationSync {
            client_id: target_client_id,
            token: effects_token.clone(),
        })
    );
    let mut replayed_mouse = false;
    let mut replayed_keyboard = false;
    loop {
        match read_server_message(target_control.recv().expect("presentation effect or fence")) {
            ServerMessage::MouseCapture { .. } => replayed_mouse = true,
            ServerMessage::ClientShellKeyboardReportAll { .. } => replayed_keyboard = true,
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND =>
            {
                assert_eq!(data, effects_token);
                assert_eq!(
                    activation.receive_presentation_effects_ready(&target_id, 7, &data),
                    crate::client::endpoint::SurfaceActivationProgress::Ready
                );
                break;
            }
            ServerMessage::WindowTitle { .. } => {}
            other => panic!("unexpected presentation fence message: {other:?}"),
        }
    }
    assert!(replayed_mouse);
    assert!(replayed_keyboard);
    assert_eq!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(crate::client::endpoint::ActivationCompletion::Activated)
    );
    endpoints.unfreeze_input();
    assert_eq!(endpoints.active_id(), &target_id);
    assert!(endpoints.active_surface_available());
    assert!(shell.endpoint_is_active(&target_id));

    target_sent.lock().unwrap().clear();
    let mut returning = crate::client::endpoint::PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        ClientEndpointId::Local,
        None,
        lifecycle_resize(),
        42,
        std::time::Instant::now(),
    )
    .unwrap();
    dispatch_lifecycle_messages(
        &mut target_server,
        target_client_id,
        std::mem::take(&mut *target_sent.lock().unwrap()),
    );
    loop {
        if let ServerMessage::ClientShellEndpointResponseChunk {
            request_id, data, ..
        } = read_server_message(target_control.recv().unwrap())
        {
            returning.receive_response(&target_id, 7, &request_id, &data, &mut endpoints);
            break;
        }
    }
    dispatch_lifecycle_messages(
        &mut source_server,
        source_client_id,
        std::mem::take(&mut *source_sent.lock().unwrap()),
    );
    assert_eq!(
        source_server.clients[&source_client_id].outer_terminal_focus,
        Some(true),
        "returning to Local must restore focus without a host focus event"
    );
    assert_eq!(
        target_server.clients[&target_client_id].outer_terminal_focus,
        Some(false)
    );
    shutdown_test_runtimes(&mut source_server);
    shutdown_test_runtimes(&mut target_server);
}
