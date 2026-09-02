use super::*;

#[test]
fn clipboard_image_targets_the_focused_pane_or_active_popup() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));

    assert_eq!(
        state.clipboard_image_target(),
        Some(crate::protocol::ClientClipboardImageTarget::Pane(
            "pane_1".into()
        ))
    );

    state.mode = ClientShellMode::Prefix;
    assert_eq!(state.clipboard_image_target(), None);
    state.mode = ClientShellMode::Terminal;
    state.overlay = Some(ClientShellOverlay::Onboarding);
    assert_eq!(state.clipboard_image_target(), None);
    state.overlay = None;

    state.set_pane_surface(surface_with_popup());
    assert_eq!(
        state.clipboard_image_target(),
        Some(crate::protocol::ClientClipboardImageTarget::Popup(
            "terminal-popup".into()
        ))
    );
    state.overlay = Some(ClientShellOverlay::Onboarding);
    assert_eq!(state.clipboard_image_target(), None);
}

#[test]
fn modal_paste_target_requires_a_focused_editable_client_field() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    assert!(!state.modal_paste_target_active());

    state.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
        title: "rename pane",
        input: String::new(),
        replace_on_type: false,
        target: ClientRenameTarget::Pane {
            pane_id: "pane_1".into(),
        },
    }));
    assert!(state.modal_paste_target_active());

    state.overlay = Some(ClientShellOverlay::WorktreeCreate(
        ClientWorktreeCreateOverlay {
            source_workspace_id: "ws_1".into(),
            repo_name: "repo".into(),
            branch: String::new(),
            checkout_path: String::new(),
            replace_on_type: false,
            error: None,
            creating: true,
        },
    ));
    assert!(!state.modal_paste_target_active());
    if let Some(ClientShellOverlay::WorktreeCreate(create)) = state.overlay.as_mut() {
        create.creating = false;
    }
    assert!(state.modal_paste_target_active());

    state.overlay = Some(ClientShellOverlay::WorktreeOpen(
        ClientWorktreeOpenOverlay {
            source_workspace_id: "ws_1".into(),
            entries: Vec::new(),
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
            opening: false,
        },
    ));
    assert!(!state.modal_paste_target_active());
    if let Some(ClientShellOverlay::WorktreeOpen(open)) = state.overlay.as_mut() {
        open.search_focused = true;
    }
    assert!(state.modal_paste_target_active());

    state.overlay = Some(ClientShellOverlay::Navigator(ClientNavigatorOverlay {
        query: String::new(),
        search_focused: false,
        selected: 0,
        scroll: 0,
        filter: None,
        expanded_workspaces: HashSet::new(),
    }));
    assert!(!state.modal_paste_target_active());
    if let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() {
        navigator.search_focused = true;
    }
    assert!(state.modal_paste_target_active());

    state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
        query: String::new(),
        search_focused: false,
        scroll: 0,
    }));
    assert!(!state.modal_paste_target_active());
    if let Some(ClientShellOverlay::Help(help)) = state.overlay.as_mut() {
        help.search_focused = true;
    }
    assert!(state.modal_paste_target_active());

    state.overlay = None;
    state.copy_mode = Some(ClientCopyModeState {
        pane_id: "pane_1".into(),
        content_revision: 0,
        geometry: (80, 24),
        cursor: crate::api::schema::PaneTextPoint { row: 0, col: 0 },
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        entry_offset_from_bottom: 0,
        selection: None,
        search_prompt: Some(ClientCopySearchPrompt {
            direction: crate::api::schema::PaneCopySearchDirection::Forward,
            query: String::new(),
        }),
        search_query: String::new(),
        search_direction: None,
        search_matches: Vec::new(),
        search_total: 0,
        search_current: None,
        search_current_global: None,
        search_generation: 0,
        copy_after_search: false,
    });
    assert!(state.modal_paste_target_active());
    state.popup_pending = true;
    assert!(!state.modal_paste_target_active());
}

