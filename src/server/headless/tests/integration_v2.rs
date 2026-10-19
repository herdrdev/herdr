use super::*;

#[tokio::test]
async fn integration_v2_endpoint_dispatch_preserves_correlation_and_connection() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("integration-v2")];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    let (writer, control_rx, _render_rx) = test_client_writer();
    let client_id = 81;
    server.handle_server_event(ServerEvent::ClientShellConnected {
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        client_id,
        surface_cols: 80,
        surface_rows: 23,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: false,
        surface_active: true,
        writer,
    });
    let _snapshot = client_shell_snapshot(&control_rx);
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellIntegrationV2Request {
            client_id,
            boot_id: server.client_shell_boot_id.clone(),
            request: api::integration_v2::Request {
                id: "v2-unknown".into(),
                method: api::integration_v2::Method::Install(api::integration_v2::InstallParams {
                    target: "unknown-agent".into(),
                }),
            },
        })
    );
    let ServerMessage::ClientShellEndpointResponseChunk {
        boot_id,
        request_id,
        final_chunk,
        data,
    } = read_server_message(control_rx.recv_timeout(Duration::from_secs(1)).unwrap())
    else {
        panic!("expected integration v2 response");
    };
    assert_eq!(boot_id, server.client_shell_boot_id);
    assert_eq!(request_id, "v2-unknown");
    assert!(final_chunk);
    let response: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(response["id"], request_id);
    assert_eq!(response["error"]["code"], "invalid_target");
    assert!(server.clients.contains_key(&client_id));
    assert!(!server.clients[&client_id].shell_endpoint_command_in_flight);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn integration_v2_endpoint_dispatch_rejects_stale_busy_and_inactive_clients() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("integration-v2")];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    let (writer, control_rx, _render_rx) = test_client_writer();
    let client_id = 82;
    server.handle_server_event(ServerEvent::ClientShellConnected {
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        client_id,
        surface_cols: 80,
        surface_rows: 23,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: false,
        surface_active: true,
        writer,
    });
    let _snapshot = client_shell_snapshot(&control_rx);
    for (boot, busy, active, code) in [
        ("stale", false, true, "stale_boot"),
        (
            server.client_shell_boot_id.as_str(),
            true,
            true,
            "endpoint_busy",
        ),
        (
            server.client_shell_boot_id.as_str(),
            false,
            false,
            "surface_inactive",
        ),
    ]
    .map(|(boot, busy, active, code)| (boot.to_owned(), busy, active, code))
    {
        let client = server.clients.get_mut(&client_id).unwrap();
        client.shell_endpoint_command_in_flight = busy;
        client.shell_surface_active = active;
        assert!(
            !server.handle_server_event(ServerEvent::ClientShellIntegrationV2Request {
                client_id,
                boot_id: boot,
                request: api::integration_v2::Request {
                    id: code.into(),
                    method: api::integration_v2::Method::Install(
                        api::integration_v2::InstallParams {
                            target: "unknown-agent".into(),
                        }
                    ),
                },
            })
        );
        let ServerMessage::ClientShellEndpointResponseChunk { data, .. } =
            read_server_message(control_rx.recv_timeout(Duration::from_secs(1)).unwrap())
        else {
            panic!("expected rejection");
        };
        let response: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(response["id"], code);
        assert_eq!(response["error"]["code"], code);
        assert!(server.clients.contains_key(&client_id));
    }
    shutdown_test_runtimes(&mut server);
}
