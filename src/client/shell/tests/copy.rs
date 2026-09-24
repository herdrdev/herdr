use super::*;

#[test]
fn pasted_help_and_copy_queries_normalize_single_line_text() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
        query: TextEditor::default(),
        search_focused: true,
        scroll: 0,
    }));

    assert!(state.insert_overlay_text("work\nspace"));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Help(ClientHelpOverlay { ref query, .. }))
            if query.as_str() == "work space"
    ));

    state.overlay = None;
    state.mode = ClientShellMode::Copy;
    state.copy_mode = Some(ClientCopyModeState {
        pane_id: "pane_1".into(),
        content_revision: 0,
        geometry: (80, 24),
        alternate_screen_active: false,
        cursor: crate::api::schema::PaneTextPoint { row: 0, col: 0 },
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        entry_offset_from_bottom: 0,
        selection: None,
        search_prompt: Some(ClientCopySearchPrompt {
            direction: crate::api::schema::PaneCopySearchDirection::Forward,
            query: TextEditor::default(),
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
        Some("needle ")
    );
}

#[test]
fn client_selection_uses_host_background_and_repaints_when_it_changes() {
    use crate::terminal_theme::{DefaultColorKind, HostAppearance, RgbColor};
    use ratatui::style::Color;

    for explicit_appearance in [false, true] {
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.palette = Palette::terminal();
        config.theme_runtime.auto_switch = false;
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("composed frame");
        let pane = state.hits.panes[0].clone();
        for (kind, column) in [
            (MouseEventKind::Down(MouseButton::Left), pane.inner_rect.x),
            (
                MouseEventKind::Drag(MouseButton::Left),
                pane.inner_rect.x + 2,
            ),
        ] {
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row: pane.inner_rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        }
        let cell_index = usize::from(pane.inner_rect.y) * 106 + usize::from(pane.inner_rect.x);
        let fallback = state.compose(106, 20).expect("fallback frame");
        assert_eq!(
            fallback.cells[cell_index].bg,
            crate::protocol::color_to_u32(Color::DarkGray)
        );
        if explicit_appearance {
            state.handle_raw_events(vec![RawInputEvent::HostColorSchemeChanged(
                HostAppearance::Light,
            )]);
        }
        for (background, selected_bg, selected_fg) in [
            ((237, 237, 234), (171, 171, 168), (0, 0, 0)),
            ((26, 27, 38), (90, 91, 99), (255, 255, 255)),
        ] {
            let (r, g, b) = background;
            let outcome = state.handle_raw_events(vec![RawInputEvent::HostDefaultColor {
                kind: DefaultColorKind::Background,
                color: RgbColor { r, g, b },
            }]);
            assert!(outcome
                .requests
                .iter()
                .any(|request| matches!(request, ClientMessage::ClientShellHostTheme { .. })));
            let frame = state.compose(106, 20).expect("host-colored selection");
            let cell = &frame.cells[cell_index];
            assert_eq!(
                cell.bg,
                crate::protocol::color_to_u32(Color::Rgb(
                    selected_bg.0,
                    selected_bg.1,
                    selected_bg.2
                ))
            );
            assert_eq!(
                cell.fg,
                crate::protocol::color_to_u32(Color::Rgb(
                    selected_fg.0,
                    selected_fg.1,
                    selected_fg.2
                ))
            );
            assert!(
                outcome.repaint,
                "host background changes must repaint selection"
            );
        }
    }
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
    assert!(drag.repaint || state.selection_repaint_deadline.is_some());
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
                && params.content_revision.is_none()
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
fn retained_mouse_selection_survives_output_and_copies_without_terminal_input() {
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
        // Output can arrive between drag and release, including an in-flight revision.
        let mut updated = state.pane_surface.clone().expect("pane surface");
        updated.panes[0].content_revision += 1;
        updated.frame.cells[0].symbol = "x".into();
        state.set_pane_surface(updated);
    }
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_finalized));

    // A patch that redraws selected text must retain the same live terminal range.
    let mut updated = state.pane_surface.clone().expect("pane surface");
    updated.panes[0].content_revision += 1;
    let mut cell = updated.frame.cells[0].clone();
    cell.symbol = "y".into();
    assert!(matches!(
        state.apply_pane_surface_patch(crate::protocol::PaneSurfacePatch {
            boot_id: updated.boot_id,
            projection_revision: updated.projection_revision,
            base_surface_revision: updated.surface_revision,
            surface_revision: updated.surface_revision + 1,
            panes: updated.panes,
            rows: vec![crate::protocol::PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![cell]
            }],
            cursor: updated.frame.cursor,
        }),
        super::super::surface_patch::ClientPaneSurfacePatchOutcome::Applied(_)
    ));
    assert!(state
        .selection
        .as_ref()
        .is_some_and(crate::selection::Selection::is_finalized));

    let highlighted = state.compose(106, 20).expect("highlighted frame");
    let cell_index = usize::from(pane.inner_rect.y) * 106 + usize::from(pane.inner_rect.x);
    let selected_cell = highlighted.cells[cell_index].clone();
    let selection = state.selection.take();
    let unselected = state.compose(106, 20).expect("unselected frame");
    assert_ne!(selected_cell.bg, unselected.cells[cell_index].bg);
    state.selection = selection;

    let copy = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))]);
    assert!(state.selection.is_none());
    assert!(matches!(
        &copy.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::PaneSelectionRead(
                crate::api::schema::PaneSelectionReadParams { content_revision: None, .. }
            ))
    ));
    assert!(copy.requests.is_empty());
    let request_id = match &copy.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!(),
    };
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::PaneSelection {
            pane_id: "pane_1".into(),
            text: "yIV".into(),
        }),
    );
    assert!(matches!(&actions[..], [ClientShellAction::ClipboardWrite(bytes)] if bytes == b"yIV"));
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
    state.config.copy_on_select = false;
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
            if matches!(&request.method, crate::api::schema::Method::PaneSelectionRead(params)
                if params.content_revision.is_none())
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
fn keyboard_selections_survive_output_and_copy_live_ranges() {
    // Character and linewise selections have distinct anchor/range projections.
    for selection_key in [b"v", b"V"] {
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
        state.handle_input_bytes(b"\x02[");
        state.handle_input_bytes(selection_key);
        state.handle_input_bytes(b"k");
        let range = state
            .selection
            .as_ref()
            .expect("selected range")
            .ordered_cells();

        pane_surface.surface_revision += 1;
        pane_surface.panes[0].content_revision += 2;
        pane_surface.frame.cells[0].symbol = "X".into();
        state.set_pane_surface(pane_surface);
        assert_eq!(state.mode, ClientShellMode::Copy);
        assert!(state.copy_mode.as_ref().unwrap().selection.is_some());
        assert_eq!(
            state
                .selection
                .as_ref()
                .expect("retained range")
                .ordered_cells(),
            range
        );

        let copied = state.handle_input_bytes(b"y");
        assert!(copied.actions.iter().any(|action| matches!(
            action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(&request.method, crate::api::schema::Method::PaneSelectionRead(params)
                    if params.content_revision.is_none()
                        && (params.anchor.row, params.anchor.col) == range.0
                        && (params.cursor.row, params.cursor.col) == range.1)
        )));
        assert_eq!(state.mode, ClientShellMode::Terminal);
        assert!(state.selection.is_none());
        assert!(state.copy_mode.is_none());
    }
}

