use super::*;
use crate::input::{KeybindAction, KeybindMatch, TerminalKey, TextCommit};
use crossterm::event::KeyEventKind;

fn shell(field: usize) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut frame = surface();
    frame.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        viewport_rows: 2,
    });
    state.set_pane_surface(frame);
    state.compose(106, 30).expect("initial shell");
    match field {
        0 => state.open_new_workspace_overlay(),
        1 => state.open_rename_workspace_overlay(),
        2 => state.open_new_tab_overlay(),
        3 => state.open_rename_tab_overlay(),
        4 => state.open_rename_pane_overlay(),
        5 => {
            state.handle_worktree_endpoint_result(
                PendingEndpointKind::PrepareWorktreeCreate {
                    workspace_id: "ws_1".into(),
                },
                Ok(worktree_list_result(None)),
            );
        }
        6 => {
            state.open_navigator_overlay();
            state.handle_input_bytes(b"/");
        }
        7 => {
            state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
                query: TextEditor::default(),
                search_focused: true,
                scroll: 0,
            }))
        }
        8 => {
            state.handle_worktree_endpoint_result(
                PendingEndpointKind::PrepareWorktreeOpen {
                    workspace_id: "ws_1".into(),
                },
                Ok(worktree_list_result(None)),
            );
            state.handle_input_bytes(b"/");
        }
        9 => {
            state.record_binding(
                KeybindMatch::Action(KeybindAction::CopyMode),
                &mut ClientShellInput::default(),
            );
            state.handle_input_bytes(b"/");
        }
        _ => unreachable!(),
    }
    state
}

fn editor(state: &mut ClientShellState) -> &mut TextEditor {
    match state.overlay.as_mut() {
        Some(ClientShellOverlay::Rename(v)) => &mut v.input,
        Some(ClientShellOverlay::Navigator(v)) => &mut v.query,
        Some(ClientShellOverlay::Help(v)) => &mut v.query,
        Some(ClientShellOverlay::WorktreeCreate(v)) => &mut v.branch,
        Some(ClientShellOverlay::WorktreeOpen(v)) => &mut v.query,
        _ => {
            &mut state
                .copy_mode
                .as_mut()
                .expect("copy mode")
                .search_prompt
                .as_mut()
                .expect("prompt")
                .query
        }
    }
}

fn press(state: &mut ClientShellState, code: KeyCode, modifiers: KeyModifiers) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(code, modifiers))])
}

#[test]
fn all_ten_fields_share_keys_text_paste_clipboard_and_local_yank() {
    for field in 0..10 {
        for delivery in 0..5 {
            let mut state = shell(field);
            *editor(&mut state) = TextEditor::from("ab");
            press(&mut state, KeyCode::Left, KeyModifiers::NONE);
            let result = match delivery {
                0 => press(&mut state, KeyCode::Char('X'), KeyModifiers::NONE),
                1 => state.handle_raw_events(vec![RawInputEvent::Key(
                    TerminalKey::new(KeyCode::Char('x'), KeyModifiers::NONE)
                        .with_generated_text(Some("X".into())),
                )]),
                2 => state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("X"))]),
                3 => state.handle_raw_events(vec![RawInputEvent::Paste("X".into())]),
                _ => {
                    let mut result = ClientShellInput::default();
                    assert!(state.handle_modal_paste_shortcut_with(
                        &TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
                        &mut result,
                        || Some("X".into())
                    ));
                    result
                }
            };
            assert!(result.repaint, "field {field}, delivery {delivery}");
            assert!(result.requests.is_empty() && result.actions.is_empty());
            assert_eq!(editor(&mut state).as_str(), "aXb");
            press(&mut state, KeyCode::Char('k'), KeyModifiers::CONTROL);
            press(&mut state, KeyCode::Char('y'), KeyModifiers::CONTROL);
            press(&mut state, KeyCode::Char('y'), KeyModifiers::CONTROL);
            assert_eq!(editor(&mut state).as_str(), "aXbb");
            let mut reopened = shell(field);
            editor(&mut reopened).clear();
            press(&mut reopened, KeyCode::Char('y'), KeyModifiers::CONTROL);
            assert!(editor(&mut reopened).is_empty());
        }
    }
}

