use super::*;

#[test]
fn mouse_hits_use_stable_workspace_tab_and_pane_ids() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let workspace_down =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(workspace_down.actions.is_empty());
    let workspace =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(workspace.requests.is_empty());
    let [ClientShellAction::Endpoint { request, .. }] = &workspace.actions[..] else {
        panic!("workspace click should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceFocus(target)
            if target.workspace_id == "ws_1"
    ));

    let pane = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 27,
        row: 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(pane.requests.is_empty());
    let [ClientShellAction::Endpoint { request, .. }] = &pane.actions[..] else {
        panic!("pane click should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));
}

#[test]
fn collapsed_workspace_jitter_remains_a_click() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.sidebar_collapsed = true;
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("collapsed sidebar");
    let workspace = state.hits.workspaces[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: workspace.x,
        row: workspace.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: workspace.x + 1,
        row: workspace.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.chrome_drag.is_none());
    assert!(state.workspace_press.is_some());
    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: workspace.x + 1,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &release.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_1"
            )
    ));
}

#[test]
fn grouped_worktrees_render_parent_branch_and_indented_child() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    let mut snapshot = snapshot();
    snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    snapshot.workspaces.push(ClientShellWorkspace {
        workspace_id: "ws_2".into(),
        active_tab_id: "tab_ws2".into(),
        new_workspace_cwd: "/repo/feature".into(),
        number: 2,
        label: "repo-feature".into(),
        custom_label: false,
        branch: Some("worktree/feature".into()),
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        }),
        focused: false,
        agent_status: AgentStatus::Idle,
    });
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
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
    assert!(text.contains("main"));
    assert!(text.contains("└─"));
    assert!(text.contains("feature"));

    let mut replacement = (**state.snapshot.as_ref().expect("snapshot")).clone();
    replacement.revision = 2;
    replacement.focused_workspace_id = Some("ws_2".into());
    replacement.workspaces[0].focused = false;
    replacement.workspaces[1].focused = true;
    replacement.workspaces[1].agent_status = AgentStatus::Blocked;
    let mut replacement_surface = surface();
    replacement_surface.projection_revision = 2;
    state.collapsed_groups.insert("repo".into());
    state.set_snapshot(Box::new(replacement));
    state.set_pane_surface(replacement_surface);
    let collapsed = state.compose(106, 20).expect("collapsed worktree group");
    let parent = state.hits.workspaces[0].rect;
    let status_cell = usize::from(parent.y) * usize::from(collapsed.width)
        + usize::from(parent.x.saturating_add(1));
    assert_eq!(
        collapsed.cells[status_cell].fg,
        crate::protocol::color_to_u32(state.config.palette.red)
    );

    let mut next = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextWorkspace),
        &mut next,
    );
    assert!(matches!(
        &next.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_1"
            )
    ));
}

#[test]
fn workspace_click_waits_for_release_and_drag_reorders_by_stable_id() {
    let mut projected = snapshot();
    for index in 2..=3 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index;
        workspace.label = format!("workspace-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("three workspaces");
    let first = state.hits.workspaces[0].rect;
    let third = state.hits.workspaces[2].rect;

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x + 2,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(down.actions.is_empty());
    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: third.x + 2,
        row: third.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(drag.repaint);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Workspace {
            ref source_workspace_id,
            target: Some((None, _)),
        }) if source_workspace_id == "ws_1"
    ));
    let frame = state.compose(106, 24).expect("workspace drop indicator");
    assert!(frame
        .cells
        .chunks(frame.width as usize)
        .any(|row| row.iter().take(20).any(|cell| cell.symbol == "─")));

    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: third.x + 2,
            row: third.bottom(),
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &release.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMove(params)
                    if params.workspace_id == "ws_1" && params.insert_index == 3
            )
    ));

    state.compose(106, 24).expect("workspaces after drag");
    let second = state.hits.workspaces[1].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: second.x + 2,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: second.x + 2,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &click.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_2"
            )
    ));
}

#[test]
fn workspace_drag_moves_parent_worktree_as_one_block_and_rejects_child() {
    let mut projected = snapshot();
    projected.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    let mut child = projected.workspaces[0].clone();
    child.workspace_id = "ws_child".into();
    child.number = 2;
    child.label = "feature".into();
    child.focused = false;
    child.worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: true,
    });
    let mut other = projected.workspaces[0].clone();
    other.workspace_id = "ws_other".into();
    other.number = 3;
    other.label = "other".into();
    other.focused = false;
    other.worktree = None;
    projected.workspaces.extend([child, other]);

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("worktree workspaces");
    assert!(state.hits.workspaces[1].indented);
    let parent = state.hits.workspaces[0].rect;
    let child = state.hits.workspaces[1].rect;
    let other = state.hits.workspaces[2].rect;

    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: parent.x + 2,
        row: parent.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: other.x + 2,
        row: other.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    let moved = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: other.x + 2,
        row: other.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &moved.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMoveBlock(params)
                    if params.workspace_ids == ["ws_1", "ws_child"]
                        && params.before_workspace_id.is_none()
            )
    ));

    state.compose(106, 24).expect("worktree child");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: child.x + 2,
        row: child.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let dragging_child =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: other.x + 2,
            row: other.bottom(),
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(dragging_child.actions.is_empty());
    assert!(state.chrome_drag.is_none());
}

