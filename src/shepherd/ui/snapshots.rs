//! Snapshot timeline of an agent (F11c): pure state + key handling.
//!
//! The timeline is a sub-view of the Agents tab. It only builds typed requests
//! (`ShepherdSnapshotsAction`) and ingests typed results; all git work happens in the
//! server (`shepherd.snapshot.*`). Rendering lives in `snapshots_view.rs`.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shepherd_core::snapshot::{
    CaptureResult, DeleteResult, DiffFile, DiffResult, PruneResult, RestoreResult, ShowResult,
    SnapshotInfo,
};

use super::components::inline_form::{FormField, InlineFormState};

/// Diff lines kept in memory/rendered; the rest is summarised in a notice.
pub const MAX_DIFF_LINES: usize = 3000;
/// A single diff line is cut at this many characters.
pub const MAX_DIFF_LINE_CHARS: usize = 400;
/// Default for the `keep` field of the prune form.
pub const DEFAULT_PRUNE_KEEP: &str = "10";

/// Typed requests the timeline asks the client to send (one per `shepherd.snapshot.*` call).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdSnapshotsAction {
    /// `agent: None` lists every snapshot of the repository (used for finished agents).
    List {
        root: Option<String>,
        agent: Option<String>,
    },
    /// Summary (no patch) of one snapshot against its predecessor.
    Show {
        root: String,
        id: String,
    },
    Capture {
        root: Option<String>,
        agent: String,
    },
    /// Full patch of the snapshot against its predecessor.
    DiffPrevious {
        root: String,
        id: String,
    },
    /// Full patch of the snapshot against the current worktree.
    DiffCurrent {
        root: String,
        id: String,
    },
    /// Names only: files that differ between the snapshot and the current worktree.
    FilesVsCurrent {
        root: String,
        id: String,
    },
    /// `paths` empty = whole worktree (the client sets `confirm`).
    Restore {
        root: String,
        id: String,
        paths: Vec<String>,
    },
    Delete {
        root: String,
        id: String,
    },
    Prune {
        root: Option<String>,
        agent: String,
        keep: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SnapshotSource {
    /// Snapshots of the selected agent.
    #[default]
    Agent,
    /// Snapshots of agents that are finished or no longer registered.
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffMode {
    Previous,
    Current,
}

/// Buttons of the detail pane, in display order (shared by render and mouse routing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotButton {
    DiffPrevious,
    DiffCurrent,
    RestoreFile,
    RestoreAll,
    Delete,
    Prune,
    Back,
}

impl SnapshotButton {
    pub const ALL: [Self; 7] = [
        Self::DiffPrevious,
        Self::DiffCurrent,
        Self::RestoreFile,
        Self::RestoreAll,
        Self::Delete,
        Self::Prune,
        Self::Back,
    ];

    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::DiffPrevious => ("Ver diff vs anterior", "d"),
            Self::DiffCurrent => ("Ver diff vs actual", "D"),
            Self::RestoreFile => ("Restaurar archivo…", "f"),
            Self::RestoreAll => ("Restaurar todo", "R"),
            Self::Delete => ("Borrar", "x"),
            Self::Prune => ("Podar…", "p"),
            Self::Back => ("Volver", "Esc"),
        }
    }
}

/// One selectable file of the "restore file" selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChoice {
    pub path: String,
    pub status: String,
    pub added: u64,
    pub removed: u64,
    pub checked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSelector {
    pub id: String,
    pub files: Vec<FileChoice>,
    pub cursor: usize,
    /// Files only present now (not in the snapshot): cannot be restored.
    pub not_restorable: usize,
}

impl FileSelector {
    pub fn checked_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| f.checked)
            .map(|f| f.path.clone())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SnapSub {
    #[default]
    None,
    Files(FileSelector),
    ConfirmFiles {
        id: String,
        paths: Vec<String>,
    },
    ConfirmAll(String),
    ConfirmDelete(String),
    Prune(InlineFormState),
}

