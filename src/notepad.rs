//! Cross-device notepad: a folder of Markdown notes edited in place at the
//! bottom of the sidebar.
//!
//! The notes are plain files in one directory, so "synced across devices" is
//! whatever already syncs that directory — Syncthing, a cloud folder, or the
//! optional git remote driven from [`sync`]. Herdr's job is to reload when the
//! file changes underneath it and to write back promptly so the sync tool has
//! something current to carry.
//!
//! [`NotepadState`] is pure data: the buffer, the caret, and the dirty flag are
//! testable without a terminal or a filesystem. Every path that touches disk is
//! a free function in this module or lives in the `App` layer.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::events::AppEvent;
use notify::Watcher;

/// A note larger than this is not a hand-written note, and re-rendering it on
/// every keystroke would stall the frame.
const MAX_NOTE_BYTES: u64 = 512 * 1024;
/// More files than this in the notes directory means it is being used as a
/// document folder; the switcher stops being useful long before then.
const MAX_NOTES: usize = 32;
/// Keystrokes land in bursts. Writing once the burst stops keeps the file whole
/// for whatever is syncing it without writing on every character.
pub(crate) const SAVE_DEBOUNCE: Duration = Duration::from_millis(700);

/// One note file offered by the switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotepadFile {
    pub(crate) path: PathBuf,
    /// File stem, which is what the panel header shows.
    pub(crate) name: String,
}

/// One clickable tab in the panel's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotepadTabTarget {
    Note(usize),
    /// The focused pane's work context, rendered by the dock's Context surface.
    Context,
    /// The focused pane's agent state, read-only.
    Agent,
    /// Live provider quotas for every local coding account, read-only.
    Usage,
}

/// The Context tab's header label.
pub(crate) const NOTEPAD_CONTEXT_TAB_LABEL: &str = "context";

/// Smallest and largest panel heights, header row included. Dragging the
/// panel's top edge and `[notepad] height` both clamp to this range.
///
/// The ceiling is deliberately far above any real sidebar so it never binds on
/// its own: what actually limits the panel is the sidebar's spare rows, applied
/// by `ui::notepad::notepad_height`. Keeping a finite bound still stops a
/// corrupt config or presentation file from storing a nonsense height.
pub(crate) const MIN_HEIGHT: u16 = 3;
pub(crate) const MAX_HEIGHT: u16 = 200;

/// One collapsible section of the agent tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentSection {
    Status,
    Tasks,
    Subagents,
    Links,
}

/// Which agent-tab sections are folded. Default is nothing collapsed, and the
/// flags live only in this session's state: they are never written to disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AgentSectionCollapse {
    pub(crate) status: bool,
    pub(crate) tasks: bool,
    pub(crate) subagents: bool,
    pub(crate) links: bool,
}

/// Attach-local notepad controls. Note contents remain session-owned, while
/// each TUI client may select and fold the read-only projection independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotepadPresentationState {
    pub(crate) agent_tab: bool,
    pub(crate) usage_tab: bool,
    pub(crate) usage_collapsed: bool,
    pub(crate) usage_expanded_providers:
        std::collections::BTreeSet<crate::provider_usage::QuotaProvider>,
    pub(crate) agent_collapsed: AgentSectionCollapse,
    pub(crate) agent_scroll: usize,
    pub(crate) usage_scroll: usize,
}

impl Default for NotepadPresentationState {
    fn default() -> Self {
        Self {
            agent_tab: false,
            usage_tab: false,
            usage_collapsed: true,
            usage_expanded_providers: std::collections::BTreeSet::new(),
            agent_collapsed: AgentSectionCollapse::default(),
            agent_scroll: 0,
            usage_scroll: 0,
        }
    }
}

impl AgentSectionCollapse {
    pub(crate) fn collapsed(&self, section: AgentSection) -> bool {
        match section {
            AgentSection::Status => self.status,
            AgentSection::Tasks => self.tasks,
            AgentSection::Subagents => self.subagents,
            AgentSection::Links => self.links,
        }
    }

    pub(crate) fn toggle(&mut self, section: AgentSection) {
        match section {
            AgentSection::Status => self.status = !self.status,
            AgentSection::Tasks => self.tasks = !self.tasks,
            AgentSection::Subagents => self.subagents = !self.subagents,
            AgentSection::Links => self.links = !self.links,
        }
    }
}

