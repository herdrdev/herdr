//! Client-local Scratch data and Markdown storage. No server or terminal ownership.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub(crate) const DEFAULT_DIR: &str = "~/workspaces/personal/vault/Scratch";
pub(crate) const SAVE_DELAY: Duration = Duration::from_millis(300);
const MAX_BYTES: u64 = 512 * 1024;

pub(crate) fn expand_dir(value: &str) -> PathBuf {
    crate::worktree::expand_tilde_path(value)
}
#[cfg(test)]
fn expand_with_home(value: &str, home: Option<&Path>) -> PathBuf {
    match (value.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if value == "~" => home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(value)),
        _ => PathBuf::from(value),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Note {
    pub path: PathBuf,
    pub title: String,
    pub modified: SystemTime,
    pub synced: bool,
    pub body: String,
    /// Retained verbatim apart from the sync field.
    pub frontmatter: Vec<String>,
    pub original: Option<String>,
}
impl Note {
    fn parse(path: PathBuf, raw: String, modified: SystemTime, local: bool) -> Self {
        let (frontmatter, body) = split_frontmatter(&raw);
        let synced = !local
            && frontmatter
                .iter()
                .any(|line| line.trim_end() == "sync: true");
        let title = body
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().trim_start_matches('#').trim().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Untitled".into());
        Self {
            path,
            title,
            modified,
            synced,
            body,
            frontmatter,
            original: Some(raw),
        }
    }
    pub fn conflict_copy(&self, dir: &Path) -> Self {
        let mut copy = self.clone();
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        copy.path = dir.join(".local").join(format!("{stamp}-conflict.md"));
        copy.synced = false;
        copy.original = None;
        copy.modified = SystemTime::now();
        copy
    }
    fn text(&self) -> String {
        let mut fields: Vec<_> = self
            .frontmatter
            .iter()
            .filter(|line| !line.starts_with("sync:"))
            .cloned()
            .collect();
        if self.synced {
            fields.push("sync: true".into());
        }
        if fields.is_empty() {
            self.body.clone()
        } else {
            format!("---\n{}\n---\n{}", fields.join("\n"), self.body)
        }
    }
}
fn split_frontmatter(raw: &str) -> (Vec<String>, String) {
    let normalized = raw.replace("\r\n", "\n");
    if let Some(rest) = normalized.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return (
                rest[..end].lines().map(str::to_string).collect(),
                rest[end + 5..].to_string(),
            );
        }
    }
    (Vec::new(), normalized)
}

