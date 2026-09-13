use super::*;

fn workspaces(count: usize) -> ClientShellSnapshot {
    let mut projected = snapshot();
    projected.workspaces = (1..=count)
        .map(|number| {
            let mut workspace = projected.workspaces[0].clone();
            workspace.workspace_id = format!("ws_{number}");
            workspace.number = number;
            workspace.focused = number == 1;
            workspace
        })
        .collect();
    projected
}

fn grouped_workspaces() -> ClientShellSnapshot {
    let mut projected = workspaces(3);
    for (index, linked) in [(0, false), (2, true)] {
        projected.workspaces[index].worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: linked,
        });
    }
    projected
}

fn navigation_state(mut projected: ClientShellSnapshot) -> (ClientShellState, ClientEndpointId) {
    let (mut state, remote) = state_with_remote();
    state.set_snapshot(Box::new(projected.clone()));
    projected.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote, Box::new(projected));
    (state, remote)
}

fn preview_key(state: &mut ClientShellState, bytes: &[u8]) {
    let outcome = state.handle_input_bytes(bytes);
    assert!(outcome.actions.is_empty(), "{bytes:?}");
    assert!(outcome.requests.is_empty(), "{bytes:?}");
    assert!(outcome.repaint, "{bytes:?}");
}

fn enter_navigation(state: &mut ClientShellState) {
    preview_key(state, &[0x02]);
    preview_key(state, b"w");
    assert_eq!(state.mode, ClientShellMode::Navigate);
}

fn assert_selected(state: &ClientShellState, endpoint: &ClientEndpointId, workspace: &str) {
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(endpoint, workspace)
    );
}

fn workspace_rect(state: &ClientShellState, endpoint: &ClientEndpointId, workspace: &str) -> Rect {
    state
        .hits
        .workspaces
        .iter()
        .find(|hit| &hit.endpoint_id == endpoint && hit.workspace_id == workspace)
        .map(|hit| hit.rect)
        .or_else(|| {
            state.hits.mobile_targets.iter().find_map(|(rect, target)| {
                matches!(target, ClientMobileTarget::Workspace { endpoint_id, workspace_id }
                if endpoint_id == endpoint && workspace_id == workspace)
                .then_some(*rect)
            })
        })
        .expect("visible workspace")
}

#[test]
fn navigation_highlights_only_the_preview_and_activates_on_enter() {
    for (compact, cols) in [(true, 100), (false, 100), (false, 44)] {
        for terminal_theme in [false, true] {
            let (mut state, remote) = navigation_state(workspaces(2));
            state.sidebar_collapsed = compact;
            if terminal_theme {
                state.config.palette = Palette::terminal();
            }
            state.compose(cols, 28).unwrap();
            enter_navigation(&mut state);
            for (endpoint, collision, steps) in [
                (&ClientEndpointId::Local, &remote, 1),
                (&remote, &ClientEndpointId::Local, 2),
            ] {
                for _ in 0..steps {
                    preview_key(&mut state, b"\x1b[B");
                }
                assert_selected(&state, endpoint, "ws_2");
                let buffer = state
                    .compose(cols, 28)
                    .unwrap()
                    .to_ratatui_buffer()
                    .unwrap();
                let selected = workspace_rect(&state, endpoint, "ws_2");
                let other = workspace_rect(&state, collision, "ws_2");
                let focused = workspace_rect(&state, &ClientEndpointId::Local, "ws_1");
                let palette = &state.config.palette;
                let color = if cols == 44 {
                    palette.surface0
                } else {
                    palette.selection_bg
                };
                let color = if color == ratatui::style::Color::Reset {
                    palette.active_row_bg
                } else {
                    color
                };
                assert_eq!(buffer[(selected.x + 2, selected.y)].bg, color);
                assert_ne!(buffer[(other.x + 2, other.y)].bg, color);
                assert_eq!(
                    buffer[(focused.x + 2, focused.y)].bg,
                    if cols == 44 {
                        palette.surface_dim
                    } else {
                        palette.active_row_bg
                    }
                );
            }
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
            assert_eq!(state.pane_surface.as_ref().unwrap().boot_id, "boot-1");
            let enter = state.handle_input_bytes(b"\r");
            assert!(enter.requests.is_empty());
            assert!(
                matches!(enter.actions.as_slice(), [ClientShellAction::ActivateEndpoint {
                endpoint_id, target: Some(ClientEndpointFocusTarget::Workspace(id)),
            }] if endpoint_id == &remote && id == "ws_2")
            );
            assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
            assert_eq!(state.mode, ClientShellMode::Terminal);
            assert!(state.navigate_workspace_id.is_none());
        }
    }
}

