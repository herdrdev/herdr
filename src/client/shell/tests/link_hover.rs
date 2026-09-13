use super::*;

fn hover_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_endpoint_methods(Some(vec!["pane.link.resolve".into()]));
    state.set_pane_surface(surface());
    state.compose(106, 20).unwrap();
    state
}

fn hover_mouse(state: &ClientShellState, col: u16, row: u16) -> MouseEvent {
    let pane = &state.hits.panes[0];
    MouseEvent {
        kind: MouseEventKind::Moved,
        column: pane.inner_rect.x + col,
        row: pane.inner_rect.y + row,
        modifiers: KeyModifiers::CONTROL,
    }
}

fn hover_request(outcome: &ClientShellInput) -> String {
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("expected one side-effect-free link resolution");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::PaneLinkResolve(_)
    ));
    request.id.clone()
}

fn resolve_hover(state: &mut ClientShellState, id: &str) -> bool {
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        id,
        Ok(crate::api::schema::ResponseResult::PaneLinkResolved {
            regions: vec![
                crate::api::schema::PaneLinkRegion {
                    row: 0,
                    start_col: 1,
                    end_col: 3,
                },
                crate::api::schema::PaneLinkRegion {
                    row: 1,
                    start_col: 0,
                    end_col: 2,
                },
            ],
        }),
    );
    assert!(
        actions.is_empty(),
        "hover must never open a URL or activate a plugin"
    );
    repaint
}

#[test]
fn ctrl_hover_underlines_wrapped_segments_and_caches_the_link() {
    let mut state = hover_state();
    let mouse = hover_mouse(&state, 1, 0);
    let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
    assert!(resolve_hover(&mut state, &id));
    let frame = state.compose(106, 20).unwrap();
    let pane = &state.hits.panes[0];
    let underlined = |col, row| {
        frame.cells[usize::from(pane.inner_rect.y + row) * usize::from(frame.width)
            + usize::from(pane.inner_rect.x + col)]
        .modifier
            & Modifier::UNDERLINED.bits()
            != 0
    };
    assert!(!underlined(0, 0));
    assert!(underlined(1, 0));
    assert!(underlined(3, 0));
    assert!(underlined(0, 1));
    assert!(underlined(2, 1));
    assert!(!underlined(3, 1));
    let moved = hover_mouse(&state, 2, 1);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(moved)]);
    assert!(outcome.actions.is_empty());
    assert!(!outcome.repaint);
    assert!(state.selection.is_none());
}

#[test]
fn ctrl_hover_pending_request_does_not_block_another_endpoint_with_the_same_boot_id() {
    use crate::client::endpoint::{
        ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
    };
    let mut state = hover_state();
    let mouse = hover_mouse(&state, 1, 0);
    let old_id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Other connection".into(),
        target: "host".into(),
        session: "default".into(),
        enabled: true,
    };
    let endpoint = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot(&endpoint, Box::new(snapshot()));
    assert!(state.activate_endpoint_projection(&endpoint));
    assert!(
        state.pending_requests.is_empty(),
        "endpoint switches retire old requests"
    );
    state.set_endpoint_methods(Some(vec!["pane.link.resolve".into()]));
    state.set_pane_surface(surface());
    state.compose(106, 20).unwrap();
    let mouse = hover_mouse(&state, 1, 0);
    let new_id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
    assert!(
        !resolve_hover(&mut state, &old_id),
        "old endpoint must not paint here"
    );
    assert!(resolve_hover(&mut state, &new_id));
}

#[test]
fn ctrl_hover_coalesces_motion_and_ignores_old_replies() {
    let mut state = hover_state();
    let first = hover_mouse(&state, 1, 0);
    let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(first)]));
    for col in [2, 3, 0] {
        let mouse = hover_mouse(&state, col, 1);
        assert!(state
            .handle_raw_events(vec![RawInputEvent::Mouse(mouse)])
            .actions
            .is_empty());
    }
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(crate::api::schema::ResponseResult::PaneLinkResolved { regions: vec![] }),
    );
    assert!(!repaint);
    assert!(
        matches!(
            &actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(&request.method, crate::api::schema::Method::PaneLinkResolve(params)
                    if params.viewport_row == 1 && params.col == 0)
        ),
        "only the latest pointer position should be queried"
    );
    assert!(state.link_hover.as_ref().unwrap().regions.is_empty());
}