#[test]
fn pane_cycle_last_and_agent_actions_resolve_to_stable_pane_ids() {
    let mut initial = snapshot();
    let mut second = initial.panes[0].clone();
    second.pane_id = "pane_2".into();
    second.focused = false;
    initial.panes.push(second);
    initial.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("first".into()),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("second".into()),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 2,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(initial.clone()));

    let mut cycle = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CyclePaneNext),
        &mut cycle,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &cycle.actions[..] else {
        panic!("pane cycle should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));

    let mut replacement = initial;
    replacement.revision = 2;
    replacement.focused_pane_id = Some("pane_2".into());
    replacement.panes[0].focused = false;
    replacement.panes[1].focused = true;
    state.set_snapshot(Box::new(replacement));
    let mut last = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::LastPane),
        &mut last,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &last.actions[..] else {
        panic!("last pane should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));

    let mut agent = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(1)),
        &mut agent,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &agent.actions[..] else {
        panic!("agent focus should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));
}

#[test]
fn agent_sidebar_honors_priority_symbols_tokens_and_stable_hits() {
    let mut projected = snapshot();
    let mut second_pane = projected.panes[0].clone();
    second_pane.pane_id = "pane_2".into();
    second_pane.focused = false;
    projected.panes.push(second_pane);
    projected.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("pi one".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: Some("first title".into()),
            terminal_title_stripped: Some("first".into()),
            agent_status: AgentStatus::Done,
            state_change_seq: 10,
            state_labels: Vec::new(),
            tokens: vec![("summary".into(), "review complete".into())],
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("pi two".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: Some("second title".into()),
            terminal_title_stripped: Some("second".into()),
            agent_status: AgentStatus::Blocked,
            state_change_seq: 20,
            state_labels: vec![("blocked".into(), "needs input".into())],
            tokens: vec![("summary".into(), "waiting for Can".into())],
            focused: false,
        },
    ];
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    config.ui.status_indicators = crate::config::StatusIndicatorStyle::Symbols;
    config.ui.sidebar.agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
    config.ui.sidebar.agents.rows_by_agent.insert(
        "pi".into(),
        vec![
            vec![
                crate::config::AgentSidebarToken::StateIcon,
                crate::config::AgentSidebarToken::StateText,
            ],
            vec![
                crate::config::AgentSidebarToken::Agent,
                crate::config::AgentSidebarToken::Custom("summary".into()),
            ],
        ],
    );
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());

    let frame = state.compose(106, 30).expect("agent sidebar frame");
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
    assert!(text.contains("× needs input"), "frame: {text}");
    assert!(text.contains("pi two"), "frame: {text}");
    assert!(text.contains("waiting for"), "frame: {text}");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_2")
    );

    let first = state.hits.agents[0].0;
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &click.actions[..] else {
        panic!("agent row should focus through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));

    state.compose(106, 10).expect("short agent sidebar frame");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_2")
    );
    let body = state.hits.agent_body;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: body.x,
        row: body.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state
        .compose(106, 10)
        .expect("scrolled agent sidebar frame");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_1")
    );

    state.sidebar_collapsed = true;
    let compact = state.compose(106, 30).expect("compact agent sidebar frame");
    let blocked = state
        .hits
        .agents
        .iter()
        .find(|(_, pane_id)| pane_id == "pane_2")
        .expect("blocked compact agent")
        .0;
    let row_start = blocked.y as usize * compact.width as usize + blocked.x as usize;
    assert_ne!(compact.cells[row_start].fg, compact.cells[row_start + 2].fg);
    assert_eq!(compact.cells[row_start].bg, compact.cells[row_start + 2].bg);
}

#[test]
fn active_agent_view_controls_sidebar_order_and_focus_indices() {
    let mut projected = snapshot();
    let mut second_pane = projected.panes[0].clone();
    second_pane.pane_id = "pane_2".into();
    second_pane.focused = false;
    projected.panes.push(second_pane.clone());
    let mut third_pane = second_pane;
    third_pane.pane_id = "pane_3".into();
    projected.panes.push(third_pane);
    projected.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("first".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("second".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Blocked,
            state_change_seq: 2,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
        ClientShellAgent {
            pane_id: "pane_3".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("third".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 3,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
    ];
    projected.agent_view_label = Some("review".into());
    projected.agent_order = vec!["pane_2".into(), "pane_3".into()];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("filtered agent sidebar");
    assert_eq!(
        state
            .hits
            .agents
            .iter()
            .map(|(_, pane_id)| pane_id.as_str())
            .collect::<Vec<_>>(),
        vec!["pane_2", "pane_3"]
    );
    assert_eq!(state.hits.agent_sort_toggle, Rect::default());

    let mut focus = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(0)),
        &mut focus,
    );
    assert!(matches!(
        &focus.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
            )
    ));

    let mut next = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextAgent),
        &mut next,
    );
    assert!(matches!(
        &next.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
            )
    ));
}

