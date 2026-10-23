use super::*;

const COLS: u16 = 106;
const ROWS: u16 = 20;

fn bidi_config() -> Config {
    let mut config = Config::default();
    config.ui.bidi = true;
    config
}

fn logical_config() -> Config {
    let mut config = Config::default();
    config.ui.bidi = false;
    config
}

/// One pane whose two rows hold `top` and `bottom`, each `width` cells wide.
fn text_surface(top: &str, bottom: &str, width: u16) -> PaneSurfaceFrame {
    let pad = |line: &str| {
        let mut line = line.to_owned();
        while line.chars().count() < usize::from(width) {
            line.push(' ');
        }
        line
    };
    let mut surface = surface();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines([pad(top), pad(bottom)]),
        None,
        &[],
    );
    for pane in &mut surface.panes {
        pane.rect.width = width;
        pane.inner_rect.width = width;
    }
    surface
}

fn shell(config: &Config, surface: PaneSurfaceFrame) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface);
    state.compose(COLS, ROWS).expect("pane frame");
    state
}

fn pane_row(frame: &FrameData, inner: Rect, row: u16) -> String {
    let start = usize::from(inner.y + row) * usize::from(frame.width) + usize::from(inner.x);
    frame.cells[start..start + usize::from(inner.width)]
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect()
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    }
}

#[test]
fn bidi_reorders_pane_rows_and_leaves_everything_else_identical() {
    let off = shell(&logical_config(), text_surface("שלום ab", "PANE", 8))
        .compose(COLS, ROWS)
        .unwrap()
        .frame;
    let mut on_state = shell(&bidi_config(), text_surface("שלום ab", "PANE", 8));
    let on = on_state.compose(COLS, ROWS).unwrap().frame;
    let inner = on_state.hits.panes[0].inner_rect;

    assert_eq!(pane_row(&off, inner, 0), "שלום ab ");
    assert_eq!(pane_row(&on, inner, 0), "םולש ab ");
    assert_eq!(pane_row(&on, inner, 1), "PANE    ");
    for y in 0..on.height {
        for x in 0..on.width {
            let inside = y == inner.y && x >= inner.x && x < inner.x + inner.width;
            if !inside {
                let index = usize::from(y) * usize::from(on.width) + usize::from(x);
                assert_eq!(on.cells[index], off.cells[index], "cell ({x}, {y})");
            }
        }
    }
}

#[test]
fn default_config_reorders_and_disabled_bidi_keeps_logical_order() {
    let mut state = shell(&Config::default(), text_surface("שלום", "PANE", 4));
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, state.hits.panes[0].inner_rect, 0), "םולש");
    assert!(state.bidi_enabled());

    let mut state = shell(&logical_config(), text_surface("שלום", "PANE", 4));
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, state.hits.panes[0].inner_rect, 0), "שלום");
    assert!(state.display_maps.is_empty());
    assert!(!state.bidi_enabled());
}

#[test]
fn a_host_that_always_reorders_text_keeps_pane_rows_logical() {
    let mut config = ClientShellConfig::from_config(&bidi_config());
    config.set_host_reorders_text(true);
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(text_surface("א x", "PANE", 4));
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, state.hits.panes[0].inner_rect, 0), "א x ");
    assert!(state.display_maps.is_empty());
    // The host lays the row out, so patches keep the fast path.
    assert!(matches!(
        state.apply_pane_surface_patch(patch(cells("1"), 2, 0)),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));
    // A live reload keeps the host's permanent mode.
    state.config.apply_live_config(&bidi_config(), &[], &[]);
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert!(pane_row(&frame, state.hits.panes[0].inner_rect, 0).starts_with('א'));
}

#[test]
fn side_by_side_panes_reorder_independently() {
    // The panes are separated by a plain space, which would not split a
    // paragraph: reordering the whole row would draw "דג בא".
    let mut surface = surface();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["אב גד", "PA NE"]),
        None,
        &[],
    );
    surface.panes[0].rect.width = 2;
    surface.panes[0].inner_rect.width = 2;
    let mut right = surface.panes[0].clone();
    right.pane_id = "pane_2".into();
    right.rect.x = 3;
    right.inner_rect.x = 3;
    surface.panes.push(right);
    let mut state = shell(&bidi_config(), surface);
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    let left = state.hits.panes[0].inner_rect;
    let row: String = (left.x..left.x + 5)
        .map(|x| {
            frame.cells[usize::from(left.y) * usize::from(frame.width) + usize::from(x)]
                .symbol
                .clone()
        })
        .collect();
    assert_eq!(row, "בא דג");
    assert_eq!(state.display_maps.len(), 2);
}

