use super::*;

fn animation_server(
    client_count: u64,
    pane_count: usize,
) -> (HeadlessServer, Vec<std::sync::mpsc::Receiver<Vec<u8>>>) {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("render benchmark");
    let pane_id = workspace.focused_pane_id().expect("focused pane");
    workspace.insert_test_runtime(
        pane_id,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"idle"),
    );
    let mut pane_ids = vec![pane_id];
    for index in 1..pane_count {
        let target = pane_ids[(index - 1) / 2];
        workspace.tabs[0].layout.focus_pane(target);
        let direction = if index % 2 == 0 {
            ratatui::layout::Direction::Vertical
        } else {
            ratatui::layout::Direction::Horizontal
        };
        let pane_id = workspace.test_split(direction);
        workspace.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"idle"),
        );
        pane_ids.push(pane_id);
    }
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.set_server_mode(crate::app::Mode::Terminal);
    server.app.state.hyperspace.enabled = true;

    let mut receivers = Vec::new();
    for client_id in 1..=client_count {
        let (writer, _control_rx, render_rx) = test_client_writer();
        server.clients.insert(
            client_id,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                client_id,
                RenderEncoding::SemanticFrame,
                Some(writer),
            ),
        );
        receivers.push(render_rx);
    }
    server.foreground_client_id = Some(1);
    server.sync_foreground_client_state();
    server.resize_shared_runtime_to_effective_size();
    server.render_and_stream();
    let initial_frames = receive_frames(&receivers);
    assert_eq!(initial_frames.len(), client_count as usize);
    assert!(server
        .clients
        .values()
        .all(|client| { client.animation_rect.width > 0 && client.animation_rect.height > 0 }));

    (server, receivers)
}

fn receive_frames(receivers: &[std::sync::mpsc::Receiver<Vec<u8>>]) -> Vec<FrameData> {
    receivers
        .iter()
        .map(|receiver| read_server_frame(receiver.recv().expect("render frame")))
        .collect()
}

fn assert_animation_falls_back_and_matches_full_render(
    mut server: HeadlessServer,
    receivers: &[std::sync::mpsc::Receiver<Vec<u8>>],
) {
    for client in server.clients.values_mut() {
        client.render_state.reset_baseline();
        client.render_terminal = None;
    }
    server.render_and_stream();
    let _ = receive_frames(receivers);

    let now = Instant::now() + crate::hyperspace::FRAME_INTERVAL * 2;
    let scheduled = server.handle_scheduled_tasks_headless_with_render_kind(now, false);
    assert!(scheduled.changed);
    assert!(scheduled.sidebar_animation_only);
    assert!(
        !server.render_sidebar_animation_and_stream(),
        "overlays must take the full-render path"
    );

    server.render_and_stream();
    let fallback_frames = receive_frames(receivers);
    for client in server.clients.values_mut() {
        client.render_state.reset_baseline();
        client.render_terminal = None;
    }
    server.render_and_stream();
    let fully_rendered = receive_frames(receivers);
    assert_eq!(fallback_frames, fully_rendered);
}

#[tokio::test]
async fn animation_patch_matches_a_full_render_for_every_client() {
    let (mut server, receivers) = animation_server(4, 15);
    server
        .clients
        .get_mut(&2)
        .expect("second app client")
        .dock_presentation
        .hovered_control = Some(crate::app::state::ControlId::SidebarAnimationPause);
    for client in server.clients.values_mut() {
        client.render_state.reset_baseline();
    }
    server.render_and_stream();
    let _ = receive_frames(&receivers);

    for client in server.clients.values_mut() {
        let mut frame = client
            .render_state
            .last_frame()
            .expect("cached semantic frame")
            .clone();
        frame.graphics = vec![0x1b, b'_'];
        client.render_state.restore_semantic_frame(frame);
    }

    let now = Instant::now() + crate::hyperspace::FRAME_INTERVAL * 2;
    let scheduled = server.handle_scheduled_tasks_headless_with_render_kind(now, false);
    assert!(scheduled.changed);
    assert!(scheduled.sidebar_animation_only);
    assert!(server.render_sidebar_animation_and_stream());
    let patched = receive_frames(&receivers);
    assert!(
        patched.iter().all(|frame| frame.graphics.is_empty()),
        "animation patch frames must not replay cached graphics"
    );

    for client in server.clients.values_mut() {
        client.render_state.reset_baseline();
        client.render_terminal = None;
    }
    server.render_and_stream();
    let fully_rendered = receive_frames(&receivers);
    assert_eq!(patched, fully_rendered);

    for client in server.clients.values_mut() {
        client.render_state.reset_baseline();
    }
    server.render_and_stream();
    let reused_terminal = receive_frames(&receivers);
    assert_eq!(fully_rendered, reused_terminal);
}