#[test]
fn non_overlay_ctrl_v_is_forwarded_to_the_focused_pane() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let key = crate::input::TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL);

    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(key)]);

    assert!(matches!(
        &outcome.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, events }]
            if pane_id == "pane_1"
                && matches!(&events[..], [ClientPaneInputEvent::Key { .. }])
    ));
}

#[test]
fn desktop_composition_keeps_shell_outside_origin_relative_surface() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
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
    assert!(text.contains("spaces"));
    assert!(text.contains("client-shell"));
    assert!(text.contains("main"));
    assert!(text.contains("LIVE"));
    assert!(!text.contains("1 1"));
    assert_eq!(
        frame.cursor.as_ref().map(|cursor| (cursor.x, cursor.y)),
        Some((27, 2))
    );
}

#[test]
fn client_composes_popup_terminal_content_inside_client_owned_chrome() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface_with_popup());

    let frame = state.compose(106, 20).expect("popup frame");
    let popup = state.hits.popup.as_ref().expect("popup hit geometry");
    assert_eq!(popup.rect.width, 12);
    assert_eq!(popup.rect.height, 5);
    assert_eq!(popup.inner_rect.width, 9);
    assert_eq!(popup.inner_rect.height, 3);
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
    assert!(text.contains("popup tit"));
    assert!(text.contains("popup-liv"));
    assert_eq!(
        frame.cursor.as_ref().map(|cursor| (cursor.x, cursor.y)),
        Some((popup.inner_rect.x + 2, popup.inner_rect.y + 1))
    );
}

#[test]
fn popup_owns_keys_text_paste_and_mouse_before_shell_controls() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface_with_popup());
    state.compose(106, 20).expect("popup frame");

    for bytes in [b"x".as_slice(), b"\x02".as_slice(), b"\x1b".as_slice()] {
        let input = state.handle_input_bytes(bytes);
        assert!(matches!(
            &input.requests[..],
            [ClientMessage::ClientShellPopupInput { terminal_id, .. }]
                if terminal_id == "terminal-popup"
        ));
        assert_eq!(state.mode, ClientShellMode::Terminal);
    }

    let text = state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "ime",
    ))]);
    assert!(matches!(
        &text.requests[..],
        [ClientMessage::ClientShellPopupInput { events, .. }]
            if matches!(&events[..], [ClientPaneInputEvent::TextCommit(value)] if value == "ime")
    ));
    let paste = state.handle_raw_events(vec![RawInputEvent::Paste("paste".into())]);
    assert!(matches!(
        &paste.requests[..],
        [ClientMessage::ClientShellPopupInput { events, .. }]
            if matches!(&events[..], [ClientPaneInputEvent::Paste(value)] if value == "paste")
    ));

    let popup = state.hits.popup.as_ref().expect("popup hit").clone();
    let mouse = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: popup.inner_rect.x + 3,
        row: popup.inner_rect.y + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &mouse.requests[..],
        [ClientMessage::ClientShellPopupInput { events, .. }]
            if matches!(
                &events[..],
                [ClientPaneInputEvent::Mouse {
                    position: ClientMousePosition::Cell { column: 3, row: 1 },
                    ..
                }]
            )
    ));
    assert!(state.pane_mouse_gesture.is_some());
    state.set_pane_surface(surface());
    assert!(state.pane_mouse_gesture.is_none());
}

#[test]
fn popup_transition_dismisses_client_overlays_and_restores_pane_input_after_close() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::Help),
        &mut ClientShellInput::default(),
    );
    assert!(state.overlay.is_some());

    state.set_pane_surface(surface_with_popup());
    assert!(state.overlay.is_none());
    assert_eq!(state.mode, ClientShellMode::Terminal);
    let popup_input = state.handle_input_bytes(b"p");
    assert!(matches!(
        &popup_input.requests[..],
        [ClientMessage::ClientShellPopupInput { .. }]
    ));

    state.set_pane_surface(surface());
    let pane_input = state.handle_input_bytes(b"p");
    assert!(matches!(
        &pane_input.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
    ));
}