fn snapshot_with_bidi_explicit(pane_ids: &[&str]) -> Box<crate::protocol::ClientShellSnapshot> {
    let mut snapshot = snapshot();
    let template = snapshot.panes[0].clone();
    snapshot.panes = ["pane_1", "pane_2"]
        .into_iter()
        .map(|pane_id| crate::protocol::ClientShellPane {
            pane_id: pane_id.into(),
            bidi_explicit: pane_ids.contains(&pane_id),
            ..template.clone()
        })
        .collect();
    Box::new(snapshot)
}

#[test]
fn panes_in_explicit_bidi_mode_are_not_reordered_again() {
    // pane_1 sent CSI 8 l: it already draws visual order, as a nested Herdr
    // does. pane_2 stays implicit and is still laid out.
    let mut surface = surface();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["אב גד", "PA NE"]),
        None,
        &[],
    );
    surface.panes[0].rect.width = 2;
    surface.panes[0].inner_rect.width = 2;
    let mut right = surface.panes[0].clone();
    right.pane_id = "pane_2".into();
    right.rect.x = 3;
    right.inner_rect.x = 3;
    surface.panes.push(right);
    let panes = surface.panes.clone();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&bidi_config()));
    state.set_snapshot(snapshot_with_bidi_explicit(&["pane_1"]));
    state.set_pane_surface(surface.clone());
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    let left = state.hits.panes[0].inner_rect;
    let right = state.hits.panes[1].inner_rect;
    assert_eq!(pane_row(&frame, left, 0), "אב");
    assert_eq!(pane_row(&frame, right, 0), "דג");
    assert_eq!(state.display_maps.len(), 1);
    assert!(!state.display_maps[0].contains(left.x, left.y));
    // Patches on the explicit pane keep the fast path.
    let mut left_patch = patch(cells("x"), 0, 0);
    left_patch.panes = panes.clone();
    assert!(matches!(
        state.apply_pane_surface_patch(left_patch),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));

    // CSI 8 h returns the pane to implicit mode, so it is laid out again.
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&bidi_config()));
    state.set_snapshot(snapshot_with_bidi_explicit(&["pane_1"]));
    state.set_pane_surface(surface);
    state.compose(COLS, ROWS).unwrap();
    state.set_snapshot(snapshot_with_bidi_explicit(&[]));
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, state.hits.panes[0].inner_rect, 0), "בא");
    assert_eq!(state.display_maps.len(), 2);
    let mut left_patch = patch(cells("x"), 0, 0);
    left_patch.panes = panes;
    assert!(matches!(
        state.apply_pane_surface_patch(left_patch),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));
}

#[test]
fn cursor_follows_its_glyph_on_reordered_rows() {
    let mut surface = text_surface("אבגד", "PANE", 4);
    surface.frame.cursor = Some(crate::protocol::CursorState {
        x: 1,
        y: 0,
        visible: true,
        shape: 2,
    });
    let mut state = shell(&bidi_config(), surface);
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    let inner = state.hits.panes[0].inner_rect;
    assert_eq!(pane_row(&frame, inner, 0), "דגבא");
    let cursor = frame.cursor.expect("cursor");
    assert_eq!((cursor.x, cursor.y), (inner.x + 2, inner.y));

    let mut ascii = text_surface("abcd", "PANE", 4);
    ascii.frame.cursor = Some(crate::protocol::CursorState {
        x: 1,
        y: 0,
        visible: true,
        shape: 2,
    });
    let mut state = shell(&bidi_config(), ascii);
    let cursor = state.compose(COLS, ROWS).unwrap().frame.cursor.unwrap();
    assert_eq!(cursor.x, state.hits.panes[0].inner_rect.x + 1);
}

