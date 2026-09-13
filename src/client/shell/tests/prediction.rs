use super::*;

fn remote_shell(enabled: bool, remote: bool) -> ClientShellState {
    let mut config = Config::default();
    config.remote.predict_input = enabled;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.primary_remote = remote;
    state.set_snapshot(Box::new(snapshot()));
    let size = state.surface_size(80, 24);
    let mut initial = surface();
    let rect = SurfaceRect {
        x: 0,
        y: 0,
        width: size.cols,
        height: size.rows,
    };
    initial.panes[0].rect = rect;
    initial.panes[0].inner_rect = rect;
    initial.frame = FrameData::from_ratatui_buffer(
        &Buffer::empty(Rect::new(0, 0, size.cols, size.rows)),
        Some(crate::protocol::CursorState {
            x: 0,
            y: 0,
            visible: true,
            shape: 2,
        }),
    );
    state.set_pane_surface(initial);
    state.compose(80, 24).expect("initial surface");
    state
}

fn echo(state: &mut ClientShellState, text: &str) {
    let mut next = state.pane_surface.clone().expect("authoritative surface");
    for (x, c) in text.chars().enumerate() {
        next.frame.cells[x].symbol = c.to_string();
    }
    next.frame.cursor.as_mut().expect("cursor").x = text.len() as u16;
    next.surface_revision += 1;
    next.panes[0].content_revision += 1;
    state.set_pane_surface(next);
}

fn visible_prefix(state: &mut ClientShellState, count: usize) -> String {
    let area = state.layout(80, 24).pane_surface;
    let frame = state.compose(80, 24).expect("composed surface");
    let start = usize::from(area.y) * usize::from(frame.width) + usize::from(area.x);
    frame.cells[start..start + count]
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect()
}

#[test]
fn remote_prediction_composes_before_echo_and_sparse_patch_confirms_without_duplicates() {
    let mut state = remote_shell(true, true);
    let first = state.handle_input_bytes(b"a");
    assert_eq!(first.requests.len(), 1, "input is still sent once");
    assert_eq!(
        visible_prefix(&mut state, 3),
        "   ",
        "first echo must be learned"
    );
    echo(&mut state, "a");
    let typing = state.handle_input_bytes(b"bc");
    assert!(typing.repaint);
    assert_eq!(visible_prefix(&mut state, 3), "abc");
    let authoritative = state.pane_surface.as_ref().unwrap();
    assert_eq!(authoritative.frame.cells[1].symbol, " ");
    assert_eq!(authoritative.frame.cursor.as_ref().unwrap().x, 1);

    let mut pane = authoritative.panes[0].clone();
    pane.content_revision += 1;
    let mut cells = authoritative.frame.cells[1..3].to_vec();
    cells[0].symbol = "b".into();
    cells[1].symbol = "c".into();
    let mut cursor = authoritative.frame.cursor.clone();
    cursor.as_mut().unwrap().x = 3;
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: authoritative.boot_id.clone(),
        projection_revision: authoritative.projection_revision,
        base_surface_revision: authoritative.surface_revision,
        surface_revision: authoritative.surface_revision + 1,
        rows: vec![crate::protocol::PaneSurfacePatchRow { x: 1, y: 0, cells }],
        panes: vec![pane],
        cursor,
    };
    assert!(
        matches!(
            state.apply_pane_surface_patch(patch),
            ClientPaneSurfacePatchOutcome::Applied(None)
        ),
        "confirmation must redraw speculative cells, including underline removal"
    );
    assert_eq!(visible_prefix(&mut state, 4), "abc ");
    assert!(!state.input_prediction.has_pending());
    let area = state.layout(80, 24).pane_surface;
    let frame = state.compose(80, 24).unwrap();
    let index = usize::from(area.y) * usize::from(frame.width) + usize::from(area.x) + 1;
    assert_eq!(
        frame.cells[index].modifier,
        state.pane_surface.as_ref().unwrap().frame.cells[1].modifier
    );
}