/// Patch of the selected snapshot, ready to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffView {
    pub id: String,
    pub mode: DiffMode,
    pub lines: Vec<String>,
    /// Lines dropped because the patch exceeded `MAX_DIFF_LINES`.
    pub hidden: usize,
    pub files: usize,
    pub added: u64,
    pub removed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ShepherdSnapshotsState {
    pub open: bool,
    pub source: SnapshotSource,
    pub agent_id: String,
    /// Worktree (or repo root) used to list; `None` lets the server pick its own.
    pub root: Option<String>,
    /// Snapshots of `agent_id`, newest first.
    pub agent_snaps: Vec<SnapshotInfo>,
    /// Snapshots of finished/unregistered agents, newest first.
    pub finished_snaps: Vec<SnapshotInfo>,
    /// Agents that are alive; everything else with snapshots counts as finished.
    pub active_agents: Vec<String>,
    pub selected: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    /// Single-column layouts: show the detail instead of the list.
    pub detail_open: bool,
    pub summaries: HashMap<String, DiffResult>,
    pub summary_inflight: Option<String>,
    /// Background summary that failed (e.g. the snapshot was deleted meanwhile): not retried
    /// until the selection changes, and not shown as an error.
    pub summary_failed: Option<String>,
    /// Select this snapshot when the next list arrives (the one a capture/restore just made).
    pub pending_select: Option<String>,
    pub diff: Option<DiffView>,
    pub sub: SnapSub,
    pub pending_files: Option<String>,
    /// "trabajando…" label of the slow operation in flight.
    pub busy: Option<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
    /// Unix seconds of the last refresh (relative times are computed from it, never in render).
    pub now: u64,
}

fn newest(n: u64) -> std::cmp::Reverse<u64> {
    std::cmp::Reverse(n)
}

fn clean_lines(patch: &str) -> (Vec<String>, usize) {
    let total = patch.lines().count();
    let lines = patch
        .lines()
        .take(MAX_DIFF_LINES)
        .map(|l| {
            if l.chars().count() > MAX_DIFF_LINE_CHARS {
                let mut s: String = l.chars().take(MAX_DIFF_LINE_CHARS).collect();
                s.push('…');
                s
            } else {
                l.replace('\t', "    ")
            }
        })
        .collect();
    (lines, total.saturating_sub(MAX_DIFF_LINES))
}

impl ShepherdSnapshotsState {
    // ---------- queries ----------

    pub fn current(&self) -> &[SnapshotInfo] {
        match self.source {
            SnapshotSource::Agent => &self.agent_snaps,
            SnapshotSource::Finished => &self.finished_snaps,
        }
    }

    pub fn selected_snapshot(&self) -> Option<&SnapshotInfo> {
        self.current().get(self.selected)
    }

    pub fn has_sub_action(&self) -> bool {
        self.sub != SnapSub::None
    }

    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    pub fn summary_for_selected(&self) -> Option<&DiffResult> {
        self.selected_snapshot()
            .and_then(|s| self.summaries.get(&s.id))
    }

    pub fn diff_for_selected(&self) -> Option<&DiffView> {
        let id = &self.selected_snapshot()?.id;
        self.diff.as_ref().filter(|d| &d.id == id)
    }

    fn root_of(&self, s: &SnapshotInfo) -> String {
        if s.worktree.is_empty() {
            self.root.clone().unwrap_or_default()
        } else {
            s.worktree.clone()
        }
    }

    // ---------- opening / closing ----------

    /// Open the timeline of `agent_id`; returns the list request to send.
    pub fn open_for(
        &mut self,
        agent_id: &str,
        root: Option<String>,
    ) -> Option<ShepherdSnapshotsAction> {
        let same = self.open && self.agent_id == agent_id;
        self.open = true;
        self.root = root.filter(|r| !r.is_empty());
        if !same {
            self.agent_id = agent_id.to_string();
            self.source = if agent_id.is_empty() {
                SnapshotSource::Finished
            } else {
                SnapshotSource::Agent
            };
            self.agent_snaps.clear();
            self.selected = 0;
            self.list_scroll = 0;
            self.detail_scroll = 0;
            self.detail_open = false;
            self.diff = None;
            self.sub = SnapSub::None;
            self.pending_files = None;
            self.busy = None;
            self.error = None;
            self.notice = None;
        }
        self.list_request()
    }

    pub fn close(&mut self) {
        self.open = false;
        self.sub = SnapSub::None;
        self.pending_files = None;
        self.busy = None;
        self.error = None;
        self.detail_open = false;
    }

    /// Esc: cancel the sub-action, then leave compact detail, then close the timeline.
    pub fn cancel(&mut self) -> bool {
        if !self.open {
            return false;
        }
        if self.sub != SnapSub::None {
            self.sub = SnapSub::None;
            self.error = None;
        } else if self.detail_open {
            self.detail_open = false;
        } else {
            self.close();
        }
        true
    }