/// The notepad's editable buffer and everything the panel draws from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotepadState {
    pub(crate) enabled: bool,
    /// Rows the panel occupies in the sidebar, header included.
    pub(crate) height: u16,
    pub(crate) dir: Option<PathBuf>,
    pub(crate) files: Vec<NotepadFile>,
    pub(crate) active: usize,
    /// Configured sidebar tabs. Note entries use `note:<stem>` keys.
    pub(crate) visible_tabs: Vec<String>,
    /// The Context tab is showing instead of a note. `active` still names the
    /// note the operator was on, so leaving the tab lands back on it.
    pub(crate) context_active: bool,
    /// The note body, one entry per line. Always at least one (empty) line.
    pub(crate) lines: Vec<String>,
    pub(crate) cursor_line: usize,
    /// Caret column counted in characters, not bytes or columns.
    pub(crate) cursor_col: usize,
    pub(crate) scroll: usize,
    pub(crate) focused: bool,
    pub(crate) dirty: bool,
    /// Set when the file changed on disk while the local buffer was dirty. The
    /// local edit wins on the next write; the flag is what tells the operator.
    pub(crate) diverged: bool,
    pub(crate) error: Option<String>,
    /// When the debounced write is due. Cleared once it runs.
    pub(crate) save_due: Option<Instant>,
    /// The read-only agent tab is showing instead of a note. It never holds
    /// the editor focus: there is no caret in a view the operator cannot type
    /// into.
    pub(crate) agent_tab: bool,
    /// The read-only per-account quota list is showing instead of a note.
    pub(crate) usage_tab: bool,
    /// The provider rows in the read-only Usage tab are folded to one row per provider.
    pub(crate) usage_collapsed: bool,
    pub(crate) usage_expanded_providers:
        std::collections::BTreeSet<crate::provider_usage::QuotaProvider>,
    /// Folded agent-tab sections, kept for the rest of the client session.
    pub(crate) agent_collapsed: AgentSectionCollapse,
    /// Scroll offset of the agent-tab body, clamped by view computation.
    pub(crate) agent_scroll: usize,
    /// Scroll offset of the Usage tab, clamped by view computation.
    pub(crate) usage_scroll: usize,
}

impl Default for NotepadState {
    fn default() -> Self {
        Self {
            enabled: false,
            height: 8,
            dir: None,
            files: Vec::new(),
            active: 0,
            visible_tabs: vec!["usage".to_string()],
            context_active: false,
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            scroll: 0,
            focused: false,
            dirty: false,
            diverged: false,
            error: None,
            save_due: None,
            agent_tab: false,
            usage_tab: true,
            usage_collapsed: true,
            usage_expanded_providers: std::collections::BTreeSet::new(),
            agent_collapsed: AgentSectionCollapse::default(),
            agent_scroll: 0,
            usage_scroll: 0,
        }
    }
}

impl NotepadState {
    pub(crate) fn swap_presentation(&mut self, other: &mut NotepadPresentationState) {
        std::mem::swap(&mut self.agent_tab, &mut other.agent_tab);
        std::mem::swap(&mut self.usage_tab, &mut other.usage_tab);
        std::mem::swap(&mut self.usage_collapsed, &mut other.usage_collapsed);
        std::mem::swap(
            &mut self.usage_expanded_providers,
            &mut other.usage_expanded_providers,
        );
        std::mem::swap(&mut self.agent_collapsed, &mut other.agent_collapsed);
        std::mem::swap(&mut self.agent_scroll, &mut other.agent_scroll);
        std::mem::swap(&mut self.usage_scroll, &mut other.usage_scroll);
        // A client's saved selection can predate a visibility change.
        self.reconcile_active_tab();
    }

    pub(crate) fn from_config(config: &crate::config::NotepadConfig) -> Self {
        let mut state = Self {
            enabled: config.enabled,
            height: config.height.clamp(MIN_HEIGHT, MAX_HEIGHT),
            dir: config.enabled.then(|| notes_dir(config)),
            visible_tabs: config.visible_tabs.clone(),
            ..Self::default()
        };
        state.reconcile_active_tab();
        state
    }

    pub(crate) fn tab_config_key(&self, target: NotepadTabTarget) -> Option<String> {
        match target {
            NotepadTabTarget::Note(index) => self
                .files
                .get(index)
                .map(|file| format!("note:{}", file.name)),
            NotepadTabTarget::Context => Some("context".to_string()),
            NotepadTabTarget::Agent => Some("agent".to_string()),
            NotepadTabTarget::Usage => Some("usage".to_string()),
        }
    }

    pub(crate) fn is_tab_visible(&self, target: NotepadTabTarget) -> bool {
        self.tab_config_key(target)
            .is_some_and(|key| self.visible_tabs.iter().any(|visible| visible == &key))
    }

    pub(crate) fn visible_tab_targets(&self) -> Vec<NotepadTabTarget> {
        let mut targets = self
            .files
            .iter()
            .enumerate()
            .filter_map(|(index, _)| {
                let target = NotepadTabTarget::Note(index);
                self.is_tab_visible(target).then_some(target)
            })
            .collect::<Vec<_>>();
        for target in [
            NotepadTabTarget::Context,
            NotepadTabTarget::Agent,
            NotepadTabTarget::Usage,
        ] {
            if self.is_tab_visible(target) {
                targets.push(target);
            }
        }
        targets
    }

