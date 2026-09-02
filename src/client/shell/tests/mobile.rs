use super::*;

#[test]
fn navigate_update_status_uses_released_desktop_and_mobile_placement() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;
    config.hide_tab_bar_when_single_tab = false;
    let mut state = ClientShellState::new(config);
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.update_available = Some("0.8.3".into());
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Navigate;

    let bottom = state.compose(106, 30).expect("bottom-tab update shell");
    let row_text = |frame: &FrameData, row: u16| {
        let width = usize::from(frame.width);
        let start = usize::from(row) * width;
        frame.cells[start..start + width]
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>()
    };
    assert!(row_text(&bottom, 29).contains("update ready"));
    assert!(!row_text(&bottom, 28).contains("update ready"));
    assert!(state.hits.tabs.is_empty());
    assert!(state.hits.new_tab.is_empty());
    assert!(state.hits.tab_scroll_left.is_empty());
    assert!(state.hits.tab_scroll_right.is_empty());

    state.config.tab_bar_position = crate::config::TabBarPositionConfig::Top;
    state.visible_notification = Some(ClientVisibleNotification {
        event: SemanticNotification {
            kind: SemanticNotificationKind::Custom,
            title: "bottom notification".into(),
            body: None,
            sound: None,
            agent: None,
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            position: Some(crate::config::ToastHerdrPosition::BottomRight),
        },
        deadline: std::time::Instant::now(),
    });
    let top = state.compose(106, 30).expect("top-tab update shell");
    assert!(row_text(&top, 29).contains("update ready"));

    let mobile = state.compose(44, 30).expect("mobile update shell");
    let mobile_text = mobile
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(mobile_text.contains("update ready"));
}

#[test]
fn mobile_layout_reserves_only_client_header() {
    let config = ClientShellConfig::from_config(&Config::default());
    let state = ClientShellState::new(config);
    let layout = state.layout(44, 20);
    assert_eq!(layout.mobile_header, Rect::new(0, 0, 44, 2));
    assert_eq!(layout.pane_surface, Rect::new(0, 2, 44, 18));
    assert_eq!(
        state.surface_size(44, 20),
        ClientSurfaceSize { cols: 44, rows: 18 }
    );
}

#[test]
fn mobile_shell_controls_remain_clickable_when_pane_mouse_capture_is_disabled() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.mouse_capture = false;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(44, 20).expect("mobile header");
    assert!(!state.hits.mobile_switch.is_empty());
    let switch = state.hits.mobile_switch;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: switch.x,
        row: switch.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    state.compose(44, 20).expect("mobile switcher");
    assert!(!state.hits.mobile_close.is_empty());
    assert!(!state.hits.mobile_targets.is_empty());
}

#[test]
fn mobile_header_and_switcher_render_released_sections_and_stable_targets() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("pi".into()),
        display_agent: Some("pi".into()),
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: vec![("blocked".into(), "waiting".into())],
        tokens: Vec::new(),
        focused: true,
    });
    projected.workspaces[0].agent_status = AgentStatus::Blocked;
    state.set_snapshot(Box::new(projected));
    let mut projected_surface = surface();
    for cell in &mut projected_surface.frame.cells {
        cell.symbol = "X".to_owned();
    }
    state.set_pane_surface(projected_surface);

    let header = state.compose(44, 20).expect("mobile header");
    let header_text = header
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(header_text.contains("client-shell"));
    assert!(header_text.contains("tab 1"));
    assert!(header_text.contains("blocked"));
    assert!(header_text.contains("switch"));
    assert_eq!(state.hits.mobile_switch, Rect::new(34, 0, 10, 2));

    let click = |rect: Rect| {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    let opened = state.handle_raw_events(vec![click(state.hits.mobile_switch)]);
    assert!(opened.repaint);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    let switcher = state.compose(44, 20).expect("mobile switcher");
    let switcher_text = switcher
        .cells
        .chunks(switcher.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !switcher_text.contains('X'),
        "switcher must clear the pane surface"
    );
    for expected in [
        "switch",
        "close",
        "agents",
        "spaces",
        "+ new workspace",
        "tabs",
        "+ new tab",
        "menu",
        "settings",
        "detach",
    ] {
        assert!(switcher_text.contains(expected), "missing {expected}");
    }
    let workspace_hit = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(target, ClientMobileTarget::Workspace(id) if id == "ws_1").then_some(*rect)
        })
        .expect("workspace hit");
    let focused = state.handle_raw_events(vec![click(workspace_hit)]);
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(state.navigate_workspace_id.is_none());
    assert!(focused.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(params)
                    if params.workspace_id == "ws_1"
            )
    )));

    state.compose(44, 20).expect("restored mobile header");
    state.handle_raw_events(vec![click(state.hits.mobile_switch)]);
    state.compose(44, 20).expect("agent switcher");
    let agent_hit = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(target, ClientMobileTarget::Agent(id) if id == "pane_1").then_some(*rect)
        })
        .expect("agent hit");
    let focused = state.handle_raw_events(vec![click(agent_hit)]);
    assert!(focused.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(params)
                    if params.pane_id == "pane_1"
            )
    )));

    state.compose(44, 20).expect("restored mobile header");
    state.handle_raw_events(vec![click(state.hits.mobile_switch)]);
    state.compose(44, 20).expect("tab switcher");
    let tab_hit = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(target, ClientMobileTarget::Tab(id) if id == "tab_1").then_some(*rect)
        })
        .expect("tab hit");
    let focused = state.handle_raw_events(vec![click(tab_hit)]);
    assert!(focused.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::TabFocus(params)
                    if params.tab_id == "tab_1"
            )
    )));
}