    // ---------- requests ----------

    pub fn list_request(&self) -> Option<ShepherdSnapshotsAction> {
        if !self.open {
            return None;
        }
        // Finished agents need the whole repo; an agent timeline only its own refs.
        let agent = match self.source {
            SnapshotSource::Agent if !self.agent_id.is_empty() => Some(self.agent_id.clone()),
            _ => None,
        };
        Some(ShepherdSnapshotsAction::List {
            root: self.root.clone(),
            agent,
        })
    }

    /// Background refresh (cheap, silent): the list, plus the summary of the selection if missing.
    pub fn refresh_requests(&mut self) -> Vec<ShepherdSnapshotsAction> {
        let mut out = Vec::new();
        if self.is_busy() {
            return out;
        }
        if let Some(a) = self.list_request() {
            out.push(a);
        }
        out.extend(self.summary_request());
        out
    }

    /// `show` for the selected snapshot when its summary is not known yet.
    pub fn summary_request(&mut self) -> Option<ShepherdSnapshotsAction> {
        if !self.open {
            return None;
        }
        let snap = self.selected_snapshot()?.clone();
        if self.summaries.contains_key(&snap.id)
            || self.summary_inflight.as_deref() == Some(snap.id.as_str())
            || self.summary_failed.as_deref() == Some(snap.id.as_str())
        {
            return None;
        }
        self.summary_inflight = Some(snap.id.clone());
        Some(ShepherdSnapshotsAction::Show {
            root: self.root_of(&snap),
            id: snap.id,
        })
    }

    fn slow(&mut self, label: &str, a: ShepherdSnapshotsAction) -> Option<ShepherdSnapshotsAction> {
        self.busy = Some(label.to_string());
        self.error = None;
        self.notice = None;
        Some(a)
    }

    pub fn start_capture(&mut self) -> Option<ShepherdSnapshotsAction> {
        if self.is_busy() {
            return None;
        }
        if self.agent_id.is_empty() {
            self.error = Some("Elige un agente activo para capturar".into());
            return None;
        }
        let a = ShepherdSnapshotsAction::Capture {
            root: self.root.clone(),
            agent: self.agent_id.clone(),
        };
        self.slow("capturando…", a)
    }

    pub fn start_diff(&mut self, mode: DiffMode) -> Option<ShepherdSnapshotsAction> {
        if self.is_busy() {
            return None;
        }
        let snap = self.selected_snapshot()?.clone();
        let (root, id) = (self.root_of(&snap), snap.id);
        match mode {
            DiffMode::Previous => self.slow(
                "cargando diff…",
                ShepherdSnapshotsAction::DiffPrevious { root, id },
            ),
            DiffMode::Current => self.slow(
                "cargando diff…",
                ShepherdSnapshotsAction::DiffCurrent { root, id },
            ),
        }
    }

    pub fn start_restore_file(&mut self) -> Option<ShepherdSnapshotsAction> {
        if self.is_busy() {
            return None;
        }
        let snap = self.selected_snapshot()?.clone();
        self.pending_files = Some(snap.id.clone());
        let a = ShepherdSnapshotsAction::FilesVsCurrent {
            root: self.root_of(&snap),
            id: snap.id,
        };
        self.slow("leyendo archivos…", a)
    }

    pub fn start_restore_all(&mut self) {
        if self.is_busy() {
            return;
        }
        if let Some(s) = self.selected_snapshot() {
            self.sub = SnapSub::ConfirmAll(s.id.clone());
            self.error = None;
        }
    }

    pub fn start_delete(&mut self) {
        if self.is_busy() {
            return;
        }
        if let Some(s) = self.selected_snapshot() {
            self.sub = SnapSub::ConfirmDelete(s.id.clone());
            self.error = None;
        }
    }

    pub fn start_prune(&mut self) {
        if self.is_busy() {
            return;
        }
        if self.agent_id.is_empty() || self.source != SnapshotSource::Agent {
            self.error = Some("Para podar elige un agente (pestaña «Del agente»)".into());
            return;
        }
        let mut field = FormField::new("Conservar los últimos N snapshots (keep)", "ej. 10");
        field.value = DEFAULT_PRUNE_KEEP.to_string();
        self.sub = SnapSub::Prune(InlineFormState::new(
            &format!("PODAR SNAPSHOTS DE {}", self.agent_id),
            vec![field],
        ));
        self.error = None;
    }

