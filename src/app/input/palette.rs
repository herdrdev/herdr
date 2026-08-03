//! Command palette input: search, selection, execution, and inline rebinding.

use crossterm::event::{KeyCode, KeyModifiers};

use super::modal::leave_modal;
use super::navigate::ActionContext;
use crate::app::palette::{palette_line_of_row, PaletteAction};
use crate::app::state::{AppState, Mode, PendingShortcutConflict, ShortcutCapture};
use crate::app::App;
use crate::config::KeyCombo;
use crate::input::TerminalKey;

impl App {
    pub(crate) fn handle_keybind_help_key(&mut self, key: TerminalKey) {
        if self.state.keybind_help.capture.is_some() {
            self.handle_shortcut_capture_key(key);
            return;
        }

        self.state.keybind_help.notice = None;
        let modifiers = key.modifiers.difference(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Esc => {
                leave_modal(&mut self.state);
                return;
            }
            KeyCode::Up => {
                self.state.move_palette_selection(-1);
                return;
            }
            KeyCode::Down => {
                self.state.move_palette_selection(1);
                return;
            }
            KeyCode::Enter => {
                self.run_selected_palette_command();
                return;
            }
            KeyCode::Backspace => {
                self.state.keybind_help.query.pop();
                self.state.reset_palette_selection();
                return;
            }
            KeyCode::Char('p') if modifiers == KeyModifiers::CONTROL => {
                self.state.move_palette_selection(-1);
                return;
            }
            KeyCode::Char('n') if modifiers == KeyModifiers::CONTROL => {
                self.state.move_palette_selection(1);
                return;
            }
            KeyCode::Char('u') if modifiers == KeyModifiers::CONTROL => {
                self.state.keybind_help.query.clear();
                self.state.reset_palette_selection();
                return;
            }
            KeyCode::Char('s') if modifiers == KeyModifiers::CONTROL => {
                self.begin_shortcut_capture();
                return;
            }
            KeyCode::Char('x') if modifiers == KeyModifiers::CONTROL => {
                self.clear_selected_shortcut();
                return;
            }
            _ => {}
        }

        if let Some(character) = palette_text_char(key) {
            insert_keybind_help_query_text(&mut self.state, &character.to_string());
        }
    }

    fn run_selected_palette_command(&mut self) {
        let Some(command) = self.state.selected_palette_command() else {
            return;
        };
        match command.action {
            PaletteAction::RebindOnly => {
                self.state.keybind_help.notice = Some(format!(
                    "{} runs from its shortcut; ctrl+s rebinds it",
                    command.label
                ));
            }
            PaletteAction::PrefixMode => {
                leave_modal(&mut self.state);
                self.state.mode = Mode::Prefix;
            }
            PaletteAction::Navigate(action) => {
                leave_modal(&mut self.state);
                self.execute_prefix_key_action(action);
            }
            PaletteAction::Custom(idx) => {
                let Some(binding) = self.state.keybinds.custom_commands.get(idx).cloned() else {
                    return;
                };
                leave_modal(&mut self.state);
                self.launch_custom_command(binding, ActionContext::Prefix);
            }
        }
    }

    fn begin_shortcut_capture(&mut self) {
        let Some(command) = self.state.selected_palette_command() else {
            return;
        };
        let Some(config_key) = command.config_key else {
            self.state.keybind_help.notice =
                Some("custom commands are rebound in config.toml".to_string());
            return;
        };
        self.state.keybind_help.capture = Some(ShortcutCapture {
            config_key,
            command_label: command.label.to_string(),
            pending_conflict: None,
        });
    }

    fn clear_selected_shortcut(&mut self) {
        let Some(command) = self.state.selected_palette_command() else {
            return;
        };
        let Some(config_key) = command.config_key else {
            self.state.keybind_help.notice =
                Some("custom commands are rebound in config.toml".to_string());
            return;
        };
        if config_key == PREFIX_CONFIG_KEY {
            self.state.keybind_help.notice = Some("prefix mode always needs a key".to_string());
            return;
        }
        if !command.is_bound() {
            self.state.keybind_help.notice = Some(format!("{} has no shortcut", command.label));
            return;
        }

        if self.write_keybindings(&[(config_key, String::new())]) {
            self.state.keybind_help.notice = Some(format!("cleared {}", command.label));
        }
    }

