use super::*;

#[test]
fn tab_overflow_controls_scroll_the_client_owned_tab_bar() {
    let mut snapshot = snapshot();
    snapshot.tabs.extend((2..=8).map(|number| ClientShellTab {
        tab_id: format!("tab_{number}"),
        workspace_id: "ws_1".into(),
        number,
        label: number.to_string(),
        custom_label: false,
        zoomed: false,
        focused: false,
        agent_status: AgentStatus::Idle,
    }));
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("overflow tab bar");

    assert!(state.hits.tab_scroll_right.width > 0);
    let scroll_right = state.hits.tab_scroll_right;
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: scroll_right.x + 1,
            row: scroll_right.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outcome.repaint);
    assert_eq!(state.tab_scroll, 1);

    let mut update = state.snapshot.as_deref().expect("snapshot").clone();
    update.focused_tab_id = Some("tab_8".into());
    for tab in &mut update.tabs {
        tab.focused = tab.tab_id == "tab_8";
    }
    state.set_snapshot(Box::new(update));
    state.compose(80, 20).expect("focused overflow tab");
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));

    state.compose(300, 20).expect("tabs without overflow");
    assert_eq!(state.tab_scroll, 0);
    assert_eq!(state.hits.tabs.len(), 8);
    state.compose(80, 20).expect("focused tab after narrowing");
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));
}

#[test]
fn focused_last_overflow_tab_shows_its_full_label() {
    let mut projected = snapshot();
    let labels = [
        "1",
        "Laiza Portfolio Site",
        "linkedin posts",
        "update cv",
        "nvim",
        "brother",
        "day organiser",
        "nvim test",
    ];
    projected.tabs = labels
        .iter()
        .enumerate()
        .map(|(index, label)| ClientShellTab {
            tab_id: format!("tab_{}", index + 1),
            workspace_id: "ws_1".into(),
            number: index + 1,
            label: (*label).into(),
            custom_label: index > 0,
            zoomed: false,
            focused: index == 7,
            agent_status: AgentStatus::Idle,
        })
        .collect();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    for number in [8, 7, 8] {
        let tab_id = format!("tab_{number}");
        projected.focused_tab_id = Some(tab_id.clone());
        projected.workspaces[0].active_tab_id = tab_id.clone();
        projected.panes[0].tab_id = tab_id.clone();
        for tab in &mut projected.tabs {
            tab.focused = tab.tab_id == tab_id;
        }
        state.set_snapshot(Box::new(projected.clone()));
        state.set_pane_surface(surface());
        let frame = state
            .compose(133, 20)
            .expect("reporter's overflowing strip");
        assert_eq!(
            state.hits.new_tab.right() - state.hits.tab_scroll_left.x,
            107
        );
        let rect = state
            .hits
            .tabs
            .iter()
            .find(|(_, id)| id == &tab_id)
            .expect("focused tab")
            .0;
        let text = (rect.x..rect.right())
            .map(|x| {
                frame.cells[(rect.y * frame.width + x) as usize]
                    .symbol
                    .as_str()
            })
            .collect::<String>();
        assert!(
            text.contains(labels[number - 1]),
            "focused tab rendered as {text:?}, rect={rect:?}"
        );
    }

    // Manual scrolling must be able to reveal the rest of a partially drawn last tab.
    let end_scroll = state.tab_scroll;
    let left = state.hits.tab_scroll_left;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: left.x + 1,
        row: left.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let frame = state.compose(133, 20).expect("manual scroll left");
    assert_eq!(state.tab_scroll, end_scroll - 1);
    let right = state.hits.tab_scroll_right;
    assert_eq!(
        frame.cells[(right.y * frame.width + right.x + 1) as usize].fg,
        crate::protocol::color_to_u32(state.config.palette.overlay1),
        "right arrow stays enabled while the final tab is clipped"
    );
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: right.x + 1,
        row: right.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let frame = state.compose(133, 20).expect("manual scroll right");
    assert_eq!(state.tab_scroll, end_scroll);
    assert!(frame_rows(&frame)[0].contains("nvim test"));
    assert_eq!(
        frame.cells[(right.y * frame.width + right.x + 1) as usize].fg,
        crate::protocol::color_to_u32(state.config.palette.overlay0),
        "right arrow dims at the useful scroll limit"
    );
}