#[test]
fn popup_target_survives_surface_invalidation_during_resize() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface_with_popup());
    state.invalidate_pane_surface();

    assert!(matches!(
        &state.handle_input_bytes(b"x").requests[..],
        [ClientMessage::ClientShellPopupInput { terminal_id, .. }]
            if terminal_id == "terminal-popup"
    ));
    let mouse = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(mouse.requests.is_empty());
    assert!(mouse.actions.is_empty());
}

#[test]
fn popup_close_reprocesses_held_key_repeats_into_the_focused_pane() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface_with_popup());

    let press = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('x'),
        KeyModifiers::empty(),
    ))]);
    assert!(matches!(
        &press.requests[..],
        [ClientMessage::ClientShellPopupInput { .. }]
    ));

    state.set_pane_surface(surface());
    let repeat = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty())
            .with_kind(crossterm::event::KeyEventKind::Repeat),
    )]);
    assert!(matches!(
        &repeat.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
    ));
}

#[test]
fn pending_popup_suppresses_held_pane_repeats_but_preserves_release() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let key = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty());
    assert!(matches!(
        &state
            .handle_raw_events(vec![RawInputEvent::Key(key.clone())])
            .requests[..],
        [ClientMessage::ClientShellPaneInput { .. }]
    ));

    state.popup_pending = true;
    let repeat = state.handle_raw_events(vec![RawInputEvent::Key(
        key.clone()
            .with_kind(crossterm::event::KeyEventKind::Repeat),
    )]);
    assert!(repeat.requests.is_empty());
    let release = state.handle_raw_events(vec![RawInputEvent::Key(
        key.with_kind(crossterm::event::KeyEventKind::Release),
    )]);
    assert!(matches!(
        &release.requests[..],
        [ClientMessage::ClientShellPaneInput { events, .. }]
            if matches!(
                &events[..],
                [ClientPaneInputEvent::Key {
                    kind: crate::protocol::ClientKeyKind::Release,
                    ..
                }]
            )
    ));
}

#[test]
fn prefix_input_source_changes_are_client_owned_and_focus_safe() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.switch_ascii_input_source_in_prefix = true;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    assert!(state.take_input_source_changes().is_empty());

    let prefix = crate::input::TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
    state.handle_raw_events(vec![RawInputEvent::Key(prefix)]);
    assert_eq!(state.take_input_source_changes(), vec![true]);

    state.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
    assert!(state.take_input_source_changes().is_empty());
    let escape = crate::input::TerminalKey::new(KeyCode::Esc, KeyModifiers::empty());
    state.handle_raw_events(vec![RawInputEvent::Key(escape.clone())]);
    assert!(state.take_input_source_changes().is_empty());
    state.handle_raw_events(vec![RawInputEvent::OuterFocusGained]);
    assert_eq!(state.take_input_source_changes(), vec![false]);

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
    ))]);
    assert_eq!(state.take_input_source_changes(), vec![true]);
    state.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
    state.handle_raw_events(vec![RawInputEvent::OuterFocusGained]);
    assert!(state.take_input_source_changes().is_empty());
    state.handle_raw_events(vec![RawInputEvent::Key(escape)]);
    assert_eq!(state.take_input_source_changes(), vec![false]);
}

#[test]
fn focus_loss_releases_held_pane_keys_before_reporting_focus() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let key = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty())
        .with_generated_text(Some("x".to_owned()))
        .with_windows_record(crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 0x58,
            virtual_scan_code: 0x2d,
            unicode: 'x' as u16,
            control_key_state: 0,
        });
    let press = state.handle_raw_events(vec![RawInputEvent::Key(key)]);
    assert!(matches!(
        &press.requests[..],
        [ClientMessage::ClientShellPaneInput { .. }]
    ));

    let lost = state.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
    assert!(matches!(
        &lost.requests[..],
        [
            ClientMessage::ClientShellPaneInput { events, .. },
            ClientMessage::ClientShellFocus { focused: false }
        ] if matches!(
            &events[..],
            [ClientPaneInputEvent::Key {
                kind: crate::protocol::ClientKeyKind::Release,
                ..
            }]
        )
    ));
    assert!(state.input_leases.is_empty());
}

