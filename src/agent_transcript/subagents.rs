//! The subagents an omp agent spawned with its `task` tool, rebuilt from its
//! session file and each subagent's own one.
//!
//! omp records a subagent in three places. The `task` tool result (written at
//! spawn) lists each one in `details.progress` and, for a background spawn,
//! says `details.async.state: "running"`. An `async-result` note settles it:
//! `details.jobs[]` names the job (`type: "task"`) and its `<task-result>`
//! envelope carries the status. Its conversation is an ordinary omp session
//! file at `<parent session path without .jsonl>/<id>.jsonl`, whose header
//! names the parent in `parentSession`. omp persists no progress beyond the
//! spawn, so the newest tool intent comes from that file's tail, which also
//! settles a subagent whose `async-result` a `hub wait` consumed.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde::Deserialize;
use serde_json::Value;

use crate::api::schema::{TranscriptSubagent, TranscriptSubagentStatus};

use super::index::Kind;
use super::normalize::{self, NoteLine, ResultLine, StampLine};
use super::{TranscriptError, View};

/// Subagents one page lists at most.
const MAX_SUBAGENTS: usize = 20;
/// How much of a running subagent's file is read for its newest step.
const CHILD_TAIL_BYTES: u64 = 64 * 1024;
/// Where a session file's header must appear.
const CHILD_HEADER_BYTES: u64 = 16 * 1024;
/// Subagent files whose tail is remembered. A finished subagent's file stops
/// growing, so a session with many of them reads each tail once.
const CACHED_CHILDREN: usize = 512;

/// The child file's length when its tail was read, and what the tail said.
static CHILD_TAILS: LazyLock<Mutex<HashMap<PathBuf, (u64, ChildTail)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The session file path of `parent`'s subagent `id`, when the id is a plain
/// omp agent id (no separators, so the path stays in `parent`'s directory).
fn child_path(parent: &Path, id: &str) -> Option<PathBuf> {
    let plain = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if !plain || parent.extension()? != "jsonl" {
        return None;
    }
    Some(
        parent
            .with_file_name(parent.file_stem()?)
            .join(format!("{id}.jsonl")),
    )
}

/// The session file of `parent`'s subagent `id`, when there is one that
/// names `parent` as its parent session.
pub(super) fn child_session(parent: &Path, id: &str) -> Option<PathBuf> {
    let path = child_path(parent, id)?;
    (header_parent(&path)?.as_path() == parent).then_some(path)
}

/// What `parent`'s subagent `id` says in its own file; `None` when it has no
/// file of its own (see `child_session`). The tail is read again only once
/// the file has grown.
fn child_tail(parent: &Path, id: &str) -> Option<ChildTail> {
    let path = child_path(parent, id)?;
    let len = std::fs::metadata(&path).ok()?.len();
    let mut cache = CHILD_TAILS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((seen, tail)) = cache.get(&path) {
        if *seen == len {
            return Some(tail.clone());
        }
    }
    if header_parent(&path)?.as_path() != parent {
        cache.remove(&path);
        return None;
    }
    let tail = read_child_tail(&path).ok()?;
    if cache.len() >= CACHED_CHILDREN {
        cache.clear();
    }
    cache.insert(path, (len, tail.clone()));
    Some(tail)
}

/// The `parentSession` of the session header in `path`'s first lines.
fn header_parent(path: &Path) -> Option<PathBuf> {
    #[derive(Deserialize)]
    struct Header {
        #[serde(rename = "type", default)]
        kind: Option<String>,
        #[serde(rename = "parentSession", default)]
        parent_session: Option<String>,
    }
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(CHILD_HEADER_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    head.split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Header>(line).ok())
        .find(|header| header.kind.as_deref() == Some("session"))?
        .parent_session
        .map(PathBuf::from)
}

/// What a subagent's own file says about it.
#[derive(Debug, Clone, Default, PartialEq)]
struct ChildTail {
    last_intent: Option<String>,
    last_ms: Option<u64>,
    /// How and when it ended, once its session exited for good.
    ended: Option<(TranscriptSubagentStatus, u64)>,
}