#[test]
fn all_fields_normalize_unicode_delivery_and_preserve_key_lifecycle() {
    for field in 0..10 {
        let mut state = shell(field);
        *editor(&mut state) = TextEditor::from("e\u{301}中👩‍💻");
        let left = TerminalKey::new(KeyCode::Left, KeyModifiers::NONE);
        state.handle_raw_events(vec![
            RawInputEvent::Key(left.clone()),
            RawInputEvent::Key(left.clone().with_kind(KeyEventKind::Repeat)),
            RawInputEvent::Key(left.with_kind(KeyEventKind::Release)),
        ]);
        state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("X\r\n\t\x00"))]);
        assert_eq!(
            editor(&mut state).as_str(),
            "e\u{301}X  中👩‍💻",
            "field {field}"
        );
        state.handle_raw_events(vec![RawInputEvent::Key(
            TerminalKey::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            )
            .with_generated_text(Some("β".into())),
        )]);
        assert_eq!(editor(&mut state).as_str(), "e\u{301}X  β中👩‍💻");
        for ch in ['n', 'p', 'c', 'v'] {
            let key =
                TerminalKey::new(KeyCode::Char(ch), KeyModifiers::CONTROL | KeyModifiers::ALT)
                    .with_generated_text(Some("β".into()));
            let previous_len = editor(&mut state).len();
            state.handle_raw_events(vec![RawInputEvent::Key(key)]);
            assert_eq!(
                editor(&mut state).len(),
                previous_len + "β".len(),
                "field {field}, AltGr {ch}"
            );
        }
    }
}

#[test]
fn every_editor_binding_reaches_each_active_field() {
    use KeyCode::*;
    let plain = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let alt = KeyModifiers::ALT;
    for field in 0..10 {
        for (code, modifiers) in [
            (Left, plain),
            (Right, plain),
            (Home, plain),
            (End, plain),
            (Backspace, plain),
            (Delete, plain),
            (Char('a'), ctrl),
            (Char('e'), ctrl),
            (Char('b'), ctrl),
            (Char('f'), ctrl),
            (Char('h'), ctrl),
            (Char('d'), ctrl),
            (Char('b'), alt),
            (Char('f'), alt),
            (Char('u'), ctrl),
            (Char('k'), ctrl),
            (Char('w'), ctrl),
            (Backspace, alt),
            (Backspace, ctrl),
            (Char('d'), alt),
            (Char('y'), ctrl),
        ] {
            let mut state = shell(field);
            let mut expected = TextEditor::from("one e\u{301}中 👩‍💻/two");
            expected.handle_key(&TerminalKey::new(Char('w'), ctrl));
            expected.handle_key(&TerminalKey::new(Left, plain));
            *editor(&mut state) = expected.clone();
            expected
                .handle_key(&TerminalKey::new(code, modifiers))
                .expect("binding");
            let result = press(&mut state, code, modifiers);
            assert_eq!(
                editor(&mut state),
                &expected,
                "field {field}, {code:?} {modifiers:?}"
            );
            assert!(result.requests.is_empty() && result.actions.is_empty());
        }
    }
}

#[test]
fn rename_clear_exceptions_remain_local_and_copy_prompt_yields_to_popup() {
    for field in 0..5 {
        for (code, modifiers) in [
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
            (KeyCode::Backspace, KeyModifiers::SUPER),
        ] {
            let mut state = shell(field);
            *editor(&mut state) = TextEditor::from("name");
            press(&mut state, code, modifiers);
            assert!(state.overlay.is_some());
            assert!(editor(&mut state).is_empty());
        }
    }
    let mut state = shell(9);
    *editor(&mut state) = TextEditor::from("query");
    state.popup_terminal_id = Some("popup-test".into());
    let result = state.handle_raw_events(vec![
        RawInputEvent::Text(TextCommit::new("text")),
        RawInputEvent::Paste("paste".into()),
    ]);
    assert!(
        matches!(&result.requests[..], [ClientMessage::ClientShellPopupInput { terminal_id, events }] if terminal_id == "popup-test" && events.len() == 2)
    );
    assert_eq!(editor(&mut state).as_str(), "query");
    assert!(!state.modal_paste_target_active());
}