    /// Enter in the file selector: go to the confirmation stripe.
    pub fn submit_files(&mut self) {
        let SnapSub::Files(sel) = &self.sub else {
            return;
        };
        let paths = sel.checked_paths();
        if paths.is_empty() {
            self.error = Some("Marca al menos un archivo (Espacio)".into());
            return;
        }
        self.sub = SnapSub::ConfirmFiles {
            id: sel.id.clone(),
            paths,
        };
        self.error = None;
    }

    pub fn submit_prune(&mut self) -> Option<ShepherdSnapshotsAction> {
        let SnapSub::Prune(form) = &mut self.sub else {
            return None;
        };
        let raw = form.fields[0].value.trim().to_string();
        let keep: usize = match raw.parse() {
            Ok(n) if n >= 1 => n,
            _ => {
                form.error = Some("keep debe ser un número entero ≥ 1".into());
                return None;
            }
        };
        self.sub = SnapSub::None;
        let a = ShepherdSnapshotsAction::Prune {
            root: self.root.clone(),
            agent: self.agent_id.clone(),
            keep,
        };
        self.slow("podando…", a)
    }

    /// Run the pending confirmation (stripe button or Enter).
    pub fn confirm(&mut self) -> Option<ShepherdSnapshotsAction> {
        match std::mem::take(&mut self.sub) {
            SnapSub::ConfirmFiles { id, paths } => {
                let root = self.root_for_id(&id);
                self.slow(
                    "restaurando…",
                    ShepherdSnapshotsAction::Restore { root, id, paths },
                )
            }
            SnapSub::ConfirmAll(id) => {
                let root = self.root_for_id(&id);
                self.slow(
                    "restaurando…",
                    ShepherdSnapshotsAction::Restore {
                        root,
                        id,
                        paths: Vec::new(),
                    },
                )
            }
            SnapSub::ConfirmDelete(id) => {
                let root = self.root_for_id(&id);
                self.slow("borrando…", ShepherdSnapshotsAction::Delete { root, id })
            }
            other => {
                self.sub = other;
                None
            }
        }
    }

    fn root_for_id(&self, id: &str) -> String {
        self.current()
            .iter()
            .chain(self.agent_snaps.iter())
            .chain(self.finished_snaps.iter())
            .find(|s| s.id == id)
            .map(|s| self.root_of(s))
            .unwrap_or_default()
    }

    // ---------- selection ----------

    fn on_selection_changed(&mut self) -> Option<ShepherdSnapshotsAction> {
        self.detail_scroll = 0;
        self.summary_failed = None;
        self.summary_request()
    }

    pub fn select(&mut self, idx: usize) -> Option<ShepherdSnapshotsAction> {
        if idx < self.current().len() && idx != self.selected {
            self.selected = idx;
            return self.on_selection_changed();
        }
        None
    }