#[test]
fn mouse_selection_uses_logical_columns_and_highlights_the_selected_glyphs() {
    let mut state = shell(&bidi_config(), text_surface("אבגד", "PANE", 4));
    let inner = state.hits.panes[0].inner_rect;
    // Visual "דגבא": א is drawn at column 3 and ב at column 2.
    state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 3,
        inner.y,
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        inner.x + 2,
        inner.y,
    ))]);
    let selection = state.selection.as_ref().expect("selection");
    assert_eq!(selection.ordered_cells(), ((0, 0), (0, 1)));

    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, inner, 0), "דגבא");
    let bg = |col: u16| {
        frame.cells[usize::from(inner.y) * usize::from(frame.width) + usize::from(inner.x + col)].bg
    };
    assert_eq!(bg(2), bg(3), "selected glyphs share the highlight");
    assert_ne!(bg(0), bg(3), "unselected glyphs are not highlighted");
    assert_eq!(bg(0), bg(1));
}

#[test]
fn selection_drag_outside_the_pane_maps_columns_through_the_selected_pane() {
    let mut state = shell(&bidi_config(), text_surface("PANE", "אבגד", 4));
    let inner = state.hits.panes[0].inner_rect;
    // Bottom row draws "דגבא": ב (logical 1) at column 2, ד (logical 3) at column 0.
    state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 2,
        inner.y + 1,
    ))]);
    // Dragging below the pane, under its leftmost column, ends on ד.
    state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        inner.x,
        inner.y + inner.height + 1,
    ))]);
    let selection = state.selection.as_ref().expect("selection");
    assert_eq!(selection.ordered_cells(), ((1, 1), (1, 3)));
}

#[test]
fn dragging_past_the_pane_edge_from_its_last_cell_selects_that_cell() {
    // The clamped endpoint stays on the anchor cell, so only the raw pointer
    // movement can turn the press into a drag.
    for (mut config, line, column) in [
        (logical_config(), "abcd", 3),
        (bidi_config(), "abcd", 3),
        // Visual "דגבא": the rightmost cell draws א, logical column 0.
        (bidi_config(), "אבגד", 0),
    ] {
        config.ui.copy_on_select = false;
        let mut state = shell(&config, text_surface(line, "PANE", 4));
        let inner = state.hits.panes[0].inner_rect;
        let right = inner.x + inner.width - 1;
        state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right,
            inner.y,
        ))]);
        for kind in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(kind, right + 3, inner.y))]);
        }
        let selection = state.selection.as_ref().unwrap_or_else(|| {
            panic!(
                "{line:?} bidi={}: release kept no selection",
                config.ui.bidi
            )
        });
        assert!(!selection.is_just_click(), "{line:?}");
        assert_eq!(
            selection.ordered_cells(),
            ((0, column), (0, column)),
            "{line:?}"
        );
    }
}

fn scrolled_text_surface(top: &str, bottom: &str, offset: u64, max: u64) -> PaneSurfaceFrame {
    let mut surface = text_surface(top, bottom, 4);
    surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: offset,
        max_offset_from_bottom: max,
        viewport_rows: 2,
    });
    surface
}

#[test]
fn selection_autoscroll_remaps_the_pointer_against_the_row_shown_after_scrolling() {
    // The pointer rests on the top row, visual column 0. Before the scroll
    // that row is Hebrew, drawn "דגבא", so the column maps to logical 3. The
    // ASCII row that scrolls under the pointer maps it to logical 0.
    for at_scroll_limit in [false, true] {
        let max = if at_scroll_limit { 1 } else { 5 };
        let mut config = bidi_config();
        config.ui.copy_on_select = false;
        let mut state = shell(&config, scrolled_text_surface("אבגד", "wxyz", 0, max));
        let inner = state.hits.panes[0].inner_rect;
        let top = max as u32;
        state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            inner.x + 1,
            inner.y + 1,
        ))]);
        state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            inner.x,
            inner.y,
        ))]);
        let selection = state.selection.as_ref().expect("selection");
        assert_eq!(selection.ordered_cells(), ((top, 3), (top + 1, 1)));
        assert!(state.selection_autoscroll.is_some());

        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        state.tick_selection_autoscroll(later);
        state.set_pane_surface(scrolled_text_surface("abcd", "אבגד", 1, max));
        state.compose(COLS, ROWS).expect("scrolled frame");
        if at_scroll_limit {
            // The next tick finds no more scrollback and stops, after
            // resolving the endpoint against the row now displayed.
            state.tick_selection_autoscroll(later + std::time::Duration::from_secs(1));
            assert!(state.selection_autoscroll.is_none());
        }
        state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            inner.x,
            inner.y,
        ))]);
        let selection = state
            .selection
            .as_ref()
            .expect("selection survives release");
        assert_eq!(
            selection.ordered_cells(),
            ((top - 1, 0), (top + 1, 1)),
            "at_scroll_limit={at_scroll_limit}"
        );
    }
}

