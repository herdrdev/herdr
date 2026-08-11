//! Claude main-session JSONL adapter. User/assistant envelopes contain nested
//! message content blocks; `thinking` blocks are excluded and `tool_use`/
//! `tool_result` records share the provider tool id.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_conversation::{
    cap_text, safe_display_path, tool_command_preview, validate_under_root, NativeRecord,
    ProviderAdapter, SourceFingerprint, TranscriptError, MAX_TEXT_BYTES,
};
use crate::api::schema::conversations::{
    AssistantMessagePhase, CompletionState, ConversationItemPayload, ToolStatus, TurnStateKind,
};

pub struct ClaudeAdapter;

impl ProviderAdapter for ClaudeAdapter {
    fn provider_name(&self) -> &'static str {
        "claude"
    }

    fn validate_source(&self, path: &Path) -> Result<SourceFingerprint, TranscriptError> {
        let roots = crate::agent_conversation::provider_roots("claude");
        let refs: Vec<_> = roots.iter().map(PathBuf::as_path).collect();
        validate_under_root(path, &refs)
    }

    fn normalize_line(&self, line: &str) -> Vec<NativeRecord> {
        normalize_claude_line(line)
    }
}

fn normalize_claude_line(line: &str) -> Vec<NativeRecord> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || value.get("isMeta").and_then(Value::as_bool) == Some(true)
    {
        return Vec::new();
    }
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let id = value
        .get("uuid")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let parent = value
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .or_else(|| value.get("timestamp").and_then(Value::as_u64));
    let turn_id = value
        .get("turn_id")
        .or_else(|| value.get("turnId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut records = match kind {
        "user" => normalize_user(&value, id, parent, timestamp),
        "assistant" => normalize_assistant(&value, id, parent, timestamp),
        "system" => normalize_system(&value, id, timestamp),
        "result" => normalize_result(&value, id, timestamp),
        _ => Vec::new(),
    };
    for record in &mut records {
        if record.turn_id.is_none() {
            record.turn_id = turn_id.clone();
        }
    }
    records
}

fn normalize_user(
    value: &Value,
    id: Option<String>,
    parent: Option<String>,
    timestamp: Option<u64>,
) -> Vec<NativeRecord> {
    let message = value.get("message").unwrap_or(value);
    let content = message.get("content").unwrap_or(&Value::Null);
    if let Some(blocks) = content.as_array() {
        let mut records = Vec::new();
        let mut text = String::new();
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                let tool_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let failed = block_failed(block);
                let mut tool_record = record(
                    tool_id,
                    parent.clone(),
                    timestamp,
                    ConversationItemPayload::ToolActivity {
                        action: "tool".into(),
                        label: if failed {
                            "failed".into()
                        } else {
                            "completed".into()
                        },
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        preview: None,
                        detail: None,
                        duration_ms: None,
                        paths: Vec::new(),
                    },
                );
                tool_record.entry_id = id.clone();
                records.push(tool_record);
            } else if block.get("type").and_then(Value::as_str) == Some("text") {
                text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""));
            }
        }
        if !text.is_empty() {
            records.push(record(
                id,
                parent,
                timestamp,
                ConversationItemPayload::UserMessage {
                    text: cap_text(&text, MAX_TEXT_BYTES),
                    attachments: Vec::new(),
                },
            ));
        }
        records
    } else {
        let text = content.as_str().unwrap_or("");
        (!text.is_empty())
            .then(|| {
                record(
                    id,
                    parent,
                    timestamp,
                    ConversationItemPayload::UserMessage {
                        text: cap_text(text, MAX_TEXT_BYTES),
                        attachments: Vec::new(),
                    },
                )
            })
            .into_iter()
            .collect()
    }
}

fn normalize_assistant(
    value: &Value,
    id: Option<String>,
    parent: Option<String>,
    timestamp: Option<u64>,
) -> Vec<NativeRecord> {
    let message = value.get("message").unwrap_or(value);
    let blocks = message.get("content").and_then(Value::as_array);
    let mut text = String::new();
    let mut records = Vec::new();
    if let Some(blocks) = blocks {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""))
                }
                Some("tool_use") => {
                    let tool_id = block.get("id").and_then(Value::as_str).map(str::to_string);
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = block.get("input");
                    let file_path = input
                        .and_then(|input| {
                            input
                                .get("file_path")
                                .or_else(|| input.get("notebook_path"))
                        })
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    records.push(record(
                        tool_id.clone(),
                        parent.clone(),
                        timestamp,
                        ConversationItemPayload::ToolActivity {
                            action: cap_text(name, MAX_TEXT_BYTES),
                            label: cap_text(name, MAX_TEXT_BYTES),
                            status: ToolStatus::Running,
                            preview: tool_command_preview(name, input),
                            detail: None,
                            duration_ms: None,
                            paths: if matches!(name, "Edit" | "Write" | "NotebookEdit") {
                                file_path
                                    .as_deref()
                                    .and_then(safe_display_path)
                                    .into_iter()
                                    .collect()
                            } else {
                                Vec::new()
                            },
                        },
                    ));
                }
                Some("thinking") => {}
                _ => {}
            }
        }
    } else if let Some(value) = message.get("content").and_then(Value::as_str) {
        text.push_str(value);
    }
    if !text.is_empty() {
        let stop_reason = message
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        let aborted_mid_stream =
            value.get("isAbortedMidStream").and_then(Value::as_bool) == Some(true);
        let api_error = value.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true);
        let state = if aborted_mid_stream || stop_reason == "aborted" {
            CompletionState::Interrupted
        } else if api_error || stop_reason == "error" {
            CompletionState::Failed
        } else {
            CompletionState::Completed
        };
        let phase = if matches!(
            stop_reason,
            "end_turn" | "stop_sequence" | "max_tokens" | "aborted" | "error"
        ) || aborted_mid_stream
            || api_error
        {
            AssistantMessagePhase::Final
        } else {
            AssistantMessagePhase::Commentary
        };
        records.push(record(
            id,
            parent,
            timestamp,
            ConversationItemPayload::AssistantMessage {
                phase,
                text: cap_text(&text, MAX_TEXT_BYTES),
                state,
            },
        ));
    }
    records
}

