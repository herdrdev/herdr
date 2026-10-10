use super::*;

#[test]
fn host_appearance_prefers_explicit_reports_over_background_inference() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.theme_runtime.auto_switch = true;

    let light = crate::app::client_palette_for_appearance(
        &state.config.theme_runtime,
        crate::terminal_theme::HostAppearance::Light,
    );
    let dark = crate::app::client_palette_for_appearance(
        &state.config.theme_runtime,
        crate::terminal_theme::HostAppearance::Dark,
    );

    let inferred = state.handle_raw_events(vec![RawInputEvent::HostDefaultColor {
        kind: crate::terminal_theme::DefaultColorKind::Background,
        color: crate::terminal_theme::RgbColor {
            r: 255,
            g: 255,
            b: 255,
        },
    }]);
    assert!(inferred.repaint);
    assert!(matches!(
        inferred.requests.as_slice(),
        [ClientMessage::ClientShellHostTheme {
            update: crate::protocol::ClientHostThemeUpdate::DefaultColor {
                kind: crate::protocol::ClientHostDefaultColorKind::Background,
                ..
            }
        }]
    ));
    assert_eq!(
        state.host_appearance,
        Some(crate::terminal_theme::HostAppearance::Light)
    );
    assert!(!state.host_appearance_explicit);
    assert_eq!(state.config.palette, light);

    let explicit = state.handle_raw_events(vec![RawInputEvent::HostColorSchemeChanged(
        crate::terminal_theme::HostAppearance::Dark,
    )]);
    assert!(explicit.repaint);
    assert!(explicit.query_host_theme);
    assert!(matches!(
        explicit.requests.as_slice(),
        [ClientMessage::ClientShellHostTheme {
            update: crate::protocol::ClientHostThemeUpdate::Appearance(
                crate::protocol::ClientHostAppearance::Dark
            )
        }]
    ));
    assert_eq!(
        state.host_appearance,
        Some(crate::terminal_theme::HostAppearance::Dark)
    );
    assert!(state.host_appearance_explicit);
    assert_eq!(state.config.palette, dark);

    let ignored = state.handle_raw_events(vec![RawInputEvent::HostDefaultColor {
        kind: crate::terminal_theme::DefaultColorKind::Background,
        color: crate::terminal_theme::RgbColor {
            r: 255,
            g: 255,
            b: 255,
        },
    }]);
    assert!(!ignored.repaint);
    assert_eq!(ignored.requests.len(), 1);
    assert_eq!(
        state.host_appearance,
        Some(crate::terminal_theme::HostAppearance::Dark)
    );
    assert_eq!(state.config.palette, dark);
}

#[test]
fn full_host_palette_response_is_sent_as_one_theme_update() {
    use std::fmt::Write as _;

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut responses = String::new();
    for index in 0..=u8::MAX {
        let _ = write!(responses, "\x1b]4;{index};rgb:1111/2222/3333\x1b\\");
    }

    let outcome = state.handle_input_bytes(responses.as_bytes());

    let [ClientMessage::ClientShellHostTheme {
        update: crate::protocol::ClientHostThemeUpdate::PaletteColors(colors),
    }] = outcome.requests.as_slice()
    else {
        panic!(
            "expected one batched palette update, got {} requests",
            outcome.requests.len()
        );
    };
    assert_eq!(colors.len(), 256);
    assert_eq!(
        colors.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
        (0..=u8::MAX).collect::<Vec<_>>()
    );
}