    pub(crate) fn has_visible_tabs(&self) -> bool {
        self.files
            .iter()
            .enumerate()
            .any(|(index, _)| self.is_tab_visible(NotepadTabTarget::Note(index)))
            || [
                NotepadTabTarget::Context,
                NotepadTabTarget::Agent,
                NotepadTabTarget::Usage,
            ]
            .into_iter()
            .any(|target| self.is_tab_visible(target))
    }

    /// Replaces the configured visibility and moves selection away from a tab
    /// that became hidden.
    pub(crate) fn set_visible_tabs(&mut self, visible_tabs: Vec<String>) -> bool {
        self.visible_tabs = visible_tabs;
        self.reconcile_active_tab()
    }

    pub(crate) fn set_tab_visible(&mut self, target: NotepadTabTarget, visible: bool) -> bool {
        let Some(key) = self.tab_config_key(target) else {
            return false;
        };
        self.set_tab_key_visible(&key, visible)
    }

    pub(crate) fn set_note_tab_visible(&mut self, name: &str, visible: bool) -> bool {
        self.set_tab_key_visible(&format!("note:{name}"), visible)
    }

    fn set_tab_key_visible(&mut self, key: &str, visible: bool) -> bool {
        let was_visible = self.visible_tabs.iter().any(|item| item == key);
        if was_visible == visible {
            return false;
        }
        if visible {
            self.visible_tabs.push(key.to_string());
        } else {
            self.visible_tabs.retain(|item| item != key);
        }
        self.reconcile_active_tab();
        true
    }

    pub(crate) fn active_tab_target(&self) -> Option<NotepadTabTarget> {
        if self.context_active {
            Some(NotepadTabTarget::Context)
        } else if self.agent_tab {
            Some(NotepadTabTarget::Agent)
        } else if self.usage_tab {
            Some(NotepadTabTarget::Usage)
        } else {
            let target = self
                .active_file()
                .map(|_| NotepadTabTarget::Note(self.active));
            target.filter(|target| self.is_tab_visible(*target))
        }
    }

    fn reconcile_active_tab(&mut self) -> bool {
        let previous = self.active_tab_target();
        if previous.is_some_and(|target| self.is_tab_visible(target)) {
            return false;
        }
        let next = self.visible_tab_targets().into_iter().next();
        match next {
            Some(NotepadTabTarget::Note(index)) => {
                self.active = index;
                self.context_active = false;
                self.agent_tab = false;
                self.usage_tab = false;
                self.cursor_line = 0;
                self.cursor_col = 0;
                self.scroll = 0;
            }
            Some(NotepadTabTarget::Context) => {
                self.context_active = true;
                self.agent_tab = false;
                self.usage_tab = false;
            }
            Some(NotepadTabTarget::Agent) => {
                self.context_active = false;
                self.agent_tab = true;
                self.usage_tab = false;
                self.focused = false;
            }
            Some(NotepadTabTarget::Usage) => {
                self.context_active = false;
                self.agent_tab = false;
                self.usage_tab = true;
                self.focused = false;
            }
            None => {
                self.context_active = false;
                self.agent_tab = false;
                self.usage_tab = false;
                self.focused = false;
            }
        }
        previous != self.active_tab_target()
    }

    pub(crate) fn active_file(&self) -> Option<&NotepadFile> {
        self.files.get(self.active)
    }

    pub(crate) fn active_path(&self) -> Option<&Path> {
        self.active_file().map(|file| file.path.as_path())
    }

    /// Rows of note body the panel can draw, header row excluded.
    pub(crate) fn body_rows(&self) -> u16 {
        self.height.saturating_sub(1)
    }

    pub(crate) fn body(&self) -> String {
        let mut body = self.lines.join("\n");
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body
    }

    pub(crate) fn set_body(&mut self, body: &str) {
        self.lines = split_body(body);
        self.clamp_cursor();
        self.dirty = false;
        self.diverged = false;
    }

    /// Replaces the file list, keeping the operator on the same note across a
    /// rescan even when a new file sorted in ahead of it.
    pub(crate) fn set_files(&mut self, files: Vec<NotepadFile>) {
        let current = self.active_path().map(Path::to_path_buf);
        self.files = files;
        self.active = current
            .and_then(|path| self.files.iter().position(|file| file.path == path))
            .unwrap_or(0);
        self.reconcile_active_tab();
    }

    pub(crate) fn select(&mut self, index: usize) -> bool {
        if self.files.is_empty()
            || index >= self.files.len()
            || !self.is_tab_visible(NotepadTabTarget::Note(index))
        {
            return false;
        }
        if index == self.active && !self.context_active && !self.agent_tab && !self.usage_tab {
            return false;
        }
        self.active = index;
        self.context_active = false;
        self.agent_tab = false;
        self.usage_tab = false;
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.scroll = 0;
        true
    }