#[test]
fn focused_workspace_change_reveals_new_workspace_in_full_sidebar() {
    let mut initial = snapshot();
    let template = initial.workspaces[0].clone();
    initial.workspaces = (1..=12)
        .map(|number| ClientShellWorkspace {
            workspace_id: format!("ws_{number}"),
            number,
            label: format!("space-{number}"),
            branch: None,
            focused: number == 1,
            ..template.clone()
        })
        .collect();

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(initial));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("full sidebar");
    assert!(state.hits.workspace_max_scroll > 0);
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.workspace_id != "ws_12"));

    let mut update = state.snapshot.as_deref().expect("snapshot").clone();
    update.revision = 2;
    update.focused_workspace_id = Some("ws_12".into());
    for workspace in &mut update.workspaces {
        workspace.focused = workspace.workspace_id == "ws_12";
    }
    let mut updated_surface = surface();
    updated_surface.projection_revision = 2;
    state.set_snapshot(Box::new(update));
    state.set_pane_surface(updated_surface);
    state.compose(106, 2).expect("zero-height workspace body");
    assert!(state.reveal_focused_workspace);
    state.compose(106, 20).expect("updated full sidebar");

    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.workspace_id == "ws_12"));
}

#[test]
fn client_owned_sidebar_dividers_resize_live() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("expanded sidebar");
    assert!(state.hits.machines.is_empty());
    let workspace_body = state.hits.workspace_body;
    let needless_scroll =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: workspace_body.x,
            row: workspace_body.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert_eq!(state.hits.workspace_max_scroll, 0);
    assert_eq!(state.workspace_scroll, 0);
    assert!(!needless_scroll.repaint);
    let width_divider = state.hits.sidebar_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: width_divider.x,
        row: width_divider.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    let resize =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 31,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert_eq!(state.sidebar_width, 32);
    assert!(state.sidebar_width_manual);
    assert!(resize.repaint);
    assert!(resize.resize);
    let waiting_frame = state.compose(106, 30).expect("waiting for resized surface");
    let waiting_text: String = waiting_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(
        waiting_text.contains(" spaces"),
        "local sidebar must keep spaces while resizing: {waiting_text}"
    );
    assert!(!waiting_text.contains(" machines"));
    assert!(!waiting_text.contains("Select a connected machine"));
    assert!(!waiting_text.contains("LIVE"));
    assert!(waiting_frame.cursor.is_none());
    assert!(state.pane_surface.is_none());
    assert!(state.hits.panes.is_empty());
    assert!(state.hits.pane_splits.is_empty());
    assert!(state.hits.machines.is_empty());
    assert_eq!(state.hits.sidebar_divider.x, 31);
    assert_eq!(state.hits.workspaces[0].workspace_id, "ws_1");

    let next_resize =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 32,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(next_resize.resize);
    state.compose(106, 30).expect("continued resize");
    assert_eq!(state.hits.sidebar_divider.x, 32);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 32,
        row: width_divider.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.chrome_drag.is_none());

    state.set_pane_surface(surface());
    let recovered_frame = state.compose(106, 30).expect("resized sidebar");
    let recovered_text: String = recovered_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(recovered_text.contains(" spaces"));
    assert!(recovered_text.contains("LIVE"));
    assert!(!state.hits.panes.is_empty());
    let section_divider = state.hits.sidebar_section_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: section_divider.x + 2,
        row: section_divider.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let split = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: section_divider.x + 2,
        row: 20,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.sidebar_section_split > 0.6);
    assert!(split.repaint);
    assert!(!split.resize);
}

#[test]
fn context_menus_capture_stable_targets_and_route_actions() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let workspace = state.hits.workspaces[0].rect;
    let open_workspace_menu =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: workspace.x + 2,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(open_workspace_menu.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace { ref workspace_id, .. },
            ..
        })) if workspace_id == "ws_1"
    ));
    let workspace_items = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("workspace context menu"),
    };
    assert!(workspace_items
        .iter()
        .any(|item| item.action == ClientContextMenuAction::NewWorktree));
    state.compose(106, 20).expect("workspace context menu");
    let rename = state.hits.context_menu_rows[0].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rename.x + 1,
        row: rename.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Workspace { ref workspace_id },
            ..
        })) if workspace_id == "ws_1"
    ));

    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: pane.x + 1,
        row: pane.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 20).expect("pane context menu");
    let split_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::SplitRight)
            .expect("split right item"),
        _ => panic!("pane context menu"),
    };
    let split = state.hits.context_menu_rows[split_index].0;
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: split.x + 1,
            row: split.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("pane split context action should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneSplit(params)
            if params.target_pane_id.as_deref() == Some("pane_1")
                && params.direction == crate::api::schema::SplitDirection::Right
    ));
}

