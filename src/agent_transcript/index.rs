//! A per-session index of an omp session file: one small record per JSONL
//! entry (byte range, parent, kind), built by one streaming scan and extended
//! by tailing the appended bytes. Pages read only the lines they render.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub(super) const NO_PARENT: u32 = u32::MAX;

/// Reminders and mode context omp injects for the model, not for the user.
const HIDDEN_NOTE_TYPES: &[&str] = &[
    "mid-run-todo-nudge",
    "resolve-reminder",
    "todo-error-reminder",
    "plan-mode-context",
    "plan-mode-reference",
    "vibe-mode-context",
    "tool-call-loop-redirect",
    "thinking-loop-redirect",
    "interrupted-thinking",
    "tool-roster-notice",
    "eval-prelude-notice",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    User,
    Assistant,
    ToolResult,
    /// A `todo` tool result: carries the full todo state.
    TodoResult,
    /// A `custom_message` shown to the user.
    Note,
    ModelChange,
    Recap,
    Compaction,
    /// omp `/clear`: the conversation restarts after it.
    ResetBoundary,
    /// The omp process exited; nothing still pending can complete.
    SessionExit,
    Hidden,
}

impl Kind {
    /// Entries a page reads and pays for. Tool results render nothing of
    /// their own (their status is in the record) and bookkeeping is skipped.
    pub(super) fn is_rendered(self) -> bool {
        !matches!(
            self,
            Kind::ToolResult
                | Kind::Hidden
                | Kind::ResetBoundary
                | Kind::SessionExit
                | Kind::ModelChange
                | Kind::Recap
        )
    }
}

#[derive(Debug)]
pub(super) struct Record {
    pub(super) offset: u64,
    pub(super) len: u32,
    pub(super) parent: u32,
    pub(super) kind: Kind,
    /// A tool result that reported an error.
    pub(super) failed: bool,
    pub(super) id: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Self {
                dev: meta.dev(),
                ino: meta.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            // Windows has no stable inode in std; a replaced file is caught by
            // the last-record check in `refresh` instead.
            let _ = meta;
            Self {}
        }
    }
}

#[derive(Debug)]
pub(super) struct SessionIndex {
    path: PathBuf,
    identity: Option<FileIdentity>,
    /// Bytes consumed; always just past a newline.
    scanned: u64,
    pub(super) session_id: Option<String>,
    pub(super) records: Vec<Record>,
    by_id: HashMap<Box<str>, u32>,
    /// `toolCallId` → its tool result record.
    results: HashMap<Box<str>, u32>,
    /// Lines that were not valid JSON entries; skipped.
    pub(super) malformed: u64,
    /// Record indices of the active branch (root first), all of it.
    branch: Vec<u32>,
    /// Where the visible conversation starts in `branch` (after a `/clear`).
    branch_start: usize,
}

#[derive(Deserialize)]
struct Head<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    id: Option<Cow<'a, str>>,
    #[serde(rename = "parentId", borrow, default)]
    parent_id: Option<Cow<'a, str>>,
    #[serde(rename = "customType", borrow, default)]
    custom_type: Option<Cow<'a, str>>,
    #[serde(default)]
    display: Option<bool>,
    #[serde(borrow, default)]
    role: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    message: Option<MessageHead<'a>>,
}

#[derive(Deserialize)]
struct MessageHead<'a> {
    #[serde(borrow, default)]
    role: Option<Cow<'a, str>>,
    #[serde(rename = "toolCallId", borrow, default)]
    tool_call_id: Option<Cow<'a, str>>,
    #[serde(rename = "toolName", borrow, default)]
    tool_name: Option<Cow<'a, str>>,
    #[serde(rename = "isError", default)]
    is_error: Option<bool>,
}

