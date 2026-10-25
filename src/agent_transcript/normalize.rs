//! omp session lines → chat entries. Pure functions over one parsed line each;
//! the page builder in `mod.rs` decides which lines to read and in what order.

use serde::Deserialize;
use serde_json::Value;

use crate::api::schema::{
    AskQuestion, TranscriptAuthor, TranscriptNoteSource, TranscriptPlanItem, TranscriptTaskStatus,
    TranscriptTurnStatus,
};

pub(super) const DETAIL_MAX_CHARS: usize = 300;
pub(super) const NOTE_TITLE_MAX_CHARS: usize = 120;
pub(super) const NOTE_BODY_MAX_CHARS: usize = 4000;
pub(super) const SUMMARY_TEXT: &str = "Earlier messages were summarised";
/// What herdr prefixes to every prompt a guest sends through HerdrUp
/// (`crate::guest::store`), so a transcript can tell guests from the owner.
const GUEST_PROMPT_MARKER: &str = " (via HerdrUp): ";
/// Arguments that say what a tool acted on, in the order worth showing.
const DETAIL_KEYS: &[&str] = &[
    "command", "path", "file", "pattern", "url", "query", "target", "task",
];

#[derive(Deserialize)]
pub(super) struct UserLine {
    #[serde(default)]
    pub(super) timestamp: Option<String>,
    pub(super) message: UserMessage,
}

#[derive(Deserialize)]
pub(super) struct UserMessage {
    #[serde(default)]
    pub(super) timestamp: Option<u64>,
    #[serde(default)]
    pub(super) content: Value,
}

#[derive(Deserialize)]
pub(super) struct AssistantLine {
    #[serde(default)]
    pub(super) timestamp: Option<String>,
    pub(super) message: AssistantMessage,
}

#[derive(Deserialize)]
pub(super) struct AssistantMessage {
    #[serde(default)]
    pub(super) timestamp: Option<u64>,
    #[serde(rename = "completedAt", default)]
    pub(super) completed_at: Option<u64>,
    #[serde(default)]
    pub(super) content: Vec<Part>,
    #[serde(rename = "stopReason", default)]
    pub(super) stop_reason: Option<String>,
    #[serde(rename = "errorMessage", default)]
    pub(super) error_message: Option<String>,
    #[serde(default)]
    pub(super) provider: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Part {
    #[serde(rename = "type", default)]
    pub(super) kind: String,
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde(default)]
    pub(super) thinking: Option<String>,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) arguments: Option<Value>,
    #[serde(default)]
    pub(super) intent: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ResultLine {
    #[serde(default)]
    pub(super) timestamp: Option<String>,
    pub(super) message: ResultMessage,
}

#[derive(Deserialize)]
pub(super) struct ResultMessage {
    #[serde(default)]
    pub(super) timestamp: Option<u64>,
    #[serde(default)]
    pub(super) details: Option<Value>,
}

#[derive(Deserialize)]
pub(super) struct NoteLine {
    #[serde(default)]
    pub(super) timestamp: Option<String>,
    #[serde(rename = "customType", default)]
    pub(super) custom_type: Option<String>,
    #[serde(default)]
    pub(super) content: Value,
    #[serde(default)]
    pub(super) details: Option<Value>,
}

#[derive(Deserialize)]
pub(super) struct StampLine {
    #[serde(default)]
    pub(super) timestamp: Option<String>,
}

