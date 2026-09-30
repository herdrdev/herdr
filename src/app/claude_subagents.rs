use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime};

use serde_json::Value;

use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind};
use crate::agent_state::{AgentTask, AgentTaskStatus, ObservedAgentLink};
use crate::detect::AgentState;

pub(crate) const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1500);
pub(crate) const WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const PER_TARGET_READ_QUANTUM: usize = 128 * 1024;
pub(crate) const BATCH_READ_BUDGET: usize = 2 * 1024 * 1024;
const MAX_TOTAL_CARRY: usize = 2 * 1024 * 1024;
// Real Claude rows have reached 514,181 bytes. Keep one row up to 1 MiB so
// those valid records parse across bounded read quanta. A row beyond this cap
// is discarded and makes subagent history untrustworthy: it could itself be a
// lifecycle event, so later rows must not silently restore authority.
const MAX_BUFFERED_JSON_ROW: usize = 1024 * 1024;
const MAX_ACTIVE_IDS: usize = 4096;
const MAX_SUBAGENT_ID_BYTES: usize = 512;
const MAX_TRANSCRIPT_PATH_BYTES: usize = 4096;
const PENDING_TASK_ID_PREFIX: &str = "tool-use:";

#[derive(Clone, Debug, PartialEq, Eq)]
enum TranscriptEvent {
    Started(ClaudeSubagentObservation),
    StateChanged {
        id: String,
        state: AgentState,
        observed_at: Option<SystemTime>,
    },
}