#[test]
fn empty_keyboard_anchor_keeps_search_fallback_revision_guard() {
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
    state.handle_input_bytes(b"\x02[");
    let search = state.handle_input_bytes(b"/LIVE\r");
    let [ClientShellAction::Endpoint { request, .. }] = &search.actions[..] else {
        panic!("search request");
    };
    let found = crate::api::schema::PaneTextRange {
        start: crate::api::schema::PaneTextPoint { row: 0, col: 0 },
        end: crate::api::schema::PaneTextPoint { row: 0, col: 3 },
    };
    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(copy_search_result(vec![found], Some(0))),
    );
    state.handle_input_bytes(b"v");
    assert!(!state.selection.as_ref().unwrap().is_visible());
    let copy = state.handle_input_bytes(b"y");
    assert!(copy.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::PaneSelectionRead(params)
                if params.anchor == found.start
                    && params.cursor == found.end
                    && params.content_revision == Some(0))
    )));
}

#[test]
fn keyboard_selection_does_not_return_after_resize_or_screen_switch() {
    for screen_switch in [false, true] {
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
        state.handle_input_bytes(b"\x02[");
        state.handle_input_bytes(b"vk");
        assert!(state.selection.is_some());
        pane_surface.surface_revision += 1;
        pane_surface.panes[0].content_revision += 2;
        if screen_switch {
            pane_surface.panes[0].alternate_screen_active = true;
        } else {
            pane_surface.panes[0].inner_rect.width -= 1;
        }
        state.set_pane_surface(pane_surface);
        assert!(state.selection.is_none());
        assert!(state.copy_mode.as_ref().unwrap().selection.is_none());
        state.compose(106, 20).expect("changed frame");
        state.handle_input_bytes(b"l");
        assert!(
            state.selection.is_none(),
            "movement must not resurrect the old anchor"
        );
    }
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
fn navigator_workspace_headings_use_the_active_themes_primary_text() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.theme_runtime.auto_switch = false;
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    for palette in [
        Palette::catppuccin(),
        Palette::catppuccin_latte(),
        Palette::terminal(),
    ] {
        state.config.palette = palette;
        let frame = state.compose(106, 30).expect("navigator");
        let (rect, _) = state
            .hits
            .navigator_rows
            .iter()
            .find(|(_, target)| matches!(target, ClientNavigatorTarget::Workspace { .. }))
            .expect("workspace heading");
        let position = cell_symbol_position(&frame, *rect, "client-shell");
        let buffer = frame.to_ratatui_buffer().expect("buffer");
        assert_eq!(buffer[position].fg, state.config.palette.text);
        assert!(buffer[position].modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn navigator_renders_every_terminal_in_workspace_sections() {
    let mut snapshot = snapshot();
    snapshot.focused_pane_id = None;
    snapshot.tabs[0].label = "editor".into();
    snapshot.panes[0].label = Some("agent".into());
    snapshot.panes[0].focused = false;
    let mut shell = snapshot.panes[0].clone();
    shell.pane_id = "pane_shell".into();
    shell.label = Some("shell".into());
    snapshot.panes.push(shell);
    for label in ["notes", "logs"] {
        let mut tab = snapshot.tabs[0].clone();
        tab.tab_id = format!("tab_{label}");
        tab.label = label.into();
        tab.focused = false;
        tab.number = snapshot.tabs.len() + 1;
        let mut pane = snapshot.panes[0].clone();
        pane.pane_id = format!("pane_{label}");
        pane.tab_id = tab.tab_id.clone();
        pane.label = Some(label.into());
        pane.focused = false;
        snapshot.tabs.push(tab);
        snapshot.panes.push(pane);
    }
    let mut workspace = snapshot.workspaces[0].clone();
    workspace.workspace_id = "ws_2".into();
    workspace.active_tab_id = "tab_last".into();
    workspace.label = "second".into();
    workspace.number = 2;
    workspace.focused = false;
    let mut tab = snapshot.tabs[0].clone();
    tab.workspace_id = workspace.workspace_id.clone();
    tab.tab_id = workspace.active_tab_id.clone();
    tab.label = "last".into();
    tab.focused = false;
    let mut pane = snapshot.panes[0].clone();
    pane.workspace_id = workspace.workspace_id.clone();
    pane.tab_id = tab.tab_id.clone();
    pane.pane_id = "pane_last".into();
    snapshot.workspaces.push(workspace);
    snapshot.tabs.push(tab);
    snapshot.panes.push(pane);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    let visible_rows = |state: &mut ClientShellState, height| {
        let frame = state.compose(106, height).expect("navigator frame");
        state
            .hits
            .navigator_rows
            .iter()
            .map(|(rect, _)| {
                frame.cells[rect.y as usize * frame.width as usize + rect.x as usize..]
                    .iter()
                    .take(rect.width as usize)
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let visible = visible_rows(&mut state, 30);
    assert_eq!(visible.len(), 7);
    for (row, prefix) in visible
        .iter()
        .zip([" client", " ├─ ", " ├─ ", " ├─ ", " └─ ", " second", " └─ "])
    {
        assert!(
            row.starts_with(prefix),
            "{row:?} should start with {prefix:?}"
        );
    }
    for (row, label) in visible.iter().zip([
        "client-shell",
        "editor · agent · 1",
        "editor · shell · 2",
        "notes",
        "logs",
        "second",
        "agent",
    ]) {
        assert!(row.contains(label), "{row:?} should contain {label}");
        assert!(!row.contains("/repo"));
        assert!(!row.contains("──"));
    }

    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("expected navigator");
    };
    navigator.scroll = 2;
    navigator.selected = Some(ClientNavigatorTarget::Pane {
        endpoint_id: state.active_endpoint_id.clone(),
        pane_id: "pane_shell".into(),
    });
    let visible = visible_rows(&mut state, 11);
    assert_eq!(visible.len(), 2);
    assert!(visible.iter().any(|row| row.contains("editor · shell · 2")));
    assert!(visible[0].starts_with(" ├─ "));
    assert!(visible[1].starts_with(" ├─ "));

    // Filtering retains the section and the exact split destination.
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("expected navigator");
    };
    navigator.query = "pane_shell".into();
    navigator.scroll = 0;
    let visible = visible_rows(&mut state, 30);
    assert_eq!(visible.len(), 2);
    assert!(visible[1].starts_with(" └─ "));
    assert!(visible.iter().any(|row| row.contains("editor · shell · 2")));
    assert!(visible.iter().all(|row| !row.contains("second")));
}

#[test]
fn navigator_search_matches_non_adjacent_words_without_losing_the_pane_target() {
    let mut projected = snapshot();
    projected.panes[0].label = Some("alpha beta gamma".into());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("navigator");
    };
    for (query, matches) in [
        ("alpha gamma", true),
        ("  ALP\tGAM  ", true),
        ("gamma alpha", true),
        ("beta gamma", true),
        ("alpha missing", false),
        ("alphagamma", false),
    ] {
        navigator.query = query.into();
        navigator.selected = None;
        let rows =
            render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
        let target =
            super::super::aggregate_navigation::selected_navigator_target(&rows, navigator);
        assert_eq!(
            target,
            matches.then(|| ClientNavigatorTarget::Pane {
                endpoint_id: state.active_endpoint_id.clone(),
                pane_id: "pane_1".into(),
            }),
            "query={query:?}"
        );
    }
}

#[test]
fn navigator_searches_ancestor_context_and_keeps_split_agents_individually_actionable() {
    let mut projected = snapshot();
    projected.tabs[0].label = "review".into();
    projected.tabs[0].custom_label = true;
    let mut second = projected.panes[0].clone();
    second.pane_id = "pane_2".into();
    second.focused = false;
    second.foreground_cwd = Some("/repo/subproject".into());
    projected.panes.push(second);
    let first_agent = ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("writer".into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: Some("implementing navigation".into()),
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    };
    let mut second_agent = first_agent.clone();
    second_agent.pane_id = "pane_2".into();
    second_agent.name = Some("reviewer".into());
    second_agent.agent = Some("claude".into());
    second_agent.title = Some("checking navigation".into());
    second_agent.agent_status = AgentStatus::Blocked;
    second_agent.focused = false;
    projected.agents = vec![first_agent, second_agent];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    for (query, filter, expected) in [
        ("", None, vec!["pane_1", "pane_2"]),
        ("review", None, vec!["pane_1", "pane_2"]),
        ("client-shell", None, vec!["pane_1", "pane_2"]),
        ("main", None, vec!["pane_1", "pane_2"]),
        ("claude", None, vec!["pane_2"]),
        ("checking navigation", None, vec!["pane_2"]),
        ("/repo/subproject", None, vec!["pane_2"]),
        (
            "review",
            Some(ClientNavigatorFilter::Blocked),
            vec!["pane_2"],
        ),
        ("", Some(ClientNavigatorFilter::Working), vec!["pane_1"]),
        ("no such agent", None, vec![]),
    ] {
        let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
            panic!("navigator");
        };
        navigator.query = query.into();
        navigator.filter = filter;
        navigator.selected = None;
        let rows =
            render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
        let pane_ids = rows
            .iter()
            .filter_map(|row| match &row.target {
                ClientNavigatorTarget::Pane { pane_id, .. } => Some(pane_id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(pane_ids, expected, "query={query:?} filter={filter:?}");
        assert_eq!(
            rows.len(),
            if expected.is_empty() {
                0
            } else {
                expected.len() + 1
            }
        );
        if !expected.is_empty() {
            let selected =
                super::super::aggregate_navigation::navigator_selected_index(&rows, navigator)
                    .expect("search destination");
            assert!(matches!(
                rows[selected].target,
                ClientNavigatorTarget::Pane { .. }
            ));
        }
    }
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("navigator");
    };
    navigator.query.clear();
    let frame = state.compose(160, 48).expect("navigator");
    assert_eq!(state.hits.navigator_popup.width, 116);
    let pane_rows = state
        .hits
        .navigator_rows
        .iter()
        .filter(|(_, target)| matches!(target, ClientNavigatorTarget::Pane { .. }))
        .collect::<Vec<_>>();
    assert_eq!(pane_rows.len(), 2);
    for ((rect, _), (name, kind, status)) in pane_rows.iter().zip([
        ("writer", "pi", "working"),
        ("reviewer", "claude", "blocked"),
    ]) {
        cell_symbol_position(&frame, *rect, name);
        cell_symbol_position(&frame, *rect, kind);
        cell_symbol_position(&frame, *rect, status);
    }
    let rect = pane_rows[1].0;
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.right() - 1,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(
        matches!(outcome.actions.as_slice(), [ClientShellAction::Endpoint { request, .. }]
        if matches!(&request.method, crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"))
    );
}

#[test]
fn navigator_distinguishes_unnamed_terminals_on_numbered_tabs() {
    let mut projected = snapshot();
    for (number, label) in [(2, "2"), (3, "logs")] {
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = format!("tab_{number}");
        tab.number = number;
        tab.label = label.into();
        tab.custom_label = number == 3;
        tab.focused = false;
        let mut pane = projected.panes[0].clone();
        pane.pane_id = format!("pane_{number}");
        pane.tab_id = tab.tab_id.clone();
        pane.focused = false;
        projected.tabs.push(tab);
        projected.panes.push(pane);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.open_navigator_overlay();
    let Some(ClientShellOverlay::Navigator(navigator)) = &state.overlay else {
        panic!("navigator");
    };
    let rows =
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
    let labels = rows
        .iter()
        .filter(|row| matches!(row.target, ClientNavigatorTarget::Pane { .. }))
        .map(|row| row.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["terminal · 1", "terminal · 2", "logs"]);
}

#[test]
fn navigator_keeps_empty_workspaces_searchable_without_status_filters() {
    let mut projected = snapshot();
    projected.tabs.clear();
    projected.panes.clear();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.open_navigator_overlay();
    for (query, filter, expected) in [
        ("", None, true),
        ("client-shell", None, true),
        ("main", None, true),
        ("missing", None, false),
        ("main", Some(ClientNavigatorFilter::Idle), false),
        ("", Some(ClientNavigatorFilter::Working), false),
    ] {
        let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
            panic!("navigator");
        };
        navigator.query = query.into();
        navigator.filter = filter;
        let rows =
            render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
        assert_eq!(
            rows.len(),
            usize::from(expected),
            "query={query:?}, filter={filter:?}"
        );
        let target =
            super::super::aggregate_navigation::selected_navigator_target(&rows, navigator);
        assert_eq!(
            target,
            expected.then(|| ClientNavigatorTarget::Workspace {
                endpoint_id: ClientEndpointId::Local,
                workspace_id: "ws_1".into(),
            })
        );
    }
}

#[test]
fn navigator_horizontal_arrows_jump_sections_but_edit_the_search_cursor() {
    let mut projected = snapshot();
    projected.panes[0].label = Some("needle-first".into());
    let mut sibling = projected.panes[0].clone();
    sibling.pane_id = "pane_sibling".into();
    sibling.label = Some("other".into());
    sibling.focused = false;
    projected.panes.push(sibling);
    let mut empty = projected.workspaces[0].clone();
    empty.workspace_id = "ws_empty".into();
    empty.label = "empty".into();
    empty.focused = false;
    projected.workspaces.push(empty);
    let mut last = projected.workspaces[0].clone();
    last.workspace_id = "ws_last".into();
    last.label = "last".into();
    last.active_tab_id = "tab_last".into();
    last.focused = false;
    let mut tab = projected.tabs[0].clone();
    tab.workspace_id = last.workspace_id.clone();
    tab.tab_id = last.active_tab_id.clone();
    tab.focused = false;
    for (id, label) in [
        ("pane_last", "needle-last"),
        ("pane_last_sibling", "other-last"),
    ] {
        let mut pane = projected.panes[0].clone();
        pane.pane_id = id.into();
        pane.label = Some(label.into());
        pane.workspace_id = last.workspace_id.clone();
        pane.tab_id = tab.tab_id.clone();
        pane.focused = false;
        projected.panes.push(pane);
    }
    projected.workspaces.push(last);
    projected.tabs.push(tab);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    let press = |state: &mut ClientShellState, code| {
        let outcome = state.handle_raw_events(vec![RawInputEvent::Key(
            crate::input::TerminalKey::new(code, KeyModifiers::empty()),
        )]);
        assert!(outcome.actions.is_empty());
    };
    let selected = |state: &ClientShellState| {
        let Some(ClientShellOverlay::Navigator(navigator)) = &state.overlay else {
            panic!("navigator");
        };
        navigator.selected.clone()
    };
    let target = |id: &str| {
        Some(ClientNavigatorTarget::Pane {
            endpoint_id: ClientEndpointId::Local,
            pane_id: id.into(),
        })
    };
    press(&mut state, KeyCode::Left);
    assert_eq!(selected(&state), target("pane_1"));
    press(&mut state, KeyCode::Right);
    assert_eq!(selected(&state), target("pane_last"));
    press(&mut state, KeyCode::Down);
    assert_eq!(selected(&state), target("pane_last_sibling"));
    press(&mut state, KeyCode::Right);
    assert_eq!(selected(&state), target("pane_last_sibling"));
    press(&mut state, KeyCode::Left);
    assert_eq!(selected(&state), target("pane_1"));
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("navigator");
    };
    navigator.query = "needle".into();
    press(&mut state, KeyCode::Right);
    assert_eq!(selected(&state), target("pane_last"));
    press(&mut state, KeyCode::Left);
    assert_eq!(selected(&state), target("pane_1"));
    press(&mut state, KeyCode::Char('/'));
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Right);
    assert_eq!(selected(&state), target("pane_1"));
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Char('X'));
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("navigator");
    };
    assert_eq!(navigator.query.as_str(), "needlXe");
    navigator.search_focused = false;
    navigator.selected = None;
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Right);
    assert_eq!(selected(&state), None);
}

#[test]
fn navigator_scrollbar_click_and_drag_scroll_without_opening_a_destination() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    state.compose(106, 24).expect("small navigator");
    assert!(state.hits.navigator_scrollbar.is_empty());

    let mut projected = snapshot();
    for index in 2..=60 {
        let mut pane = projected.panes[0].clone();
        pane.pane_id = format!("pane_{index}");
        pane.label = Some(format!("agent {index}"));
        pane.focused = false;
        projected.panes.push(pane);
    }
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(106, 24).expect("overflowing navigator");
    let track = state.hits.navigator_scrollbar;
    let metrics = state.hits.navigator_scroll_metrics.expect("scroll metrics");
    assert!(!track.is_empty());
    assert_eq!(metrics.offset_from_bottom, metrics.max_offset_from_bottom);
    assert!(track.y > state.hits.navigator_search.y);
    assert!(track.bottom() < state.hits.navigator_popup.bottom() - 3);
    assert!(state
        .hits
        .navigator_rows
        .iter()
        .all(|(rect, _)| rect.right() == track.x));
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    assert_eq!(buffer[(track.x, track.y)].fg, state.config.palette.overlay1);
    assert_eq!(
        buffer[(track.x, track.bottom() - 1)].fg,
        state.config.palette.overlay0
    );
    let mouse = |state: &mut ClientShellState, kind, row| {
        let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind,
            column: track.x,
            row,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(outcome.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Navigator(_))
        ));
    };
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        track.bottom() - 1,
    );
    state.compose(106, 24).expect("track jump");
    assert_eq!(
        state
            .hits
            .navigator_scroll_metrics
            .expect("metrics")
            .offset_from_bottom,
        0
    );
    assert!(state.hits.navigator_rows.iter().any(|(_, target)| matches!(target, ClientNavigatorTarget::Pane { pane_id, .. } if pane_id == "pane_60")));
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), track.y);
    state.compose(106, 24).expect("jump back to top");
    assert_eq!(
        state
            .hits
            .navigator_scroll_metrics
            .expect("metrics")
            .offset_from_bottom,
        metrics.max_offset_from_bottom
    );
    let thumb = crate::ui::scrollbar_thumb(metrics, track).expect("thumb");
    let grab = thumb.len - 1;
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        thumb.top + grab,
    );
    assert!(
        matches!(state.chrome_drag, Some(ClientChromeDrag::NavigatorScrollbar { grab_row_offset }) if grab_row_offset == grab)
    );
    mouse(
        &mut state,
        MouseEventKind::Drag(MouseButton::Left),
        thumb.top + grab,
    );
    state.compose(106, 24).expect("grab does not move viewport");
    assert_eq!(
        state
            .hits
            .navigator_scroll_metrics
            .expect("metrics")
            .offset_from_bottom,
        metrics.max_offset_from_bottom
    );
    mouse(
        &mut state,
        MouseEventKind::Drag(MouseButton::Left),
        track.bottom() + 5,
    );
    state.compose(106, 24).expect("drag to bottom");
    assert_eq!(
        state
            .hits
            .navigator_scroll_metrics
            .expect("metrics")
            .offset_from_bottom,
        0
    );
    mouse(
        &mut state,
        MouseEventKind::Up(MouseButton::Left),
        track.bottom() + 5,
    );
    assert!(state.chrome_drag.is_none());
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Up,
        KeyModifiers::empty(),
    ))]);
    state.compose(106, 24).expect("keyboard resumes after drag");
    assert_eq!(
        state
            .hits
            .navigator_scroll_metrics
            .expect("metrics")
            .offset_from_bottom,
        1
    );

    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("navigator");
    };
    navigator.query = "pane_60".into();
    navigator.selected = None;
    state.compose(106, 24).expect("filtered navigator");
    assert!(state.hits.navigator_scrollbar.is_empty());
    assert_eq!(state.hits.navigator_rows.len(), 2);
    state.compose(106, 90).expect("tall filtered navigator");
    assert!(state.hits.navigator_scrollbar.is_empty());
}

