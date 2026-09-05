use super::*;

#[test]
fn pasted_help_and_copy_queries_strip_control_characters() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
        query: String::new(),
        search_focused: true,
        scroll: 0,
    }));

    assert!(state.insert_overlay_text("work\nspace"));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Help(ClientHelpOverlay { ref query, .. }))
            if query == "workspace"
    ));

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

    assert!(state.insert_copy_search_text("needle\r\n"));
    assert_eq!(
        state
            .copy_mode
            .as_ref()
            .and_then(|copy_mode| copy_mode.search_prompt.as_ref())
            .map(|prompt| prompt.query.as_str()),
        Some("needle")
    );
}

#[test]
fn client_mouse_selection_highlights_and_copies_through_endpoint_extraction() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x,
        row: pane.inner_rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &down.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
            )
    ));
    assert!(state
        .selection
        .as_ref()
        .is_some_and(|selection| !selection.is_visible()));

    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: pane.inner_rect.x + 2,
        row: pane.inner_rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(drag.repaint);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_visible));
    let selected = state.compose(106, 20).expect("selected frame");
    let selected_cell =
        &selected.cells[usize::from(pane.inner_rect.y) * 106 + usize::from(pane.inner_rect.x)];
    assert_ne!(
        selected_cell.bg,
        crate::protocol::color_to_u32(ratatui::style::Color::Reset)
    );

    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: pane.inner_rect.x + 2,
            row: pane.inner_rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(state.selection.is_none());
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("selection release should request endpoint extraction");
    };
    let request_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneSelectionRead(params)
            if params.pane_id == "pane_1"
                && params.anchor == crate::api::schema::PaneTextPoint { row: 0, col: 0 }
                && params.cursor == crate::api::schema::PaneTextPoint { row: 0, col: 2 }
    ));

    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::PaneSelection {
            pane_id: "pane_1".into(),
            text: "LIV".into(),
        }),
    );
    assert!(repaint);
    assert!(matches!(
        &actions[..],
        [ClientShellAction::ClipboardWrite(bytes)] if bytes == b"LIV"
    ));
    assert_eq!(
        state
            .copy_feedback
            .as_ref()
            .map(|feedback| feedback.message.as_str()),
        Some("copied to clipboard")
    );
}

#[test]
fn clipboard_feedback_is_client_local_and_respects_config() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let now = std::time::Instant::now();
    assert!(state.show_copy_feedback(now));
    assert_eq!(
        state
            .copy_feedback
            .as_ref()
            .map(|feedback| feedback.message.as_str()),
        Some("copied to clipboard")
    );
    assert_eq!(
        state.copy_feedback_deadline,
        Some(now + std::time::Duration::from_secs(2))
    );

    state.config.clipboard_toast_enabled = false;
    state.copy_feedback = None;
    state.copy_feedback_deadline = None;
    assert!(!state.show_copy_feedback(now));
    assert!(state.copy_feedback.is_none());
    assert!(state.copy_feedback_deadline.is_none());
}

#[test]
fn retained_mouse_selection_copies_only_on_exact_copy_shortcut() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.copy_on_select = false;
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();
    for event in [
        crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.inner_rect.x,
            row: pane.inner_rect.y,
            modifiers: KeyModifiers::empty(),
        },
        crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: pane.inner_rect.x + 2,
            row: pane.inner_rect.y,
            modifiers: KeyModifiers::empty(),
        },
        crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: pane.inner_rect.x + 2,
            row: pane.inner_rect.y,
            modifiers: KeyModifiers::empty(),
        },
    ] {
        state.handle_raw_events(vec![RawInputEvent::Mouse(event)]);
    }
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_finalized));

    // Agent output outside the selected text must not cancel a pending copy.
    let mut updated = state.pane_surface.clone().expect("pane surface");
    updated.panes[0].content_revision = 2;
    updated.frame.cells[4].symbol = "-".into();
    state.set_pane_surface(updated);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_finalized));

    let copy = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))]);
    assert!(state.selection.is_none());
    assert!(matches!(
        &copy.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::PaneSelectionRead(_))
    ));
    assert!(copy.requests.is_empty());
    let request_id = match &copy.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    let (_, fallback) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::PaneSelection {
            pane_id: "pane_1".into(),
            text: String::new(),
        }),
    );
    assert!(matches!(
        &fallback[..],
        [ClientShellAction::Request(ClientMessage::ClientShellPaneInput {
            pane_id,
            events,
        })] if pane_id == "pane_1"
            && matches!(
                &events[..],
                [ClientPaneInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('c'),
                    kind: crate::protocol::ClientKeyKind::Press,
                    ..
                }, ClientPaneInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('c'),
                    kind: crate::protocol::ClientKeyKind::Release,
                    ..
                }]
            )
    ));
}

