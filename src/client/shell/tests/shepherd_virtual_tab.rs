use super::*;
use crate::api::schema::{AgentStatus, Method};
use crate::client::endpoint::{ClientEndpointId, ClientEndpointStatus};
use crate::config::Config;
use crate::protocol::ClientShellTab;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn test_config(shepherd_enabled: bool) -> ClientShellConfig {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.shepherd_enabled = shepherd_enabled;
    config
}

fn test_snapshot_with_two_tabs() -> ClientShellSnapshot {
    let mut snap = snapshot();
    snap.tabs = vec![
        ClientShellTab {
            tab_id: "tab_1".into(),
            workspace_id: "ws_1".into(),
            number: 1,
            label: "1".into(),
            custom_label: false,
            zoomed: false,
            focused: true,
            agent_status: AgentStatus::Idle,
        },
        ClientShellTab {
            tab_id: "tab_2".into(),
            workspace_id: "ws_1".into(),
            number: 2,
            label: "2".into(),
            custom_label: false,
            zoomed: false,
            focused: false,
            agent_status: AgentStatus::Idle,
        },
    ];
    snap
}

fn endpoint_methods(outcome: &ClientShellInput) -> Vec<&Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .collect()
}

#[test]
fn test_render_tab_bar_with_virtual_shepherd_tab_active() {
    let config = test_config(true);
    let snap = snapshot();
    let area = Rect::new(0, 0, 80, 1);
    let mut buffer = Buffer::empty(area);
    let mut hits = ShellHitMap::default();
    let mut tab_scroll = 0;
    let mut reveal_focused = true;

    render::render_tab_bar(
        &mut buffer,
        area,
        &snap,
        &config,
        &mut tab_scroll,
        &mut reveal_focused,
        None,
        true, // shepherd_tab_active
        &mut hits,
    );

    // Shepherd tab is first (index 0)
    assert_eq!(hits.tabs.len(), 2);
    assert_eq!(hits.tabs[0].1, crate::shepherd::SHEPHERD_VIRTUAL_TAB_ID);
    assert_eq!(hits.tabs[0].0.x, 0);
    assert_eq!(hits.tabs[0].0.width, 12); // "Shepherd" (8) + 4 = 12

    // Real tab 1 is second (index 1)
    assert_eq!(hits.tabs[1].1, "tab_1");
    assert_eq!(hits.tabs[1].0.x, 13);
    assert_eq!(hits.tabs[1].0.width, 8); // "1" (1) + 4 = 5.max(8) = 8

    // Verify cell contents
    let shepherd_text: String = (0..12)
        .map(|x| buffer.cell((x, 0)).unwrap().symbol().to_string())
        .collect();
    assert_eq!(shepherd_text, "  Shepherd  ");

    // Shepherd tab is active -> accent background
    let shepherd_style = buffer.cell((2, 0)).unwrap().style();
    assert_eq!(shepherd_style.bg, Some(config.palette.accent));

    // Real tab 1 is unfocused -> surface0 background
    let tab1_text: String = (13..21)
        .map(|x| buffer.cell((x, 0)).unwrap().symbol().to_string())
        .collect();
    assert_eq!(tab1_text, "   1    ");
    let tab1_style = buffer.cell((14, 0)).unwrap().style();
    assert_eq!(tab1_style.bg, Some(config.palette.surface0));
}

#[test]
fn test_render_tab_bar_with_shepherd_inactive() {
    let config = test_config(true);
    let snap = snapshot();
    let area = Rect::new(0, 0, 80, 1);
    let mut buffer = Buffer::empty(area);
    let mut hits = ShellHitMap::default();
    let mut tab_scroll = 0;
    let mut reveal_focused = true;

    render::render_tab_bar(
        &mut buffer,
        area,
        &snap,
        &config,
        &mut tab_scroll,
        &mut reveal_focused,
        None,
        false, // shepherd_tab_active == false
        &mut hits,
    );

    // Shepherd tab is unfocused -> surface0 background
    let shepherd_style = buffer.cell((2, 0)).unwrap().style();
    assert_eq!(shepherd_style.bg, Some(config.palette.surface0));

    // Real tab 1 is focused -> accent background
    let tab1_style = buffer.cell((14, 0)).unwrap().style();
    assert_eq!(tab1_style.bg, Some(config.palette.accent));
}

