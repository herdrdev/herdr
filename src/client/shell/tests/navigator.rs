use super::*;
use crossterm::event::KeyModifiers;

fn search_state() -> ClientShellState {
    let mut snapshot = snapshot();
    snapshot.workspaces[0].label = "PR reviews".into();
    snapshot.tabs[0].label = "review tab".into();
    snapshot.panes[0].label = Some("notes".into());
    let mut matching_pane = snapshot.panes[0].clone();
    matching_pane.pane_id = "pane_2".into();
    matching_pane.label = Some("Exclusive lock".into());
    matching_pane.focused = false;
    snapshot.panes.push(matching_pane);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    state
}

fn press(state: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        code,
        KeyModifiers::NONE,
    ))])
}

fn selected_target(state: &ClientShellState) -> Option<ClientNavigatorTarget> {
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_ref() else {
        panic!("expected navigator");
    };
    let rows =
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
    super::super::aggregate_navigation::selected_navigator_target(&rows, navigator)
}

#[test]
fn navigator_search_selects_matching_pane_instead_of_workspace_context() {
    let mut state = search_state();
    press(&mut state, KeyCode::Char('/'));
    state.handle_input_bytes(b"exclusive");
    let expected = ClientNavigatorTarget::Pane {
        endpoint_id: ClientEndpointId::Local,
        pane_id: "pane_2".into(),
    };
    assert_eq!(selected_target(&state), Some(expected.clone()));

    let frame = state.compose(106, 30).expect("filtered navigator frame");
    let pane_rect = state
        .hits
        .navigator_rows
        .iter()
        .find(|(_, target)| target == &expected)
        .map(|(rect, _)| *rect)
        .expect("matching pane should remain visible");
    let buffer = frame.to_ratatui_buffer().unwrap();
    assert_eq!(
        buffer.cell((pane_rect.x, pane_rect.y)).unwrap().bg,
        state.config.palette.accent
    );

    let accepted = press(&mut state, KeyCode::Enter);
    assert!(matches!(
        accepted.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, crate::api::schema::Method::PaneFocus(params)
                if params.pane_id == "pane_2")
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn navigator_search_selects_first_direct_match_at_each_level() {
    let workspace = ClientNavigatorTarget::Workspace {
        endpoint_id: ClientEndpointId::Local,
        workspace_id: "ws_1".into(),
    };
    let tab = ClientNavigatorTarget::Tab {
        endpoint_id: ClientEndpointId::Local,
        tab_id: "tab_1".into(),
    };
    let pane = ClientNavigatorTarget::Pane {
        endpoint_id: ClientEndpointId::Local,
        pane_id: "pane_2".into(),
    };
    for (query, expected) in [
        ("pr reviews", workspace.clone()),
        ("main", workspace),
        (
            "review",
            ClientNavigatorTarget::Workspace {
                endpoint_id: ClientEndpointId::Local,
                workspace_id: "ws_1".into(),
            },
        ),
        ("exclusive", pane.clone()),
        ("  EXCLUSIVE  ", pane),
        (
            "repo",
            ClientNavigatorTarget::Pane {
                endpoint_id: ClientEndpointId::Local,
                pane_id: "pane_1".into(),
            },
        ),
        ("review tab", tab),
    ] {
        let mut state = search_state();
        press(&mut state, KeyCode::Char('/'));
        state.handle_raw_events(vec![RawInputEvent::Paste(query.into())]);
        assert_eq!(selected_target(&state), Some(expected), "query {query:?}");
    }
}

#[test]
fn navigator_search_keeps_manual_context_selection_and_home() {
    let mut state = search_state();
    press(&mut state, KeyCode::Char('/'));
    state.handle_input_bytes(b"exclusive");
    press(&mut state, KeyCode::Up);
    assert!(matches!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Tab { .. })
    ));
    let accepted = press(&mut state, KeyCode::Enter);
    assert!(matches!(
        accepted.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, crate::api::schema::Method::TabFocus(params)
                if params.tab_id == "tab_1")
    ));

    let mut state = search_state();
    press(&mut state, KeyCode::Char('/'));
    state.handle_input_bytes(b"exclusive");
    press(&mut state, KeyCode::Esc);
    press(&mut state, KeyCode::Home);
    assert_eq!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Workspace {
            endpoint_id: ClientEndpointId::Local,
            workspace_id: "ws_1".into(),
        })
    );
    let accepted = press(&mut state, KeyCode::Enter);
    assert!(matches!(
        accepted.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, crate::api::schema::Method::WorkspaceFocus(params)
                if params.workspace_id == "ws_1")
    ));
}

