use super::*;

const BORDER_ROWS: [&str; 4] = [
    "┌──────────────┐┌──────────────┐",
    "│left          ││right         │",
    "│              ││              │",
    "└──────────────┘└──────────────┘",
];

fn split_pane(pane_id: &str, x: u16, focused: bool) -> PaneSurfacePane {
    PaneSurfacePane {
        pane_id: pane_id.into(),
        content_revision: 0,
        rect: SurfaceRect {
            x,
            y: 0,
            width: 16,
            height: 4,
        },
        inner_rect: SurfaceRect {
            x: x + 1,
            y: 1,
            width: 14,
            height: 2,
        },
        scrollbar_rect: None,
        scroll: None,
        focused,
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        alternate_screen_active: false,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn bordered_split_state() -> ClientShellState {
    let mut snapshot = snapshot();
    let mut second = snapshot.panes[0].clone();
    second.pane_id = "pane_2".into();
    second.focused = false;
    snapshot.panes.push(second);

    let mut surface = surface();
    surface.frame =
        FrameData::from_ratatui_buffer_with_hyperlinks(&Buffer::with_lines(BORDER_ROWS), None, &[]);
    surface.panes = vec![
        split_pane("pane_1", 0, true),
        split_pane("pane_2", 16, false),
    ];

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface);
    state
}

fn pane_rect(state: &ClientShellState, pane_id: &str) -> Rect {
    state
        .hits
        .panes
        .iter()
        .find(|hit| hit.pane_id == pane_id)
        .map(|hit| hit.rect)
        .expect("pane hit")
}

#[test]
fn resize_mode_highlights_focused_pane_frame_and_badges_it() {
    let mut state = bordered_split_state();
    state.mode = ClientShellMode::Resize;
    let frame = state.compose(80, 12).expect("composed frame").frame;
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let palette = state.config.palette.clone();
    let focused = pane_rect(&state, "pane_1");
    let other = pane_rect(&state, "pane_2");

    let (badge_x, badge_y) = cell_symbol_position(&frame, focused, "RESIZE");
    assert_eq!(
        badge_y, focused.y,
        "badge sits on the focused pane's top edge"
    );
    assert_eq!(buffer[(badge_x, badge_y)].bg, palette.mauve);

    for (x, y) in [
        (focused.x, focused.y),
        (focused.x, focused.y + 1),
        (focused.right() - 1, focused.bottom() - 1),
    ] {
        assert_eq!(buffer[(x, y)].fg, palette.mauve, "focused frame at {x},{y}");
        assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));
    }
    for (x, y) in [(other.x, other.y), (other.right() - 1, other.y + 1)] {
        assert_eq!(
            buffer[(x, y)].fg,
            palette.surface1,
            "unfocused frame at {x},{y}"
        );
    }
    // Pane content is not restyled.
    let (text_x, text_y) = cell_symbol_position(&frame, focused, "left");
    assert_eq!(buffer[(text_x, text_y)].fg, ratatui::style::Color::Reset);
}

#[test]
fn leaving_resize_mode_restores_server_pane_frames() {
    let mut state = bordered_split_state();
    let baseline = state.compose(80, 12).expect("terminal frame").frame;

    state.mode = ClientShellMode::Resize;
    let resized = state.compose(80, 12).expect("resize frame").frame;
    assert_ne!(frame_rows(&resized), frame_rows(&baseline));

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.mode, ClientShellMode::Terminal);
    let restored = state.compose(80, 12).expect("restored frame").frame;
    assert_eq!(frame_rows(&restored), frame_rows(&baseline));
    let focused = pane_rect(&state, "pane_1");
    let buffer = restored.to_ratatui_buffer().expect("buffer");
    assert_eq!(
        buffer[(focused.x, focused.y)].fg,
        ratatui::style::Color::Reset
    );
}

#[test]
fn resize_mode_badge_never_covers_borderless_pane_content() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut surface = surface();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["borderless pane text", "second row"]),
        None,
        &[],
    );
    let full = SurfaceRect {
        x: 0,
        y: 0,
        width: 20,
        height: 2,
    };
    surface.panes[0].rect = full;
    surface.panes[0].inner_rect = full;
    state.set_pane_surface(surface);
    let baseline = state.compose(80, 12).expect("terminal frame").frame;
    let pane = pane_rect(&state, "pane_1");

    state.mode = ClientShellMode::Resize;
    let resized = state.compose(80, 12).expect("resize frame").frame;
    let rows = |frame: &FrameData| -> Vec<String> {
        frame_rows(frame)[usize::from(pane.y)..usize::from(pane.bottom())].to_vec()
    };
    assert_eq!(rows(&resized), rows(&baseline));
}
