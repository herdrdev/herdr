//! `agent.transcript`: an omp agent's conversation, read from the session
//! JSONL omp appends as it runs, normalised into chat entries and paged by
//! turns from the newest end.
//!
//! The file is a tree (`id`/`parentId`); only the active branch, the parent
//! chain of the newest entry, is shown. A per-file index (see `index`) keeps
//! one small record per entry so a page reads only the lines it renders.

mod history;
mod index;
mod normalize;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::HashSet;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use base64::Engine as _;

use crate::api::schema::PendingAsk;
use crate::api::schema::{
    TranscriptEntry, TranscriptModel, TranscriptPlanItem, TranscriptRecap, TranscriptStepStatus,
    TranscriptTurnStatus, TranscriptWorking,
};
use index::{read_line, Kind, SessionIndex};
use normalize::{AssistantLine, NoteLine, ResultLine, StampLine, UserLine, SUMMARY_TEXT};

pub(crate) const DEFAULT_TURNS: u32 = 20;
pub(crate) const MAX_TURNS: u32 = 50;
/// Rendered entries one page reads at most. A turn longer than this (hours of
/// autonomous work) is split, so a page never parses a whole giant session.
const PAGE_RECORD_BUDGET: usize = 1500;
/// Session indexes kept warm; each is a few MB for a very long session.
const CACHED_SESSIONS: usize = 8;

#[derive(Debug, Clone, Default)]
pub(crate) struct PageRequest {
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
    pub(crate) limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Page {
    pub(crate) session_id: String,
    pub(crate) entries: Vec<TranscriptEntry>,
    pub(crate) before: Option<String>,
    pub(crate) after: String,
    pub(crate) pending_ask: Option<PendingAsk>,
    pub(crate) working: Option<TranscriptWorking>,
    pub(crate) model: Option<TranscriptModel>,
    pub(crate) plan: Option<Vec<TranscriptPlanItem>>,
    pub(crate) recap: Option<TranscriptRecap>,
}

#[derive(Debug)]
pub(crate) enum TranscriptError {
    /// The session file is missing or unreadable.
    Unavailable(std::io::Error),
    /// The cursor no longer names this file's active branch.
    CursorExpired,
    InvalidRequest(&'static str),
}

impl TranscriptError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "transcript_unavailable",
            Self::CursorExpired => "cursor_expired",
            Self::InvalidRequest(_) => "invalid_request",
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Unavailable(error) => format!("the agent's session file is unreadable: {error}"),
            Self::CursorExpired => {
                "the transcript changed under this cursor; reload the newest page".into()
            }
            Self::InvalidRequest(message) => (*message).into(),
        }
    }
}

impl From<std::io::Error> for TranscriptError {
    fn from(error: std::io::Error) -> Self {
        Self::Unavailable(error)
    }
}

static CACHE: LazyLock<Mutex<Vec<Arc<Mutex<SessionIndex>>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// The warm index for `path`, most recently used last.
fn cached_index(path: &Path) -> Arc<Mutex<SessionIndex>> {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let found = cache.iter().position(|index| {
        index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .path()
            == path
    });
    let index = match found {
        Some(position) => cache.remove(position),
        None => Arc::new(Mutex::new(SessionIndex::new(path))),
    };
    cache.push(index.clone());
    if cache.len() > CACHED_SESSIONS {
        cache.remove(0);
    }
    index
}