    fn handle_shortcut_capture_key(&mut self, key: TerminalKey) {
        let Some(capture) = self.state.keybind_help.capture.clone() else {
            return;
        };

        if matches!(key.code, KeyCode::Modifier(_)) {
            return;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.state.keybind_help.capture = None;
            self.state.keybind_help.notice = Some("rebind cancelled".to_string());
            return;
        }

        let Some(combo) = capture_key_combo(key) else {
            return;
        };
        let binding = binding_string(combo, capture.config_key);

        if let Some(pending) = &capture.pending_conflict {
            if pending.binding == binding {
                let owner_key = pending.owner_config_key;
                let owner_label = pending.owner_label.clone();
                if self.write_keybindings(&[
                    (owner_key, String::new()),
                    (capture.config_key, binding.clone()),
                ]) {
                    self.state.keybind_help.notice = Some(format!(
                        "{} → {} (unbound {owner_label})",
                        capture.command_label, binding
                    ));
                }
                self.state.keybind_help.capture = None;
                return;
            }
        }

        if let Some(conflict) = self
            .state
            .palette_binding_conflict(&binding, capture.config_key)
        {
            let Some(owner_config_key) = conflict.config_key else {
                return;
            };
            // Stealing the prefix would leave it unset, so it is rebound from
            // its own row instead.
            if owner_config_key == PREFIX_CONFIG_KEY {
                self.state.keybind_help.notice =
                    Some(format!("{binding} is the prefix key; rebind prefix mode"));
                self.state.keybind_help.capture = None;
                return;
            }
            if let Some(capture) = self.state.keybind_help.capture.as_mut() {
                capture.pending_conflict = Some(PendingShortcutConflict {
                    binding,
                    owner_config_key,
                    owner_label: conflict.label.to_string(),
                });
            }
            return;
        }

        if self.write_keybindings(&[(capture.config_key, binding.clone())]) {
            self.state.keybind_help.notice = Some(format!("{} → {binding}", capture.command_label));
        }
        self.state.keybind_help.capture = None;
    }

    /// Writes `[keys]` fields to config.toml and reloads the live config so the
    /// palette immediately shows the new shortcut.
    fn write_keybindings(&mut self, entries: &[(&str, String)]) -> bool {
        let entries: Vec<(String, String)> = entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        let wrote = self.update_config_file("keybinding", |content| {
            let mut content = content.to_string();
            for (key, value) in &entries {
                content = crate::config::upsert_section_value(
                    &content,
                    "keys",
                    key,
                    &format!("\"{value}\""),
                );
            }
            content
        });
        if !wrote {
            self.state.keybind_help.notice =
                Some("could not write config.toml; shortcut unchanged".to_string());
            return false;
        }

        let report = self.apply_config_from_disk(false);
        self.state.clamp_palette_selection();
        if report.status == crate::config::ConfigReloadStatus::Failed {
            self.state.keybind_help.notice =
                Some("config.toml was written but could not be reloaded".to_string());
            return false;
        }
        true
    }
}

pub(crate) const PREFIX_CONFIG_KEY: &str = "prefix";

impl AppState {
    pub(crate) fn move_palette_selection(&mut self, delta: isize) {
        let count = self.palette_filtered_commands().len();
        if count == 0 {
            self.keybind_help.selected = 0;
            self.keybind_help.scroll = 0;
            return;
        }
        let current = self.keybind_help.selected.min(count - 1) as isize;
        self.keybind_help.selected = (current + delta).clamp(0, count as isize - 1) as usize;
        self.ensure_palette_selection_visible();
    }

    pub(crate) fn reset_palette_selection(&mut self) {
        self.keybind_help.selected = 0;
        self.keybind_help.scroll = 0;
    }

    pub(crate) fn clamp_palette_selection(&mut self) {
        let count = self.palette_filtered_commands().len();
        self.keybind_help.selected = self.keybind_help.selected.min(count.saturating_sub(1));
        self.ensure_palette_selection_visible();
    }