#[test]
fn selection_edge_drag_requests_scroll_and_timer_continues_it() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: pane.inner_rect.x,
        row: pane.inner_rect.y + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: pane.inner_rect.x,
        row: pane.inner_rect.y.saturating_sub(1),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &drag.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneScroll(params)
                    if params.offset_from_bottom == 3
            )
    ));
    let drag_request_id = match &drag.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    let now = std::time::Instant::now();
    state.selection_autoscroll_deadline = Some(now);
    let tick = state.tick_selection_autoscroll(now);
    assert!(tick.actions.is_empty());
    let (_, next_scroll) =
        state.handle_endpoint_result("boot-1", &drag_request_id, Ok(pane_scroll_result(3, 20, 3)));
    assert!(matches!(
        &next_scroll[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneScroll(params)
                    if params.offset_from_bottom == 4
            )
    ));
}

#[test]
fn keyboard_copy_mode_owns_cursor_selection_copy_and_scroll_restore() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");

    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor.row),
        Some(21)
    );
    assert!(enter.actions.is_empty());

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
    ))]);
    assert_eq!(state.mode, ClientShellMode::Prefix);
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Copy);

    let page = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::PageUp,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor.row),
        Some(20)
    );
    assert!(matches!(
        &page.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneScroll(params)
                    if params.offset_from_bottom == 1
            )
    ));
    let page_request_id = match &page.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };

    let top = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('g'),
        KeyModifiers::empty(),
    ))]);
    assert!(top.actions.is_empty());
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor.row),
        Some(0)
    );
    let (_, top_actions) =
        state.handle_endpoint_result("boot-1", &page_request_id, Ok(pane_scroll_result(1, 20, 2)));
    let [ClientShellAction::Endpoint { request, .. }] = &top_actions[..] else {
        panic!("latest queued scroll should follow the completed request");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneScroll(params)
            if params.pane_id == "pane_1" && params.offset_from_bottom == 20
    ));
    let top_request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &top_request_id, Ok(pane_scroll_result(20, 20, 2)));

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('v'),
        KeyModifiers::empty(),
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('l'),
        KeyModifiers::empty(),
    ))]);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_visible));

    let copy = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('y'),
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(state.copy_mode.is_none());
    assert!(state.selection.is_none());
    assert_eq!(copy.actions.len(), 2);
    assert!(copy.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::PaneSelectionRead(_))
    )));
    assert!(copy.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneScroll(params)
                    if params.offset_from_bottom == 0
            )
    )));
}

#[test]
fn keyboard_copy_mode_content_motion_is_endpoint_backed_and_stale_safe() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    let origin = state.copy_mode.as_ref().expect("copy mode").cursor;

    let motion = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('w'),
        KeyModifiers::empty(),
    ))]);
    let [ClientShellAction::Endpoint { request, .. }] = &motion.actions[..] else {
        panic!("word motion should use endpoint semantics");
    };
    let request_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneCopyMotion(params)
            if params.cursor == origin
                && params.motion == crate::api::schema::PaneCopyMotion::NextWordStart
    ));
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::PaneCopyMotion {
            pane_id: "pane_1".into(),
            cursor: crate::api::schema::PaneTextPoint {
                row: origin.row,
                col: 3,
            },
            content_revision: 0,
        }),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor.col),
        Some(3)
    );
}