#[test]
fn test_render_tab_bar_disabled_matches_upstream() {
    let config = test_config(false);
    let snap = snapshot();
    let area = Rect::new(0, 0, 80, 1);
    let mut buffer = Buffer::empty(area);
    let mut hits = ShellHitMap::default();
    let mut tab_scroll = 0;
    let mut reveal_focused = true;

    render::render_tab_bar(
        &mut buffer,
        area,
        &snap,
        &config,
        &mut tab_scroll,
        &mut reveal_focused,
        None,
        false,
        &mut hits,
    );

    // Only 1 tab (tab_1), at x=0
    assert_eq!(hits.tabs.len(), 1);
    assert_eq!(hits.tabs[0].1, "tab_1");
    assert_eq!(hits.tabs[0].0.x, 0);

    let tab1_text: String = (0..8)
        .map(|x| buffer.cell((x, 0)).unwrap().symbol().to_string())
        .collect();
    assert_eq!(tab1_text, "   1    ");
}

#[test]
fn test_default_active_on_session_start_and_workspace_switch() {
    let mut state = ClientShellState::new(test_config(true));
    state.apply_active_snapshot(Box::new(test_snapshot_with_two_tabs()), None);

    // Initially in ws_1: Shepherd virtual tab is active by default
    assert!(state.is_shepherd_tab_active());

    // Switch to tab 1 explicitly
    let mut outcome = ClientShellInput::default();
    state.set_shepherd_tab_active(false, &mut outcome);
    assert!(!state.is_shepherd_tab_active());

    // Switch to ws_2 (simulate changing workspace in snapshot)
    let mut snap2 = test_snapshot_with_two_tabs();
    snap2.focused_workspace_id = Some("ws_2".into());
    snap2.workspaces.push(ClientShellWorkspace {
        workspace_id: "ws_2".into(),
        active_tab_id: "tab_2_1".into(),
        new_workspace_cwd: "/repo2".into(),
        number: 2,
        label: "feature".into(),
        custom_label: false,
        branch: Some("feat".into()),
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: None,
        focused: true,
        agent_status: AgentStatus::Idle,
    });
    snap2.tabs.push(ClientShellTab {
        tab_id: "tab_2_1".into(),
        workspace_id: "ws_2".into(),
        number: 1,
        label: "1".into(),
        custom_label: false,
        zoomed: false,
        focused: true,
        agent_status: AgentStatus::Idle,
    });
    state.apply_active_snapshot(Box::new(snap2), None);

    // In ws_2 (no explicit choice yet): Shepherd virtual tab is ACTIVE BY DEFAULT!
    assert!(state.is_shepherd_tab_active());

    // Switch back to ws_1: remembers user's explicit choice (false)
    let mut snap1_again = test_snapshot_with_two_tabs();
    snap1_again.focused_workspace_id = Some("ws_1".into());
    state.apply_active_snapshot(Box::new(snap1_again), None);
    assert!(!state.is_shepherd_tab_active());
}

#[test]
fn test_tab_numbering_and_order_preserved() {
    let mut state = ClientShellState::new(test_config(true));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(test_snapshot_with_two_tabs()));

    // SwitchTab(0) corresponds to prefix+1 / Alt+1 -> switches to real tab 1 ("tab_1")
    let mut outcome = ClientShellInput::default();
    let handled = state.handle_tab_navigation(crate::input::KeybindAction::SwitchTab(0), &mut outcome);
    assert!(handled);
    assert!(!state.is_shepherd_tab_active());
    let methods = endpoint_methods(&outcome);
    assert_eq!(methods.len(), 1);
    match methods[0] {
        Method::TabFocus(target) => {
            assert_eq!(target.tab_id, "tab_1");
        }
        other => panic!("expected TabFocus, got {other:?}"),
    }

    // SwitchTab(1) corresponds to prefix+2 / Alt+2 -> switches to real tab 2 ("tab_2")
    let mut outcome2 = ClientShellInput::default();
    let handled2 = state.handle_tab_navigation(crate::input::KeybindAction::SwitchTab(1), &mut outcome2);
    assert!(handled2);
    assert!(!state.is_shepherd_tab_active());
    let methods2 = endpoint_methods(&outcome2);
    assert_eq!(methods2.len(), 1);
    match methods2[0] {
        Method::TabFocus(target) => {
            assert_eq!(target.tab_id, "tab_2");
        }
        other => panic!("expected TabFocus, got {other:?}"),
    }
}