#[test]
fn agent_sort_toggle_is_client_local_and_persists_per_endpoint() {
    let path = std::env::temp_dir().join(format!(
        "herdr-shell-agent-sort-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("pi".into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    });
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("agent sidebar frame");
    let toggle = state.hits.agent_sort_toggle;

    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x,
        row: toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);

    assert_eq!(
        state.config.agent_panel_sort,
        crate::config::AgentPanelSortConfig::Priority
    );
    assert!(click.actions.is_empty());
    let reloaded_config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let reloaded = ClientShellState::new(reloaded_config);
    assert_eq!(
        reloaded.config.agent_panel_sort,
        crate::config::AgentPanelSortConfig::Priority
    );
    assert!(reloaded.agent_panel_sort_manual);
    std::fs::remove_file(path).expect("remove agent sort preferences");
}

#[test]
fn workspace_actions_preserve_selected_target_and_client_confirmation() {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.mode = ClientShellMode::Navigate;
    state.navigate_workspace_id = Some("ws_2".into());

    let rename = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('w'),
        KeyModifiers::SHIFT,
    ))]);
    assert!(rename.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Workspace { workspace_id },
            ..
        })) if workspace_id == "ws_2"
    ));
    assert!(state.handle_input_bytes(&[0x15]).actions.is_empty());
    assert!(state.handle_input_bytes(b"renamed").actions.is_empty());
    let save = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
        panic!("workspace rename should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceRename(params)
            if params.workspace_id == "ws_2" && params.label == "renamed"
    ));

    state.navigate_workspace_id = Some("ws_2".into());
    let mut close = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseWorkspace),
        &mut close,
    );
    assert!(close.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(ClientConfirmCloseOverlay {
            workspace_id,
            ..
        })) if workspace_id == "ws_2"
    ));
    let confirm = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
        panic!("workspace confirmation should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceClose(params)
            if params.workspace_id == "ws_2" && params.close_group
    ));
}

#[test]
fn desktop_workspace_navigation_reveals_overflowing_selection() {
    let mut projected = snapshot();
    for index in 2..=8 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index;
        workspace.label = format!("workspace-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Navigate;
    state.navigate_workspace_id = Some("ws_1".into());
    state.compose(106, 12).expect("overflowing sidebar");

    for _ in 0..6 {
        state.handle_input_bytes(b"\x1b[B");
        state.compose(106, 12).expect("revealed workspace");
        let selected = state.navigate_workspace_id.as_deref().expect("selection");
        assert!(
            state
                .hits
                .workspaces
                .iter()
                .any(|hit| hit.workspace_id == selected),
            "{selected} should remain visible"
        );
    }
}

#[test]
fn named_workspace_overlay_targets_projected_source_workspace() {
    let mut config = Config::default();
    config.ui.prompt_new_workspace_name = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorkspace),
        &mut open,
    );
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            input: value,
            target: ClientRenameTarget::NewWorkspace {
                source_workspace_id,
                ..
            },
            ..
        })) if value == "repo" && source_workspace_id.as_deref() == Some("ws_1")
    ));
    let create = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
        panic!("named workspace should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceCreate(params)
            if params.source_workspace_id.as_deref() == Some("ws_1")
                && params.cwd.as_deref() == Some("/repo")
                && params.label.is_none()
    ));
}