#[test]
fn remote_prediction_is_opt_in_and_never_applies_to_local_panes() {
    assert!(!Config::default().remote.predict_input);
    for (enabled, remote) in [(false, true), (true, false)] {
        let mut state = remote_shell(enabled, remote);
        state.handle_input_bytes(b"a");
        echo(&mut state, "a");
        state.handle_input_bytes(b"b");
        assert_eq!(visible_prefix(&mut state, 2), "a ");
        assert!(!state.input_prediction.has_pending());
    }
}

#[test]
fn remote_prediction_rolls_back_on_timeout_and_starts_a_new_epoch_after_enter() {
    let mut state = remote_shell(true, true);
    state.handle_input_bytes(b"a");
    echo(&mut state, "a");
    state.handle_input_bytes(b"b");
    assert_eq!(visible_prefix(&mut state, 2), "ab");
    let deadline = state.input_prediction.deadline().unwrap();
    assert!(state.timer_delay(deadline).is_zero());
    assert!(state.tick_prediction(deadline));
    assert_eq!(visible_prefix(&mut state, 2), "a ");
    state.handle_input_bytes(b"b");
    echo(&mut state, "ab");
    state.handle_input_bytes(b"c");
    assert_eq!(visible_prefix(&mut state, 3), "abc");
    state.handle_input_bytes(b"\r");
    state.handle_input_bytes(b"s");
    assert_eq!(
        visible_prefix(&mut state, 3),
        "ab ",
        "no speculative password after Enter"
    );
}

#[test]
fn remote_prediction_clears_when_client_consumes_prefix_or_invalidates_surface() {
    for invalidate in [false, true] {
        let mut state = remote_shell(true, true);
        state.handle_input_bytes(b"a");
        echo(&mut state, "a");
        state.handle_input_bytes(b"b");
        assert_eq!(visible_prefix(&mut state, 2), "ab");
        if invalidate {
            state.invalidate_pane_surface();
        } else {
            let prefix = state.handle_input_bytes(b"\x02");
            assert!(prefix.repaint);
            assert!(
                prefix.requests.is_empty(),
                "client prefix does not reach the pane"
            );
        }
        assert!(!state.input_prediction.has_pending());
    }
}

#[test]
fn remote_prediction_does_not_accept_input_while_the_endpoint_is_offline() {
    let mut state = remote_shell(true, true);
    state.handle_input_bytes(b"a");
    echo(&mut state, "a");
    state.handle_input_bytes(b"b");
    assert_eq!(visible_prefix(&mut state, 2), "ab");
    state.mark_endpoint_disconnected(&crate::client::endpoint::ClientEndpointId::Local);
    state.handle_input_bytes(b"c");
    assert_eq!(visible_prefix(&mut state, 3), "a  ");
    assert!(!state.input_prediction.has_pending());
}

fn agent_prompt_shell(
    hidden_cursor: bool,
    alternate_screen: bool,
    mouse: bool,
) -> ClientShellState {
    let mut state = remote_shell(true, true);
    let mut initial = state.pane_surface.clone().expect("agent prompt surface");
    initial
        .frame
        .cursor
        .as_mut()
        .expect("known input anchor")
        .visible = !hidden_cursor;
    initial.panes[0].alternate_screen_active = alternate_screen;
    initial.panes[0].mouse_reporting = mouse;
    initial.surface_revision += 1;
    state.set_pane_surface(initial);
    state.compose(80, 24).expect("agent prompt frame");
    state
}

fn assert_agent_prompt_learns_echo(mut state: ClientShellState) {
    let first = state.handle_input_bytes(b"a");
    assert_eq!(
        first.requests.len(),
        1,
        "training input must still reach the pane once"
    );
    assert_eq!(
        visible_prefix(&mut state, 2),
        "  ",
        "unknown input behavior must not be predicted"
    );
    echo(&mut state, "a");
    let next = state.handle_input_bytes(b"b");
    assert_eq!(
        next.requests.len(),
        1,
        "predicted input must still reach the pane once"
    );
    assert_eq!(
        visible_prefix(&mut state, 2),
        "ab",
        "an exact echo at the same known input anchor should permit the next character locally"
    );
    assert!(next.repaint);
    assert_eq!(
        state.pane_surface.as_ref().unwrap().frame.cells[1].symbol,
        " ",
        "prediction must remain outside the authoritative frame"
    );
}