#[test]
fn focus_loss_releases_active_pane_mouse_before_reporting_focus() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].mouse_reporting = true;
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("pane frame");
    let pane = state.hits.panes[0].clone();

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x + 2,
        row: pane.inner_rect.y + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &down.requests[..],
        [ClientMessage::ClientShellPaneInput { .. }]
    ));
    assert!(state.pane_mouse_gesture.is_some());

    let lost = state.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
    assert!(matches!(
        &lost.requests[..],
        [
            ClientMessage::ClientShellPaneInput { pane_id, events },
            ClientMessage::ClientShellFocus { focused: false }
        ] if pane_id == "pane_1" && matches!(
            &events[..],
            [ClientPaneInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Up(
                    crate::protocol::ClientMouseButton::Left
                ),
                position: ClientMousePosition::Cell { column: 2, row: 1 },
                ..
            }]
        )
    ));
    assert!(state.pane_mouse_gesture.is_none());
}

#[test]
fn focus_gain_reports_focus_and_honors_redraw_policy() {
    let mut config = Config::default();
    config.ui.redraw_on_focus_gained = false;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let gained = state.handle_raw_events(vec![RawInputEvent::OuterFocusGained]);
    assert!(!gained.repaint);
    assert!(gained.query_host_appearance);
    assert!(matches!(
        &gained.requests[..],
        [ClientMessage::ClientShellFocus { focused: true }]
    ));
}

#[test]
fn pane_mouse_release_survives_popup_open_transition() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].mouse_reporting = true;
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("pane frame");
    let pane = state.hits.panes[0].clone();

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x,
        row: pane.inner_rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &down.requests[..],
        [ClientMessage::ClientShellPaneInput { .. }]
    ));
    assert!(state.pane_mouse_gesture.is_some());

    state.set_pane_surface(surface_with_popup());
    let up = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &up.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, events }]
            if pane_id == "pane_1"
                && matches!(
                    &events[..],
                    [ClientPaneInputEvent::Mouse {
                        kind: crate::protocol::ClientMouseKind::Up(
                            crate::protocol::ClientMouseButton::Left
                        ),
                        ..
                    }]
                )
    ));
    assert!(state.pane_mouse_gesture.is_none());
}

#[test]
fn popup_command_blocks_underlying_input_until_surface_or_error() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let binding = crate::config::CustomCommandKeybind {
        bindings: crate::config::ActionKeybinds::prefix("t"),
        label: "prefix+t".into(),
        command: "secret-popup-command".into(),
        action: crate::config::CustomCommandAction::Popup,
        description: None,
        width: None,
        height: None,
    };
    let mut projection = snapshot();
    projection
        .commands
        .push(crate::protocol::ClientShellCommand {
            command_id: "cmd_popup".into(),
            binding_label: binding.label.clone(),
            binding_labels: binding.bindings.labels(),
            action: crate::protocol::ClientShellCommandAction::Popup,
            description: None,
        });
    state.set_snapshot(Box::new(projection));
    state.set_pane_surface(surface());

    let mut invoke = ClientShellInput::default();
    state.record_binding(crate::input::KeybindMatch::Command(binding), &mut invoke);
    assert!(state.popup_pending);
    assert!(state
        .handle_input_bytes(b"not-for-pane")
        .requests
        .is_empty());
    assert!(state
        .handle_raw_events(vec![RawInputEvent::Paste("secret".into())])
        .requests
        .is_empty());

    let request_id = match &invoke.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => request.id.clone(),
        other => panic!("expected popup command request, got {other:?}"),
    };
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("command_failed".into()),
            message: "popup failed".into(),
        }),
    );
    assert!(repaint);
    assert!(!state.popup_pending);
    assert!(matches!(
        &state.handle_input_bytes(b"p").requests[..],
        [ClientMessage::ClientShellPaneInput { .. }]
    ));

    let binding = crate::config::CustomCommandKeybind {
        bindings: crate::config::ActionKeybinds::prefix("t"),
        label: "prefix+t".into(),
        command: "secret-popup-command".into(),
        action: crate::config::CustomCommandAction::Popup,
        description: None,
        width: None,
        height: None,
    };
    let mut invoke = ClientShellInput::default();
    state.record_binding(crate::input::KeybindMatch::Command(binding), &mut invoke);
    let request_id = match &invoke.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => request.id.clone(),
        other => panic!("expected popup command request, got {other:?}"),
    };
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::Ok {}),
    );
    assert!(state.popup_pending);
    assert!(state
        .handle_input_bytes(b"still-blocked")
        .requests
        .is_empty());
    let deadline = state.popup_pending_deadline.expect("pending timeout");
    state.tick_popup_pending(deadline);
    assert!(!state.popup_pending);
}