fn with_index<T>(
    path: &Path,
    read: impl FnOnce(&View<'_>) -> Result<T, TranscriptError>,
) -> Result<T, TranscriptError> {
    let index = cached_index(path);
    let mut index = index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let file = match index.refresh() {
        Ok(file) => file,
        Err(error) => {
            // Forget a file that vanished so a later one at the path starts clean.
            *index = SessionIndex::new(path);
            return Err(error.into());
        }
    };
    let view = View {
        index: &index,
        branch: index.branch(),
        file: Mutex::new(file),
    };
    read(&view)
}

/// One page of `path`'s conversation, as the owner sees it.
pub(crate) fn read_page(path: &Path, request: &PageRequest) -> Result<Page, TranscriptError> {
    if request.before.is_some() && request.after.is_some() {
        return Err(TranscriptError::InvalidRequest(
            "pass either before or after, not both",
        ));
    }
    let limit = request.limit.unwrap_or(DEFAULT_TURNS).clamp(1, MAX_TURNS) as usize;
    with_index(path, |view| view.page(request, limit))
}

/// The newest `ask` on the active branch still waiting for its answer.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "agent.answer and its push (herdr#308/#309) call it"
    )
)]
pub(crate) fn pending_ask(session_path: &Path) -> std::io::Result<Option<PendingAsk>> {
    match with_index(session_path, |view| Ok(view.live_state()?.0)) {
        Ok(ask) => Ok(ask),
        Err(TranscriptError::Unavailable(error)) => Err(error),
        Err(other) => Err(std::io::Error::other(other.message())),
    }
}

/// The selection recorded for the `ask` call `ask_id`, once its result is
/// written; `None` while it is unanswered (or not an ask in this file).
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "agent.answer and its push (herdr#308/#309) call it"
    )
)]
pub(crate) fn ask_result(
    session_path: &Path,
    ask_id: &str,
) -> std::io::Result<Option<Vec<String>>> {
    let read = with_index(session_path, |view| {
        let Some(result) = view.index.result_for(ask_id) else {
            return Ok(None);
        };
        let line: ResultLine = view.parse(result)?.ok_or(TranscriptError::CursorExpired)?;
        Ok(Some(normalize::ask_answer(line.message.details.as_ref())))
    });
    match read {
        Ok(answer) => Ok(answer),
        Err(TranscriptError::Unavailable(error)) => Err(error),
        Err(other) => Err(std::io::Error::other(other.message())),
    }
}

/// Opaque cursors. `before` names the first record of a page. `after` holds
/// the number of records seen and the newest active-branch record among them
/// (`tip`), which must still be on the active branch: after a rewind the
/// client's view is gone and it reloads.
#[derive(Debug, PartialEq, Eq)]
enum Cursor {
    Before {
        record: u32,
        id: String,
    },
    After {
        seen: u32,
        tip: Option<(u32, String)>,
    },
}

impl Cursor {
    fn encode(&self, session_id: &str) -> String {
        let raw = match self {
            Self::Before { record, id } => format!("1\nb\n{session_id}\n{record}\n{id}"),
            Self::After { seen, tip: None } => format!("1\na\n{session_id}\n{seen}\n"),
            Self::After {
                seen,
                tip: Some((tip, id)),
            } => format!("1\na\n{session_id}\n{seen}\n{tip} {id}"),
        };
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    }

    /// Any cursor this file did not issue (another session, a garbled or
    /// foreign token) is expired: the client restarts from the newest page.
    fn decode(token: &str, session_id: &str) -> Result<Self, TranscriptError> {
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .ok()
            .and_then(|raw| String::from_utf8(raw).ok())
            .ok_or(TranscriptError::CursorExpired)?;
        let mut fields = raw.splitn(5, '\n');
        let (Some("1"), Some(kind), Some(session), Some(number), Some(id)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(TranscriptError::CursorExpired);
        };
        if session != session_id {
            return Err(TranscriptError::CursorExpired);
        }
        let number = number.parse().map_err(|_| TranscriptError::CursorExpired)?;
        match kind {
            "b" => Ok(Self::Before {
                record: number,
                id: id.to_owned(),
            }),
            "a" if id.is_empty() => Ok(Self::After {
                seen: number,
                tip: None,
            }),
            "a" => {
                let (tip, id) = id.split_once(' ').ok_or(TranscriptError::CursorExpired)?;
                let tip = tip.parse().map_err(|_| TranscriptError::CursorExpired)?;
                Ok(Self::After {
                    seen: number,
                    tip: Some((tip, id.to_owned())),
                })
            }
            _ => Err(TranscriptError::CursorExpired),
        }
    }
}

struct View<'a> {
    index: &'a SessionIndex,
    /// Visible active branch: record indices, root first.
    branch: &'a [u32],
    file: Mutex<File>,
}