#[tokio::test]
async fn animation_patch_falls_back_when_settings_dims_the_background() {
    let (mut server, receivers) = animation_server(2, 15);
    server.app.state.set_server_mode(crate::app::Mode::Settings);

    assert_animation_falls_back_and_matches_full_render(server, &receivers);
}

#[tokio::test]
async fn animation_patch_falls_back_when_a_context_menu_covers_the_panel() {
    let (mut server, receivers) = animation_server(2, 15);
    let workspace_id = server.app.state.workspaces[0].id.clone();
    let client = server.clients.get_mut(&1).expect("first app client");
    let animation_rect = client.animation_rect;
    client.sidebar_presentation.overlay.kind = crate::app::state::ClientOverlay::ContextMenu;
    client.sidebar_presentation.overlay.context_menu = Some(crate::app::state::ContextMenuState {
        kind: crate::app::state::ContextMenuKind::Workspace {
            workspace_id,
            ws_idx: 0,
        },
        x: animation_rect.x,
        y: animation_rect.y,
        selected: crate::app::state::ContextMenuAction::RenameWorkspace,
    });
    assert!(animation_rect.width > 0 && animation_rect.height > 0);

    assert_animation_falls_back_and_matches_full_render(server, &receivers);
}

#[tokio::test]
#[ignore = "manual idle-render timing benchmark; see test output for reproducible command"]
async fn headless_idle_render_profile() {
    const CLIENTS: u64 = 4;
    const TICKS: usize = 120;
    for pane_count in [1, 15] {
        let (mut full_server, full_receivers) = animation_server(CLIENTS, pane_count);
        let (mut partial_server, partial_receivers) = animation_server(CLIENTS, pane_count);
        let base = Instant::now() + crate::hyperspace::FRAME_INTERVAL * 4;

        let full_started = Instant::now();
        for tick in 0..TICKS {
            let now = base + crate::hyperspace::FRAME_INTERVAL * (tick as u32 + 1);
            assert!(full_server.app.tick_sidebar_animation(now));
            for client in full_server.clients.values_mut() {
                client.render_terminal = None;
            }
            full_server.render_and_stream();
            let _ = receive_frames(&full_receivers);
        }
        let full_elapsed = full_started.elapsed();

        let partial_started = Instant::now();
        for tick in 0..TICKS {
            let now = base + crate::hyperspace::FRAME_INTERVAL * (tick as u32 + 1);
            assert!(partial_server.app.tick_sidebar_animation(now));
            assert!(partial_server.render_sidebar_animation_and_stream());
            let _ = receive_frames(&partial_receivers);
        }
        let partial_elapsed = partial_started.elapsed();

        let fresh_full_started = Instant::now();
        for _ in 0..TICKS {
            for client in full_server.clients.values_mut() {
                client.render_state.reset_baseline();
                client.render_terminal = None;
            }
            full_server.render_and_stream();
            let _ = receive_frames(&full_receivers);
        }
        let fresh_full_elapsed = fresh_full_started.elapsed();

        let reused_full_started = Instant::now();
        for _ in 0..TICKS {
            for client in partial_server.clients.values_mut() {
                client.render_state.reset_baseline();
            }
            partial_server.render_and_stream();
            let _ = receive_frames(&partial_receivers);
        }
        let reused_full_elapsed = reused_full_started.elapsed();

        let full_rate = TICKS as f64 / full_elapsed.as_secs_f64();
        let partial_rate = TICKS as f64 / partial_elapsed.as_secs_f64();
        let fresh_full_rate = TICKS as f64 / fresh_full_elapsed.as_secs_f64();
        let reused_full_rate = TICKS as f64 / reused_full_elapsed.as_secs_f64();
        let full_ui_draws = CLIENTS as usize * TICKS;
        let panel_draws = CLIENTS as usize * TICKS;
        let full_surface_cells = full_ui_draws * 120 * 40;
        let panel_surface_cells = panel_draws * 12 * 6;
        eprintln!(
            "idle_render_benchmark clients={CLIENTS} populated_panes={pane_count} simulated_ticks={TICKS} interval_ms={} full_ui_draws={full_ui_draws} partial_full_ui_draws=0 animation_panel_draws={panel_draws} full_surface_cells={full_surface_cells} animation_surface_cells={panel_surface_cells} idle_full_ticks_per_second={full_rate:.2} idle_partial_ticks_per_second={partial_rate:.2} busy_fresh_full_ticks_per_second={fresh_full_rate:.2} busy_reused_full_ticks_per_second={reused_full_rate:.2} idle_full_elapsed_ms={} idle_partial_elapsed_ms={} busy_fresh_full_elapsed_ms={} busy_reused_full_elapsed_ms={}",
            crate::hyperspace::FRAME_INTERVAL.as_millis(),
            full_elapsed.as_millis(),
            partial_elapsed.as_millis(),
            fresh_full_elapsed.as_millis(),
            reused_full_elapsed.as_millis(),
        );
    }
}