#[test]
fn navigator_search_reselects_after_query_edits_but_not_cursor_movement() {
    let mut state = search_state();
    press(&mut state, KeyCode::Char('/'));
    state.handle_input_bytes(b"exclusive");
    press(&mut state, KeyCode::Up);
    let selected = selected_target(&state);
    press(&mut state, KeyCode::Left);
    assert_eq!(selected_target(&state), selected);
    press(&mut state, KeyCode::End);
    press(&mut state, KeyCode::Backspace);
    assert!(matches!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Pane { pane_id, .. }) if pane_id == "pane_2"
    ));

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    ))]);
    assert!(matches!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Workspace { .. })
    ));
    state.handle_raw_events(vec![RawInputEvent::Paste("notes".into())]);
    assert!(matches!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Pane { pane_id, .. }) if pane_id == "pane_1"
    ));
}

#[test]
fn navigator_search_does_not_treat_context_metadata_as_searchable() {
    let mut state = search_state();
    press(&mut state, KeyCode::Char('/'));
    state.handle_input_bytes(b"panes");
    assert_eq!(selected_target(&state), None);
    let accepted = press(&mut state, KeyCode::Enter);
    assert!(accepted.actions.is_empty());
    assert!(state.overlay.is_some());
}

#[test]
fn navigator_search_selects_qualified_remote_targets_not_machine_context() {
    use crate::client::endpoint::{ProfileId, SavedSshEndpoint};

    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote_id = ClientEndpointId::Ssh(profile.id.clone());
    for (query, expected) in [
        (
            "exclusive",
            ClientNavigatorTarget::Pane {
                endpoint_id: remote_id.clone(),
                pane_id: "pane_1".into(),
            },
        ),
        (
            "remote reviews",
            ClientNavigatorTarget::Workspace {
                endpoint_id: remote_id.clone(),
                workspace_id: "ws_1".into(),
            },
        ),
        (
            "build",
            ClientNavigatorTarget::Machine {
                endpoint_id: remote_id.clone(),
            },
        ),
    ] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_endpoint_catalog(std::slice::from_ref(&profile));
        state.set_endpoint_status(&remote_id, ClientEndpointStatus::Online);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut remote = snapshot();
        remote.boot_id = "remote-boot".into();
        remote.workspaces[0].label = "remote reviews".into();
        remote.panes[0].label = Some("Exclusive lock".into());
        state.set_endpoint_snapshot(&remote_id, Box::new(remote));
        state.open_navigator_overlay();
        press(&mut state, KeyCode::Char('/'));
        state.handle_raw_events(vec![RawInputEvent::Paste(query.into())]);
        assert_eq!(
            selected_target(&state),
            Some(expected.clone()),
            "query {query}"
        );

        let accepted = press(&mut state, KeyCode::Enter);
        let [ClientShellAction::ActivateEndpoint {
            endpoint_id,
            target,
        }] = accepted.actions.as_slice()
        else {
            panic!("remote search result must activate its endpoint");
        };
        assert_eq!(endpoint_id, &remote_id);
        match expected {
            ClientNavigatorTarget::Pane { pane_id, .. } => {
                assert_eq!(target, &Some(ClientEndpointFocusTarget::Pane(pane_id)));
            }
            ClientNavigatorTarget::Workspace { workspace_id, .. } => {
                assert_eq!(
                    target,
                    &Some(ClientEndpointFocusTarget::Workspace(workspace_id))
                );
            }
            ClientNavigatorTarget::Machine { .. } => assert_eq!(target, &None),
            _ => unreachable!(),
        }
    }
}

#[test]
fn navigator_search_no_query_preserves_current_selection_and_state_filter_defaults() {
    let mut state = search_state();
    assert_eq!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Pane {
            endpoint_id: ClientEndpointId::Local,
            pane_id: "pane_1".into(),
        })
    );
    press(&mut state, KeyCode::Char('i'));
    assert!(matches!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Workspace { .. })
    ));
    press(&mut state, KeyCode::Char('/'));
    state.handle_raw_events(vec![RawInputEvent::Paste("   ".into())]);
    assert!(matches!(
        selected_target(&state),
        Some(ClientNavigatorTarget::Workspace { .. })
    ));
}