#[test]
fn remote_prediction_learns_agent_input_at_a_hidden_but_known_cursor_anchor() {
    assert_agent_prompt_learns_echo(agent_prompt_shell(true, false, false));
}

#[test]
fn remote_prediction_learns_agent_input_in_an_alternate_screen() {
    assert_agent_prompt_learns_echo(agent_prompt_shell(false, true, false));
}

#[test]
fn remote_prediction_learns_agent_input_while_mouse_reporting_is_enabled() {
    assert_agent_prompt_learns_echo(agent_prompt_shell(false, false, true));
}

#[test]
fn remote_prediction_moves_pi_software_caret_and_sparse_confirmation_restores_authority() {
    let mut state = agent_prompt_shell(true, false, false);
    let mut initial = state.pane_surface.clone().unwrap();
    let neutral = initial.frame.cells[1].clone();
    assert_eq!((neutral.fg, neutral.bg, neutral.modifier), (0, 0, 0));
    let mut caret = neutral.clone();
    // Pi's reverse-video blank arrives with resolved foreground/background colors,
    // not the REVERSED modifier, in the live endpoint surface.
    caret.fg = 33_554_432;
    caret.bg = 50_331_647;
    initial.frame.cells[0] = caret.clone();
    initial.surface_revision += 1;
    state.set_pane_surface(initial);
    state.compose(80, 24).unwrap();

    assert_eq!(state.handle_input_bytes(b"a").requests.len(), 1);
    assert_eq!(visible_prefix(&mut state, 2), "  ");
    let mut confirmed = state.pane_surface.clone().unwrap();
    confirmed.frame.cells[0] = neutral.clone();
    confirmed.frame.cells[0].symbol = "a".into();
    confirmed.frame.cells[1] = caret.clone();
    confirmed.frame.cursor.as_mut().unwrap().x = 1;
    confirmed.surface_revision += 1;
    confirmed.panes[0].content_revision += 1;
    state.set_pane_surface(confirmed.clone());

    let typing = state.handle_input_bytes(b"b");
    assert_eq!(typing.requests.len(), 1);
    assert_eq!(
        visible_prefix(&mut state, 3),
        "ab ",
        "a confirmed moving software caret must allow the next append locally"
    );
    assert!(typing.repaint);
    assert_eq!(state.pane_surface.as_ref().unwrap(), &confirmed);
    let area = state.layout(80, 24).pane_surface;
    let predicted = state.compose(80, 24).unwrap();
    let start = usize::from(area.y) * usize::from(predicted.width) + usize::from(area.x);
    let predicted_text = &predicted.cells[start + 1];
    assert_eq!(
        (
            predicted_text.fg,
            predicted_text.bg,
            predicted_text.modifier
        ),
        (0, 0, ratatui::style::Modifier::UNDERLINED.bits()),
        "the predicted character must not inherit the software caret's colors"
    );
    assert_eq!(predicted.cells[start + 2], caret);
    let predicted_cursor = predicted.cursor.as_ref().unwrap();
    assert!(!predicted_cursor.visible);
    assert_eq!(predicted_cursor.x, area.x + 2);

    let mut echoed = neutral;
    echoed.symbol = "b".into();
    let mut pane = confirmed.panes[0].clone();
    pane.content_revision += 1;
    let mut cursor = confirmed.frame.cursor.clone();
    cursor.as_mut().unwrap().x = 2;
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: confirmed.boot_id.clone(),
        projection_revision: confirmed.projection_revision,
        base_surface_revision: confirmed.surface_revision,
        surface_revision: confirmed.surface_revision + 1,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 1,
            y: 0,
            cells: vec![echoed, caret],
        }],
        panes: vec![pane],
        cursor,
    };
    assert!(matches!(
        state.apply_pane_surface_patch(patch),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));
    assert!(!state.input_prediction.has_pending());
    let settled = state.compose(80, 24).unwrap();
    assert_eq!(
        settled.cells[start..start + 3],
        state.pane_surface.as_ref().unwrap().frame.cells[..3],
        "confirmation must restore exact authoritative colors and remove prediction underlines"
    );
}