/// One Claude Task observation reconstructed by the existing transcript scan.
/// This remains runtime-only evidence alongside the active count derived from
/// the same rows; it is never persisted or exposed through the server API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClaudeSubagentObservation {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) name: String,
    pub(crate) state: AgentState,
    pub(crate) observed_at: Option<SystemTime>,
    pub(crate) transcript_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TranscriptCursor {
    pub(crate) scan_offset: u64,
    pub(crate) committed_offset: u64,
    line_buffer: Vec<u8>,
    line_overflowed: bool,
    pub(crate) active_ids: HashSet<String>,
    pub(crate) observations: Vec<ClaudeSubagentObservation>,
    pub(crate) tasks: Vec<TrackedTask>,
    pub(crate) links: Vec<TranscriptLink>,
    pub(crate) last_row_at: Option<SystemTime>,
    pub(crate) last_tasks_at: Option<SystemTime>,
    pub(crate) caught_up_once: bool,
    pub(crate) trustworthy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptLink {
    pub(crate) url: String,
    pub(crate) first_seen: SystemTime,
    pub(crate) last_seen: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackedTask {
    id: String,
    task: AgentTask,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParseStats {
    pub(crate) bytes_read: u64,
    pub(crate) lines_parsed: u64,
    pub(crate) partial_rows: u64,
    pub(crate) oversized_rows: u64,
    pub(crate) malformed_rows: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptTracker {
    pub(crate) session_id: String,
    pub(crate) path: PathBuf,
    pub(crate) target_generation: u64,
    pub(crate) cursor: TranscriptCursor,
    file_identity: Option<FileIdentity>,
    last_len: Option<u64>,
    last_modified: Option<SystemTime>,
    transcript_paths_initialized: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScanStats {
    pub(crate) parse: ParseStats,
    pub(crate) files_opened: u64,
    pub(crate) metadata_hits: u64,
    pub(crate) identity_resets: u64,
    pub(crate) open_failures: u64,
    pub(crate) deadline_exhausted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetIdentity {
    pub(crate) terminal_id: crate::terminal::TerminalId,
    pub(crate) source: String,
    pub(crate) session_id: String,
    pub(crate) path: PathBuf,
    pub(crate) target_generation: u64,
    pub(crate) turn_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RefreshObservation {
    pub(crate) target: TargetIdentity,
    pub(crate) tracker: TranscriptTracker,
    pub(crate) count: Option<u32>,
    pub(crate) observations: Option<Vec<ClaudeSubagentObservation>>,
    pub(crate) transcript_activity: crate::terminal::state::SubagentTranscriptActivity,
    pub(crate) stats: ScanStats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BatchStats {
    pub(crate) targets_total: u64,
    pub(crate) targets_attempted: u64,
    pub(crate) files_opened: u64,
    pub(crate) metadata_hits: u64,
    pub(crate) bytes_read: u64,
    pub(crate) lines_parsed: u64,
    pub(crate) partial_rows: u64,
    pub(crate) oversized_rows: u64,
    pub(crate) malformed_rows: u64,
    pub(crate) identity_resets: u64,
    pub(crate) open_failures: u64,
    pub(crate) targets_caught_up: u64,
    pub(crate) deadline_exhausted: bool,
    pub(crate) elapsed_us: u64,
    pub(crate) max_target_us: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RefreshWorkItem {
    pub(crate) target: TargetIdentity,
    pub(crate) tracker: TranscriptTracker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows(crate::platform::WindowsFileIdentity),
    #[cfg(not(any(unix, windows)))]
    Portable { created: Option<SystemTime> },
}

impl TranscriptCursor {
    pub(crate) fn new() -> Self {
        Self {
            trustworthy: true,
            ..Self::default()
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn count(&self) -> Option<u32> {
        (self.caught_up_once && self.trustworthy)
            .then(|| u32::try_from(self.active_ids.len()).unwrap_or(u32::MAX))
    }

    pub(crate) fn observations(&self) -> Option<&[ClaudeSubagentObservation]> {
        (self.caught_up_once && self.trustworthy).then_some(&self.observations)
    }

    pub(crate) fn tasks(&self) -> Option<Vec<AgentTask>> {
        (self.caught_up_once && self.trustworthy)
            .then(|| self.tasks.iter().map(|task| task.task.clone()).collect())
    }

    fn apply_task_result(&mut self, value: &Value) -> bool {
        let mut changed = false;
        if let Some(task) = value
            .get("toolUseResult")
            .and_then(|result| result.get("task"))
        {
            let Some(id) = task.get("id").and_then(Value::as_str) else {
                return false;
            };
            let Some(text) = task
                .get("subject")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                return false;
            };
            if let Some(existing) = self.tasks.iter_mut().find(|task| task.id == id) {
                if existing.task.text != text {
                    existing.task.text = text.to_string();
                    changed = true;
                }
            } else if let Some(pending) = self
                .tasks
                .iter_mut()
                .find(|task| task.id.starts_with(PENDING_TASK_ID_PREFIX) && task.task.text == text)
            {
                pending.id = id.to_string();
                changed = true;
            } else {
                self.tasks.push(TrackedTask {
                    id: id.to_string(),
                    task: AgentTask {
                        text: text.to_string(),
                        status: AgentTaskStatus::Pending,
                    },
                });
                changed = true;
            }
        }
        let Some(result) = value.get("toolUseResult") else {
            return changed;
        };
        let Some(id) = result.get("taskId").and_then(Value::as_str) else {
            return changed;
        };
        let Some(status) = result
            .get("statusChange")
            .and_then(|change| change.get("to"))
            .and_then(Value::as_str)
            .and_then(parse_task_status)
        else {
            return changed;
        };
        if let Some(existing) = self.tasks.iter_mut().find(|task| task.id == id) {
            if existing.task.status != status {
                existing.task.status = status;
                changed = true;
            }
        }
        changed
    }

    fn apply_task_tool_calls(&mut self, value: &Value) -> bool {
        let mut observed = false;
        let Some(content) = value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            return false;
        };
        for item in content {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(input) = item.get("input") else {
                continue;
            };
            match name {
                "TaskCreate" => {
                    let Some(tool_use_id) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(text) = input
                        .get("subject")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                    else {
                        continue;
                    };
                    observed = true;
                    let id = format!("{PENDING_TASK_ID_PREFIX}{tool_use_id}");
                    if self.tasks.iter().any(|task| task.id == id) {
                        continue;
                    }
                    self.tasks.push(TrackedTask {
                        id,
                        task: AgentTask {
                            text: text.to_string(),
                            status: AgentTaskStatus::Pending,
                        },
                    });
                }
                "TaskUpdate" => {
                    let Some(task_id) = input.get("taskId").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(task) = self.tasks.iter_mut().find(|task| task.id == task_id) else {
                        continue;
                    };
                    observed = true;
                    if let Some(text) = input
                        .get("subject")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                    {
                        if task.task.text != text {
                            task.task.text = text.to_string();
                        }
                    }
                    if let Some(status) = input
                        .get("status")
                        .and_then(Value::as_str)
                        .and_then(parse_task_status)
                    {
                        if task.task.status != status {
                            task.task.status = status;
                        }
                    }
                }
                _ => {}
            }
        }
        observed
    }

    fn apply_todo_write(&mut self, value: &Value) -> bool {
        let Some(content) = value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            return false;
        };
        for item in content.iter().rev() {
            if item.get("name").and_then(Value::as_str) != Some("TodoWrite") {
                continue;
            }
            let Some(todos) = item
                .get("input")
                .and_then(|input| input.get("todos"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            self.tasks = todos
                .iter()
                .enumerate()
                .filter_map(|(index, todo)| {
                    let text = todo
                        .get("content")
                        .or_else(|| todo.get("text"))?
                        .as_str()?
                        .trim();
                    let status = todo
                        .get("status")
                        .and_then(Value::as_str)
                        .and_then(parse_task_status)?;
                    (!text.is_empty()).then(|| TrackedTask {
                        id: format!("todo-{index}"),
                        task: AgentTask {
                            text: text.to_string(),
                            status,
                        },
                    })
                })
                .collect();
            return true;
        }
        false
    }

    fn observe_row_links(&mut self, value: &Value, observed_at: Option<SystemTime>) {
        let Some(observed_at) = observed_at else {
            return;
        };
        let mut urls = Vec::new();
        collect_value_urls(value, &mut urls);
        for url in urls {
            if let Some(existing) = self.links.iter_mut().find(|link| link.url == url) {
                existing.last_seen = existing.last_seen.max(observed_at);
            } else {
                self.links.push(TranscriptLink {
                    url,
                    first_seen: observed_at,
                    last_seen: observed_at,
                });
            }
        }
        self.links.sort_by_key(|link| link.last_seen);
        if self.links.len() > crate::agent_state::MAX_LINKS {
            self.links
                .drain(..self.links.len() - crate::agent_state::MAX_LINKS);
        }
    }

    fn apply_event(&mut self, event: TranscriptEvent) {
        match event {
            TranscriptEvent::Started(observation) => {
                let id = observation.id.clone();
                let known_observation = self.observations.iter().any(|existing| existing.id == id);
                if (!known_observation && self.observations.len() >= MAX_ACTIVE_IDS)
                    || (self.active_ids.len() >= MAX_ACTIVE_IDS && !self.active_ids.contains(&id))
                {
                    self.trustworthy = false;
                } else {
                    self.active_ids.insert(id.clone());
                    if let Some(existing) = self
                        .observations
                        .iter_mut()
                        .find(|existing| existing.id == id)
                    {
                        existing.state = AgentState::Working;
                        existing.observed_at = observation.observed_at.or(existing.observed_at);
                        if existing.name == existing.id {
                            existing.name = observation.name;
                        }
                        existing.parent_id = observation.parent_id.or(existing.parent_id.take());
                    } else {
                        self.observations.push(observation);
                    }
                }
            }
            TranscriptEvent::StateChanged {
                id,
                state,
                observed_at,
            } => {
                if state == AgentState::Idle {
                    self.active_ids.remove(&id);
                }
                if let Some(existing) = self
                    .observations
                    .iter_mut()
                    .find(|existing| existing.id == id)
                {
                    existing.state = state;
                    existing.observed_at = observed_at.or(existing.observed_at);
                }
            }
        }
    }

    /// Consume bytes from the current file offset. The caller owns seeking and
    /// file-identity checks; this parser never retains more than one capped row.
    pub(crate) fn ingest(&mut self, bytes: &[u8], reached_eof: bool) -> ParseStats {
        let mut stats = ParseStats {
            bytes_read: bytes.len() as u64,
            ..ParseStats::default()
        };

        for &byte in bytes {
            self.scan_offset = self.scan_offset.saturating_add(1);
            if byte == b'\n' {
                if self.line_overflowed {
                    self.trustworthy = false;
                    stats.oversized_rows = stats.oversized_rows.saturating_add(1);
                } else {
                    let line = self
                        .line_buffer
                        .strip_suffix(b"\r")
                        .unwrap_or(&self.line_buffer);
                    if !line.is_empty() {
                        match parse_transcript_row(line) {
                            Ok((value, event, observed_at)) => {
                                if let Some(event) = event {
                                    self.apply_event(event);
                                }
                                if let Some(observed_at) = observed_at {
                                    self.last_row_at =
                                        Some(self.last_row_at.map_or(observed_at, |current| {
                                            current.max(observed_at)
                                        }));
                                }
                                let task_tool_call_observed = self.apply_todo_write(&value)
                                    | self.apply_task_tool_calls(&value);
                                self.apply_task_result(&value);
                                if task_tool_call_observed {
                                    if let Some(observed_at) = observed_at {
                                        self.last_tasks_at = Some(
                                            self.last_tasks_at.map_or(observed_at, |current| {
                                                current.max(observed_at)
                                            }),
                                        );
                                    }
                                }
                                self.observe_row_links(&value, observed_at);
                                stats.lines_parsed = stats.lines_parsed.saturating_add(1);
                            }
                            Err(()) => {
                                self.trustworthy = false;
                                stats.malformed_rows = stats.malformed_rows.saturating_add(1);
                            }
                        }
                    }
                }
                self.line_buffer.clear();
                self.line_overflowed = false;
                self.committed_offset = self.scan_offset;
            } else if self.line_overflowed {
                continue;
            } else if self.line_buffer.len() < MAX_BUFFERED_JSON_ROW {
                self.line_buffer.push(byte);
            } else {
                self.line_buffer.clear();
                self.line_overflowed = true;
            }
        }

        if reached_eof {
            if self.line_buffer.is_empty() && !self.line_overflowed {
                self.caught_up_once = true;
            } else {
                stats.partial_rows = 1;
            }
        }
        stats
    }
}

impl TranscriptTracker {
    pub(crate) fn new(session_id: String, path: PathBuf, target_generation: u64) -> Self {
        Self {
            session_id,
            path,
            target_generation,
            cursor: TranscriptCursor::new(),
            file_identity: None,
            last_len: None,
            last_modified: None,
            transcript_paths_initialized: false,
        }
    }

    pub(crate) fn count(&self) -> Option<u32> {
        self.cursor.count()
    }

    pub(crate) fn observations(&self) -> Option<Vec<ClaudeSubagentObservation>> {
        Some(self.cursor.observations()?.to_vec())
    }

    pub(crate) fn tasks(&self) -> Option<Vec<AgentTask>> {
        self.cursor.tasks()
    }

    pub(crate) fn links(&self) -> Option<&[TranscriptLink]> {
        (self.cursor.caught_up_once && self.cursor.trustworthy).then_some(&self.cursor.links)
    }

    pub(crate) fn last_row_at(&self) -> Option<SystemTime> {
        (self.cursor.caught_up_once && self.cursor.trustworthy)
            .then_some(self.cursor.last_row_at)
            .flatten()
    }

    pub(crate) fn last_tasks_at(&self) -> Option<SystemTime> {
        (self.cursor.caught_up_once && self.cursor.trustworthy)
            .then_some(self.cursor.last_tasks_at)
            .flatten()
    }

    fn refresh_transcript_paths_and_activity(
        &mut self,
        freshness_window: std::time::Duration,
        deadline: Instant,
    ) -> crate::terminal::state::SubagentTranscriptActivity {
        let wall_clock_now = SystemTime::now();
        let mut saw_known_transcript = false;
        let mut saw_fresh_transcript = false;
        let mut completed = true;
        for observation in &mut self.cursor.observations {
            if Instant::now() >= deadline {
                completed = false;
                break;
            }
            let active = self.cursor.active_ids.contains(&observation.id);
            let metadata = if let Some(path) = observation.transcript_path.as_ref() {
                active.then(|| regular_file_metadata(path)).flatten()
            } else if !self.transcript_paths_initialized || observation.state != AgentState::Idle {
                known_subagent_transcript(&self.path, &observation.id).map(|(path, metadata)| {
                    observation.transcript_path = Some(path);
                    metadata
                })
            } else {
                None
            };
            if !active {
                continue;
            }
            let Some(modified_at) = metadata.and_then(|metadata| metadata.modified().ok()) else {
                continue;
            };
            saw_known_transcript = true;
            if match wall_clock_now.duration_since(modified_at) {
                Ok(age) => age <= freshness_window,
                Err(_) => true,
            } {
                saw_fresh_transcript = true;
            }
        }
        self.transcript_paths_initialized = completed;
        if saw_fresh_transcript {
            crate::terminal::state::SubagentTranscriptActivity::Fresh
        } else if saw_known_transcript && completed {
            crate::terminal::state::SubagentTranscriptActivity::Stale
        } else {
            crate::terminal::state::SubagentTranscriptActivity::Unknown
        }
    }

    fn retained_bytes(&self) -> usize {
        self.cursor.line_buffer.len()
    }

    fn invalidate_partial_row(&mut self) {
        self.cursor.trustworthy = false;
        self.cursor.line_buffer.clear();
        self.cursor.line_overflowed = true;
    }

    pub(crate) fn scan(&mut self, byte_budget: usize, deadline: Instant) -> ScanStats {
        let mut stats = ScanStats::default();
        let Ok((mut file, metadata, identity)) = open_regular_transcript(&self.path) else {
            stats.open_failures = 1;
            return stats;
        };
        stats.files_opened = 1;

        let replaced = self
            .file_identity
            .as_ref()
            .is_some_and(|previous| previous != &identity)
            || metadata.len() < self.cursor.scan_offset;
        if replaced {
            self.cursor.reset();
            self.transcript_paths_initialized = false;
            stats.identity_resets = 1;
        }
        self.file_identity = Some(identity);

        let modified = metadata.modified().ok();
        if self.last_len == Some(metadata.len())
            && self.last_modified == modified
            && self.cursor.scan_offset == metadata.len()
            && self.cursor.line_buffer.is_empty()
            && !self.cursor.line_overflowed
        {
            stats.metadata_hits = 1;
            return stats;
        }

        if file.seek(SeekFrom::Start(self.cursor.scan_offset)).is_err() {
            stats.open_failures = 1;
            return stats;
        }
        let mut remaining = byte_budget;
        let mut buffer = [0_u8; 8192];
        let mut reached_eof = false;
        while remaining > 0 {
            if Instant::now() >= deadline {
                stats.deadline_exhausted = true;
                break;
            }
            let limit = remaining.min(buffer.len());
            match file.read(&mut buffer[..limit]) {
                Ok(0) => {
                    reached_eof = true;
                    break;
                }
                Ok(read) => {
                    remaining -= read;
                    let parsed = self.cursor.ingest(&buffer[..read], false);
                    merge_parse_stats(&mut stats.parse, parsed);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    stats.open_failures = 1;
                    break;
                }
            }
        }
        if reached_eof {
            merge_parse_stats(&mut stats.parse, self.cursor.ingest(&[], true));
        }
        self.last_len = Some(metadata.len().max(self.cursor.scan_offset));
        self.last_modified = modified;
        stats
    }
}

pub(crate) fn refresh_trackers(
    work: Vec<RefreshWorkItem>,
    deadline: Instant,
    freshness_window: std::time::Duration,
) -> (Vec<RefreshObservation>, BatchStats) {
    let started = Instant::now();
    let mut budget = BATCH_READ_BUDGET;
    let mut observations = Vec::new();
    let mut batch = BatchStats {
        targets_total: work.len() as u64,
        ..BatchStats::default()
    };
    let mut total_carry = 0usize;

    for mut item in work {
        if budget == 0 || Instant::now() >= deadline {
            batch.deadline_exhausted = Instant::now() >= deadline;
            break;
        }
        let target_started = Instant::now();
        let stats = item
            .tracker
            .scan(budget.min(PER_TARGET_READ_QUANTUM), deadline);
        let target_us = target_started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        batch.max_target_us = batch.max_target_us.max(target_us);
        let bytes_read = usize::try_from(stats.parse.bytes_read).unwrap_or(usize::MAX);
        budget = budget.saturating_sub(bytes_read);
        batch.targets_attempted = batch.targets_attempted.saturating_add(1);
        batch.files_opened = batch.files_opened.saturating_add(stats.files_opened);
        batch.metadata_hits = batch.metadata_hits.saturating_add(stats.metadata_hits);
        batch.bytes_read = batch.bytes_read.saturating_add(stats.parse.bytes_read);
        batch.lines_parsed = batch.lines_parsed.saturating_add(stats.parse.lines_parsed);
        batch.partial_rows = batch.partial_rows.saturating_add(stats.parse.partial_rows);
        batch.oversized_rows = batch
            .oversized_rows
            .saturating_add(stats.parse.oversized_rows);
        batch.malformed_rows = batch
            .malformed_rows
            .saturating_add(stats.parse.malformed_rows);
        batch.identity_resets = batch.identity_resets.saturating_add(stats.identity_resets);
        batch.open_failures = batch.open_failures.saturating_add(stats.open_failures);
        batch.deadline_exhausted |= stats.deadline_exhausted;
        batch.targets_caught_up = batch
            .targets_caught_up
            .saturating_add(u64::from(item.tracker.cursor.caught_up_once));

        let retained = item.tracker.retained_bytes();
        if total_carry.saturating_add(retained) > MAX_TOTAL_CARRY {
            item.tracker.invalidate_partial_row();
        } else {
            total_carry = total_carry.saturating_add(retained);
        }
        let transcript_activity = item
            .tracker
            .refresh_transcript_paths_and_activity(freshness_window, deadline);
        let count = item.tracker.count();
        let subagents = item.tracker.observations();
        observations.push(RefreshObservation {
            target: item.target,
            tracker: item.tracker,
            count,
            observations: subagents,
            transcript_activity,
            stats,
        });
    }
    batch.elapsed_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    (observations, batch)
}

impl crate::app::App {
    pub(crate) fn claude_subagent_refresh_deadline(&self) -> Option<Instant> {
        if let Some(refresh) = self.claude_subagent_refresh_in_flight.as_ref() {
            return Some(refresh.deadline);
        }
        (!self.state.terminals.is_empty()).then_some(self.next_claude_subagent_refresh)
    }

    pub(crate) fn start_claude_subagent_refresh_if_due(&mut self, now: Instant) {
        if self
            .claude_subagent_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.claude_subagent_refresh_in_flight = None;
        }
        if self.claude_subagent_refresh_in_flight.is_some()
            || now < self.next_claude_subagent_refresh
        {
            return;
        }
        self.next_claude_subagent_refresh = now + POLL_INTERVAL;

        let mut raw_targets = self
            .state
            .terminals
            .iter()
            .filter_map(|(terminal_id, terminal)| {
                Some((
                    terminal_id.clone(),
                    "herdr:claude".to_string(),
                    terminal.claude_transcript_session_id.clone()?,
                    terminal.claude_transcript_path.clone()?,
                    terminal.agent_turn_generation(),
                ))
            })
            .collect::<Vec<_>>();
        raw_targets.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        if raw_targets.is_empty() {
            self.claude_subagent_trackers.clear();
            return;
        }

        let current_ids = raw_targets
            .iter()
            .map(|target| target.0.clone())
            .collect::<HashSet<_>>();
        self.claude_subagent_trackers
            .retain(|terminal_id, _| current_ids.contains(terminal_id));

        let mut work = Vec::with_capacity(raw_targets.len());
        for (terminal_id, source, session_id, path, turn_generation) in raw_targets {
            let replace = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .is_none_or(|tracker| tracker.session_id != session_id || tracker.path != path);
            if replace {
                self.next_claude_subagent_target_generation =
                    self.next_claude_subagent_target_generation.wrapping_add(1);
                self.claude_subagent_trackers.insert(
                    terminal_id.clone(),
                    TranscriptTracker::new(
                        session_id.clone(),
                        path.clone(),
                        self.next_claude_subagent_target_generation,
                    ),
                );
            }
            let tracker = self.claude_subagent_trackers[&terminal_id].clone();
            work.push(RefreshWorkItem {
                target: TargetIdentity {
                    terminal_id,
                    source,
                    session_id,
                    path,
                    target_generation: tracker.target_generation,
                    turn_generation,
                },
                tracker,
            });
        }

        self.claude_subagent_refresh_rotation =
            rotate_refresh_work(&mut work, self.claude_subagent_refresh_rotation);

        self.last_claude_subagent_refresh_generation =
            self.last_claude_subagent_refresh_generation.wrapping_add(1);
        let generation = self.last_claude_subagent_refresh_generation;
        let deadline = now + WORKER_TIMEOUT;
        self.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation,
            deadline,
        });
        let freshness_window = self.state.agent_subagent_stale_after;
        let event_tx = self.event_tx.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-claude-subagents".into())
            .spawn(move || {
                let (observations, stats) = refresh_trackers(work, deadline, freshness_window);
                let _ = event_tx.blocking_send(crate::events::AppEvent::ClaudeSubagentsRefreshed {
                    generation,
                    observations,
                    stats,
                });
            });
    }

    pub(crate) fn handle_claude_subagents_refreshed(
        &mut self,
        generation: u64,
        observations: Vec<RefreshObservation>,
        stats: BatchStats,
    ) -> bool {
        if generation != self.last_claude_subagent_refresh_generation
            || generation <= self.last_applied_claude_subagent_refresh_generation
        {
            return false;
        }
        if self
            .claude_subagent_refresh_in_flight
            .as_ref()
            .is_none_or(|refresh| refresh.generation != generation)
        {
            return false;
        }
        if self
            .claude_subagent_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| Instant::now() >= refresh.deadline)
        {
            self.claude_subagent_refresh_in_flight = None;
            self.next_claude_subagent_refresh = Instant::now() + POLL_INTERVAL;
            return false;
        }
        self.claude_subagent_refresh_in_flight = None;
        self.last_applied_claude_subagent_refresh_generation = generation;

        let mut updated_terminal_ids = Vec::new();
        let mut transcript_activities = HashMap::new();
        for observation in observations {
            let current_matches = self
                .state
                .terminals
                .get(&observation.target.terminal_id)
                .is_some_and(|terminal| {
                    observation.target.source == "herdr:claude"
                        && terminal.claude_transcript_session_id.as_deref()
                            == Some(observation.target.session_id.as_str())
                        && terminal.claude_transcript_path.as_ref()
                            == Some(&observation.target.path)
                        && terminal.agent_turn_generation() == observation.target.turn_generation
                        && self
                            .claude_subagent_trackers
                            .get(&observation.target.terminal_id)
                            .is_some_and(|tracker| {
                                tracker.target_generation == observation.target.target_generation
                            })
                });
            if !current_matches {
                continue;
            }
            self.claude_subagent_trackers
                .insert(observation.target.terminal_id.clone(), observation.tracker);
            transcript_activities.insert(
                observation.target.terminal_id.clone(),
                observation.transcript_activity,
            );
            let _ = (observation.count, observation.observations);
            updated_terminal_ids.push(observation.target.terminal_id);
            let _ = observation.stats;
        }

        // A partial JSONL row is the only variable-size cursor state. Enforce
        // the process-wide cap after merging worker results, not only per batch.
        let mut tracker_ids = self
            .claude_subagent_trackers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        tracker_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let mut total_carry = 0usize;
        for terminal_id in tracker_ids {
            let Some(tracker) = self.claude_subagent_trackers.get_mut(&terminal_id) else {
                continue;
            };
            let retained = tracker.retained_bytes();
            if total_carry.saturating_add(retained) > MAX_TOTAL_CARRY {
                tracker.invalidate_partial_row();
                updated_terminal_ids.push(terminal_id);
            } else {
                total_carry = total_carry.saturating_add(retained);
            }
        }

        updated_terminal_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        updated_terminal_ids.dedup();
        let mut counts_changed = 0_u64;
        let mut observations_changed = 0_u64;
        let mut transcript_activities_changed = 0_u64;
        let mut changed_panes = Vec::new();
        let mut pane_updates = Vec::new();
        let previous_toast = self.state.toast.clone();
        for terminal_id in updated_terminal_ids {
            let count = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .and_then(TranscriptTracker::count);
            let observations = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .and_then(TranscriptTracker::observations);
            let transcript_tasks = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .and_then(TranscriptTracker::tasks);
            let transcript_last_row_at = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .and_then(TranscriptTracker::last_row_at);
            let transcript_last_tasks_at = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .and_then(TranscriptTracker::last_tasks_at);
            let transcript_links = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .and_then(TranscriptTracker::links)
                .map(|links| {
                    links
                        .iter()
                        .map(|link| ObservedAgentLink {
                            url: link.url.clone(),
                            first_seen: link.first_seen,
                            last_seen: link.last_seen,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let transcript_session_id = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .map(|tracker| tracker.session_id.clone());
            let transcript_activity = transcript_activities.remove(&terminal_id);
            let location =
                self.state
                    .workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(ws_idx, workspace)| {
                        workspace.tabs.iter().find_map(|tab| {
                            tab.panes.iter().find_map(|(pane_id, pane)| {
                                (pane.attached_terminal_id == terminal_id)
                                    .then_some((ws_idx, *pane_id, pane.seen))
                            })
                        })
                    });
            let mut count_changed = false;
            let mut observation_changed = false;
            let mut transcript_activity_changed = false;
            let state_update = if let Some((_, pane_id, seen)) = location {
                let now = Instant::now();
                self.state
                    .update_terminal_state_at(pane_id, now, |terminal| {
                        let (count_changed_now, mutation) = if transcript_activity
                            != Some(crate::terminal::state::SubagentTranscriptActivity::Fresh)
                            && terminal.active_subagents == count
                        {
                            (false, None)
                        } else {
                            terminal.set_active_subagents_with_projection_at(count, seen, now)
                        };
                        count_changed = count_changed_now;
                        observation_changed =
                            terminal.set_claude_subagent_observations(observations.clone());
                        if let Some(activity) = transcript_activity {
                            transcript_activity_changed =
                                terminal.set_claude_subagent_transcript_activity_at(activity, now);
                        }
                        mutation
                    })
            } else {
                if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                    if transcript_activity
                        == Some(crate::terminal::state::SubagentTranscriptActivity::Fresh)
                        || terminal.active_subagents != count
                    {
                        count_changed = terminal.set_active_subagents(count);
                    }
                    observation_changed =
                        terminal.set_claude_subagent_observations(observations.clone());
                    if let Some(activity) = transcript_activity {
                        transcript_activity_changed = terminal
                            .set_claude_subagent_transcript_activity_at(activity, Instant::now());
                    }
                }
                None
            };
            if count_changed {
                counts_changed = counts_changed.saturating_add(1);
            }
            if observation_changed {
                observations_changed = observations_changed.saturating_add(1);
            }
            if transcript_activity_changed {
                transcript_activities_changed = transcript_activities_changed.saturating_add(1);
            }
            let transcript_changed = location.is_some_and(|(_, pane_id, _)| {
                let subagents = observations
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|observation| crate::agent_state::AgentSubagent {
                        name: observation.name,
                        status: match observation.state {
                            AgentState::Working => crate::api::schema::AgentStatus::Working,
                            AgentState::Blocked => crate::api::schema::AgentStatus::Blocked,
                            AgentState::Idle => crate::api::schema::AgentStatus::Done,
                            AgentState::Unknown => crate::api::schema::AgentStatus::Unknown,
                        },
                        last_active_at: observation
                            .observed_at
                            .and_then(crate::agent_state::format_rfc3339),
                        pane_id: None,
                        source: crate::agent_state::AgentSubagentSource::Observed,
                    })
                    .collect();
                let session_changed = transcript_session_id.as_deref().is_some_and(|session_id| {
                    self.state
                        .agent_states
                        .observe_transcript_session(pane_id, session_id)
                });
                let observation_changed = self.state.agent_states.observe_transcript(
                    pane_id,
                    transcript_last_row_at,
                    transcript_tasks.clone(),
                    transcript_last_tasks_at,
                    subagents,
                    transcript_links.clone(),
                );
                session_changed || observation_changed
            });
            if !count_changed
                && !observation_changed
                && !transcript_activity_changed
                && !transcript_changed
            {
                continue;
            }
            if let Some((ws_idx, pane_id, _)) = location {
                changed_panes.push((ws_idx, pane_id));
            }
            if let Some(update) = state_update {
                self.refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
                self.emit_pane_state_update(&update);
                pane_updates.push(update);
            }
        }
        for (ws_idx, pane_id) in changed_panes {
            self.emit_pane_updated(ws_idx, pane_id);
        }
        if !pane_updates.is_empty() {
            self.emit_terminal_or_system_agent_notifications(&pane_updates);
            self.sync_toast_deadline(previous_toast);
        }
        let active_subagents_total = self
            .claude_subagent_trackers
            .values()
            .filter_map(TranscriptTracker::count)
            .fold(0_u64, |total, count| total.saturating_add(u64::from(count)));

        tracing::debug!(
            generation,
            targets_total = stats.targets_total,
            targets_attempted = stats.targets_attempted,
            files_opened = stats.files_opened,
            metadata_hits = stats.metadata_hits,
            bytes_read = stats.bytes_read,
            lines_parsed = stats.lines_parsed,
            partial_rows = stats.partial_rows,
            oversized_rows = stats.oversized_rows,
            malformed_rows = stats.malformed_rows,
            identity_resets = stats.identity_resets,
            open_failures = stats.open_failures,
            targets_caught_up = stats.targets_caught_up,
            deadline_exhausted = stats.deadline_exhausted,
            elapsed_us = stats.elapsed_us,
            max_target_us = stats.max_target_us,
            counts_changed,
            observations_changed,
            transcript_activities_changed,
            active_subagents_total,
            "refreshed Claude subagent transcripts"
        );
        counts_changed > 0 || observations_changed > 0 || transcript_activities_changed > 0
    }
}

fn rotate_refresh_work(work: &mut [RefreshWorkItem], rotation: usize) -> usize {
    if work.len() <= 1 {
        return 0;
    }
    let start = rotation % work.len();
    work.rotate_left(start);
    let full_quanta = (BATCH_READ_BUDGET / PER_TARGET_READ_QUANTUM).max(1);
    (start + full_quanta.min(work.len())) % work.len()
}

fn merge_parse_stats(total: &mut ParseStats, next: ParseStats) {
    total.bytes_read = total.bytes_read.saturating_add(next.bytes_read);
    total.lines_parsed = total.lines_parsed.saturating_add(next.lines_parsed);
    total.partial_rows = total.partial_rows.saturating_add(next.partial_rows);
    total.oversized_rows = total.oversized_rows.saturating_add(next.oversized_rows);
    total.malformed_rows = total.malformed_rows.saturating_add(next.malformed_rows);
}

fn open_regular_transcript(path: &Path) -> std::io::Result<(File, Metadata, FileIdentity)> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Claude transcript must not be a symlink",
        ));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Claude transcript must be a regular file",
        ));
    }
    let identity = file_identity(&file, &metadata)?;
    Ok((file, metadata, identity))
}

#[cfg(unix)]
fn file_identity(_file: &File, metadata: &Metadata) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(file: &File, _metadata: &Metadata) -> std::io::Result<FileIdentity> {
    crate::platform::windows_file_identity(file).map(FileIdentity::Windows)
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File, metadata: &Metadata) -> std::io::Result<FileIdentity> {
    Ok(FileIdentity::Portable {
        created: metadata.created().ok(),
    })
}

pub(crate) fn validated_transcript_path(
    source: &str,
    agent_label: &str,
    session_ref: Option<&AgentSessionRef>,
    raw_path: Option<&str>,
) -> Option<PathBuf> {
    if source != "herdr:claude" || agent_label != "claude" {
        return None;
    }
    let session_ref = session_ref?;
    if session_ref.kind != AgentSessionRefKind::Id {
        return None;
    }
    let raw_path = raw_path?;
    if raw_path.is_empty()
        || raw_path.len() > MAX_TRANSCRIPT_PATH_BYTES
        || raw_path.chars().any(char::is_control)
    {
        return None;
    }

    let path = Path::new(raw_path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    let expected_file = format!("{}.jsonl", session_ref.value);
    let mut components = path.components().rev();
    let file = components.next()?.as_os_str();
    let project_slug = components.next()?;
    let projects = components.next()?.as_os_str();
    if file != expected_file.as_str()
        || projects != "projects"
        || !matches!(project_slug, Component::Normal(value) if !value.is_empty())
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn parse_transcript_row(
    line: &[u8],
) -> Result<(Value, Option<TranscriptEvent>, Option<SystemTime>), ()> {
    let value: Value = serde_json::from_slice(line).map_err(|_| ())?;
    let observed_at = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(crate::agent_state::parse_rfc3339);
    if let Some(result) = value.get("toolUseResult") {
        if result.get("status").and_then(Value::as_str) == Some("async_launched")
            && result.get("isAsync").and_then(Value::as_bool) == Some(true)
        {
            let event = result
                .get("agentId")
                .and_then(Value::as_str)
                .and_then(valid_subagent_id)
                .map(|id| {
                    let name = result
                        .get("description")
                        .and_then(Value::as_str)
                        .and_then(valid_subagent_name)
                        .unwrap_or_else(|| id.clone());
                    let parent_id = result
                        .get("parentAgentId")
                        .and_then(Value::as_str)
                        .and_then(valid_subagent_id);
                    TranscriptEvent::Started(ClaudeSubagentObservation {
                        id,
                        parent_id,
                        name,
                        state: AgentState::Working,
                        observed_at,
                        transcript_path: None,
                    })
                });
            return Ok((value, event, observed_at));
        }
        if result.get("success").and_then(Value::as_bool) == Some(true) {
            let event = result
                .get("resumedAgentId")
                .and_then(Value::as_str)
                .and_then(valid_subagent_id)
                .map(|id| {
                    TranscriptEvent::Started(ClaudeSubagentObservation {
                        name: id.clone(),
                        id,
                        parent_id: None,
                        state: AgentState::Working,
                        observed_at,
                        transcript_path: None,
                    })
                });
            return Ok((value, event, observed_at));
        }
    }

    if value.get("type").and_then(Value::as_str) != Some("queue-operation")
        || value.get("operation").and_then(Value::as_str) != Some("enqueue")
    {
        return Ok((value, None, observed_at));
    }
    let Some(content) = value.get("content").and_then(Value::as_str) else {
        return Ok((value, None, observed_at));
    };
    if !content.starts_with("<task-notification>") {
        return Ok((value, None, observed_at));
    }
    let Some(state) = tag_value(content, "status").and_then(notification_state) else {
        return Ok((value, None, observed_at));
    };
    let event = tag_value(content, "task-id")
        .and_then(valid_subagent_id)
        .map(|id| TranscriptEvent::StateChanged {
            id,
            state,
            observed_at,
        });
    Ok((value, event, observed_at))
}

fn parse_task_status(value: &str) -> Option<AgentTaskStatus> {
    match value {
        "pending" => Some(AgentTaskStatus::Pending),
        "in_progress" => Some(AgentTaskStatus::InProgress),
        "completed" => Some(AgentTaskStatus::Completed),
        _ => None,
    }
}

fn collect_value_urls(value: &Value, urls: &mut Vec<String>) {
    match value {
        Value::String(text) => urls.extend(crate::agent_state::extract_urls(text)),
        Value::Array(values) => {
            for value in values {
                collect_value_urls(value, urls);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_value_urls(value, urls);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn notification_state(value: &str) -> Option<AgentState> {
    match value {
        "completed" => Some(AgentState::Idle),
        "blocked" => Some(AgentState::Blocked),
        "running" => Some(AgentState::Working),
        _ => None,
    }
}

fn tag_value<'a>(content: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)? + start;
    Some(&content[start..end])
}

fn valid_subagent_id(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_SUBAGENT_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then(|| value.to_string())
}

fn valid_subagent_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.chars().take(120).collect())
}

fn known_subagent_transcript(parent: &Path, id: &str) -> Option<(PathBuf, Metadata)> {
    let path = parent
        .with_extension("")
        .join("subagents")
        .join(format!("agent-{id}.jsonl"));
    let metadata = regular_file_metadata(&path)?;
    Some((path, metadata))
}

fn regular_file_metadata(path: &Path) -> Option<Metadata> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    (!metadata.file_type().is_symlink() && metadata.is_file()).then_some(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SESSION_ID: &str = "6be5e8e1-cce2-4c1e-b04c-62e3e38eb75a";
    const AGENT_A: &str = "a5f724db00c0e2f2b";
    const AGENT_B: &str = "a2e17f250c84a35b2";

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "herdr-claude-subagents-{}-{label}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn transcript(&self) -> PathBuf {
            let projects = self.0.join("projects").join("-tmp-repro");
            std::fs::create_dir_all(&projects).unwrap();
            projects.join(format!("{SESSION_ID}.jsonl"))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn absolute_fixture_path(relative: impl AsRef<Path>) -> String {
        std::env::temp_dir().join(relative).display().to_string()
    }

    fn line(value: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn launch(agent_id: &str) -> Vec<u8> {
        line(serde_json::json!({
            "type": "user",
            "toolUseResult": {
                "isAsync": true,
                "status": "async_launched",
                "agentId": agent_id,
                "description": "bounded fixture"
            }
        }))
    }

    fn completion(agent_id: &str) -> Vec<u8> {
        line(serde_json::json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "sessionId": SESSION_ID,
            "content": format!(
                "<task-notification>\n<task-id>{agent_id}</task-id>\n<status>completed</status>\n</task-notification>"
            )
        }))
    }

    fn launch_named(agent_id: &str, name: &str, timestamp: &str) -> Vec<u8> {
        line(serde_json::json!({
            "type": "user",
            "timestamp": timestamp,
            "toolUseResult": {
                "isAsync": true,
                "status": "async_launched",
                "agentId": agent_id,
                "description": name
            }
        }))
    }

    fn notification(agent_id: &str, status: &str, timestamp: &str) -> Vec<u8> {
        line(serde_json::json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "timestamp": timestamp,
            "content": format!(
                "<task-notification><task-id>{agent_id}</task-id><status>{status}</status></task-notification>"
            )
        }))
    }

    #[test]
    fn claude_async_launch_and_completion_fixture_uses_one_proven_identity() {
        let mut cursor = TranscriptCursor::new();
        let mut fixture = launch(AGENT_A);
        fixture.extend(launch(AGENT_B));
        cursor.ingest(&fixture, true);
        assert_eq!(cursor.count(), Some(2));

        cursor.ingest(&completion(AGENT_A), true);
        assert_eq!(cursor.count(), Some(1));
        assert!(cursor.active_ids.contains(AGENT_B));
    }

    #[test]
    fn transcript_observations_keep_name_status_and_latest_age_source() {
        let mut cursor = TranscriptCursor::new();
        let mut fixture = launch_named(AGENT_A, "review parser", "2026-09-06T10:00:00Z");
        fixture.extend(notification(AGENT_A, "blocked", "2026-09-06T10:02:00Z"));
        fixture.extend(launch_named(AGENT_B, "run tests", "2026-09-06T10:03:00Z"));
        fixture.extend(notification(AGENT_B, "completed", "2026-09-06T10:04:00Z"));

        cursor.ingest(&fixture, true);

        let observations = cursor.observations().expect("authoritative replay");
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].name, "review parser");
        assert_eq!(observations[0].state, AgentState::Blocked);
        assert_eq!(observations[1].name, "run tests");
        assert_eq!(observations[1].state, AgentState::Idle);
        assert_eq!(cursor.count(), Some(1), "blocked work remains active");
        assert_eq!(
            observations[1].observed_at,
            crate::agent_state::parse_rfc3339("2026-09-06T10:04:00Z")
        );
    }

    #[test]
    fn real_stripped_transcript_fixture_projects_latest_tasks_links_and_row_time() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(
            include_bytes!("../../tests/fixtures/claude-transcripts/task-state.jsonl"),
            true,
        );

        assert_eq!(
            cursor.tasks(),
            Some(vec![
                AgentTask {
                    text: "inspect runtime boundary".into(),
                    status: AgentTaskStatus::InProgress,
                },
                AgentTask {
                    text: "add state tests".into(),
                    status: AgentTaskStatus::Completed,
                },
            ])
        );
        assert_eq!(cursor.links.len(), 1);
        assert_eq!(cursor.links[0].url, "https://example.test/review/42");
        assert_eq!(
            cursor.last_row_at,
            crate::agent_state::parse_rfc3339("2026-08-14T06:18:47.437Z")
        );
    }

    #[test]
    fn transcript_task_timestamp_ignores_later_unrelated_rows() {
        let task_at = "2026-08-14T06:18:40.100Z";
        let unrelated_at = "2026-08-14T06:18:41.100Z";
        let mut bytes = line(serde_json::json!({
            "type": "assistant",
            "timestamp": task_at,
            "message": {"content": [{
                "type": "tool_use",
                "name": "TodoWrite",
                "input": {"todos": [{
                    "content": "keep report precedence honest",
                    "status": "in_progress"
                }]}
            }]}
        }));
        bytes.extend(line(serde_json::json!({
            "type": "assistant",
            "timestamp": unrelated_at,
            "message": {"content": [{"type": "text", "text": "still working"}]}
        })));
        let mut cursor = TranscriptCursor::new();

        cursor.ingest(&bytes, true);

        assert_eq!(
            cursor.last_tasks_at,
            crate::agent_state::parse_rfc3339(task_at)
        );
        assert_eq!(
            cursor.last_row_at,
            crate::agent_state::parse_rfc3339(unrelated_at)
        );
    }

    #[test]
    fn transcript_task_timestamp_tracks_latest_unchanged_task_tool_call() {
        let created_at = "2026-08-14T06:18:40.100Z";
        let repeated_at = "2026-08-14T06:18:43.100Z";
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(
            &line(serde_json::json!({
                "type": "assistant",
                "timestamp": created_at,
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TaskCreate",
                    "id": "toolu_task_create",
                    "input": {"subject": "keep precedence current"}
                }]}
            })),
            false,
        );
        cursor.ingest(
            &line(serde_json::json!({
                "type": "assistant",
                "timestamp": repeated_at,
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TaskUpdate",
                    "input": {
                        "taskId": "tool-use:toolu_task_create",
                        "subject": "keep precedence current",
                        "status": "pending"
                    }
                }]}
            })),
            true,
        );

        assert_eq!(
            cursor.last_tasks_at,
            crate::agent_state::parse_rfc3339(repeated_at)
        );
    }

    #[test]
    fn task_result_does_not_override_a_newer_reported_task() {
        let created_at = crate::agent_state::parse_rfc3339("2026-08-14T06:18:40Z").unwrap();
        let reported_at = crate::agent_state::parse_rfc3339("2026-08-14T06:18:41Z").unwrap();
        let result_at = crate::agent_state::parse_rfc3339("2026-08-14T06:18:42Z").unwrap();
        let pane_id = crate::layout::PaneId::from_raw(82);
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(
            &line(serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-08-14T06:18:40Z",
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TaskCreate",
                    "id": "toolu_task_create",
                    "input": {"subject": "transcript task"}
                }]}
            })),
            true,
        );

        let mut store = crate::agent_state::AgentStateStore::default();
        store.observe_transcript(
            pane_id,
            Some(created_at),
            cursor.tasks(),
            cursor.last_tasks_at,
            Vec::new(),
            Vec::new(),
        );
        let reported_task = AgentTask {
            text: "reported task".into(),
            status: AgentTaskStatus::InProgress,
        };
        store
            .report(
                pane_id,
                crate::agent_state::AgentReportPayload {
                    tasks: Some(vec![reported_task.clone()]),
                    ..crate::agent_state::AgentReportPayload::default()
                },
                reported_at,
            )
            .expect("valid report");

        cursor.ingest(
            &line(serde_json::json!({
                "type": "user",
                "timestamp": "2026-08-14T06:18:42Z",
                "toolUseResult": {"task": {
                    "id": "17",
                    "subject": "transcript task"
                }}
            })),
            true,
        );
        store.observe_transcript(
            pane_id,
            Some(result_at),
            cursor.tasks(),
            cursor.last_tasks_at,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            store
                .snapshot(pane_id, crate::api::schema::AgentStatus::Working)
                .tasks,
            vec![reported_task]
        );
        assert_eq!(cursor.last_tasks_at, Some(created_at));
    }

    #[test]
    fn transcript_task_timestamp_tracks_latest_unchanged_todo_write() {
        let first_at = "2026-08-14T06:18:40.100Z";
        let repeated_at = "2026-08-14T06:18:44.100Z";
        let todo_row = |timestamp| {
            line(serde_json::json!({
                "type": "assistant",
                "timestamp": timestamp,
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {"todos": [{
                        "content": "keep precedence current",
                        "status": "in_progress"
                    }]}
                }]}
            }))
        };
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&todo_row(first_at), false);
        cursor.ingest(&todo_row(repeated_at), true);

        assert_eq!(
            cursor.last_tasks_at,
            crate::agent_state::parse_rfc3339(repeated_at)
        );
    }

    #[test]
    fn empty_todo_write_clears_transcript_tasks_at_its_timestamp() {
        let cleared_at = "2026-08-14T06:18:45.100Z";
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(
            &line(serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-08-14T06:18:40.100Z",
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {"todos": [{
                        "content": "remove me",
                        "status": "pending"
                    }]}
                }]}
            })),
            false,
        );
        cursor.ingest(
            &line(serde_json::json!({
                "type": "assistant",
                "timestamp": cleared_at,
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {"todos": []}
                }]}
            })),
            true,
        );

        assert_eq!(cursor.tasks(), Some(Vec::new()));
        assert_eq!(
            cursor.last_tasks_at,
            crate::agent_state::parse_rfc3339(cleared_at)
        );
    }

    #[test]
    fn task_tool_calls_project_before_results_and_reconcile_without_duplicates() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(
            &line(serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-08-14T06:18:40.100Z",
                "message": {"role": "assistant", "content": [{
                    "type": "tool_use",
                    "name": "TaskCreate",
                    "id": "toolu_task_create",
                    "input": {
                        "subject": "inspect runtime boundary",
                        "description": "stripped fixture",
                        "activeForm": "inspecting runtime boundary"
                    }
                }]}
            })),
            true,
        );
        assert_eq!(
            cursor.tasks(),
            Some(vec![AgentTask {
                text: "inspect runtime boundary".into(),
                status: AgentTaskStatus::Pending,
            }])
        );

        cursor.ingest(
            &line(serde_json::json!({
                "type": "user",
                "timestamp": "2026-08-14T06:18:40.126Z",
                "toolUseResult": {"task": {
                    "id": "17",
                    "subject": "inspect runtime boundary",
                    "description": "stripped fixture"
                }}
            })),
            true,
        );
        assert_eq!(cursor.tasks().expect("tasks after result").len(), 1);

        cursor.ingest(
            &line(serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-08-14T06:18:47.401Z",
                "message": {"role": "assistant", "content": [{
                    "type": "tool_use",
                    "name": "TaskUpdate",
                    "id": "toolu_task_update",
                    "input": {
                        "taskId": "17",
                        "subject": "inspect shared runtime boundary",
                        "status": "in_progress"
                    }
                }]}
            })),
            true,
        );
        cursor.ingest(
            &line(serde_json::json!({
                "type": "user",
                "timestamp": "2026-08-14T06:18:47.437Z",
                "toolUseResult": {
                    "success": true,
                    "taskId": "17",
                    "updatedFields": ["subject", "status"]
                }
            })),
            true,
        );
        assert_eq!(
            cursor.tasks(),
            Some(vec![AgentTask {
                text: "inspect shared runtime boundary".into(),
                status: AgentTaskStatus::InProgress,
            }])
        );
    }

    #[test]
    fn valid_large_row_does_not_hide_later_transcript_projections() {
        let oversized_for_old_limit = line(serde_json::json!({
            "type": "attachment",
            "timestamp": "2026-08-14T06:18:39.000Z",
            "unrelatedPayload": "x".repeat(600 * 1024)
        }));
        assert!(oversized_for_old_limit.len() > 514_181);
        let mut fixture = oversized_for_old_limit;
        fixture.extend(line(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-08-14T06:18:40.100Z",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "name": "TaskCreate",
                "id": "toolu_after_large_row",
                "input": {"subject": "recover transcript projection"}
            }]}
        })));
        fixture.extend(line(serde_json::json!({
            "type": "user",
            "timestamp": "2026-08-14T06:18:40.126Z",
            "toolUseResult": {"task": {
                "id": "23",
                "subject": "recover transcript projection"
            }}
        })));
        fixture.extend(line(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-08-14T06:18:41.000Z",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "name": "TaskUpdate",
                "id": "toolu_update_after_large_row",
                "input": {"taskId": "23", "status": "completed"}
            }]}
        })));
        fixture.extend(launch_named(
            AGENT_A,
            "review recovered rows",
            "2026-08-14T06:18:42.000Z",
        ));
        fixture.extend(line(serde_json::json!({
            "type": "user",
            "timestamp": "2026-08-14T06:18:43.125Z",
            "message": {"role": "user", "content":
                "See https://example.test/after-large-row for context."
            }
        })));

        let mut cursor = TranscriptCursor::new();
        let stats = cursor.ingest(&fixture, true);

        assert_eq!(stats.oversized_rows, 0);
        assert_eq!(
            cursor.tasks(),
            Some(vec![AgentTask {
                text: "recover transcript projection".into(),
                status: AgentTaskStatus::Completed,
            }])
        );
        let observations = cursor.observations().expect("authoritative later rows");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].name, "review recovered rows");
        assert_eq!(observations[0].state, AgentState::Working);
        assert!(cursor
            .links
            .iter()
            .any(|link| link.url == "https://example.test/after-large-row"));
        assert_eq!(
            cursor.last_row_at,
            crate::agent_state::parse_rfc3339("2026-08-14T06:18:43.125Z")
        );
    }

    #[test]
    fn tracker_exposes_only_existing_regular_subagent_transcripts() {
        let dir = TestDir::new("transcript-path");
        let path = dir.transcript();
        let child = path
            .with_extension("")
            .join("subagents")
            .join(format!("agent-{AGENT_A}.jsonl"));
        std::fs::create_dir_all(child.parent().expect("subagent directory")).unwrap();
        std::fs::write(&child, b"{}\n").unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path, 1);
        tracker.cursor.ingest(&launch(AGENT_A), true);
        tracker.cursor.ingest(&launch(AGENT_B), true);
        let activity = tracker.refresh_transcript_paths_and_activity(
            std::time::Duration::from_secs(5 * 60),
            Instant::now() + WORKER_TIMEOUT,
        );

        let observations = tracker.observations().expect("authoritative replay");
        assert_eq!(observations[0].transcript_path.as_ref(), Some(&child));
        assert_eq!(observations[1].transcript_path, None);
        assert_eq!(
            activity,
            crate::terminal::state::SubagentTranscriptActivity::Fresh
        );
    }

    #[test]
    fn declared_subagent_count_survives_thirty_minutes_without_transcript_writes() {
        let dir = TestDir::new("subagent-freshness");
        let path = dir.transcript();
        let child = path
            .with_extension("")
            .join("subagents")
            .join(format!("agent-{AGENT_A}.jsonl"));
        std::fs::create_dir_all(child.parent().expect("subagent directory")).unwrap();
        std::fs::write(&child, b"{}\n").unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&child)
            .unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path, 1);
        tracker.cursor.ingest(&launch(AGENT_A), true);
        let freshness_window = std::time::Duration::from_secs(30 * 60);

        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::now() - std::time::Duration::from_secs(29 * 60)),
        )
        .unwrap();
        assert_eq!(
            tracker.refresh_transcript_paths_and_activity(
                freshness_window,
                Instant::now() + WORKER_TIMEOUT,
            ),
            crate::terminal::state::SubagentTranscriptActivity::Fresh
        );
        assert_eq!(tracker.count(), Some(1));

        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::now() - std::time::Duration::from_secs(31 * 60)),
        )
        .unwrap();
        assert_eq!(
            tracker.refresh_transcript_paths_and_activity(
                freshness_window,
                Instant::now() + WORKER_TIMEOUT,
            ),
            crate::terminal::state::SubagentTranscriptActivity::Stale
        );
        assert_eq!(
            tracker.count(),
            Some(1),
            "staleness must not invent a zero worker report"
        );
    }

    #[test]
    fn completed_observation_history_is_bounded() {
        let mut cursor = TranscriptCursor::new();
        cursor.observations = (0..MAX_ACTIVE_IDS)
            .map(|index| ClaudeSubagentObservation {
                id: format!("agent-{index}"),
                parent_id: None,
                name: format!("agent {index}"),
                state: AgentState::Idle,
                observed_at: None,
                transcript_path: None,
            })
            .collect();

        cursor.apply_event(TranscriptEvent::Started(ClaudeSubagentObservation {
            id: "overflow".into(),
            parent_id: None,
            name: "overflow".into(),
            state: AgentState::Working,
            observed_at: None,
            transcript_path: None,
        }));

        assert!(!cursor.trustworthy);
        assert_eq!(cursor.observations.len(), MAX_ACTIVE_IDS);
    }

    #[test]
    fn successful_resume_readds_only_the_reported_agent() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch(AGENT_A), true);
        cursor.ingest(&completion(AGENT_A), true);
        cursor.ingest(
            &line(serde_json::json!({
                "type": "user",
                "toolUseResult": {
                    "success": true,
                    "message": "Resuming agent",
                    "resumedAgentId": AGENT_A
                }
            })),
            true,
        );
        assert_eq!(cursor.count(), Some(1));
    }

    #[test]
    fn duplicate_launch_and_completion_are_idempotent() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch(AGENT_A), true);
        cursor.ingest(&launch(AGENT_A), true);
        assert_eq!(cursor.count(), Some(1));
        cursor.ingest(&completion(AGENT_A), true);
        cursor.ingest(&completion(AGENT_A), true);
        assert_eq!(cursor.count(), Some(0));
    }

    #[test]
    fn unrelated_background_task_notification_does_not_remove_agent() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch(AGENT_A), true);
        cursor.ingest(&completion("bhqhdnqhg"), true);
        assert_eq!(cursor.count(), Some(1));
    }

    #[test]
    fn torn_final_record_is_retried_without_double_counting() {
        let launch = launch(AGENT_A);
        for split in 1..launch.len() {
            let mut cursor = TranscriptCursor::new();
            cursor.ingest(&launch[..split], true);
            assert_eq!(cursor.count(), None);
            cursor.ingest(&launch[split..], true);
            assert_eq!(cursor.count(), Some(1), "split at {split}");
        }
    }

    #[test]
    fn malformed_or_oversized_complete_row_invalidates_authority() {
        let mut malformed = TranscriptCursor::new();
        malformed.ingest(b"not-json\n", true);
        assert_eq!(malformed.count(), None);

        let mut oversized = TranscriptCursor::new();
        let mut row = vec![b'x'; MAX_BUFFERED_JSON_ROW + 1];
        row.push(b'\n');
        let stats = oversized.ingest(&row, true);
        assert_eq!(stats.oversized_rows, 1);
        assert_eq!(oversized.count(), None);
        oversized.ingest(&launch(AGENT_A), true);
        assert_eq!(
            oversized.count(),
            None,
            "a discarded lifecycle-sized row keeps later history fail-closed"
        );
    }

    #[test]
    fn count_stays_unknown_until_initial_replay_reaches_complete_eof() {
        let launch = launch(AGENT_A);
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch[..launch.len() - 1], true);
        assert_eq!(cursor.count(), None);
        cursor.ingest(&launch[launch.len() - 1..], true);
        assert_eq!(cursor.count(), Some(1));
    }

    #[test]
    fn claude_transcript_path_binds_profile_path_to_session_id() {
        let session = AgentSessionRef::id(SESSION_ID).unwrap();
        let default = absolute_fixture_path(format!(
            "home/user/.claude/projects/-tmp-repro/{SESSION_ID}.jsonl"
        ));
        let profile = absolute_fixture_path(format!(
            "profiles/team-a/projects/-tmp-repro/{SESSION_ID}.jsonl"
        ));
        assert_eq!(
            validated_transcript_path("herdr:claude", "claude", Some(&session), Some(&default)),
            Some(PathBuf::from(default))
        );
        assert_eq!(
            validated_transcript_path("herdr:claude", "claude", Some(&session), Some(&profile)),
            Some(PathBuf::from(profile))
        );
    }

    #[test]
    fn spoofed_or_mismatched_transcript_path_is_rejected() {
        let session = AgentSessionRef::id(SESSION_ID).unwrap();
        let other = absolute_fixture_path("projects/-tmp-repro/other.jsonl");
        let traversal = absolute_fixture_path(format!("projects/../-tmp-repro/{SESSION_ID}.jsonl"));
        assert!(
            validated_transcript_path("custom:claude", "claude", Some(&session), Some(&other))
                .is_none()
        );
        assert!(
            validated_transcript_path("herdr:claude", "claude", Some(&session), Some(&other))
                .is_none()
        );
        assert!(validated_transcript_path(
            "herdr:claude",
            "claude",
            Some(&session),
            Some(&traversal)
        )
        .is_none());
    }

    #[test]
    fn tracker_reads_only_appended_bytes_after_initial_replay() {
        let dir = TestDir::new("append");
        let path = dir.transcript();
        let mut fixture = launch(AGENT_A);
        fixture.extend(launch(AGENT_B));
        std::fs::write(&path, &fixture).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path.clone(), 1);
        let first = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(2));
        assert_eq!(first.parse.bytes_read as usize, fixture.len());

        let completed = completion(AGENT_A);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&completed)
            .unwrap();
        let second = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(1));
        assert_eq!(second.parse.bytes_read as usize, completed.len());

        let unchanged = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(unchanged.metadata_hits, 1);
        assert_eq!(unchanged.parse.bytes_read, 0);
    }

    #[test]
    fn tracker_bounds_initial_replay_and_does_not_publish_partial_count() {
        let dir = TestDir::new("bounded");
        let path = dir.transcript();
        let mut fixture = Vec::new();
        for index in 0..800 {
            fixture.extend(line(serde_json::json!({
                "type": "attachment",
                "index": index,
                "padding": "x".repeat(256)
            })));
        }
        fixture.extend(launch(AGENT_A));
        std::fs::write(&path, fixture).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path, 1);
        let first = tracker.scan(32 * 1024, Instant::now() + WORKER_TIMEOUT);
        assert!(first.parse.bytes_read <= 32 * 1024);
        assert_eq!(tracker.count(), None);

        for _ in 0..16 {
            tracker.scan(32 * 1024, Instant::now() + WORKER_TIMEOUT);
            if tracker.count().is_some() {
                break;
            }
        }
        assert_eq!(tracker.count(), Some(1));
    }

    #[test]
    fn batch_read_budget_bounds_large_claude_fleets() {
        let dir = TestDir::new("fleet-budget");
        let mut fixture = Vec::new();
        while fixture.len() <= PER_TARGET_READ_QUANTUM {
            fixture.extend(line(serde_json::json!({
                "type": "attachment",
                "padding": "x".repeat(512)
            })));
        }

        let work = (0..41)
            .map(|index| {
                let path = dir.0.join(format!("target-{index}.jsonl"));
                std::fs::write(&path, &fixture).unwrap();
                let terminal_id = crate::terminal::TerminalId::alloc();
                RefreshWorkItem {
                    target: TargetIdentity {
                        terminal_id,
                        source: "herdr:claude".into(),
                        session_id: SESSION_ID.into(),
                        path: path.clone(),
                        target_generation: 1,
                        turn_generation: 0,
                    },
                    tracker: TranscriptTracker::new(SESSION_ID.into(), path, 1),
                }
            })
            .collect();

        let (observations, stats) = refresh_trackers(
            work,
            Instant::now() + std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5 * 60),
        );
        assert_eq!(stats.targets_total, 41);
        assert_eq!(stats.targets_attempted, 16);
        assert_eq!(observations.len(), 16);
        assert_eq!(stats.bytes_read as usize, BATCH_READ_BUDGET);
        assert!(observations
            .iter()
            .all(|observation| observation.count.is_none()));
    }

    #[test]
    fn unchanged_41_target_fleet_reads_zero_transcript_bytes() {
        let dir = TestDir::new("fleet-steady-state");
        let fixture = line(serde_json::json!({ "type": "attachment" }));
        let work = (0..41)
            .map(|index| {
                let path = dir.0.join(format!("target-{index}.jsonl"));
                std::fs::write(&path, &fixture).unwrap();
                RefreshWorkItem {
                    target: TargetIdentity {
                        terminal_id: crate::terminal::TerminalId::alloc(),
                        source: "herdr:claude".into(),
                        session_id: SESSION_ID.into(),
                        path: path.clone(),
                        target_generation: 1,
                        turn_generation: 0,
                    },
                    tracker: TranscriptTracker::new(SESSION_ID.into(), path, 1),
                }
            })
            .collect();
        let (initial, initial_stats) = refresh_trackers(
            work,
            Instant::now() + std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5 * 60),
        );
        let steady_work = initial
            .into_iter()
            .map(|observation| RefreshWorkItem {
                target: observation.target,
                tracker: observation.tracker,
            })
            .collect();
        let (_, steady_stats) = refresh_trackers(
            steady_work,
            Instant::now() + std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5 * 60),
        );
        assert_eq!(initial_stats.targets_attempted, 41);
        assert_eq!(steady_stats.targets_attempted, 41);
        assert_eq!(steady_stats.metadata_hits, 41);
        assert_eq!(steady_stats.bytes_read, 0);
    }

    #[test]
    fn refresh_rotation_reaches_every_target_in_a_41_pane_fleet() {
        let dir = TestDir::new("rotation");
        let mut work = (0..41)
            .map(|index| {
                let path = dir.0.join(format!("target-{index}.jsonl"));
                RefreshWorkItem {
                    target: TargetIdentity {
                        terminal_id: crate::terminal::TerminalId::alloc(),
                        source: "herdr:claude".into(),
                        session_id: SESSION_ID.into(),
                        path: path.clone(),
                        target_generation: index,
                        turn_generation: 0,
                    },
                    tracker: TranscriptTracker::new(SESSION_ID.into(), path, index),
                }
            })
            .collect::<Vec<_>>();
        let mut rotation = 0;
        let mut visited = HashSet::new();

        for _ in 0..3 {
            rotation = rotate_refresh_work(&mut work, rotation);
            visited.extend(
                work.iter()
                    .take(BATCH_READ_BUDGET / PER_TARGET_READ_QUANTUM)
                    .map(|item| item.target.target_generation),
            );
            work.sort_by_key(|item| item.target.target_generation);
        }

        assert_eq!(visited.len(), 41);
        assert_eq!(rotation, 7);
    }

    fn app_with_claude_target(path: PathBuf) -> (crate::app::App, crate::terminal::TerminalId) {
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("claude")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(SESSION_ID).unwrap(),
        });
        terminal.set_claude_transcript_target(Some(SESSION_ID.into()), Some(path));
        (app, terminal_id)
    }

    fn merged_work(url: &str) -> crate::work_index::Snapshot {
        crate::work_index::Snapshot {
            items: vec![crate::work_index::WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(21),
                pr_url: Some(url.into()),
                pr_title: None,
                pr_state: Some("merged".into()),
                draft: false,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: Default::default(),
                audience: Default::default(),
                cached_pr_detail: None,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: None,
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: Default::default(),
            }],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::now(),
        }
    }

    fn observation(
        terminal_id: crate::terminal::TerminalId,
        path: PathBuf,
        target_generation: u64,
        active_id: &str,
    ) -> RefreshObservation {
        let mut tracker =
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), target_generation);
        tracker.cursor.ingest(
            &launch_named(active_id, "bounded fixture", "2026-08-14T06:18:40.100Z"),
            true,
        );
        RefreshObservation {
            target: TargetIdentity {
                terminal_id,
                source: "herdr:claude".into(),
                session_id: SESSION_ID.into(),
                path,
                target_generation,
                turn_generation: 0,
            },
            count: tracker.count(),
            observations: tracker.observations(),
            transcript_activity: crate::terminal::state::SubagentTranscriptActivity::Unknown,
            tracker,
            stats: ScanStats::default(),
        }
    }

    fn completed_observation(
        terminal_id: crate::terminal::TerminalId,
        path: PathBuf,
        target_generation: u64,
        active_id: &str,
    ) -> RefreshObservation {
        let mut tracker =
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), target_generation);
        tracker.cursor.ingest(&launch(active_id), true);
        tracker.cursor.ingest(&completion(active_id), true);
        RefreshObservation {
            target: TargetIdentity {
                terminal_id,
                source: "herdr:claude".into(),
                session_id: SESSION_ID.into(),
                path,
                target_generation,
                turn_generation: 0,
            },
            count: tracker.count(),
            observations: tracker.observations(),
            transcript_activity: crate::terminal::state::SubagentTranscriptActivity::Unknown,
            tracker,
            stats: ScanStats::default(),
        }
    }

    #[test]
    fn stale_transcript_refresh_preserves_an_existing_watchdog_mark() {
        let dir = TestDir::new("stale-refresh-preserves-watchdog");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let mut refresh = observation(terminal_id.clone(), path.clone(), 7, AGENT_A);
        refresh.transcript_activity = crate::terminal::state::SubagentTranscriptActivity::Stale;
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_active_subagents(Some(1));
        terminal.supervisor_stale = true;
        terminal.stale_resolution = Some((AgentState::Unknown, true));
        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path, 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(app.handle_claude_subagents_refreshed(1, vec![refresh], BatchStats::default(),));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.supervisor_stale);
        assert_eq!(terminal.active_subagents, Some(1));
    }

    #[test]
    fn completed_task_becomes_unread_done_when_native_worker_count_drains() {
        let dir = TestDir::new("done-after-native-worker");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.active = None;
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(crate::detect::Agent::Claude), AgentState::Idle);

        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });
        assert!(app.handle_claude_subagents_refreshed(
            1,
            vec![observation(terminal_id.clone(), path.clone(), 7, AGENT_A,)],
            BatchStats::default(),
        ));

        app.handle_internal_event_with_pane_updates(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "herdr:claude-closing-block".into(),
            agent_label: "claude".into(),
            state: AgentState::Idle,
            message: None,
            seq: Some(1),
            wait: None,
            eta_s: None,
            reported_at: None,
            session_ref: None,
            closing_block: Some(Box::new(crate::events::ClosingBlockReport {
                gates: Vec::new(),
                items: Vec::new(),
                decisions: Vec::new(),
                agents: None,
                completion: Some(crate::api::schema::ClosingCompletion::Complete),
                external_wait: None,
                parse_status: Some(crate::api::schema::ClosingParseStatus::Ok),
                workers_unknown: Some(false),
                dependencies_authoritative: true,
                session_id: Some(SESSION_ID.into()),
                last_turn_at: None,
                settle_ready: false,
            })),
        });
        let pane = app.state.workspaces[0].pane_state(pane_id).unwrap();
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            pane.agent_projection(terminal).status_key(),
            "waiting_on_agents"
        );
        assert!(pane.seen);
        assert!(pane.done_since.is_none());

        app.last_claude_subagent_refresh_generation = 2;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 2,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });
        assert!(app.handle_claude_subagents_refreshed(
            2,
            vec![completed_observation(terminal_id.clone(), path, 7, AGENT_A)],
            BatchStats::default(),
        ));

        let pane = app.state.workspaces[0].pane_state(pane_id).unwrap();
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(pane.agent_projection(terminal).status_key(), "done");
        assert!(!pane.seen);
        assert!(pane.done_since.is_some());
    }

    #[test]
    fn active_subagent_blocks_overdue_inactivity_and_ripe_finished_work() {
        let dir = TestDir::new("settle-after-completion");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let url = "https://github.com/owner/repo/pull/21";
        let work = merged_work(url);
        let old_activity = Instant::now();
        let now = old_activity + std::time::Duration::from_secs(2 * 60 * 60);
        app.state.active = None;
        app.state.auto_settle_inactive = false;
        app.state.auto_settle_finished = true;
        app.state.auto_settle_done = false;
        app.state.settle_after = std::time::Duration::from_secs(60 * 60);
        app.state.settle_finished_after = std::time::Duration::from_secs(30 * 60);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(crate::detect::Agent::Claude), AgentState::Idle);
        terminal
            .restore_work_context(crate::work_context::PaneWorkContext {
                pr_urls: vec![url.into()],
                ..Default::default()
            })
            .expect("valid work context");
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .activity
            .set_last_at(old_activity);
        let armed_at = now - app.state.settle_finished_after;
        assert_eq!(
            app.state
                .refresh_settled_panes_at(Some(&work), armed_at, 1_725_000_000),
            0
        );
        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });
        assert!(app.handle_claude_subagents_refreshed(
            1,
            vec![observation(terminal_id.clone(), path.clone(), 7, AGENT_A)],
            BatchStats::default(),
        ));
        app.state.auto_settle_inactive = true;
        assert!(
            app.state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(now)
                >= app.state.settle_after
        );
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&pane_id].finished_since,
            Some(armed_at)
        );
        assert_eq!(
            app.state
                .refresh_settled_panes_at(Some(&work), now, 1_725_000_001),
            0,
            "a live sub-agent must outrank both ripe settle triggers"
        );
        assert!(!app.state.pane_is_settled(0, pane_id));

        app.last_claude_subagent_refresh_generation = 2;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 2,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });
        assert!(app.handle_claude_subagents_refreshed(
            2,
            vec![completed_observation(terminal_id, path, 7, AGENT_A)],
            BatchStats::default(),
        ));
        app.state.auto_settle_inactive = false;
        assert_eq!(
            app.state
                .refresh_settled_panes_at(Some(&work), now, 1_725_000_002),
            0,
            "clearing the last sub-agent must arm a fresh finished-work window"
        );
        assert_eq!(
            app.state.refresh_settled_panes_at(
                Some(&work),
                now + app.state.settle_finished_after - std::time::Duration::from_nanos(1),
                1_725_001_800,
            ),
            0
        );
        assert_eq!(
            app.state.refresh_settled_panes_at(
                Some(&work),
                now + app.state.settle_finished_after,
                1_725_001_801,
            ),
            1
        );
    }

    #[test]
    fn refresh_applies_live_count_and_rejects_stale_session_result() {
        let dir = TestDir::new("stale-result");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state
            .agent_states
            .report(
                pane_id,
                crate::agent_state::AgentReportPayload {
                    subagents: Some(vec![crate::agent_state::AgentSubagent {
                        name: "reported reviewer".into(),
                        status: crate::api::schema::AgentStatus::Blocked,
                        last_active_at: None,
                        pane_id: None,
                        source: crate::agent_state::AgentSubagentSource::Reported,
                    }]),
                    ..crate::agent_state::AgentReportPayload::default()
                },
                SystemTime::now(),
            )
            .expect("valid report");
        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(app.handle_claude_subagents_refreshed(
            1,
            vec![observation(terminal_id.clone(), path.clone(), 7, AGENT_A)],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, Some(1));
        let projected = app
            .state
            .agent_states
            .snapshot(pane_id, crate::api::schema::AgentStatus::Working)
            .subagents;
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].name, "bounded fixture");
        assert_eq!(
            projected[0].status,
            crate::api::schema::AgentStatus::Working
        );
        assert_eq!(
            projected[0].last_active_at.as_deref(),
            Some("2026-08-14T06:18:40.1Z")
        );
        assert_eq!(
            projected[0].source,
            crate::agent_state::AgentSubagentSource::Observed
        );
        assert_eq!(
            projected[1].source,
            crate::agent_state::AgentSubagentSource::Reported
        );

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_claude_transcript_target(None, None);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(None);
        app.last_claude_subagent_refresh_generation = 2;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 2,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(!app.handle_claude_subagents_refreshed(
            2,
            vec![observation(terminal_id.clone(), path, 7, AGENT_B)],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, None);
    }

    #[test]
    fn new_transcript_session_without_tasks_clears_previous_session_tasks() {
        let dir = TestDir::new("new-session-clears-tasks");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let old_task_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        app.state.agent_states.observe_transcript(
            pane_id,
            Some(old_task_at),
            Some(vec![AgentTask {
                text: "old session task".into(),
                status: AgentTaskStatus::InProgress,
            }]),
            Some(old_task_at),
            Vec::new(),
            Vec::new(),
        );

        let new_session = "e6f47cd4-0e58-4de7-8ce3-8a704654573a";
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_claude_transcript_target(Some(new_session.into()), Some(path.clone()));
        let mut tracker = TranscriptTracker::new(new_session.into(), path.clone(), 8);
        tracker.cursor.ingest(
            &line(serde_json::json!({
                "type": "user",
                "timestamp": "2026-08-14T06:18:45.100Z",
                "message": {"content": "new session without todo calls"}
            })),
            true,
        );
        assert_eq!(tracker.tasks(), Some(Vec::new()));
        assert_eq!(tracker.last_tasks_at(), None);
        let refresh = RefreshObservation {
            target: TargetIdentity {
                terminal_id: terminal_id.clone(),
                source: "herdr:claude".into(),
                session_id: new_session.into(),
                path: path.clone(),
                target_generation: 8,
                turn_generation: app.state.terminals[&terminal_id].agent_turn_generation(),
            },
            count: tracker.count(),
            observations: tracker.observations(),
            transcript_activity: crate::terminal::state::SubagentTranscriptActivity::Unknown,
            tracker: tracker.clone(),
            stats: ScanStats::default(),
        };
        app.claude_subagent_trackers.insert(terminal_id, tracker);
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(app.handle_claude_subagents_refreshed(1, vec![refresh], BatchStats::default(),));
        assert!(app
            .state
            .agent_states
            .snapshot(pane_id, crate::api::schema::AgentStatus::Working)
            .tasks
            .is_empty());
    }

    #[test]
    fn refresh_sampled_before_a_new_turn_cannot_clear_its_worker() {
        let dir = TestDir::new("stale-turn-result");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let sampled_zero = completed_observation(terminal_id.clone(), path.clone(), 7, AGENT_A);
        let reported_at = Instant::now();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(crate::detect::Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1),
            reported_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(crate::detect::Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            reported_at + std::time::Duration::from_secs(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(crate::detect::Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            reported_at + std::time::Duration::from_secs(2),
        );
        terminal.set_active_subagents(Some(1));

        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path, 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(!app.handle_claude_subagents_refreshed(
            1,
            vec![sampled_zero],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, Some(1));
    }

    #[test]
    fn refresh_sampled_before_a_hookless_native_turn_cannot_clear_its_worker() {
        let dir = TestDir::new("stale-hookless-turn-result");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let sampled_zero = completed_observation(terminal_id.clone(), path.clone(), 7, AGENT_A);
        let observed_at = Instant::now();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(crate::detect::Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            observed_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(crate::detect::Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed_at + std::time::Duration::from_secs(1),
        );
        terminal.set_active_subagents(Some(1));

        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path, 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(!app.handle_claude_subagents_refreshed(
            1,
            vec![sampled_zero],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, Some(1));
    }

    #[test]
    fn refresh_sampled_before_a_user_prompt_hook_turn_cannot_clear_its_worker() {
        let dir = TestDir::new("stale-user-prompt-turn-result");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        let sampled_zero = completed_observation(terminal_id.clone(), path.clone(), 7, AGENT_A);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_hook_authority(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            Some(1),
        );
        let sampled_generation = terminal.agent_turn_generation();

        app.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "herdr:claude-closing-block".into(),
            agent_label: "claude".into(),
            state: AgentState::Working,
            message: None,
            seq: Some(2),
            wait: None,
            eta_s: None,
            reported_at: None,
            session_ref: None,
            closing_block: None,
        });
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(Some(1));
        assert_eq!(
            app.state.terminals[&terminal_id].agent_turn_generation(),
            sampled_generation.wrapping_add(1),
            "the accepted UserPromptSubmit hook starts a new turn"
        );

        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path, 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(!app.handle_claude_subagents_refreshed(
            1,
            vec![sampled_zero],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, Some(1));
    }

    #[test]
    fn refresh_result_after_worker_deadline_is_ignored() {
        let dir = TestDir::new("expired-result");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), 3),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() - std::time::Duration::from_millis(1),
        });

        assert!(!app.handle_claude_subagents_refreshed(
            1,
            vec![observation(terminal_id.clone(), path, 3, AGENT_A)],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, None);
    }

    #[test]
    fn transcript_truncation_clears_and_rebuilds_active_set() {
        let dir = TestDir::new("truncate");
        let path = dir.transcript();
        std::fs::write(&path, launch(AGENT_A)).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path.clone(), 1);
        tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(1));

        std::fs::write(&path, []).unwrap();
        let reset = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(reset.identity_resets, 1);
        assert_eq!(tracker.count(), Some(0));
    }

    #[test]
    fn transcript_replacement_discards_old_active_ids() {
        let dir = TestDir::new("replace");
        let path = dir.transcript();
        let old_path = path.with_extension("old");
        std::fs::write(&path, launch(AGENT_A)).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path.clone(), 1);
        tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(1));

        std::fs::rename(&path, &old_path).unwrap();
        std::fs::write(&path, launch(AGENT_B)).unwrap();
        let reset = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(reset.identity_resets, 1);
        assert_eq!(tracker.count(), Some(1));
        assert!(tracker.cursor.active_ids.contains(AGENT_B));
        assert!(!tracker.cursor.active_ids.contains(AGENT_A));
    }

    #[cfg(unix)]
    #[test]
    fn tracker_rejects_symlink_and_non_regular_targets() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new("unsafe-targets");
        let regular = dir.0.join("regular.jsonl");
        let linked = dir.transcript();
        std::fs::write(&regular, launch(AGENT_A)).unwrap();
        symlink(&regular, &linked).unwrap();
        let mut linked_tracker = TranscriptTracker::new(SESSION_ID.into(), linked, 1);
        let linked_stats =
            linked_tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(linked_stats.open_failures, 1);
        assert_eq!(linked_tracker.count(), None);

        let mut directory_tracker =
            TranscriptTracker::new(SESSION_ID.into(), dir.0.join("projects"), 1);
        let directory_stats =
            directory_tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(directory_stats.open_failures, 1);
        assert_eq!(directory_tracker.count(), None);
    }
}