#[test]
fn shell_refuses_mismatched_projection_and_clears_stale_hits_in_either_order() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("initial frame");
    assert!(!state.hits.panes.is_empty());

    let mut replacement = snapshot();
    replacement.revision = 2;
    state.set_snapshot(Box::new(replacement));
    assert!(state.hits.panes.is_empty());
    assert!(state.compose(106, 20).is_none());

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("initial frame");
    let mut replacement_surface = surface();
    replacement_surface.projection_revision = 2;
    state.set_pane_surface(replacement_surface);
    assert!(state.hits.panes.is_empty());
    assert!(state.compose(106, 20).is_none());
}

#[test]
fn shell_ignores_older_same_boot_snapshot_and_surface() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut current_snapshot = snapshot();
    current_snapshot.revision = 2;
    current_snapshot.workspaces[0].label = "current".into();
    state.set_snapshot(Box::new(current_snapshot));
    let mut current_surface = surface();
    current_surface.projection_revision = 2;
    current_surface.frame.cells[0].symbol = "N".into();
    state.set_pane_surface(current_surface);
    state.compose(106, 20).expect("current shell");
    assert!(!state.hits.panes.is_empty());
    let held_key = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty());
    assert!(matches!(
        &state
            .handle_raw_events(vec![RawInputEvent::Key(held_key.clone())])
            .requests[..],
        [ClientMessage::ClientShellPaneInput { .. }]
    ));

    let mut stale_snapshot = snapshot();
    stale_snapshot.workspaces[0].label = "stale".into();
    state.set_snapshot(Box::new(stale_snapshot));
    let mut stale_surface = surface();
    stale_surface.frame.cells[0].symbol = "O".into();
    state.set_pane_surface(stale_surface);

    let installed_snapshot = state.snapshot.as_deref().expect("current snapshot");
    assert_eq!(installed_snapshot.revision, 2);
    assert_eq!(installed_snapshot.workspaces[0].label, "current");
    let installed_surface = state.pane_surface.as_ref().expect("current pane surface");
    assert_eq!(installed_surface.projection_revision, 2);
    assert_eq!(installed_surface.frame.cells[0].symbol, "N");

    let mut ahead_surface = surface();
    ahead_surface.projection_revision = 4;
    ahead_surface.frame.cells[0].symbol = "A".into();
    state.set_pane_surface(ahead_surface);
    let mut delayed_surface = surface();
    delayed_surface.projection_revision = 3;
    delayed_surface.frame.cells[0].symbol = "D".into();
    state.set_pane_surface(delayed_surface);
    let installed_surface = state.pane_surface.as_ref().expect("newest pane surface");
    assert_eq!(installed_surface.projection_revision, 4);
    assert_eq!(installed_surface.frame.cells[0].symbol, "A");

    let mut replacement_boot = snapshot();
    replacement_boot.boot_id = "boot-2".into();
    state.set_snapshot(Box::new(replacement_boot));
    assert!(state.pane_surface.is_none());
    assert!(state.hits.panes.is_empty());
    let release = state.handle_raw_events(vec![RawInputEvent::Key(
        held_key.with_kind(crossterm::event::KeyEventKind::Release),
    )]);
    assert!(
        release.requests.is_empty(),
        "boot replacement must discard held input leases"
    );
    let mut prior_boot_surface = surface();
    prior_boot_surface.projection_revision = u64::MAX;
    state.set_pane_surface(prior_boot_surface);
    assert!(
        state.pane_surface.is_none(),
        "an old endpoint surface must not cross the boot boundary"
    );
}

