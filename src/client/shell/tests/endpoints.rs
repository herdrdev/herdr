use super::*;

#[path = "workspace_navigation.rs"]
mod workspace_navigation;
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn remote_profile() -> SavedSshEndpoint {
    SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    }
}

fn agent(
    name: &str,
    status: crate::api::schema::AgentStatus,
    state_change_seq: u64,
) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some(name.into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    }
}

fn state_with_remote() -> (ClientShellState, ClientEndpointId) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let profile = remote_profile();
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.workspaces[0].label = "remote-workspace".into();
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
    (state, endpoint_id)
}

#[test]
fn switching_machines_from_copy_mode_restores_terminal_input() {
    let (mut state, remote) = state_with_remote();
    let mut local_surface = surface();
    local_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(local_surface);
    state.compose(100, 28).unwrap();
    assert!(state.enter_copy_mode(&mut ClientShellInput::default()));
    assert_eq!(state.mode, ClientShellMode::Copy);

    assert!(state.activate_endpoint_projection(&remote));
    let mut remote_surface = surface();
    remote_surface.boot_id = "remote-boot".into();
    state.set_pane_surface(remote_surface);
    state.compose(100, 28).unwrap();

    assert!(state.copy_mode.is_none());
    assert_eq!(state.mode, ClientShellMode::Terminal);
    let input = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    ))]);
    assert!(matches!(
        input.requests.as_slice(),
        [ClientMessage::ClientShellPaneInput { pane_id, events }]
            if pane_id == "pane_1" && events.len() == 1
    ));
}

#[test]
fn live_catalog_rename_preserves_snapshot_and_disable_reenable_clears_it() {
    let (mut state, remote) = state_with_remote();
    let mut profile = remote_profile();
    profile.label = "Renamed".into();
    state.set_endpoint_catalog(&[profile.clone()]);
    assert_eq!(state.endpoint_label(&remote), "Renamed");
    assert!(state.endpoint_is_online(&remote));
    assert_eq!(state.endpoint_boot_id(&remote), Some("remote-boot"));
    profile.enabled = false;
    state.set_endpoint_catalog(&[profile.clone()]);
    assert_eq!(
        state.endpoint_status(&remote),
        Some(ClientEndpointStatus::Disabled)
    );
    assert!(!state.endpoint_has_snapshot(&remote));
    profile.enabled = true;
    state.set_endpoint_catalog(&[profile]);
    assert_eq!(
        state.endpoint_status(&remote),
        Some(ClientEndpointStatus::Connecting)
    );
    assert!(!state.endpoint_has_snapshot(&remote));
}

#[test]
fn live_catalog_active_removal_does_not_retain_remote_projection_or_input() {
    let (mut state, remote) = state_with_remote();
    assert!(state.activate_endpoint_projection(&remote));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Prefix;
    state.overlay = Some(ClientShellOverlay::Onboarding);
    state.select_unavailable_local();
    state.retire_endpoint(&remote);
    state.set_endpoint_catalog(&[]);
    assert!(state.endpoint_is_active(&ClientEndpointId::Local));
    assert!(state.snapshot.is_none());
    assert!(state.pane_surface.is_none());
    assert!(state.pending_pane_surface.is_none());
    assert!(state.overlay.is_none());
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(state.endpoint_has_snapshot(&ClientEndpointId::Local));
    let frame = state.compose(100, 30).unwrap();
    let buffer = frame.to_ratatui_buffer().unwrap();
    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!text.contains("remote-workspace"));
}

#[test]
fn machine_navigation_does_not_require_a_local_snapshot_or_surface() {
    for (cols, rows) in [(100, 28), (36, 18)] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let profile = remote_profile();
        let remote = ClientEndpointId::Ssh(profile.id.clone());
        state.set_endpoint_catalog(&[profile]);
        state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
        state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
        state.set_endpoint_snapshot(&remote, Box::new(snapshot()));
        assert!(state.snapshot.is_none());
        assert!(state.pane_surface.is_none());
        let frame = state
            .compose(cols, rows)
            .expect("connection chrome without Local");
        let local = state
            .hits
            .machines
            .iter()
            .find(|hit| hit.endpoint_id.is_local())
            .unwrap()
            .rect;
        let buffer = frame.to_ratatui_buffer().unwrap();
        let local_row = (local.x..local.right())
            .map(|x| buffer[(x, local.y)].symbol())
            .collect::<String>();
        assert!(!local_row.contains("reconnecting"));
        assert!(
            !local_row.contains('◐'),
            "Local never gets a connection badge"
        );
        let hit = state
            .hits
            .machines
            .iter()
            .find(|hit| hit.endpoint_id == remote)
            .unwrap()
            .rect;
        let mut outcome = ClientShellInput::default();
        state.handle_mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x + 5,
                row: hit.y,
                modifiers: KeyModifiers::NONE,
            },
            &mut outcome,
        );
        assert!(
            matches!(outcome.actions.as_slice(), [ClientShellAction::ActivateEndpoint { endpoint_id, .. }] if endpoint_id == &remote)
        );
        assert!(
            state.snapshot.is_none(),
            "selection is committed only by coherent activation"
        );
    }
}