/// Milliseconds since the epoch: the message's own clock when it has one,
/// else the entry's ISO-8601 `timestamp`.
pub(super) fn ts_ms(message: Option<u64>, entry: Option<&str>) -> u64 {
    message
        .or_else(|| entry.and_then(parse_iso8601_ms))
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SS[.fff]Z` (what omp writes) to epoch milliseconds.
pub(super) fn parse_iso8601_ms(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = value.get(range)?;
        slice
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| slice.parse().ok())?
    };
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (hour, minute, second) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    let mut millis = 0;
    let mut rest = &value[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let end = fraction
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(fraction.len());
        let padded = format!("{:0<3}", &fraction[..end.min(3)]);
        millis = padded.parse::<i64>().ok()?;
        rest = &fraction[end..];
    }
    if rest != "Z" || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = ((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000) + millis;
    u64::try_from(ms).ok()
}

/// Text parts of a message's `content` (a string, or a list of parts).
pub(super) fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// Split a guest's `<name> (via HerdrUp): <text>` prompt.
pub(super) fn user_author(text: &str) -> (TranscriptAuthor, Option<String>, String) {
    if let Some((name, rest)) = text.split_once(GUEST_PROMPT_MARKER) {
        let name = name.trim();
        if !name.is_empty() && !name.contains('\n') {
            return (
                TranscriptAuthor::Guest,
                Some(name.to_owned()),
                rest.to_owned(),
            );
        }
    }
    (TranscriptAuthor::Owner, None, text.to_owned())
}

pub(super) fn is_final_stop(stop_reason: Option<&str>) -> bool {
    matches!(stop_reason, Some("stop" | "length"))
}

/// The turn status an assistant message's stop reason settles, if it settles one.
pub(super) fn turn_status(stop_reason: Option<&str>) -> Option<TranscriptTurnStatus> {
    match stop_reason {
        Some("stop" | "length") => Some(TranscriptTurnStatus::Done),
        Some("error") => Some(TranscriptTurnStatus::Error),
        Some("aborted") => Some(TranscriptTurnStatus::Aborted),
        _ => None,
    }
}

/// The step label: the call's `intent`, else its `i` argument, else the tool.
pub(super) fn step_intent(part: &Part) -> String {
    part.intent
        .as_deref()
        .filter(|intent| !intent.trim().is_empty())
        .or_else(|| {
            part.arguments
                .as_ref()
                .and_then(|arguments| arguments.get("i"))
                .and_then(Value::as_str)
                .filter(|intent| !intent.trim().is_empty())
        })
        .or(part.name.as_deref())
        .unwrap_or("tool")
        .to_owned()
}

/// What the call acted on (command, path, …), else its arguments.
pub(super) fn step_detail(arguments: Option<&Value>) -> Option<String> {
    let arguments = arguments?;
    let named = DETAIL_KEYS
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        .map(str::to_owned);
    let detail = named.or_else(|| {
        let mut rest = arguments.as_object()?.clone();
        rest.remove("i");
        (!rest.is_empty()).then(|| Value::Object(rest).to_string())
    })?;
    let detail = detail.trim();
    (!detail.is_empty()).then(|| truncate_chars(detail, DETAIL_MAX_CHARS))
}

/// The questions of an `ask` call. omp's schema is `questions: [{question,
/// options: [{label}], multi?, recommended?}]`; plain-string options and a
/// single top-level question are accepted too.
pub(super) fn ask_questions(arguments: Option<&Value>) -> Vec<AskQuestion> {
    let Some(arguments) = arguments else {
        return Vec::new();
    };
    let one = |question: &Value| -> Option<AskQuestion> {
        let text = question.get("question").and_then(Value::as_str)?;
        let options = question
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| match option {
                        Value::String(label) => Some(label.clone()),
                        other => other
                            .get("label")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(AskQuestion {
            question: text.to_owned(),
            options,
            multi: question
                .get("multi")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            recommended: question
                .get("recommended")
                .and_then(Value::as_u64)
                .and_then(|index| u32::try_from(index).ok()),
        })
    };
    match arguments.get("questions").and_then(Value::as_array) {
        Some(questions) => questions.iter().filter_map(one).collect(),
        None => one(arguments).into_iter().collect(),
    }
}

/// The selection recorded in an `ask` result's details: `selectedOptions`
/// (plus any typed-in answer), per question for multi-question asks.
pub(super) fn ask_answer(details: Option<&Value>) -> Vec<String> {
    let Some(details) = details else {
        return Vec::new();
    };
    let mut answer = Vec::new();
    let mut take = |result: &Value| {
        if let Some(selected) = result.get("selectedOptions").and_then(Value::as_array) {
            answer.extend(selected.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        if let Some(custom) = result
            .get("customInput")
            .and_then(Value::as_str)
            .filter(|custom| !custom.trim().is_empty())
        {
            answer.push(custom.to_owned());
        }
    };
    match details.get("results").and_then(Value::as_array) {
        Some(results) => results.iter().for_each(&mut take),
        None => take(details),
    }
    answer
}

/// Todo items from a `todo` result's `details.phases[].tasks[]`; abandoned
/// tasks are left out. `None` when the result carries no todo state.
pub(super) fn plan_items(details: Option<&Value>) -> Option<Vec<TranscriptPlanItem>> {
    let phases = details?.get("phases")?.as_array()?;
    Some(
        phases
            .iter()
            .flat_map(|phase| {
                let name = phase.get("name").and_then(Value::as_str);
                phase
                    .get("tasks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(move |task| {
                        let text = task.get("content").and_then(Value::as_str)?;
                        let status = match task.get("status").and_then(Value::as_str) {
                            Some("pending") => TranscriptTaskStatus::Pending,
                            Some("in_progress") => TranscriptTaskStatus::InProgress,
                            Some("completed") => TranscriptTaskStatus::Completed,
                            Some("blocked") => TranscriptTaskStatus::Blocked,
                            Some("abandoned") => return None,
                            _ => TranscriptTaskStatus::Unknown,
                        };
                        Some(TranscriptPlanItem {
                            text: text.to_owned(),
                            done: status == TranscriptTaskStatus::Completed,
                            status,
                            phase: name.map(str::to_owned),
                            blocker: task
                                .get("blocker")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        })
                    })
            })
            .collect(),
    )
}

pub(super) struct Note {
    pub(super) source: TranscriptNoteSource,
    pub(super) title: String,
    pub(super) from: Option<String>,
    pub(super) body: String,
}

/// A `custom_message` as a note card: the title is the first meaningful line
/// once the wrapper tag and the "This is information for you…" preamble are
/// gone; the body is what follows it.
pub(super) fn note(line: &NoteLine) -> Option<Note> {
    let custom_type = line.custom_type.as_deref().unwrap_or_default();
    let source = match custom_type {
        "gitmoot-inbox" => TranscriptNoteSource::Gitmoot,
        "async-result" => TranscriptNoteSource::AsyncResult,
        "advisor" => TranscriptNoteSource::Advisor,
        _ => TranscriptNoteSource::Other,
    };
    let text = content_text(&line.content);
    let mut lines = text.lines().map(str::trim_end).filter(|line| {
        let trimmed = line.trim();
        !is_wrapper_tag(trimmed) && !trimmed.contains("This is information for you")
    });
    let title = lines.by_ref().find(|line| !line.trim().is_empty())?.trim();
    let body = lines.collect::<Vec<_>>().join("\n");
    let from = line.details.as_ref().and_then(|details| {
        details
            .get("from")
            .and_then(Value::as_str)
            .or_else(|| {
                details
                    .get("notes")
                    .and_then(Value::as_array)
                    .and_then(|notes| notes.first())
                    .and_then(|note| note.get("advisor"))
                    .and_then(Value::as_str)
            })
            .filter(|from| !from.trim().is_empty())
            .map(str::to_owned)
    });
    Some(Note {
        source,
        title: truncate_chars(title, NOTE_TITLE_MAX_CHARS),
        from,
        body: truncate_chars(body.trim(), NOTE_BODY_MAX_CHARS),
    })
}

/// `<tag …>` or `</tag>` on a line of its own.
fn is_wrapper_tag(line: &str) -> bool {
    line.starts_with('<')
        && line.ends_with('>')
        && line[1..]
            .trim_start_matches('/')
            .starts_with(|c: char| c.is_ascii_alphabetic())
}

/// At most `max` characters, ending in `…` when cut.
pub(super) fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max.saturating_sub(1)) {
        Some((cut, _)) if text.len() > cut && text[cut..].chars().nth(1).is_some() => {
            format!("{}…", &text[..cut])
        }
        _ => text.to_owned(),
    }
}
