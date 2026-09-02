use super::*;

#[test]
fn ctrl_click_routes_link_activation_through_endpoint_then_client_host() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("pane frame");
    let pane = state.hits.panes[0].clone();
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x + 2,
        row: pane.inner_rect.y + 1,
        modifiers: KeyModifiers::CONTROL,
    };
    let activate = state.handle_raw_events(vec![RawInputEvent::Mouse(down)]);
    let [ClientShellAction::Endpoint { request, .. }] = &activate.actions[..] else {
        panic!("expected link activation request");
    };
    let request_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneLinkActivate(params)
            if params.pane_id == "pane_1" && params.viewport_row == 1 && params.col == 2
    ));

    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        ..down
    };
    let held = state.handle_raw_events(vec![RawInputEvent::Mouse(up)]);
    assert!(held.requests.is_empty() && held.actions.is_empty());
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::PaneLinkActivated {
            url: Some("https://example.test".to_owned()),
            handled: false,
        }),
    );
    assert!(matches!(
        &actions[..],
        [ClientShellAction::OpenSafeWebUrl(url)] if url == "https://example.test"
    ));
    assert!(!state.url_click_consumes_until_up);
}

#[test]
fn ctrl_click_without_a_link_replays_the_original_gesture() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("pane frame");
    let pane = state.hits.panes[0].clone();
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x + 2,
        row: pane.inner_rect.y + 1,
        modifiers: KeyModifiers::CONTROL,
    };
    let activate = state.handle_raw_events(vec![RawInputEvent::Mouse(down)]);
    let request_id = match &activate.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => request.id.clone(),
        _ => panic!("expected link activation request"),
    };
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::PaneLinkActivated {
            url: None,
            handled: false,
        }),
    );
    assert!(matches!(
        &actions[..],
        [ClientShellAction::ReplayMouse(events)] if events == &vec![down]
    ));
    let replay = match actions.into_iter().next().expect("replay action") {
        ClientShellAction::ReplayMouse(events) => state.replay_mouse_events(events),
        _ => unreachable!(),
    };
    assert!(matches!(
        &replay.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::PaneFocus(_))
    ));
    assert!(state.selection.is_some());
}

#[test]
fn pane_split_drag_uses_projected_handle_and_stable_tab_path() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.splits.push(PaneSurfaceSplit {
        direction: PaneSurfaceSplitDirection::Horizontal,
        pos: 40,
        area: SurfaceRect {
            x: 0,
            y: 0,
            width: 80,
            height: 19,
        },
        hit_rect: SurfaceRect {
            x: 40,
            y: 0,
            width: 1,
            height: 19,
        },
        path: vec![false, true],
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("split pane surface");
    let split = state.hits.pane_splits[0].clone();

    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: split.hit_rect.x,
        row: split.hit_rect.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::PaneSplit { .. })
    ));
    let mut replacement = snapshot();
    replacement.revision = 2;
    replacement
        .tab_bar_right
        .push(crate::protocol::ClientShellTabStatusSegment {
            text: "updated".into(),
            accent: false,
        });
    let mut replacement_surface = surface();
    replacement_surface.projection_revision = 2;
    replacement_surface.splits.push(PaneSurfaceSplit {
        direction: PaneSurfaceSplitDirection::Horizontal,
        pos: 40,
        area: SurfaceRect {
            x: 0,
            y: 0,
            width: 80,
            height: 19,
        },
        hit_rect: SurfaceRect {
            x: 40,
            y: 0,
            width: 1,
            height: 19,
        },
        path: vec![false, true],
    });
    state.set_snapshot(Box::new(replacement));
    state.set_pane_surface(replacement_surface);
    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: split.area.x + 48,
        row: split.hit_rect.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &drag.actions[..] else {
        panic!("pane split drag should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::LayoutSetSplitRatio(params)
            if params.tab_id.as_deref() == Some("tab_1")
                && params.path == vec![false, true]
                && (params.ratio - 0.6).abs() < f32::EPSILON
    ));
    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: split.area.x + 48,
            row: split.hit_rect.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(release.actions.is_empty());
    assert!(state.chrome_drag.is_none());
}

