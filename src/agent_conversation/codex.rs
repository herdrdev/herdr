//! Codex rollout JSONL adapter. The installed rollout files contain
//! `response_item` rows (message/function_call/function_call_output) and
//! `event_msg` lifecycle rows. Encrypted reasoning rows are ignored.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_conversation::{
    cap_text, safe_display_path, tool_command_preview, validate_under_root, NativeRecord,
    ProviderAdapter, SourceFingerprint, TranscriptError, MAX_DETAIL_BYTES, MAX_ITEMS_PER_PAGE,
    MAX_TEXT_BYTES,
};
use crate::api::schema::conversations::{
    AssistantMessagePhase, ConversationItemPayload, FileChangeKind, PlanStep, PlanStepStatus,
    ToolStatus, TurnStateKind,
};

pub struct CodexAdapter;

impl ProviderAdapter for CodexAdapter {
    fn provider_name(&self) -> &'static str {
        "codex"
    }

    fn validate_source(&self, path: &Path) -> Result<SourceFingerprint, TranscriptError> {
        let roots = crate::agent_conversation::provider_roots("codex");
        let refs: Vec<_> = roots.iter().map(PathBuf::as_path).collect();
        validate_under_root(path, &refs)
    }

    fn normalize_line(&self, line: &str) -> Vec<NativeRecord> {
        normalize_codex_line(line)
    }
}

fn normalize_codex_line(line: &str) -> Vec<NativeRecord> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response_item") => normalize_response_item(&value),
        Some("event_msg") => normalize_event_msg(&value),
        _ => Vec::new(),
    }
}

fn normalize_response_item(value: &Value) -> Vec<NativeRecord> {
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| value.get("id").and_then(Value::as_str))
        .map(str::to_string);
    let parent = value
        .get("parent_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timestamp = value.get("timestamp").and_then(Value::as_u64).or_else(|| {
        value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
    });
    let turn_id = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("internal_chat_message_metadata_passthrough")
                .and_then(|metadata| metadata.get("turn_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("turn_id").and_then(Value::as_str))
        .map(str::to_string);
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
            let text = payload
                .get("content")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|part| {
                            matches!(
                                part.get("type").and_then(Value::as_str),
                                Some("output_text") | Some("input_text")
                            )
                        })
                        .map(|part| part.get("text").and_then(Value::as_str).unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .or_else(|| {
                    payload
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            if role == "user" {
                return (!text.is_empty())
                    .then(|| {
                        record_with_turn(
                            id,
                            parent,
                            timestamp,
                            turn_id.clone(),
                            ConversationItemPayload::UserMessage {
                                text: cap_text(&text, MAX_TEXT_BYTES),
                                attachments: Vec::new(),
                            },
                        )
                    })
                    .into_iter()
                    .collect();
            }
            if role == "assistant" && !text.is_empty() {
                let phase = if payload.get("phase").and_then(Value::as_str) == Some("final_answer")
                {
                    AssistantMessagePhase::Final
                } else {
                    AssistantMessagePhase::Commentary
                };
                return vec![record_with_turn(
                    id,
                    parent,
                    timestamp,
                    turn_id,
                    ConversationItemPayload::AssistantMessage {
                        phase,
                        text: cap_text(&text, MAX_TEXT_BYTES),
                        state: crate::api::schema::conversations::CompletionState::Completed,
                    },
                )];
            }
            Vec::new()
        }
        Some("function_call") | Some("custom_tool_call") => {
            let call_id = payload
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| payload.get("id").and_then(Value::as_str))
                .or(id.as_deref())
                .map(str::to_string);
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let input = payload.get("arguments").or_else(|| payload.get("input"));
            if name == "update_plan" || name == "plan" {
                let plan_id = turn_id
                    .clone()
                    .map(|turn| format!("plan:{turn}"))
                    .or(call_id.clone());
                return vec![record_with_turn(
                    plan_id,
                    parent,
                    timestamp,
                    turn_id.clone(),
                    ConversationItemPayload::PlanUpdate {
                        steps: plan_steps(input),
                    },
                )];
            }
            vec![record_with_turn(
                call_id,
                parent,
                timestamp,
                turn_id.clone(),
                ConversationItemPayload::ToolActivity {
                    action: cap_text(name, MAX_TEXT_BYTES),
                    label: cap_text(name, MAX_TEXT_BYTES),
                    status: ToolStatus::Running,
                    preview: tool_command_preview(name, input),
                    detail: None,
                    duration_ms: None,
                    paths: tool_paths(input),
                },
            )]
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            let call_id = payload
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| payload.get("id").and_then(Value::as_str))
                .or(id.as_deref())
                .map(str::to_string);
            let failed = payload.get("success").and_then(Value::as_bool) == Some(false)
                || payload
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| matches!(status, "failed" | "error"))
                || payload
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0);
            vec![record_with_turn(
                call_id,
                parent,
                timestamp,
                turn_id,
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
            )]
        }
        _ => Vec::new(),
    }
}