#[test]
fn global_menu_opens_from_sidebar_and_routes_client_actions() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("shell frame");
    let launcher = state.hits.global_launcher;
    assert_ne!(launcher, Rect::default());

    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: launcher.x,
        row: launcher.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(open.repaint);
    let menu = state.compose(106, 30).expect("global menu");
    let text = menu
        .cells
        .chunks(menu.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("settings"));
    assert!(text.contains("keybinds"));
    assert!(text.contains("reload config"));
    assert!(text.contains("detach"));

    let keybinds = state.hits.global_menu_rows[1].0;
    let help = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: keybinds.x,
        row: keybinds.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(help.actions.is_empty());
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));

    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: 3,
    }));
    let detach = state.handle_input_bytes(b"\r");
    assert!(detach.detach);
    assert!(state.overlay.is_none());
}

#[test]
fn new_tab_overlay_owns_text_cursor_and_submits_public_api_request() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
        &mut open,
    );
    assert!(open.actions.is_empty());
    let frame = state.compose(106, 20).expect("new tab overlay");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("new tab"));
    assert!(text.contains("save"));
    let restored = frame.to_ratatui_buffer().expect("overlay frame");
    assert!(!restored
        .cell((26, 7))
        .expect("overlay title cell")
        .modifier
        .contains(Modifier::DIM));
    assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

    assert!(state.handle_input_bytes(b"logs").actions.is_empty());
    let create = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
        panic!("new tab save should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::TabCreate(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.label.as_deref() == Some("logs")
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn close_confirmation_error_becomes_client_owned_overlay_and_stable_workspace_close() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut close = ClientShellInput::default();
    // Exercise the server error fallback directly: last-pane keybindings now
    // confirm locally before issuing a request.
    state.push_endpoint_method(
        crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
            pane_id: "pane_1".into(),
        }),
        &mut close,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &close.actions[..] else {
        panic!("pane close should use endpoint API");
    };
    let request_id = request.id.clone();
    assert!(
        state
            .handle_endpoint_result(
                "boot-1",
                &request_id,
                Err(ClientShellEndpointError {
                    code: Some("confirmation_required".into()),
                    message: "confirmation required".into(),
                }),
            )
            .0
    );
    let frame = state.compose(106, 20).expect("confirmation overlay");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Close workspace?"));
    assert!(text.contains("1 pane"));

    let confirm = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
        panic!("confirmation should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceClose(params)
            if params.workspace_id == "ws_1" && !params.close_group
    ));
}

fn frame_rows(frame: &FrameData) -> Vec<String> {
    frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn top_sidebar_widget_renders_above_spaces_and_zero_rows_when_absent() {
    let mut base_state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    base_state.set_snapshot(Box::new(snapshot()));
    base_state.set_pane_surface(surface());
    let base_frame = base_state.compose(100, 24).expect("base frame");
    let base_rows = frame_rows(&base_frame);
    assert!(
        base_rows[0].starts_with(" spaces"),
        "expected spaces header at row 0 when no widgets exist, got: {:?}",
        base_rows[0]
    );

    let mut with_widget = snapshot();
    with_widget
        .sidebar_widgets
        .push(crate::protocol::ClientShellSidebarWidget {
            source: "drover".into(),
            slot: crate::protocol::ClientShellSidebarWidgetSlot::Top,
            title: "drover".into(),
            right: Some("2/5".into()),
            lines: vec![
                "● 2 working · ✓ 3 done".into(),
                "PR #42 waiting review with a very long status line".into(),
            ],
            rows: vec![],
            action: None,
        });
    let mut widget_state =
        ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    widget_state.set_snapshot(Box::new(with_widget));
    widget_state.set_pane_surface(surface());
    let widget_frame = widget_state.compose(100, 24).expect("widget frame");
    let rows = frame_rows(&widget_frame);
    let sidebar_width = usize::from(widget_state.config.sidebar_width);

    let header: String = rows[0].chars().take(sidebar_width).collect();
    assert!(header.contains("drover"), "missing title in {header:?}");
    assert!(header.contains("2/5"), "missing right text in {header:?}");
    assert!(
        header.ends_with('│'),
        "widget header must not overwrite sidebar border: {header:?}"
    );

    let spacer: String = rows[1].chars().take(sidebar_width).collect();
    assert!(
        spacer.trim_end_matches('│').trim().is_empty(),
        "expected blank spacer row below widget header, got {spacer:?}"
    );

    let line1: String = rows[2].chars().take(sidebar_width).collect();
    assert!(
        line1.contains("● 2 working"),
        "missing first widget line in {line1:?}"
    );
    assert!(line1.ends_with('│'));

    let line2: String = rows[3].chars().take(sidebar_width).collect();
    assert!(
        line2.contains("PR #42"),
        "missing second widget line in {line2:?}"
    );
    assert!(line2.ends_with('│'));

    let divider: String = rows[4].chars().take(sidebar_width).collect();
    assert!(
        divider.starts_with("───"),
        "expected subtle divider below widget, got {divider:?}"
    );

    let spaces_row: String = rows[5].chars().take(sidebar_width).collect();
    assert!(
        spaces_row.starts_with(" spaces"),
        "expected spaces header directly below widget divider, got {spaces_row:?}"
    );
}

#[test]
fn top_sidebar_widget_renders_above_machines_in_multi_endpoint_sidebar() {
    let mut local = snapshot();
    local
        .sidebar_widgets
        .push(crate::protocol::ClientShellSidebarWidget {
            source: "drover".into(),
            slot: crate::protocol::ClientShellSidebarWidgetSlot::Top,
            title: "drover".into(),
            right: Some("live".into()),
            lines: vec!["3 tasks queued".into()],
            rows: vec![],
            action: None,
        });

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let profile = crate::client::endpoint::SavedSshEndpoint {
        id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "default".into(),
        enabled: true,
    };
    let remote_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote_id, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(local));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote_id, Box::new(remote));

    let frame = state.compose(100, 24).expect("multi-endpoint frame");
    let rows = frame_rows(&frame);
    let sidebar_width = usize::from(state.config.sidebar_width);

    let header: String = rows[0].chars().take(sidebar_width).collect();
    assert!(header.contains("drover") && header.contains("live"));
    let spacer: String = rows[1].chars().take(sidebar_width).collect();
    assert!(spacer.trim_end_matches('│').trim().is_empty());
    let body: String = rows[2].chars().take(sidebar_width).collect();
    assert!(body.contains("3 tasks queued"));
    let divider: String = rows[3].chars().take(sidebar_width).collect();
    assert!(divider.starts_with("───"));
    let machines_row: String = rows[4].chars().take(sidebar_width).collect();
    assert!(
        machines_row.starts_with(" machines"),
        "expected machines header directly below widget divider, got {machines_row:?}"
    );
}