#[test]
fn forwarded_mouse_reports_and_link_clicks_use_logical_columns() {
    let mut surface = text_surface("אבגד", "PANE", 4);
    surface.panes[0].mouse_reporting = true;
    let mut state = shell(&bidi_config(), surface);
    let inner = state.hits.panes[0].inner_rect;
    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 3,
        inner.y,
    ))]);
    assert!(matches!(
        &down.requests[..],
        [ClientMessage::ClientShellPaneInput { events, .. }]
            if matches!(
                &events[..],
                [ClientPaneInputEvent::Mouse {
                    position: ClientMousePosition::Cell { column: 0, row: 0 },
                    ..
                }]
            )
    ));

    let mut state = shell(&bidi_config(), text_surface("אבגד", "PANE", 4));
    let inner = state.hits.panes[0].inner_rect;
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        modifiers: KeyModifiers::CONTROL,
        ..mouse(MouseEventKind::Down(MouseButton::Left), inner.x, inner.y)
    })]);
    assert!(matches!(
        &click.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneLinkActivate(params) if params.col == 3
            )
    ));
}

#[test]
fn sgr_pixel_reports_shift_with_the_column_remap() {
    let mut surface = text_surface("אבגד", "PANE", 4);
    surface.panes[0].mouse_reporting = true;
    surface.panes[0].sgr_pixel_mouse = true;
    surface.panes[0].pixel_width = 40;
    surface.panes[0].pixel_height = 40;
    let mut state = shell(&bidi_config(), surface);
    let inner = state.hits.panes[0].inner_rect;
    let geometry = crate::input::mouse::HostGeometry::new(COLS, ROWS, u32::from(COLS) * 10, 400)
        .expect("host geometry");
    // Pointer 3 px into visual column 3 (glyph א, logical column 0).
    let x = (u32::from(inner.x) + 3) * 10 + 3;
    let y = u32::from(inner.y) * 20 + 1;
    let down = state.handle_pixel_mouse(format!("\x1b[<0;{x};{y}M").as_bytes(), geometry);
    let [ClientMessage::ClientShellPaneInput { events, .. }] = &down.requests[..] else {
        panic!("expected one pane input request");
    };
    let [ClientPaneInputEvent::Mouse { position, .. }] = &events[..] else {
        panic!("expected one mouse event");
    };
    let ClientMousePosition::Pixels { x, column, .. } = position else {
        panic!("expected a pixel report, got {position:?}");
    };
    assert_eq!(*column, 0);
    assert_eq!(*x, 3);
}

fn patch(
    cells: Vec<crate::protocol::CellData>,
    x: u16,
    y: u16,
) -> crate::protocol::PaneSurfacePatch {
    let mut pane = surface().panes.remove(0);
    pane.rect.width = 4;
    pane.inner_rect.width = 4;
    pane.content_revision = 2;
    crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow { x, y, cells }],
        panes: vec![pane],
        cursor: None,
    }
}

fn cells(text: &str) -> Vec<crate::protocol::CellData> {
    FrameData::from_ratatui_buffer_with_hyperlinks(&Buffer::with_lines([text]), None, &[]).cells
}

#[test]
fn ascii_patches_on_left_to_right_rows_keep_the_fast_path() {
    let mut state = shell(&bidi_config(), text_surface("אבגד", "PANE", 4));
    assert!(matches!(
        state.apply_pane_surface_patch(patch(cells("x"), 0, 1)),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));
}