#[test]
fn resize_invalidation_drops_stale_hits_but_preserves_gesture_release() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].mouse_reporting = true;
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x + 1,
        row: pane.inner_rect.y + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.pane_mouse_gesture.is_some());

    state.invalidate_pane_surface();
    assert!(state.pane_surface.is_none());
    assert!(state.hits.panes.is_empty());
    let stale_click =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 27,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(stale_click.requests.is_empty());
    assert!(stale_click.actions.is_empty());

    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: pane.inner_rect.x + 1,
            row: pane.inner_rect.y + 1,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &release.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, events }]
            if pane_id == "pane_1"
                && matches!(
                    &events[..],
                    [ClientPaneInputEvent::Mouse {
                        kind: crate::protocol::ClientMouseKind::Up(
                            crate::protocol::ClientMouseButton::Left
                        ),
                        ..
                    }]
                )
    ));
    assert!(state.pane_mouse_gesture.is_none());
}

#[test]
fn pane_scrollbar_track_and_thumb_use_stable_endpoint_scroll_requests() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scrollbar_rect = Some(SurfaceRect {
        x: 3,
        y: 0,
        width: 1,
        height: 2,
    });
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();
    let track = pane.scrollbar_rect.expect("scrollbar track");
    let metrics = pane.scroll.expect("scroll metrics");

    let track_click =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: track.x,
            row: track.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let expected = crate::ui::scrollbar_offset_from_row(metrics, track, track.y);
    assert!(track_click.requests.is_empty());
    assert!(matches!(
        &track_click.actions[..],
        [
            ClientShellAction::Endpoint { request: focus, .. },
            ClientShellAction::Endpoint { request: scroll, .. }
        ] if matches!(
            &focus.method,
            crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
        ) && matches!(
            &scroll.method,
            crate::api::schema::Method::PaneScroll(params)
                if params.pane_id == "pane_1"
                    && params.offset_from_bottom == expected as u64
        )
    ));
    let track_scroll_id = match &track_click.actions[1] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    state.handle_endpoint_result(
        "boot-1",
        &track_scroll_id,
        Ok(pane_scroll_result(expected as u64, 20, 2)),
    );

    let thumb = crate::ui::scrollbar_thumb(metrics, track).expect("scrollbar thumb");
    let thumb_down =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: track.x,
            row: thumb.top,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &thumb_down.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
            )
    ));
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::PaneScrollbar { .. })
    ));

    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: track.x,
        row: track.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let expected = crate::ui::scrollbar_offset_from_drag_row(metrics, track, track.y, 0);
    assert!(matches!(
        &drag.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneScroll(params)
                    if params.pane_id == "pane_1"
                        && params.offset_from_bottom == expected as u64
            )
    ));
    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(release.actions.is_empty());
    assert!(state.chrome_drag.is_none());
}

#[test]
fn edit_scrollback_binding_targets_the_focused_endpoint_pane() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut input = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::EditScrollback),
        &mut input,
    );

    assert!(input.requests.is_empty());
    assert!(matches!(
        &input.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneEditScrollback(target)
                    if target.pane_id == "pane_1"
            )
    ));
}