#[test]
fn modal_paste_shortcut_modifiers_are_platform_specific() {
    let key = |code, modifiers| crate::input::TerminalKey::new(code, modifiers);
    for macos in [false, true] {
        assert!(!input::is_modal_paste_shortcut_for_platform(
            &key(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            macos
        ));
    }

    assert!(input::is_modal_paste_shortcut_for_platform(
        &key(KeyCode::Char('v'), KeyModifiers::CONTROL),
        false
    ));
    assert!(input::is_modal_paste_shortcut_for_platform(
        &key(
            KeyCode::Char('V'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        ),
        false
    ));
    assert!(!input::is_modal_paste_shortcut_for_platform(
        &key(KeyCode::Char('v'), KeyModifiers::SUPER),
        false
    ));
    assert!(input::is_modal_paste_shortcut_for_platform(
        &key(KeyCode::Char('v'), KeyModifiers::CONTROL),
        true
    ));
    assert!(input::is_modal_paste_shortcut_for_platform(
        &key(KeyCode::Char('v'), KeyModifiers::SUPER),
        true
    ));
    assert!(!input::is_modal_paste_shortcut_for_platform(
        &key(KeyCode::Char('v'), KeyModifiers::ALT),
        true
    ));
}

#[test]
fn modal_paste_inserts_clipboard_text_through_overlay_text_path() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
        title: "rename pane",
        input: TextEditor::new("replace me", true),
        target: ClientRenameTarget::Pane {
            pane_id: "pane_1".into(),
        },
    }));
    let mut outcome = ClientShellInput::default();
    let key = crate::input::TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL);

    assert!(
        state.handle_modal_paste_shortcut_with(&key, &mut outcome, || {
            Some("feature/pasted".into())
        })
    );
    assert!(outcome.repaint);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay { ref input, .. }))
            if input.as_str() == "feature/pasted"
    ));
}

#[test]
fn client_shell_graphics_follow_final_shell_origin_and_local_overlay_visibility() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    let key = crate::protocol::SurfaceGraphicsAssetKey {
        source: crate::protocol::SurfaceGraphicsSource::Terminal {
            target: crate::protocol::SurfaceGraphicsTarget::Pane {
                pane_id: "pane_1".into(),
            },
            image_id: 1,
        },
        image_width: 1,
        image_height: 1,
        format: crate::protocol::SurfaceGraphicsFormat::Rgba,
        data_len: 4,
        data_fingerprint: 17,
    };
    pane_surface.graphics = crate::protocol::SurfaceGraphicsScene {
        assets: vec![crate::protocol::SurfaceGraphicsAsset {
            key: key.clone(),
            data: vec![1, 2, 3, 4],
        }],
        placements: vec![crate::protocol::SurfaceGraphicsPlacement {
            asset: key,
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
        }],
        retained_assets: Vec::new(),
    };
    state.set_pane_surface(pane_surface);

    let visible = state.compose(106, 20).expect("visible graphics frame");
    let visible = visible.graphics.clone().into_inline_bytes();
    let visible = String::from_utf8_lossy(&visible);
    assert!(visible.contains("a=t,t=d"));
    assert!(visible.contains("\u{1b}[2;27H"));

    state.overlay = Some(ClientShellOverlay::Onboarding);
    let uncovered = state.compose(106, 20).expect("overlay frame");
    assert!(
        !String::from_utf8_lossy(&uncovered.graphics.clone().into_inline_bytes()).contains("a=d")
    );
    assert!(
        String::from_utf8_lossy(&uncovered.graphics.clone().into_inline_bytes()).contains("a=p")
    );

    state.overlay = None;
    let restored = state.compose(106, 20).expect("restored graphics frame");
    let restored = restored.graphics.clone().into_inline_bytes();
    let restored = String::from_utf8_lossy(&restored);
    assert!(restored.contains("a=p"));
    assert!(!restored.contains("a=t,t=d"));
}