#[test]
fn structured_widget_rows_render_tones_progress_bar_and_right_alignment() {
    let mut snap = snapshot();
    snap.sidebar_widgets
        .push(crate::protocol::ClientShellSidebarWidget {
            source: "drover".into(),
            slot: crate::protocol::ClientShellSidebarWidgetSlot::Top,
            title: "compute".into(),
            right: Some("ok".into()),
            lines: vec![],
            rows: vec![
                crate::protocol::ClientShellSidebarWidgetRow {
                    left: "● 2 working".into(),
                    right: Some("3m".into()),
                    tone: crate::protocol::ClientShellSidebarWidgetTone::Ok,
                },
                crate::protocol::ClientShellSidebarWidgetRow {
                    left: "× 1 blocked".into(),
                    right: Some("err".into()),
                    tone: crate::protocol::ClientShellSidebarWidgetTone::Err,
                },
                crate::protocol::ClientShellSidebarWidgetRow {
                    left: "▓▓▓▓░░".into(),
                    right: Some("67%".into()),
                    tone: crate::protocol::ClientShellSidebarWidgetTone::Accent,
                },
            ],
            action: Some("plugin:herdr-projects:compute:split".into()),
        });

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snap));
    state.set_pane_surface(surface());
    let frame = state.compose(100, 24).expect("structured widget frame");
    let rows = frame_rows(&frame);
    let sidebar_width = usize::from(state.config.sidebar_width);

    // Row 0 = header, Row 1 = spacer, Row 2 = ok row, Row 3 = err row, Row 4 = bar row.
    let row_ok: String = rows[2].chars().take(sidebar_width).collect();
    assert!(row_ok.contains("● 2 working") && row_ok.contains("3m"));
    let row_err: String = rows[3].chars().take(sidebar_width).collect();
    assert!(row_err.contains("× 1 blocked") && row_err.contains("err"));
    let row_bar: String = rows[4].chars().take(sidebar_width).collect();
    assert!(row_bar.contains("▓▓▓▓░░") && row_bar.contains("67%"));

    // Verify leading glyph fg colors match palette tones.
    let width = frame.width as usize;
    let ok_glyph = &frame.cells[2 * width + 1];
    assert_eq!(ok_glyph.symbol, "●");
    assert_eq!(
        ok_glyph.fg,
        crate::protocol::color_to_u32(state.config.palette.green)
    );

    let err_glyph = &frame.cells[3 * width + 1];
    assert_eq!(err_glyph.symbol, "×");
    assert_eq!(
        err_glyph.fg,
        crate::protocol::color_to_u32(state.config.palette.red)
    );

    // Verify progress bar filled vs empty colors.
    let bar_filled = &frame.cells[4 * width + 1];
    let bar_empty = &frame.cells[4 * width + 5];
    assert_eq!(bar_filled.symbol, "▓");
    assert_eq!(
        bar_filled.fg,
        crate::protocol::color_to_u32(state.config.palette.accent)
    );
    assert_eq!(bar_empty.symbol, "░");
    assert_eq!(
        bar_empty.fg,
        crate::protocol::color_to_u32(state.config.palette.overlay0)
    );

    // Verify clicking the widget header dispatches PluginPaneOpen.
    let click = state.handle_input_bytes(b"\x1b[<0;3;1M");
    let [ClientShellAction::Endpoint { request, .. }] = &click.actions[..] else {
        panic!(
            "expected PluginPaneOpen endpoint action, got {:?}",
            click.actions
        );
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PluginPaneOpen(params)
            if params.plugin_id == "herdr-projects"
                && params.entrypoint == "compute"
                && params.placement == Some(crate::api::schema::PluginPanePlacement::Split)
    ));
}