#[test]
fn disabled_mouse_chrome_keeps_tab_wheel_but_removes_split_drag_hits() {
    let mut config = Config::default();
    config.ui.mouse_capture = false;
    let mut projected = snapshot();
    let mut second_tab = projected.tabs[0].clone();
    second_tab.tab_id = "tab_2".into();
    second_tab.number = 2;
    second_tab.label = "2".into();
    second_tab.focused = false;
    projected.tabs.push(second_tab);
    let mut pane_surface = surface();
    pane_surface.splits.push(PaneSurfaceSplit {
        direction: PaneSurfaceSplitDirection::Horizontal,
        pos: 40,
        area: SurfaceRect {
            x: 0,
            y: 0,
            width: 80,
            height: 19,
        },
        hit_rect: SurfaceRect {
            x: 40,
            y: 0,
            width: 1,
            height: 19,
        },
        path: Vec::new(),
    });
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("mouse-disabled shell");
    assert!(state.hits.pane_splits.is_empty());
    let first_tab = state.hits.tabs[0].0;
    let wheel = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: first_tab.x,
        row: first_tab.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &wheel.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_2"
            )
    ));
}

#[test]
fn client_double_click_selects_and_copies_endpoint_row_word() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();
    let click = || {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.inner_rect.x + 1,
            row: pane.inner_rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    let release = || {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: pane.inner_rect.x + 1,
            row: pane.inner_rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };

    state.handle_raw_events(vec![click()]);
    state.handle_raw_events(vec![release()]);
    let second = state.handle_raw_events(vec![click()]);
    let ClientShellAction::Endpoint { request, .. } = second
        .actions
        .iter()
        .find(|action| {
            matches!(
                action,
                ClientShellAction::Endpoint { request, .. }
                    if matches!(request.method, crate::api::schema::Method::PaneSelectionRead(_))
            )
        })
        .expect("word-row read")
    else {
        unreachable!()
    };
    let word_request_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneSelectionRead(params)
            if params.anchor == crate::api::schema::PaneTextPoint { row: 0, col: 0 }
                && params.cursor == crate::api::schema::PaneTextPoint { row: 0, col: 3 }
    ));

    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &word_request_id,
        Ok(crate::api::schema::ResponseResult::PaneSelection {
            pane_id: "pane_1".into(),
            text: "LIVE".into(),
        }),
    );
    assert!(repaint);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_finalized));
    let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
        panic!("auto-copy should read the selected word");
    };
    let copy_request_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneSelectionRead(params)
            if params.anchor.col == 0 && params.cursor.col == 3
    ));
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &copy_request_id,
        Ok(crate::api::schema::ResponseResult::PaneSelection {
            pane_id: "pane_1".into(),
            text: "LIVE".into(),
        }),
    );
    assert!(matches!(
        &actions[..],
        [ClientShellAction::ClipboardWrite(bytes)] if bytes == b"LIVE"
    ));
}

#[test]
fn pane_mouse_input_keeps_stable_target_and_endpoint_encoding() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].mouse_reporting = true;
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();

    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x + 2,
        row: pane.inner_rect.y + 1,
        modifiers: KeyModifiers::ALT,
    })]);
    let [ClientMessage::ClientShellPaneInput { pane_id, events }] = &click.requests[..] else {
        panic!("pane application click should use targeted canonical input");
    };
    assert_eq!(pane_id, "pane_1");
    assert!(matches!(
        &events[..],
        [ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Down(
                crate::protocol::ClientMouseButton::Left
            ),
            position: ClientMousePosition::Cell { column: 2, row: 1 },
            modifiers,
            ..
        }] if *modifiers == KeyModifiers::ALT.bits()
    ));
    let moved = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::ALT,
    })]);
    assert!(moved.requests.is_empty());
    assert!(state.pane_mouse_gesture.is_some());
    state.hits.panes.clear();
    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::ALT,
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
fn pane_pixel_mouse_preserves_pane_relative_pixel_coordinates() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].mouse_reporting = true;
    pane_surface.panes[0].sgr_pixel_mouse = true;
    pane_surface.panes[0].pixel_width = 39;
    pane_surface.panes[0].pixel_height = 38;
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();
    let geometry =
        crate::input::mouse::HostGeometry::new(106, 20, 1060, 400).expect("host geometry");
    let x = u32::from(pane.inner_rect.x) * 10 + 21;
    let y = u32::from(pane.inner_rect.y) * 20 + 21;
    let report = format!("\x1b[<0;{x};{y}M");

    let outcome = state.handle_pixel_mouse(report.as_bytes(), geometry);
    assert!(matches!(
        &outcome.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, events }]
            if pane_id == "pane_1"
                && matches!(
                    &events[..],
                    [ClientPaneInputEvent::Mouse {
                        kind: crate::protocol::ClientMouseKind::Down(
                            crate::protocol::ClientMouseButton::Left
                        ),
                        position: ClientMousePosition::Pixels { x: 20, y: 20, .. },
                        ..
                    }]
                )
    ));

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
                position: ClientMousePosition::Pixels { x: 20, y: 20, .. },
                ..
            }]
        )
    ));
}