#[test]
fn navigator_narrow_layout_and_long_search_stay_inside_the_popup() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.open_navigator_overlay();
    for (width, height) in [(24, 12), (50, 24), (106, 30)] {
        let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
            panic!("navigator");
        };
        navigator.search_focused = true;
        navigator.query = "界".repeat(100).as_str().into();
        let frame = state.compose(width, height).expect("navigator frame");
        let popup = state.hits.navigator_popup;
        let cursor = frame.cursor.expect("search cursor");
        assert!(super::super::contains(popup, (cursor.x, cursor.y)));
        assert!(state.hits.navigator_rows.is_empty());
        assert!(popup.right() <= width && popup.bottom() <= height);
    }
}

fn navigator_scale_snapshot(workspaces: usize, tabs: usize, panes: usize) -> ClientShellSnapshot {
    let mut result = snapshot();
    let workspace_template = result.workspaces[0].clone();
    let tab_template = result.tabs[0].clone();
    let pane_template = result.panes[0].clone();
    result.workspaces.clear();
    result.tabs.clear();
    result.panes.clear();
    for w in 0..workspaces {
        let mut workspace = workspace_template.clone();
        workspace.workspace_id = format!("workspace_{w}");
        workspace.active_tab_id = format!("tab_{w}_0");
        workspace.number = w + 1;
        workspace.label = format!("workspace {w}");
        workspace.focused = w == 0;
        for t in 0..tabs {
            let mut tab = tab_template.clone();
            tab.workspace_id = workspace.workspace_id.clone();
            tab.tab_id = format!("tab_{w}_{t}");
            tab.number = t + 1;
            tab.label = format!("tab {t}");
            tab.focused = w == 0 && t == 0;
            for p in 0..panes {
                let mut pane = pane_template.clone();
                pane.workspace_id = workspace.workspace_id.clone();
                pane.tab_id = tab.tab_id.clone();
                pane.pane_id = format!("pane_{w}_{t}_{p}");
                pane.label = Some(format!("terminal {p}"));
                pane.focused = w == 0 && t == 0 && p == 0;
                result.panes.push(pane);
            }
            result.tabs.push(tab);
        }
        result.workspaces.push(workspace);
    }
    result.focused_workspace_id = Some(result.workspaces[0].workspace_id.clone());
    result.focused_tab_id = Some(result.tabs[0].tab_id.clone());
    result.focused_pane_id = Some(result.panes[0].pane_id.clone());
    result
}