#[test]
fn delayed_link_fallback_does_not_replay_against_changed_geometry() {
    for url in [
        None,
        Some("https://example.test"),
        Some("file:///C:/Code/note.md"),
    ] {
        for changed in 0..4 {
            let mut state =
                ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
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
            match changed {
                0 => state.hits.panes[0].inner_rect.x += 1,
                1 => state.mode = ClientShellMode::Navigate,
                2 => state.config.mouse_capture = false,
                _ => {
                    state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
                        query: TextEditor::default(),
                        search_focused: false,
                        scroll: 0,
                    }))
                }
            }
            let (_, actions) = state.handle_endpoint_result(
                "boot-1",
                &request.id,
                Ok(crate::api::schema::ResponseResult::PaneLinkActivated {
                    url: url.map(str::to_owned),
                    handled: false,
                }),
            );
            assert!(
                actions.is_empty(),
                "late replies must not replay or open links"
            );
            assert!(state.url_click_consumes_until_up);
        }
    }
}

#[test]
fn invalid_experimental_reload_keeps_input_source_preference() {
    let mut shell = ClientShellConfig::from_config(&Config::default());
    shell.switch_ascii_input_source_in_prefix = true;
    let config = Config::default();
    shell.apply_live_config(&config, &[], &["experimental".to_owned()]);
    assert!(shell.switch_ascii_input_source_in_prefix);
    shell.apply_live_config(&config, &[], &[]);
    assert!(!shell.switch_ascii_input_source_in_prefix);
}

fn pane_key_events(
    outcome: &ClientShellInput,
) -> Vec<(
    crate::protocol::ClientKeyCode,
    crate::protocol::ClientKeyKind,
)> {
    outcome
        .requests
        .iter()
        .filter_map(|request| match request {
            ClientMessage::ClientShellPaneInput { events, .. } => Some(events),
            _ => None,
        })
        .flatten()
        .filter_map(|event| match event {
            ClientPaneInputEvent::Key { code, kind, .. } => Some((code.clone(), *kind)),
            _ => None,
        })
        .collect()
}

#[test]
fn text_press_release_survives_shift_released_first_and_never_becomes_a_repeat() {
    use crate::protocol::{ClientKeyCode, ClientKeyKind};

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_host_reports_key_releases(true);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    // Shift+A arrives as text; Shift is let go before A, so the release names
    // plain `a` with no shifted alternate.
    let press = state.handle_input_bytes(b"A");
    assert_eq!(
        pane_key_events(&press),
        [(ClientKeyCode::Char('A'), ClientKeyKind::Press)]
    );
    let release = state.handle_input_bytes(b"\x1b[97;1:3u");
    assert_eq!(
        pane_key_events(&release),
        [(ClientKeyCode::Char('a'), ClientKeyKind::Release)]
    );

    // A text press whose release never arrived must not turn the next press
    // into a repeat routed to the old target.
    let _ = state.handle_input_bytes(b"%");
    let again = state.handle_input_bytes(b"%");
    assert_eq!(
        pane_key_events(&again),
        [(ClientKeyCode::Char('%'), ClientKeyKind::Press)]
    );
}

#[test]
fn raw_backspace_follows_the_host_tty_erase_character() {
    use crate::protocol::{ClientKeyCode, ClientKeyKind};

    // #3244: MobaXterm sends 0x08 for Backspace and its tty says erase is ^H.
    for (erase, plain, alt) in [
        (
            Some(0x08),
            ClientKeyCode::Backspace,
            ClientKeyCode::Backspace,
        ),
        (
            Some(0x7f),
            ClientKeyCode::Char('h'),
            ClientKeyCode::Char('h'),
        ),
        (None, ClientKeyCode::Char('h'), ClientKeyCode::Char('h')),
    ] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_host_erase_byte(erase);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());

        let press = state.handle_input_bytes(b"\x08");
        assert_eq!(
            pane_key_events(&press),
            [(plain.clone(), ClientKeyKind::Press)],
            "erase {erase:?}"
        );
        let alt_press = state.handle_input_bytes(b"\x1b\x08");
        assert_eq!(
            pane_key_events(&alt_press),
            [(alt, ClientKeyKind::Press)],
            "alt, erase {erase:?}"
        );
    }
}

