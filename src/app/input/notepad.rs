//! Keyboard and mouse routing for the sidebar notepad and the break-timer
//! overlay.
//!
//! Both surfaces intercept input before the rest of the app sees it, so the
//! rules for when they may do that live in one file:
//!
//! - The break prompt takes every key while it is up. That is the feature.
//!   `ctrl+alt+b` is its documented escape hatch and pauses the timer instead.
//! - The notepad only takes keys while a visible tab is focused, which a panel
//!   click or the `toggle_notepad` action can do.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::state::AppState;
use crate::ui::notepad::notepad_body_rect;
use crate::ui::notepad_agent::NotepadAgentAction;
use crate::ui::notepad_usage::NotepadUsageAction;

/// Work the notepad's input handlers cannot do themselves because it touches
/// the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotepadRequest {
    /// Load another note, flushing the current one first.
    Select(usize),
    /// Show the focused pane's work context instead of a note.
    SelectContext,
    Cycle {
        backwards: bool,
    },
    /// Write the buffer without waiting for the debounce.
    Save,
    /// Log and advance a confirmed break prompt.
    ConfirmPomodoro,
    /// Focus the pane behind an agent-tab subagent row. `None` is a native
    /// Claude subagent: its row focuses the pane the tab is showing.
    FocusAgentPane(Option<String>),
    /// Copy the full URL of an agent-tab link row.
    CopyAgentLink(String),
    /// Open the full URL of an agent-tab link row in the operator's browser.
    OpenAgentLink(String),
}

fn rect_contains(rect: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.right()
        && row >= rect.y
        && row < rect.bottom()
}

impl AppState {
    fn request_notepad(&mut self, request: NotepadRequest) {
        self.notepad_request = Some(request);
    }

    fn notes_area_visible(&self) -> bool {
        !self.sidebar_sections_layout || self.sidebar_areas.notes
    }

    pub(crate) fn set_notepad_focus(&mut self, focused: bool) {
        // A zero-row panel has nothing to focus; typing would edit a hidden note.
        if focused && (!self.notes_area_visible() || !self.notepad.has_visible_tabs()) {
            return;
        }
        if focused {
            let active_note = crate::notepad::NotepadTabTarget::Note(self.notepad.active);
            let active_note_visible = self.notepad.enabled
                && self.notepad.active_file().is_some()
                && self.notepad.is_tab_visible(active_note);
            if active_note_visible {
                // Read-only tabs have no editor focus. Return to the active
                // note only when an enabled, visible note can take over.
                self.notepad.context_active = false;
                self.notepad.agent_tab = false;
                self.notepad.usage_tab = false;
            } else if self.notepad.enabled {
                if let Some(index) = self
                    .notepad
                    .files
                    .iter()
                    .enumerate()
                    .find_map(|(index, _)| {
                        self.notepad
                            .is_tab_visible(crate::notepad::NotepadTabTarget::Note(index))
                            .then_some(index)
                    })
                {
                    // Keep the read-only tab active until the app has loaded
                    // the visible note through its normal selection path.
                    self.request_notepad(NotepadRequest::Select(index));
                }
            }
        }
        if self.notepad.focused == focused {
            return;
        }
        self.notepad.focused = focused;
        if !focused {
            self.request_notepad(NotepadRequest::Save);
        }
    }

    /// Toggles focus for an enabled note editor or a visible read-only tab.
    pub(crate) fn toggle_notepad_focus(&mut self) -> bool {
        let focused = !self.notepad.focused;
        let readonly_tab =
            self.notepad.context_active || self.notepad.agent_tab || self.notepad.usage_tab;
        if (!self.notepad.enabled && !readonly_tab)
            || (focused && (!self.notes_area_visible() || !self.notepad.has_visible_tabs()))
        {
            return false;
        }
        self.set_notepad_focus(focused);
        true
    }