#[test]
fn cursor_movement_preserves_filter_selection_scroll_and_branch_error() {
    for field in [5, 6, 7, 8] {
        let mut state = shell(field);
        *editor(&mut state) = TextEditor::from("ab");
        match state.overlay.as_mut().expect("overlay") {
            ClientShellOverlay::WorktreeCreate(v) => {
                v.checkout_path = "sentinel".into();
                v.error = Some("error".into());
            }
            ClientShellOverlay::Navigator(v) => {
                v.scroll = 3;
                v.selected = Some(ClientNavigatorTarget::Pane {
                    endpoint_id: ClientEndpointId::Local,
                    pane_id: "pane_1".into(),
                });
            }
            ClientShellOverlay::Help(v) => v.scroll = 3,
            ClientShellOverlay::WorktreeOpen(v) => v.selected = 3,
            _ => unreachable!(),
        }
        press(&mut state, KeyCode::Home, KeyModifiers::NONE);
        press(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL); // Empty kill must not refresh results.
        match state.overlay.as_ref().expect("overlay") {
            ClientShellOverlay::WorktreeCreate(v) => {
                assert_eq!(v.checkout_path, "sentinel");
                assert_eq!(v.error.as_deref(), Some("error"));
            }
            ClientShellOverlay::Navigator(v) => {
                assert_eq!(v.scroll, 3);
                assert!(v.selected.is_some());
            }
            ClientShellOverlay::Help(v) => assert_eq!(v.scroll, 3),
            ClientShellOverlay::WorktreeOpen(v) => assert_eq!(v.selected, 3),
            _ => unreachable!(),
        }
        press(&mut state, KeyCode::Char('k'), KeyModifiers::CONTROL);
        match state.overlay.as_ref().expect("overlay") {
            ClientShellOverlay::WorktreeCreate(v) => {
                assert_ne!(v.checkout_path, "sentinel");
                assert!(v.error.is_none());
            }
            ClientShellOverlay::Navigator(v) => assert!(v.selected.is_none()),
            ClientShellOverlay::Help(v) => assert_eq!(v.scroll, 0),
            ClientShellOverlay::WorktreeOpen(v) => assert_eq!(v.selected, 0),
            _ => unreachable!(),
        }
    }
}

#[test]
fn busy_worktree_inputs_ignore_edits_paste_and_cancel() {
    for field in [5, 8] {
        let mut state = shell(field);
        *editor(&mut state) = TextEditor::from("ab");
        match state.overlay.as_mut().expect("overlay") {
            ClientShellOverlay::WorktreeCreate(v) => v.creating = true,
            ClientShellOverlay::WorktreeOpen(v) => v.opening = true,
            _ => unreachable!(),
        }
        let before = editor(&mut state).clone();
        for code in [
            KeyCode::Left,
            KeyCode::Backspace,
            KeyCode::Esc,
            KeyCode::Enter,
        ] {
            press(&mut state, code, KeyModifiers::NONE);
        }
        state.handle_raw_events(vec![RawInputEvent::Paste("ignored".into())]);
        assert_eq!(editor(&mut state), &before);
        assert!(!state.modal_paste_target_active());
    }
}

