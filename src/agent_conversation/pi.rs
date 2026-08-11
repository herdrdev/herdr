//! Pi's durable session format is documented in the installed 0.84.1
//! `session-format.md`: a header followed by `type=message` rows whose nested
//! `message.role` is user, assistant, or toolResult. Branches use id/parentId.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_conversation::{
    cap_text, safe_display_path_under_root, tool_command_preview, validate_under_root,
    NativeRecord, ProviderAdapter, SourceFingerprint, TranscriptError, MAX_ITEMS_PER_PAGE,
    MAX_TEXT_BYTES,
};
use crate::api::schema::conversations::{
    AssistantMessagePhase, AttachmentMetadata, CompletionState, ConversationItemPayload, PlanStep,
    PlanStepStatus, ToolStatus,
};

pub struct PiAdapter;

impl ProviderAdapter for PiAdapter {
    fn provider_name(&self) -> &'static str {
        "pi"
    }

    fn validate_source(&self, path: &Path) -> Result<SourceFingerprint, TranscriptError> {
        let roots = crate::agent_conversation::provider_roots("pi");
        let refs: Vec<_> = roots.iter().map(PathBuf::as_path).collect();
        validate_under_root(path, &refs)
    }

    fn normalize_line(&self, line: &str) -> Vec<NativeRecord> {
        normalize_pi_line(line)
    }

    fn normalize_line_for_display(
        &self,
        line: &str,
        display_root: Option<&Path>,
    ) -> Vec<NativeRecord> {
        normalize_pi_line_with_root(line, display_root)
    }

    fn select_active_branch(&self, records: Vec<NativeRecord>) -> Vec<NativeRecord> {
        select_active_branch(records)
    }
    fn select_active_branch_from_tip(
        &self,
        records: Vec<NativeRecord>,
        tip: Option<&str>,
    ) -> Vec<NativeRecord> {
        select_active_branch_with_tip(records, tip)
    }
}

pub(crate) fn normalize_pi_line(line: &str) -> Vec<NativeRecord> {
    normalize_pi_line_with_root(line, None)
}