#[test]
fn copy_search_owns_prompt_repeat_highlights_selection_and_restore() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    let origin = state.copy_mode.as_ref().expect("copy mode").cursor;

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('?'),
        KeyModifiers::SHIFT,
    ))]);
    assert!(state.copy_mode.as_ref().is_some_and(|mode| {
        mode.search_prompt.as_ref().is_some_and(|prompt| {
            prompt.direction == crate::api::schema::PaneCopySearchDirection::Backward
        })
    }));
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert!(state
        .copy_mode
        .as_ref()
        .is_some_and(|mode| mode.search_prompt.is_none()));

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('/'),
        KeyModifiers::empty(),
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "junk",
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "nee",
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Paste("dleX".into())]);
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Backspace,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(
        state
            .copy_mode
            .as_ref()
            .and_then(|mode| mode.search_prompt.as_ref())
            .map(|prompt| prompt.query.as_str()),
        Some("needle")
    );

    let search = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))]);
    let [ClientShellAction::Endpoint { request, .. }] = &search.actions[..] else {
        panic!("search should use endpoint terminal semantics");
    };
    let request_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneCopySearch(params)
            if params.pane_id == "pane_1"
                && params.query == "needle"
                && params.direction == crate::api::schema::PaneCopySearchDirection::Forward
                && params.cursor == origin
                && params.previous.is_none()
    ));
    let matches = vec![
        crate::api::schema::PaneTextRange {
            start: crate::api::schema::PaneTextPoint { row: 5, col: 2 },
            end: crate::api::schema::PaneTextPoint { row: 5, col: 7 },
        },
        crate::api::schema::PaneTextRange {
            start: crate::api::schema::PaneTextPoint { row: 15, col: 1 },
            end: crate::api::schema::PaneTextPoint { row: 15, col: 6 },
        },
    ];
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(copy_search_result(matches.clone(), Some(0))),
    );
    assert!(repaint);
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor.row),
        Some(5)
    );
    assert!(actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneScroll(params)
                    if params.offset_from_bottom == 15
            )
    )));
    let initial_scroll_id = actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. }
                if matches!(request.method, crate::api::schema::Method::PaneScroll(_)) =>
            {
                Some(request.id.clone())
            }
            _ => None,
        })
        .expect("initial search scroll");
    state.handle_endpoint_result(
        "boot-1",
        &initial_scroll_id,
        Ok(pane_scroll_result(15, 20, 2)),
    );
    let mut scrolled_surface = state.pane_surface.clone().expect("pane surface");
    scrolled_surface.panes[0]
        .scroll
        .as_mut()
        .expect("scroll metrics")
        .offset_from_bottom = 15;
    state.set_pane_surface(scrolled_surface);
    let frame = state.compose(106, 20).expect("search frame");
    let hit = state.hits.panes[0].clone();
    let viewport_top = 5u16;
    let restored = frame.to_ratatui_buffer().expect("search frame buffer");
    let highlighted = restored
        .cell((hit.inner_rect.x + 2, hit.inner_rect.y + (5 - viewport_top)))
        .expect("highlighted search cell");
    assert_eq!(highlighted.bg, state.config.palette.accent);

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('v'),
        KeyModifiers::empty(),
    ))]);
    let repeat = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('n'),
        KeyModifiers::empty(),
    ))]);
    let [ClientShellAction::Endpoint { request, .. }] = &repeat.actions[..] else {
        panic!("repeat should use endpoint search");
    };
    let repeat_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneCopySearch(params)
            if params.direction == crate::api::schema::PaneCopySearchDirection::Forward
                && params.previous == Some(matches[0])
    ));
    let (_, repeat_actions) = state.handle_endpoint_result(
        "boot-1",
        &repeat_id,
        Ok(copy_search_result(matches.clone(), Some(1))),
    );
    if let Some(scroll_id) = repeat_actions.iter().find_map(|action| match action {
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::PaneScroll(_)) =>
        {
            Some(request.id.clone())
        }
        _ => None,
    }) {
        state.handle_endpoint_result("boot-1", &scroll_id, Ok(pane_scroll_result(6, 20, 2)));
    }
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor.row),
        Some(15)
    );
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_visible));

    let reverse = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
    )]);
    let [ClientShellAction::Endpoint { request, .. }] = &reverse.actions[..] else {
        panic!("reverse search should use endpoint search");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneCopySearch(params)
            if params.direction == crate::api::schema::PaneCopySearchDirection::Backward
                && params.previous == Some(matches[1])
    ));
    let (_, reverse_actions) = state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(copy_search_result(matches.clone(), Some(0))),
    );
    if let Some(scroll_id) = reverse_actions.iter().find_map(|action| match action {
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::PaneScroll(_)) =>
        {
            Some(request.id.clone())
        }
        _ => None,
    }) {
        state.handle_endpoint_result("boot-1", &scroll_id, Ok(pane_scroll_result(15, 20, 2)));
    }

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert!(state
        .copy_mode
        .as_ref()
        .is_some_and(|mode| mode.search_query.is_empty() && mode.selection.is_none()));
    let exit = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(exit.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneScroll(params)
                    if params.offset_from_bottom == 0
            )
    )));
}