#[test]
fn shifted_punctuation_release_after_shift_reaches_the_pane() {
    use crate::protocol::{ClientKeyCode, ClientKeyKind};

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_host_reports_key_releases(true);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    // "?" arrives as text; with Shift let go first the release names only the
    // unshifted key, which no layout-free rule can relate to "?".
    let _ = state.handle_input_bytes(b"?");
    let release = state.handle_input_bytes(b"\x1b[47;1:3u");
    assert_eq!(
        pane_key_events(&release),
        [(ClientKeyCode::Char('/'), ClientKeyKind::Release)]
    );
    // Nothing is left to be released later as a stale "?".
    let blur = state.handle_input_bytes(b"\x1b[O");
    assert!(pane_key_events(&blur).is_empty());

    // With two text keys held the release is ambiguous and nothing is taken.
    let _ = state.handle_input_bytes(b"?");
    let _ = state.handle_input_bytes(b"!");
    let ambiguous = state.handle_input_bytes(b"\x1b[47;1:3u");
    assert!(pane_key_events(&ambiguous).is_empty());
}

#[test]
fn held_key_reports_keep_their_release_around_ime_commits_and_focus_loss() {
    use crate::protocol::{ClientKeyCode, ClientKeyKind};

    let new_state = || {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_host_reports_key_releases(true);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state
    };

    // A key report carrying its text (report-all host) is a real held key:
    // losing focus releases it.
    let mut state = new_state();
    let _ = state.handle_input_bytes(b"\x1b[97;;97u");
    let blur = state.handle_input_bytes(b"\x1b[O");
    assert_eq!(
        pane_key_events(&blur),
        [(ClientKeyCode::Char('a'), ClientKeyKind::Release)]
    );

    // Many IME commits while a key is held never displace that key: its
    // release still reaches the pane as the held key.
    let mut state = new_state();
    let _ = state.handle_input_bytes(b"\x1b[120;;120u");
    for ch in
        "\u{65e5}\u{672c}\u{8a9e}\u{d55c}\u{ad6d}\u{c5b4}\u{4e2d}\u{6587}\u{3042}\u{3044}\u{3046}"
            .chars()
    {
        let _ = state.handle_input_bytes(ch.to_string().as_bytes());
    }
    let release = state.handle_input_bytes(b"\x1b[120;1:3u");
    assert_eq!(
        pane_key_events(&release),
        [(ClientKeyCode::Char('x'), ClientKeyKind::Release)]
    );
}

#[test]
fn non_latin_ctrl_chord_release_matches_after_ctrl_is_let_go() {
    use crate::protocol::{ClientKeyCode, ClientKeyKind};

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_host_reports_key_releases(true);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    // Russian layout: Ctrl+\u{441} is the C key; Ctrl goes up before C.
    let press = state.handle_input_bytes(b"\x1b[1089::99;5u");
    assert_eq!(
        pane_key_events(&press),
        [(ClientKeyCode::Char('c'), ClientKeyKind::Press)]
    );
    let release = state.handle_input_bytes(b"\x1b[1089::99;1:3u");
    assert_eq!(
        pane_key_events(&release),
        [(ClientKeyCode::Char('c'), ClientKeyKind::Release)]
    );

    // Plain Cyrillic typing still releases as its own key.
    let _ = state.handle_input_bytes(b"\x1b[1089::99;;1089u");
    let release = state.handle_input_bytes(b"\x1b[1089::99;1:3u");
    assert_eq!(
        pane_key_events(&release),
        [(ClientKeyCode::Char('\u{441}'), ClientKeyKind::Release)]
    );
}

#[test]
fn unmatched_release_never_takes_a_held_key_while_herdr_owns_another_press() {
    use crate::protocol::{ClientKeyCode, ClientKeyKind};

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_host_reports_key_releases(true);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    // Hold `a` in the pane, then press the prefix: Herdr owns that press.
    let _ = state.handle_input_bytes(b"a");
    let _ = state.handle_input_bytes(b"\x1b[98;5u");
    // A release the leases cannot name must not be charged to the held `a`.
    let stray = state.handle_input_bytes(b"\x1b[47;1:3u");
    assert!(pane_key_events(&stray).is_empty());
    let release = state.handle_input_bytes(b"\x1b[97;1:3u");
    assert_eq!(
        pane_key_events(&release),
        [(ClientKeyCode::Char('a'), ClientKeyKind::Release)]
    );
}