    /// Shows the focused pane's agent state instead of a note. The view is
    /// read-only, so the editor focus is released.
    pub(crate) fn select_agent_tab(&mut self) -> bool {
        if !self.is_tab_visible(NotepadTabTarget::Agent) || self.agent_tab {
            return false;
        }
        self.agent_tab = true;
        self.usage_tab = false;
        self.context_active = false;
        self.focused = false;
        self.agent_scroll = 0;
        true
    }

    pub(crate) fn select_usage_tab(&mut self) -> bool {
        if !self.is_tab_visible(NotepadTabTarget::Usage) || self.usage_tab {
            return false;
        }
        self.usage_tab = true;
        self.agent_tab = false;
        self.context_active = false;
        self.focused = false;
        self.usage_scroll = 0;
        true
    }

    pub(crate) fn toggle_usage_collapsed(&mut self) {
        self.usage_collapsed = !self.usage_collapsed;
        self.usage_scroll = 0;
    }

    pub(crate) fn toggle_usage_provider(&mut self, provider: crate::provider_usage::QuotaProvider) {
        if !self.usage_collapsed {
            self.usage_collapsed = true;
            self.usage_expanded_providers.clear();
            for candidate in [
                crate::provider_usage::QuotaProvider::Claude,
                crate::provider_usage::QuotaProvider::Codex,
                crate::provider_usage::QuotaProvider::Kimi,
                crate::provider_usage::QuotaProvider::Agy,
                crate::provider_usage::QuotaProvider::OpenCode,
            ] {
                if candidate != provider {
                    self.usage_expanded_providers.insert(candidate);
                }
            }
        } else if !self.usage_expanded_providers.remove(&provider) {
            self.usage_expanded_providers.insert(provider);
        }
        self.usage_scroll = 0;
    }

    pub(crate) fn toggle_agent_section(&mut self, section: AgentSection) {
        self.agent_collapsed.toggle(section);
    }

    /// Scrolls the agent-tab body, clamped against the rows the view
    /// computation last derived for it.
    pub(crate) fn agent_scroll_by(&mut self, delta: isize, max: usize) {
        let scroll = self.agent_scroll as isize + delta;
        self.agent_scroll = (scroll.max(0) as usize).min(max);
    }

    pub(crate) fn usage_scroll_by(&mut self, delta: isize, max: usize) {
        let scroll = self.usage_scroll as isize + delta;
        self.usage_scroll = (scroll.max(0) as usize).min(max);
    }

    /// Shows the Context tab. The note buffer is untouched, so returning to a
    /// note needs no reload.
    pub(crate) fn select_context(&mut self) -> bool {
        if !self.is_tab_visible(NotepadTabTarget::Context) || self.context_active {
            return false;
        }
        self.context_active = true;
        self.agent_tab = false;
        self.usage_tab = false;
        true
    }

    pub(crate) fn cycle(&mut self, backwards: bool) -> bool {
        let targets = self.visible_tab_targets();
        if targets.len() < 2 {
            return false;
        }
        let current = self
            .active_tab_target()
            .and_then(|active| targets.iter().position(|target| *target == active))
            .unwrap_or(0);
        let next = if backwards {
            (current + targets.len() - 1) % targets.len()
        } else {
            (current + 1) % targets.len()
        };
        match targets[next] {
            NotepadTabTarget::Note(index) => self.select(index),
            NotepadTabTarget::Context => self.select_context(),
            NotepadTabTarget::Agent => self.select_agent_tab(),
            NotepadTabTarget::Usage => self.select_usage_tab(),
        }
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(|l| l.chars().count()).unwrap_or(0)
    }

    fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_line = self.cursor_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.line_len(self.cursor_line));
    }

    fn byte_offset(&self, line: usize, col: usize) -> usize {
        self.lines
            .get(line)
            .map(|text| {
                text.char_indices()
                    .nth(col)
                    .map(|(index, _)| index)
                    .unwrap_or(text.len())
            })
            .unwrap_or(0)
    }

    fn touch(&mut self, now: Instant) {
        self.dirty = true;
        self.save_due = Some(now + SAVE_DEBOUNCE);
    }

    pub(crate) fn insert_char(&mut self, ch: char, now: Instant) {
        self.clamp_cursor();
        let offset = self.byte_offset(self.cursor_line, self.cursor_col);
        if let Some(line) = self.lines.get_mut(self.cursor_line) {
            line.insert(offset, ch);
            self.cursor_col += 1;
            self.touch(now);
        }
    }

    pub(crate) fn insert_newline(&mut self, now: Instant) {
        self.clamp_cursor();
        let offset = self.byte_offset(self.cursor_line, self.cursor_col);
        let Some(line) = self.lines.get_mut(self.cursor_line) else {
            return;
        };
        let rest = line.split_off(offset);
        self.lines.insert(self.cursor_line + 1, rest);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.touch(now);
    }

    pub(crate) fn backspace(&mut self, now: Instant) {
        self.clamp_cursor();
        if self.cursor_col > 0 {
            let offset = self.byte_offset(self.cursor_line, self.cursor_col - 1);
            if let Some(line) = self.lines.get_mut(self.cursor_line) {
                line.remove(offset);
                self.cursor_col -= 1;
                self.touch(now);
            }
            return;
        }
        if self.cursor_line == 0 {
            return;
        }
        let removed = self.lines.remove(self.cursor_line);
        self.cursor_line -= 1;
        self.cursor_col = self.line_len(self.cursor_line);
        if let Some(line) = self.lines.get_mut(self.cursor_line) {
            line.push_str(&removed);
        }
        self.touch(now);
    }

    pub(crate) fn delete(&mut self, now: Instant) {
        self.clamp_cursor();
        if self.cursor_col < self.line_len(self.cursor_line) {
            let offset = self.byte_offset(self.cursor_line, self.cursor_col);
            if let Some(line) = self.lines.get_mut(self.cursor_line) {
                line.remove(offset);
                self.touch(now);
            }
            return;
        }
        if self.cursor_line + 1 >= self.lines.len() {
            return;
        }
        let next = self.lines.remove(self.cursor_line + 1);
        if let Some(line) = self.lines.get_mut(self.cursor_line) {
            line.push_str(&next);
        }
        self.touch(now);
    }

    pub(crate) fn move_left(&mut self) {
        self.clamp_cursor();
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.line_len(self.cursor_line);
        }
    }

    pub(crate) fn move_right(&mut self) {
        self.clamp_cursor();
        if self.cursor_col < self.line_len(self.cursor_line) {
            self.cursor_col += 1;
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    pub(crate) fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(self.line_len(self.cursor_line));
        }
    }

    pub(crate) fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(self.line_len(self.cursor_line));
        }
    }

    pub(crate) fn move_line_start(&mut self) {
        self.cursor_col = 0;
    }

    pub(crate) fn move_line_end(&mut self) {
        self.clamp_cursor();
        self.cursor_col = self.line_len(self.cursor_line);
    }

    pub(crate) fn insert_text(&mut self, text: &str, now: Instant) {
        for ch in text.chars() {
            match ch {
                '\n' => self.insert_newline(now),
                '\r' => {}
                ch if ch.is_control() => {}
                ch => self.insert_char(ch, now),
            }
        }
    }

    /// Keeps the caret inside the drawn window. Called from view computation,
    /// which is the only place that knows how many rows the panel really got.
    pub(crate) fn sync_scroll(&mut self, visible_rows: u16) {
        let visible = usize::from(visible_rows).max(1);
        if self.cursor_line < self.scroll {
            self.scroll = self.cursor_line;
        } else if self.cursor_line >= self.scroll + visible {
            self.scroll = self.cursor_line + 1 - visible;
        }
        let max_scroll = self.lines.len().saturating_sub(visible);
        self.scroll = self.scroll.min(max_scroll);
    }

    pub(crate) fn scroll_by(&mut self, delta: isize) {
        let scroll = self.scroll as isize + delta;
        self.scroll = scroll.max(0) as usize;
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
    }

    /// A due debounced write, taken exactly once.
    pub(crate) fn take_due_save(&mut self, now: Instant) -> bool {
        if self.save_due.is_some_and(|due| now >= due) {
            self.save_due = None;
            return self.dirty;
        }
        false
    }
}

fn split_body(body: &str) -> Vec<String> {
    let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
    // A trailing newline is a terminator, not an extra empty line.
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The configured notes directory, defaulting to `<config dir>/notes`.
pub(crate) fn notes_dir(config: &crate::config::NotepadConfig) -> PathBuf {
    let configured = config.dir.trim();
    if configured.is_empty() {
        crate::config::config_dir().join("notes")
    } else {
        crate::worktree::expand_tilde_path(configured)
    }
}

/// Lists the Markdown notes in `dir`, ordered by the configured `files` list
/// first and alphabetically after it, so the operator's own ordering survives a
/// rescan that picked up a new file.
pub(crate) fn discover_notes(dir: &Path, preferred: &[String]) -> Vec<NotepadFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<NotepadFile> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().to_string();
            Some(NotepadFile { path, name })
        })
        .collect();
    found.sort_by_key(|file| file.name.to_lowercase());
    let rank = |file: &NotepadFile| -> usize {
        preferred
            .iter()
            .position(|wanted| {
                let wanted = wanted.trim().trim_end_matches(".md");
                wanted.eq_ignore_ascii_case(&file.name)
            })
            .unwrap_or(preferred.len())
    };
    found.sort_by_key(rank);
    found.truncate(MAX_NOTES);
    found
}