pub(crate) fn load_notes(dir: &Path) -> std::io::Result<Vec<Note>> {
    let mut notes = Vec::new();
    for (folder, local) in [(dir.to_path_buf(), false), (dir.join(".local"), true)] {
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") || !entry.file_type()?.is_file() {
                continue;
            }
            let metadata = entry.metadata()?;
            if metadata.len() > MAX_BYTES {
                continue;
            }
            let raw = std::fs::read_to_string(&path)?;
            let note = Note::parse(path, raw, metadata.modified()?, local);
            if local || note.synced {
                notes.push(note);
            }
        }
    }
    notes.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(notes)
}
fn check_original(note: &Note) -> std::io::Result<()> {
    match (&note.original, std::fs::read_to_string(&note.path)) {
        (Some(original), Ok(current)) if original == &current => Ok(()),
        (None, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        (Some(_), Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "note removed on disk; draft retained",
        )),
        (_, Err(e)) => Err(e),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "note changed on disk; draft retained",
        )),
    }
}
/// Save via a sibling temporary file; an external edit is never silently lost.
pub(crate) fn save_note(note: &mut Note) -> std::io::Result<()> {
    use std::io::Write;
    check_original(note)?;
    let parent = note
        .path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing note directory"))?;
    std::fs::create_dir_all(parent)?;
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_path = parent.join(format!(".scratch-{}-{stamp}.tmp", std::process::id()));
    let raw = note.text();
    let result = (|| -> std::io::Result<()> {
        let mut temp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        temp.write_all(raw.as_bytes())?;
        temp.sync_all()?;
        drop(temp);
        check_original(note)?;
        if note.original.is_none() {
            std::fs::hard_link(&temp_path, &note.path)?;
        } else {
            crate::platform::replace_file(&temp_path, &note.path)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&temp_path);
    result?;
    note.original = Some(raw);
    note.modified = std::fs::metadata(&note.path)?.modified()?;
    note.title = note
        .body
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().trim_start_matches('#').trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Untitled".into());
    Ok(())
}
pub(crate) fn toggle_sync(note: &mut Note, dir: &Path) -> std::io::Result<()> {
    check_original(note)?;
    let old_path = note.path.clone();
    let mut moved = note.clone();
    moved.synced = !note.synced;
    let name = old_path
        .file_name()
        .ok_or_else(|| std::io::Error::other("missing note filename"))?;
    moved.path = if moved.synced {
        dir.join(name)
    } else {
        dir.join(".local").join(name)
    };
    moved.original = None;
    save_note(&mut moved)?;
    if let Err(e) = check_original(note).and_then(|()| std::fs::remove_file(&old_path)) {
        // This destination was created by this operation, never an existing file.
        let _ = std::fs::remove_file(&moved.path);
        return Err(e);
    }
    *note = moved;
    Ok(())
}
pub(crate) fn delete_note(note: &Note) -> std::io::Result<()> {
    check_original(note)?;
    std::fs::remove_file(&note.path)
}

#[derive(Debug, Clone)]
pub(crate) struct Editor {
    pub note: Note,
    /// UTF-8 byte offset, always a character boundary.
    pub cursor: usize,
    pub scroll: usize,
    pub dirty_at: Option<Instant>,
}
impl Editor {
    fn new(note: Note) -> Self {
        let cursor = note.body.len();
        Self {
            note,
            cursor,
            scroll: 0,
            dirty_at: None,
        }
    }
    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.note.body.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.dirty_at = Some(Instant::now());
    }
    pub fn words(&self) -> usize {
        self.note.body.split_whitespace().count()
    }
    fn previous(&self) -> usize {
        self.note.body[..self.cursor]
            .char_indices()
            .last()
            .map_or(0, |(i, _)| i)
    }
    fn next(&self) -> usize {
        self.note.body[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }
    pub fn key(&mut self, key: KeyEvent) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return;
        }
        match key.code {
            KeyCode::Char(c) => self.insert(&c.to_string()),
            KeyCode::Enter => self.insert("\n"),
            KeyCode::Tab => self.insert("    "),
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Home => {
                self.cursor = self.note.body[..self.cursor]
                    .rfind('\n')
                    .map_or(0, |i| i + 1)
            }
            KeyCode::End => {
                self.cursor += self.note.body[self.cursor..]
                    .find('\n')
                    .unwrap_or(self.note.body.len() - self.cursor)
            }
            KeyCode::Up | KeyCode::Down => {
                let start = self.note.body[..self.cursor]
                    .rfind('\n')
                    .map_or(0, |i| i + 1);
                let col = self.note.body[start..self.cursor].chars().count();
                let target = if key.code == KeyCode::Up {
                    if start == 0 {
                        return;
                    }
                    let prev = self.note.body[..start - 1].rfind('\n').map_or(0, |i| i + 1);
                    Some((prev, start - 1))
                } else {
                    self.note.body[self.cursor..].find('\n').map(|i| {
                        let next = self.cursor + i + 1;
                        let end = self.note.body[next..]
                            .find('\n')
                            .map_or(self.note.body.len(), |i| next + i);
                        (next, end)
                    })
                };
                if let Some((start, end)) = target {
                    self.cursor = self.note.body[start..end]
                        .char_indices()
                        .nth(col)
                        .map_or(end, |(i, _)| start + i);
                }
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let prev = self.previous();
                self.note.body.drain(prev..self.cursor);
                self.cursor = prev;
                self.dirty_at = Some(Instant::now());
            }
            KeyCode::Delete if self.cursor < self.note.body.len() => {
                let next = self.next();
                self.note.body.drain(self.cursor..next);
                self.dirty_at = Some(Instant::now());
            }
            _ => {}
        }
    }
    /// Soft wrapping shared by render and cursor placement (including wide glyphs).
    pub fn rows(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        use unicode_width::UnicodeWidthChar;
        let width = width.max(1);
        let mut rows = vec![String::new()];
        let mut x = 0;
        let mut cursor = (0, 0);
        for (i, c) in self.note.body.char_indices() {
            let w = c.width().unwrap_or(0);
            if c != '\n' && x + w > width {
                rows.push(String::new());
                x = 0;
            }
            if i == self.cursor {
                cursor = (rows.len() - 1, x);
            }
            if c == '\n' {
                rows.push(String::new());
                x = 0;
            } else {
                if let Some(row) = rows.last_mut() {
                    row.push(c);
                }
                x += w;
            }
        }
        if self.cursor == self.note.body.len() {
            if x >= width {
                rows.push(String::new());
                x = 0;
            }
            cursor = (rows.len() - 1, x);
        }
        (rows, cursor)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Request {
    Toggle(PathBuf),
    Delete(PathBuf),
}
/// Attach-local UI state. Never persisted or serialized in a server codec.
#[derive(Debug, Clone, Default)]
pub(crate) struct ScratchPresentation {
    pub open: bool,
    pub list: bool,
    pub selected: usize,
    pub editor: Option<Editor>,
    pub pending_saves: Vec<Editor>,
    pub expanded: bool,
    pub collapsed: bool,
    pub confirm_delete: bool,
    pub delete_target: Option<PathBuf>,
    pub requests: Vec<Request>,
    pub error: Option<String>,
}
#[derive(Debug, Clone)]
pub(crate) struct ScratchState {
    pub dir: PathBuf,
    pub notes: Vec<Note>,
    pub poll_at: Option<Instant>,
    pub presentation: ScratchPresentation,
}
impl Default for ScratchState {
    fn default() -> Self {
        Self {
            dir: expand_dir(DEFAULT_DIR),
            notes: Vec::new(),
            poll_at: None,
            presentation: Default::default(),
        }
    }
}
impl std::ops::Deref for ScratchState {
    type Target = ScratchPresentation;
    fn deref(&self) -> &Self::Target {
        &self.presentation
    }
}
impl std::ops::DerefMut for ScratchState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.presentation
    }
}
impl ScratchState {
    pub(crate) fn has_unsaved_drafts(&self) -> bool {
        !self.pending_saves.is_empty()
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.dirty_at.is_some())
    }

    pub fn open_writer(&mut self) {
        self.confirm_delete = false;
        self.delete_target = None;
        self.open = true;
        self.list = false;
        if self.editor.is_none() {
            if !self.notes.is_empty() {
                self.open_note(0);
            } else {
                self.new_note();
            }
        }
    }
    fn retain_draft(&mut self) {
        if let Some(editor) = self.editor.take() {
            if editor.dirty_at.is_some() {
                self.pending_saves.push(editor);
            }
        }
    }
    pub fn open_note(&mut self, index: usize) {
        self.confirm_delete = false;
        self.delete_target = None;
        if index >= self.notes.len() {
            return;
        }
        let note = self.notes[index].clone();
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.note.path == note.path)
        {
            if self
                .editor
                .as_ref()
                .is_some_and(|e| e.dirty_at.is_none() && e.note.original != note.original)
            {
                self.editor = Some(Editor::new(note));
            }
            self.selected = index;
            self.open = true;
            self.list = false;
            return;
        }
        let draft = self
            .pending_saves
            .iter()
            .position(|e| e.note.path == note.path)
            .map(|i| self.pending_saves.remove(i));
        self.retain_draft();
        self.editor = Some(draft.unwrap_or_else(|| Editor::new(note)));
        self.selected = index;
        self.open = true;
        self.list = false;
    }
    pub fn new_note(&mut self) {
        self.confirm_delete = false;
        self.delete_target = None;
        self.retain_draft();
        static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let modified = SystemTime::now();
        let stamp = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let note = Note {
            path: self.dir.join(".local").join(format!("{stamp}-{id}.md")),
            title: "Untitled".into(),
            modified,
            synced: false,
            body: String::new(),
            frontmatter: Vec::new(),
            original: None,
        };
        let mut editor = Editor::new(note);
        editor.dirty_at = Some(Instant::now());
        self.editor = Some(editor);
        self.open = true;
        self.list = false;
        self.error = None;
    }
    pub fn visible_count(&self) -> usize {
        if self.collapsed {
            0
        } else if self.expanded {
            self.notes.len()
        } else {
            self.notes.len().min(3)
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        if self.confirm_delete {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(path) = self.delete_target.take() {
                        self.requests.push(Request::Delete(path));
                    }
                    self.confirm_delete = false;
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.confirm_delete = false;
                    self.delete_target = None;
                }
                _ => {}
            }
            return;
        }
        if key.code == KeyCode::Esc {
            if self.list {
                self.open = false;
            } else {
                self.list = true;
            }
            return;
        }
        if !self.list {
            if let Some(editor) = &mut self.editor {
                editor.key(key);
            }
            return;
        }
        if !key.modifiers.is_empty() {
            return;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.notes.len().saturating_sub(1))
            }
            KeyCode::Enter => self.open_note(self.selected),
            KeyCode::Char('n') => self.new_note(),
            KeyCode::Char('s') => {
                if let Some(note) = self.notes.get(self.selected).cloned() {
                    self.requests.push(Request::Toggle(note.path.clone()));
                }
            }
            KeyCode::Char('d') => {
                self.delete_target = self.notes.get(self.selected).map(|n| n.path.clone());
                self.confirm_delete = self.delete_target.is_some();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> std::io::Result<Temp> {
        let path = std::env::temp_dir().join(format!(
            "scratch-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Temp(path))
    }
    fn app() -> crate::app::AppState {
        crate::app::AppState::test_new()
    }
    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    #[test]
    fn scratch_default_dir_and_tilde_expansion() {
        assert_eq!(
            expand_with_home(DEFAULT_DIR, Some(Path::new("/test"))),
            PathBuf::from("/test/workspaces/personal/vault/Scratch")
        );
        assert_eq!(
            expand_with_home("~", Some(Path::new("/test"))),
            PathBuf::from("/test")
        );
        let config: crate::config::Config =
            toml::from_str("[scratch]\ndir = '/custom'").expect("config");
        assert_eq!(config.scratch.dir, "/custom");
    }
    #[test]
    fn scratch_opens_writer_and_escape_notes_then_closes() {
        let mut app = app();
        app.scratch.open_writer();
        assert!(app.scratch.open && !app.scratch.list);
        assert!(matches!(
            app.input_owner(),
            crate::app::state::InputOwner::Surface(crate::app::state::SurfaceInputOwner::Scratch)
        ));
        app.scratch.key(key(KeyCode::Esc));
        assert!(app.scratch.list);
        app.scratch.key(key(KeyCode::Esc));
        assert!(!app.scratch.open);
    }
    #[test]
    fn scratch_editor_unicode_navigation_words_and_paste() {
        let mut app = app();
        app.scratch.new_note();
        let editor = app.scratch.editor.as_mut().expect("editor");
        editor.insert("Hi 世界\r\nnext");
        assert_eq!(editor.words(), 3);
        editor.key(key(KeyCode::Home));
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.note.body, "Hi 世界next");
        editor.key(key(KeyCode::Home));
        editor.key(key(KeyCode::Right));
        editor.key(key(KeyCode::Delete));
        assert_eq!(editor.note.body, "H 世界next");
        editor.cursor = editor.note.body.len();
        editor.key(key(KeyCode::Left));
        editor.key(key(KeyCode::Backspace));
        assert!(editor.note.body.is_char_boundary(editor.cursor));
        assert!(editor.rows(4).0.len() > 1);
    }
    #[test]
    fn scratch_local_autosave_and_reopen_markdown() {
        let temp = tempdir().expect("temp");
        let mut app = app();
        app.scratch.dir = temp.path().into();
        app.scratch.new_note();
        let editor = app.scratch.editor.as_mut().expect("editor");
        editor.insert("# Notes\nhello world");
        save_note(&mut editor.note).expect("save");
        assert!(editor.note.path.starts_with(temp.path().join(".local")));
        app.scratch.notes = load_notes(temp.path()).expect("load");
        app.scratch.list = true;
        app.scratch.key(key(KeyCode::Enter));
        assert_eq!(
            app.scratch.editor.as_ref().expect("open").note.body,
            "# Notes\nhello world"
        );
        app.scratch.key(key(KeyCode::Esc));
        app.scratch.key(key(KeyCode::Char('n')));
        assert_eq!(app.scratch.editor.as_ref().expect("new").words(), 0);
    }
    #[test]
    fn scratch_sync_preserves_frontmatter_and_moves_folder() {
        let temp = tempdir().expect("temp");
        std::fs::write(
            temp.path().join("zen.md"),
            "---\ntitle: Zen\nsync: true\ncustom: keep\n---\nbody",
        )
        .expect("write");
        let mut app = app();
        app.scratch.notes = load_notes(temp.path()).expect("load");
        app.scratch.list = true;
        app.scratch.key(key(KeyCode::Char('s')));
        assert_eq!(app.scratch.requests.len(), 1);
        let mut note = app.scratch.notes[0].clone();
        toggle_sync(&mut note, temp.path()).expect("unsync");
        assert!(!note.synced && note.path.starts_with(temp.path().join(".local")));
        assert!(!temp.path().join("zen.md").exists());
        toggle_sync(&mut note, temp.path()).expect("sync");
        let raw = std::fs::read_to_string(&note.path).expect("read");
        assert!(raw.contains("sync: true") && raw.contains("custom: keep"));
    }
    #[test]
    fn scratch_delete_requires_confirmation_and_cancel_works() {
        let temp = tempdir().expect("temp");
        let mut app = app();
        app.scratch.dir = temp.path().into();
        app.scratch.new_note();
        save_note(&mut app.scratch.editor.as_mut().expect("editor").note).expect("save");
        app.scratch.notes = load_notes(temp.path()).expect("load");
        app.scratch.list = true;
        app.scratch.key(key(KeyCode::Char('d')));
        assert!(app.scratch.confirm_delete && app.scratch.requests.is_empty());
        app.scratch.key(key(KeyCode::Esc));
        assert!(!app.scratch.confirm_delete);
        app.scratch.key(key(KeyCode::Char('d')));
        app.scratch.key(key(KeyCode::Char('y')));
        assert!(matches!(app.scratch.requests[0], Request::Delete(_)));
        delete_note(&app.scratch.notes[0]).expect("delete");
        assert!(load_notes(temp.path()).expect("load").is_empty());
    }
    #[test]
    fn scratch_sidebar_three_latest_and_expand() {
        let temp = tempdir().expect("temp");
        std::fs::create_dir(temp.path().join(".local")).expect("mkdir");
        for i in 0..5 {
            std::fs::write(
                temp.path().join(".local").join(format!("{i}.md")),
                format!("note {i}"),
            )
            .expect("write");
        }
        let mut app = app();
        app.scratch.notes = load_notes(temp.path()).expect("load");
        assert_eq!(app.scratch.visible_count(), 3);
        app.scratch.expanded = true;
        assert_eq!(app.scratch.visible_count(), 5);
        assert!(app
            .scratch
            .notes
            .windows(2)
            .all(|n| n[0].modified >= n[1].modified));
    }
    #[test]
    fn scratch_external_changes_and_collision_are_not_overwritten() {
        let temp = tempdir().expect("temp");
        let mut app = app();
        app.scratch.dir = temp.path().into();
        app.scratch.new_note();
        let note = &mut app.scratch.editor.as_mut().expect("editor").note;
        save_note(note).expect("save");
        std::fs::write(&note.path, "external").expect("write");
        assert!(save_note(note).is_err());
        assert!(delete_note(note).is_err());
        assert_eq!(
            std::fs::read_to_string(&note.path).expect("read"),
            "external"
        );
        note.original = Some("external".into());
        note.body = "external".into();
        let destination = temp.path().join(note.path.file_name().expect("name"));
        std::fs::write(&destination, "collision").expect("write");
        assert!(toggle_sync(note, temp.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(destination).expect("read"),
            "collision"
        );
    }
    #[test]
    fn scratch_confirmation_pins_path_and_new_note_cancels_it() {
        let temp = tempdir().expect("temp");
        let mut app = app();
        app.scratch.dir = temp.path().into();
        for text in ["first", "second"] {
            app.scratch.new_note();
            let e = app.scratch.editor.as_mut().expect("editor");
            e.insert(text);
            save_note(&mut e.note).expect("save");
            e.dirty_at = None;
        }
        app.scratch.notes = load_notes(temp.path()).expect("load");
        app.scratch.list = true;
        let path = app.scratch.notes[0].path.clone();
        app.scratch.key(key(KeyCode::Char('d')));
        app.scratch.notes.reverse();
        app.scratch.key(key(KeyCode::Enter));
        assert!(matches!(&app.scratch.requests[0],Request::Delete(target) if target==&path));
        app.scratch.requests.clear();
        app.scratch.list = true;
        app.scratch.key(key(KeyCode::Char('d')));
        app.scratch.new_note();
        assert!(!app.scratch.confirm_delete);
        app.scratch.key(key(KeyCode::Char('y')));
        assert!(app.scratch.requests.is_empty());
    }
    #[test]
    fn scratch_keeps_prefix_shortcuts_as_the_input_owner() {
        let mut app = app();
        app.scratch.open_writer();
        app.set_server_mode(crate::app::Mode::Prefix);
        assert!(matches!(
            app.input_owner(),
            crate::app::state::InputOwner::Server(crate::app::state::ServerInputOwner::Prefix)
        ));
    }
}