#[test]
fn patches_on_right_to_left_rows_recompose() {
    // The old row holds Hebrew; an ASCII span must still recompose so the
    // whole row is laid out again ('א x' becomes 'א 1', drawn '1 א').
    let mut state = shell(&bidi_config(), text_surface("א x", "PANE", 4));
    assert!(matches!(
        state.apply_pane_surface_patch(patch(cells("1"), 2, 0)),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, state.hits.panes[0].inner_rect, 0), "1 א ");

    // A Hebrew span arriving on an ASCII row recomposes too.
    let mut state = shell(&bidi_config(), text_surface("abcd", "PANE", 4));
    assert!(matches!(
        state.apply_pane_surface_patch(patch(cells("אב"), 0, 1)),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));

    // With BiDi layout off the same patch keeps the fast path.
    let mut state = shell(&logical_config(), text_surface("א x", "PANE", 4));
    assert!(matches!(
        state.apply_pane_surface_patch(patch(cells("1"), 2, 0)),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));
}

#[test]
fn popup_rows_reorder_inside_the_popup_only() {
    let mut surface = surface_with_popup();
    surface.popup.as_mut().unwrap().frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["שלום      ", "", ""]),
        None,
        &[],
    );
    let mut state = shell(&bidi_config(), surface);
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    let popup = state.hits.popup.clone().expect("popup hit");
    let row = pane_row(&frame, popup.inner_rect, 0);
    assert!(row.starts_with("םולש"), "{row:?}");
    assert_eq!(state.popup_display_maps.len(), 1);
    assert!(state.display_maps.is_empty());
}

#[test]
fn live_reload_toggles_bidi_layout() {
    let mut state = shell(&logical_config(), text_surface("שלום", "PANE", 4));
    let diagnostics = state.config.apply_live_config(&bidi_config(), &[], &[]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(state.bidi_enabled());
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, state.hits.panes[0].inner_rect, 0), "םולש");

    state.config.apply_live_config(&logical_config(), &[], &[]);
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(pane_row(&frame, state.hits.panes[0].inner_rect, 0), "שלום");
    assert!(state.display_maps.is_empty());
}

#[test]
fn context_menu_over_reordered_rows_uses_raw_columns() {
    // Every pane row is Hebrew, so the menu opens on top of reordered rows.
    let line = "שלום עולם ".repeat(4);
    let lines: Vec<String> = (0..12).map(|_| line.clone()).collect();
    let mut surface = surface();
    surface.frame =
        FrameData::from_ratatui_buffer_with_hyperlinks(&Buffer::with_lines(lines), None, &[]);
    for pane in &mut surface.panes {
        pane.rect.width = 40;
        pane.rect.height = 12;
        pane.inner_rect.width = 40;
        pane.inner_rect.height = 12;
    }
    let mut state = shell(&bidi_config(), surface);
    assert!(!state.display_maps.is_empty());
    let pane = state.hits.panes[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        pane.x + 1,
        pane.y,
    ))]);
    state.compose(COLS, ROWS).expect("pane context menu");
    let split_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::SplitRight)
            .expect("split right item"),
        _ => panic!("pane context menu"),
    };
    let split = state.hits.context_menu_rows[split_index].0;
    assert!(
        state
            .display_maps
            .iter()
            .any(|map| map.contains(split.x + 1, split.y)
                && map.to_logical(split.x + 1) != split.x + 1),
        "the menu item must sit over a reordered pane row"
    );
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        split.x + 1,
        split.y,
    ))]);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("the context menu item should run");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneSplit(params)
            if params.direction == crate::api::schema::SplitDirection::Right
    ));
}