#[test]
fn foreign_preview_blocks_keyboard_actions_but_keeps_active_action_context() {
    let (mut state, remote) = state_with_remote();
    state.compose(100, 28).unwrap();
    enter_navigation(&mut state);
    preview_key(&mut state, b"\x1b[B");
    for confirm in [false, true] {
        state.config.confirm_close = confirm;
        for key in [
            b"W".as_slice(),
            b"D",
            b"\x1b[D",
            b"\x1b[C",
            b"\t",
            b"1",
            b"c",
            b"N",
        ] {
            preview_key(&mut state, key);
            assert!(state.overlay.is_none());
            assert_eq!(state.mode, ClientShellMode::Navigate);
        }
    }
    assert_selected(&state, &remote, "ws_1");
    let mut remote_snapshot = workspaces(2);
    remote_snapshot.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote, Box::new(remote_snapshot));
    preview_key(&mut state, b"\x1b[B");
    assert_selected(&state, &remote, "ws_2");
    assert_eq!(state.workspace_action_id().as_deref(), Some("ws_1"));
    state.config.prompt_new_workspace_name = false;
    let mut create = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorkspace),
        &mut create,
    );
    assert!(
        matches!(create.actions.as_slice(), [ClientShellAction::Endpoint { endpoint_id: ClientEndpointId::Local, request, .. }]
        if matches!(&request.method, crate::api::schema::Method::WorkspaceCreate(params) if params.source_workspace_id.as_deref() == Some("ws_1")))
    );
    preview_key(&mut state, b"\x1b");
    assert!(state.navigate_workspace_id.is_none());
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
    assert!(state.activate_endpoint_projection(&remote));
    enter_navigation(&mut state);
    preview_key(&mut state, b"W");
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Rename(_))));
}

#[test]
fn empty_workspace_navigation_enter_exits_without_focusing() {
    let (mut state, _) = state_with_remote();
    let mut empty = workspaces(0);
    empty.tabs.clear();
    empty.panes.clear();
    empty.focused_workspace_id = None;
    empty.focused_tab_id = None;
    empty.focused_pane_id = None;
    state.set_snapshot(Box::new(empty));
    enter_navigation(&mut state);
    assert!(state.navigate_workspace_id.is_none());
    let enter = state.handle_input_bytes(b"\r");
    assert!(enter.actions.is_empty() && enter.requests.is_empty() && enter.repaint);
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn foreign_workspace_preview_blocks_paste_into_hidden_copy_search() {
    let (mut state, _) = state_with_remote();
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(100, 28).unwrap();
    assert!(state.enter_copy_mode(&mut ClientShellInput::default()));
    state.copy_mode.as_mut().unwrap().search_prompt = Some(ClientCopySearchPrompt {
        direction: crate::api::schema::PaneCopySearchDirection::Forward,
        query: "original".into(),
    });
    enter_navigation(&mut state);
    preview_key(&mut state, b"\x1b[B");
    assert!(state.workspace_preview_action_blocked());
    assert!(!state.modal_paste_target_active());
    let key = crate::input::TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert!(!state.handle_modal_paste_shortcut_with(
        &key,
        &mut ClientShellInput::default(),
        || { panic!("hidden search must not read the clipboard") }
    ));
    let paste = state.handle_raw_events(vec![RawInputEvent::Paste("unexpected".into())]);
    assert!(paste.actions.is_empty() && paste.requests.is_empty());
    assert_eq!(
        state.copy_mode.unwrap().search_prompt.unwrap().query,
        "original"
    );
}

#[test]
fn mouse_clicks_cancel_remote_workspace_navigation() {
    for pane in [false, true] {
        let (mut state, _) = state_with_remote();
        state.compose(100, 28).unwrap();
        enter_navigation(&mut state);
        preview_key(&mut state, b"\x1b[B");
        state.compose(100, 28).unwrap();
        let rect = if pane {
            state.hits.panes[0].inner_rect
        } else {
            workspace_rect(&state, &ClientEndpointId::Local, "ws_1")
        };
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                kind,
                column: rect.x + 2,
                row: rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        }
        assert_eq!(state.mode, ClientShellMode::Terminal);
        assert!(state.navigate_workspace_id.is_none());
        enter_navigation(&mut state);
        assert_selected(&state, &ClientEndpointId::Local, "ws_1");
    }
}