    pub(crate) fn ensure_palette_selection_visible(&mut self) {
        let commands = self.palette_filtered_commands();
        let lines = crate::app::palette::palette_display_lines(&commands);
        let Some(line_idx) = palette_line_of_row(&lines, self.keybind_help.selected) else {
            return;
        };
        let viewport = self.keybind_help_viewport_rows().max(1);
        // Keep the group heading above the first row visible when selecting it.
        let line_idx = if line_idx > 0
            && matches!(lines.get(line_idx - 1), Some(heading) if heading.is_heading())
        {
            line_idx - 1
        } else {
            line_idx
        };
        if line_idx < self.keybind_help.scroll {
            self.keybind_help.scroll = line_idx;
        } else if line_idx >= self.keybind_help.scroll + viewport {
            self.keybind_help.scroll = line_idx + 1 - viewport;
        }
        let max_scroll = lines.len().saturating_sub(viewport);
        self.keybind_help.scroll = self.keybind_help.scroll.min(max_scroll);
    }

    /// Row nearest the current scroll offset, used after mouse wheel scrolling
    /// so the selection follows the visible window.
    pub(crate) fn align_palette_selection_to_scroll(&mut self) {
        let commands = self.palette_filtered_commands();
        let lines = crate::app::palette::palette_display_lines(&commands);
        let viewport = self.keybind_help_viewport_rows().max(1);
        let start = self.keybind_help.scroll;
        let end = lines.len().min(start + viewport);
        let Some(current_line) = palette_line_of_row(&lines, self.keybind_help.selected) else {
            return;
        };
        if current_line >= start && current_line < end {
            return;
        }
        let target = if current_line < start {
            lines[start..end].iter().find_map(|line| line.row())
        } else {
            lines[start..end].iter().rev().find_map(|line| line.row())
        };
        if let Some(row) = target {
            self.keybind_help.selected = row;
        }
    }
}

/// Characters that go into the search box: no ctrl/alt chords, and the shifted
/// codepoint wins so `?` does not arrive as `shift+/`.
fn palette_text_char(key: TerminalKey) -> Option<char> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    if let Some(character) = key.shifted_codepoint.and_then(char::from_u32) {
        return Some(character);
    }
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    Some(character)
}

pub(crate) fn insert_keybind_help_query_text(state: &mut AppState, text: &str) {
    state
        .keybind_help
        .query
        .extend(text.chars().filter(|ch| !ch.is_control()));
    state.reset_palette_selection();
}