#[test]
#[ignore = "non-gating fixed-geometry pane display composition profile"]
fn pane_display_render_scale_profile() {
    let ascii = "x".repeat(120);
    let hebrew = "שלום עולם abc 123 ".repeat(7)[..]
        .chars()
        .take(120)
        .collect::<String>();
    for count in [1u16, 15] {
        for (content, line) in [("ascii", &ascii), ("hebrew", &hebrew)] {
            for bidi in [false, true] {
                let config = if bidi {
                    bidi_config()
                } else {
                    logical_config()
                };
                let mut next = surface();
                next.frame = FrameData::from_ratatui_buffer(
                    &Buffer::with_lines(vec![line.clone(); 30]),
                    None,
                );
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
                let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
                state.set_snapshot(Box::new(snapshot()));
                state.set_pane_surface(next);
                state.compose(146, 32).unwrap();
                let mut samples = Vec::new();
                for _ in 0..200 {
                    let started = std::time::Instant::now();
                    std::hint::black_box(state.compose(146, 32).unwrap());
                    samples.push(started.elapsed().as_micros());
                }
                samples.sort_unstable();
                eprintln!(
                    "pane-display panes={count} content={content} bidi={bidi} \
                     median_us={} p95_us={} reordered_rows={}",
                    samples[100],
                    samples[190],
                    state.display_maps.len()
                );
            }
        }
    }
}

fn image_at(
    target: crate::protocol::SurfaceGraphicsTarget,
) -> crate::protocol::SurfaceGraphicsPlacement {
    crate::protocol::SurfaceGraphicsPlacement {
        asset: crate::protocol::SurfaceGraphicsAssetKey {
            source: crate::protocol::SurfaceGraphicsSource::Terminal {
                target,
                image_id: 1,
            },
            image_width: 1,
            image_height: 1,
            format: crate::protocol::SurfaceGraphicsFormat::Rgba,
            data_len: 4,
            data_fingerprint: 1,
        },
        logical_placement_id: 1,
        x: 0,
        y: 0,
        cols: 1,
        rows: 1,
        source_x: 0,
        source_y: 0,
        source_width: 1,
        source_height: 1,
        x_offset: 0,
        y_offset: 0,
        z: 0,
        scrollback_offset: 0,
    }
}

#[test]
fn popup_images_skip_the_popup_and_leave_tiled_panes_reordered() {
    let mut surface = surface_with_popup();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["שלום ab", "PANE"]),
        None,
        &[],
    );
    surface.popup.as_mut().unwrap().frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["שלום      ", "", ""]),
        None,
        &[],
    );
    surface
        .graphics
        .placements
        .push(image_at(crate::protocol::SurfaceGraphicsTarget::Popup {
            terminal_id: "terminal-popup".into(),
        }));
    let mut state = shell(&bidi_config(), surface);
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    let popup = state.hits.popup.clone().expect("popup hit");
    assert!(pane_row(&frame, popup.inner_rect, 0).starts_with("שלום"));
    assert!(state.popup_display_maps.is_empty());
    assert!(!state.display_maps.is_empty());
}

#[test]
fn pane_images_skip_only_their_pane() {
    let mut surface = text_surface("שלום ab", "PANE", 8);
    surface
        .graphics
        .placements
        .push(image_at(crate::protocol::SurfaceGraphicsTarget::Pane {
            pane_id: "pane_1".into(),
        }));
    let mut state = shell(&bidi_config(), surface);
    let frame = state.compose(COLS, ROWS).unwrap().frame;
    assert_eq!(
        pane_row(&frame, state.hits.panes[0].inner_rect, 0),
        "שלום ab "
    );
    assert!(state.display_maps.is_empty());
}

#[test]
fn patches_check_only_the_patched_pane_segment() {
    // Hebrew sits in the right pane only, so an ASCII span in the left pane
    // keeps the fast path, and a span in the right pane recomposes.
    let mut surface = surface();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["ab אב", "PA NE"]),
        None,
        &[],
    );
    surface.panes[0].rect.width = 2;
    surface.panes[0].inner_rect.width = 2;
    let mut right = surface.panes[0].clone();
    right.pane_id = "pane_2".into();
    right.rect.x = 3;
    right.inner_rect.x = 3;
    surface.panes.push(right);
    let panes = surface.panes.clone();
    let mut state = shell(&bidi_config(), surface.clone());

    let mut left_patch = patch(cells("x"), 0, 0);
    left_patch.panes = panes.clone();
    assert!(matches!(
        state.apply_pane_surface_patch(left_patch),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));

    let mut state = shell(&bidi_config(), surface);
    let mut right_patch = patch(cells("x"), 3, 0);
    right_patch.panes = panes;
    assert!(matches!(
        state.apply_pane_surface_patch(right_patch),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));
}