#[test]
fn sidebar_renders_local_and_saved_ssh_endpoints_with_status() {
    let (mut state, _) = state_with_remote();
    let frame = state.compose(100, 28).expect("combined endpoint frame");
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
    assert!(text.contains("Local"));
    assert!(text.contains("Build"));
    assert!(text.contains("remote-workspace"));
    let local = state
        .hits
        .machines
        .iter()
        .find(|hit| hit.endpoint_id.is_local())
        .expect("local machine row")
        .rect;
    let remote = state
        .hits
        .machines
        .iter()
        .find(|hit| !hit.endpoint_id.is_local())
        .expect("remote machine row")
        .rect;
    let buffer = frame.to_ratatui_buffer().expect("frame should reconstruct");
    assert_ne!(buffer[(local.right() - 1, local.y)].symbol(), "●");
    assert_eq!(buffer[(remote.right() - 1, remote.y)].symbol(), "●");
    assert_eq!(
        buffer[(remote.right() - 1, remote.y)].fg,
        state.config.palette.green
    );

    state.sidebar_collapsed = true;
    let frame = state.compose(100, 28).expect("collapsed endpoint frame");
    let local = state
        .hits
        .machines
        .iter()
        .find(|hit| hit.endpoint_id.is_local())
        .expect("collapsed local machine row")
        .rect;
    let remote = state
        .hits
        .machines
        .iter()
        .find(|hit| !hit.endpoint_id.is_local())
        .expect("collapsed remote machine row")
        .rect;
    let buffer = frame.to_ratatui_buffer().expect("frame should reconstruct");
    assert_ne!(buffer[(local.right() - 1, local.y)].symbol(), "●");
    assert_eq!(buffer[(remote.right() - 1, remote.y)].symbol(), "●");
}

#[test]
fn saved_machine_preserves_endpoint_scoped_worktree_collapses() {
    fn add_worktree_group(snapshot: &mut ClientShellSnapshot, parent_id: &str, child_id: &str) {
        snapshot.workspaces[0].workspace_id = parent_id.into();
        snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        });
        let mut child = snapshot.workspaces[0].clone();
        child.workspace_id = child_id.into();
        child.active_tab_id = format!("tab_{child_id}");
        child.number = 2;
        child.label = "feature".into();
        child.focused = false;
        child.agent_status = AgentStatus::Blocked;
        child.worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        });
        snapshot.workspaces.push(child);
    }

    let (mut state, remote_id) = state_with_remote();
    let mut local = snapshot();
    add_worktree_group(&mut local, "ws_1", "ws_2");
    state.set_snapshot(Box::new(local));
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.focused_workspace_id = Some("remote_ws_1".into());
    add_worktree_group(&mut remote, "remote_ws_1", "remote_ws_2");
    state.set_endpoint_snapshot(&remote_id, Box::new(remote));

    state.open_workspace_context_menu("ws_1".into(), 0, 0);
    let toggle_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::ToggleGroup)
            .expect("collapse menu item"),
        _ => panic!("workspace context menu"),
    };
    state.activate_context_menu_item(toggle_index, &mut ClientShellInput::default());

    let frame = state
        .compose(100, 28)
        .expect("collapsed local worktree group");
    assert!(!state
        .hits
        .workspaces
        .iter()
        .any(|hit| { hit.endpoint_id == ClientEndpointId::Local && hit.workspace_id == "ws_2" }));
    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.endpoint_id == remote_id && hit.workspace_id == "remote_ws_2"));
    let local_parent = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == ClientEndpointId::Local && hit.workspace_id == "ws_1")
        .expect("local parent workspace");
    let (local_toggle, key) = local_parent
        .group_toggle
        .as_ref()
        .expect("local worktree group marker");
    assert_eq!(key, "repo");
    let buffer = frame.to_ratatui_buffer().expect("frame should reconstruct");
    assert_eq!(buffer[(local_toggle.x, local_toggle.y)].symbol(), "▸");
    assert!((local_parent.rect.x..local_parent.rect.right())
        .any(|x| buffer[(x, local_parent.rect.y)].fg == state.config.palette.red));

    assert!(state.activate_endpoint_projection(&remote_id));
    let mut remote_surface = surface();
    remote_surface.boot_id = "remote-boot".into();
    state.set_pane_surface(remote_surface);
    state
        .compose(100, 28)
        .expect("active remote worktree group");
    let mut switch = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::SwitchWorkspace(1)),
        &mut switch,
    );
    assert!(matches!(
        &switch.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "remote_ws_2"
            )
    ));
    let remote_parent = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == remote_id && hit.workspace_id == "remote_ws_1")
        .expect("visible remote worktree parent")
        .rect;
    let remote_child = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == remote_id && hit.workspace_id == "remote_ws_2")
        .expect("visible remote worktree child")
        .rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: remote_parent.x + 3,
        row: remote_parent.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: remote_child.x + 3,
        row: remote_child.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Workspace {
            target: Some(_),
            ..
        })
    ));
    state.chrome_drag = None;
    state.workspace_press = None;

    let remote_toggle = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == remote_id && hit.workspace_id == "remote_ws_1")
        .and_then(|hit| hit.group_toggle.as_ref())
        .expect("remote worktree group marker")
        .0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: remote_toggle.x,
        row: remote_toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(100, 28).expect("both groups collapsed");
    assert!(!state
        .hits
        .workspaces
        .iter()
        .any(|hit| { hit.endpoint_id == remote_id && hit.workspace_id == "remote_ws_2" }));

    let local_toggle = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == ClientEndpointId::Local && hit.workspace_id == "ws_1")
        .and_then(|hit| hit.group_toggle.as_ref())
        .expect("collapsed local group marker")
        .0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: local_toggle.x,
        row: local_toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(100, 28).expect("only remote group collapsed");
    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| { hit.endpoint_id == ClientEndpointId::Local && hit.workspace_id == "ws_2" }));
    assert!(!state
        .hits
        .workspaces
        .iter()
        .any(|hit| { hit.endpoint_id == remote_id && hit.workspace_id == "remote_ws_2" }));
}