#[test]
fn pane_owned_right_click_forwards_the_complete_gesture() {
    let mut snapshot = snapshot();
    snapshot.panes[0].right_click_passthrough = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    let mut pane_surface = surface();
    pane_surface.panes[0].mouse_reporting = true;
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: pane.inner_rect.x + 1,
        row: pane.inner_rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &down.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
    ));
    assert!(state.overlay.is_none());
    assert!(state.pane_mouse_gesture.is_some());

    let up = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Right),
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
                            crate::protocol::ClientMouseButton::Right
                        ),
                        ..
                    }]
                )
    ));
    assert!(state.pane_mouse_gesture.is_none());
}

#[test]
fn tab_click_waits_for_release_and_drag_reorders_by_stable_id() {
    let mut projected = snapshot();
    for index in 2..=3 {
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = format!("tab_{index}");
        tab.number = index;
        tab.label = index.to_string();
        tab.focused = false;
        projected.tabs.push(tab);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("three tabs");
    let first = state.hits.tabs[0].0;
    let third = state.hits.tabs[2].0;

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x + 1,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(down.actions.is_empty());
    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: third.right().saturating_sub(1),
        row: third.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(drag.repaint);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Tab {
            ref tab_id,
            insert_index: Some(3),
            ..
        }) if tab_id == "tab_1"
    ));
    let frame = state.compose(106, 20).expect("tab drop indicator");
    assert!(frame
        .cells
        .iter()
        .take(frame.width as usize)
        .any(|cell| cell.symbol == "│"));

    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: third.right().saturating_sub(1),
            row: third.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab drag should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::TabMove(params)
            if params.tab_id == "tab_1" && params.insert_index == 3
    ));

    state.compose(106, 20).expect("tabs after drag");
    let second = state.hits.tabs[1].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: second.x + 1,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: second.x + 1,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &click.actions[0],
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_2")
    ));
}

#[test]
fn tab_drag_clears_its_drop_target_after_leaving_the_tab_row() {
    let mut projected = snapshot();
    for index in 2..=3 {
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = format!("tab_{index}");
        tab.number = index;
        tab.label = index.to_string();
        tab.focused = false;
        projected.tabs.push(tab);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("three tabs");
    let first = state.hits.tabs[0].0;
    let third = state.hits.tabs[2].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x + 1,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: third.x,
        row: third.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: third.x,
        row: third.y + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Tab {
            insert_index: None,
            ..
        })
    ));
    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: third.x,
            row: third.y + 1,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(release.actions.is_empty());
}

#[test]
fn tab_wheel_switches_tabs_without_changing_overflow_scroll() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("tab bar");
    let tab = state.hits.tabs[0].0;

    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: tab.x,
            row: tab.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_1"
            )
    ));
    assert_eq!(state.tab_scroll, 0);
    state.compose(106, 20).expect("tab bar after wheel");
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_1"));
}

#[test]
fn context_menu_keyboard_and_outside_click_are_client_owned() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let tab = state.hits.tabs[0].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: tab.x + 1,
        row: tab.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 20).expect("tab context menu");
    let moved = state.handle_input_bytes(b"\x1b[B");
    assert!(moved.repaint);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            highlighted: 1,
            ..
        }))
    ));
    let text = state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "not pane input",
    ))]);
    assert!(text.requests.is_empty());
    let paste = state.handle_raw_events(vec![RawInputEvent::Paste("not pane input".into())]);
    assert!(paste.requests.is_empty());
    let outside =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 105,
            row: 19,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outside.repaint);
    assert!(state.overlay.is_none());
}
