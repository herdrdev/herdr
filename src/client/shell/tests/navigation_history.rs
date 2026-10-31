use super::*;

/// Two workspaces: ws_1 holds tab_1 (pane_1, pane_1b) and tab_2 (pane_2);
/// ws_2 holds tab_3 (pane_3).
fn layout() -> ClientShellSnapshot {
    let mut projected = snapshot();
    let base_tab = projected.tabs[0].clone();
    let base_pane = projected.panes[0].clone();
    let base_workspace = projected.workspaces[0].clone();
    let tab = |tab_id: &str, workspace_id: &str| ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: workspace_id.into(),
        ..base_tab.clone()
    };
    let pane = |pane_id: &str, tab_id: &str, workspace_id: &str| ClientShellPane {
        pane_id: pane_id.into(),
        tab_id: tab_id.into(),
        workspace_id: workspace_id.into(),
        ..base_pane.clone()
    };
    projected.workspaces.push(ClientShellWorkspace {
        workspace_id: "ws_2".into(),
        active_tab_id: "tab_3".into(),
        ..base_workspace
    });
    projected.tabs = vec![
        tab("tab_1", "ws_1"),
        tab("tab_2", "ws_1"),
        tab("tab_3", "ws_2"),
    ];
    projected.panes = vec![
        pane("pane_1", "tab_1", "ws_1"),
        pane("pane_1b", "tab_1", "ws_1"),
        pane("pane_2", "tab_2", "ws_1"),
        pane("pane_3", "tab_3", "ws_2"),
    ];
    projected
}

/// Applies a server snapshot that focuses `pane_id`.
fn focus(state: &mut ClientShellState, pane_id: &str) {
    let mut next = state.snapshot.as_deref().cloned().unwrap_or_else(layout);
    let pane = next
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .expect("pane exists in layout")
        .clone();
    next.revision += 1;
    next.focused_workspace_id = Some(pane.workspace_id);
    next.focused_tab_id = Some(pane.tab_id);
    next.focused_pane_id = Some(pane.pane_id);
    state.set_snapshot(Box::new(next));
}

/// Presses a binding and returns the pane it asks the endpoint to focus. With
/// nowhere to step, the action falls through to the runtime's no-op `Keybind`.
fn press(state: &mut ClientShellState, action: crate::input::KeybindAction) -> Option<String> {
    let mut input = ClientShellInput::default();
    state.record_binding(crate::input::KeybindMatch::Action(action), &mut input);
    match &input.actions[..] {
        [ClientShellAction::Keybind(fallback)] if *fallback == action => None,
        [ClientShellAction::Endpoint { request, .. }] => match &request.method {
            crate::api::schema::Method::PaneFocus(target) => Some(target.pane_id.clone()),
            other => panic!("history step should focus a pane, got {other:?}"),
        },
        other => panic!("unexpected actions {other:?}"),
    }
}

/// Presses a binding and applies the resulting focus change, like the server would.
fn step(state: &mut ClientShellState, action: crate::input::KeybindAction) -> Option<String> {
    let target = press(state, action)?;
    focus(state, &target);
    Some(target)
}

#[test]
fn history_steps_through_tab_and_workspace_jumps_but_not_pane_moves() {
    use crate::input::KeybindAction::{HistoryBack, HistoryForward};

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(layout()));
    focus(&mut state, "pane_2"); // tab jump
    focus(&mut state, "pane_3"); // workspace jump
    focus(&mut state, "pane_1"); // workspace jump back to ws_1
    focus(&mut state, "pane_1b"); // pane move within tab_1, not recorded

    assert_eq!(step(&mut state, HistoryBack).as_deref(), Some("pane_3"));
    assert_eq!(step(&mut state, HistoryBack).as_deref(), Some("pane_2"));
    assert_eq!(step(&mut state, HistoryBack).as_deref(), Some("pane_1"));
    assert_eq!(press(&mut state, HistoryBack), None);

    assert_eq!(step(&mut state, HistoryForward).as_deref(), Some("pane_2"));
    assert_eq!(step(&mut state, HistoryForward).as_deref(), Some("pane_3"));
    // Returns to the exact pane left within tab_1, not the one first arrived at.
    assert_eq!(step(&mut state, HistoryForward).as_deref(), Some("pane_1b"));
    assert_eq!(press(&mut state, HistoryForward), None);
}

#[test]
fn history_is_cleared_when_the_endpoint_reboots() {
    use crate::input::KeybindAction::HistoryBack;

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(layout()));
    focus(&mut state, "pane_2");

    let mut rebooted = state.snapshot.as_deref().cloned().unwrap();
    rebooted.boot_id = "boot-2".into();
    state.set_snapshot(Box::new(rebooted));

    assert_eq!(press(&mut state, HistoryBack), None);
}

#[test]
fn a_step_between_panes_of_one_tab_does_not_swallow_the_next_jump() {
    use crate::input::KeybindAction::HistoryBack;

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(layout()));
    focus(&mut state, "pane_3");
    focus(&mut state, "pane_1");
    focus(&mut state, "pane_2");
    focus(&mut state, "pane_1b");

    // pane_2 closes, leaving pane_1 and pane_1b as neighbors in the history.
    let mut closed = state.snapshot.as_deref().cloned().unwrap();
    closed.revision += 1;
    closed.panes.retain(|pane| pane.pane_id != "pane_2");
    state.set_snapshot(Box::new(closed));

    // This step stays inside tab_1, so no tab or workspace change marks it.
    assert_eq!(step(&mut state, HistoryBack).as_deref(), Some("pane_1"));
    // A jump to a pane already in the history must still be recorded.
    focus(&mut state, "pane_3");

    assert_eq!(step(&mut state, HistoryBack).as_deref(), Some("pane_1"));
    assert_eq!(step(&mut state, HistoryBack).as_deref(), Some("pane_3"));
}