#[test]
fn navigator_owns_search_mouse_selection_and_stable_target_focus() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenNavigator),
        &mut open,
    );
    let navigator = state.compose(106, 30).expect("navigator overlay");
    let navigator_text = navigator
        .cells
        .chunks(navigator.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(navigator_text.contains("client-shell"));
    assert!(navigator_text.contains("pane 1"));

    let search = state.hits.navigator_search;
    let focus_search =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: search.x,
            row: search.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(focus_search.repaint);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Navigator(ClientNavigatorOverlay {
            search_focused: true,
            ..
        }))
    ));
    assert!(state.handle_input_bytes(b"client").actions.is_empty());
    let filtered = state.compose(106, 30).expect("filtered navigator");
    assert!(filtered
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.visible));

    state.handle_input_bytes(b"\x1b");
    state.handle_input_bytes(b"a");
    state.compose(106, 30).expect("navigator rows");
    let pane_index = {
        let snapshot = state.snapshot.as_deref().expect("snapshot");
        let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
        else {
            panic!("expected navigator");
        };
        render::client_navigator_rows(snapshot, navigator)
            .iter()
            .position(|row| matches!(row.target, ClientNavigatorTarget::Pane(_)))
            .expect("pane row")
    };
    let pane_rect = state
        .hits
        .navigator_rows
        .iter()
        .find(|(_, index)| *index == pane_index)
        .map(|(rect, _)| *rect)
        .expect("visible pane row");
    let select =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Moved,
            column: pane_rect.x + 6,
            row: pane_rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(select.repaint);
    let accept =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane_rect.x + 6,
            row: pane_rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let [ClientShellAction::Endpoint { request, .. }] = &accept.actions[..] else {
        panic!("navigator pane click should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn copy_mode_survives_mouse_motion_and_parks_across_focus_changes() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 10,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );

    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert!(state.copy_mode.is_some());

    state.handle_input_bytes(b"v");
    assert!(state
        .copy_mode
        .as_ref()
        .is_some_and(|copy_mode| copy_mode.selection.is_some()));

    let mut unfocused = snapshot();
    unfocused.focused_pane_id = Some("pane_2".into());
    unfocused.panes[0].focused = false;
    unfocused.panes.push(ClientShellPane {
        pane_id: "pane_2".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: None,
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: true,
        right_click_passthrough: false,
    });
    state.set_snapshot(Box::new(unfocused.clone()));
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(state
        .copy_mode
        .as_ref()
        .is_some_and(|copy_mode| copy_mode.selection.is_some()));

    let (prefix_key, prefix_modifiers) = state.config.keybinds.prefix;
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        prefix_key,
        prefix_modifiers,
    ))]);
    state.set_snapshot(Box::new(unfocused.clone()));
    assert_eq!(state.mode, ClientShellMode::Prefix);
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Terminal);

    let mut other_selection =
        crate::selection::Selection::absolute_range("pane_2".to_owned(), (0, 0), (0, 1));
    assert!(other_selection.finish());
    state.selection = Some(other_selection);
    state.set_snapshot(Box::new(unfocused));
    assert!(state
        .selection
        .as_ref()
        .is_some_and(|selection| selection.pane_id == "pane_2"));

    state.set_snapshot(Box::new(snapshot()));
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert!(state.copy_mode.is_some());
    assert!(state
        .selection
        .as_ref()
        .is_some_and(|selection| selection.pane_id == "pane_1"));
    state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "ignored",
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Paste("ignored".into())]);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(|selection| selection.pane_id == "pane_1"));

    state.mode = ClientShellMode::Navigate;
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(|selection| selection.pane_id == "pane_1"));
    state.mode = ClientShellMode::Resize;
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(|selection| selection.pane_id == "pane_1"));
}

#[test]
fn retained_selection_copy_suppresses_key_repeats() {
    let mut config = Config::default();
    config.ui.copy_on_select = false;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut selection =
        crate::selection::Selection::absolute_range("pane_1".to_owned(), (0, 0), (0, 1));
    assert!(selection.finish());
    state.selection = Some(selection);

    let key = crate::input::TerminalKey::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let press = state.handle_raw_events(vec![RawInputEvent::Key(key.clone())]);
    assert!(press.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::PaneSelectionRead(_))
    )));
    let repeat = state.handle_raw_events(vec![RawInputEvent::Key(
        key.with_kind(crossterm::event::KeyEventKind::Repeat),
    )]);
    assert!(repeat.actions.is_empty());
    assert!(repeat.requests.is_empty());
}