impl View<'_> {
    fn session_id(&self) -> &str {
        self.index.session_id.as_deref().unwrap_or_default()
    }

    fn kind(&self, position: usize) -> Kind {
        self.index.record(self.branch[position]).kind
    }

    /// Branch position of `record`, if it is on the visible branch.
    fn position_of(&self, record: u32) -> Option<usize> {
        self.branch.binary_search(&record).ok()
    }

    /// Parse one record's line; `None` when it no longer parses as `T`.
    fn parse<T: serde::de::DeserializeOwned>(
        &self,
        record: u32,
    ) -> Result<Option<T>, TranscriptError> {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let line = read_line(&mut file, self.index.record(record))?;
        Ok(serde_json::from_slice(&line).ok())
    }

    fn page(&self, request: &PageRequest, limit: usize) -> Result<Page, TranscriptError> {
        let session_id = self.session_id().to_owned();
        let (start, end, changed_since) = match (&request.before, &request.after) {
            (Some(token), _) => {
                let Cursor::Before { record, id } = Cursor::decode(token, &session_id)? else {
                    return Err(TranscriptError::CursorExpired);
                };
                let end = self
                    .valid_record(record, &id)
                    .and_then(|record| self.position_of(record))
                    .ok_or(TranscriptError::CursorExpired)?;
                (self.page_start(end, limit), end, None)
            }
            (None, Some(token)) => {
                let Cursor::After { seen, tip } = Cursor::decode(token, &session_id)? else {
                    return Err(TranscriptError::CursorExpired);
                };
                if seen as usize > self.index.records.len() {
                    return Err(TranscriptError::CursorExpired);
                }
                if let Some((tip, id)) = tip {
                    self.valid_record(tip, &id)
                        .and_then(|record| self.position_of(record))
                        .ok_or(TranscriptError::CursorExpired)?;
                }
                let start = self.branch.partition_point(|&record| record < seen);
                (start, self.budget_end(start), Some(seen))
            }
            (None, None) => {
                let end = self.branch.len();
                (self.page_start(end, limit), end, None)
            }
        };
        let mut entries = Vec::new();
        if let Some(seen) = changed_since {
            self.resend_changed(seen, start, &mut entries)?;
        }
        self.render(start, end, &mut entries)?;
        let before = (changed_since.is_none() && start > 0).then(|| {
            let record = self.branch[start];
            Cursor::Before {
                record,
                id: self.index.record(record).id.to_string(),
            }
            .encode(&session_id)
        });
        // An `after` page that hit the budget continues from where it stopped.
        let seen = match changed_since {
            Some(_) if end < self.branch.len() => self.branch[end],
            _ => self.index.records.len() as u32,
        };
        let tip = self.branch[..self.branch.partition_point(|&record| record < seen)]
            .last()
            .map(|&tip| (tip, self.index.record(tip).id.to_string()));
        let after = Cursor::After { seen, tip }.encode(&session_id);
        let (pending_ask, working) = self.live_state()?;
        Ok(Page {
            session_id,
            entries,
            before,
            after,
            pending_ask,
            working,
            model: self.current_model()?,
            plan: self.current_plan()?,
            recap: self.current_recap()?,
        })
    }

    fn current_model(&self) -> Result<Option<TranscriptModel>, TranscriptError> {
        let mut read_assistant = false;
        for record in self.index.model_records() {
            if self.index.record(record).kind == Kind::ModelChange {
                #[derive(serde::Deserialize)]
                struct ModelChange {
                    model: String,
                }
                return Ok(self.parse::<ModelChange>(record)?.and_then(|change| {
                    let (provider, id) = change.model.split_once('/')?;
                    (!provider.is_empty() && !id.is_empty()).then(|| TranscriptModel {
                        provider: provider.to_owned(),
                        id: id.to_owned(),
                    })
                }));
            }
            if !read_assistant {
                read_assistant = true;
                if let Some(model) = self.parse::<AssistantLine>(record)?.and_then(|line| {
                    let provider = line.message.provider?;
                    let id = line.message.model?;
                    (!provider.is_empty() && !id.is_empty())
                        .then_some(TranscriptModel { provider, id })
                }) {
                    return Ok(Some(model));
                }
            }
        }
        Ok(None)
    }

    fn current_plan(&self) -> Result<Option<Vec<TranscriptPlanItem>>, TranscriptError> {
        let record = self
            .branch
            .iter()
            .rev()
            .copied()
            .find(|&record| self.index.record(record).kind == Kind::TodoResult);
        match record {
            Some(record) => Ok(self
                .parse::<ResultLine>(record)?
                .and_then(|line| normalize::plan_items(line.message.details.as_ref()))),
            None => Ok(None),
        }
    }

    /// The completion recap for the newest turn. A session whose omp writes
    /// turn-bound `completion-recap` records is read from those alone; older omp
    /// only saves recaps to its `history.db`, matched to the turn by time.
    fn current_recap(&self) -> Result<Option<TranscriptRecap>, TranscriptError> {
        if self
            .branch
            .iter()
            .any(|&record| self.index.record(record).kind == Kind::Recap)
        {
            self.turn_bound_recap()
        } else {
            self.history_recap()
        }
    }

    /// A `history.db` recap is current only when written after the newest
    /// conversational entry and that entry is the assistant's reply. Rows keep
    /// whole seconds, so an entry in the recap's own second counts as newer.
    fn history_recap(&self) -> Result<Option<TranscriptRecap>, TranscriptError> {
        let Some(&latest) = self.branch.iter().rev().find(|&&record| {
            matches!(
                self.index.record(record).kind,
                Kind::User
                    | Kind::Assistant
                    | Kind::ToolResult
                    | Kind::TodoResult
                    | Kind::Compaction
                    | Kind::ResetBoundary
            )
        }) else {
            return Ok(None);
        };
        let entry = self.index.record(latest);
        if entry.kind != Kind::Assistant {
            return Ok(None);
        }
        let Some(latest_ms) = self
            .parse::<StampLine>(latest)?
            .and_then(|line| line.timestamp)
            .and_then(|stamp| normalize::parse_iso8601_ms(&stamp))
        else {
            return Ok(None);
        };
        let Some(recap) = history::latest_recap(self.index.path(), self.session_id()) else {
            return Ok(None);
        };
        let text = recap.text.trim();
        Ok(
            (latest_ms < recap.created_at_ms && !text.is_empty()).then(|| TranscriptRecap {
                turn_id: entry.id.to_string(),
                text: text.to_owned(),
            }),
        )
    }

    fn turn_bound_recap(&self) -> Result<Option<TranscriptRecap>, TranscriptError> {
        #[derive(serde::Deserialize)]
        struct RecapLine {
            data: RecapData,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RecapData {
            version: u32,
            session_id: String,
            turn_id: String,
            state: String,
            #[serde(default)]
            text: Option<String>,
        }
        for &record in self.branch.iter().rev() {
            match self.index.record(record).kind {
                Kind::Recap => {
                    let Some(line) = self.parse::<RecapLine>(record)? else {
                        return Ok(None);
                    };
                    let data = line.data;
                    if data.version != 1
                        || data.state != "available"
                        || data.session_id != self.session_id()
                    {
                        return Ok(None);
                    }
                    let latest_assistant = self
                        .branch
                        .iter()
                        .rev()
                        .map(|&id| self.index.record(id))
                        .find(|entry| {
                            matches!(
                                entry.kind,
                                Kind::User
                                    | Kind::Assistant
                                    | Kind::ToolResult
                                    | Kind::TodoResult
                                    | Kind::Compaction
                                    | Kind::ResetBoundary
                            )
                        })
                        .filter(|entry| entry.kind == Kind::Assistant)
                        .map(|entry| entry.id.as_ref());
                    if latest_assistant != Some(data.turn_id.as_str()) {
                        return Ok(None);
                    }
                    return Ok(data
                        .text
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| TranscriptRecap {
                            turn_id: data.turn_id,
                            text,
                        }));
                }
                Kind::User
                | Kind::Assistant
                | Kind::ToolResult
                | Kind::TodoResult
                | Kind::Compaction
                | Kind::ResetBoundary => return Ok(None),
                _ => {}
            }
        }
        Ok(None)
    }

    fn valid_record(&self, record: u32, id: &str) -> Option<u32> {
        let found = self.index.records.get(record as usize)?;
        (*found.id == *id).then_some(record)
    }

    /// Where a page ending before `end` starts: `limit` turns back, each turn
    /// starting at a user entry, but never more than the record budget. A
    /// single turn over budget is split mid-turn.
    fn page_start(&self, end: usize, limit: usize) -> usize {
        let mut turns = 0;
        let mut cost = 0;
        let mut last_turn_start = None;
        let mut position = end;
        while position > 0 {
            position -= 1;
            let kind = self.kind(position);
            if kind.is_rendered() {
                cost += 1;
            }
            if kind == Kind::User {
                turns += 1;
                last_turn_start = Some(position);
                if turns == limit {
                    return position;
                }
            }
            if cost >= PAGE_RECORD_BUDGET {
                return last_turn_start.unwrap_or(position);
            }
        }
        0
    }

    /// The end of an `after` page starting at `start`, within the budget.
    fn budget_end(&self, start: usize) -> usize {
        let mut cost = 0;
        for position in start..self.branch.len() {
            if self.kind(position).is_rendered() {
                cost += 1;
                if cost > PAGE_RECORD_BUDGET {
                    return position;
                }
            }
        }
        self.branch.len()
    }

    /// The turn (its user entry, or the branch start) `position` belongs to.
    fn turn_start(&self, position: usize) -> usize {
        (0..=position)
            .rev()
            .find(|&candidate| self.kind(candidate) == Kind::User)
            .unwrap_or(0)
    }

    fn turn_end(&self, position: usize) -> usize {
        (position + 1..self.branch.len())
            .find(|&candidate| self.kind(candidate) == Kind::User)
            .unwrap_or(self.branch.len())
    }

    /// omp may continue after a stop (retries, todo nudges, a pending async
    /// wake): the turn settled only if what comes next is not more model
    /// output, i.e. the user, a note that wakes the agent, an exit, or nothing.
    fn continues_after(&self, position: usize) -> bool {
        (position + 1..self.branch.len())
            .map(|candidate| self.kind(candidate))
            .find(|kind| {
                matches!(
                    kind,
                    Kind::User | Kind::Assistant | Kind::Note | Kind::SessionExit
                )
            })
            == Some(Kind::Assistant)
    }

    fn assistant_after(&self, position: usize) -> bool {
        (position + 1..self.branch.len())
            .any(|candidate| matches!(self.kind(candidate), Kind::Assistant | Kind::SessionExit))
    }

    fn render(
        &self,
        start: usize,
        end: usize,
        entries: &mut Vec<TranscriptEntry>,
    ) -> Result<(), TranscriptError> {
        let mut planned = HashSet::new();
        for position in start..end {
            let record = self.branch[position];
            let id = &*self.index.record(record).id;
            match self.kind(position) {
                Kind::User => {
                    if let Some(line) = self.parse::<UserLine>(record)? {
                        let text = normalize::content_text(&line.message.content);
                        if text.trim().is_empty() {
                            continue;
                        }
                        let (author, author_name, text) = normalize::user_author(&text);
                        entries.push(TranscriptEntry::User {
                            id: id.to_owned(),
                            ts_ms: normalize::ts_ms(
                                line.message.timestamp,
                                line.timestamp.as_deref(),
                            ),
                            text,
                            author,
                            author_name,
                        });
                    }
                }
                Kind::Assistant => self.render_assistant(position, None, entries)?,
                Kind::TodoResult => {
                    let turn = self.turn_start(position);
                    if planned.insert(turn) {
                        entries.extend(self.plan(turn)?);
                    }
                }
                Kind::Note => {
                    if let Some(line) = self.parse::<NoteLine>(record)? {
                        if let Some(note) = normalize::note(&line) {
                            entries.push(TranscriptEntry::Note {
                                id: id.to_owned(),
                                ts_ms: normalize::ts_ms(None, line.timestamp.as_deref()),
                                source: note.source,
                                title: note.title,
                                from: note.from,
                                body: note.body,
                            });
                        }
                    }
                }
                Kind::Compaction => {
                    let ts_ms = self
                        .parse::<StampLine>(record)?
                        .map(|line| normalize::ts_ms(None, line.timestamp.as_deref()))
                        .unwrap_or(0);
                    entries.push(TranscriptEntry::Summary {
                        id: id.to_owned(),
                        ts_ms,
                        text: SUMMARY_TEXT.to_owned(),
                    });
                }
                Kind::ToolResult
                | Kind::ResetBoundary
                | Kind::SessionExit
                | Kind::Hidden
                | Kind::ModelChange
                | Kind::Recap => {}
            }
        }
        Ok(())
    }

    /// An assistant message: its thinking, its reply, one step per tool call
    /// (an `ask` call as an ask card), then the turn end if it settled the
    /// turn. With `changed_since`, only the steps and asks whose result
    /// arrived at or after that record (their status changed).
    fn render_assistant(
        &self,
        position: usize,
        changed_since: Option<u32>,
        entries: &mut Vec<TranscriptEntry>,
    ) -> Result<(), TranscriptError> {
        let record = self.branch[position];
        let id = &*self.index.record(record).id;
        let Some(line) = self.parse::<AssistantLine>(record)? else {
            return Ok(());
        };
        let message = &line.message;
        let ts_ms = normalize::ts_ms(message.timestamp, line.timestamp.as_deref());
        let resend_only = changed_since.is_some();
        if !resend_only {
            let thinking = message
                .content
                .iter()
                .filter(|part| part.kind == "thinking")
                .filter_map(|part| part.thinking.as_deref())
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !thinking.is_empty() {
                entries.push(TranscriptEntry::Thinking {
                    id: format!("{id}:thinking"),
                    ts_ms,
                    text: thinking,
                });
            }
            let reply = message
                .content
                .iter()
                .filter(|part| part.kind == "text")
                .filter_map(|part| part.text.as_deref())
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !reply.is_empty() {
                entries.push(TranscriptEntry::Reply {
                    id: format!("{id}:reply"),
                    ts_ms,
                    text: reply,
                    is_final: normalize::is_final_stop(message.stop_reason.as_deref()),
                });
            }
        }
        let lost = self.assistant_after(position);
        for part in message
            .content
            .iter()
            .filter(|part| part.kind == "toolCall")
        {
            let (Some(call_id), Some(tool)) = (part.id.as_deref(), part.name.as_deref()) else {
                continue;
            };
            if tool == "todo" {
                continue;
            }
            let result = self.index.result_for(call_id);
            if let Some(seen) = changed_since {
                // Re-send only what moved since the client's cursor: a result
                // that landed after it, or a call that will never get one.
                let moved = match result {
                    Some(result) => result >= seen,
                    None => lost,
                };
                if !moved {
                    continue;
                }
            }
            if tool == "ask" {
                let answer = match result {
                    Some(result) => Some(
                        self.parse::<ResultLine>(result)?
                            .map(|line| normalize::ask_answer(line.message.details.as_ref()))
                            .unwrap_or_default(),
                    ),
                    None => None,
                };
                entries.push(TranscriptEntry::Ask {
                    id: call_id.to_owned(),
                    ts_ms,
                    questions: normalize::ask_questions(part.arguments.as_ref()),
                    answer,
                });
                continue;
            }
            let status = match result {
                Some(result) if self.index.record(result).failed => TranscriptStepStatus::Error,
                Some(_) => TranscriptStepStatus::Ok,
                // A later model message or an exit means the call never finished.
                None if lost => TranscriptStepStatus::Error,
                None => TranscriptStepStatus::Running,
            };
            entries.push(TranscriptEntry::Step {
                id: format!("{id}:{call_id}"),
                ts_ms,
                tool: tool.to_owned(),
                intent: normalize::step_intent(part),
                status,
                detail: normalize::step_detail(part.arguments.as_ref()),
            });
        }
        if resend_only {
            return Ok(());
        }
        if let Some(status) = normalize::turn_status(message.stop_reason.as_deref()) {
            if !self.continues_after(position) {
                let error = (status == TranscriptTurnStatus::Error)
                    .then(|| message.error_message.clone())
                    .flatten();
                entries.push(TranscriptEntry::TurnEnd {
                    id: format!("{id}:end"),
                    ts_ms: message.completed_at.unwrap_or(ts_ms),
                    status,
                    error,
                });
            }
        }
        Ok(())
    }

    /// The turn's todo list: one entry per turn (id from the turn's first
    /// record), holding the newest todo state the turn has written so far.
    fn plan(&self, turn: usize) -> Result<Option<TranscriptEntry>, TranscriptError> {
        let end = self.turn_end(turn);
        for position in (turn..end).rev() {
            if self.kind(position) != Kind::TodoResult {
                continue;
            }
            let Some(line) = self.parse::<ResultLine>(self.branch[position])? else {
                continue;
            };
            let Some(items) = normalize::plan_items(line.message.details.as_ref()) else {
                continue;
            };
            let turn_id = &self.index.record(self.branch[turn]).id;
            return Ok(Some(TranscriptEntry::Plan {
                id: format!("{turn_id}:plan"),
                ts_ms: normalize::ts_ms(line.message.timestamp, line.timestamp.as_deref()),
                items,
            }));
        }
        Ok(None)
    }

    /// Entries emitted before the client's `after` cursor whose state moved
    /// since: the steps and asks of the newest earlier model message (its
    /// results land after it). A changed plan needs nothing here: its new todo
    /// result is in the range and re-renders the plan under the turn's id.
    fn resend_changed(
        &self,
        seen: u32,
        start: usize,
        entries: &mut Vec<TranscriptEntry>,
    ) -> Result<(), TranscriptError> {
        if let Some(position) = (0..start)
            .rev()
            .find(|&position| self.kind(position) == Kind::Assistant)
        {
            self.render_assistant(position, Some(seen), entries)?;
        }
        Ok(())
    }

    /// The ask waiting for an answer and the step running now, both from the
    /// newest model message (an exit after it cancels both).
    fn live_state(
        &self,
    ) -> Result<(Option<PendingAsk>, Option<TranscriptWorking>), TranscriptError> {
        let Some(position) = (0..self.branch.len())
            .rev()
            .find(|&position| matches!(self.kind(position), Kind::Assistant | Kind::SessionExit))
        else {
            return Ok((None, None));
        };
        if self.kind(position) == Kind::SessionExit {
            return Ok((None, None));
        }
        let Some(line) = self.parse::<AssistantLine>(self.branch[position])? else {
            return Ok((None, None));
        };
        let message = &line.message;
        let open = message.content.iter().filter(|part| {
            part.kind == "toolCall"
                && part
                    .id
                    .as_deref()
                    .is_some_and(|call_id| self.index.result_for(call_id).is_none())
        });
        // A prompt typed after the ask means the user moved on without it.
        let abandoned =
            (position + 1..self.branch.len()).any(|later| self.kind(later) == Kind::User);
        let mut ask = None;
        let mut running = None;
        for part in open {
            if part.name.as_deref() == Some("ask") {
                if abandoned {
                    continue;
                }
                let questions = normalize::ask_questions(part.arguments.as_ref());
                let answerable = matches!(questions.as_slice(), [only] if !only.multi && !only.options.is_empty());
                ask = Some(PendingAsk {
                    id: part.id.clone().unwrap_or_default(),
                    questions,
                    answerable,
                });
            } else {
                running = Some(part);
            }
        }
        let working = match (&ask, running) {
            (None, Some(part)) => Some(TranscriptWorking {
                intent: normalize::step_intent(part),
                tool: part.name.clone().unwrap_or_default(),
                since_ms: message.completed_at.unwrap_or_else(|| {
                    normalize::ts_ms(message.timestamp, line.timestamp.as_deref())
                }),
            }),
            _ => None,
        };
        Ok((ask, working))
    }
}