#[test]
fn physical_release_uses_the_leased_press_code_with_current_modifiers() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let press = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty())
        .with_windows_record(crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 0x58,
            virtual_scan_code: 0x2d,
            unicode: 0,
            control_key_state: 0,
        });
    state.handle_raw_events(vec![RawInputEvent::Key(press)]);
    let release = crate::input::TerminalKey::new(KeyCode::Char('z'), KeyModifiers::SHIFT)
        .with_kind(crossterm::event::KeyEventKind::Release)
        .with_windows_record(crate::input::WindowsKeyRecord {
            key_down: false,
            repeat_count: 1,
            virtual_key_code: 0x5a,
            virtual_scan_code: 0x2d,
            unicode: 0,
            control_key_state: 0x0010,
        });

    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(release)]);

    assert!(matches!(
        &outcome.requests[..],
        [ClientMessage::ClientShellPaneInput { events, .. }]
            if matches!(
                &events[..],
                [ClientPaneInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('x'),
                    modifiers,
                    kind: crate::protocol::ClientKeyKind::Release,
                    physical_key_id: Some(0x2d),
                    ..
                }] if *modifiers == KeyModifiers::SHIFT.bits()
            )
    ));
}

#[test]
fn highlighted_search_match_copies_after_in_flight_repeat() {
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

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('/'),
        KeyModifiers::empty(),
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "needle",
    ))]);
    let initial = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()),
    )]);
    let [ClientShellAction::Endpoint { request, .. }] = &initial.actions[..] else {
        panic!("initial search request");
    };
    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(copy_search_result(matches.clone(), Some(0))),
    );
    let repeat = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('n'),
        KeyModifiers::empty(),
    ))]);
    let [ClientShellAction::Endpoint { request, .. }] = &repeat.actions[..] else {
        panic!("repeat search request");
    };
    let repeat_id = request.id.clone();

    let early_copy = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()),
    )]);
    assert!(early_copy.actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Copy);

    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &repeat_id,
        Ok(copy_search_result(matches, Some(1))),
    );
    assert_eq!(state.mode, ClientShellMode::Terminal);
    let selection_request_id = actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. }
                if matches!(
                    request.method,
                    crate::api::schema::Method::PaneSelectionRead(_)
                ) =>
            {
                Some(request.id.clone())
            }
            _ => None,
        })
        .expect("deferred selection read");
    let (_, clipboard) = state.handle_endpoint_result(
        "boot-1",
        &selection_request_id,
        Ok(crate::api::schema::ResponseResult::PaneSelection {
            pane_id: "pane_1".into(),
            text: "needle".into(),
        }),
    );
    assert!(matches!(
        &clipboard[..],
        [ClientShellAction::ClipboardWrite(bytes)] if bytes == b"needle"
    ));
}

#[test]
fn pixel_host_reports_use_cells_without_target_pixel_mode_and_release_outside() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].mouse_reporting = true;
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].clone();
    let geometry =
        crate::input::mouse::HostGeometry::new(106, 20, 1060, 400).expect("host geometry");
    let x = u32::from(pane.inner_rect.x) * 10 + 21;
    let y = u32::from(pane.inner_rect.y) * 20 + 21;

    let down = state.handle_pixel_mouse(format!("\x1b[<0;{x};{y}M").as_bytes(), geometry);
    assert!(matches!(
        &down.requests[..],
        [ClientMessage::ClientShellPaneInput { events, .. }]
            if matches!(
                &events[..],
                [ClientPaneInputEvent::Mouse {
                    position: ClientMousePosition::Cell { column: 2, row: 1 },
                    ..
                }]
            )
    ));

    state.hits.panes.clear();
    let release = state.handle_pixel_mouse(b"\x1b[<0;1;1m", geometry);
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
                        position: ClientMousePosition::Cell { .. },
                        ..
                    }]
                )
    ));
    assert!(state.pane_mouse_gesture.is_none());
}

