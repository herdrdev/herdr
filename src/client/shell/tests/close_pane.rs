use super::*;
use crate::api::schema::Method;
use crossterm::event::{MouseButton, MouseEventKind};

fn close_state(confirm: bool, tab_count: usize) -> ClientShellState {
    let mut projected = snapshot();
    for number in 2..=tab_count {
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = format!("tab_{number}");
        tab.number = number;
        tab.focused = false;
        projected.tabs.push(tab);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.confirm_close = confirm;
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).unwrap();
    state
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn request_close(state: &mut ClientShellState, menu: bool) -> ClientShellInput {
    if menu {
        let pane = state.hits.panes[0].rect;
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: pane.x + 1,
            row: pane.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.compose(106, 24).unwrap();
        let index = match state.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu
                .items()
                .iter()
                .position(|item| item.action == ClientContextMenuAction::ClosePane)
                .unwrap(),
            _ => panic!("pane menu"),
        };
        let close_row = state.hits.context_menu_rows[index].0;
        click(state, close_row)
    } else {
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::ClosePane),
            &mut outcome,
        );
        outcome
    }
}

fn assert_no_close(outcome: &ClientShellInput) {
    assert!(outcome.requests.is_empty());
    assert!(outcome.actions.iter().all(|action| {
        !matches!(action, ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, Method::PaneClose(_) | Method::TabClose(_) | Method::WorkspaceClose(_)))
    }));
}

fn assert_pane_close(outcome: &ClientShellInput) {
    let methods = outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .filter(|method| !matches!(method, Method::TabFocus(_) | Method::PaneFocus(_)))
        .collect::<Vec<_>>();
    assert!(
        matches!(methods.as_slice(), [Method::PaneClose(target)] if target.pane_id == "pane_1")
    );
}

#[test]
fn last_pane_close_waits_for_keyboard_or_mouse_confirmation() {
    for menu in [false, true] {
        let mut state = close_state(true, 1);
        let requested = request_close(&mut state, menu);
        assert_no_close(&requested);
        assert!(requested.repaint);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ConfirmClose(_))
        ));
        let frame = state.compose(106, 24).unwrap();
        let text = frame_rows(&frame).join("\n");
        assert!(text.contains("Close workspace?"));
        assert!(text.contains("1 pane"));
        let accepted = if menu {
            let primary = state.hits.overlay_primary;
            click(&mut state, primary)
        } else {
            state.handle_input_bytes(b"\r")
        };
        assert_pane_close(&accepted);
        assert!(state.overlay.is_none());
    }
}

#[test]
fn last_pane_close_confirmation_can_be_cancelled() {
    for mouse in [false, true] {
        let mut state = close_state(true, 1);
        assert_no_close(&request_close(&mut state, false));
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ConfirmClose(_))
        ));
        state.compose(106, 24).unwrap();
        let cancelled = if mouse {
            let cancel = state.hits.overlay_cancel;
            click(&mut state, cancel)
        } else {
            state.handle_input_bytes(b"\x1b")
        };
        assert_no_close(&cancelled);
        assert!(state.overlay.is_none());
    }
}

#[test]
fn pane_close_stays_immediate_with_confirmation_disabled_or_other_tabs() {
    for (confirm, tabs) in [(false, 1), (false, 2), (true, 2)] {
        for menu in [false, true] {
            let mut state = close_state(confirm, tabs);
            assert_pane_close(&request_close(&mut state, menu));
            assert!(state.overlay.is_none());
        }
    }
}

#[test]
fn last_pane_confirmation_preserves_target_across_focus_changes_and_new_tabs() {
    let mut state = close_state(true, 1);
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    let mut other_workspace = projected.workspaces[0].clone();
    other_workspace.workspace_id = "ws_2".into();
    other_workspace.active_tab_id = "other_tab".into();
    other_workspace.focused = false;
    projected.workspaces.push(other_workspace);
    let mut other_tab = projected.tabs[0].clone();
    other_tab.workspace_id = "ws_2".into();
    other_tab.tab_id = "other_tab".into();
    other_tab.focused = false;
    projected.tabs.push(other_tab);
    state.set_snapshot(Box::new(projected.clone()));

    assert_no_close(&request_close(&mut state, false));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(_))
    ));
    let mut new_tab = projected.tabs[0].clone();
    new_tab.tab_id = "new_tab".into();
    new_tab.focused = false;
    projected.tabs.push(new_tab);
    let mut new_pane = projected.panes[0].clone();
    new_pane.pane_id = "new_pane".into();
    new_pane.focused = false;
    projected.panes.push(new_pane);
    projected.focused_pane_id = Some("new_pane".into());
    projected.focused_workspace_id = Some("ws_2".into());
    projected.focused_tab_id = Some("other_tab".into());
    for workspace in &mut projected.workspaces {
        workspace.focused = workspace.workspace_id == "ws_2";
    }
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "other_tab";
    }
    state.set_snapshot(Box::new(projected));
    assert_pane_close(&state.handle_input_bytes(b"\r"));
}