/// Creates the notes directory and, when it holds no note yet, the first one, so
/// the panel opens on something the operator can type into.
pub(crate) fn ensure_notes_dir(dir: &Path, preferred: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if !discover_notes(dir, preferred).is_empty() {
        return Ok(());
    }
    let first = preferred
        .first()
        .map(|name| name.trim().trim_end_matches(".md").to_string())
        .filter(|name| !name.is_empty() && !name.contains(['/', '\\']))
        .unwrap_or_else(|| "notes".to_string());
    let path = dir.join(format!("{first}.md"));
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, "")
}

pub(crate) fn load_note(path: &Path) -> Result<String, String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_NOTE_BYTES => {
            return Err(format!("note is larger than {}KB", MAX_NOTE_BYTES / 1024));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.to_string()),
    }
    std::fs::read_to_string(path).map_err(|error| error.to_string())
}

/// Writes through a sibling temporary file so a sync tool watching the directory
/// never picks up a half-written note.
pub(crate) fn save_note(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = path.with_extension("md.herdr-tmp");
    std::fs::write(&temp, body).map_err(|error| error.to_string())?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        error.to_string()
    })
}

/// Appends one line to a note without disturbing the editor buffer. Used by the
/// pomodoro break log.
pub(crate) fn append_line(path: &Path, line: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| error.to_string())
}

/// Watches the notes directory. The directory, not the file, because sync tools
/// and editors replace notes rather than rewriting them in place, and because a
/// new note appearing has to reach the switcher.
pub(crate) fn watch_notes(
    dir: &Path,
    event_tx: tokio::sync::mpsc::Sender<AppEvent>,
) -> Option<notify::RecommendedWatcher> {
    if std::fs::create_dir_all(dir).is_err() && !dir.is_dir() {
        return None;
    }
    let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let relevant = matches!(
            event.kind,
            notify::EventKind::Any
                | notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_)
        );
        // Our own atomic write lands as a `.herdr-tmp` create first; reacting to
        // it would reload the note the operator is still typing into.
        let touches_note = event.paths.iter().any(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        });
        if relevant && touches_note {
            let _ = event_tx.try_send(AppEvent::NotepadChanged);
        }
    });
    let mut watcher = watcher.ok()?;
    if let Err(error) = watcher.watch(dir, notify::RecursiveMode::NonRecursive) {
        tracing::warn!(
            path = %dir.display(),
            error = %error,
            "failed to register notepad watcher"
        );
        return None;
    }
    Some(watcher)
}

/// Optional git-backed sync for the notes directory.
///
/// This is the fallback for operators without a file-syncing daemon: the notes
/// directory is an ordinary git checkout, and Herdr pulls it on an interval and
/// pushes after a write. Every command is bounded and best-effort — a notepad
/// that cannot reach its remote still edits local files.
pub(crate) mod sync {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    fn git(dir: &Path, args: &[&str]) -> bool {
        let Ok(status) = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        else {
            return false;
        };
        status.success()
    }

    pub(crate) fn is_git_checkout(dir: &Path) -> bool {
        dir.join(".git").exists()
    }

    /// Fetches and fast-forwards. `--autostash` keeps an unsynced local edit
    /// rather than refusing to move.
    pub(crate) fn pull(dir: PathBuf) {
        std::thread::spawn(move || {
            if !is_git_checkout(&dir) {
                return;
            }
            if !git(&dir, &["pull", "--rebase", "--autostash", "--quiet"]) {
                tracing::debug!(path = %dir.display(), "notepad git pull did not succeed");
            }
        });
    }

