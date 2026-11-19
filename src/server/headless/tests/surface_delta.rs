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

#[test]
fn native_hyperlinks_reserve_only_enabled_plugin_matches_per_client() {
    fn receive_plugin_refresh(
        server: &mut HeadlessServer,
        runtime: &tokio::runtime::Runtime,
    ) -> AppEvent {
        loop {
            let event = runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), server.app.event_rx.recv())
                    .await
                    .expect("background plugin refresh completed")
                    .expect("app event channel")
            });
            if matches!(event, AppEvent::PluginLinkPatternsRefreshed { .. }) {
                return event;
            }
            server.handle_internal_event_with_forwarding(event);
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    fn targets(frame: &FrameData) -> HashSet<&str> {
        frame
            .cells
            .iter()
            .filter_map(|cell| frame.hyperlinks.get(cell.hyperlink? as usize))
            .map(String::as_str)
            .collect()
    }

    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    struct RestoreConfigHome(Option<std::ffi::OsString>);
    impl Drop for RestoreConfigHome {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
    let _env = RestoreConfigHome(std::env::var_os("XDG_CONFIG_HOME"));
    let mut server = test_headless_server();
    std::env::set_var(
        "XDG_CONFIG_HOME",
        server.client_socket_path.parent().unwrap(),
    );
    server.app.policy.persist_plugin_registry = true;
    let pane_id = install_shared_view_test_runtime(&mut server);
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\r\x1b]8;;https://example.test\x1b\\web\x1b]8;;\x1b\\ \
          \x1b]8;;obsidian://open?vault=notes\x1b\\note\x1b]8;;\x1b\\ \
          \x1b]8;;vscode://file/local\x1b\\editor\x1b]8;;\x1b\\ \
          \x1b]8;;winscp-sftp://host/path\x1b\\transfer\x1b]8;;\x1b\\",
    );
    for uri in [
        "https://example.test/wrapped",
        "obsidian://open?vault=notes",
    ] {
        write_shared_test_pane(
            &mut server,
            pane_id,
            format!(
                "\r\n\x1b]8;;{uri}\x1b\\{}\x1b]8;;\x1b\\",
                "label".repeat(40)
            )
            .as_bytes(),
        );
    }
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\r\nhttps://example.test/plain\r\nhttps://example.test/plain-wrapped/abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    );
    let root = server
        .client_socket_path
        .parent()
        .unwrap()
        .join("link-plugin");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("herdr-plugin.toml"),
        r#"
