//! OMP uses Pi's durable message tree but its todo and edit result details
//! follow the OMP extension contract rather than Pi's argument shape.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_conversation::{
    cap_text, safe_display_path_under_root, validate_under_root, NativeRecord, ProviderAdapter,
    SourceFingerprint, TranscriptError, MAX_DETAIL_BYTES, MAX_ITEMS_PER_PAGE, MAX_TEXT_BYTES,
};
use crate::api::schema::conversations::{
    ConversationItemPayload, FileChangeKind, PlanStep, PlanStepStatus,
};

pub struct OmpAdapter;

impl ProviderAdapter for OmpAdapter {
    fn provider_name(&self) -> &'static str {
        "omp"
    }

    fn validate_source(&self, path: &Path) -> Result<SourceFingerprint, TranscriptError> {
        let roots = crate::agent_conversation::provider_roots("omp");
        let refs: Vec<_> = roots.iter().map(PathBuf::as_path).collect();
        validate_under_root(path, &refs)
    }

    fn normalize_line(&self, line: &str) -> Vec<NativeRecord> {
        normalize_omp_line(line, None)
    }

    fn normalize_line_for_display(
        &self,
        line: &str,
        display_root: Option<&Path>,
    ) -> Vec<NativeRecord> {
        normalize_omp_line(line, display_root)
    }

    fn select_active_branch(&self, records: Vec<NativeRecord>) -> Vec<NativeRecord> {
        super::pi::select_active_branch_for_omp(records)
    }

    fn select_active_branch_from_tip(
        &self,
        records: Vec<NativeRecord>,
        tip: Option<&str>,
    ) -> Vec<NativeRecord> {
        super::pi::select_active_branch_from_tip_for_omp(records, tip)
    }
}

