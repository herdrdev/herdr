use crossterm::event::{KeyCode, KeyModifiers};

use crate::{
    app::{App, InputSourceId},
    input::TerminalKey,
};

use super::{ConsumedInputLease, InputLeaseKey};

fn is_retained_selection_copy_key(key: &TerminalKey) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C'))
        && matches!(key.modifiers, KeyModifiers::CONTROL | KeyModifiers::SUPER)
}

impl App {
    pub(super) fn dispatch_pending_clipboard_write(&mut self) -> bool {
        let Some(content) = self.state.request_clipboard_write.take() else {
            return false;
        };
        if self
            .event_tx
            .try_send(crate::events::AppEvent::ClipboardWrite { content })
            .is_err()
        {
            tracing::warn!("failed to queue clipboard write event");
        }
        true
    }

    pub(super) fn try_copy_retained_selection(
        &mut self,
        source_id: InputSourceId,
        key: TerminalKey,
    ) -> bool {
        if self.state.copy_on_select
            || !is_retained_selection_copy_key(&key)
            || !self
                .state
                .selection
                .as_ref()
                .is_some_and(crate::selection::Selection::is_finalized)
        {
            return false;
        }

        self.state.copy_selection(&self.terminal_runtimes);
        if !self.dispatch_pending_clipboard_write() {
            return false;
        }

        self.input_leases.insert_consumed(
            InputLeaseKey::new(source_id, &key),
            ConsumedInputLease::SuppressRepeats,
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, mouse};
    use super::*;
    use crate::{app::Mode, events::AppEvent, workspace::Workspace};

    fn app_with_screen_bytes_and_input(
        bytes: &[u8],
    ) -> (
        App,
        crate::layout::PaneInfo,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                bytes,
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        (app, info, input_rx)
    }

    fn drag_select_range(
        app: &mut App,
        info: &crate::layout::PaneInfo,
        start_col: u16,
        end_col: u16,
    ) {
        let row = info.inner_rect.y;
        let start_col = info.inner_rect.x + start_col;
        let end_col = info.inner_rect.x + end_col;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_col,
            row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));
    }

    fn clipboard_write_content(app: &mut App) -> Vec<u8> {
        match app.event_rx.try_recv().expect("clipboard write event") {
            AppEvent::ClipboardWrite { content } => content,
            event => panic!("unexpected event: {event:?}"),
        }
    }

    fn assert_visible_selection(app: &App) {
        assert!(app
            .state
            .selection
            .as_ref()
            .is_some_and(crate::selection::Selection::is_visible));
    }

    #[tokio::test]
    async fn copy_on_select_disabled_ctrl_c_copies_and_clears_retained_selection() {
        let (mut app, info, mut input_rx) = app_with_screen_bytes_and_input(b"alpha beta");
        app.state.copy_on_select = false;
        drag_select_range(&mut app, &info, 0, 4);
        assert_visible_selection(&app);
        assert!(app.event_rx.try_recv().is_err());

        let ctrl_c = TerminalKey::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            .with_windows_record(crate::input::WindowsKeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key_code: 0x43,
                virtual_scan_code: 0x2e,
                unicode: 'c' as u16,
                control_key_state: 0x0008,
            });
        let source_id = 41;
        app.route_client_events_from(
            source_id,
            vec![crate::raw_input::RawInputEvent::Key(ctrl_c.clone())],
            false,
        );

        let content = clipboard_write_content(&mut app);
        assert_eq!(content, b"alpha");
        assert!(app.state.selection.is_none());
        assert!(input_rx.try_recv().is_err());

        app.handle_internal_event(AppEvent::ClipboardWrite { content });
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("copied to clipboard")
        );

        app.route_client_events_from(
            source_id,
            vec![
                crate::raw_input::RawInputEvent::Key(ctrl_c.clone()),
                crate::raw_input::RawInputEvent::Key(
                    ctrl_c.clone().with_kind(KeyEventKind::Repeat),
                ),
            ],
            false,
        );
        assert_eq!(app.input_leases.len(), 1);
        assert!(app.event_rx.try_recv().is_err());
        assert!(input_rx.try_recv().is_err());

        app.route_client_events_from(
            source_id,
            vec![crate::raw_input::RawInputEvent::Key(
                ctrl_c.clone().with_kind(KeyEventKind::Release),
            )],
            false,
        );
        assert!(app.input_leases.is_empty());
        app.route_client_events_from(
            source_id,
            vec![crate::raw_input::RawInputEvent::Key(ctrl_c.clone())],
            false,
        );
        let expected = if cfg!(windows) {
            b"\x1b[67;46;99;1;8;1_".as_slice()
        } else {
            b"\x03".as_slice()
        };
        assert_eq!(
            input_rx.try_recv().expect("forwarded Ctrl-C").as_ref(),
            expected
        );
        assert!(app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn copy_on_select_disabled_cmd_c_copies_retained_selection() {
        let (mut app, info, mut input_rx) = app_with_screen_bytes_and_input(b"alpha beta");
        app.state.copy_on_select = false;
        drag_select_range(&mut app, &info, 0, 4);

        app.handle_terminal_key_headless(TerminalKey::new(KeyCode::Char('c'), KeyModifiers::SUPER));

        assert_eq!(clipboard_write_content(&mut app), b"alpha");
        assert!(app.state.selection.is_none());
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn retained_selection_copy_shortcut_is_disabled_with_copy_on_select() {
        let (mut app, _info, mut input_rx) = app_with_screen_bytes_and_input(b"alpha beta");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let mut selection = crate::selection::Selection::range(
            pane_id,
            0,
            0,
            4,
            app.state
                .pane_scroll_metrics(&app.terminal_runtimes, pane_id),
        );
        assert!(selection.finish());
        app.state.selection = Some(selection);
        app.state.copy_on_select = true;

        app.handle_terminal_key_headless(TerminalKey::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ));

        assert!(app.state.selection.is_none());
        assert!(app.event_rx.try_recv().is_err());
        assert_eq!(
            input_rx.try_recv().expect("forwarded Ctrl-C").as_ref(),
            b"\x03"
        );
    }

    #[tokio::test]
    async fn retained_selection_copy_shortcut_forwards_when_selection_text_is_empty() {
        let (mut app, info, mut input_rx) = app_with_screen_bytes_and_input(b"");
        app.state.copy_on_select = false;
        drag_select_range(&mut app, &info, 0, 4);
        assert_visible_selection(&app);

        app.handle_terminal_key_headless(TerminalKey::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ));

        assert!(app.state.selection.is_none());
        assert!(app.event_rx.try_recv().is_err());
        assert_eq!(
            input_rx.try_recv().expect("forwarded Ctrl-C").as_ref(),
            b"\x03"
        );
    }

    #[tokio::test]
    async fn retained_selection_copy_shortcut_requires_exact_modifiers() {
        let (mut app, info, mut input_rx) = app_with_screen_bytes_and_input(b"alpha beta");
        app.state.copy_on_select = false;
        drag_select_range(&mut app, &info, 0, 4);

        app.handle_terminal_key_headless(TerminalKey::new(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));

        assert!(app.state.selection.is_none());
        assert!(app.event_rx.try_recv().is_err());
        assert_eq!(
            input_rx
                .try_recv()
                .expect("forwarded Ctrl-Shift-C")
                .as_ref(),
            b"\x03"
        );
    }

    fn app_with_popup_screen_bytes_and_input(
        bytes: &[u8],
    ) -> (
        App,
        crate::layout::PaneId,
        Rect,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.terminal_area = Rect::new(0, 0, 100, 30);

        let pane_id = crate::layout::PaneId::alloc();
        let terminal_id = crate::terminal::TerminalId::alloc();

        app.state.popup_pane = Some(crate::app::state::PopupPaneState {
            pane_id,
            terminal_id: terminal_id.clone(),
            width: None,
            height: None,
        });

        let (_, inner) =
            crate::ui::popup_pane_rects(&app.state, app.state.view.terminal_area).unwrap();

        let (runtime, input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                inner.width,
                inner.height,
                0,
                bytes,
                4,
            );

        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        app.state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(
                terminal_id.clone(),
                std::path::PathBuf::from("/popup"),
            ),
        );

        (app, pane_id, inner, input_rx)
    }

    #[tokio::test]
    async fn popup_pane_drag_selection_highlights_and_copies_to_clipboard() {
        let (mut app, pane_id, inner, _rx) =
            app_with_popup_screen_bytes_and_input(b"popup content");
        app.state.copy_on_select = false;

        let start_col = inner.x;
        let end_col = inner.x + 5;
        let row = inner.y;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), start_col, row));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));

        assert_visible_selection(&app);
        assert_eq!(app.state.selection.as_ref().unwrap().pane_id, pane_id);

        app.state.copy_selection(&app.terminal_runtimes);
        assert!(app.dispatch_pending_clipboard_write());
        assert_eq!(clipboard_write_content(&mut app), b"popup");
    }

    #[tokio::test]
    async fn popup_pane_copy_on_select_copies_on_mouse_up() {
        let (mut app, _pane_id, inner, _rx) =
            app_with_popup_screen_bytes_and_input(b"popup content");
        app.state.copy_on_select = true;

        let start_col = inner.x;
        let end_col = inner.x + 5;
        let row = inner.y;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), start_col, row));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));

        assert_eq!(clipboard_write_content(&mut app), b"popup");
    }
}