#[test]
fn active_workspace_is_the_only_highlight_when_machine_is_expanded() {
    let (mut state, endpoint_id) = state_with_remote();
    assert!(state.activate_endpoint_projection(&endpoint_id));
    let mut remote_surface = surface();
    remote_surface.boot_id = "remote-boot".into();
    state.set_pane_surface(remote_surface);

    let frame = state.compose(100, 28).expect("combined endpoint frame");
    let machine = state
        .hits
        .machines
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote machine hit")
        .rect;
    let workspace = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote workspace hit")
        .rect;
    let buffer = frame.to_ratatui_buffer().expect("frame should reconstruct");
    assert_ne!(
        buffer[(machine.x, machine.y)].bg,
        state.config.palette.active_row_bg
    );
    assert_eq!(
        buffer[(workspace.x + 2, workspace.y)].bg,
        state.config.palette.active_row_bg
    );

    state.collapsed_endpoints.insert(endpoint_id.clone());
    let frame = state.compose(100, 28).expect("collapsed endpoint frame");
    let machine = state
        .hits
        .machines
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote machine hit")
        .rect;
    let buffer = frame.to_ratatui_buffer().expect("frame should reconstruct");
    assert_eq!(
        buffer[(machine.x, machine.y)].bg,
        state.config.palette.active_row_bg
    );
}

#[test]
fn aggregate_agents_use_configured_rows_machine_token_and_status_colors() {
    use crate::api::schema::AgentStatus;
    use crate::config::{AgentSidebarToken, StatusIndicatorStyle};

    let mut config = Config::default();
    config.ui.status_indicators = StatusIndicatorStyle::Symbols;
    config.ui.sidebar.agents.rows = vec![vec![
        AgentSidebarToken::StateIcon,
        AgentSidebarToken::Machine,
        AgentSidebarToken::Agent,
    ]];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let profile = remote_profile();
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);

    let mut local = snapshot();
    local.agents = vec![agent("local agent", AgentStatus::Idle, 1)];
    state.set_snapshot(Box::new(local));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.agents = vec![agent("remote agent", AgentStatus::Blocked, 1)];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));

    let frame = state.compose(100, 28).expect("combined endpoint frame");
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
    assert!(text.contains("○ Local · local agent"), "frame: {text}");
    assert!(text.contains("× Build · remote agent"), "frame: {text}");
    assert!(text.contains("grouped"), "frame: {text}");
    let toggle = state.hits.agent_sort_toggle;
    assert!(!toggle.is_empty());
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
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

    let buffer = frame
        .to_ratatui_buffer()
        .expect("aggregate frame should reconstruct");
    assert!(buffer
        .content()
        .iter()
        .any(|cell| cell.symbol() == "×" && cell.fg == state.config.palette.red));
}

#[test]
fn aggregate_priority_uses_client_observed_recency_across_machines() {
    use crate::api::schema::AgentStatus;
    use crate::config::AgentSidebarToken;

    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    config.ui.sidebar.agents.rows =
        vec![vec![AgentSidebarToken::Machine, AgentSidebarToken::Agent]];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let profile = remote_profile();
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);

    let mut local = snapshot();
    local.agents = vec![agent("local agent", AgentStatus::Idle, 1)];
    state.set_snapshot(Box::new(local));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.agents = vec![agent("remote agent", AgentStatus::Idle, 1)];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote.clone()));

    let mut local = snapshot();
    local.agents = vec![agent("local agent", AgentStatus::Idle, 2)];
    state.set_snapshot(Box::new(local));
    let frame_text = |state: &mut ClientShellState| {
        let frame = state.compose(100, 28).expect("combined endpoint frame");
        frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = frame_text(&mut state);
    assert!(
        text.find("Local · local agent").expect("local agent")
            < text.find("Build · remote agent").expect("remote agent")
    );

    remote.agents = vec![agent("remote agent", AgentStatus::Working, 2)];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote.clone()));
    let text = frame_text(&mut state);
    assert!(
        text.find("Build · remote agent").expect("remote agent")
            < text.find("Local · local agent").expect("local agent")
    );

    remote.agents = vec![agent("remote agent", AgentStatus::Idle, 3)];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
    let text = frame_text(&mut state);
    assert!(
        text.find("Build · remote agent").expect("remote agent")
            < text.find("Local · local agent").expect("local agent")
    );
    let mut outcome = ClientShellInput::default();
    assert!(
        state.handle_endpoint_navigation(crate::input::KeybindAction::FocusAgent(0), &mut outcome,)
    );
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: activated,
            target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
        }] if activated == &endpoint_id && pane_id == "pane_1"
    ));
}