    /// Consumes every key while a break prompt is up. Returns whether the key
    /// was handled, which is always true except for the escape hatch, which is
    /// handled here too but reported so the caller can stop as well.
    pub(crate) fn handle_pomodoro_prompt_key(
        &mut self,
        key: KeyEvent,
        now: std::time::Instant,
    ) -> bool {
        if self.pomodoro.prompt.is_none() {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('b') | KeyCode::Char('B') if ctrl && alt => {
                self.pomodoro.dismiss_and_pause(now);
            }
            KeyCode::Char('c') | KeyCode::Char('u') if ctrl => {
                if let Some(prompt) = self.pomodoro.prompt.as_mut() {
                    prompt.input.clear();
                    prompt.error = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.pomodoro.prompt.as_mut() {
                    prompt.input.pop();
                    prompt.error = None;
                }
            }
            KeyCode::Enter => self.request_notepad(NotepadRequest::ConfirmPomodoro),
            KeyCode::Char(ch) if !ctrl && !alt => {
                if let Some(prompt) = self.pomodoro.prompt.as_mut() {
                    // A one-line answer; the field is one row wide.
                    if prompt.input.chars().count() < 200 {
                        prompt.input.push(ch);
                    }
                    prompt.error = None;
                }
            }
            _ => {}
        }
        true
    }

    /// Routes a key into the focused panel tab. Unhandled modifier combinations
    /// fall through so global shortcuts keep working.
    pub(crate) fn handle_notepad_key(&mut self, key: KeyEvent, now: std::time::Instant) -> bool {
        let readonly_tab = self.notepad.agent_tab || self.notepad.usage_tab;
        if !self.notepad.focused
            || (!self.notepad.enabled && !readonly_tab && !self.notepad.context_active)
        {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if readonly_tab {
            match key.code {
                KeyCode::Esc => self.set_notepad_focus(false),
                KeyCode::Tab => self.request_notepad(NotepadRequest::Cycle { backwards: false }),
                KeyCode::BackTab => self.request_notepad(NotepadRequest::Cycle { backwards: true }),
                KeyCode::Up => {
                    if self.notepad.agent_tab {
                        self.notepad
                            .agent_scroll_by(-1, self.view.notepad_agent_max_scroll);
                    } else {
                        self.notepad
                            .usage_scroll_by(-1, self.view.notepad_usage_max_scroll);
                    }
                }
                KeyCode::Down => {
                    if self.notepad.agent_tab {
                        self.notepad
                            .agent_scroll_by(1, self.view.notepad_agent_max_scroll);
                    } else {
                        self.notepad
                            .usage_scroll_by(1, self.view.notepad_usage_max_scroll);
                    }
                }
                KeyCode::PageUp => {
                    let delta = -(i32::from(self.notepad.body_rows()) as isize);
                    if self.notepad.agent_tab {
                        self.notepad
                            .agent_scroll_by(delta, self.view.notepad_agent_max_scroll);
                    } else {
                        self.notepad
                            .usage_scroll_by(delta, self.view.notepad_usage_max_scroll);
                    }
                }
                KeyCode::PageDown => {
                    let delta = i32::from(self.notepad.body_rows()) as isize;
                    if self.notepad.agent_tab {
                        self.notepad
                            .agent_scroll_by(delta, self.view.notepad_agent_max_scroll);
                    } else {
                        self.notepad
                            .usage_scroll_by(delta, self.view.notepad_usage_max_scroll);
                    }
                }
                _ => return false,
            }
            return true;
        }
        // The Context tab is read-only: navigation keys scroll it, everything
        // else plain is swallowed so it cannot edit the note it hides, and
        // modifier combinations keep reaching the global shortcuts.
        if self.notepad.context_active {
            match key.code {
                KeyCode::Esc => self.set_notepad_focus(false),
                KeyCode::Tab => self.request_notepad(NotepadRequest::Cycle { backwards: false }),
                KeyCode::BackTab => self.request_notepad(NotepadRequest::Cycle { backwards: true }),
                KeyCode::Up => self.dock_scroll = self.dock_scroll.saturating_sub(1),
                KeyCode::Down => self.dock_scroll = self.dock_scroll.saturating_add(1),
                KeyCode::PageUp => {
                    self.dock_scroll = self.dock_scroll.saturating_sub(self.notepad.body_rows())
                }
                KeyCode::PageDown => {
                    self.dock_scroll = self.dock_scroll.saturating_add(self.notepad.body_rows())
                }
                KeyCode::Char(_) if !ctrl && !alt => {}
                _ => {
                    let _ = shift;
                    return false;
                }
            }
            return true;
        }
        match key.code {
            KeyCode::Esc => self.set_notepad_focus(false),
            KeyCode::Char('s') if ctrl => self.request_notepad(NotepadRequest::Save),
            KeyCode::Tab => self.request_notepad(NotepadRequest::Cycle { backwards: false }),
            KeyCode::BackTab => self.request_notepad(NotepadRequest::Cycle { backwards: true }),
            KeyCode::Enter => self.notepad.insert_newline(now),
            KeyCode::Backspace => self.notepad.backspace(now),
            KeyCode::Delete => self.notepad.delete(now),
            KeyCode::Left => self.notepad.move_left(),
            KeyCode::Right => self.notepad.move_right(),
            KeyCode::Up => self.notepad.move_up(),
            KeyCode::Down => self.notepad.move_down(),
            KeyCode::Home => self.notepad.move_line_start(),
            KeyCode::End => self.notepad.move_line_end(),
            KeyCode::PageUp => self
                .notepad
                .scroll_by(-(i32::from(self.notepad.body_rows()) as isize)),
            KeyCode::PageDown => self
                .notepad
                .scroll_by(i32::from(self.notepad.body_rows()) as isize),
            KeyCode::Char(ch) if !ctrl && !alt => self.notepad.insert_char(ch, now),
            _ => {
                let _ = shift;
                return false;
            }
        }
        true
    }

    /// Handles a mouse event over the notepad panel. Returns whether the event
    /// was consumed; a click outside the panel only drops focus and is left for
    /// its real target.
    pub(crate) fn handle_notepad_mouse(&mut self, mouse: &MouseEvent) -> bool {
        // A drag off the top edge leaves the panel's rows; the active
        // resize owns the gesture wherever the pointer goes.
        if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left))
            && matches!(
                self.drag.as_ref().map(|drag| &drag.target),
                Some(crate::app::state::DragTarget::NotepadDivider)
            )
        {
            self.set_manual_notepad_height(mouse.row);
            return true;
        }
        let panel = self.view.notepad_rect;
        let inside = rect_contains(panel, mouse.column, mouse.row);
        if !inside {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.set_notepad_focus(false);
            }
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta: isize = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -1
                } else {
                    1
                };
                if self.notepad.agent_tab {
                    self.notepad
                        .agent_scroll_by(delta, self.view.notepad_agent_max_scroll);
                } else if self.notepad.usage_tab {
                    self.notepad
                        .usage_scroll_by(delta, self.view.notepad_usage_max_scroll);
                } else if self.notepad.context_active {
                    self.dock_scroll = if delta < 0 {
                        self.dock_scroll.saturating_sub(1)
                    } else {
                        self.dock_scroll.saturating_add(1)
                    };
                } else {
                    self.notepad.scroll_by(delta);
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = self
                    .view
                    .notepad_tab_hit_areas
                    .iter()
                    .find(|(_, rect)| rect_contains(*rect, mouse.column, mouse.row))
                    .map(|(target, _)| *target)
                {
                    match target {
                        crate::notepad::NotepadTabTarget::Note(index) => {
                            self.set_notepad_focus(true);
                            if index != self.notepad.active
                                || self.notepad.context_active
                                || self.notepad.agent_tab
                                || self.notepad.usage_tab
                            {
                                self.request_notepad(NotepadRequest::Select(index));
                            }
                        }
                        crate::notepad::NotepadTabTarget::Context => {
                            self.set_notepad_focus(true);
                            if !self.notepad.context_active {
                                self.request_notepad(NotepadRequest::SelectContext);
                            }
                        }
                        crate::notepad::NotepadTabTarget::Agent => {
                            if self.notepad.select_agent_tab() {
                                self.request_notepad(NotepadRequest::Save);
                            }
                        }
                        crate::notepad::NotepadTabTarget::Usage => {
                            if self.notepad.select_usage_tab() {
                                self.request_notepad(NotepadRequest::Save);
                            }
                        }
                    }
                    return true;
                }
                if rect_contains(
                    self.view.notepad_usage_toggle_hit_area,
                    mouse.column,
                    mouse.row,
                ) {
                    self.notepad.toggle_usage_collapsed();
                    return true;
                }
                // The header row outside the tabs is the panel's drag handle:
                // pressing it starts a vertical resize rather than an edit.
                if mouse.row == panel.y {
                    self.drag = Some(crate::app::state::DragState {
                        target: crate::app::state::DragTarget::NotepadDivider,
                    });
                    return true;
                }
                if self.notepad.agent_tab {
                    self.handle_agent_tab_click(mouse);
                    return true;
                }
                if self.notepad.usage_tab {
                    self.handle_usage_tab_click(mouse);
                    return true;
                }
                self.set_notepad_focus(true);
                if !self.notepad.context_active {
                    let body = notepad_body_rect(panel);
                    if rect_contains(body, mouse.column, mouse.row) {
                        self.place_notepad_caret(mouse.column, mouse.row, body);
                    }
                }
                true
            }
            _ => true,
        }
    }

    /// A click in the agent tab's body resolves to the row's action: fold a
    /// section, focus a subagent's pane, or follow a link. A plain click opens
    /// the link; alt-click copies it instead, which is the only way to get a
    /// ticket's compact id rather than its URL.
    fn handle_agent_tab_click(&mut self, mouse: &MouseEvent) {
        let body = notepad_body_rect(self.view.notepad_rect);
        if !rect_contains(body, mouse.column, mouse.row) {
            return;
        }
        let index = usize::from(mouse.row.saturating_sub(body.y));
        match self
            .view
            .notepad_agent_rows
            .get(index)
            .map(|row| row.action.clone())
        {
            Some(NotepadAgentAction::ToggleSection(section)) => {
                self.notepad.toggle_agent_section(section);
            }
            Some(NotepadAgentAction::FocusSubagent(target)) => {
                self.request_notepad(NotepadRequest::FocusAgentPane(target));
            }
            Some(NotepadAgentAction::CopyLink(url)) => {
                if mouse
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::ALT)
                {
                    self.request_notepad(NotepadRequest::CopyAgentLink(url));
                } else {
                    self.request_notepad(NotepadRequest::OpenAgentLink(url));
                }
            }
            Some(NotepadAgentAction::CopyLinkIndex(_)) | Some(NotepadAgentAction::None) | None => {}
        }
    }

    fn handle_usage_tab_click(&mut self, mouse: &MouseEvent) {
        let body = notepad_body_rect(self.view.notepad_rect);
        if !rect_contains(body, mouse.column, mouse.row) {
            return;
        }
        let index = usize::from(mouse.row.saturating_sub(body.y));
        match self
            .view
            .notepad_usage_rows
            .get(index)
            .map(|row| row.action)
        {
            Some(NotepadUsageAction::OpenDashboard) => self.toggle_usage_view(),
            Some(NotepadUsageAction::ToggleProvider(provider)) => {
                self.notepad.toggle_usage_provider(provider)
            }
            _ => {}
        }
    }

    /// Drags the notepad's top edge to `row`: the panel is bottom-anchored, so
    /// the dragged row becomes its new top. The height persists per host.
    pub(crate) fn set_manual_notepad_height(&mut self, row: u16) {
        let panel = self.view.notepad_rect;
        if panel.height == 0 {
            return;
        }
        let height = panel
            .bottom()
            .saturating_sub(row)
            .clamp(crate::notepad::MIN_HEIGHT, crate::notepad::MAX_HEIGHT);
        if self.notepad.height != height {
            self.notepad.height = height;
            self.notepad_height_persistence_request = Some(height);
        }
    }

    fn place_notepad_caret(&mut self, column: u16, row: u16, body: ratatui::layout::Rect) {
        let line = self.notepad.scroll + usize::from(row.saturating_sub(body.y));
        let line = line.min(self.notepad.lines.len().saturating_sub(1));
        let target = usize::from(column.saturating_sub(body.x));
        let len = self
            .notepad
            .lines
            .get(line)
            .map(|text| text.chars().count())
            .unwrap_or(0);
        self.notepad.cursor_line = line;
        self.notepad.cursor_col = target.min(len);
    }

    /// Ends the current phase early without a reminder.
    pub(crate) fn skip_pomodoro_phase(&mut self, now: std::time::Instant) -> bool {
        if !self.pomodoro.enabled {
            return false;
        }
        self.pomodoro.skip(now);
        true
    }

    /// Toggles the break timer, or dismisses a prompt that is already up.
    pub(crate) fn toggle_pomodoro(&mut self, now: std::time::Instant) -> bool {
        if !self.pomodoro.enabled {
            return false;
        }
        if self.pomodoro.prompt.is_some() {
            self.pomodoro.dismiss_and_pause(now);
        } else {
            self.pomodoro.toggle_pause(now);
        }
        true
    }
}

