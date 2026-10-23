//! What an omp session file says about `ask` tool calls.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// One question of an `ask` call, as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskQuestion {
    pub question: String,
    pub options: Vec<String>,
    pub multi: bool,
    pub recommended: Option<usize>,
}

/// The newest `ask` call without a tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingAsk {
    pub id: String,
    pub questions: Vec<AskQuestion>,
}

impl PendingAsk {
    /// The one single-select question `agent.answer` can drive, if that is
    /// all this ask is.
    pub(crate) fn single_select(&self) -> Option<&AskQuestion> {
        match self.questions.as_slice() {
            [question] if !question.multi && !question.options.is_empty() => Some(question),
            _ => None,
        }
    }
}

pub(crate) fn pending_ask(path: &Path) -> std::io::Result<Option<PendingAsk>> {
    let text = std::fs::read_to_string(path)?;
    let mut pending: Option<PendingAsk> = None;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let message = &entry["message"];
        match message["role"].as_str() {
            Some("assistant") => {
                let calls = message["content"].as_array().into_iter().flatten();
                for part in calls {
                    if part["type"] == "toolCall" && part["name"] == "ask" {
                        if let Some(ask) = parse_ask(part) {
                            pending = Some(ask);
                        }
                    }
                }
            }
            Some("toolResult") => {
                if pending
                    .as_ref()
                    .is_some_and(|ask| message["toolCallId"].as_str() == Some(&ask.id))
                {
                    pending = None;
                }
            }
            // A new prompt means an unanswered ask was abandoned (aborted turn).
            Some("user") => pending = None,
            _ => {}
        }
    }
    Ok(pending)
}

fn parse_ask(part: &Value) -> Option<PendingAsk> {
    let id = part["id"].as_str()?.to_owned();
    let questions = part["arguments"]["questions"]
        .as_array()?
        .iter()
        .map(|question| {
            let options = question["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    option
                        .as_str()
                        .or_else(|| option["label"].as_str())
                        .map(str::to_owned)
                })
                .collect();
            AskQuestion {
                question: question["question"].as_str().unwrap_or_default().to_owned(),
                options,
                multi: question["multi"].as_bool().unwrap_or(false),
                recommended: question["recommended"]
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok()),
            }
        })
        .collect();
    Some(PendingAsk { id, questions })
}

/// Bytes in the session file now: where to look for the answer once Enter
/// has been sent.
pub(crate) fn len(path: &Path) -> std::io::Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

/// The options recorded for `ask_id` by a tool result written at or after
/// byte `offset`. A trailing line still being written is left for a later
/// look.
pub(crate) fn answer_since(
    path: &Path,
    offset: u64,
    ask_id: &str,
) -> std::io::Result<Option<Vec<String>>> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut appended = Vec::new();
    file.read_to_end(&mut appended)?;
    let complete = match appended.iter().rposition(|byte| *byte == b'\n') {
        Some(end) => &appended[..end],
        None => return Ok(None),
    };
    for line in complete.split(|byte| *byte == b'\n') {
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let message = &entry["message"];
        if message["role"] == "toolResult" && message["toolCallId"].as_str() == Some(ask_id) {
            let selected = message["details"]["selectedOptions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|option| option.as_str().map(str::to_owned))
                .collect();
            return Ok(Some(selected));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask_call(id: &str, questions: Value) -> String {
        serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": id,
                    "name": "ask",
                    "arguments": {"questions": questions},
                }],
            },
        })
        .to_string()
    }

    fn tool_result(id: &str, selected: &[&str]) -> String {
        serde_json::json!({
            "type": "message",
            "message": {
                "role": "toolResult",
                "toolCallId": id,
                "toolName": "ask",
                "details": {"selectedOptions": selected},
            },
        })
        .to_string()
    }

    fn one_question() -> Value {
        serde_json::json!([{
            "id": "q",
            "question": "Pick?",
            "options": [{"label": "A"}, {"label": "B"}],
            "recommended": 1,
        }])
    }

    fn write(lines: &[String]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), lines.join("\n") + "\n").unwrap();
        file
    }

    #[test]
    fn newest_unanswered_ask_is_pending() {
        let file = write(&[
            ask_call("call_1", one_question()),
            tool_result("call_1", &["A"]),
            ask_call("call_2", one_question()),
        ]);
        let pending = pending_ask(file.path()).unwrap().unwrap();
        assert_eq!(pending.id, "call_2");
        assert_eq!(
            pending.single_select(),
            Some(&AskQuestion {
                question: "Pick?".into(),
                options: vec!["A".into(), "B".into()],
                multi: false,
                recommended: Some(1),
            })
        );
    }

    #[test]
    fn answered_or_abandoned_ask_is_not_pending() {
        let answered = write(&[
            ask_call("call_1", one_question()),
            tool_result("call_1", &["A"]),
        ]);
        assert_eq!(pending_ask(answered.path()).unwrap(), None);
        let abandoned = write(&[
            ask_call("call_1", one_question()),
            serde_json::json!({"type": "message", "message": {"role": "user", "content": "hi"}})
                .to_string(),
        ]);
        assert_eq!(pending_ask(abandoned.path()).unwrap(), None);
    }

    #[test]
    fn multi_select_and_multi_question_asks_are_not_single_select() {
        let multi = PendingAsk {
            id: "x".into(),
            questions: vec![AskQuestion {
                question: "Pick?".into(),
                options: vec!["A".into()],
                multi: true,
                recommended: None,
            }],
        };
        assert_eq!(multi.single_select(), None);
        let single = AskQuestion {
            multi: false,
            ..multi.questions[0].clone()
        };
        let two = PendingAsk {
            id: "x".into(),
            questions: vec![single.clone(), single],
        };
        assert_eq!(two.single_select(), None);
    }

    #[test]
    fn answer_since_reads_only_complete_appended_lines() {
        let file = write(&[ask_call("call_1", one_question())]);
        let offset = len(file.path()).unwrap();
        assert_eq!(answer_since(file.path(), offset, "call_1").unwrap(), None);
        let result = tool_result("call_1", &["B"]);
        let mut contents = std::fs::read_to_string(file.path()).unwrap();
        contents.push_str(&result);
        std::fs::write(file.path(), &contents).unwrap();
        assert_eq!(
            answer_since(file.path(), offset, "call_1").unwrap(),
            None,
            "a line without its newline is still being written"
        );
        contents.push('\n');
        std::fs::write(file.path(), &contents).unwrap();
        assert_eq!(
            answer_since(file.path(), offset, "call_1").unwrap(),
            Some(vec!["B".to_owned()])
        );
        assert_eq!(answer_since(file.path(), offset, "call_2").unwrap(), None);
    }
}