fn normalize_event_msg(value: &Value) -> Vec<NativeRecord> {
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let timestamp = value.get("timestamp").and_then(Value::as_u64).or_else(|| {
        value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
    });
    let id = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .or_else(|| value.get("turn_id").and_then(Value::as_str))
        .or_else(|| value.get("id").and_then(Value::as_str))
        .map(str::to_string);
    match event_type {
        "task_started" | "turn_started" => vec![record_with_turn(
            id.clone(),
            None,
            timestamp,
            id,
            ConversationItemPayload::TurnState {
                state: TurnStateKind::Started,
                started_ms: timestamp,
                duration_ms: None,
                error: None,
            },
        )],
        "task_complete" | "turn_complete" => vec![record_with_turn(
            id.clone(),
            None,
            timestamp,
            id,
            ConversationItemPayload::TurnState {
                state: TurnStateKind::Completed,
                started_ms: None,
                duration_ms: payload.get("duration_ms").and_then(Value::as_u64),
                error: None,
            },
        )],
        "turn_aborted" | "task_interrupted" => vec![record_with_turn(
            id.clone(),
            None,
            timestamp,
            id,
            ConversationItemPayload::TurnState {
                state: TurnStateKind::Interrupted,
                started_ms: None,
                duration_ms: None,
                error: None,
            },
        )],
        "patch_apply_end" => {
            if payload.get("success").and_then(Value::as_bool) == Some(false)
                || payload
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| matches!(status, "failed" | "error"))
            {
                return Vec::new();
            }
            let call_id = payload
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or("patch");
            payload
                .get("changes")
                .and_then(Value::as_object)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|(path, change)| {
                            let change_type = change
                                .get("type")
                                .or_else(|| change.get("kind"))
                                .or_else(|| change.get("change"))
                                .and_then(Value::as_str)
                                .unwrap_or("update");
                            let kind = if change.get("move_path").and_then(Value::as_str).is_some()
                            {
                                FileChangeKind::Renamed
                            } else {
                                match change_type {
                                    "add" | "created" | "added" => FileChangeKind::Created,
                                    "delete" | "deleted" | "removed" => FileChangeKind::Deleted,
                                    "rename" | "renamed" => FileChangeKind::Renamed,
                                    _ => FileChangeKind::Modified,
                                }
                            };
                            let path = safe_display_path(path)?;
                            Some(record_with_turn(
                                Some(format!("{call_id}:file:{path}")),
                                None,
                                timestamp,
                                id.clone(),
                                ConversationItemPayload::FileChange {
                                    path,
                                    change: kind,
                                    summary: change
                                        .get("status")
                                        .or_else(|| change.get("success"))
                                        .map(|value| {
                                            cap_text(&value.to_string(), MAX_DETAIL_BYTES)
                                        }),
                                },
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        "file_change" | "file_changed" => payload
            .get("path")
            .and_then(Value::as_str)
            .and_then(safe_display_path)
            .map(|path| {
                record_with_turn(
                    id.clone(),
                    None,
                    timestamp,
                    id,
                    ConversationItemPayload::FileChange {
                        path,
                        change: FileChangeKind::Modified,
                        summary: payload
                            .get("summary")
                            .and_then(Value::as_str)
                            .map(|text| cap_text(text, MAX_DETAIL_BYTES)),
                    },
                )
            })
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn tool_paths(input: Option<&Value>) -> Vec<String> {
    let Some(input) = input else {
        return Vec::new();
    };
    let parsed = match input {
        Value::String(value) => serde_json::from_str::<Value>(value).unwrap_or(Value::Null),
        value => value.clone(),
    };
    let mut paths = Vec::new();
    for key in ["path", "file_path", "notebook_path"] {
        if let Some(path) = parsed
            .get(key)
            .and_then(Value::as_str)
            .and_then(safe_display_path)
        {
            paths.push(path);
        }
    }
    if let Some(values) = parsed.get("paths").and_then(Value::as_array) {
        paths.extend(
            values
                .iter()
                .filter_map(Value::as_str)
                .filter_map(safe_display_path),
        );
    }
    paths.sort();
    paths.dedup();
    paths.truncate(64);
    paths
}

fn plan_steps(input: Option<&Value>) -> Vec<PlanStep> {
    let Some(input) = input else {
        return Vec::new();
    };
    let parsed = match input {
        Value::String(value) => serde_json::from_str::<Value>(value).unwrap_or(Value::Null),
        value => value.clone(),
    };
    parsed
        .get("plan")
        .or_else(|| parsed.get("steps"))
        .or_else(|| parsed.get("items"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(MAX_ITEMS_PER_PAGE)
                .filter_map(|item| {
                    let label = item
                        .get("step")
                        .or_else(|| item.get("content"))
                        .or_else(|| item.get("text"))
                        .and_then(Value::as_str)?;
                    let status = match item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending")
                    {
                        "completed" | "done" => PlanStepStatus::Completed,
                        "in_progress" | "active" => PlanStepStatus::Active,
                        "failed" => PlanStepStatus::Failed,
                        _ => PlanStepStatus::Pending,
                    };
                    Some(PlanStep {
                        label: cap_text(label, MAX_TEXT_BYTES),
                        status,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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

fn record_with_turn(
    native_id: Option<String>,
    parent_id: Option<String>,
    timestamp_ms: Option<u64>,
    turn_id: Option<String>,
    payload: ConversationItemPayload,
) -> NativeRecord {
    let mut record = record(native_id, parent_id, timestamp_ms, payload);
    record.turn_id = turn_id;
    record
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::conversations::{AssistantMessagePhase, ConversationItemPayload};

    #[test]
    fn codex_fixture_excludes_reasoning_and_pairs_custom_tool_output() {
        let assistant = normalize_codex_line(
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","turn_id":"turn-1","content":[{"type":"output_text","text":"done"}]}}"#,
        );
        assert!(matches!(
            assistant[0].payload,
            ConversationItemPayload::AssistantMessage {
                phase: AssistantMessagePhase::Final,
                ..
            }
        ));
        assert_eq!(assistant[0].turn_id.as_deref(), Some("turn-1"));
        assert!(normalize_codex_line(
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":["secret"]}}"#
        )
        .is_empty());
        let call = normalize_codex_line(
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call-1","name":"edit","input":"{}"}}"#,
        );
        let output = normalize_codex_line(
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call-1","output":"ok"}}"#,
        );
        assert_eq!(call[0].native_id, output[0].native_id);
        assert!(matches!(
            output[0].payload,
            ConversationItemPayload::ToolActivity {
                status: ToolStatus::Completed,
                preview: None,
                detail: None,
                ..
            }
        ));
    }

    #[test]
    fn shell_tool_call_exposes_its_command_as_a_bounded_preview() {
        let records = normalize_codex_line(
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"call-1","name":"exec_command","arguments":"{\"cmd\":\"cargo test\"}"}}"#,
        );

        assert!(matches!(
            &records[0].payload,
            ConversationItemPayload::ToolActivity { preview: Some(preview), .. }
                if preview == "cargo test"
        ));
    }
    #[test]
    fn real_rollout_rows_keep_nested_turn_and_patch_changes() {
        let plan = normalize_codex_line(
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"plan-a","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"Build\",\"status\":\"in_progress\"}]}","internal_chat_message_metadata_passthrough":{"turn_id":"turn-7"}}}"#,
        );
        let next_plan = normalize_codex_line(
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"plan-b","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"Test\",\"status\":\"completed\"}]}","internal_chat_message_metadata_passthrough":{"turn_id":"turn-7"}}}"#,
        );
        assert_eq!(plan[0].native_id, next_plan[0].native_id);
        let patch = normalize_codex_line(
            r#"{"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call-1","changes":{"src/app.ts":{"status":"modified","success":true}},"turn_id":"turn-7"}}"#,
        );
        assert!(
            matches!(patch[0].payload, ConversationItemPayload::FileChange { ref path, .. } if path == "src/app.ts")
        );
    }
}