impl crate::app::App {
    pub(crate) fn intercept_pomodoro_send_off_raw_input_with_visibility(
        &mut self,
        source_id: crate::app::InputSourceId,
        event: &crate::raw_input::RawInputEvent,
        visible: bool,
    ) -> bool {
        if let crate::raw_input::RawInputEvent::Mouse(mouse) = event {
            if let Some(button) = self
                .pending_pomodoro_send_off_mouse_releases
                .get(&source_id)
                .copied()
            {
                if let crossterm::event::MouseEventKind::Up(released) = mouse.kind {
                    if released == button {
                        self.pending_pomodoro_send_off_mouse_releases
                            .remove(&source_id);
                        return true;
                    }
                    return false;
                }
                return true;
            }
        }
        if !visible {
            return false;
        }

        let dismisses = match event {
            crate::raw_input::RawInputEvent::Key(key) => {
                if key.kind == crossterm::event::KeyEventKind::Release {
                    return false;
                }
                true
            }
            crate::raw_input::RawInputEvent::Text(_)
            | crate::raw_input::RawInputEvent::Paste(_) => true,
            crate::raw_input::RawInputEvent::Mouse(mouse) => {
                if let crossterm::event::MouseEventKind::Down(button) = mouse.kind {
                    self.pending_pomodoro_send_off_mouse_releases
                        .insert(source_id, button);
                    true
                } else {
                    !matches!(mouse.kind, crossterm::event::MouseEventKind::Up(_))
                }
            }
            _ => false,
        };
        if !dismisses {
            return false;
        }
        self.state
            .pomodoro
            .dismiss_send_off_at(std::time::Instant::now());
        true
    }