    pub fn select_prev(&mut self) -> Option<ShepherdSnapshotsAction> {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.list_scroll {
                self.list_scroll = self.selected;
            }
            return self.on_selection_changed();
        }
        None
    }

    pub fn select_next(&mut self) -> Option<ShepherdSnapshotsAction> {
        if self.selected + 1 < self.current().len() {
            self.selected += 1;
            return self.on_selection_changed();
        }
        None
    }

    pub fn set_source(&mut self, source: SnapshotSource) -> Option<ShepherdSnapshotsAction> {
        if self.source == source || (source == SnapshotSource::Agent && self.agent_id.is_empty()) {
            return None;
        }
        self.source = source;
        self.selected = 0;
        self.list_scroll = 0;
        self.detail_scroll = 0;
        self.detail_open = false;
        self.diff = None;
        self.sub = SnapSub::None;
        self.error = None;
        self.notice = None;
        self.list_request()
    }

    pub fn toggle_source(&mut self) -> Option<ShepherdSnapshotsAction> {
        let next = match self.source {
            SnapshotSource::Agent => SnapshotSource::Finished,
            SnapshotSource::Finished => SnapshotSource::Agent,
        };
        self.set_source(next)
    }

    pub fn scroll_detail(&mut self, delta: isize) {
        self.detail_scroll = self.detail_scroll.saturating_add_signed(delta);
    }

    pub fn press(&mut self, b: SnapshotButton) -> Option<ShepherdSnapshotsAction> {
        match b {
            SnapshotButton::DiffPrevious => self.start_diff(DiffMode::Previous),
            SnapshotButton::DiffCurrent => self.start_diff(DiffMode::Current),
            SnapshotButton::RestoreFile => self.start_restore_file(),
            SnapshotButton::RestoreAll => {
                self.start_restore_all();
                None
            }
            SnapshotButton::Delete => {
                self.start_delete();
                None
            }
            SnapshotButton::Prune => {
                self.start_prune();
                None
            }
            SnapshotButton::Back => {
                self.cancel();
                None
            }
        }
    }

    // ---------- results ----------

    fn sort_newest_first(v: &mut [SnapshotInfo]) {
        v.sort_by_key(|s| (newest(s.created), newest(s.n)));
    }

    /// `agent`: the agent the request was for (`None` = repository-wide list).
    pub fn ingest_list(&mut self, agent: Option<&str>, mut snaps: Vec<SnapshotInfo>, now: u64) {
        self.now = now;
        let keep_id = self.selected_snapshot().map(|s| s.id.clone());
        match agent {
            Some(a) if a == self.agent_id => {
                snaps.retain(|s| s.agent == a);
                Self::sort_newest_first(&mut snaps);
                self.agent_snaps = snaps;
            }
            Some(_) => return, // answer for another agent (stale)
            None => {
                snaps.retain(|s| !self.active_agents.iter().any(|a| *a == s.agent));
                Self::sort_newest_first(&mut snaps);
                self.finished_snaps = snaps;
            }
        }
        // keep the same snapshot selected when the list shifts under us
        let len = self.current().len();
        let keep_id = self.pending_select.take().or(keep_id);
        if let Some(id) = keep_id {
            if let Some(pos) = self.current().iter().position(|s| s.id == id) {
                self.selected = pos;
            }
        }
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }

    pub fn ingest_show(&mut self, r: ShowResult) {
        if self.summary_inflight.as_deref() == Some(r.snapshot.id.as_str()) {
            self.summary_inflight = None;
        }
        self.summaries.insert(r.snapshot.id, r.diff);
    }

    pub fn ingest_capture(&mut self, r: &CaptureResult) {
        self.busy = None;
        self.error = None;
        self.notice = Some(if r.created {
            format!(
                "Capturado {} ({}, {} archivo(s), +{} −{})",
                r.snapshot.id,
                r.snapshot.reason.as_str(),
                r.snapshot.files,
                r.snapshot.added,
                r.snapshot.removed
            )
        } else {
            format!("Sin cambios desde {}: no se creó snapshot", r.snapshot.id)
        });
        if r.created {
            self.pending_select = Some(r.snapshot.id.clone());
        }
    }

    pub fn ingest_diff(&mut self, id: &str, mode: DiffMode, d: &DiffResult) {
        self.busy = None;
        let (lines, hidden) = clean_lines(d.patch.as_deref().unwrap_or(""));
        self.diff = Some(DiffView {
            id: id.to_string(),
            mode,
            lines,
            hidden,
            files: d.files.len(),
            added: d.added,
            removed: d.removed,
        });
        self.detail_scroll = 0;
        self.detail_open = true;
    }

    pub fn ingest_files(&mut self, id: &str, d: &DiffResult) {
        self.busy = None;
        if self.pending_files.as_deref() != Some(id) {
            return;
        }
        self.pending_files = None;
        let (restorable, extra): (Vec<&DiffFile>, Vec<&DiffFile>) =
            d.files.iter().partition(|f| f.status != "added");
        if restorable.is_empty() {
            self.notice = Some(if extra.is_empty() {
                format!("{id} coincide con el estado actual: nada que restaurar")
            } else {
                format!(
                    "{id}: solo hay archivos nuevos desde entonces ({}); no hay nada que restaurar",
                    extra.len()
                )
            });
            return;
        }
        self.sub = SnapSub::Files(FileSelector {
            id: id.to_string(),
            files: restorable
                .into_iter()
                .map(|f| FileChoice {
                    path: f.path.clone(),
                    status: f.status.clone(),
                    added: f.added,
                    removed: f.removed,
                    checked: false,
                })
                .collect(),
            cursor: 0,
            not_restorable: extra.len(),
        });
        self.detail_open = true;
    }

    pub fn ingest_restore(&mut self, r: &RestoreResult) {
        self.busy = None;
        self.error = None;
        let mut msg = format!(
            "Restaurado {} ({} archivo(s))",
            r.restored.id,
            r.restored_files.len()
        );
        if r.pre_restore_created {
            msg.push_str(&format!(
                "; estado anterior guardado como {}",
                r.pre_restore.id
            ));
            self.pending_select = Some(r.pre_restore.id.clone());
        } else {
            msg.push_str(&format!(
                "; el estado anterior ya estaba guardado como {}",
                r.pre_restore.id
            ));
        }
        if !r.extra_files.is_empty() {
            msg.push_str(&format!(
                "; se conservan {} archivo(s) extra",
                r.extra_files.len()
            ));
        }
        self.notice = Some(msg);
        self.diff = None;
    }

    pub fn ingest_delete(&mut self, r: &DeleteResult) {
        self.busy = None;
        self.error = None;
        self.notice = Some(format!("Borrado {}", r.deleted));
        self.summaries.remove(&r.deleted);
        self.diff = None;
    }

    pub fn ingest_prune(&mut self, r: &PruneResult) {
        self.busy = None;
        self.error = None;
        self.notice = Some(format!(
            "Podados {} snapshot(s); quedan {}",
            r.pruned.len(),
            r.kept
        ));
        self.diff = None;
        self.selected = 0;
        self.list_scroll = 0;
        for id in &r.pruned {
            self.summaries.remove(id);
        }
    }

    /// The background summary request failed: forget it quietly (no error banner).
    pub fn ingest_summary_error(&mut self) {
        self.summary_failed = self.summary_inflight.take();
    }

    /// Any failed request: the message stays visible in the panel.
    pub fn ingest_error(&mut self, message: &str) {
        self.busy = None;
        self.pending_files = None;
        self.summary_inflight = None;
        self.error = Some(message.to_string());
    }

    // ---------- keys ----------

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdSnapshotsAction>) {
        if !self.open {
            return (false, None);
        }
        match &mut self.sub {
            SnapSub::ConfirmFiles { .. } | SnapSub::ConfirmAll(_) | SnapSub::ConfirmDelete(_) => {
                return match key.code {
                    KeyCode::Enter => (true, self.confirm()),
                    KeyCode::Esc => {
                        self.cancel();
                        (true, None)
                    }
                    _ => (true, None),
                };
            }
            SnapSub::Prune(form) => {
                match key.code {
                    KeyCode::Esc => {
                        self.cancel();
                    }
                    KeyCode::Enter => return (true, self.submit_prune()),
                    KeyCode::Backspace => {
                        form.fields[0].value.pop();
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        form.fields[0].value.push(c);
                    }
                    _ => {}
                }
                return (true, None);
            }
            SnapSub::Files(sel) => {
                match key.code {
                    KeyCode::Esc => {
                        self.cancel();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        sel.cursor = sel.cursor.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if sel.cursor + 1 < sel.files.len() {
                            sel.cursor += 1;
                        }
                    }
                    KeyCode::Char(' ') => {
                        let c = sel.cursor;
                        if let Some(f) = sel.files.get_mut(c) {
                            f.checked = !f.checked;
                        }
                    }
                    KeyCode::Char('a') => {
                        let all = sel.files.iter().all(|f| f.checked);
                        for f in &mut sel.files {
                            f.checked = !all;
                        }
                    }
                    KeyCode::Enter => self.submit_files(),
                    _ => {}
                }
                return (true, None);
            }
            SnapSub::None => {}
        }

        let act = match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Enter => {
                if self.selected_snapshot().is_some() {
                    self.detail_open = !self.detail_open;
                }
                None
            }
            KeyCode::PageUp => {
                self.scroll_detail(-10);
                None
            }
            KeyCode::PageDown => {
                self.scroll_detail(10);
                None
            }
            KeyCode::Home => {
                self.detail_scroll = 0;
                None
            }
            KeyCode::Tab | KeyCode::BackTab => self.toggle_source(),
            KeyCode::Char('c') => self.start_capture(),
            KeyCode::Char('d') => self.start_diff(DiffMode::Previous),
            KeyCode::Char('D') => self.start_diff(DiffMode::Current),
            KeyCode::Char('f') => self.start_restore_file(),
            KeyCode::Char('R') => {
                self.start_restore_all();
                None
            }
            KeyCode::Char('x') => {
                self.start_delete();
                None
            }
            KeyCode::Char('p') => {
                self.start_prune();
                None
            }
            KeyCode::Char('r') => self.list_request(),
            _ => return (false, None),
        };
        (true, act)
    }
}