#[test]
fn navigate_mode_selects_workspace_locally_then_focuses_by_stable_id() {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());

    assert!(state.handle_input_bytes(&[0x02]).actions.is_empty());
    let enter_navigate = state.handle_input_bytes(b"w");
    assert!(enter_navigate.repaint);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(state.navigate_workspace_id.as_deref(), Some("ws_1"));

    let invalid = state.handle_input_bytes(b"9");
    assert!(invalid.actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(state.navigate_workspace_id.as_deref(), Some("ws_1"));

    let move_selection = state.handle_input_bytes(b"\x1b[B");
    assert!(move_selection.actions.is_empty());
    assert_eq!(state.navigate_workspace_id.as_deref(), Some("ws_2"));
    let frame = state.compose(106, 20).expect("navigate frame");
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
    assert!(text.contains("second"));
    assert!(text.contains("NAVIGATE"));

    let focus = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &focus.actions[..] else {
        panic!("selected workspace should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceFocus(target)
            if target.workspace_id == "ws_2"
    ));
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn worktree_create_previews_the_endpoint_owned_checkout_path() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeList(params)
            if params.workspace_id.as_deref() == Some("ws_1")
    ));
    let request_id = request.id.clone();
    assert!(
        state
            .handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)))
            .0
    );
    let frame = state.compose(106, 30).expect("new worktree modal");
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
    assert!(text.contains("new worktree"));
    assert!(text.contains("create and open"));
    assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

    assert!(state
        .handle_input_bytes(b"feature/client-shell")
        .actions
        .is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create))
            if create.checkout_path
                == "/tmp/herdr-worktrees/repo/feature-client-shell"
    ));
    let submit = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &submit.actions[..] else {
        panic!("worktree create should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeCreate(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.branch.as_deref() == Some("feature/client-shell")
                && params.path.is_none()
                && params.focus
    ));
}

#[test]
fn unavailable_worktree_create_does_not_wedge_the_overlay() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    state.handle_endpoint_result("boot-1", &request.id, Ok(worktree_list_result(None)));
    state.set_endpoint_methods(Some(vec!["worktree.list".into()]));
    state.handle_input_bytes(b"feature/unavailable");

    let submit = state.handle_input_bytes(b"\r");

    assert!(submit.actions.is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create)) if !create.creating
    ));
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .is_some_and(|notice| notice.key.code == "worktree.create"));
}

#[test]
fn worktree_open_filters_and_clicks_a_stable_public_entry() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open worktree should prepare through worktree.list");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)));
    let frame = state.compose(106, 30).expect("open worktree modal");
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
    assert!(text.contains("feature"));
    let row = state.hits.worktree_rows[0].0;
    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
        panic!("worktree row should open through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeOpen(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.path.as_deref() == Some("/repo-feature")
                && params.focus
    ));
}

#[test]
fn worktree_remove_escalates_dirty_failure_to_force_confirmation() {
    let mut snapshot = snapshot();
    snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo-key".into(),
        label: "repo".into(),
        is_linked_worktree: true,
    });
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::RemoveWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("remove worktree should prepare through worktree.list");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(worktree_list_result(Some("ws_1"))),
    );
    let remove = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &remove.actions[..] else {
        panic!("worktree remove should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeRemove(params)
            if params.workspace_id == "ws_1" && !params.force
    ));
    let request_id = request.id.clone();
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("dirty_worktree_requires_force".into()),
            message: "dirty worktree".into(),
        }),
    );
    let frame = state.compose(106, 30).expect("force remove modal");
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
    assert!(text.contains("delete anyway"));
    assert!(text.contains("permanently deleted"));
    let force = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &force.actions[..] else {
        panic!("forced worktree remove should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeRemove(params)
            if params.workspace_id == "ws_1" && params.force
    ));
}

#[test]
fn semantic_notifications_use_client_policy_and_stable_navigation_targets() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Herdr;
    config.toast_delay_seconds = 0;
    let mut state = ClientShellState::new(config);
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_2".into(),
        workspace_id: "ws_2".into(),
        tab_id: "tab_2".into(),
        name: None,
        display_agent: Some("codex".into()),
        agent: Some("codex".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    });
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let now = std::time::Instant::now();
    let (effects, repaint) = state.receive_notification(
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: Some("other · 2".into()),
            sound: Some(SemanticNotificationSound::Request),
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(repaint);
    assert!(matches!(
        effects.as_slice(),
        [ClientShellNotificationEffect::Sound {
            sound: crate::sound::Sound::Request,
            ..
        }]
    ));
    let frame = state.compose(100, 28).expect("notification frame");
    let rendered = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("codex needs attention"));
    let hit = state.hits.notification_toast;
    let click = || {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    state.mode = ClientShellMode::Navigate;
    let ignored = state.handle_raw_events(vec![click()]);
    assert!(ignored.actions.is_empty());
    assert!(state.visible_notification.is_some());

    state.mode = ClientShellMode::Terminal;
    let outcome = state.handle_raw_events(vec![click()]);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(params)
                    if params.pane_id == "pane_2"
            )
    )));
    assert!(state.visible_notification.is_none());

    state.receive_notification(
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    let mut keybind = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenNotificationTarget),
        &mut keybind,
    );
    assert!(keybind.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(params)
                    if params.pane_id == "pane_2"
            )
    )));
    assert!(state.visible_notification.is_none());

    state.receive_notification(
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "first".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(state.visible_notification.is_some());
    state.config.toast_delay_seconds = 1;
    let (_, repaint) = state.receive_notification(
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "replacement".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(repaint);
    assert!(state.visible_notification.is_none());
    assert_eq!(state.pending_notifications.len(), 1);
}