#[test]
fn show_agents_false_hides_agents_and_renders_top_and_bottom_widgets() {
    let mut cfg = Config::default();
    cfg.ui.sidebar.show_agents = false;

    let mut snap = snapshot();
    snap.sidebar_widgets
        .push(crate::protocol::ClientShellSidebarWidget {
            source: "plugin:drover:01-compute".into(),
            slot: crate::protocol::ClientShellSidebarWidgetSlot::Top,
            title: "compute".into(),
            right: Some("ok".into()),
            lines: vec![],
            rows: vec![crate::protocol::ClientShellSidebarWidgetRow {
                left: "● 2 active".into(),
                right: None,
                tone: crate::protocol::ClientShellSidebarWidgetTone::Ok,
            }],
            action: None,
        });
    snap.sidebar_widgets
        .push(crate::protocol::ClientShellSidebarWidget {
            source: "plugin:drover:02-feed".into(),
            slot: crate::protocol::ClientShellSidebarWidgetSlot::Bottom,
            title: "feed".into(),
            right: None,
            lines: vec![],
            rows: vec![crate::protocol::ClientShellSidebarWidgetRow {
                left: "✓ merged #12".into(),
                right: Some("1m".into()),
                tone: crate::protocol::ClientShellSidebarWidgetTone::Ok,
            }],
            action: None,
        });
    snap.sidebar_widgets
        .push(crate::protocol::ClientShellSidebarWidget {
            source: "plugin:drover:01-sprints".into(),
            slot: crate::protocol::ClientShellSidebarWidgetSlot::Bottom,
            title: "sprints".into(),
            right: Some("3/5".into()),
            lines: vec![],
            rows: vec![crate::protocol::ClientShellSidebarWidgetRow {
                left: "▓▓▓░░".into(),
                right: Some("60%".into()),
                tone: crate::protocol::ClientShellSidebarWidgetTone::Accent,
            }],
            action: None,
        });

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&cfg));
    state.set_snapshot(Box::new(snap));
    state.set_pane_surface(surface());
    let frame = state.compose(100, 30).expect("no-agents frame");
    let rows = frame_rows(&frame);
    let sidebar_width = usize::from(state.config.sidebar_width);
    let sidebar_rows: Vec<String> = rows
        .iter()
        .map(|row| row.chars().take(sidebar_width).collect())
        .collect();

    assert!(
        !sidebar_rows.iter().any(|row| row.starts_with(" agents")),
        "stock agents header should be hidden when show_agents = false: {sidebar_rows:#?}"
    );
    let compute_idx = sidebar_rows
        .iter()
        .position(|row| row.contains("compute"))
        .expect("top compute widget");
    let spaces_idx = sidebar_rows
        .iter()
        .position(|row| row.starts_with(" spaces"))
        .expect("spaces section");
    let sprints_idx = sidebar_rows
        .iter()
        .position(|row| row.contains("sprints"))
        .expect("bottom sprints widget");
    let feed_idx = sidebar_rows
        .iter()
        .position(|row| row.contains("feed"))
        .expect("bottom feed widget");

    assert!(compute_idx < spaces_idx);
    assert!(spaces_idx < sprints_idx);
    assert!(
        sprints_idx < feed_idx,
        "bottom widgets should sort deterministically by source"
    );
    assert!(
        sidebar_rows[sprints_idx - 1].starts_with("───"),
        "expected separator above sprints widget"
    );
    assert!(
        sidebar_rows[feed_idx - 1].starts_with("───"),
        "expected separator above feed widget"
    );
}