#[test]
fn ctrl_hover_clears_on_release_content_change_and_focus_loss() {
    for clear in 0..4 {
        let mut state = hover_state();
        let mouse = hover_mouse(&state, 1, 0);
        let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
        assert!(resolve_hover(&mut state, &id));
        match clear {
            0 => {
                state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                    modifiers: KeyModifiers::empty(),
                    ..mouse
                })]);
            }
            1 => {
                let mut next = surface();
                next.surface_revision += 1;
                next.panes[0].content_revision += 2;
                state.set_pane_surface(next);
            }
            2 => {
                state.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
            }
            _ => {
                state.handle_raw_events(vec![RawInputEvent::Key(
                    crate::input::TerminalKey::new(
                        KeyCode::Modifier(crossterm::event::ModifierKeyCode::LeftControl),
                        KeyModifiers::empty(),
                    )
                    .with_kind(crossterm::event::KeyEventKind::Release),
                )]);
            }
        }
        assert!(state.link_hover.is_none());
    }
}

#[test]
fn ctrl_hover_content_patch_removes_all_old_underlines() {
    let mut state = hover_state();
    let mouse = hover_mouse(&state, 1, 0);
    let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
    assert!(resolve_hover(&mut state, &id));
    state.compose(106, 20).unwrap();
    let mut pane = surface().panes.remove(0);
    pane.content_revision = 2;
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: vec![surface().frame.cells[0].clone()],
        }],
        panes: vec![pane],
        cursor: None,
    };
    assert!(matches!(
        state.apply_pane_surface_patch(patch),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));
    assert!(state.link_hover.is_none());
    let frame = state.compose(106, 20).unwrap();
    let pane = &state.hits.panes[0];
    for row in 0..2 {
        for col in 0..4 {
            let idx = usize::from(pane.inner_rect.y + row) * usize::from(frame.width)
                + usize::from(pane.inner_rect.x + col);
            assert_eq!(frame.cells[idx].modifier & Modifier::UNDERLINED.bits(), 0);
        }
    }
}

#[test]
fn ctrl_hover_preserves_fast_patches_for_other_panes() {
    let mut state = hover_state();
    let mut next = surface();
    next.surface_revision = 2;
    next.frame.height = 4;
    next.frame.cells.extend(next.frame.cells.clone());
    let mut other = next.panes[0].clone();
    other.pane_id = "pane_2".into();
    other.rect.y = 2;
    other.inner_rect.y = 2;
    next.panes.push(other.clone());
    state.set_pane_surface(next);
    state.compose(106, 20).unwrap();
    let mouse = hover_mouse(&state, 1, 0);
    let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
    assert!(resolve_hover(&mut state, &id));
    state.compose(106, 20).unwrap();
    other.content_revision = 2;
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 2,
        surface_revision: 3,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 2,
            cells: vec![surface().frame.cells[0].clone()],
        }],
        panes: vec![other],
        cursor: None,
    };
    assert!(matches!(
        state.apply_pane_surface_patch(patch),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));
    assert!(!state.link_hover.as_ref().unwrap().regions.is_empty());
}

#[test]
fn ctrl_hover_ignores_late_reply_after_pointer_leaves() {
    let mut state = hover_state();
    let mouse = hover_mouse(&state, 1, 0);
    let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        column: 0,
        row: 0,
        ..mouse
    })]);
    assert!(!resolve_hover(&mut state, &id));
    assert!(state.link_hover.is_none());
}