#[test]
fn rapid_copy_motions_are_chained_from_the_previous_result() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    let origin = state.copy_mode.as_ref().expect("copy mode").cursor;

    let first = state.handle_input_bytes(b"w");
    let second = state.handle_input_bytes(b"w");
    assert_eq!(first.actions.len(), 1);
    assert!(second.actions.is_empty());
    let first_id = match &first.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    let intermediate = crate::api::schema::PaneTextPoint {
        row: origin.row,
        col: 2,
    };
    let (_, follow_up) = state.handle_endpoint_result(
        "boot-1",
        &first_id,
        Ok(crate::api::schema::ResponseResult::PaneCopyMotion {
            pane_id: "pane_1".into(),
            cursor: intermediate,
            content_revision: 0,
        }),
    );
    assert!(matches!(
        &follow_up[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneCopyMotion(params)
                    if params.cursor == intermediate
            )
    ));
}

#[test]
fn queued_copy_keys_preserve_prefix_order() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 10,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    let origin = state.copy_mode.as_ref().expect("copy mode").cursor;
    let motion = state.handle_input_bytes(b"w");
    state.handle_input_bytes(b"l");
    let (prefix_key, prefix_modifiers) = state.config.keybinds.prefix;
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        prefix_key,
        prefix_modifiers,
    ))]);
    let motion_id = match &motion.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    state.handle_endpoint_result(
        "boot-1",
        &motion_id,
        Ok(crate::api::schema::ResponseResult::PaneCopyMotion {
            pane_id: "pane_1".into(),
            cursor: origin,
            content_revision: 0,
        }),
    );
    assert_eq!(state.mode, ClientShellMode::Prefix);
    assert_eq!(
        state
            .copy_mode
            .as_ref()
            .map(|copy_mode| copy_mode.cursor.col),
        Some(origin.col.saturating_add(1))
    );
}

#[test]
fn reentering_copy_mode_on_the_same_pane_is_a_no_op() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 10,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut first = ClientShellInput::default();
    assert!(state.enter_copy_mode(&mut first));
    state
        .copy_mode
        .as_mut()
        .expect("copy mode")
        .offset_from_bottom = 10;
    let mut reenter = ClientShellInput::default();
    assert!(state.enter_copy_mode(&mut reenter));
    assert!(reenter.actions.is_empty());
    assert_eq!(
        state
            .copy_mode
            .as_ref()
            .map(|copy_mode| copy_mode.entry_offset_from_bottom),
        Some(0)
    );
}

#[test]
fn copy_waits_for_endpoint_motion_before_copying_selection() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    state.handle_input_bytes(b"v");
    let origin = state.copy_mode.as_ref().expect("copy mode").cursor;
    let motion = state.handle_input_bytes(b"w");
    let queued_copy = state.handle_input_bytes(b"y");
    assert!(queued_copy.actions.is_empty());
    let motion_id = match &motion.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    let target = crate::api::schema::PaneTextPoint {
        row: origin.row,
        col: 2,
    };
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &motion_id,
        Ok(crate::api::schema::ResponseResult::PaneCopyMotion {
            pane_id: "pane_1".into(),
            cursor: target,
            content_revision: 0,
        }),
    );
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneSelectionRead(params)
                    if params.anchor == origin && params.cursor == target
            )
    )));
}

#[test]
fn new_content_revision_invalidates_copy_search_coordinates() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface.clone());
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    let copy_mode = state.copy_mode.as_mut().expect("copy mode");
    copy_mode.search_query = "needle".into();
    copy_mode
        .search_matches
        .push(crate::api::schema::PaneTextRange {
            start: crate::api::schema::PaneTextPoint { row: 0, col: 0 },
            end: crate::api::schema::PaneTextPoint { row: 0, col: 1 },
        });
    copy_mode.search_total = 1;
    copy_mode.search_current = Some(0);
    copy_mode.search_current_global = Some(0);

    pane_surface.panes[0].content_revision = 2;
    state.set_pane_surface(pane_surface);
    let copy_mode = state.copy_mode.as_ref().expect("copy mode retained");
    assert!(copy_mode.search_matches.is_empty());
    assert_eq!(copy_mode.search_total, 0);
    assert_eq!(copy_mode.search_current, None);
}

#[test]
fn word_selection_result_survives_focus_snapshot_lag() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    let hit = state.hits.panes[0].clone();
    let mut request = ClientShellInput::default();
    state.request_word_selection(&hit, 0, 1, &mut request);
    let request_id = match &request.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    let mut lagging = snapshot();
    lagging.focused_pane_id = None;
    lagging.panes[0].focused = false;
    state.set_snapshot(Box::new(lagging));
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::PaneSelection {
            pane_id: "pane_1".into(),
            text: "hello world".into(),
        }),
    );
    assert!(repaint);
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_visible));
}