    pub(crate) fn intercept_notepad_key_with_prompt_visibility(
        &mut self,
        key: &crate::input::TerminalKey,
        prompt_visible: bool,
    ) -> bool {
        let now = std::time::Instant::now();
        if prompt_visible {
            if self.state.pomodoro.prompt.is_some() {
                self.state
                    .handle_pomodoro_prompt_key(key.as_key_event(), now);
                self.apply_notepad_request();
            }
            return true;
        }
        if self.state.notepad.focused && self.state.handle_notepad_key(key.as_key_event(), now) {
            self.apply_notepad_request();
            return true;
        }
        false
    }

    /// Runs the filesystem half of a notepad interaction.
    pub(crate) fn apply_notepad_request(&mut self) -> bool {
        let Some(request) = self.state.notepad_request.take() else {
            return false;
        };
        match request {
            NotepadRequest::Select(index) => self.select_notepad_file(index),
            NotepadRequest::SelectContext => self.select_notepad_context(),
            NotepadRequest::Cycle { backwards } => self.cycle_notepad_file(backwards),
            NotepadRequest::Save => self.write_notepad_now(),
            NotepadRequest::ConfirmPomodoro => self.confirm_pomodoro(std::time::Instant::now()),
            NotepadRequest::FocusAgentPane(target) => self.focus_agent_tab_pane(target),
            NotepadRequest::CopyAgentLink(url) => {
                if self
                    .event_tx
                    .try_send(crate::events::AppEvent::ClipboardWrite {
                        content: url.into_bytes(),
                    })
                    .is_err()
                {
                    self.show_work_link_notice("could not copy work link");
                } else {
                    self.show_work_link_notice("copied");
                }
                true
            }
            NotepadRequest::OpenAgentLink(url) => {
                if let Err(error) = crate::platform::open_url(&url) {
                    tracing::warn!(%error, %url, "failed to open agent link");
                    self.show_work_link_notice("could not open link");
                } else {
                    self.show_work_link_notice("opened");
                }
                true
            }
        }
    }