fn normalize_system(
    value: &Value,
    id: Option<String>,
    timestamp: Option<u64>,
) -> Vec<NativeRecord> {
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
    let state = match subtype {
        "turn_duration" | "stop" => TurnStateKind::Completed,
        "error" | "api_error" => TurnStateKind::Failed,
        _ => return Vec::new(),
    };
    vec![record(
        id,
        None,
        timestamp,
        ConversationItemPayload::TurnState {
            state,
            started_ms: None,
            duration_ms: value
                .get("durationMs")
                .or_else(|| value.get("duration_ms"))
                .and_then(Value::as_u64),
            error: value
                .get("error")
                .and_then(|error| {
                    error
                        .as_str()
                        .or_else(|| error.get("message").and_then(Value::as_str))
                })
                .map(|text| cap_text(text, MAX_TEXT_BYTES)),
        },
    )]
}

fn normalize_result(
    value: &Value,
    id: Option<String>,
    timestamp: Option<u64>,
) -> Vec<NativeRecord> {
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
    let state = match subtype {
        "success" => TurnStateKind::Completed,
        "error" | "failed" => TurnStateKind::Failed,
        "interrupted" => TurnStateKind::Interrupted,
        _ => return Vec::new(),
    };
    vec![record(
        id,
        None,
        timestamp,
        ConversationItemPayload::TurnState {
            state,
            started_ms: None,
            duration_ms: value.get("duration_ms").and_then(Value::as_u64),
            error: value
                .get("result")
                .and_then(Value::as_str)
                .map(|text| cap_text(text, MAX_TEXT_BYTES)),
        },
    )]
}

fn block_failed(block: &Value) -> bool {
    block
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn record(
    native_id: Option<String>,
    parent_id: Option<String>,
    timestamp_ms: Option<u64>,
    payload: ConversationItemPayload,
) -> NativeRecord {
    NativeRecord {
        entry_id: native_id.clone(),
        native_id,
        parent_id,
        timestamp_ms,
        turn_id: None,
        payload,
        topology_only: false,
        anchor: 0,
    }
}

fn parse_timestamp(value: &str) -> Option<u64> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .and_then(|timestamp| u64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::conversations::{AssistantMessagePhase, ConversationItemPayload};

    #[test]
    fn claude_fixture_excludes_thinking_and_pairs_tool_use_result() {
        let assistant = normalize_claude_line(
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"secret"},{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"cargo test"}}],"stop_reason":null}}"#,
        );
        assert_eq!(assistant.len(), 1);
        assert!(!format!("{:?}", assistant[0].payload).contains("secret"));
        assert!(matches!(
            &assistant[0].payload,
            ConversationItemPayload::ToolActivity { preview: Some(preview), .. }
                if preview == "cargo test"
        ));
        let result = normalize_claude_line(
            r#"{"type":"user","uuid":"r1","parentUuid":"a1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ok","is_error":false}]}}"#,
        );
        assert_eq!(assistant[0].native_id, result[0].native_id);
        assert!(matches!(
            result[0].payload,
            ConversationItemPayload::ToolActivity {
                status: ToolStatus::Completed,
                preview: None,
                detail: None,
                ..
            }
        ));
        let final_message = normalize_claude_line(
            r#"{"type":"assistant","uuid":"a2","parentUuid":"r1","turnId":"turn-1","sessionId":"session-1","message":{"role":"assistant","content":[{"type":"text","text":"final"}],"stop_reason":"end_turn"}}"#,
        );
        assert!(matches!(
            final_message[0].payload,
            ConversationItemPayload::AssistantMessage {
                phase: AssistantMessagePhase::Final,
                ..
            }
        ));
        assert_eq!(final_message[0].turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn session_identity_is_not_used_as_turn_identity() {
        let message = normalize_claude_line(
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"session-1","message":{"role":"user","content":"hello"}}"#,
        );

        assert_eq!(message.len(), 1);
        assert_eq!(message[0].turn_id, None);
    }
}