#[test]
fn mobile_background_workspace_uses_its_own_active_tab_status() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut projected = snapshot();
    projected.tabs.push(ClientShellTab {
        tab_id: "tab_7".into(),
        workspace_id: "ws_1".into(),
        number: 7,
        label: "logs".into(),
        custom_label: true,
        zoomed: false,
        focused: false,
        agent_status: AgentStatus::Idle,
    });
    projected.workspaces.push(ClientShellWorkspace {
        workspace_id: "ws_2".into(),
        active_tab_id: "tab_3".into(),
        new_workspace_cwd: "/feature".into(),
        number: 2,
        label: "background".into(),
        custom_label: true,
        branch: Some("feature".into()),
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: None,
        focused: false,
        agent_status: AgentStatus::Idle,
    });
    for (number, tab_id, label) in [(1, "tab_2", "one"), (7, "tab_3", "two")] {
        projected.tabs.push(ClientShellTab {
            tab_id: tab_id.into(),
            workspace_id: "ws_2".into(),
            number,
            label: label.into(),
            custom_label: true,
            zoomed: false,
            focused: false,
            agent_status: AgentStatus::Idle,
        });
    }
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Navigate;
    state.navigate_workspace_id = Some("ws_2".into());
    let frame = state.compose(44, 20).expect("mobile switcher");
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
    assert!(text.contains("feature · tab two · 2/2"), "{text}");
    assert!(text.contains("2 · logs"), "{text}");
    assert!(!text.contains("7 · logs"), "{text}");
}

#[test]
fn mobile_switcher_create_and_menu_rows_reuse_client_actions() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.prompt_new_workspace_name = true;
    config.prompt_new_tab_name = true;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let click = |rect: Rect| {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };

    state.mode = ClientShellMode::Navigate;
    state.compose(44, 20).expect("mobile create switcher");
    let new_tab = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| matches!(target, ClientMobileTarget::NewTab).then_some(*rect))
        .expect("new tab hit");
    state.handle_raw_events(vec![click(new_tab)]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::NewTab { .. },
            ..
        }))
    ));
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert!(state.overlay.is_none());
    assert_eq!(state.mode, ClientShellMode::Terminal);

    state.mode = ClientShellMode::Navigate;
    state.compose(44, 20).expect("mobile workspace switcher");
    let new_workspace = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(target, ClientMobileTarget::NewWorkspace).then_some(*rect)
        })
        .expect("new workspace hit");
    state.handle_raw_events(vec![click(new_workspace)]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::NewWorkspace { .. },
            ..
        }))
    ));
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert!(state.overlay.is_none());
    assert_eq!(state.mode, ClientShellMode::Terminal);

    state.mode = ClientShellMode::Navigate;
    state.compose(44, 20).expect("mobile menu switcher");
    let settings = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| matches!(target, ClientMobileTarget::Menu(0)).then_some(*rect))
        .expect("settings hit");
    state.handle_raw_events(vec![click(settings)]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(_))
    ));
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert!(state.overlay.is_none());
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn mobile_menu_keeps_inert_notes_open_and_cancel_without_workspace_in_navigate() {
    let mut source_config = Config::default();
    source_config.ui.prompt_new_workspace_name = true;
    let config = ClientShellConfig::from_config(&source_config);
    let mut projected = snapshot();
    projected.latest_release_notes_available = true;
    projected.release_notes = None;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Navigate;
    state.compose(44, 20).expect("mobile switcher");
    let inert_notes = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| matches!(target, ClientMobileTarget::Menu(3)).then_some(*rect))
        .expect("what's new row");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: inert_notes.x,
        row: inert_notes.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert!(state.overlay.is_none());
    assert!(!state.mobile_switcher_suspended);

    let mut empty = snapshot();
    empty.focused_workspace_id = None;
    empty.focused_tab_id = None;
    empty.focused_pane_id = None;
    empty.workspaces.clear();
    empty.tabs.clear();
    empty.panes.clear();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&source_config));
    state.set_snapshot(Box::new(empty));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Navigate;
    state.compose(44, 20).expect("empty mobile switcher");
    let new_workspace = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(target, ClientMobileTarget::NewWorkspace).then_some(*rect)
        })
        .expect("new workspace row");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: new_workspace.x,
        row: new_workspace.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Rename(_))));
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Navigate);
}