fn normalize_omp_line(line: &str, display_root: Option<&Path>) -> Vec<NativeRecord> {
    let mut records = super::pi::normalize_pi_line_with_root(line, display_root);
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return records;
    };
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let message = value.get("message").unwrap_or(&Value::Null);
    if kind == "message" && message.get("role").and_then(Value::as_str) == Some("toolResult") {
        let failed = message.get("isError").and_then(Value::as_bool) == Some(true);
        let tool_id = message
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let details = message.get("details").unwrap_or(&Value::Null);
        if !failed {
            if let Some(steps) = omp_result_steps(details) {
                records.push(NativeRecord {
                    native_id: Some(format!("plan:{tool_id}")),
                    entry_id: value.get("id").and_then(Value::as_str).map(str::to_string),
                    parent_id: value
                        .get("parentId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp_ms: None,
                    turn_id: None,
                    payload: ConversationItemPayload::PlanUpdate { steps },
                    topology_only: false,
                    anchor: 0,
                });
            }
        }
    }
    if kind == "message" && message.get("role").and_then(Value::as_str) == Some("toolResult") {
        let failed = message.get("isError").and_then(Value::as_bool) == Some(true);
        if !failed {
            let tool_id = message
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let details = message.get("details").unwrap_or(&Value::Null);
            let path = details
                .get("resolvedPath")
                .or_else(|| details.get("path"))
                .and_then(Value::as_str);
            if let Some(path) =
                path.and_then(|path| safe_display_path_under_root(path, display_root))
            {
                records.push(NativeRecord {
                    native_id: Some(format!("{tool_id}:file")),
                    entry_id: value.get("id").and_then(Value::as_str).map(str::to_string),
                    parent_id: value
                        .get("parentId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp_ms: None,
                    turn_id: None,
                    payload: ConversationItemPayload::FileChange {
                        path,
                        change: FileChangeKind::Modified,
                        summary: details
                            .get("op")
                            .and_then(Value::as_str)
                            .map(|value| cap_text(value, MAX_DETAIL_BYTES)),
                    },
                    topology_only: false,
                    anchor: 0,
                });
            }
        }
    }
    records
}

fn omp_result_steps(details: &Value) -> Option<Vec<PlanStep>> {
    let phases = details.get("phases").and_then(Value::as_array)?;
    Some(
        phases
            .iter()
            .flat_map(|phase| {
                let name = phase.get("name").and_then(Value::as_str).unwrap_or("phase");
                let tasks = phase
                    .get("tasks")
                    .or_else(|| phase.get("items"))
                    .or_else(|| phase.get("steps"))
                    .and_then(Value::as_array);
                tasks.into_iter().flatten().flat_map(move |task| {
                    let nested = task
                        .get("tasks")
                        .or_else(|| task.get("items"))
                        .and_then(Value::as_array);
                    let candidates = nested
                        .map(|items| items.iter().collect::<Vec<_>>())
                        .unwrap_or_else(|| vec![task]);
                    candidates.into_iter().filter_map(move |candidate| {
                        let content = candidate
                            .as_str()
                            .or_else(|| candidate.get("content").and_then(Value::as_str))
                            .or_else(|| candidate.get("label").and_then(Value::as_str))
                            .or_else(|| candidate.get("text").and_then(Value::as_str))?;
                        let status = match candidate
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("pending")
                        {
                            "completed" | "done" => PlanStepStatus::Completed,
                            "in_progress" | "active" => PlanStepStatus::Active,
                            "blocked" | "failed" => PlanStepStatus::Failed,
                            _ => PlanStepStatus::Pending,
                        };
                        Some(PlanStep {
                            label: cap_text(&format!("{name}: {content}"), MAX_TEXT_BYTES),
                            status,
                        })
                    })
                })
            })
            .take(MAX_ITEMS_PER_PAGE)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn omp_adapter_uses_authoritative_todo_list_shape_and_edit_details() {
        let call = serde_json::json!({
            "type": "message",
            "id": "p1",
            "parentId": null,
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "todo-1",
                    "name": "todo",
                    "arguments": {"i": "todo", "op": "init", "list": [{"phase": "Checks", "items": ["Run tests"]}]}
                }]
            }
        }).to_string();
        let call_records = OmpAdapter.normalize_line(&call);
        assert!(call_records.iter().any(|record| matches!(
            &record.payload,
            ConversationItemPayload::PlanUpdate { steps } if !steps.is_empty()
        )));
        for status in ["pending", "in_progress", "completed", "blocked"] {
            let result = serde_json::json!({"type":"message","id":"r1","parentId":"p1","message":{"role":"toolResult","toolCallId":"todo-1","content":"ok","details":{"op":"done","phases":[{"name":"Checks","tasks":[{"content":"Run tests","status":status}]}]},"isError":false}}).to_string();
            let records = OmpAdapter.normalize_line(&result);
            assert!(records.iter().any(|record| matches!(&record.payload, ConversationItemPayload::PlanUpdate { steps } if !steps.is_empty() && steps[0].label.contains("Run tests"))));
        }
        let native_items = serde_json::json!({"type":"message","id":"r-native","parentId":"p1","message":{"role":"toolResult","toolCallId":"todo-1","content":"ok","details":{"phases":[{"name":"Native","items":[{"label":"Review output","status":"done"}]}]},"isError":false}}).to_string();
        assert!(OmpAdapter.normalize_line(&native_items).iter().any(|record| matches!(&record.payload, ConversationItemPayload::PlanUpdate { steps } if steps.iter().any(|step| step.label.contains("Review output")))));
        let failed = serde_json::json!({"type":"message","id":"r2","parentId":"p1","message":{"role":"toolResult","toolCallId":"todo-1","details":{"op":"done","phases":[]},"isError":true}}).to_string();
        assert!(!OmpAdapter
            .normalize_line(&failed)
            .iter()
            .any(|record| matches!(record.payload, ConversationItemPayload::PlanUpdate { .. })));
        let edit = serde_json::json!({"type":"message","id":"r3","parentId":"p1","message":{"role":"toolResult","toolCallId":"edit-1","content":"ok","details":{"op":"edit","path":"src/app.ts"},"isError":false}}).to_string();
        assert!(OmpAdapter.normalize_line(&edit).iter().any(|record| matches!(record.payload, ConversationItemPayload::FileChange { ref path, .. } if path == "src/app.ts")));
    }
}