    /// Focuses the pane an agent-tab subagent row names. A native Claude
    /// subagent has no pane of its own, so its row re-focuses the pane the tab
    /// is showing.
    fn focus_agent_tab_pane(&mut self, target: Option<String>) -> bool {
        let resolved = match target {
            Some(id) => self.parse_current_public_pane_id(&id),
            None => self.state.active.and_then(|ws_idx| {
                self.state
                    .workspaces
                    .get(ws_idx)?
                    .focused_pane_id()
                    .map(|pane_id| (ws_idx, pane_id))
            }),
        };
        let Some((ws_idx, pane_id)) = resolved else {
            return false;
        };
        self.focus_pane_internal_via_api(ws_idx, pane_id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notepad::NotepadTabTarget;
    use ratatui::layout::Rect;
    use std::time::{Duration, Instant};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn state_with_notepad() -> AppState {
        let mut state = AppState::test_new();
        state.notepad.enabled = true;
        state.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/ideas.md".into(),
                name: "ideas".into(),
            },
        ]);
        state.notepad.set_visible_tabs(vec![
            "note:todo".to_string(),
            "note:ideas".to_string(),
            "context".to_string(),
            "agent".to_string(),
            "usage".to_string(),
        ]);
        state.notepad.focused = true;
        state
    }

    #[test]
    fn focusing_the_default_usage_only_panel_keeps_the_usage_tab() {
        let mut state = AppState::test_new();
        let config = crate::config::Config::default();
        state.notepad = crate::notepad::NotepadState::from_config(&config.notepad);

        assert_eq!(
            state.notepad.active_tab_target(),
            Some(NotepadTabTarget::Usage)
        );
        assert!(state.toggle_notepad_focus());

        assert!(state.notepad.usage_tab);
        assert_eq!(
            state.notepad.active_tab_target(),
            Some(NotepadTabTarget::Usage)
        );
    }

    #[test]
    fn hiding_every_tab_blocks_focus_so_typing_cannot_reach_a_hidden_note() {
        let mut state = AppState::test_new();
        state.notepad.enabled = true;
        state.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/hidden.md".into(),
            name: "hidden".into(),
        }]);
        state.notepad.set_visible_tabs(Vec::new());

        assert!(!state.notepad.has_visible_tabs());
        assert!(!state.toggle_notepad_focus());
        state.set_notepad_focus(true);
        assert!(!state.notepad.focused);
        assert!(!state.handle_notepad_key(key(KeyCode::Char('x')), Instant::now()));
    }

    #[test]
    fn focusing_from_usage_selects_a_visible_note_when_the_active_note_is_hidden() {
        let mut state = AppState::test_new();
        state.notepad.enabled = true;
        state.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/hidden.md".into(),
                name: "hidden".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/visible.md".into(),
                name: "visible".into(),
            },
        ]);
        state
            .notepad
            .set_visible_tabs(vec!["note:visible".into(), "usage".into()]);
        state.notepad.active = 0;
        state.notepad.usage_tab = true;

        assert_eq!(
            state.notepad.active_tab_target(),
            Some(NotepadTabTarget::Usage)
        );
        assert!(state.toggle_notepad_focus());

        assert_eq!(state.notepad_request, Some(NotepadRequest::Select(1)));
        assert_eq!(
            state.notepad.active, 0,
            "selection waits for the app to load it"
        );
        assert!(
            state.notepad.usage_tab,
            "keep the read-only tab active until load"
        );
    }

    #[test]
    fn typing_reaches_the_note_buffer_only_while_focused() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        assert!(state.handle_notepad_key(key(KeyCode::Char('h')), now));
        assert_eq!(state.notepad.lines, vec!["h".to_string()]);
        state.notepad.focused = false;
        assert!(!state.handle_notepad_key(key(KeyCode::Char('x')), now));
        assert_eq!(state.notepad.lines, vec!["h".to_string()]);
    }

    #[test]
    fn escape_leaves_the_notepad_and_asks_for_a_write() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        state.handle_notepad_key(key(KeyCode::Char('a')), now);
        assert!(state.handle_notepad_key(key(KeyCode::Esc), now));
        assert!(!state.notepad.focused);
        assert_eq!(state.notepad_request, Some(NotepadRequest::Save));
    }

    #[test]
    fn tab_asks_for_the_next_note() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        assert!(state.handle_notepad_key(key(KeyCode::Tab), now));
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::Cycle { backwards: false })
        );
    }

    #[test]
    fn an_unknown_control_combination_falls_through_to_global_shortcuts() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        let key = KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert!(!state.handle_notepad_key(key, now));
    }

    #[test]
    fn a_click_below_the_last_line_puts_the_caret_on_it() {
        let mut state = state_with_notepad();
        state.notepad.set_body("one\ntwo");
        state.view.notepad_rect = Rect::new(0, 10, 20, 5);
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 14,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&event));
        assert_eq!(
            (state.notepad.cursor_line, state.notepad.cursor_col),
            (1, 2)
        );
    }

    #[test]
    fn a_click_outside_the_panel_only_drops_focus() {
        let mut state = state_with_notepad();
        state.view.notepad_rect = Rect::new(0, 10, 20, 5);
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: 3,
            modifiers: KeyModifiers::empty(),
        };
        assert!(!state.handle_notepad_mouse(&event));
        assert!(!state.notepad.focused);
        assert_eq!(state.notepad_request, Some(NotepadRequest::Save));
    }

    #[test]
    fn clicking_the_context_tab_asks_for_it() {
        let mut state = state_with_notepad();
        state.view.notepad_rect = Rect::new(0, 10, 20, 5);
        state.view.notepad_tab_hit_areas = vec![(
            crate::notepad::NotepadTabTarget::Context,
            Rect::new(2, 10, 7, 1),
        )];
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 10,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&event));
        assert_eq!(state.notepad_request, Some(NotepadRequest::SelectContext));
    }

    #[test]
    fn the_context_tab_is_read_only_but_still_cycles_and_scrolls() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        state.notepad.context_active = true;
        state.notepad.set_body("keep me");
        // Plain keys never reach the note the tab hides.
        assert!(state.handle_notepad_key(key(KeyCode::Char('x')), now));
        assert_eq!(state.notepad.lines, vec!["keep me".to_string()]);
        // Scrolling drives the context view's scroll, not the note's.
        state.dock_scroll = 4;
        assert!(state.handle_notepad_key(key(KeyCode::Up), now));
        assert_eq!(state.dock_scroll, 3);
        // Global modifier shortcuts still fall through.
        assert!(!state.handle_notepad_key(
            KeyEvent::new(
                KeyCode::Char('m'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            now
        ));
        assert!(state.handle_notepad_key(key(KeyCode::Tab), now));
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::Cycle { backwards: false })
        );
    }

    #[test]
    fn dragging_the_top_edge_resizes_the_panel_and_queues_persistence() {
        let mut state = state_with_notepad();
        state.notepad.focused = false;
        state.notepad.height = 6;
        state.view.notepad_rect = Rect::new(0, 14, 20, 6);
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 14,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&down));
        assert!(matches!(
            state.drag.as_ref().map(|drag| &drag.target),
            Some(crate::app::state::DragTarget::NotepadDivider)
        ));
        // A grab on the handle does not hand the panel the keyboard.
        assert!(!state.notepad.focused);

        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 3,
            row: 11,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&drag));
        assert_eq!(state.notepad.height, 9, "the panel is bottom-anchored");
        assert_eq!(state.notepad_height_persistence_request, Some(9));

        // The handle clamps to the same range the config key documents.
        state.view.notepad_rect = Rect::new(0, 14, 20, 26);
        state.set_manual_notepad_height(30);
        assert_eq!(state.notepad.height, 10);
        // Dragging to the top of a 26-row panel keeps every row it covers: the
        // fixed ceiling no longer truncates the drag.
        state.set_manual_notepad_height(2);
        assert_eq!(state.notepad.height, 38);
        state.set_manual_notepad_height(45);
        assert_eq!(state.notepad.height, crate::notepad::MIN_HEIGHT);
    }

    #[test]
    fn the_break_prompt_swallows_ordinary_keys_and_confirms_on_enter() {
        let now = Instant::now();
        let mut state = AppState::test_new();
        state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );
        state.pomodoro.tick(now + Duration::from_secs(25 * 60));
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Char('t')), now));
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Char('e')), now));
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Char('a')), now));
        assert_eq!(
            state.pomodoro.prompt.as_ref().map(|p| p.input.as_str()),
            Some("tea")
        );
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Enter), now));
        assert_eq!(state.notepad_request, Some(NotepadRequest::ConfirmPomodoro));
    }

    #[test]
    fn the_break_prompt_can_always_be_escaped_with_the_timer_shortcut() {
        let now = Instant::now();
        let mut state = AppState::test_new();
        state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );
        state.pomodoro.tick(now + Duration::from_secs(25 * 60));
        let escape = KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert!(state.handle_pomodoro_prompt_key(escape, now));
        assert!(state.pomodoro.prompt.is_none());
        assert!(state.pomodoro.paused());
    }

    const AGENT_BASE_SECS: u64 = 1_760_000_000;

    fn state_with_agent_tab() -> (AppState, crate::layout::PaneId) {
        let mut state = AppState::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_new("alpha")];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.notepad.enabled = true;
        state.notepad.height = 18;
        state.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/todo.md".into(),
            name: "todo".into(),
        }]);
        state
            .notepad
            .set_visible_tabs(vec!["note:todo".to_string(), "agent".to_string()]);
        state.status_now_unix = Some(AGENT_BASE_SECS as i64 + 600);
        let pane_id = state.workspaces[0].focused_pane_id().unwrap();
        let terminal_id = state.workspaces[0].terminal_id(pane_id).unwrap().clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Codex),
                crate::detect::AgentState::Working,
            );
        state
            .agent_states
            .report(
                pane_id,
                crate::agent_state::AgentReportPayload {
                    goal: Some(Some("ship MAT-160".into())),
                    tasks: Some(vec![crate::agent_state::AgentTask {
                        text: "read transcript".into(),
                        status: crate::agent_state::AgentTaskStatus::Completed,
                    }]),
                    subagents: Some(vec![
                        crate::agent_state::AgentSubagent {
                            name: "native worker".into(),
                            status: crate::api::schema::AgentStatus::Working,
                            last_active_at: None,
                            pane_id: None,
                            source: crate::agent_state::AgentSubagentSource::Observed,
                        },
                        crate::agent_state::AgentSubagent {
                            name: "reported reviewer".into(),
                            status: crate::api::schema::AgentStatus::Blocked,
                            last_active_at: None,
                            pane_id: Some("w1:p2".into()),
                            source: crate::agent_state::AgentSubagentSource::Reported,
                        },
                    ]),
                    ..crate::agent_state::AgentReportPayload::default()
                },
                std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(AGENT_BASE_SECS),
            )
            .expect("valid report");
        state.agent_states.observe_links(
            pane_id,
            ["https://github.com/owner/repo/pull/1259".to_string()],
            crate::agent_state::AgentLinkSource::Output,
            std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(AGENT_BASE_SECS),
        );
        state.notepad.select_agent_tab();
        crate::ui::compute_view(&mut state, Rect::new(0, 0, 120, 40));
        (state, pane_id)
    }

    fn click_at(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn click_agent_row(state: &mut AppState, index: usize) {
        let body = notepad_body_rect(state.view.notepad_rect);
        assert!(state.handle_notepad_mouse(&click_at(body.x, body.y + index as u16)));
    }

    fn alt_click_agent_row(state: &mut AppState, index: usize) {
        let body = notepad_body_rect(state.view.notepad_rect);
        let mut click = click_at(body.x, body.y + index as u16);
        click.modifiers = KeyModifiers::ALT;
        assert!(state.handle_notepad_mouse(&click));
    }

    #[test]
    fn clicking_the_agent_header_selects_the_read_only_tab() {
        let mut state = state_with_notepad();
        state.view.notepad_rect = Rect::new(0, 10, 26, 8);
        state.view.notepad_tab_hit_areas =
            crate::ui::notepad::notepad_tab_hit_areas(&state, state.view.notepad_rect);
        let agent = state
            .view
            .notepad_tab_hit_areas
            .iter()
            .find(|(target, _)| *target == NotepadTabTarget::Agent)
            .map(|(_, rect)| *rect)
            .expect("agent tab hit area");

        assert!(state.handle_notepad_mouse(&click_at(agent.x, agent.y)));
        assert!(state.notepad.agent_tab);
        assert!(
            !state.notepad.focused,
            "a read-only tab holds no editor focus"
        );
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::Save),
            "the note it covered is flushed"
        );
    }

    #[test]
    fn clicking_a_usage_row_opens_the_existing_dashboard() {
        let mut state = state_with_notepad();
        state.provider_usage = crate::provider_usage::ProviderUsageSnapshot::with_primary_accounts(
            crate::provider_usage::AccountUsage::default(),
            crate::provider_usage::AccountUsage::default(),
            crate::provider_usage::AccountUsage::default(),
        );
        state.notepad.select_usage_tab();
        crate::ui::compute_view(&mut state, Rect::new(0, 0, 120, 40));
        let body = notepad_body_rect(state.view.notepad_rect);

        assert!(state.handle_notepad_mouse(&click_at(body.x, body.y)));
        assert!(state.usage_view.is_none(), "the provider header is inert");
        crate::ui::compute_view(&mut state, Rect::new(0, 0, 120, 40));
        assert!(state.handle_notepad_mouse(&click_at(body.x, body.y + 1)));
        assert!(state.usage_view.is_some());
        assert!(state.request_usage_scan);
    }

    #[test]
    fn clicking_the_notepad_usage_header_chevron_toggles_collapsed_rows() {
        let mut state = state_with_notepad();
        state.provider_usage = crate::provider_usage::ProviderUsageSnapshot::with_primary_accounts(
            crate::provider_usage::AccountUsage::default(),
            crate::provider_usage::AccountUsage::default(),
            crate::provider_usage::AccountUsage::default(),
        );
        state.notepad.select_usage_tab();
        crate::ui::compute_view(&mut state, Rect::new(0, 0, 120, 40));
        assert_eq!(state.view.notepad_usage_rows.len(), 3);

        let toggle = state.view.notepad_usage_toggle_hit_area;
        assert_eq!(toggle.width, 1);
        assert!(state.handle_notepad_mouse(&click_at(toggle.x, toggle.y)));
        assert!(!state.notepad.usage_collapsed);

        crate::ui::compute_view(&mut state, Rect::new(0, 0, 120, 40));
        assert_eq!(state.view.notepad_usage_rows.len(), 6);
    }

    #[test]
    fn clicking_a_note_tab_returns_from_the_agent_view() {
        let (mut state, _) = state_with_agent_tab();
        let note = state
            .view
            .notepad_tab_hit_areas
            .iter()
            .find(|(target, _)| matches!(target, NotepadTabTarget::Note(0)))
            .map(|(_, rect)| *rect)
            .expect("note tab hit area");

        assert!(state.handle_notepad_mouse(&click_at(note.x, note.y)));
        assert!(!state.notepad.agent_tab);
        assert!(state.notepad.focused);
        assert_eq!(
            state.notepad_request, None,
            "the same note is still loaded; nothing to reload"
        );
    }

    #[test]
    fn clicking_a_section_header_folds_it_for_the_session() {
        let (mut state, _) = state_with_agent_tab();
        let header = state
            .view
            .notepad_agent_rows
            .iter()
            .position(|row| {
                matches!(
                    row.action,
                    NotepadAgentAction::ToggleSection(crate::notepad::AgentSection::Tasks)
                )
            })
            .expect("tasks header row");

        click_agent_row(&mut state, header);
        assert!(state
            .notepad
            .agent_collapsed
            .collapsed(crate::notepad::AgentSection::Tasks));

        crate::ui::compute_view(&mut state, Rect::new(0, 0, 120, 40));
        let texts: Vec<String> = state
            .view
            .notepad_agent_rows
            .iter()
            .map(|row| {
                row.line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let text = texts.join("\n");
        assert!(text.contains("▸ Tasks"), "{text}");
        assert!(!text.contains("read transcript"), "{text}");

        click_agent_row(&mut state, header);
        assert!(!state
            .notepad
            .agent_collapsed
            .collapsed(crate::notepad::AgentSection::Tasks));
    }

    #[test]
    fn clicking_a_subagent_row_asks_for_its_pane() {
        let (mut state, _) = state_with_agent_tab();
        let rows: Vec<usize> = state
            .view
            .notepad_agent_rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                matches!(row.action, NotepadAgentAction::FocusSubagent(_)).then_some(index)
            })
            .collect();
        assert_eq!(rows.len(), 2);

        click_agent_row(&mut state, rows[0]);
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::FocusAgentPane(None)),
            "a native Claude subagent re-focuses the pane the tab shows"
        );
        click_agent_row(&mut state, rows[1]);
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::FocusAgentPane(Some("w1:p2".into())))
        );
    }

    #[test]
    fn clicking_a_link_row_asks_to_open_the_full_url() {
        let (mut state, _) = state_with_agent_tab();
        let link = state
            .view
            .notepad_agent_rows
            .iter()
            .position(|row| matches!(row.action, NotepadAgentAction::CopyLink(_)))
            .expect("link row");

        click_agent_row(&mut state, link);
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::OpenAgentLink(
                "https://github.com/owner/repo/pull/1259".into()
            ))
        );
    }

    #[test]
    fn alt_clicking_a_link_row_still_copies_it() {
        let (mut state, _) = state_with_agent_tab();
        let link = state
            .view
            .notepad_agent_rows
            .iter()
            .position(|row| matches!(row.action, NotepadAgentAction::CopyLink(_)))
            .expect("link row");

        alt_click_agent_row(&mut state, link);
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::CopyAgentLink(
                "https://github.com/owner/repo/pull/1259".into()
            ))
        );
    }

    #[test]
    fn scrolling_the_agent_tab_moves_its_own_offset_only() {
        let (mut state, _) = state_with_agent_tab();
        state.notepad.height = 6;
        crate::ui::compute_view(&mut state, Rect::new(0, 0, 120, 40));
        assert!(state.view.notepad_agent_max_scroll > 0);
        let panel = state.view.notepad_rect;

        let down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: panel.x,
            row: panel.y + 1,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&down));
        assert_eq!(state.notepad.agent_scroll, 1);
        assert_eq!(state.notepad.scroll, 0, "the note buffer is not scrolled");
    }

    fn app_with_two_panes() -> (
        crate::app::App,
        crate::layout::PaneId,
        crate::layout::PaneId,
    ) {
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("alpha")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let second = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.state.ensure_test_terminals();
        (app, first, second)
    }

    #[test]
    fn applying_a_subagent_focus_request_focuses_its_pane() {
        let (mut app, first, second) = app_with_two_panes();
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(second));
        let public = app.public_pane_id(0, first).expect("public pane id");

        app.state.notepad_request = Some(NotepadRequest::FocusAgentPane(Some(public)));
        assert!(app.apply_notepad_request());
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(first));

        // A native Claude subagent has no pane of its own: its row re-focuses
        // the pane the tab is showing.
        app.state.notepad_request = Some(NotepadRequest::FocusAgentPane(None));
        assert!(app.apply_notepad_request());
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(first));
    }

    #[test]
    fn applying_a_link_copy_request_writes_the_clipboard() {
        let (mut app, _, _) = app_with_two_panes();
        app.state.notepad_request = Some(NotepadRequest::CopyAgentLink(
            "https://github.com/owner/repo/pull/1259".into(),
        ));
        assert!(app.apply_notepad_request());
        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://github.com/owner/repo/pull/1259")
            }
            event => panic!("unexpected event: {event:?}"),
        }
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("copied")
        );
    }
}