#[test]
fn unselected_endpoint_completion_projects_done_client_side() {
    use crate::api::schema::AgentStatus;

    let (mut state, endpoint_id) = state_with_remote();
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.agents = vec![agent("background agent", AgentStatus::Working, 2)];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote.clone()));
    remote.revision = 2;
    remote.agents = vec![agent("background agent", AgentStatus::Idle, 3)];

    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));

    let status = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .and_then(|endpoint| endpoint.snapshot.as_deref())
        .and_then(|snapshot| snapshot.agents.first())
        .map(|agent| agent.agent_status);
    assert_eq!(status, Some(AgentStatus::Done));
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
}

#[test]
fn clicking_remote_machine_name_requests_activation_without_mutating_projection() {
    let (mut state, endpoint_id) = state_with_remote();
    state.compose(100, 28).expect("combined endpoint frame");
    let hit = state
        .hits
        .machines
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote endpoint hit")
        .rect;
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.x + 3,
        row: hit.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: activated,
            target: None,
        }] if activated == &endpoint_id
    ));
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
    assert_eq!(
        state
            .snapshot
            .as_deref()
            .map(|snapshot| snapshot.boot_id.as_str()),
        Some("boot-1")
    );
}

#[test]
fn machine_arrow_toggles_inactive_machine_without_switching() {
    for sidebar_collapsed in [false, true] {
        for status in [
            ClientEndpointStatus::Online,
            ClientEndpointStatus::Reconnecting,
        ] {
            let (mut state, remote_id) = state_with_remote();
            let mut other_profile = remote_profile();
            other_profile.id = ProfileId::parse("1123456789abcdef0123456789abcdef").unwrap();
            let other_id = ClientEndpointId::Ssh(other_profile.id.clone());
            state.set_endpoint_catalog(&[remote_profile(), other_profile]);
            state.set_endpoint_status(&other_id, ClientEndpointStatus::Online);
            state.set_endpoint_snapshot(&other_id, Box::new(snapshot()));
            state.set_endpoint_status(&remote_id, status);
            state.sidebar_collapsed = sidebar_collapsed;

            for collapsed in [true, false] {
                let frame = state.compose(100, 28).expect("three machine frame");
                let machine = state
                    .hits
                    .machines
                    .iter()
                    .find(|hit| hit.endpoint_id == remote_id)
                    .expect("remote machine")
                    .rect;
                let column = machine.x + u16::from(!sidebar_collapsed);
                let buffer = frame.to_ratatui_buffer().expect("frame buffer");
                assert_eq!(
                    buffer[(column, machine.y)].symbol(),
                    if collapsed { "▾" } else { "▸" }
                );
                let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row: machine.y,
                    modifiers: KeyModifiers::empty(),
                })]);
                assert!(
                    outcome.actions.is_empty(),
                    "collapse must not switch machines"
                );
                assert!(outcome.requests.is_empty());
                assert!(outcome.repaint);
                assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
                assert_eq!(state.snapshot.as_ref().unwrap().boot_id, "boot-1");
                assert_eq!(
                    state
                        .snapshot
                        .as_ref()
                        .unwrap()
                        .focused_workspace_id
                        .as_deref(),
                    Some("ws_1")
                );
                assert_eq!(state.collapsed_endpoints.contains(&remote_id), collapsed);
                assert!(!state.collapsed_endpoints.contains(&ClientEndpointId::Local));
                assert!(!state.collapsed_endpoints.contains(&other_id));
                assert!(state.endpoint_error.is_none());

                state.compose(100, 28).expect("toggled machine frame");
                assert_eq!(
                    state
                        .hits
                        .workspaces
                        .iter()
                        .any(|hit| hit.endpoint_id == remote_id),
                    !collapsed
                );
                for endpoint_id in [&ClientEndpointId::Local, &other_id] {
                    assert!(state
                        .hits
                        .workspaces
                        .iter()
                        .any(|hit| &hit.endpoint_id == endpoint_id));
                }
            }
        }
    }
}

#[test]
fn context_menu_lookup_ignores_inactive_endpoint_workspaces() {
    let (mut state, endpoint_id) = state_with_remote();
    state.compose(100, 28).expect("combined endpoint frame");
    let remote = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote workspace")
        .rect;
    let local = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id.is_local())
        .expect("local workspace")
        .rect;

    assert_eq!(
        state.active_endpoint_workspace_at((remote.x, remote.y)),
        None
    );
    assert_eq!(
        state.active_endpoint_workspace_at((local.x, local.y)),
        Some("ws_1".into())
    );
}