#[test]
fn sidebar_scrollbars_use_proportional_shared_geometry_and_drag() {
    let mut projected = snapshot();
    for index in 2..=10 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index;
        workspace.label = format!("workspace-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    for index in 1..=10 {
        projected.agents.push(crate::protocol::ClientShellAgent {
            pane_id: format!("agent-pane-{index}"),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some(format!("agent-{index}")),
            display_agent: None,
            agent: Some("codex".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: index,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        });
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("overflowing sidebars");

    for agent in [false, true] {
        let (track, metrics) = if agent {
            (
                state.hits.agent_scrollbar,
                state.hits.agent_scroll_metrics.expect("agent metrics"),
            )
        } else {
            (
                state.hits.workspace_scrollbar,
                state
                    .hits
                    .workspace_scroll_metrics
                    .expect("workspace metrics"),
            )
        };
        assert!(track.width > 0);
        let thumb = crate::ui::scrollbar_thumb(metrics, track).expect("scrollbar thumb");
        assert!(thumb.len > 1);
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: track.x,
            row: thumb.top,
            modifiers: KeyModifiers::empty(),
        })]);
        let dragged =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: track.x,
                row: track.bottom().saturating_sub(1),
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(dragged.repaint);
        if agent {
            assert_eq!(state.agent_scroll, metrics.max_offset_from_bottom);
        } else {
            assert_eq!(state.workspace_scroll, metrics.max_offset_from_bottom);
        }
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: track.x,
            row: track.bottom().saturating_sub(1),
            modifiers: KeyModifiers::empty(),
        })]);
    }
}

#[test]
fn popup_preemption_cancels_settings_theme_preview() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.open_settings_overlay();
    let original_theme = state.config.theme_name.clone();
    let original_palette = state.config.palette.clone();

    state.handle_input_bytes(b"j");
    assert_ne!(state.config.theme_name, original_theme);
    assert_ne!(state.config.palette.accent, original_palette.accent);

    state.set_pane_surface(surface_with_popup());

    assert!(!matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(_))
    ));
    assert_eq!(state.config.theme_name, original_theme);
    assert_eq!(state.config.palette, original_palette);
}

#[test]
fn pending_scroll_target_does_not_relabel_an_older_surface() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface.clone());
    let mut outcome = ClientShellInput::default();
    state.push_pane_scroll_offset("pane_1".into(), 10, &mut outcome);
    state.set_pane_surface(pane_surface);
    assert_eq!(
        state
            .pane_surface
            .as_ref()
            .and_then(|surface| surface.panes[0].scroll)
            .map(|scroll| scroll.offset_from_bottom),
        Some(0)
    );
    assert_eq!(state.pane_scroll_targets.get("pane_1"), Some(&10));
}

#[test]
fn retained_surface_patch_updates_only_pane_cells_without_recomposing_chrome() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let pane_surface = surface();
    let mut updated_pane = pane_surface.panes[0].clone();
    updated_pane.content_revision = 2;
    state.set_pane_surface(pane_surface);
    let composed = state.compose(100, 30).expect("initial composed frame");
    let layout = state.layout(100, 30);
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: vec![
                crate::protocol::CellData {
                    symbol: "N".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                };
                4
            ],
        }],
        panes: vec![updated_pane],
        cursor: None,
    };

    let ClientPaneSurfacePatchOutcome::Applied(Some(patch)) = state.apply_pane_surface_patch(patch)
    else {
        panic!("expected fast retained patch");
    };
    let patched = apply_composed_surface_patch(&composed, patch).expect("apply composed patch");
    let pane_index = usize::from(layout.pane_surface.y) * usize::from(patched.width)
        + usize::from(layout.pane_surface.x);
    assert_eq!(patched.cells[pane_index].symbol, "N");
    assert_eq!(
        patched.cells[0], composed.cells[0],
        "sidebar chrome changed"
    );
    assert_eq!(state.pane_surface.as_ref().unwrap().surface_revision, 2);

    let mut forced_full = surface();
    forced_full.surface_revision = 3;
    forced_full.frame.cells[0].symbol = "F".into();
    state.set_pane_surface(forced_full);
    assert_eq!(state.pane_surface.as_ref().unwrap().surface_revision, 3);
    assert_eq!(
        state.pane_surface.as_ref().unwrap().frame.cells[0].symbol,
        "F"
    );
}