#[test]
fn mobile_previous_workspace_action_wraps_across_expanded_entries() {
    let mut projected = snapshot();
    for index in 2..=3 {
        projected.workspaces.push(ClientShellWorkspace {
            workspace_id: format!("ws_{index}"),
            active_tab_id: format!("tab_{index}"),
            new_workspace_cwd: "/tmp".into(),
            number: index,
            label: format!("workspace-{index}"),
            custom_label: true,
            branch: None,
            git_ahead_behind: None,
            tokens: Vec::new(),
            worktree: None,
            focused: false,
            agent_status: AgentStatus::Idle,
        });
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(44, 20).expect("mobile layout");
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::PreviousWorkspace),
        &mut outcome,
    );
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_3"
            )
    )));
}

#[test]
fn mobile_switcher_scroll_close_and_width_transition_clear_mobile_hits() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut projected = snapshot();
    for index in 2..=8 {
        projected.workspaces.push(ClientShellWorkspace {
            workspace_id: format!("ws_{index}"),
            active_tab_id: format!("tab_{index}"),
            new_workspace_cwd: "/tmp".into(),
            number: index,
            label: format!("workspace-{index}"),
            custom_label: true,
            branch: None,
            git_ahead_behind: None,
            tokens: Vec::new(),
            worktree: None,
            focused: false,
            agent_status: AgentStatus::Idle,
        });
    }
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(44, 10).expect("mobile header");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: state.hits.mobile_switch.x,
        row: state.hits.mobile_switch.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(44, 10).expect("mobile switcher");
    let wheel = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 20,
        row: 8,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(wheel.repaint);
    assert_eq!(state.mobile_switcher_scroll, 2);
    state.compose(44, 10).expect("wheel position stays stable");
    assert_eq!(state.mobile_switcher_scroll, 2);
    for _ in 0..7 {
        state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Down,
            KeyModifiers::empty(),
        ))]);
    }
    state.compose(44, 10).expect("revealed mobile selection");
    assert_eq!(state.navigate_workspace_id.as_deref(), Some("ws_8"));
    assert!(state.mobile_switcher_scroll > 2);
    assert!(state.hits.mobile_targets.iter().any(|(_, target)| {
        matches!(target, ClientMobileTarget::Workspace(id) if id == "ws_8")
    }));
    let close = state.hits.mobile_close;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: close.x,
        row: close.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(state.navigate_workspace_id.is_none());

    state.compose(80, 20).expect("desktop transition");
    assert!(state.hits.mobile_switch.is_empty());
    assert!(state.hits.mobile_close.is_empty());
    assert!(state.hits.mobile_targets.is_empty());

    state.mode = ClientShellMode::Navigate;
    let short = state.compose(44, 2).expect("short mobile switcher");
    assert_eq!(short.cells[0].symbol, "─");
    assert!(state.hits.mobile_close.is_empty());
    assert!(state.hits.mobile_targets.is_empty());
}