#[test]
fn last_pane_confirmation_rejects_missing_moved_or_reconnected_targets() {
    for change in [
        "missing",
        "moved",
        "reconnected",
        "pane_missing",
        "pane_moved",
        "pane_workspace",
        "endpoint",
    ] {
        let mut state = close_state(true, 1);
        assert_no_close(&request_close(&mut state, false));
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ConfirmClose(_))
        ));
        let mut projected = state.snapshot.as_deref().unwrap().clone();
        match change {
            "missing" => projected.tabs.clear(),
            "moved" => projected.tabs[0].workspace_id = "different_workspace".into(),
            "reconnected" => state.endpoints[0].snapshot_generation = Some(2),
            "pane_missing" => projected.panes.clear(),
            "pane_moved" => projected.panes[0].tab_id = "different_tab".into(),
            "pane_workspace" => projected.panes[0].workspace_id = "different_workspace".into(),
            "endpoint" => {
                state.active_endpoint_id =
                    ClientEndpointId::Ssh(crate::client::endpoint::ProfileId::generate())
            }
            _ => unreachable!(),
        }
        if !matches!(change, "reconnected" | "endpoint") {
            state.set_snapshot(Box::new(projected));
        }
        assert_no_close(&state.handle_input_bytes(b"\r"));
        assert!(state.overlay.is_none());
    }
}

#[test]
fn last_pane_close_preserves_parent_group_and_linked_workspace_scope() {
    for linked in [false, true] {
        let mut state = close_state(true, 1);
        let mut projected = state.snapshot.as_deref().unwrap().clone();
        projected.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: linked,
        });
        let mut sibling = projected.workspaces[0].clone();
        sibling.workspace_id = "ws_2".into();
        sibling.active_tab_id = "tab_2".into();
        sibling.focused = false;
        sibling.worktree.as_mut().unwrap().is_linked_worktree = !linked;
        projected.workspaces.push(sibling);
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = "tab_2".into();
        tab.workspace_id = "ws_2".into();
        tab.focused = false;
        projected.tabs.push(tab);
        state.set_snapshot(Box::new(projected));

        let close = request_close(&mut state, false);
        if linked {
            assert_no_close(&close);
            assert!(matches!(
                state.overlay,
                Some(ClientShellOverlay::ConfirmClose(_))
            ));
            assert_pane_close(&state.handle_input_bytes(b"\r"));
        } else {
            assert_pane_close(&close);
            assert!(state.overlay.is_none());
            let [ClientShellAction::Endpoint { request, .. }] = close.actions.as_slice() else {
                panic!("tab close request");
            };
            state.handle_endpoint_result(
                "boot-1",
                &request.id,
                Err(ClientShellEndpointError {
                    code: Some("confirmation_required".into()),
                    message: "closing this tab would close a worktree group".into(),
                }),
            );
            assert!(matches!(state.overlay.as_ref(),
                Some(ClientShellOverlay::ConfirmClose(confirm)) if confirm.title == "Close worktree group?"));
            let accepted = state.handle_input_bytes(b"\r");
            assert!(matches!(accepted.actions.as_slice(),
                [ClientShellAction::Endpoint { request, .. }]
                    if matches!(&request.method, Method::WorkspaceClose(params)
                        if params.workspace_id == "ws_1" && params.close_group)));
        }
    }
}

#[test]
fn ordinary_pane_close_stays_immediate_with_another_pane() {
    for menu in [false, true] {
        let mut state = close_state(true, 1);
        let mut projected = state.snapshot.as_deref().unwrap().clone();
        let mut pane = projected.panes[0].clone();
        pane.pane_id = "pane_2".into();
        pane.focused = false;
        projected.panes.push(pane);
        state.set_snapshot(Box::new(projected));
        assert_pane_close(&request_close(&mut state, menu));
        assert!(state.overlay.is_none());
    }
}