#[test]
fn navigator_grouping_keeps_snapshot_order_with_interleaved_tabs_and_panes() {
    let mut snapshot = navigator_scale_snapshot(2, 2, 2);
    snapshot.tabs.swap(1, 2);
    snapshot.panes.reverse();
    let expected = snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| {
            snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == workspace.workspace_id)
                .flat_map(|tab| {
                    snapshot
                        .panes
                        .iter()
                        .filter(|pane| pane.tab_id == tab.tab_id)
                        .map(|pane| pane.pane_id.clone())
                })
        })
        .collect::<Vec<_>>();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let remote = SavedSshEndpoint::new("Remote", "dev@example.invalid", "test").unwrap();
    let remote_id = ClientEndpointId::Ssh(remote.id.clone());
    state.set_endpoint_catalog(&[remote]);
    state.set_endpoint_status(&remote_id, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot(&remote_id, Box::new(snapshot.clone()));
    state.set_snapshot(Box::new(snapshot));
    state.open_navigator_overlay();
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_ref() else {
        panic!("navigator")
    };
    let rows =
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
    let actual = rows
        .iter()
        .filter_map(|row| match &row.target {
            ClientNavigatorTarget::Pane {
                endpoint_id,
                pane_id,
            } => {
                assert!(endpoint_id == &state.active_endpoint_id || endpoint_id == &remote_id);
                Some((endpoint_id.clone(), pane_id.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected = [state.active_endpoint_id.clone(), remote_id]
        .into_iter()
        .flat_map(|endpoint| {
            expected
                .iter()
                .map(move |pane| (endpoint.clone(), pane.clone()))
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
#[ignore = "manual navigator composition scaling profile"]
fn navigator_render_scale_profile() {
    for (workspaces, tabs, panes) in [
        (1, 1, 1),
        (1, 1, 15),
        (1, 1, 52),
        (1, 1, 512),
        (1, 128, 4),
        (64, 2, 4),
        (128, 4, 1),
    ] {
        for query in ["", "terminal 0"] {
            let mut state =
                ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
            state.set_snapshot(Box::new(navigator_scale_snapshot(workspaces, tabs, panes)));
            let mut pane_surface = surface();
            pane_surface.panes[0].pane_id = "pane_0_0_0".into();
            state.set_pane_surface(pane_surface);
            state.open_navigator_overlay();
            if let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() {
                navigator.query = query.into();
            }
            for _ in 0..20 {
                std::hint::black_box(state.compose(106, 30).expect("navigator frame"));
            }
            let start = std::time::Instant::now();
            for _ in 0..1000 {
                std::hint::black_box(state.compose(106, 30).expect("navigator frame"));
            }
            eprintln!(
                "navigator: {workspaces}x{tabs}x{panes}, query={query:?}, {:.1} us/frame",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
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
    assert!(navigator_text.contains("terminal"));
    assert!(!navigator_text.contains("pane 1"));

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
    let pane_target = {
        let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
        else {
            panic!("expected navigator");
        };
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator)
            .iter()
            .find(|row| matches!(row.target, ClientNavigatorTarget::Pane { .. }))
            .map(|row| row.target.clone())
            .expect("pane row")
    };
    let pane_rect = state
        .hits
        .navigator_rows
        .iter()
        .find(|(_, target)| *target == pane_target)
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
    state.config.copy_on_select = false;
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

    let mut other_surface = surface();
    other_surface.panes[0].pane_id = "pane_2".into();
    state.set_pane_surface(other_surface.clone());
    other_surface.surface_revision += 1;
    other_surface.panes[0].content_revision = 1;
    state.set_pane_surface(other_surface);
    assert!(state.selection.is_some());
    let copy = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))]);
    assert!(copy.requests.is_empty());
    assert!(
        matches!(&copy.actions[..], [ClientShellAction::Endpoint { request, .. }]
        if matches!(&request.method, crate::api::schema::Method::PaneSelectionRead(params)
            if params.pane_id == "pane_2" && params.content_revision.is_none()))
    );

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
        key.clone()
            .with_kind(crossterm::event::KeyEventKind::Repeat),
    )]);
    assert!(repeat.actions.is_empty());
    assert!(repeat.requests.is_empty());
    let release = state.handle_raw_events(vec![RawInputEvent::Key(
        key.with_kind(crossterm::event::KeyEventKind::Release),
    )]);
    assert!(release.actions.is_empty());
    assert!(release.requests.is_empty());
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