#[test]
fn future_surface_waits_for_its_exact_snapshot_revision() {
    let (mut state, _) = state_with_remote();
    let mut future = surface();
    future.projection_revision = 2;
    future.surface_revision = 2;
    state.set_pane_surface(future);
    assert_eq!(
        state
            .pane_surface
            .as_ref()
            .map(|surface| surface.projection_revision),
        Some(1)
    );
    assert_eq!(
        state
            .pending_pane_surface
            .as_ref()
            .map(|surface| surface.projection_revision),
        Some(2)
    );

    let mut next = snapshot();
    next.revision = 2;
    state.set_snapshot(Box::new(next));
    assert_eq!(
        state
            .pane_surface
            .as_ref()
            .map(|surface| surface.projection_revision),
        Some(2)
    );
    assert!(state.pending_pane_surface.is_none());
}

#[test]
fn inactive_endpoint_snapshot_cache_never_regresses_revision() {
    let (mut state, endpoint_id) = state_with_remote();
    let mut newest = snapshot();
    newest.boot_id = "remote-boot".into();
    newest.revision = 3;
    newest.workspaces[0].label = "newest".into();
    state.set_endpoint_snapshot(&endpoint_id, Box::new(newest));
    let mut delayed = snapshot();
    delayed.boot_id = "remote-boot".into();
    delayed.revision = 2;
    delayed.workspaces[0].label = "delayed".into();

    state.set_endpoint_snapshot(&endpoint_id, Box::new(delayed));

    let label = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .and_then(|endpoint| endpoint.snapshot.as_deref())
        .and_then(|snapshot| snapshot.workspaces.first())
        .map(|workspace| workspace.label.as_str());
    assert_eq!(label, Some("newest"));
}

#[test]
fn new_connection_generation_accepts_a_lower_same_boot_projection_revision() {
    let (mut state, endpoint_id) = state_with_remote();
    let mut previous = snapshot();
    previous.boot_id = "shared-server-boot".into();
    previous.revision = 9;
    previous.workspaces[0].label = "old connection".into();
    state.cache_endpoint_snapshot_inactive_for_generation(&endpoint_id, 4, Box::new(previous));
    let mut reconnected = snapshot();
    reconnected.boot_id = "shared-server-boot".into();
    reconnected.revision = 1;
    reconnected.workspaces[0].label = "new connection".into();

    state.cache_endpoint_snapshot_inactive_for_generation(&endpoint_id, 5, Box::new(reconnected));

    let endpoint = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .expect("remote endpoint");
    assert_eq!(endpoint.snapshot_generation, Some(5));
    assert_eq!(endpoint.snapshot.as_ref().unwrap().revision, 1);
    assert_eq!(
        endpoint.snapshot.as_ref().unwrap().workspaces[0].label,
        "new connection"
    );
}

#[test]
fn reconnect_snapshot_waits_for_coherent_activation_before_replacing_projection() {
    let (mut state, endpoint_id) = state_with_remote();
    assert!(state.activate_endpoint_projection(&endpoint_id));
    assert_eq!(state.snapshot.as_deref().unwrap().boot_id, "remote-boot");

    state.mark_endpoint_disconnected(&endpoint_id);
    let mut replacement = snapshot();
    replacement.boot_id = "replacement-boot".into();
    state.cache_endpoint_snapshot(&endpoint_id, Box::new(replacement));
    assert_eq!(state.snapshot.as_deref().unwrap().boot_id, "remote-boot");

    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);
    assert!(state.activate_endpoint_projection(&endpoint_id));
    assert_eq!(
        state.snapshot.as_deref().unwrap().boot_id,
        "replacement-boot"
    );
}

#[test]
fn disconnected_active_endpoint_freezes_surface_and_marks_cached_ui_stale() {
    use crate::api::schema::AgentStatus;
    use crate::config::{AgentSidebarToken, StatusIndicatorStyle};

    let (mut state, endpoint_id) = state_with_remote();
    state.config.status_indicators = StatusIndicatorStyle::Symbols;
    state.config.agents.rows = vec![vec![
        AgentSidebarToken::StateIcon,
        AgentSidebarToken::Machine,
        AgentSidebarToken::Agent,
    ]];
    let endpoint = state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .expect("remote endpoint");
    endpoint.snapshot.as_mut().expect("remote snapshot").agents =
        vec![agent("remote agent", AgentStatus::Blocked, 1)];
    assert!(state.activate_endpoint_projection(&endpoint_id));
    let mut remote_surface = surface();
    remote_surface.boot_id = "remote-boot".into();
    state.set_pane_surface(remote_surface);
    state.pending_integration_installs = 2;

    state.mark_endpoint_disconnected(&endpoint_id);
    let frame = state.compose(100, 28).expect("frozen endpoint frame");
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

    assert_eq!(state.pending_integration_installs, 0);
    assert_eq!(
        state.endpoint_status(&endpoint_id),
        Some(ClientEndpointStatus::Reconnecting)
    );
    assert!(text.contains("◐ reconnecting"), "frame: {text}");
    assert!(text.contains("Build · remote agent"), "frame: {text}");
    assert!(
        text.contains("LIVE"),
        "frozen surface should remain: {text}"
    );
    assert!(state.hits.panes.is_empty());
    assert!(frame.cursor.is_none());
    let buffer = frame.to_ratatui_buffer().expect("frame should reconstruct");
    let stale_icon = buffer
        .content()
        .iter()
        .find(|cell| cell.symbol() == "×")
        .expect("stale blocked icon");
    assert_eq!(stale_icon.fg, state.config.palette.overlay0);
}

