use std::{collections::HashMap, sync::OnceLock};

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use regex::Regex;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{
        state::{PluckMatch, PluckState},
        App, AppState, Mode,
    },
    input::TerminalKey,
    terminal::TerminalRuntimeRegistry,
};

const HINTS: &str = "asdfghjklqwertyuiopzxcvbnm";

fn token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(
        r#"((https?://|git@|git://|ssh://|ftp://|file:///)[^\s()\"']+)|(([.\w\-~$@]+)?(/[.\w\-@]+)+/?)|\b[0-9a-fA-F]{8}-[0-9a-fA-F-]{27,36}\b|\b[0-9a-fA-F]{7,40}\b|\b0x[0-9a-fA-F]+\b|\b\d{1,3}(\.\d{1,3}){3}\b|\b[0-9]{4,}\b"#,
    ).expect("built-in pluck pattern must compile"))
}

fn hints(count: usize) -> Vec<String> {
    if count <= HINTS.len() {
        HINTS.chars().map(|ch| ch.to_string()).collect()
    } else {
        HINTS
            .chars()
            .flat_map(|a| HINTS.chars().map(move |b| format!("{a}{b}")))
            .collect()
    }
}

impl App {
    pub(crate) fn handle_pluck_key(&mut self, key: TerminalKey) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let cancel = matches!(key.code, KeyCode::Esc)
            || matches!(key.code, KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL));
        if cancel {
            self.state.exit_pluck();
            return;
        }
        let KeyCode::Char(ch) = key.code else {
            return;
        };
        if !ch.is_ascii_alphabetic() || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return;
        }
        let Some(pluck) = self.state.pluck.as_mut() else {
            return;
        };
        pluck.input.push(ch.to_ascii_lowercase());
        if let Some(text) = pluck
            .matches
            .iter()
            .find(|item| item.hint == pluck.input)
            .map(|item| item.text.clone())
        {
            self.state.request_clipboard_write = Some(text.into_bytes());
            self.state.exit_pluck();
            self.dispatch_pending_clipboard_write();
        } else if !pluck
            .matches
            .iter()
            .any(|item| item.hint.starts_with(&pluck.input))
        {
            pluck.input.clear();
        }
    }
}

impl AppState {
    pub(crate) fn enter_pluck(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        self.enter_copy_mode(terminal_runtimes);
        let Some(pane_id) = self.copy_mode.as_ref().map(|copy| copy.pane_id) else {
            return;
        };
        let Some(height) = self
            .pane_info_by_id(pane_id)
            .map(|info| info.inner_rect.height)
        else {
            return;
        };
        let rows: Vec<String> = (0..height)
            .map(|row| {
                self.copy_mode_visible_row_text(terminal_runtimes, row)
                    .unwrap_or_default()
            })
            .collect();
        self.copy_mode = None;

        let mut unique = HashMap::<String, usize>::new();
        for row in &rows {
            for found in token_pattern().find_iter(row) {
                let next = unique.len();
                unique.entry(found.as_str().to_string()).or_insert(next);
            }
        }
        let assigned = hints(unique.len());
        let matches: Vec<PluckMatch> = rows
            .iter()
            .enumerate()
            .flat_map(|(row, text)| {
                let unique = &unique;
                let assigned = &assigned;
                token_pattern().find_iter(text).filter_map(move |found| {
                    let index = *unique.get(found.as_str())?;
                    let hint = assigned.get(index)?.clone();
                    Some(PluckMatch {
                        hint,
                        text: found.as_str().to_string(),
                        row: u16::try_from(row).ok()?,
                        col: u16::try_from(UnicodeWidthStr::width(&text[..found.start()])).ok()?,
                    })
                })
            })
            .collect();
        if matches.is_empty() {
            self.mode = Mode::Terminal;
            return;
        }
        self.pluck = Some(PluckState {
            pane_id,
            matches,
            input: String::new(),
        });
        self.mode = Mode::Copy;
    }

    fn exit_pluck(&mut self) {
        self.pluck = None;
        self.mode = Mode::Terminal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_tokens_share_the_same_hint() {
        let rows = ["https://example.com then https://example.com"];
        let mut unique = HashMap::new();
        for found in token_pattern().find_iter(rows[0]) {
            let next = unique.len();
            unique.entry(found.as_str()).or_insert(next);
        }
        assert_eq!(unique.len(), 1);
        assert_eq!(hints(unique.len())[unique["https://example.com"]], "a");
    }
}