#[test]
fn remote_prediction_learned_prompt_survives_a_typing_pause_longer_than_one_second() {
    let mut state = remote_shell(true, true);
    state.handle_input_bytes(b"a");
    echo(&mut state, "a");
    assert!(
        !state.input_prediction.has_pending(),
        "the training input was fully confirmed"
    );
    let after_pause = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    assert!(
        !state.tick_prediction(after_pause),
        "there is no unconfirmed text to expire"
    );
    let next = state.handle_input_bytes(b"b");
    assert_eq!(
        visible_prefix(&mut state, 2),
        "ab",
        "a quiet unchanged prompt must not require new echo training after every pause"
    );
    assert!(next.repaint);
}

fn update_unrelated_projection_metadata(state: &mut ClientShellState, surface_first: bool) {
    let mut snapshot = state.snapshot.as_ref().expect("snapshot").as_ref().clone();
    snapshot.revision += 1;
    snapshot.workspaces[0].branch = Some("background-metadata-update".into());
    let mut surface = state.pane_surface.clone().expect("same focused prompt");
    surface.projection_revision = snapshot.revision;
    surface.surface_revision += 1;
    if surface_first {
        state.set_pane_surface(surface);
        // Real surfaces can precede their matching JSON snapshot. A compose attempt in
        // between must not treat the temporarily unavailable pair as a changed prompt.
        assert!(state.compose(80, 24).is_none());
        state.set_snapshot(Box::new(snapshot));
    } else {
        state.set_snapshot(Box::new(snapshot));
        assert!(state.compose(80, 24).is_none());
        state.set_pane_surface(surface);
    }
}

#[test]
fn remote_prediction_metadata_only_projection_keeps_learned_prompt_confidence() {
    for surface_first in [false, true] {
        let mut state = remote_shell(true, true);
        state.handle_input_bytes(b"a");
        echo(&mut state, "a");
        update_unrelated_projection_metadata(&mut state, surface_first);
        let next = state.handle_input_bytes(b"b");
        assert_eq!(
            visible_prefix(&mut state, 2),
            "ab",
            "unrelated metadata does not change focused pane identity, geometry or prompt"
        );
        assert!(next.repaint);
    }
}

#[test]
fn remote_prediction_metadata_only_projection_keeps_unconfirmed_prompt_text() {
    for surface_first in [false, true] {
        let mut state = remote_shell(true, true);
        state.handle_input_bytes(b"a");
        echo(&mut state, "a");
        state.handle_input_bytes(b"b");
        assert_eq!(visible_prefix(&mut state, 2), "ab");
        update_unrelated_projection_metadata(&mut state, surface_first);
        assert_eq!(
            visible_prefix(&mut state, 2),
            "ab",
            "an unrelated coherent projection must not erase pending input in the same prompt"
        );
        state.handle_input_bytes(b"c");
        assert_eq!(visible_prefix(&mut state, 3), "abc");
        echo(&mut state, "abc");
        assert_eq!(visible_prefix(&mut state, 3), "abc");
        assert!(!state.input_prediction.has_pending());
    }
}