#[cfg(unix)]
#[test]
fn graphics_scope_qualifies_colliding_boot_ids_by_endpoint() {
    let (mut state, endpoint_id) = state_with_remote();
    let local_scope = state.graphics_scope().to_owned();
    let mut remote = snapshot();
    remote.boot_id = "boot-1".into();
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
    assert!(state.activate_endpoint_projection(&endpoint_id));
    let remote_scope = state.graphics_scope();
    assert_ne!(local_scope, remote_scope);
    assert!(remote_scope.starts_with("ssh:0123456789abcdef0123456789abcdef:"));
}

#[cfg(unix)]
#[test]
fn local_direct_graphics_accept_server_ids_across_endpoint_switches_and_restarts() {
    use crate::kitty_graphics::surface::{direct_upload_control, host_image_id};
    use crate::protocol::{SurfaceGraphicsAssetKey, SurfaceGraphicsFormat, SurfaceGraphicsSource};

    let (mut state, remote_id) = state_with_remote();
    let asset = SurfaceGraphicsAssetKey {
        source: SurfaceGraphicsSource::PaneLayer {
            pane_id: "pane_1".into(),
            layer_id: "primary".into(),
        },
        image_width: 2,
        image_height: 2,
        format: SurfaceGraphicsFormat::Rgba,
        data_len: 16,
        data_fingerprint: 17,
    };
    let (server_image_id, _) = direct_upload_control("boot-1", &asset);
    assert_eq!(
        host_image_id(state.graphics_scope(), &asset),
        server_image_id
    );
    assert!(state.trust_direct_graphics_asset(&asset, server_image_id));
    assert!(!state.trust_direct_graphics_asset(&asset, server_image_id + 1));

    let mut remote = snapshot();
    remote.boot_id = "boot-1".into();
    state.set_endpoint_snapshot(&remote_id, Box::new(remote));
    assert!(state.activate_endpoint_projection(&remote_id));
    assert!(!state.trust_direct_graphics_asset(&asset, server_image_id));
    assert!(state.activate_endpoint_projection(&ClientEndpointId::Local));
    assert!(state.trust_direct_graphics_asset(&asset, server_image_id));

    let mut restarted = snapshot();
    restarted.boot_id = "replacement-boot".into();
    state.set_snapshot(Box::new(restarted));
    let (restarted_image_id, _) = direct_upload_control("replacement-boot", &asset);
    assert!(!state.trust_direct_graphics_asset(&asset, server_image_id));
    assert_eq!(
        host_image_id(state.graphics_scope(), &asset),
        restarted_image_id
    );
    assert!(state.trust_direct_graphics_asset(&asset, restarted_image_id));
}

#[test]
fn navigator_uses_machine_parents_only_for_federated_clients() {
    let (mut state, _) = state_with_remote();
    state.open_navigator_overlay();
    let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
    else {
        panic!("expected navigator");
    };
    let rows =
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
    let machines = rows
        .iter()
        .filter(|row| matches!(row.target, ClientNavigatorTarget::Machine { .. }))
        .collect::<Vec<_>>();
    assert_eq!(
        machines
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Local", "Build"]
    );
    assert!(rows.iter().all(|row| {
        matches!(row.target, ClientNavigatorTarget::Machine { .. })
            || (!row.label.contains("Local ·") && !row.label.contains("Build ·"))
    }));
    assert!(rows.iter().all(|row| match row.target {
        ClientNavigatorTarget::Machine { .. } => row.depth == 0 && row.status.is_none(),
        ClientNavigatorTarget::Workspace { .. } => row.depth == 1 && row.status.is_none(),
        ClientNavigatorTarget::Tab { .. } => row.depth == 2 && row.status.is_none(),
        ClientNavigatorTarget::Pane { .. } => row.depth == 3 && row.status.is_some(),
    }));
    assert_eq!(rows.iter().filter(|row| row.current).count(), 1);

    let frame = state.compose(106, 30).expect("federated navigator");
    for (rect, target) in &state.hits.navigator_rows {
        let expected = match target {
            ClientNavigatorTarget::Machine { .. } => " ▾ ",
            ClientNavigatorTarget::Workspace { .. } => "   ▾ ",
            ClientNavigatorTarget::Tab { .. } => "     └── ",
            ClientNavigatorTarget::Pane { .. } => "        └── ",
        };
        let prefix = frame.cells[rect.y as usize * frame.width as usize + rect.x as usize..]
            .iter()
            .take(expected.chars().count())
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert_eq!(prefix, expected, "{target:?}");
    }

    let mut local = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    local.set_snapshot(Box::new(snapshot()));
    local.set_pane_surface(surface());
    let frame = local.compose(100, 28).expect("local-only sidebar");
    assert!(local.hits.machines.is_empty());
    assert!(!frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .contains(" machines"));
    local.open_navigator_overlay();
    let ClientShellOverlay::Navigator(navigator) = local.overlay.as_ref().expect("navigator")
    else {
        panic!("expected navigator");
    };
    let rows =
        render::client_navigator_rows(&local.endpoints, &local.active_endpoint_id, navigator);
    assert!(rows
        .iter()
        .all(|row| !matches!(row.target, ClientNavigatorTarget::Machine { .. })));
    assert!(rows.iter().all(|row| match row.target {
        ClientNavigatorTarget::Workspace { .. } => row.depth == 0,
        ClientNavigatorTarget::Tab { .. } => row.depth == 1,
        ClientNavigatorTarget::Pane { .. } => row.depth == 2,
        ClientNavigatorTarget::Machine { .. } => false,
    }));
}