pub(crate) fn normalize_pi_line_with_root(
    line: &str,
    display_root: Option<&Path>,
) -> Vec<NativeRecord> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if value.get("type").and_then(Value::as_str) != Some("message") {
        let Some(entry_id) = value.get("id").and_then(Value::as_str).map(str::to_string) else {
            return Vec::new();
        };
        return vec![NativeRecord {
            native_id: None,
            entry_id: Some(entry_id),
            parent_id: value
                .get("parentId")
                .and_then(Value::as_str)
                .map(str::to_string),
            timestamp_ms: value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_timestamp),
            turn_id: None,
            payload: ConversationItemPayload::Notice {
                message: String::new(),
            },
            topology_only: true,
            anchor: 0,
        }];
    }
    let entry_id = value.get("id").and_then(Value::as_str).map(str::to_string);
    let parent_id = value
        .get("parentId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timestamp_ms = value
        .get("message")
        .and_then(|message| message.get("timestamp"))
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_timestamp)
        });
    let message = value.get("message").unwrap_or(&Value::Null);
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    match role {
        "user" => {
            let (text, attachments) = text_and_attachments(message.get("content"));
            if text.is_empty() && attachments.is_empty() {
                return Vec::new();
            }
            vec![record(
                entry_id.clone(),
                entry_id,
                parent_id,
                timestamp_ms,
                canonical_turn_id(timestamp_ms),
                ConversationItemPayload::UserMessage { text, attachments },
            )]
        }
        "assistant" => {
            normalize_assistant(entry_id, parent_id, timestamp_ms, message, display_root)
        }
        "toolResult" => normalize_tool_result(entry_id, parent_id, timestamp_ms, message),
        "bashExecution" => {
            let action = "bash".to_string();
            let failed = message.get("cancelled").and_then(Value::as_bool) == Some(true)
                || message
                    .get("exitCode")
                    .is_some_and(|code| !code.is_null() && code != &Value::from(0));
            vec![record(
                entry_id.clone(),
                entry_id,
                parent_id,
                timestamp_ms,
                None,
                ConversationItemPayload::ToolActivity {
                    action,
                    label: "bash".into(),
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
        _ => entry_id
            .map(|entry_id| {
                vec![NativeRecord {
                    native_id: None,
                    entry_id: Some(entry_id),
                    parent_id,
                    timestamp_ms,
                    turn_id: None,
                    payload: ConversationItemPayload::Notice {
                        message: String::new(),
                    },
                    topology_only: true,
                    anchor: 0,
                }]
            })
            .unwrap_or_default(),
    }
}

fn normalize_assistant(
    entry_id: Option<String>,
    parent_id: Option<String>,
    timestamp_ms: Option<u64>,
    message: &Value,
    display_root: Option<&Path>,
) -> Vec<NativeRecord> {
    let mut records = Vec::new();
    let content = message.get("content").and_then(Value::as_array);
    let mut text = String::new();
    if let Some(blocks) = content {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""))
                }
                Some("toolCall") => {
                    let mut id = block.get("id").and_then(Value::as_str).map(str::to_string);
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = block.get("arguments").or_else(|| block.get("input"));
                    if name == "todo" || name == "plan" {
                        id = id
                            .map(|id| format!("plan:{id}"))
                            .or_else(|| entry_id.clone().map(|entry| format!("plan:{entry}")));
                    }
                    let payload = if name == "todo" || name == "plan" {
                        ConversationItemPayload::PlanUpdate {
                            steps: plan_steps(input),
                        }
                    } else {
                        ConversationItemPayload::ToolActivity {
                            action: cap_text(name, MAX_TEXT_BYTES),
                            label: cap_text(name, MAX_TEXT_BYTES),
                            status: ToolStatus::Running,
                            preview: tool_command_preview(name, input),
                            detail: None,
                            duration_ms: None,
                            paths: input
                                .and_then(|input| input.get("path"))
                                .and_then(Value::as_str)
                                .and_then(|path| safe_display_path_under_root(path, display_root))
                                .into_iter()
                                .collect(),
                        }
                    };
                    records.push(record(
                        id,
                        entry_id.clone(),
                        parent_id.clone(),
                        timestamp_ms,
                        None,
                        payload,
                    ));
                }
                Some("thinking") => {}
                _ => {}
            }
        }
    } else if let Some(value) = message.get("content").and_then(Value::as_str) {
        text.push_str(value);
    }
    let stop_reason = message
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("");
    if text.is_empty() && matches!(stop_reason, "error" | "aborted") {
        records.push(record(
            entry_id.clone(),
            entry_id.clone(),
            parent_id.clone(),
            timestamp_ms,
            None,
            ConversationItemPayload::TurnState {
                state: if stop_reason == "aborted" {
                    crate::api::schema::conversations::TurnStateKind::Interrupted
                } else {
                    crate::api::schema::conversations::TurnStateKind::Failed
                },
                started_ms: None,
                duration_ms: None,
                error: message
                    .get("errorMessage")
                    .or_else(|| message.get("errorId"))
                    .and_then(Value::as_str)
                    .map(|error| cap_text(error, MAX_TEXT_BYTES)),
            },
        ));
    }
    if !text.is_empty() {
        let state = match stop_reason {
            "aborted" => CompletionState::Interrupted,
            "error" => CompletionState::Failed,
            _ => CompletionState::Completed,
        };
        let phase = if matches!(stop_reason, "stop" | "length" | "error" | "aborted") {
            AssistantMessagePhase::Final
        } else {
            AssistantMessagePhase::Commentary
        };
        records.push(record(
            entry_id.clone(),
            entry_id,
            parent_id,
            timestamp_ms,
            None,
            ConversationItemPayload::AssistantMessage {
                phase,
                text: cap_text(&text, MAX_TEXT_BYTES),
                state,
            },
        ));
    }
    records
}