#[test]
fn remote_prediction_snapshot_focus_change_clears_without_an_intermediate_surface() {
    for other_pane in [Some("pane_2"), None] {
        for pending_input in [false, true] {
            let mut state = remote_shell(true, true);
            state.handle_input_bytes(b"a");
            echo(&mut state, "a");
            if pending_input {
                state.handle_input_bytes(b"b");
                assert_eq!(visible_prefix(&mut state, 2), "ab");
            }
            let original = state.snapshot.as_ref().unwrap().as_ref().clone();
            let mut away = original.clone();
            away.revision += 1;
            away.focused_pane_id = other_pane.map(str::to_owned);
            away.panes[0].focused = false;
            if let Some(pane_id) = other_pane {
                let mut pane = away.panes[0].clone();
                pane.pane_id = pane_id.into();
                pane.focused = true;
                away.panes.push(pane);
            }
            state.set_snapshot(Box::new(away));
            assert!(
                !state.input_prediction.has_pending(),
                "confirmed focus loss must clear speculative input before its surface arrives"
            );
            assert_eq!(state.previous_pane_id.as_deref(), Some("pane_1"));

            // The intermediate surface can be coalesced away by the server. Returning to
            // the same pixels must not restore confidence from before the focus change.
            let mut returned = original;
            returned.revision += 2;
            let mut surface = state.pane_surface.clone().unwrap();
            surface.projection_revision = returned.revision;
            surface.surface_revision += 1;
            state.set_snapshot(Box::new(returned));
            state.set_pane_surface(surface);
            assert_eq!(visible_prefix(&mut state, 2), "a ");
            let next = state.handle_input_bytes(b"c");
            assert_eq!(next.requests.len(), 1);
            assert_eq!(
                visible_prefix(&mut state, 2),
                "a ",
                "the restored prompt must learn its echo again"
            );
            assert_eq!(
                state.previous_pane_id.as_deref(),
                other_pane.or(Some("pane_1"))
            );
        }
    }
}