#[test]
fn single_machine_compact_navigation_includes_visible_collapsed_group_children() {
    let (mut state, _) = navigation_state(grouped_workspaces());
    state.set_endpoint_catalog(&[]);
    state.toggle_collapsed_group(&ClientEndpointId::Local, "repo".into());
    state.sidebar_collapsed = true;
    state.compose(100, 28).unwrap();
    workspace_rect(&state, &ClientEndpointId::Local, "ws_3");
    enter_navigation(&mut state);
    for id in ["ws_2", "ws_3"] {
        preview_key(&mut state, b"\x1b[B");
        assert_selected(&state, &ClientEndpointId::Local, id);
    }
}

#[test]
fn workspace_navigation_respects_each_machines_visible_worktree_groups() {
    for (cols, compact, unavailable, show_child) in [
        (100, false, false, false),
        (100, true, false, true),
        (44, false, false, true),
        (44, true, true, false),
    ] {
        let (mut state, remote) = navigation_state(grouped_workspaces());
        state.toggle_collapsed_group(&remote, "repo".into());
        state.sidebar_collapsed = compact;
        if unavailable {
            state.pane_surface = None;
        }
        state.compose(cols, 28).unwrap();
        enter_navigation(&mut state);
        let local = if compact && !unavailable {
            ["ws_2", "ws_3"]
        } else {
            ["ws_3", "ws_2"]
        };
        let remote_ids: &[&str] = if !show_child {
            &["ws_1", "ws_2"]
        } else if compact {
            &["ws_1", "ws_2", "ws_3"]
        } else {
            &["ws_1", "ws_3", "ws_2"]
        };
        for (endpoint, ids) in [
            (&ClientEndpointId::Local, local.as_slice()),
            (&remote, remote_ids),
        ] {
            for id in ids {
                preview_key(&mut state, b"\x1b[B");
                assert_selected(&state, endpoint, id);
                state.compose(cols, 28).unwrap();
                workspace_rect(&state, endpoint, id);
            }
        }
        assert!(state.group_is_collapsed(&remote, "repo"));
        assert!(!state.group_is_collapsed(&ClientEndpointId::Local, "repo"));
    }
}

#[test]
fn foreign_preview_survives_local_updates_and_rejects_stale_enter() {
    for invalidation in [
        "offline",
        "disabled",
        "removed",
        "deleted",
        "boot",
        "generation",
    ] {
        let (mut state, remote_id) = state_with_remote();
        let mut remote = workspaces(2);
        remote.boot_id = "remote-boot".into();
        state.set_endpoint_snapshot_for_generation(&remote_id, 7, Box::new(remote.clone()));
        state.compose(100, 28).unwrap();
        enter_navigation(&mut state);
        for _ in 0..2 {
            preview_key(&mut state, b"\x1b[B");
        }
        assert_selected(&state, &remote_id, "ws_2");
        let selected = state.navigate_workspace_id.clone();
        remote.revision += 1;
        state.set_endpoint_snapshot_for_generation(&remote_id, 7, Box::new(remote.clone()));
        assert_eq!(state.navigate_workspace_id, selected);
        assert!(state.navigation_target_valid(selected.as_ref().unwrap()));
        let mut local = snapshot();
        local.revision += 1;
        state.set_snapshot(Box::new(local));
        assert_eq!(state.navigate_workspace_id, selected);
        match invalidation {
            "offline" => state.set_endpoint_status(&remote_id, ClientEndpointStatus::Reconnecting),
            "disabled" => {
                let mut profile = remote_profile();
                profile.enabled = false;
                state.set_endpoint_catalog(&[profile]);
            }
            "removed" => state.set_endpoint_catalog(&[]),
            "deleted" => {
                remote.revision += 1;
                remote.workspaces.pop();
                state.set_endpoint_snapshot_for_generation(&remote_id, 7, Box::new(remote));
            }
            "boot" => {
                remote.boot_id = "restarted-remote".into();
                state.set_endpoint_snapshot_for_generation(&remote_id, 7, Box::new(remote));
            }
            "generation" => {
                state.cache_endpoint_snapshot_for_generation(&remote_id, 8, Box::new(remote))
            }
            _ => unreachable!(),
        }
        preview_key(&mut state, b"\r");
        assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
        assert_eq!(state.mode, ClientShellMode::Navigate);
        assert!(state.visible_endpoint_notice.is_some());
        assert!(!state.navigation_target_valid(state.navigate_workspace_id.as_ref().unwrap()));
        preview_key(&mut state, b"\x1b[B");
        assert!(state.navigation_target_valid(state.navigate_workspace_id.as_ref().unwrap()));
    }
}

