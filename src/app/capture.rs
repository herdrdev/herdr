//! Pure state and key handling for the local focus capture composer.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureAck {
    pub(crate) text: String,
    pub(crate) deadline: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureState {
    pub(crate) text: String,
    pub(crate) restored_age: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureAction {
    Consumed,
    Close,
    Save(bool),
}

impl CaptureState {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> CaptureAction {
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => CaptureAction::Close,
            KeyCode::Enter if key.modifiers == KeyModifiers::CONTROL => CaptureAction::Save(true),
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.text.push('\n');
                self.changed();
                CaptureAction::Consumed
            }
            KeyCode::Enter if key.modifiers.is_empty() => CaptureAction::Save(false),
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.text.pop();
                self.changed();
                CaptureAction::Consumed
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) && !c.is_control() =>
            {
                self.text.push(c);
                self.changed();
                CaptureAction::Consumed
            }
            _ => CaptureAction::Consumed,
        }
    }

    fn changed(&mut self) {
        self.error = None;
        self.restored_age = None;
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.text
            .extend(normalized.chars().filter(|c| !c.is_control() || *c == '\n'));
        self.changed();
    }
}

impl crate::app::state::AppState {
    pub(crate) fn open_capture(&mut self) {
        if self.capture.is_some() {
            return;
        }
        let draft = self
            .sidebar_presentation
            .load_capture_draft()
            .filter(|draft| !draft.text.trim().is_empty());
        let (text, restored_age) = draft.map_or((String::new(), None), |draft| {
            let age = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|now| now.as_secs().saturating_sub(draft.saved_at_unix))
                .unwrap_or_default();
            (draft.text, Some(crate::app::spawn_dock::format_age(age)))
        });
        self.capture = Some(CaptureState {
            text,
            restored_age,
            error: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }
    #[test]
    fn routes_edit_save_close_and_newline_keys() {
        let mut state = CaptureState {
            text: String::new(),
            restored_age: None,
            error: None,
        };
        assert_eq!(
            state.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            CaptureAction::Save(false)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Enter, KeyModifiers::CONTROL)),
            CaptureAction::Save(true)
        );
        for modifiers in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
            assert_eq!(
                state.handle_key(key(KeyCode::Enter, modifiers)),
                CaptureAction::Consumed
            );
        }
        state.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        state.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(state.text, "\n\n");
        state.insert_text("a\r\nb\rc\u{1b}");
        assert_eq!(state.text, "\n\na\nb\nc");
        assert_eq!(
            state.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)),
            CaptureAction::Close
        );
    }
}