/// Canonical combo for a captured key press. Legacy terminals report shifted
/// letters as uppercase without the SHIFT modifier; normalize both forms so the
/// written binding round-trips through the config parser.
fn capture_key_combo(key: TerminalKey) -> Option<KeyCombo> {
    if matches!(key.code, KeyCode::Modifier(_) | KeyCode::Null) {
        return None;
    }
    let (mut code, mut modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
    if let KeyCode::Char(character) = code {
        if character.is_uppercase() {
            let lowered = character.to_lowercase().next().unwrap_or(character);
            code = KeyCode::Char(lowered);
            modifiers |= KeyModifiers::SHIFT;
        }
    }
    Some((code, modifiers))
}

/// Bare keys become `prefix+<key>` because an unmodified global binding would
/// swallow that key in every pane. Function keys and modified chords bind
/// directly. The prefix key itself is always a direct combo.
fn binding_string(combo: KeyCombo, config_key: &str) -> String {
    let label = crate::config::format_key_combo(combo);
    if config_key == PREFIX_CONFIG_KEY || is_direct_capture(combo) {
        label
    } else {
        format!("prefix+{label}")
    }
}

fn is_direct_capture((code, modifiers): KeyCombo) -> bool {
    if !modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return true;
    }
    matches!(code, KeyCode::F(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input::app_for_mouse_test;
    use crate::app::palette::palette_commands;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> TerminalKey {
        TerminalKey::new(code, modifiers)
    }

    fn palette_app() -> App {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        super::super::modal::open_keybind_help(&mut app.state);
        app
    }

    fn selected_label(app: &App) -> String {
        app.state
            .selected_palette_command()
            .map(|command| command.label.to_string())
            .unwrap_or_default()
    }

    fn config_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::config::test_config_env_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    fn temp_config(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-palette-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("config.toml")
    }

    #[test]
    fn typing_filters_and_resets_selection() {
        let mut app = palette_app();
        app.state.keybind_help.selected = 5;

        for character in "split".chars() {
            app.handle_keybind_help_key(key(KeyCode::Char(character), KeyModifiers::empty()));
        }

        assert_eq!(app.state.keybind_help.query, "split");
        assert_eq!(app.state.keybind_help.selected, 0);
        let commands = app.state.palette_filtered_commands();
        assert_eq!(commands.len(), 2);
        assert!(commands
            .iter()
            .all(|command| command.label.contains("split")));
    }

    #[test]
    fn arrows_move_the_selection_and_esc_closes() {
        let mut app = palette_app();
        let first = selected_label(&app);

        app.handle_keybind_help_key(key(KeyCode::Down, KeyModifiers::empty()));
        let second = selected_label(&app);
        assert_ne!(first, second);

        app.handle_keybind_help_key(key(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(selected_label(&app), first);

        // Up at the top stays put instead of wrapping or closing.
        app.handle_keybind_help_key(key(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(selected_label(&app), first);
        assert_eq!(app.state.mode, Mode::KeybindHelp);

        app.handle_keybind_help_key(key(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn esc_closes_even_with_a_query_typed() {
        let mut app = palette_app();
        insert_keybind_help_query_text(&mut app.state, "split");

        app.handle_keybind_help_key(key(KeyCode::Esc, KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn enter_runs_the_selected_command_after_closing_the_palette() {
        let mut app = palette_app();
        insert_keybind_help_query_text(&mut app.state, "toggle sidebar");
        assert_eq!(selected_label(&app), "toggle sidebar");
        let before = app.state.sidebar_collapsed;

        app.handle_keybind_help_key(key(KeyCode::Enter, KeyModifiers::empty()));

        assert_ne!(app.state.mode, Mode::KeybindHelp);
        assert_eq!(app.state.sidebar_collapsed, !before);
    }

    #[test]
    fn enter_on_an_indexed_family_explains_it_is_rebind_only() {
        let mut app = palette_app();
        insert_keybind_help_query_text(&mut app.state, "switch tab");

        app.handle_keybind_help_key(key(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::KeybindHelp);
        assert!(app
            .state
            .keybind_help
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("ctrl+s")));
    }

    #[test]
    fn ctrl_s_captures_a_shortcut_and_writes_it_to_config() {
        let _guard = config_guard();
        let path = temp_config("capture");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = palette_app();
        insert_keybind_help_query_text(&mut app.state, "open worktree");
        assert_eq!(selected_label(&app), "open worktree");

        app.handle_keybind_help_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.state.keybind_help.capture.is_some());

        app.handle_keybind_help_key(key(KeyCode::Char('y'), KeyModifiers::empty()));

        assert!(app.state.keybind_help.capture.is_none());
        let written = std::fs::read_to_string(&path).expect("config written");
        assert!(
            written.contains("open_worktree = \"prefix+y\""),
            "unexpected config: {written}"
        );
        assert_eq!(
            app.state
                .keybinds
                .open_worktree
                .label()
                .expect("binding applied"),
            "prefix+y"
        );

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().expect("config dir"));
    }

    #[test]
    fn capturing_a_taken_shortcut_warns_before_stealing_it() {
        let _guard = config_guard();
        let path = temp_config("conflict");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = palette_app();
        insert_keybind_help_query_text(&mut app.state, "open worktree");
        app.handle_keybind_help_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));

        // prefix+v is the default split vertical binding.
        app.handle_keybind_help_key(key(KeyCode::Char('v'), KeyModifiers::empty()));

        let pending = app
            .state
            .keybind_help
            .capture
            .as_ref()
            .and_then(|capture| capture.pending_conflict.clone())
            .expect("conflict recorded");
        assert_eq!(pending.binding, "prefix+v");
        assert_eq!(pending.owner_label, "split vertical");
        assert!(!path.exists(), "conflict must not write the config");

        app.handle_keybind_help_key(key(KeyCode::Char('v'), KeyModifiers::empty()));

        let written = std::fs::read_to_string(&path).expect("config written");
        assert!(
            written.contains("open_worktree = \"prefix+v\""),
            "unexpected config: {written}"
        );
        assert!(
            written.contains("split_vertical = \"\""),
            "previous owner should be unbound: {written}"
        );
        let commands = palette_commands(&app.state);
        let split = commands
            .iter()
            .find(|command| command.label == "split vertical")
            .expect("split vertical");
        assert!(!split.is_bound());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().expect("config dir"));
    }

    #[test]
    fn capturing_the_prefix_key_points_at_the_prefix_row_instead_of_stealing_it() {
        let _guard = config_guard();
        let path = temp_config("prefix-conflict");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = palette_app();
        app.state.prefix_code = KeyCode::Char('b');
        app.state.prefix_mods = KeyModifiers::CONTROL;
        insert_keybind_help_query_text(&mut app.state, "^open worktree$");
        app.handle_keybind_help_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));

        app.handle_keybind_help_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL));

        assert!(app.state.keybind_help.capture.is_none());
        assert!(app
            .state
            .keybind_help
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("prefix")));
        assert!(!path.exists(), "the prefix must not be unbound");

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().expect("config dir"));
    }

    #[test]
    fn esc_cancels_capture_without_closing_the_palette() {
        let mut app = palette_app();
        app.handle_keybind_help_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.state.keybind_help.capture.is_some());

        app.handle_keybind_help_key(key(KeyCode::Esc, KeyModifiers::empty()));

        assert!(app.state.keybind_help.capture.is_none());
        assert_eq!(app.state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn ctrl_x_clears_a_shortcut_but_never_the_prefix() {
        let _guard = config_guard();
        let path = temp_config("clear");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = palette_app();
        insert_keybind_help_query_text(&mut app.state, "^prefix mode$");
        app.handle_keybind_help_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(!path.exists());
        assert!(app
            .state
            .keybind_help
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("prefix mode")));

        app.state.keybind_help.query.clear();
        insert_keybind_help_query_text(&mut app.state, "^split vertical$");
        app.handle_keybind_help_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL));

        let written = std::fs::read_to_string(&path).expect("config written");
        assert!(
            written.contains("split_vertical = \"\""),
            "unexpected config: {written}"
        );

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().expect("config dir"));
    }

    #[test]
    fn bare_keys_capture_as_prefix_bindings() {
        let combo = capture_key_combo(key(KeyCode::Char('v'), KeyModifiers::empty())).unwrap();
        assert_eq!(binding_string(combo, "split_vertical"), "prefix+v");
    }

    #[test]
    fn shifted_letters_capture_consistently() {
        let legacy = capture_key_combo(key(KeyCode::Char('N'), KeyModifiers::empty())).unwrap();
        let modern = capture_key_combo(key(KeyCode::Char('n'), KeyModifiers::SHIFT)).unwrap();

        assert_eq!(legacy, modern);
        assert_eq!(binding_string(legacy, "new_workspace"), "prefix+shift+n");
    }

    #[test]
    fn modified_chords_capture_as_direct_bindings() {
        let combo = capture_key_combo(key(KeyCode::Char('g'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(binding_string(combo, "goto"), "ctrl+g");

        let function = capture_key_combo(key(KeyCode::F(5), KeyModifiers::empty())).unwrap();
        assert_eq!(binding_string(function, "zoom"), "f5");
    }

    #[test]
    fn arrow_keys_capture_as_prefix_bindings() {
        let combo = capture_key_combo(key(KeyCode::Up, KeyModifiers::empty())).unwrap();
        assert_eq!(binding_string(combo, "focus_pane_up"), "prefix+up");
    }

    #[test]
    fn prefix_key_captures_without_prefix_qualifier() {
        let combo = capture_key_combo(key(KeyCode::Esc, KeyModifiers::empty())).unwrap();
        assert_eq!(binding_string(combo, PREFIX_CONFIG_KEY), "esc");
    }

    #[test]
    fn captured_bindings_round_trip_through_the_config_parser() {
        for (code, modifiers, config_key) in [
            (KeyCode::Char('v'), KeyModifiers::empty(), "split_vertical"),
            (KeyCode::Char('n'), KeyModifiers::SHIFT, "new_workspace"),
            (KeyCode::Char('g'), KeyModifiers::CONTROL, "goto"),
            (KeyCode::F(5), KeyModifiers::empty(), "zoom"),
            (KeyCode::Up, KeyModifiers::empty(), "focus_pane_up"),
            (KeyCode::Esc, KeyModifiers::empty(), PREFIX_CONFIG_KEY),
        ] {
            let combo = capture_key_combo(key(code, modifiers)).unwrap();
            let binding = binding_string(combo, config_key);
            let parsed = crate::config::test_parse_binding_label(&binding)
                .unwrap_or_else(|| panic!("{binding} should parse"));
            assert_eq!(parsed, binding, "{binding} should round-trip");
        }
    }
}