#[test]
#[ignore = "manual remote prediction sparse-patch and composition scaling profile"]
fn remote_prediction_render_scale_profile() {
    use crate::protocol::render_ansi::BlitEncoder;
    use std::hint::black_box;
    use std::time::Instant;

    const COLS: u16 = 120;
    const ROWS: u16 = 48;
    const WARMUP: u64 = 20;
    const UPDATES: u64 = 64;
    const SAMPLES: usize = 9;

    fn populated_shell(pane_count: usize, prediction: bool) -> ClientShellState {
        let mut config = Config::default();
        config.remote.predict_input = prediction;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.primary_remote = true;
        let mut snapshot = snapshot();
        for index in 1..pane_count {
            let mut pane = snapshot.panes[0].clone();
            pane.pane_id = format!("pane_{}", index + 1);
            pane.focused = false;
            snapshot.panes.push(pane);
        }
        state.set_snapshot(Box::new(snapshot));
        let size = state.surface_size(COLS, ROWS);
        assert!(usize::from(size.rows) >= pane_count * 2);
        let mut initial = surface();
        let pane_template = initial.panes[0].clone();
        initial.panes = (0..pane_count)
            .map(|index| {
                let y = (usize::from(size.rows) * index / pane_count) as u16;
                let bottom = (usize::from(size.rows) * (index + 1) / pane_count) as u16;
                let mut pane = pane_template.clone();
                pane.pane_id = format!("pane_{}", index + 1);
                pane.focused = index == 0;
                pane.rect = SurfaceRect {
                    x: 0,
                    y,
                    width: size.cols,
                    height: bottom - y,
                };
                pane.inner_rect = pane.rect;
                pane
            })
            .collect();
        initial.frame = FrameData::from_ratatui_buffer(
            &Buffer::empty(Rect::new(0, 0, size.cols, size.rows)),
            Some(crate::protocol::CursorState {
                x: 2,
                y: 0,
                visible: true,
                shape: 2,
            }),
        );
        for (index, pane) in initial.panes.iter().enumerate() {
            for y in pane.inner_rect.y..pane.inner_rect.y + pane.inner_rect.height {
                let text = format!(
                    "pane {:02} row {y:02}: populated terminal output ",
                    index + 1
                );
                for x in 0..size.cols {
                    initial.frame.cells[usize::from(y) * usize::from(size.cols) + usize::from(x)]
                        .symbol =
                        char::from(text.as_bytes()[usize::from(x) % text.len()]).to_string();
                }
            }
        }
        for cell in &mut initial.frame.cells[..usize::from(size.cols)] {
            cell.symbol = " ".into();
        }
        initial.frame.cells[0].symbol = "$".into();
        state.set_pane_surface(initial);
        state
            .compose(COLS, ROWS)
            .expect("initial populated surface");
        state.handle_input_bytes(b"a");
        echo(&mut state, "$ a");
        state.handle_input_bytes(b"b");
        assert_eq!(state.input_prediction.has_pending(), prediction);
        state
    }

    fn patches(state: &ClientShellState, count: u64) -> Vec<crate::protocol::PaneSurfacePatch> {
        let surface = state.pane_surface.as_ref().expect("populated surface");
        (0..count)
            .map(|index| {
                let mut pane = surface.panes[0].clone();
                pane.content_revision += index + 1;
                let mut cell = surface.frame.cells[usize::from(surface.frame.width)].clone();
                cell.symbol = if index % 2 == 0 { "x" } else { "y" }.into();
                crate::protocol::PaneSurfacePatch {
                    boot_id: surface.boot_id.clone(),
                    projection_revision: surface.projection_revision,
                    base_surface_revision: surface.surface_revision + index,
                    surface_revision: surface.surface_revision + index + 1,
                    rows: vec![crate::protocol::PaneSurfacePatchRow {
                        x: 0,
                        y: 1,
                        cells: vec![cell],
                    }],
                    panes: vec![pane],
                    cursor: surface.frame.cursor.clone(),
                }
            })
            .collect()
    }

    fn apply_updates(
        state: &mut ClientShellState,
        encoder: &mut BlitEncoder,
        patches: Vec<crate::protocol::PaneSurfacePatch>,
        prediction: bool,
    ) {
        for patch in patches {
            match state.apply_pane_surface_patch(patch) {
                ClientPaneSurfacePatchOutcome::Applied(Some(patch)) => {
                    assert!(
                        !prediction,
                        "pending predictions must take composition fallback"
                    );
                    let encoded = encoder
                        .encode_patch(&patch.rows, patch.cursor.clone(), false)
                        .expect("direct retained patch");
                    black_box(&encoded.bytes);
                    assert!(encoder.commit_patch(&patch.rows, patch.cursor, encoded));
                }
                ClientPaneSurfacePatchOutcome::Applied(None) => {
                    assert!(
                        prediction,
                        "the baseline should retain its direct patch path"
                    );
                    let frame = state.compose(COLS, ROWS).expect("predicted composed frame");
                    let encoded = encoder.encode(&frame, false);
                    black_box(&encoded.bytes);
                    encoder.commit(frame, encoded);
                }
                ClientPaneSurfacePatchOutcome::Rejected => panic!("valid benchmark patch rejected"),
            }
        }
    }

    let mut results = Vec::new();
    for pane_count in [1, 15] {
        let mut medians = Vec::new();
        for prediction in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..SAMPLES {
                let mut state = populated_shell(pane_count, prediction);
                let mut encoder = BlitEncoder::new();
                let initial = state.compose(COLS, ROWS).expect("benchmark initial frame");
                let encoded = encoder.encode(&initial, true);
                encoder.commit(initial, encoded);
                let warmup = patches(&state, WARMUP);
                apply_updates(&mut state, &mut encoder, warmup, prediction);
                let updates = patches(&state, UPDATES);
                let start = Instant::now();
                apply_updates(&mut state, &mut encoder, updates, prediction);
                samples.push(start.elapsed().as_secs_f64() * 1_000_000.0 / UPDATES as f64);
                assert_eq!(
                    state.input_prediction.has_pending(),
                    prediction,
                    "each sample must finish before its prediction deadline"
                );
            }
            samples.sort_by(f64::total_cmp);
            medians.push(samples[SAMPLES / 2]);
        }
        println!(
            "remote prediction {COLS}x{ROWS}, {pane_count} populated panes: direct patch {:.2} us/update; pending prediction {:.2} us/update ({:.2}x, +{:.2} us); median of {SAMPLES} samples x {UPDATES} updates, {WARMUP} warmups; excludes input setup, decoding and terminal I/O",
            medians[0], medians[1], medians[1] / medians[0], medians[1] - medians[0]
        );
        results.push(medians);
    }
    println!(
        "remote prediction 1 -> 15 populated-pane scaling: direct {:.2}x (+{:.2} us/update); pending {:.2}x (+{:.2} us/update)",
        results[1][0] / results[0][0], results[1][0] - results[0][0],
        results[1][1] / results[0][1], results[1][1] - results[0][1]
    );
}