#[test]
fn focused_filters_keep_ctrl_n_p_navigation_and_literal_commands() {
    for field in [6, 7, 8] {
        let mut state = shell(field);
        if let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() {
            navigator.selected = None;
        }
        if let Some(ClientShellOverlay::WorktreeOpen(open)) = state.overlay.as_mut() {
            let mut second = open.entries[0].clone();
            second.path = "/second".into();
            open.entries.push(second);
        }
        state.compose(106, 30).expect("filter frame");
        let before = match state.overlay.as_ref().expect("overlay") {
            ClientShellOverlay::Navigator(v) => format!("{:?}", v.selected),
            ClientShellOverlay::Help(v) => v.scroll.to_string(),
            ClientShellOverlay::WorktreeOpen(v) => v.selected.to_string(),
            _ => unreachable!(),
        };
        press(&mut state, KeyCode::Char('n'), KeyModifiers::CONTROL);
        let after = match state.overlay.as_ref().expect("overlay") {
            ClientShellOverlay::Navigator(v) => format!("{:?}", v.selected),
            ClientShellOverlay::Help(v) => v.scroll.to_string(),
            ClientShellOverlay::WorktreeOpen(v) => v.selected.to_string(),
            _ => unreachable!(),
        };
        assert_ne!(before, after, "field {field}");
        press(&mut state, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(editor(&mut state).is_empty());
        for ch in ['j', 'k', '?'] {
            press(&mut state, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(editor(&mut state).as_str(), "jk?");
    }
}

#[test]
fn all_naming_targets_preserve_submission_and_empty_semantics() {
    use crate::api::schema::Method;
    for field in 0..5 {
        for empty in [false, true] {
            let mut state = shell(field);
            *editor(&mut state) = TextEditor::from(if empty { "  " } else { "  ab " });
            if !empty {
                press(&mut state, KeyCode::Home, KeyModifiers::NONE);
                press(&mut state, KeyCode::Char('X'), KeyModifiers::NONE);
            }
            let result = press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
            assert!(state.overlay.is_none());
            if empty && matches!(field, 1 | 3) {
                assert!(result.actions.is_empty());
                continue;
            }
            let [ClientShellAction::Endpoint { request, .. }] = &result.actions[..] else {
                panic!("naming target {field}");
            };
            let expected = if empty { "" } else { "X  ab" };
            match &request.method {
                Method::WorkspaceCreate(v) => {
                    assert_eq!(v.label.as_deref(), (!empty).then_some(expected))
                }
                Method::WorkspaceRename(v) => assert_eq!(v.label, expected),
                Method::TabCreate(v) => {
                    assert_eq!(v.label.as_deref(), (!empty).then_some(expected))
                }
                Method::TabRename(v) => assert_eq!(v.label, expected),
                Method::PaneRename(v) => assert_eq!(v.label.as_deref(), Some(expected)),
                _ => panic!("wrong method"),
            }
        }
    }
}

#[test]
fn copy_search_owns_prefix_but_parked_prompt_does_not_steal_input() {
    let mut state = shell(9);
    *editor(&mut state) = TextEditor::from("ab");
    press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(state.mode, ClientShellMode::Copy);
    state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("X"))]);
    assert_eq!(editor(&mut state).as_str(), "aXb");
    state.open_rename_pane_overlay();
    assert!(state.modal_paste_target_active());
    state.handle_raw_events(vec![RawInputEvent::Paste("name".into())]);
    assert_eq!(editor(&mut state).as_str(), "name");
    state.overlay = None;
    assert_eq!(editor(&mut state).as_str(), "aXb");
    state.mode = ClientShellMode::Terminal;
    assert!(!state.modal_paste_target_active());
    let input = state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("terminal"))]);
    assert!(
        matches!(&input.requests[..], [ClientMessage::ClientShellPaneInput { events, .. }] if matches!(&events[..], [ClientPaneInputEvent::TextCommit(text)] if text == "terminal"))
    );
    assert_eq!(editor(&mut state).as_str(), "aXb");
    state.mode = ClientShellMode::Copy;
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(state.mode, ClientShellMode::Prefix);
}

#[test]
fn every_field_renders_long_unicode_across_resize_without_mutation() {
    for field in 0..10 {
        let mut state = shell(field);
        *editor(&mut state) = TextEditor::new(&"e\u{301}中👩‍💻".repeat(40), false);
        for position in [KeyCode::Home, KeyCode::End, KeyCode::Left] {
            press(&mut state, position, KeyModifiers::NONE);
            for (width, height) in [(120, 40), (60, 20), (12, 6), (1, 1), (120, 40)] {
                let before = editor(&mut state).clone();
                if let Some(frame) = state.compose(width, height) {
                    if let Some(cursor) = frame.cursor.filter(|cursor| cursor.visible) {
                        assert!(cursor.x < width && cursor.y < height, "field {field}");
                    }
                }
                assert_eq!(editor(&mut state), &before);
            }
        }
    }
}