#[test]
fn navigation_uses_displayed_group_order_when_local_is_unavailable() {
    for cols in [100, 44] {
        let (mut state, remote) = navigation_state(grouped_workspaces());
        state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
        state.select_unavailable_local();
        state.sidebar_collapsed = true;
        state.compose(cols, 18).unwrap();
        enter_navigation(&mut state);
        for id in ["ws_1", "ws_3", "ws_2"] {
            preview_key(&mut state, b"\x1b[B");
            assert_selected(&state, &remote, id);
            state.compose(cols, 18).unwrap();
            workspace_rect(&state, &remote, id);
        }
        let enter = state.handle_input_bytes(b"\r");
        assert!(
            matches!(enter.actions.as_slice(), [ClientShellAction::ActivateEndpoint {
            endpoint_id, target: Some(ClientEndpointFocusTarget::Workspace(id)),
        }] if endpoint_id == &remote && id == "ws_2")
        );
    }
}

#[test]
fn active_preview_is_not_retargeted_by_deletion_or_reboot() {
    for invalidation in ["deleted", "boot", "generation"] {
        let (mut state, _) = state_with_remote();
        let mut local = workspaces(2);
        state.set_endpoint_snapshot_for_generation(
            &ClientEndpointId::Local,
            7,
            Box::new(local.clone()),
        );
        state.compose(100, 28).unwrap();
        enter_navigation(&mut state);
        preview_key(&mut state, b"\x1b[B");
        assert_selected(&state, &ClientEndpointId::Local, "ws_2");
        let selected = state.navigate_workspace_id.clone();
        match invalidation {
            "boot" => local.boot_id = "new-local-boot".into(),
            "deleted" => {
                local.revision += 1;
                local.workspaces.pop();
            }
            _ => {}
        }
        let generation = if invalidation == "generation" { 8 } else { 7 };
        state.set_endpoint_snapshot_for_generation(
            &ClientEndpointId::Local,
            generation,
            Box::new(local),
        );
        assert_eq!(state.navigate_workspace_id, selected);
        preview_key(&mut state, b"\r");
        assert_eq!(state.mode, ClientShellMode::Navigate);
        assert!(state.visible_endpoint_notice.is_some());
        for confirm in [false, true] {
            state.config.confirm_close = confirm;
            for key in [b"W", b"D"] {
                preview_key(&mut state, key);
            }
            assert!(state.overlay.is_none());
            assert_eq!(state.mode, ClientShellMode::Navigate);
        }
        assert_eq!(state.workspace_action_id().as_deref(), Some("ws_1"));
    }
}

#[test]
fn aggregate_navigation_reveals_overflow_and_preserves_order() {
    for (compact, cols) in [(true, 100), (false, 100), (false, 44)] {
        let (mut state, remote_id) = state_with_remote();
        let mut remote = workspaces(15);
        remote.boot_id = "remote-boot".into();
        state.set_endpoint_snapshot(&remote_id, Box::new(remote));
        state.sidebar_collapsed = compact;
        state.collapsed_endpoints.insert(remote_id.clone());
        state.compose(cols, 18).unwrap();
        enter_navigation(&mut state);
        for number in 1..=15 {
            preview_key(&mut state, b"\x1b[B");
            let id = format!("ws_{number}");
            assert_selected(&state, &remote_id, &id);
            state.compose(cols, 18).unwrap();
            workspace_rect(&state, &remote_id, &id);
        }
        assert!(!state.collapsed_endpoints.contains(&remote_id));
        preview_key(&mut state, b"\x1b[B");
        if cols == 44 {
            assert_selected(&state, &remote_id, "ws_15");
        } else {
            assert_selected(&state, &ClientEndpointId::Local, "ws_1");
        }
        preview_key(&mut state, b"\x1b[A");
        assert_selected(
            &state,
            &remote_id,
            if cols == 44 { "ws_14" } else { "ws_15" },
        );
        state.set_endpoint_status(&remote_id, ClientEndpointStatus::Reconnecting);
        preview_key(&mut state, b"\x1b[B");
        assert_selected(&state, &ClientEndpointId::Local, "ws_1");
    }
}