#[test]
fn navigator_keeps_saved_machine_visible_before_metadata_arrives() {
    let (mut state, endpoint_id) = state_with_remote();
    let endpoint = state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .expect("saved remote endpoint");
    endpoint.snapshot = None;
    endpoint.status = ClientEndpointStatus::Connecting;
    state.open_navigator_overlay();
    let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
    else {
        panic!("expected navigator");
    };

    let rows =
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);

    assert!(rows.iter().any(|row| {
        matches!(
            &row.target,
            ClientNavigatorTarget::Machine { endpoint_id: target } if target == &endpoint_id
        ) && row.label == "Build"
            && row.stale
    }));
    assert!(!rows.iter().any(|row| match &row.target {
        ClientNavigatorTarget::Machine { .. } => false,
        ClientNavigatorTarget::Workspace {
            endpoint_id: target,
            ..
        }
        | ClientNavigatorTarget::Tab {
            endpoint_id: target,
            ..
        }
        | ClientNavigatorTarget::Pane {
            endpoint_id: target,
            ..
        } => target == &endpoint_id,
    }));
}

#[test]
fn navigator_machine_selection_opens_its_remembered_view() {
    let (mut state, endpoint_id) = state_with_remote();
    state.open_navigator_overlay();
    let selected = {
        let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
        else {
            panic!("expected navigator");
        };
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator)
            .into_iter()
            .find(|row| {
                matches!(
                    &row.target,
                    ClientNavigatorTarget::Machine { endpoint_id: target } if target == &endpoint_id
                )
            })
            .map(|row| row.target)
            .expect("remote machine row")
    };
    if let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() {
        navigator.selected = Some(selected);
    }

    let mut outcome = ClientShellInput::default();
    state.accept_navigator_selection(&mut outcome);

    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: activated,
            target: None,
        }] if activated == &endpoint_id
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn navigator_foreign_pane_selection_activates_its_endpoint() {
    let (mut state, endpoint_id) = state_with_remote();
    state.open_navigator_overlay();
    let selected = {
        let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
        else {
            panic!("expected navigator");
        };
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator)
            .iter()
            .find(|row| {
                matches!(
                    &row.target,
                    ClientNavigatorTarget::Pane {
                        endpoint_id: target_endpoint,
                        pane_id,
                    } if target_endpoint == &endpoint_id && pane_id == "pane_1"
                )
            })
            .map(|row| row.target.clone())
            .expect("remote pane row")
    };
    if let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() {
        navigator.selected = Some(selected);
    }
    let mut local = snapshot();
    let mut inserted = local.workspaces[0].clone();
    inserted.workspace_id = "ws_2".into();
    inserted.focused = false;
    local.workspaces.push(inserted);
    state.set_snapshot(Box::new(local));

    let mut outcome = ClientShellInput::default();
    state.accept_navigator_selection(&mut outcome);

    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: activated,
            target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
        }] if activated == &endpoint_id && pane_id == "pane_1"
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn mobile_foreign_agent_and_workspace_targets_activate_their_endpoint() {
    use crate::api::schema::AgentStatus;

    let (mut state, endpoint_id) = state_with_remote();
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .expect("remote endpoint")
        .snapshot
        .as_mut()
        .expect("remote snapshot")
        .agents = vec![agent("remote agent", AgentStatus::Working, 2)];
    state.mode = ClientShellMode::Navigate;
    state.compose(44, 30).expect("mobile switcher");
    let remote_agent = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(
                target,
                ClientMobileTarget::Agent {
                    endpoint_id: target_endpoint,
                    pane_id,
                } if target_endpoint == &endpoint_id && pane_id == "pane_1"
            )
            .then_some(*rect)
        })
        .expect("remote agent target");
    let click = |rect: Rect| {
        RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    let outcome = state.handle_raw_events(vec![click(remote_agent)]);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: activated,
            target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
        }] if activated == &endpoint_id && pane_id == "pane_1"
    ));

    state.mode = ClientShellMode::Navigate;
    state.compose(44, 30).expect("mobile switcher");
    let remote_workspace = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(
                target,
                ClientMobileTarget::Workspace {
                    endpoint_id: target_endpoint,
                    workspace_id,
                } if target_endpoint == &endpoint_id && workspace_id == "ws_1"
            )
            .then_some(*rect)
        })
        .expect("remote workspace target");
    let outcome = state.handle_raw_events(vec![click(remote_workspace)]);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: activated,
            target: Some(ClientEndpointFocusTarget::Workspace(workspace_id)),
        }] if activated == &endpoint_id && workspace_id == "ws_1"
    ));
}