#[test]
fn shell_targets_unconsumed_input_and_keeps_prefix_local() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));

    let text = state.handle_input_bytes(b"hello");
    assert_eq!(text.requests.len(), 1);
    let ClientMessage::ClientShellPaneInput { pane_id, events } = &text.requests[0] else {
        panic!("expected targeted pane input");
    };
    assert_eq!(pane_id, "pane_1");
    assert_eq!(events.len(), 5);
    assert!(matches!(
        &events[0],
        ClientPaneInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('h'),
            generated_text: Some(text),
            ..
        } if text == "h"
    ));

    let interrupt = state.handle_input_bytes(b"\x1b[99;5u");
    assert_eq!(interrupt.requests.len(), 1);
    let ClientMessage::ClientShellPaneInput { events, .. } = &interrupt.requests[0] else {
        panic!("expected semantic interrupt");
    };
    assert!(matches!(
        &events[..],
        [ClientPaneInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('c'),
            modifiers,
            kind: crate::protocol::ClientKeyKind::Press,
            ..
        }] if *modifiers == KeyModifiers::CONTROL.bits()
    ));

    let alt = state.handle_input_bytes(b"\x1b[120;3u");
    let ClientMessage::ClientShellPaneInput { events, .. } = &alt.requests[0] else {
        panic!("expected semantic alt key");
    };
    assert!(matches!(
        &events[..],
        [ClientPaneInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers,
            ..
        }] if *modifiers == KeyModifiers::ALT.bits()
    ));
    assert!(!state.handle_input_bytes(&[0x02]).detach);
    let detach = state.handle_input_bytes(b"q");
    assert!(detach.detach);
    assert!(detach.requests.is_empty());
}

#[test]
fn pane_key_release_keeps_the_press_target() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));

    let press = state.handle_input_bytes(b"\x1b[99;5u");
    let release = state.handle_input_bytes(b"\x1b[99;5:3u");
    let ClientMessage::ClientShellPaneInput {
        pane_id: press_target,
        ..
    } = &press.requests[0]
    else {
        panic!("expected targeted press");
    };
    let ClientMessage::ClientShellPaneInput {
        pane_id: release_target,
        events,
    } = &release.requests[0]
    else {
        panic!("expected targeted release");
    };
    assert_eq!(release_target, press_target);
    assert!(matches!(
        &events[..],
        [ClientPaneInputEvent::Key {
            kind: crate::protocol::ClientKeyKind::Release,
            ..
        }]
    ));
}

#[test]
fn help_overlay_uses_live_keymap_and_owns_filter_state() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::Help),
        &mut open,
    );
    let initial = state.compose(106, 30).expect("help overlay");
    let text = initial
        .cells
        .chunks(initial.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("keybinds"));
    assert!(text.contains("prefix mode"));

    assert!(state.handle_input_bytes(b"/").actions.is_empty());
    assert!(state.handle_input_bytes(b"workspace").actions.is_empty());
    let filtered = state.compose(106, 30).expect("filtered help");
    let text = filtered
        .cells
        .chunks(filtered.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("workspace navigation"));
    assert!(!text.contains("prefix mode"));
    assert!(filtered
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.visible));

    assert!(state.handle_input_bytes(b"\x1b").repaint);
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));
    assert!(state.handle_input_bytes(b"\x1b").repaint);
    assert!(state.overlay.is_none());
}