impl SessionIndex {
    pub(super) fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            identity: None,
            scanned: 0,
            session_id: None,
            records: Vec::new(),
            by_id: HashMap::new(),
            results: HashMap::new(),
            malformed: 0,
            branch: Vec::new(),
            branch_start: 0,
        }
    }

    /// The visible active branch: record indices in file order.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn branch(&self) -> &[u32] {
        &self.branch[self.branch_start..]
    }

    /// Model selection survives `/clear`, but not a switch to a different branch.
    pub(super) fn model_records(&self) -> impl Iterator<Item = u32> + '_ {
        self.branch.iter().rev().copied().filter(|&record| {
            matches!(
                self.record(record).kind,
                Kind::ModelChange | Kind::Assistant
            )
        })
    }

    pub(super) fn record(&self, index: u32) -> &Record {
        &self.records[index as usize]
    }

    pub(super) fn result_for(&self, tool_call_id: &str) -> Option<u32> {
        self.results.get(tool_call_id).copied()
    }

    /// Catch up with the file: tail what was appended, or rebuild from scratch
    /// when the file was replaced, truncated or rewritten under the index.
    pub(super) fn refresh(&mut self) -> std::io::Result<File> {
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        let identity = FileIdentity::of(&meta);
        let rewritten = self.identity.is_some_and(|known| known != identity)
            || meta.len() < self.scanned
            || !self.last_record_intact(&mut file)?;
        if rewritten {
            *self = Self::new(&self.path);
        }
        self.identity = Some(identity);
        if meta.len() > self.scanned {
            let before = self.records.len();
            self.scan(&mut file)?;
            self.extend_branch(before);
        }
        Ok(file)
    }

    /// Whether the newest indexed line still holds the entry indexed there.
    fn last_record_intact(&self, file: &mut File) -> std::io::Result<bool> {
        let Some(last) = self.records.last() else {
            return Ok(true);
        };
        // Entries start `{"type":…,"id":"…"`; a short prefix settles it
        // without reading a multi-megabyte tool result.
        let prefix_len = u64::from(last.len).min(256);
        let mut prefix = vec![0; prefix_len as usize];
        file.seek(SeekFrom::Start(last.offset))?;
        if file.read_exact(&mut prefix).is_err() {
            return Ok(false);
        }
        let needle = format!("\"id\":\"{}\"", last.id);
        if contains(&prefix, needle.as_bytes()) {
            return Ok(true);
        }
        let line = read_line(file, last)?;
        Ok(serde_json::from_slice::<Head>(&line)
            .ok()
            .and_then(|head| head.id.map(|id| *id == *last.id))
            .unwrap_or(false))
    }

    fn scan(&mut self, file: &mut File) -> std::io::Result<()> {
        file.seek(SeekFrom::Start(self.scanned))?;
        let mut reader = BufReader::with_capacity(1 << 20, file);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 || line.last() != Some(&b'\n') {
                // A line still being written is picked up by the next refresh.
                break;
            }
            let offset = self.scanned;
            self.scanned += read as u64;
            let mut body = &line[..line.len() - 1];
            if body.last() == Some(&b'\r') {
                body = &body[..body.len() - 1];
            }
            if body.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            self.index_line(offset, body);
        }
        Ok(())
    }

    fn index_line(&mut self, offset: u64, body: &[u8]) {
        let Ok(head) = serde_json::from_slice::<Head>(body) else {
            self.malformed += 1;
            return;
        };
        let kind_name = head.kind.as_deref().unwrap_or_default();
        if kind_name == "session" {
            self.session_id = head.id.map(Cow::into_owned);
            return;
        }
        let Some(id) = head.id.filter(|id| !id.is_empty()) else {
            // The title slot and other header lines carry no entry id.
            if kind_name != "title" {
                self.malformed += 1;
            }
            return;
        };
        let Ok(len) = u32::try_from(body.len()) else {
            self.malformed += 1;
            return;
        };
        let index = self.records.len() as u32;
        let kind = match kind_name {
            "message" => {
                let message = head.message.as_ref();
                match message.and_then(|message| message.role.as_deref()) {
                    Some("user") => Kind::User,
                    Some("assistant") => Kind::Assistant,
                    Some("toolResult") => {
                        if let Some(call_id) = message.and_then(|m| m.tool_call_id.as_deref()) {
                            self.results.insert(call_id.into(), index);
                        }
                        if message.and_then(|m| m.tool_name.as_deref()) == Some("todo") {
                            Kind::TodoResult
                        } else {
                            Kind::ToolResult
                        }
                    }
                    _ => Kind::Hidden,
                }
            }
            "custom_message" => {
                let custom = head.custom_type.as_deref().unwrap_or_default();
                if head.display == Some(false) || HIDDEN_NOTE_TYPES.contains(&custom) {
                    Kind::Hidden
                } else {
                    Kind::Note
                }
            }
            "compaction" => Kind::Compaction,
            "reset_boundary" => Kind::ResetBoundary,
            "model_change" if matches!(head.role.as_deref(), None | Some("default")) => {
                Kind::ModelChange
            }
            "custom" if head.custom_type.as_deref() == Some("session_exit") => Kind::SessionExit,
            "custom" if head.custom_type.as_deref() == Some("completion-recap") => Kind::Recap,
            _ => Kind::Hidden,
        };
        let failed = kind == Kind::ToolResult
            && head
                .message
                .as_ref()
                .and_then(|message| message.is_error)
                .unwrap_or(false);
        let parent = head
            .parent_id
            .as_deref()
            .and_then(|parent| self.by_id.get(parent).copied())
            .unwrap_or(NO_PARENT);
        let id: Box<str> = id.as_ref().into();
        self.by_id.insert(id.clone(), index);
        self.records.push(Record {
            offset,
            len,
            parent,
            kind,
            failed,
            id,
        });
    }

    /// The active branch is the parent chain of the newest entry. Appends
    /// usually continue it; a rewind or `/tree` jump starts a new chain.
    fn extend_branch(&mut self, first_new: usize) {
        let Some(leaf) = self.records.len().checked_sub(1) else {
            return;
        };
        if first_new == 0 || self.branch.last().is_none() {
            self.rebuild_branch(leaf as u32);
            return;
        }
        let old_leaf = *self.branch.last().unwrap_or(&NO_PARENT);
        let mut tail = Vec::new();
        let mut cursor = leaf as u32;
        while cursor != NO_PARENT && cursor as usize >= first_new {
            tail.push(cursor);
            cursor = self.records[cursor as usize].parent;
        }
        if cursor != old_leaf {
            self.rebuild_branch(leaf as u32);
            return;
        }
        tail.reverse();
        let before = self.branch.len();
        self.branch.extend(tail);
        if let Some(reset) = self.branch[before..]
            .iter()
            .rposition(|&rec| self.records[rec as usize].kind == Kind::ResetBoundary)
        {
            self.branch_start = before + reset + 1;
        }
    }

    fn rebuild_branch(&mut self, leaf: u32) {
        self.branch.clear();
        let mut cursor = leaf;
        // Parents always precede children, so the walk strictly descends and
        // cannot loop.
        while cursor != NO_PARENT {
            self.branch.push(cursor);
            let parent = self.records[cursor as usize].parent;
            if parent != NO_PARENT && parent >= cursor {
                break;
            }
            cursor = parent;
        }
        self.branch.reverse();
        self.branch_start = self
            .branch
            .iter()
            .rposition(|&rec| self.records[rec as usize].kind == Kind::ResetBoundary)
            .map_or(0, |reset| reset + 1);
    }
}

pub(super) fn read_line(file: &mut File, record: &Record) -> std::io::Result<Vec<u8>> {
    let mut line = vec![0; record.len as usize];
    file.seek(SeekFrom::Start(record.offset))?;
    file.read_exact(&mut line)?;
    Ok(line)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