id = "example.links"
name = "Links"
version = "0.1.0"
min_herdr_version = "0.9.3"
platforms = ["linux", "macos", "windows"]
[[actions]]
id = "open"
title = "Open note"
command = ["unused"]
[[link_handlers]]
id = "note"
title = "Note"
pattern = "^obsidian://"
action = "open"
[[link_handlers]]
id = "web"
title = "Web"
pattern = "^https://example[.]test($|/plain)"
action = "open"
"#,
    )
    .unwrap();
    let linked = server.app.handle_api_request(api::schema::Request {
        id: "link".into(),
        method: api::schema::Method::PluginLink(api::schema::PluginLinkParams {
            path: root.display().to_string(),
            enabled: true,
            source: None,
        }),
    });
    assert!(linked.contains("plugin_linked"), "{linked}");
    let (_control, captured) = connect_matching_test_shell(&mut server, 1);
    let (_control, native) = connect_matching_test_shell(&mut server, 2);
    server.handle_server_event(ServerEvent::ClientShellMouseCapture {
        client_id: 1,
        enabled: true,
    });
    enable_delta(&mut server, 1);
    let mut decoder = protocol::surface_reuse::Decoder::new(true, false);
    let all = HashSet::from([
        "https://example.test",
        "https://example.test/wrapped",
        "https://example.test/plain",
        "https://example.test/plain-wrapped/abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "obsidian://open?vault=notes",
        "vscode://file/local",
        "winscp-sftp://host/path",
    ]);
    let ordinary = HashSet::from([
        "https://example.test/wrapped",
        "vscode://file/local",
        "winscp-sftp://host/path",
    ]);
    server.render_and_stream();
    let (_, message) = receive_message(&captured);
    let initial = decode_surface_message(&mut decoder, message);
    assert_eq!(targets(&initial.frame), ordinary);
    let wrapped_rows = initial
        .frame
        .cells
        .chunks(usize::from(initial.frame.width))
        .filter(|row| {
            row.iter().any(|cell| {
                cell.hyperlink
                    .and_then(|index| initial.frame.hyperlinks.get(index as usize))
                    .is_some_and(|uri| uri == "https://example.test/wrapped")
            })
        })
        .count();
    assert!(
        wrapped_rows >= 2,
        "wrapped labels must keep their full URL on every row"
    );
    assert_eq!(
        targets(&recv_pane_surface(&native, "native peer").frame),
        all
    );
    let response = server.app.handle_api_request(api::schema::Request {
        id: "hover-reserved-label".into(),
        method: api::schema::Method::PaneLinkResolve(api::schema::PaneLinkActivateParams {
            pane_id: initial.panes[0].pane_id.clone(),
            viewport_row: 0,
            col: 1,
            content_revision: None,
            offset_from_bottom: None,
        }),
    });
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(
        response["result"]["regions"],
        serde_json::json!([{ "row": 0, "start_col": 0, "end_col": 2 }]),
        "plugin reservation must retain server-resolved hover regions"
    );

    // A blocked filesystem worker must not stop input handling or frame output.
    crate::persist::plugin_registry::with_registry_lock(|| {
        server.handle_scheduled_tasks_headless(Instant::now() + Duration::from_secs(3), false);
        assert!(server.app.plugin_link_refresh_in_flight);
        server.handle_scheduled_tasks_headless(Instant::now() + Duration::from_secs(6), false);
        assert!(
            server.handle_server_event(ServerEvent::ClientShellMouseCapture {
                client_id: 1,
                enabled: false,
            })
        );
        server.render_and_stream();
        let (_, message) = receive_message(&captured);
        let updated = decode_surface_message(&mut decoder, message);
        assert_eq!(targets(&updated.frame), all);
        Ok(())
    })
    .unwrap();
    let result = receive_plugin_refresh(&mut server, &runtime);
    assert!(!server.handle_internal_event_with_forwarding(result));
    server.handle_server_event(ServerEvent::ClientShellMouseCapture {
        client_id: 1,
        enabled: true,
    });
    server.render_and_stream();
    let (_, message) = receive_message(&captured);
    assert_eq!(
        targets(&decode_surface_message(&mut decoder, message).frame),
        ordinary
    );

    // Changing plugin ownership must update an unchanged screen through deltas.
    for (enabled, expected, elapsed) in [(false, &all, 9), (true, &ordinary, 12)] {
        // A different named session or offline CLI writes the shared registry.
        crate::persist::plugin_registry::update(|plugins| {
            plugins[0].enabled = enabled;
        })
        .unwrap();
        server
            .handle_scheduled_tasks_headless(Instant::now() + Duration::from_secs(elapsed), false);
        let result = receive_plugin_refresh(&mut server, &runtime);
        assert!(server.handle_internal_event_with_forwarding(result));
        server.render_and_stream();
        let (_, message) = receive_message(&captured);
        assert!(
            matches!(&message, ServerMessage::EndpointControl { kind, .. }
            if kind == protocol::surface_delta::MESSAGE_KIND)
        );
        let updated = decode_surface_message(&mut decoder, message);
        assert_eq!(targets(&updated.frame), *expected);
    }

    // A newer foreground refresh wins even when it confirms unchanged patterns.
    crate::persist::plugin_registry::update(|plugins| plugins[0].enabled = false).unwrap();
    server.handle_scheduled_tasks_headless(Instant::now() + Duration::from_secs(15), false);
    let stale_result = receive_plugin_refresh(&mut server, &runtime);
    crate::persist::plugin_registry::update(|plugins| plugins[0].enabled = true).unwrap();
    let listed = server.app.handle_api_request(api::schema::Request {
        id: "newer-plugin-list".into(),
        method: api::schema::Method::PluginList(api::schema::PluginListParams { plugin_id: None }),
    });
    assert!(listed.contains("plugin_list"), "{listed}");
    assert!(!server.handle_internal_event_with_forwarding(stale_result));
    server.render_and_stream();
    for bytes in captured.try_iter() {
        let updated = decode_surface_message(&mut decoder, read_server_message(bytes));
        assert_eq!(targets(&updated.frame), ordinary);
    }
    assert_eq!(
        targets(
            &server.clients[&1]
                .render_state
                .last_pane_surface()
                .unwrap()
                .frame
        ),
        ordinary
    );
    // Plugin activation still resolves the raw destination, which is absent from
    // the outgoing cells. A claimed launch failure must not open it natively.
    server.app.state.plugin_commands_in_flight = usize::MAX;
    let response = server.app.handle_api_request(api::schema::Request {
        id: "activate".into(),
        method: api::schema::Method::PaneLinkActivate(api::schema::PaneLinkActivateParams {
            pane_id: initial.panes[0].pane_id.clone(),
            viewport_row: 0,
            col: 5,
            content_revision: None,
            offset_from_bottom: None,
        }),
    });
    assert!(response.contains("plugin_link_failed"), "{response}");
    server.handle_server_event(ServerEvent::ClientShellMouseCapture {
        client_id: 1,
        enabled: false,
    });
    server.render_and_stream();
    let (_, message) = receive_message(&captured);
    let updated = decode_surface_message(&mut decoder, message);
    assert_eq!(targets(&updated.frame), all);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn wrapped_plain_text_links_update_every_native_segment() {
    use crate::protocol::render_ansi::BlitEncoder;

    let pattern = regex::Regex::new("/claimed$").unwrap();
    for reserved in [false, true] {
        let (mut server, _control, render, pane_id) =
            retained_test_server_with_control(b"ordinary text");
        enable_delta(&mut server, 1);
        server.clients.get_mut(&1).unwrap().shell_mouse_capture = reserved;
        if reserved {
            server.app.plugin_link_patterns = vec![pattern.clone()];
        }
        server.render_and_stream();
        let (_, message) = receive_message(&render);
        let mut decoder = protocol::surface_reuse::Decoder::new(true, false);
        let initial = decode_surface_message(&mut decoder, message);
        let width = initial.panes[0].inner_rect.width;
        let prefix = format!("https://example.test/{}", "x".repeat(usize::from(width)));
        let mut encoder = BlitEncoder::new();
        let mut host = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let encoded = encoder.encode(&initial.frame, true);
        host.write(&encoded.bytes);
        encoder.commit(initial.frame, encoded);

        for (bytes, expected, native) in [
            (
                format!("\r\x1b[2J\x1b[H{prefix}/claimed"),
                Some(format!("{prefix}/claimed")),
                !reserved,
            ),
            // Only the last row changes. Earlier cells must acquire the new URI,
            // including when a plugin previously hid their native targets.
            (
                "\x1b[7D\x1b[Kpublic".to_owned(),
                Some(format!("{prefix}/public")),
                true,
            ),
            ("\x1b[Hxxxx".to_owned(), None, false),
        ] {
            write_shared_test_pane(&mut server, pane_id, bytes.as_bytes());
            if !server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])) {
                server.render_and_stream();
            }
            let (_, message) = receive_message(&render);
            let surface = if matches!(message, ServerMessage::PaneSurfacePatch(_)) {
                server.clients[&1]
                    .render_state
                    .last_pane_surface()
                    .unwrap()
                    .clone()
            } else {
                decode_surface_message(&mut decoder, message)
            };
            let encoded = encoder.encode(&surface.frame, false);
            host.write(&encoded.bytes);
            let area = surface.panes[0].inner_rect;
            let mut linked_rows = HashSet::new();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let uri = host.viewport_hyperlink_uri(x, u32::from(y)).unwrap();
                    if let Some(uri) = uri {
                        assert!(native, "plugin-owned or removed URL stayed native");
                        assert_eq!(Some(&uri), expected.as_ref());
                        linked_rows.insert(y);
                    }
                }
            }
            if native {
                assert!(
                    linked_rows.len() >= 2,
                    "every wrapped segment must be native"
                );
                assert_eq!(
                    host.viewport_hyperlink_uri(area.x, u32::from(area.y))
                        .unwrap(),
                    expected
                );
            } else {
                assert!(linked_rows.is_empty());
            }
            encoder.commit(surface.frame, encoded);
        }
        shutdown_test_runtimes(&mut server);
    }
}

#[tokio::test]
async fn surface_delta_reconstructs_metadata_text_and_hyperlinks() {
    let (mut server, _control_rx, render_rx, pane_id) =
        retained_test_server_with_control(b"initial text");
    enable_delta(&mut server, 1);
    server.render_and_stream();
    let (initial_bytes, initial_message) = receive_message(&render_rx);
    let mut decoder = protocol::surface_reuse::Decoder::new(true, false);
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
    let mut decoder = protocol::surface_reuse::Decoder::new(true, false);
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
    let mut decoder = protocol::surface_reuse::Decoder::new(true, false);
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