#[test]
fn test_tab_cycling_next_and_previous_coherent() {
    let mut state = ClientShellState::new(test_config(true));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    let mut snap = test_snapshot_with_two_tabs();
    state.set_snapshot(Box::new(snap.clone()));

    // Case 1: On Shepherd virtual tab, NextTab -> switches to Tab 1
    assert!(state.is_shepherd_tab_active());
    let mut outcome = ClientShellInput::default();
    let handled = state.handle_tab_navigation(crate::input::KeybindAction::NextTab, &mut outcome);
    assert!(handled);
    assert!(!state.is_shepherd_tab_active());
    let methods1 = endpoint_methods(&outcome);
    match methods1[0] {
        Method::TabFocus(target) => assert_eq!(target.tab_id, "tab_1"),
        other => panic!("expected TabFocus, got {other:?}"),
    }

    // Case 2: On Tab 1, PreviousTab -> switches to Shepherd virtual tab
    snap.focused_tab_id = Some("tab_1".into());
    state.set_snapshot(Box::new(snap.clone()));
    let mut outcome2 = ClientShellInput::default();
    let handled2 = state.handle_tab_navigation(crate::input::KeybindAction::PreviousTab, &mut outcome2);
    assert!(handled2);
    assert!(state.is_shepherd_tab_active());

    // Case 3: On Shepherd virtual tab, PreviousTab -> wraps around to last real tab (Tab 2)
    let mut outcome3 = ClientShellInput::default();
    let handled3 = state.handle_tab_navigation(crate::input::KeybindAction::PreviousTab, &mut outcome3);
    assert!(handled3);
    assert!(!state.is_shepherd_tab_active());
    let methods3 = endpoint_methods(&outcome3);
    match methods3[0] {
        Method::TabFocus(target) => assert_eq!(target.tab_id, "tab_2"),
        other => panic!("expected TabFocus, got {other:?}"),
    }

    // Case 4: On Tab 2 (last real tab), NextTab -> wraps around to Shepherd virtual tab
    snap.focused_tab_id = Some("tab_2".into());
    state.set_snapshot(Box::new(snap));
    let mut outcome4 = ClientShellInput::default();
    let handled4 = state.handle_tab_navigation(crate::input::KeybindAction::NextTab, &mut outcome4);
    assert!(handled4);
    assert!(state.is_shepherd_tab_active());
}

#[test]
fn test_virtual_tab_cannot_be_closed_or_renamed() {
    let mut state = ClientShellState::new(test_config(true));
    state.apply_active_snapshot(Box::new(test_snapshot_with_two_tabs()), None);
    assert!(state.is_shepherd_tab_active());

    // Try to close virtual tab via CloseTab action
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseTab),
        &mut outcome,
    );
    // Method TabClose must NOT be pushed
    let methods = endpoint_methods(&outcome);
    assert!(methods.iter().all(|m| !matches!(m, Method::TabClose(_))));

    // Try to rename virtual tab via RenameTab action
    let mut outcome2 = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::RenameTab),
        &mut outcome2,
    );
    assert!(state.overlay.is_none());
}

#[test]
fn test_state_preservation_across_tab_switches() {
    let mut state = ClientShellState::new(test_config(true));
    state.apply_active_snapshot(Box::new(test_snapshot_with_two_tabs()), None);
    assert!(state.is_shepherd_tab_active());

    // Select Tasks tab in Shepherd section
    state
        .ensure_shepherd_section()
        .select_tab(crate::shepherd::ui::state::ShepherdSectionTab::Tasks);
    assert_eq!(
        state.ensure_shepherd_section().active_tab,
        crate::shepherd::ui::state::ShepherdSectionTab::Tasks
    );

    // Switch to terminal tab 1
    let mut outcome = ClientShellInput::default();
    state.set_shepherd_tab_active(false, &mut outcome);
    assert!(!state.is_shepherd_tab_active());

    // Section is preserved!
    assert_eq!(
        state.ensure_shepherd_section().active_tab,
        crate::shepherd::ui::state::ShepherdSectionTab::Tasks
    );

    // Switch back to Shepherd tab
    state.select_shepherd_tab(&mut outcome);
    assert!(state.is_shepherd_tab_active());
    assert_eq!(
        state.ensure_shepherd_section().active_tab,
        crate::shepherd::ui::state::ShepherdSectionTab::Tasks
    );
}