#[test]
fn ctrl_hover_is_silent_when_unsupported_or_rejected() {
    let mut state = hover_state();
    state.set_endpoint_methods(Some(vec![]));
    let mouse = hover_mouse(&state, 1, 0);
    assert!(state
        .handle_raw_events(vec![RawInputEvent::Mouse(mouse)])
        .actions
        .is_empty());
    assert!(state.visible_endpoint_notice.is_none());
    state.set_endpoint_methods(Some(vec!["pane.link.resolve".into()]));
    state.clear_link_hover();
    let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Err(ClientShellEndpointError {
            code: Some("endpoint_timeout".into()),
            message: "timeout".into(),
        }),
    );
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn ctrl_hover_explicit_wide_label_includes_the_spacer_cell() {
    let mut state = hover_state();
    state.set_endpoint_methods(Some(vec![]));
    let mut next = surface();
    next.surface_revision += 1;
    next.frame.hyperlinks.push("https://example.com/".into());
    next.frame.cells[1].symbol = "界".into();
    next.frame.cells[1].hyperlink = Some(0);
    next.frame.cells[2].symbol = "".into();
    state.set_pane_surface(next);
    let mouse = hover_mouse(&state, 2, 0);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]);
    assert!(outcome.actions.is_empty());
    assert!(outcome.repaint);
    let regions = &state.link_hover.as_ref().unwrap().regions;
    assert_eq!(regions.len(), 1);
    assert_eq!((regions[0].start_col, regions[0].end_col), (1, 2));
}

#[test]
#[ignore = "non-gating fixed-geometry hover composition profile"]
fn ctrl_hover_render_scale_profile() {
    for count in [1, 15] {
        let mut state = hover_state();
        let mut next = surface();
        next.surface_revision += 1;
        next.frame =
            FrameData::from_ratatui_buffer(&Buffer::with_lines(vec!["x".repeat(120); 30]), None);
        let template = next.panes[0].clone();
        next.panes.clear();
        for index in 0..count {
            let mut pane = template.clone();
            pane.pane_id = format!("pane_{index}");
            pane.inner_rect = SurfaceRect {
                x: if count == 1 { 0 } else { (index % 5) * 24 },
                y: if count == 1 { 0 } else { (index / 5) * 10 },
                width: if count == 1 { 120 } else { 24 },
                height: if count == 1 { 30 } else { 10 },
            };
            pane.rect = pane.inner_rect;
            next.panes.push(pane);
        }
        state.set_pane_surface(next);
        state.compose(146, 32).unwrap();
        for active in [false, true] {
            if active {
                let pane = state.hits.panes.last().unwrap();
                let mouse = MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: pane.inner_rect.x + 1,
                    row: pane.inner_rect.y,
                    modifiers: KeyModifiers::CONTROL,
                };
                let id = hover_request(&state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]));
                assert!(resolve_hover(&mut state, &id));
            }
            let mut samples = Vec::new();
            for _ in 0..200 {
                let started = std::time::Instant::now();
                std::hint::black_box(state.compose(146, 32).unwrap());
                samples.push(started.elapsed().as_micros());
            }
            samples.sort_unstable();
            eprintln!(
                "ctrl-hover panes={count} active={active} median_us={} p95_us={}",
                samples[100], samples[190]
            );
            assert!(
                state.pending_requests.is_empty(),
                "composition must never resolve links"
            );
        }
    }
}

#[test]
fn ctrl_hover_groups_contiguous_same_destination_cells_without_server_query() {
    let mut state = hover_state();
    state.set_endpoint_methods(Some(vec![]));
    let mut next = surface();
    next.surface_revision += 1;
    next.frame.hyperlinks.push("https://example.com/".into());
    for idx in 1..7 {
        next.frame.cells[idx].hyperlink = Some(0);
    }
    state.set_pane_surface(next);
    let mouse = hover_mouse(&state, 1, 0);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]);
    assert!(outcome.actions.is_empty());
    assert!(outcome.repaint);
    assert_eq!(state.link_hover.as_ref().unwrap().regions.len(), 2);

    let mut next = state.pane_surface.as_ref().unwrap().clone();
    next.surface_revision += 1;
    next.panes[0].content_revision += 2;
    next.frame.cells[3].hyperlink = None;
    state.set_pane_surface(next);
    state.handle_raw_events(vec![RawInputEvent::Mouse(mouse)]);
    let regions = &state.link_hover.as_ref().unwrap().regions;
    assert_eq!(regions.len(), 1, "an unlinked gap must stop grouping");
    assert_eq!(
        (regions[0].row, regions[0].start_col, regions[0].end_col),
        (0, 1, 2)
    );
}