#[test]
fn rename_pane_empty_value_is_preserved_as_a_clear_request() {
    let mut snapshot = snapshot();
    snapshot.panes[0].label = Some("build".into());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::RenamePane),
        &mut open,
    );
    assert!(state.handle_input_bytes(&[0x15]).actions.is_empty());
    let save = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
        panic!("pane rename should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneRename(params)
            if params.pane_id == "pane_1" && params.label.as_deref() == Some("")
    ));
}

#[test]
fn styled_client_composition_preserves_pane_hyperlinks() {
    for uri in [
        "https://example.test",
        "obsidian://open?vault=meta&file=note.md",
    ] {
        for draw_host_cursor in [false, true] {
            let mut state =
                ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
            state.set_snapshot(Box::new(snapshot()));
            let mut pane_surface = surface();
            let linked = Buffer::with_lines(["LIVE", "PANE"]);
            pane_surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
                &linked,
                None,
                &[((0, 0), "L".into(), uri.into())],
            );
            state.set_pane_surface(pane_surface);
            let mut selection =
                crate::selection::Selection::absolute_range("pane_1".to_owned(), (0, 0), (0, 1));
            assert!(selection.finish());
            state.selection = Some(selection);
            let frame = state.compose(106, 20).expect("composed frame");
            let hit = &state.hits.panes[0];
            let index = usize::from(hit.inner_rect.y) * usize::from(frame.width)
                + usize::from(hit.inner_rect.x);
            let link = frame.cells[index].hyperlink.expect("linked cell") as usize;
            assert_eq!(frame.hyperlinks[link], uri);
            let x = hit.inner_rect.x;
            let y = hit.inner_rect.y;
            let mut client = crate::client::state::ClientState::test_new();
            client.draw_host_cursor = draw_host_cursor;
            let mut terminal = crate::ghostty::Terminal::new(106, 20, 0).unwrap();
            for (phase, (capture, repaint)) in [
                (false, false),
                (true, false),
                (false, false),
                (true, true),
                (true, false),
                (false, true),
            ]
            .into_iter()
            .enumerate()
            {
                client.shell_mouse_capture_preference = capture;
                // Effective mouse reporting may still be requested by a pane app.
                client.mouse_capture_active = true;
                client.repaint_pending = repaint;
                let mut frame = state.compose(106, 20).expect("linked semantic frame");
                let link = frame.cells[index].hyperlink.expect("retained link") as usize;
                assert_eq!(frame.hyperlinks[link], uri);
                // Change the linked cell on the diff pass so it must be painted again.
                if phase == 4 {
                    frame.frame.cells[index].symbol = "D".into();
                }
                frame.frame.cursor = Some(crate::protocol::CursorState {
                    x: x + 1,
                    y,
                    visible: true,
                    shape: Default::default(),
                });
                let expected = frame.cells[index].symbol.clone();
                let mut output = Vec::new();
                assert!(client.try_present_frame_to(&mut output, frame));
                terminal.write(&output);
                assert_eq!(
                    terminal.viewport_hyperlink_uri(x, u32::from(y)).unwrap(),
                    (!capture).then_some(uri.to_owned()),
                    "host link ownership must follow configured capture for {uri}"
                );
                let (_, label) = terminal.screen_cell(x, u32::from(y)).unwrap();
                assert_eq!(label, expected.chars().map(u32::from).collect::<Vec<_>>());
            }
        }
    }
}

#[test]
fn every_configured_prefix_enters_prefix_mode() {
    let mut config = Config::default();
    config.keys.prefix =
        crate::config::BindingConfig::Many(vec!["ctrl+space".to_owned(), "ctrl+s".to_owned()]);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));

    for combo in [
        (KeyCode::Char(' '), KeyModifiers::CONTROL),
        (KeyCode::Char('s'), KeyModifiers::CONTROL),
    ] {
        let _ = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
            combo.0, combo.1,
        ))]);
        assert_eq!(state.mode, ClientShellMode::Prefix);

        let _ = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Esc,
            KeyModifiers::empty(),
        ))]);
        assert_eq!(state.mode, ClientShellMode::Terminal);
    }
}