#[test]
fn test_small_dimensions_80x24_pure_render() {
    let config = test_config(true);
    let snap = snapshot();
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    let mut hits = ShellHitMap::default();
    let mut tab_scroll = 0;
    let mut reveal_focused = true;

    // Tab bar in 80x24 layout
    let tab_bar_area = Rect::new(18, 0, 62, 1);
    render::render_tab_bar(
        &mut buffer,
        tab_bar_area,
        &snap,
        &config,
        &mut tab_scroll,
        &mut reveal_focused,
        None,
        true,
        &mut hits,
    );

    assert_eq!(hits.tabs.len(), 2);
    assert_eq!(hits.tabs[0].1, crate::shepherd::SHEPHERD_VIRTUAL_TAB_ID);
    assert_eq!(hits.tabs[1].1, "tab_1");
}

#[test]
fn test_shepherd_tab_mouse_click_hit_testing() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let mut state = ClientShellState::new(test_config(true));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(test_snapshot_with_two_tabs()));
    state.set_pane_surface(surface());
    state.compose(106, 24).unwrap();

    // Verify initial state: Shepherd is active
    assert!(state.is_shepherd_tab_active());

    // Hits for tabs must contain Shepherd and Tab 1
    assert!(state.hits.tabs.len() >= 2);
    let shepherd_rect = state.hits.tabs[0].0;
    let tab1_rect = state.hits.tabs[1].0;
    assert_eq!(state.hits.tabs[0].1, crate::shepherd::SHEPHERD_VIRTUAL_TAB_ID);
    assert_eq!(state.hits.tabs[1].1, "tab_1");

    // Click on Tab 1 -> deactivates Shepherd virtual tab
    let mut outcome = ClientShellInput::default();
    state.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: tab1_rect.x + 1,
            row: tab1_rect.y,
            modifiers: KeyModifiers::empty(),
        },
        &mut outcome,
    );
    state.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: tab1_rect.x + 1,
            row: tab1_rect.y,
            modifiers: KeyModifiers::empty(),
        },
        &mut outcome,
    );
    assert!(!state.is_shepherd_tab_active());

    // Click on Shepherd tab -> re-activates Shepherd virtual tab
    let mut outcome2 = ClientShellInput::default();
    state.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: shepherd_rect.x + 1,
            row: shepherd_rect.y,
            modifiers: KeyModifiers::empty(),
        },
        &mut outcome2,
    );
    assert!(state.is_shepherd_tab_active());
}

#[test]
fn test_toggle_shepherd_keybind() {
    let mut state = ClientShellState::new(test_config(true));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(test_snapshot_with_two_tabs()));

    // Starts active
    assert!(state.is_shepherd_tab_active());

    // Record KeybindAction::Shepherd -> toggles to inactive
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::Shepherd),
        &mut outcome,
    );
    assert!(!state.is_shepherd_tab_active());

    // Record KeybindAction::Shepherd -> toggles back to active
    let mut outcome2 = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::Shepherd),
        &mut outcome2,
    );
    assert!(state.is_shepherd_tab_active());
}

#[test]
fn test_tick_shepherd_auto_refreshes_on_snapshot_and_interval() {
    let mut state = ClientShellState::new(test_config(true));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(test_snapshot_with_two_tabs()));

    let now = std::time::Instant::now();
    let mut outcome = ClientShellInput::default();

    // 1. First tick refreshes views because last_shepherd_refresh was None
    let refreshed = state.tick_shepherd(now, &mut outcome);
    assert!(refreshed);
    assert!(!outcome.actions.is_empty());

    // 2. Immediate subsequent tick within 100ms does NOT re-trigger
    let mut outcome2 = ClientShellInput::default();
    let refreshed2 = state.tick_shepherd(now + std::time::Duration::from_millis(100), &mut outcome2);
    assert!(!refreshed2);

    // 3. New snapshot arrives (e.g. from server when agents or panes are created)
    state.set_snapshot(Box::new(test_snapshot_with_two_tabs()));
    assert!(state.pending_shepherd_refresh);

    // 4. Tick immediately executes the pending refresh from the snapshot
    let mut outcome3 = ClientShellInput::default();
    let refreshed3 = state.tick_shepherd(now + std::time::Duration::from_millis(200), &mut outcome3);
    assert!(refreshed3);
    assert!(!state.pending_shepherd_refresh);

    // 5. Periodic tick after 2s fires
    let mut outcome4 = ClientShellInput::default();
    let refreshed4 = state.tick_shepherd(now + std::time::Duration::from_millis(2500), &mut outcome4);
    assert!(refreshed4);
}