#[test]
fn cached_offline_navigator_and_mobile_targets_are_dimmed_and_disabled() {
    use crate::api::schema::AgentStatus;

    let (mut state, endpoint_id) = state_with_remote();
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .expect("remote endpoint")
        .snapshot
        .as_mut()
        .expect("remote snapshot")
        .agents = vec![agent("remote agent", AgentStatus::Blocked, 2)];
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Reconnecting);

    state.open_navigator_overlay();
    let selected = {
        let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
        else {
            panic!("expected navigator");
        };
        let rows =
            render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
        let machine = rows
            .iter()
            .find(|row| {
                matches!(
                    &row.target,
                    ClientNavigatorTarget::Machine { endpoint_id: target } if target == &endpoint_id
                )
            })
            .expect("cached remote machine row");
        assert!(machine.stale);
        let row = rows
            .iter()
            .find(|row| {
                matches!(
                    &row.target,
                    ClientNavigatorTarget::Pane {
                        endpoint_id: target_endpoint,
                        pane_id,
                    } if target_endpoint == &endpoint_id && pane_id == "pane_1"
                )
            })
            .expect("cached remote pane row");
        assert!(row.stale);
        assert!(!row.meta.contains("reconnecting"));
        row.target.clone()
    };
    if let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() {
        navigator.selected = Some(selected);
    }
    let mut navigator_outcome = ClientShellInput::default();
    state.accept_navigator_selection(&mut navigator_outcome);
    assert!(navigator_outcome.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Navigator(_))
    ));

    state.overlay = None;
    state.mode = ClientShellMode::Navigate;
    let frame = state.compose(44, 30).expect("offline mobile switcher");
    let mobile_target = state
        .hits
        .mobile_targets
        .iter()
        .find_map(|(rect, target)| {
            matches!(
                target,
                ClientMobileTarget::Workspace {
                    endpoint_id: target_endpoint,
                    workspace_id,
                } if target_endpoint == &endpoint_id && workspace_id == "ws_1"
            )
            .then_some(*rect)
        })
        .expect("cached remote workspace target");
    let buffer = frame.to_ratatui_buffer().expect("mobile frame buffer");
    assert_eq!(
        buffer[(mobile_target.x, mobile_target.y)].fg,
        state.config.palette.overlay0
    );
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: mobile_target.x,
        row: mobile_target.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(outcome.actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
}

#[test]
fn focus_agent_index_uses_online_aggregate_rows() {
    use crate::api::schema::AgentStatus;

    let (mut state, endpoint_id) = state_with_remote();
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .expect("remote endpoint")
        .snapshot
        .as_mut()
        .expect("remote snapshot")
        .agents = vec![agent("remote agent", AgentStatus::Working, 2)];
    let focus_agent =
        |index| crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(index));

    assert!(state.indexed_navigation_target_exists(&focus_agent(0)));
    assert!(!state.indexed_navigation_target_exists(&focus_agent(1)));

    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Reconnecting);
    assert!(!state.indexed_navigation_target_exists(&focus_agent(0)));
}

#[test]
fn workspace_drag_rejects_foreign_endpoint_slots() {
    let (mut state, endpoint_id) = state_with_remote();
    state.compose(100, 28).expect("aggregate sidebar");
    let local = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id.is_local())
        .expect("local workspace")
        .rect;
    let remote = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote workspace")
        .rect;
    let mouse = |kind, rect: Rect| {
        RawInputEvent::Mouse(MouseEvent {
            kind,
            column: rect.x.saturating_add(1),
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };

    state.handle_raw_events(vec![mouse(MouseEventKind::Down(MouseButton::Left), local)]);
    state.handle_raw_events(vec![mouse(MouseEventKind::Drag(MouseButton::Left), remote)]);

    assert!(state.chrome_drag.is_none());
}

#[test]
fn collapsed_aggregate_workspace_status_uses_its_status_color() {
    use crate::api::schema::AgentStatus;

    let (mut state, endpoint_id) = state_with_remote();
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .expect("remote endpoint")
        .snapshot
        .as_mut()
        .expect("remote snapshot")
        .workspaces[0]
        .agent_status = AgentStatus::Blocked;
    state.sidebar_collapsed = true;

    let frame = state.compose(100, 28).expect("collapsed aggregate sidebar");
    let workspace = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote workspace")
        .rect;
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        buffer[(workspace.x.saturating_add(2), workspace.y)].fg,
        state.config.palette.red
    );
}

#[test]
fn navigator_foreign_tab_selection_keeps_the_tab_target() {
    let (mut state, endpoint_id) = state_with_remote();
    state.open_navigator_overlay();
    let selected = {
        let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
        else {
            panic!("expected navigator");
        };
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator)
            .iter()
            .find(|row| {
                matches!(
                    &row.target,
                    ClientNavigatorTarget::Tab {
                        endpoint_id: target_endpoint,
                        tab_id,
                        ..
                    } if target_endpoint == &endpoint_id && tab_id == "tab_1"
                )
            })
            .map(|row| row.target.clone())
            .expect("remote tab row")
    };
    if let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() {
        navigator.selected = Some(selected);
    }

    let mut outcome = ClientShellInput::default();
    state.accept_navigator_selection(&mut outcome);

    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: activated,
            target: Some(ClientEndpointFocusTarget::Tab(tab_id)),
        }] if activated == &endpoint_id && tab_id == "tab_1"
    ));
}