fn normalize_tool_result(
    entry_id: Option<String>,
    parent_id: Option<String>,
    timestamp_ms: Option<u64>,
    message: &Value,
) -> Vec<NativeRecord> {
    let tool_id = message
        .get("toolCallId")
        .and_then(Value::as_str)
        .or(entry_id.as_deref())
        .map(str::to_string);
    let failed = message.get("isError").and_then(Value::as_bool) == Some(true);
    vec![record(
        tool_id,
        entry_id,
        parent_id,
        timestamp_ms,
        None,
        ConversationItemPayload::ToolActivity {
            action: message
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .into(),
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

fn text_and_attachments(content: Option<&Value>) -> (String, Vec<AttachmentMetadata>) {
    let mut text = String::new();
    let mut attachments = Vec::new();
    match content {
        Some(Value::String(value)) => text.push_str(value),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""))
                    }
                    Some("image") => attachments.push(AttachmentMetadata {
                        media_type: cap_text(
                            block
                                .get("mimeType")
                                .and_then(Value::as_str)
                                .unwrap_or("image/*"),
                            128,
                        ),
                        name: "image".into(),
                        byte_size: block
                            .get("data")
                            .and_then(Value::as_str)
                            .map(|data| data.len() as u64)
                            .unwrap_or(0),
                    }),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    (
        cap_text(&text, MAX_TEXT_BYTES),
        attachments.into_iter().take(16).collect(),
    )
}

fn plan_steps(input: Option<&Value>) -> Vec<PlanStep> {
    let Some(input) = input else {
        return Vec::new();
    };
    let Some(items) = input
        .get("items")
        .or_else(|| input.get("todos"))
        .or_else(|| input.get("todo"))
        .or_else(|| input.get("list"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut steps = Vec::new();
    for item in items {
        let group_name = item.get("phase").and_then(Value::as_str);
        let nested = item.get("items").and_then(Value::as_array);
        let candidates: Vec<&Value> = nested
            .map(|values| values.iter().collect())
            .unwrap_or_else(|| vec![item]);
        for candidate in candidates {
            if steps.len() >= MAX_ITEMS_PER_PAGE {
                break;
            }
            let label = candidate
                .as_str()
                .or_else(|| candidate.get("content").and_then(Value::as_str))
                .or_else(|| candidate.get("text").and_then(Value::as_str));
            let Some(label) = label else {
                continue;
            };
            let status = match candidate
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
            {
                "completed" | "done" => PlanStepStatus::Completed,
                "in_progress" | "active" => PlanStepStatus::Active,
                "failed" | "blocked" => PlanStepStatus::Failed,
                _ => PlanStepStatus::Pending,
            };
            let label = group_name
                .map(|group| format!("{group}: {label}"))
                .unwrap_or_else(|| label.to_string());
            steps.push(PlanStep {
                label: cap_text(&label, MAX_TEXT_BYTES),
                status,
            });
        }
    }
    steps
}

fn canonical_turn_id(timestamp_ms: Option<u64>) -> Option<String> {
    timestamp_ms.map(|timestamp| format!("turn:{timestamp}"))
}

fn record(
    native_id: Option<String>,
    entry_id: Option<String>,
    parent_id: Option<String>,
    timestamp_ms: Option<u64>,
    turn_id: Option<String>,
    payload: ConversationItemPayload,
) -> NativeRecord {
    NativeRecord {
        native_id,
        entry_id,
        parent_id,
        timestamp_ms,
        turn_id,
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

fn select_active_branch(records: Vec<NativeRecord>) -> Vec<NativeRecord> {
    select_active_branch_with_tip(records, None)
}

fn select_active_branch_with_tip(
    records: Vec<NativeRecord>,
    requested_tip: Option<&str>,
) -> Vec<NativeRecord> {
    let mut parent_by_id = std::collections::HashMap::new();
    let mut last_tip = None;
    for record in &records {
        if let Some(id) = record.entry_id.as_ref() {
            parent_by_id.insert(id.clone(), record.parent_id.clone());
            last_tip = Some(id.clone());
        }
    }
    let Some(mut current) = requested_tip.map(str::to_string).or(last_tip) else {
        return Vec::new();
    };
    let mut active = std::collections::HashSet::new();
    loop {
        if !active.insert(current.clone()) {
            break;
        }
        let Some(parent) = parent_by_id.get(&current).cloned().flatten() else {
            break;
        };
        current = parent;
    }
    records
        .into_iter()
        .filter(|record| {
            record
                .entry_id
                .as_ref()
                .is_some_and(|id| active.contains(id))
        })
        .collect()
}

pub(crate) fn select_active_branch_from_tip_for_omp(
    records: Vec<NativeRecord>,
    tip: Option<&str>,
) -> Vec<NativeRecord> {
    select_active_branch_with_tip(records, tip)
}

pub(crate) fn select_active_branch_for_omp(records: Vec<NativeRecord>) -> Vec<NativeRecord> {
    select_active_branch(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::conversations::ConversationItemPayload;

    #[test]
    fn native_pi_message_fixture_excludes_thinking_and_pairs_tool_result() {
        let assistant = normalize_pi_line(
            r#"{"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"secret"},{"type":"text","text":"Working"},{"type":"toolCall","id":"call-1","name":"edit","arguments":{"path":"a.txt"}}],"stopReason":"toolUse","timestamp":1}}"#,
        );
        assert_eq!(assistant.len(), 2);
        assert!(assistant
            .iter()
            .all(|record| !format!("{:?}", record.payload).contains("secret")));
        assert!(assistant.iter().any(|record| matches!(
            record.payload,
            ConversationItemPayload::ToolActivity {
                status: ToolStatus::Running,
                ..
            }
        )));
        let result = normalize_pi_line(
            r#"{"type":"message","id":"r1","parentId":"a1","message":{"role":"toolResult","toolCallId":"call-1","toolName":"edit","content":[{"type":"text","text":"done"}],"isError":false,"timestamp":2}}"#,
        );
        assert!(matches!(
            result[0].payload,
            ConversationItemPayload::ToolActivity {
                status: ToolStatus::Completed,
                preview: None,
                detail: None,
                ..
            }
        ));
    }

    #[test]
    fn bash_tool_call_exposes_its_command_as_a_bounded_preview() {
        let records = normalize_pi_line(
            r#"{"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"bash","arguments":{"command":"printf 'hello\\n'"}}],"stopReason":"toolUse","timestamp":1}}"#,
        );

        assert!(matches!(
            &records[0].payload,
            ConversationItemPayload::ToolActivity { preview: Some(preview), .. }
                if preview == "printf 'hello\\n'"
        ));
    }

    #[test]
    fn edit_tool_call_displays_absolute_paths_only_under_the_pane_root() {
        let line = r#"{"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"edit","arguments":{"path":"/workspace/project/src/chat.ts"}}],"stopReason":"toolUse","timestamp":1}}"#;
        let records = normalize_pi_line_with_root(line, Some(Path::new("/workspace/project")));

        assert!(matches!(
            &records[0].payload,
            ConversationItemPayload::ToolActivity { paths, .. }
                if paths == &["src/chat.ts"]
        ));

        let records = normalize_pi_line_with_root(line, Some(Path::new("/workspace/other")));
        assert!(matches!(
            &records[0].payload,
            ConversationItemPayload::ToolActivity { paths, .. }
                if paths.is_empty()
        ));
    }

    #[test]
    fn branched_pi_records_keep_only_active_parent_chain() {
        let rows = [
            r#"{"type":"message","id":"u","parentId":null,"message":{"role":"user","content":"root"}}"#,
            r#"{"type":"message","id":"abandoned","parentId":"u","message":{"role":"assistant","content":[{"type":"text","text":"old"}],"stopReason":"stop"}}"#,
            r#"{"type":"message","id":"active","parentId":"u","message":{"role":"assistant","content":[{"type":"text","text":"new"}],"stopReason":"stop"}}"#,
        ];
        let records = rows
            .iter()
            .flat_map(|row| normalize_pi_line(row))
            .collect::<Vec<_>>();
        let active = select_active_branch(records);
        let texts = active
            .iter()
            .filter_map(|record| match &record.payload {
                ConversationItemPayload::AssistantMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["new"]);
    }
    #[test]
    fn fixture_branch_keeps_tool_and_text_through_non_rendered_node() {
        let records = include_str!("fixtures/pi.jsonl")
            .lines()
            .flat_map(normalize_pi_line)
            .collect::<Vec<_>>();
        let active = select_active_branch(records);
        assert!(active.iter().any(|record| matches!(record.payload, ConversationItemPayload::AssistantMessage { ref text, .. } if text == "I will inspect it.")));
        assert!(active.iter().any(|record| matches!(
            record.payload,
            ConversationItemPayload::ToolActivity {
                status: ToolStatus::Running,
                ..
            }
        )));
        assert!(active.iter().any(|record| matches!(record.payload, ConversationItemPayload::AssistantMessage { ref text, .. } if text == "The change is complete.")));
        assert!(!active.iter().any(|record| matches!(record.payload, ConversationItemPayload::AssistantMessage { ref text, .. } if text == "Abandoned branch")));
    }
}