#[test]
fn retained_surface_patch_recomposes_client_owned_mode_and_diagnostic_rows() {
    for diagnostic in [false, true] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let pane_surface = surface();
        let mut updated_pane = pane_surface.panes[0].clone();
        updated_pane.content_revision = 2;
        state.set_pane_surface(pane_surface);
        state.compose(100, 30).expect("initial composed frame");
        if diagnostic {
            state.config_diagnostic = Some("invalid config".into());
        } else {
            state.mode = ClientShellMode::Prefix;
        }
        let patch = crate::protocol::PaneSurfacePatch {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            base_surface_revision: 1,
            surface_revision: 2,
            rows: Vec::new(),
            panes: vec![updated_pane],
            cursor: None,
        };

        assert!(matches!(
            state.apply_pane_surface_patch(patch),
            ClientPaneSurfacePatchOutcome::Applied(None)
        ));
    }
}

#[test]
fn retained_surface_patch_updates_scrollbar_cells_and_pane_hit_metadata() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["LIVE ", "PANE "]),
        None,
        &[],
    );
    pane_surface.panes[0].rect.width = 5;
    state.set_pane_surface(pane_surface.clone());
    let composed = state.compose(100, 30).expect("initial composed frame");
    let layout = state.layout(100, 30);
    let mut updated_pane = pane_surface.panes[0].clone();
    updated_pane.scrollbar_rect = Some(SurfaceRect {
        x: 4,
        y: 0,
        width: 1,
        height: 2,
    });
    updated_pane.scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 2,
        max_offset_from_bottom: 8,
        viewport_rows: 2,
    });
    updated_pane.mouse_reporting = true;
    updated_pane.sgr_pixel_mouse = true;
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 4,
            y: 0,
            cells: vec![crate::protocol::CellData {
                symbol: "▐".into(),
                fg: 0,
                bg: 0,
                modifier: 0,
                skip: false,
                hyperlink: None,
            }],
        }],
        panes: vec![updated_pane],
        cursor: None,
    };

    let ClientPaneSurfacePatchOutcome::Applied(Some(patch)) = state.apply_pane_surface_patch(patch)
    else {
        panic!("expected fast retained patch");
    };
    let patched = apply_composed_surface_patch(&composed, patch).expect("apply composed patch");
    let scrollbar_index = usize::from(layout.pane_surface.y) * usize::from(patched.width)
        + usize::from(layout.pane_surface.x + 4);
    assert_eq!(patched.cells[scrollbar_index].symbol, "▐");
    let hit = state
        .hits
        .panes
        .iter()
        .find(|hit| hit.pane_id == "pane_1")
        .expect("pane hit");
    assert_eq!(
        hit.scrollbar_rect,
        Some(Rect::new(
            layout.pane_surface.x + 4,
            layout.pane_surface.y,
            1,
            2,
        ))
    );
    assert_eq!(
        hit.scroll.map(|scroll| scroll.max_offset_from_bottom),
        Some(8)
    );
    assert!(hit.mouse_reporting);
    assert!(hit.sgr_pixel_mouse);
}

#[test]
fn retained_surface_patch_rejects_stale_base_without_mutating_surface() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let before = state.pane_surface.clone();
    let outcome = state.apply_pane_surface_patch(crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 0,
        surface_revision: 2,
        rows: Vec::new(),
        panes: Vec::new(),
        cursor: None,
    });
    assert!(matches!(outcome, ClientPaneSurfacePatchOutcome::Rejected));
    assert_eq!(state.pane_surface, before);
}