/// The newest tool intent in the last `CHILD_TAIL_BYTES` of a subagent's file
/// and, when its session exited after its last activity, how it ended: a
/// `yield` (answered or still pending at exit) or a final `stop` completed
/// it, an `error` stop failed it, and an exit in the middle of its work
/// aborted it.
fn read_child_tail(path: &Path) -> std::io::Result<ChildTail> {
    #[derive(Deserialize)]
    struct TailLine {
        #[serde(rename = "type", default)]
        kind: String,
        #[serde(rename = "customType", default)]
        custom_type: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
        #[serde(default)]
        data: Option<Value>,
        #[serde(default)]
        message: Option<TailMessage>,
    }
    #[derive(Deserialize)]
    struct TailMessage {
        #[serde(default)]
        role: Option<String>,
        #[serde(rename = "toolName", default)]
        tool_name: Option<String>,
        #[serde(rename = "isError", default)]
        is_error: bool,
        #[serde(rename = "stopReason", default)]
        stop_reason: Option<String>,
    }

    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(CHILD_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(CHILD_TAIL_BYTES).read_to_end(&mut bytes)?;
    let mut lines = bytes.split(|&byte| byte == b'\n');
    if start > 0 {
        // The first line is cut.
        lines.next();
    }
    let mut tail = ChildTail::default();
    let mut answered = false;
    let mut last_stop: Option<String> = None;
    for line in lines {
        let Ok(line) = serde_json::from_slice::<TailLine>(line) else {
            continue;
        };
        let ms = line
            .timestamp
            .as_deref()
            .and_then(normalize::parse_iso8601_ms);
        match (line.kind.as_str(), line.custom_type.as_deref()) {
            ("custom", Some("tool_execution_start")) => {
                tail.ended = None;
                if let Some(intent) = line
                    .data
                    .as_ref()
                    .and_then(|data| data.get("intent"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|intent| !intent.is_empty())
                {
                    tail.last_intent = Some(intent.to_owned());
                    tail.last_ms = ms.or(tail.last_ms);
                }
            }
            ("custom", Some("session_exit")) => {
                let pending_yield = line
                    .data
                    .as_ref()
                    .and_then(|data| data.get("pendingToolCalls"))
                    .and_then(Value::as_array)
                    .is_some_and(|calls| {
                        calls.iter().any(|call| {
                            call.get("toolName").and_then(Value::as_str) == Some("yield")
                        })
                    });
                let status = if answered || pending_yield {
                    TranscriptSubagentStatus::Completed
                } else {
                    match last_stop.as_deref() {
                        Some("stop") => TranscriptSubagentStatus::Completed,
                        Some("error") => TranscriptSubagentStatus::Failed,
                        _ => TranscriptSubagentStatus::Aborted,
                    }
                };
                tail.ended = ms.map(|ms| (status, ms));
            }
            ("message", _) => {
                let Some(message) = line.message else {
                    continue;
                };
                tail.ended = None;
                match message.role.as_deref() {
                    Some("toolResult") if message.tool_name.as_deref() == Some("yield") => {
                        answered = !message.is_error;
                    }
                    Some("assistant") => last_stop = message.stop_reason,
                    Some("user") => {
                        answered = false;
                        last_stop = None;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(tail)
}

#[derive(Deserialize)]
struct Progress {
    id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    assignment: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct SyncResult {
    id: String,
    #[serde(rename = "exitCode", default)]
    exit_code: Option<i64>,
    #[serde(default)]
    aborted: bool,
    #[serde(rename = "durationMs", default)]
    duration_ms: Option<u64>,
}

#[derive(Deserialize)]
struct Job {
    #[serde(rename = "jobId")]
    job_id: String,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(rename = "durationMs", default)]
    duration_ms: Option<u64>,
}

struct Spawn {
    subagent: TranscriptSubagent,
    /// Branch position of the `task` result that spawned it.
    position: usize,
    /// When the agent's session exited while this one still ran.
    exited_ms: Option<u64>,
}

fn progress_status(status: Option<&str>) -> TranscriptSubagentStatus {
    match status {
        Some("pending" | "running") => TranscriptSubagentStatus::Running,
        Some("completed") => TranscriptSubagentStatus::Completed,
        Some("failed") => TranscriptSubagentStatus::Failed,
        Some("aborted") => TranscriptSubagentStatus::Aborted,
        _ => TranscriptSubagentStatus::Unknown,
    }
}

fn settle(subagent: &mut TranscriptSubagent, status: TranscriptSubagentStatus, at_ms: u64) {
    subagent.status = status;
    subagent.finished_ms = Some(at_ms);
    subagent.duration_ms = Some(at_ms.saturating_sub(subagent.started_ms));
}

impl View<'_> {
    /// Every subagent still running, then those spawned since the newest
    /// prompt; running first, newest first, at most `MAX_SUBAGENTS`. A `/clear`
    /// forgets them all; an exit of the agent aborts the running ones.
    pub(super) fn current_subagents(
        &self,
    ) -> Result<Option<Vec<TranscriptSubagent>>, TranscriptError> {
        let newest_prompt = self
            .branch
            .iter()
            .rposition(|&record| self.index.record(record).kind == Kind::User);
        let in_newest_turn = |position: usize| newest_prompt.is_none_or(|prompt| position > prompt);
        let mut spawns: Vec<Spawn> = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for (position, &record) in self.branch.iter().enumerate() {
            match self.index.record(record).kind {
                Kind::TaskResult => {
                    let failed = self.index.record(record).failed;
                    let Some(line) = self.parse::<ResultLine>(record)? else {
                        continue;
                    };
                    let at_ms = normalize::ts_ms(line.message.timestamp, line.timestamp.as_deref());
                    for subagent in spawned(line.message.details.as_ref(), failed, at_ms) {
                        let spawn = Spawn {
                            subagent,
                            position,
                            exited_ms: None,
                        };
                        match by_id.get(&spawn.subagent.id) {
                            Some(&slot) => spawns[slot] = spawn,
                            None => {
                                by_id.insert(spawn.subagent.id.clone(), spawns.len());
                                spawns.push(spawn);
                            }
                        }
                    }
                }
                Kind::AsyncResult => {
                    let Some(line) = self.parse::<NoteLine>(record)? else {
                        continue;
                    };
                    let jobs = line
                        .details
                        .as_ref()
                        .and_then(|details| details.get("jobs"))
                        .and_then(|jobs| Vec::<Job>::deserialize(jobs).ok())
                        .unwrap_or_default();
                    let at_ms = normalize::ts_ms(None, line.timestamp.as_deref());
                    let text = normalize::content_text(&line.content);
                    for job in jobs
                        .iter()
                        .filter(|job| job.kind.as_deref() == Some("task"))
                    {
                        let Some(&slot) = by_id.get(&job.job_id) else {
                            continue;
                        };
                        let spawn = &mut spawns[slot];
                        if spawn.subagent.status != TranscriptSubagentStatus::Running {
                            continue;
                        }
                        let status = normalize::task_result_status(&text, &job.job_id)
                            .unwrap_or(TranscriptSubagentStatus::Unknown);
                        settle(&mut spawn.subagent, status, at_ms);
                        if let Some(duration) = job.duration_ms {
                            spawn.subagent.duration_ms = Some(duration);
                        }
                    }
                }
                Kind::SessionExit => {
                    let at_ms = self
                        .parse::<StampLine>(record)?
                        .and_then(|line| line.timestamp)
                        .and_then(|stamp| normalize::parse_iso8601_ms(&stamp));
                    for spawn in &mut spawns {
                        if spawn.subagent.status != TranscriptSubagentStatus::Running {
                            continue;
                        }
                        let at_ms = at_ms.unwrap_or(spawn.subagent.started_ms);
                        if in_newest_turn(spawn.position) {
                            // Its own file may still say it finished first.
                            spawn.exited_ms = Some(at_ms);
                        } else {
                            settle(
                                &mut spawn.subagent,
                                TranscriptSubagentStatus::Aborted,
                                at_ms,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        spawns.retain(|spawn| {
            spawn.subagent.status == TranscriptSubagentStatus::Running
                || in_newest_turn(spawn.position)
        });

        let parent = self.index.path();
        for spawn in &mut spawns {
            let subagent = &mut spawn.subagent;
            // A missing or unreadable file leaves the spawn's own record.
            let tail = child_tail(parent, &subagent.id);
            subagent.has_transcript = tail.is_some();
            let tail = tail.unwrap_or_default();
            subagent.last_intent = tail.last_intent;
            subagent.last_ms = tail.last_ms;
            if subagent.status != TranscriptSubagentStatus::Running {
                continue;
            }
            match (tail.ended, spawn.exited_ms) {
                (Some((status, at_ms)), _) => settle(subagent, status, at_ms),
                (None, Some(at_ms)) => settle(subagent, TranscriptSubagentStatus::Aborted, at_ms),
                (None, None) => {}
            }
        }
        // An older one its own file shows finished is no longer listed.
        spawns.retain(|spawn| {
            spawn.subagent.status == TranscriptSubagentStatus::Running
                || in_newest_turn(spawn.position)
        });

        spawns.sort_by(|a, b| {
            let running =
                |spawn: &Spawn| spawn.subagent.status == TranscriptSubagentStatus::Running;
            running(b)
                .cmp(&running(a))
                .then(b.subagent.started_ms.cmp(&a.subagent.started_ms))
                .then(b.position.cmp(&a.position))
        });
        spawns.truncate(MAX_SUBAGENTS);
        Ok((!spawns.is_empty()).then(|| spawns.into_iter().map(|spawn| spawn.subagent).collect()))
    }
}

/// The subagents one `task` result spawned, as of `at_ms`: running while
/// a background spawn (`async.state: "running"`) waits for its result, else
/// settled by the blocking spawn's own `results`.
fn spawned(details: Option<&Value>, failed: bool, at_ms: u64) -> Vec<TranscriptSubagent> {
    let Some(details) = details else {
        return Vec::new();
    };
    let progress = details
        .get("progress")
        .and_then(|progress| Vec::<Progress>::deserialize(progress).ok())
        .unwrap_or_default();
    let results = details
        .get("results")
        .and_then(|results| Vec::<SyncResult>::deserialize(results).ok())
        .unwrap_or_default();
    let background = details
        .get("async")
        .and_then(|state| state.get("state"))
        .and_then(Value::as_str)
        == Some("running");
    progress
        .into_iter()
        .filter(|item| !item.id.is_empty())
        .map(|item| {
            let mut subagent = TranscriptSubagent {
                agent: item.agent.unwrap_or_default(),
                label: item
                    .description
                    .filter(|description| !description.trim().is_empty())
                    .or_else(|| Some(item.id.clone())),
                task: item
                    .assignment
                    .or(item.task)
                    .map(|task| normalize::truncate_chars(task.trim(), normalize::DETAIL_MAX_CHARS))
                    .filter(|task| !task.is_empty()),
                status: TranscriptSubagentStatus::Running,
                started_ms: at_ms,
                finished_ms: None,
                duration_ms: None,
                last_intent: None,
                last_ms: None,
                has_transcript: false,
                id: item.id,
            };
            if background {
                return subagent;
            }
            match results.iter().find(|result| result.id == subagent.id) {
                Some(result) => {
                    let status = if result.aborted {
                        TranscriptSubagentStatus::Aborted
                    } else if result.exit_code == Some(0) {
                        TranscriptSubagentStatus::Completed
                    } else {
                        TranscriptSubagentStatus::Failed
                    };
                    // A blocking spawn's result is written when it ends.
                    let duration = result.duration_ms.unwrap_or(0);
                    subagent.started_ms = at_ms.saturating_sub(duration);
                    settle(&mut subagent, status, at_ms);
                }
                None if failed => settle(&mut subagent, TranscriptSubagentStatus::Failed, at_ms),
                // A blocking spawn without its result: as its progress says,
                // except that nothing will report it ending.
                None => match progress_status(item.status.as_deref()) {
                    TranscriptSubagentStatus::Running => {
                        settle(&mut subagent, TranscriptSubagentStatus::Unknown, at_ms);
                    }
                    status => settle(&mut subagent, status, at_ms),
                },
            }
            subagent
        })
        .collect()
}
