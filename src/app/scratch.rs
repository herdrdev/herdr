//! Scratch filesystem work belongs to client orchestration, never render.
use super::App;
use crate::scratch::{self, Request, SAVE_DELAY};
use std::time::{Duration, Instant};
/// A concurrent editor wins its file; preserve our work in an unsynced copy.
fn save_editor(editor: &mut scratch::Editor, dir: &std::path::Path) -> std::io::Result<bool> {
    match scratch::save_note(&mut editor.note) {
        Ok(()) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            let mut copy = editor.note.conflict_copy(dir);
            scratch::save_note(&mut copy)?;
            editor.note = copy;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}
impl App {
    pub(crate) fn handle_scratch_key(&mut self, key: crossterm::event::KeyEvent) {
        if self
            .state
            .is_prefix_key(&crate::input::TerminalKey::from(key))
        {
            self.state.set_server_mode(crate::app::state::Mode::Prefix);
        } else {
            self.state.scratch.key(key);
        }
    }

    pub(crate) fn tick_scratch(&mut self, now: Instant, force: bool) -> bool {
        let state = &mut self.state.scratch;
        let dir = state.dir.clone();
        let flush = force || !state.requests.is_empty();
        let mut changed = false;
        let mut failed = Vec::new();
        for mut editor in std::mem::take(&mut state.pending_saves) {
            if !flush
                && editor
                    .dirty_at
                    .is_some_and(|at| now.saturating_duration_since(at) < SAVE_DELAY)
            {
                failed.push(editor);
                continue;
            }
            match save_editor(&mut editor, &dir) {
                Ok(conflict) => {
                    if conflict {
                        state.error =
                            Some("External change · draft saved as a local conflict copy".into());
                    }
                    if let Some(note) = state.notes.iter_mut().find(|n| n.path == editor.note.path)
                    {
                        *note = editor.note.clone();
                    }
                    changed = true;
                }
                Err(e) => {
                    let message = e.to_string();
                    changed |= state.error.as_deref() != Some(&message);
                    state.error = Some(message);
                    editor.dirty_at = Some(now);
                    failed.push(editor);
                }
            }
        }
        state.pending_saves = failed;
        if let Some(editor) = &mut state.editor {
            if editor
                .dirty_at
                .is_some_and(|at| flush || now.saturating_duration_since(at) >= SAVE_DELAY)
            {
                match save_editor(editor, &dir) {
                    Ok(conflict) => {
                        editor.dirty_at = None;
                        state.error = if conflict {
                            Some("External change · draft saved as a local conflict copy".into())
                        } else if state.pending_saves.is_empty() {
                            None
                        } else {
                            Some("unsaved earlier note; draft retained".into())
                        };
                        changed = true;
                    }
                    Err(e) => {
                        editor.dirty_at = Some(now);
                        let message = e.to_string();
                        changed |= state.error.as_deref() != Some(&message);
                        state.error = Some(message);
                    }
                }
            }
        }
        for request in std::mem::take(&mut state.requests) {
            let path = match &request {
                Request::Toggle(path) | Request::Delete(path) => path,
            };
            if state.pending_saves.iter().any(|e| &e.note.path == path) {
                continue;
            }
            // Flush edits before acting on the list's cached copy.
            if let Some(editor) = &mut state.editor {
                if &editor.note.path == path && editor.dirty_at.is_some() {
                    match save_editor(editor, &dir) {
                        Ok(_) => {
                            editor.dirty_at = None;
                            changed = true;
                        }
                        Err(e) => {
                            let message = e.to_string();
                            changed |= state.error.as_deref() != Some(&message);
                            state.error = Some(message);
                            continue;
                        }
                    }
                }
            }
            let Some(mut note) = state
                .editor
                .as_ref()
                .filter(|e| &e.note.path == path)
                .map(|e| e.note.clone())
                .or_else(|| state.notes.iter().find(|n| &n.path == path).cloned())
            else {
                continue;
            };
            let result = match request {
                Request::Toggle(_) => scratch::toggle_sync(&mut note, &dir),
                Request::Delete(_) => scratch::delete_note(&note),
            };
            match result {
                Ok(()) => {
                    if state.editor.as_ref().is_some_and(|e| &e.note.path == path) {
                        match request {
                            Request::Toggle(_) => {
                                if let Some(e) = &mut state.editor {
                                    e.note = note;
                                }
                            }
                            Request::Delete(_) => state.editor = None,
                        }
                    }
                    state.error = if state.pending_saves.is_empty() {
                        None
                    } else {
                        Some("unsaved earlier note; draft retained".into())
                    };
                    changed = true;
                }
                Err(e) => {
                    let message = e.to_string();
                    changed |= state.error.as_deref() != Some(&message);
                    state.error = Some(message);
                }
            }
        }
        if force
            || changed
            || state
                .poll_at
                .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_secs(2))
        {
            state.poll_at = Some(now);
            match scratch::load_notes(&state.dir) {
                Ok(notes) => {
                    let previous = state.notes.get(state.selected).map(|n| n.path.clone());
                    let differs = notes.len() != state.notes.len()
                        || notes
                            .iter()
                            .zip(&state.notes)
                            .any(|(a, b)| a.path != b.path || a.original != b.original);
                    if differs {
                        state.notes = notes;
                        state.selected = previous
                            .and_then(|path| state.notes.iter().position(|n| n.path == path))
                            .unwrap_or(state.selected.min(state.notes.len().saturating_sub(1)));
                        changed = true;
                    }
                }
                Err(e) => {
                    let message = e.to_string();
                    changed |= state.error.as_deref() != Some(&message);
                    state.error = Some(message);
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(dir: &std::path::Path) -> App {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            rx,
            crate::api::EventHub::default(),
        );
        app.state = crate::app::AppState::test_new();
        app.state.scratch.dir = dir.into();
        app
    }
    #[test]
    fn freeze_scratch_autosave_never_creates_blank_drafts() {
        let dir = std::env::temp_dir().join(format!(
            "scratch-blank-{}",
            crate::config::test_unique_suffix()
        ));
        let mut app = app(&dir);
        app.state.scratch.open_writer();
        app.tick_scratch(Instant::now(), true);
        assert!(!dir.exists());
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert(" \t\n");
        app.tick_scratch(Instant::now() + SAVE_DELAY, false);
        assert!(!dir.exists());
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert(" ");
        app.state.scratch.new_note();
        app.tick_scratch(Instant::now(), true);
        assert!(app.state.scratch.pending_saves.is_empty());
        assert!(!dir.exists());
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert("first text");
        app.tick_scratch(Instant::now() + SAVE_DELAY, false);
        let notes = scratch::load_notes(&dir).expect("notes");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "first text");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn scratch_autosave_tick_and_switching_preserve_both_drafts() {
        let dir = std::env::temp_dir().join(format!(
            "scratch-tick-{}",
            crate::config::test_unique_suffix()
        ));
        let mut app = app(&dir);
        app.state.scratch.new_note();
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert("first draft");
        let now = Instant::now();
        assert!(!app.tick_scratch(now, false));
        assert!(app.tick_scratch(now + SAVE_DELAY, false));
        assert!(app
            .state
            .scratch
            .editor
            .as_ref()
            .expect("editor")
            .dirty_at
            .is_none());
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert(" more");
        app.state.scratch.new_note();
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert("second");
        app.tick_scratch(Instant::now() + SAVE_DELAY, true);
        let notes = scratch::load_notes(&dir).expect("notes");
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n.body == "first draft more"));
        assert!(notes.iter().any(|n| n.body == "second"));
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
    #[test]
    fn scratch_tick_reports_save_error_without_marking_saved() {
        let dir = std::env::temp_dir().join(format!(
            "scratch-error-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::write(&dir, "not a directory").expect("fixture");
        let mut app = app(&dir);
        app.state.scratch.new_note();
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert("draft");
        app.tick_scratch(Instant::now(), true);
        assert!(app.state.scratch.error.is_some());
        assert!(app
            .state
            .scratch
            .editor
            .as_ref()
            .expect("editor")
            .dirty_at
            .is_some());
        std::fs::remove_file(dir).expect("cleanup");
    }
    #[test]
    fn scratch_external_edit_creates_local_conflict_copy() {
        let dir = std::env::temp_dir().join(format!(
            "scratch-conflict-{}",
            crate::config::test_unique_suffix()
        ));
        let mut app = app(&dir);
        app.state.scratch.new_note();
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert("original");
        app.tick_scratch(Instant::now(), true);
        let original = app
            .state
            .scratch
            .editor
            .as_ref()
            .expect("editor")
            .note
            .path
            .clone();
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert(" ours");
        std::fs::write(&original, "external").expect("external");
        assert!(app.tick_scratch(Instant::now(), true));
        let editor = app.state.scratch.editor.as_ref().expect("editor");
        assert_ne!(editor.note.path, original);
        assert!(editor.dirty_at.is_none());
        assert!(!editor.note.synced);
        assert_eq!(std::fs::read_to_string(original).expect("read"), "external");
        assert_eq!(editor.note.body, "original ours");
        assert_eq!(scratch::load_notes(&dir).expect("notes").len(), 2);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
    #[test]
    fn scratch_sync_flushes_a_pending_draft_before_moving_it() {
        let dir = std::env::temp_dir().join(format!(
            "scratch-pending-{}",
            crate::config::test_unique_suffix()
        ));
        let mut app = app(&dir);
        app.state.scratch.new_note();
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert("first");
        app.tick_scratch(Instant::now(), true);
        let path = app
            .state
            .scratch
            .editor
            .as_ref()
            .expect("editor")
            .note
            .path
            .clone();
        app.state
            .scratch
            .editor
            .as_mut()
            .expect("editor")
            .insert(" draft");
        app.state.scratch.new_note();
        app.state
            .scratch
            .requests
            .push(Request::Toggle(path.clone()));
        app.tick_scratch(Instant::now(), false);
        assert!(!path.exists());
        let notes = scratch::load_notes(&dir).expect("notes");
        assert!(notes.iter().any(|n| n.synced && n.body == "first draft"));
        assert!(app.state.scratch.pending_saves.is_empty());
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