    /// Commits everything in the notes directory and pushes it. Nothing to
    /// commit is the normal case and not an error.
    pub(crate) fn push(dir: PathBuf, message: String) {
        std::thread::spawn(move || {
            if !is_git_checkout(&dir) {
                return;
            }
            if !git(&dir, &["add", "-A"]) {
                return;
            }
            // A no-op commit exits non-zero; treat that as "already in sync".
            if !git(&dir, &["diff", "--cached", "--quiet"]) {
                let _ = git(&dir, &["commit", "-m", &message, "--quiet"]);
            }
            let _ = git(&dir, &["pull", "--rebase", "--autostash", "--quiet"]);
            if !git(&dir, &["push", "--quiet"]) {
                tracing::debug!(path = %dir.display(), "notepad git push did not succeed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(body: &str) -> NotepadState {
        let mut state = NotepadState {
            enabled: true,
            ..NotepadState::default()
        };
        state.set_body(body);
        state
    }

    #[test]
    fn restoring_a_client_selection_drops_a_tab_hidden_since() {
        let mut state = state_with("");
        state.set_visible_tabs(vec!["agent".into()]);
        let mut saved = NotepadPresentationState {
            usage_tab: true,
            ..NotepadPresentationState::default()
        };

        state.swap_presentation(&mut saved);

        assert!(!state.usage_tab);
        assert_eq!(state.active_tab_target(), Some(NotepadTabTarget::Agent));
    }

    #[test]
    fn freeze_usage_defaults_collapsed_for_new_clients_and_preserves_saved_expansion() {
        let mut app = crate::app::AppState::test_new();
        let mut new_client = NotepadPresentationState::default();
        app.notepad.swap_presentation(&mut new_client);
        assert!(app.notepad.usage_collapsed);
        let mut saved_client = NotepadPresentationState {
            usage_collapsed: false,
            ..Default::default()
        };
        app.notepad.swap_presentation(&mut saved_client);
        assert!(!app.notepad.usage_collapsed);
        app.notepad.swap_presentation(&mut saved_client);
        assert!(app.notepad.usage_collapsed);
        app.notepad.swap_presentation(&mut saved_client);
        assert!(!app.notepad.usage_collapsed);
    }

    #[test]
    fn usage_collapse_state_round_trips_with_client_presentation() {
        let mut state = state_with("");
        let mut saved = NotepadPresentationState {
            usage_collapsed: false,
            ..Default::default()
        };
        state.usage_collapsed = true;

        state.swap_presentation(&mut saved);
        assert!(!state.usage_collapsed);
        assert!(saved.usage_collapsed);

        state.swap_presentation(&mut saved);
        assert!(state.usage_collapsed);
    }

    #[test]
    fn typing_marks_the_buffer_dirty_and_schedules_one_write() {
        let now = Instant::now();
        let mut state = state_with("");
        state.insert_char('a', now);
        state.insert_char('b', now);
        assert_eq!(state.lines, vec!["ab".to_string()]);
        assert!(state.dirty);
        assert!(!state.take_due_save(now));
        assert!(state.take_due_save(now + SAVE_DEBOUNCE));
        assert!(!state.take_due_save(now + SAVE_DEBOUNCE));
    }

    #[test]
    fn enter_splits_the_line_at_the_caret() {
        let now = Instant::now();
        let mut state = state_with("todo");
        state.cursor_col = 2;
        state.insert_newline(now);
        assert_eq!(state.lines, vec!["to".to_string(), "do".to_string()]);
        assert_eq!((state.cursor_line, state.cursor_col), (1, 0));
    }

    #[test]
    fn backspace_at_column_zero_joins_the_previous_line() {
        let now = Instant::now();
        let mut state = state_with("one\ntwo");
        state.cursor_line = 1;
        state.cursor_col = 0;
        state.backspace(now);
        assert_eq!(state.lines, vec!["onetwo".to_string()]);
        assert_eq!((state.cursor_line, state.cursor_col), (0, 3));
    }

    #[test]
    fn editing_multibyte_text_keeps_the_caret_on_characters() {
        let now = Instant::now();
        let mut state = state_with("äöü");
        state.move_line_end();
        assert_eq!(state.cursor_col, 3);
        state.backspace(now);
        assert_eq!(state.lines, vec!["äö".to_string()]);
        state.insert_char('é', now);
        assert_eq!(state.lines, vec!["äöé".to_string()]);
    }

    #[test]
    fn body_roundtrips_through_split_and_join() {
        let state = state_with("a\nb\n");
        assert_eq!(state.lines, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(state.body(), "a\nb\n");
        assert_eq!(state_with("").body(), "");
    }

    #[test]
    fn scroll_follows_the_caret_within_the_drawn_rows() {
        let mut state = state_with("1\n2\n3\n4\n5\n6");
        state.cursor_line = 5;
        state.sync_scroll(3);
        assert_eq!(state.scroll, 3);
        state.cursor_line = 0;
        state.sync_scroll(3);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn reloading_files_keeps_the_operator_on_the_same_note() {
        let mut state = state_with("");
        state.set_files(vec![
            NotepadFile {
                path: PathBuf::from("/notes/ideas.md"),
                name: "ideas".into(),
            },
            NotepadFile {
                path: PathBuf::from("/notes/todo.md"),
                name: "todo".into(),
            },
        ]);
        state.set_visible_tabs(vec!["note:ideas".into(), "note:todo".into()]);
        assert!(state.select(1));
        state.set_files(vec![
            NotepadFile {
                path: PathBuf::from("/notes/agenda.md"),
                name: "agenda".into(),
            },
            NotepadFile {
                path: PathBuf::from("/notes/ideas.md"),
                name: "ideas".into(),
            },
            NotepadFile {
                path: PathBuf::from("/notes/todo.md"),
                name: "todo".into(),
            },
        ]);
        assert_eq!(
            state.active_file().map(|file| file.name.as_str()),
            Some("todo")
        );
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        let mut state = state_with("");
        state.set_files(vec![
            NotepadFile {
                path: PathBuf::from("/notes/a.md"),
                name: "a".into(),
            },
            NotepadFile {
                path: PathBuf::from("/notes/b.md"),
                name: "b".into(),
            },
        ]);
        state.set_visible_tabs(vec![
            "note:a".into(),
            "note:b".into(),
            "context".into(),
            "agent".into(),
            "usage".into(),
        ]);
        assert!(state.select(0));
        // The read-only tabs are the last stops.
        assert!(state.cycle(false));
        assert_eq!(state.active, 1);
        assert!(state.cycle(false));
        assert!(state.context_active);
        assert!(state.cycle(false));
        assert!(state.agent_tab);
        assert!(!state.context_active);
        assert!(state.cycle(false));
        assert!(state.usage_tab);
        assert!(!state.agent_tab);
        assert!(state.cycle(false));
        assert_eq!(state.active, 0);
        assert!(!state.context_active);
        assert!(!state.agent_tab);
        assert!(!state.usage_tab);
        assert!(state.cycle(true));
        assert!(state.usage_tab);
        assert!(state.cycle(true));
        assert!(state.agent_tab);
        assert!(state.cycle(true));
        assert!(state.context_active);
        assert!(!state.agent_tab);
        assert!(state.cycle(true));
        assert_eq!(state.active, 1);
    }

    #[test]
    fn the_agent_tab_releases_the_editor_and_returns_to_the_same_note() {
        let mut state = state_with("a note");
        state.set_files(vec![NotepadFile {
            path: PathBuf::from("/notes/todo.md"),
            name: "todo".into(),
        }]);
        state.set_visible_tabs(vec!["note:todo".into(), "agent".into()]);
        state.focused = true;

        assert!(state.select_agent_tab());
        assert!(state.agent_tab);
        assert!(!state.focused);
        assert!(!state.select_agent_tab(), "already showing");

        // The note index never moved, so coming back is a view switch, not a
        // note switch — and it still has to happen.
        assert!(state.select(0));
        assert!(!state.agent_tab);
        assert_eq!(state.active, 0);
    }

    #[test]
    fn agent_sections_default_expanded_and_toggle_individually() {
        let mut state = state_with("");
        for section in [
            AgentSection::Status,
            AgentSection::Tasks,
            AgentSection::Subagents,
            AgentSection::Links,
        ] {
            assert!(!state.agent_collapsed.collapsed(section));
        }
        state.toggle_agent_section(AgentSection::Tasks);
        assert!(state.agent_collapsed.collapsed(AgentSection::Tasks));
        assert!(!state.agent_collapsed.collapsed(AgentSection::Links));
        state.toggle_agent_section(AgentSection::Tasks);
        assert!(!state.agent_collapsed.collapsed(AgentSection::Tasks));
    }

    #[test]
    fn agent_scroll_is_clamped_to_the_derived_rows() {
        let mut state = state_with("");
        state.agent_scroll_by(5, 3);
        assert_eq!(state.agent_scroll, 3);
        state.agent_scroll_by(-10, 3);
        assert_eq!(state.agent_scroll, 0);
    }

    #[test]
    fn selecting_a_note_leaves_the_context_tab() {
        let mut state = state_with("");
        state.set_files(vec![NotepadFile {
            path: PathBuf::from("/notes/a.md"),
            name: "a".into(),
        }]);
        state.set_visible_tabs(vec!["note:a".into(), "context".into()]);
        assert!(state.select_context());
        assert!(!state.select_context());
        // Re-selecting the already-active note still leaves the Context tab.
        assert!(state.select(0));
        assert!(!state.context_active);
    }

    #[test]
    fn configured_order_wins_over_alphabetical_order() {
        let dir = std::env::temp_dir().join(format!("herdr-notepad-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp notes dir");
        for name in ["alpha.md", "todo.md", "zeta.md", "ignored.txt"] {
            std::fs::write(dir.join(name), "").expect("write note");
        }
        let files = discover_notes(&dir, &["todo".to_string()]);
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, vec!["todo", "alpha", "zeta"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_and_loading_a_note_roundtrips() {
        let dir = std::env::temp_dir().join(format!("herdr-notepad-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("todo.md");
        save_note(&path, "- one\n- two\n").expect("save");
        assert_eq!(load_note(&path).expect("load"), "- one\n- two\n");
        assert!(!dir.join("todo.md.herdr-tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_note_loads_as_empty_rather_than_an_error() {
        let path = std::env::temp_dir().join("herdr-notepad-missing-note.md");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_note(&path).expect("load"), "");
    }
}
