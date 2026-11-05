//! Workflow execution and human approval gates (`approve`, `reject`).

use super::OrchestrationService;
use crate::find_task;
use serde_json::Value;
use std::path::PathBuf;

impl OrchestrationService {
    pub fn run_workflow(
        &self,
        root: PathBuf,
        task: String,
        mcp_url: Option<String>,
        auto_push: bool,
    ) -> Result<Value, String> {
        let result = (|| {
            let effective_mcp_url = mcp_url
                .filter(|u| !u.is_empty())
                .or_else(|| self.mcp_url())
                .ok_or_else(|| {
                    "mcp_url is required when in-process MCP is not running".to_owned()
                })?;
            let report = crate::workflow::run_coders_with_terminals(
                crate::workflow::RunRequest {
                    root: root.clone(),
                    task: task.clone(),
                    mcp_url: effective_mcp_url,
                },
                self.terminals.as_ref(),
                Some(self.service_config.registry_path()),
                self.active_vendors_override.clone(),
            )
            .map_err(|e| e.to_string())?;

            let mut pushed_prs = Vec::new();
            if auto_push && !report.plan_waiting {
                let loaded_task = find_task(&root, &task).map_err(|e| e.to_string())?;
                for subtask in &loaded_task.status.subtasks {
                    if subtask.state == "awaiting-approval" {
                        let pr_url = crate::workflow::approve_subtask_with_registry(
                            root.clone(),
                            &task,
                            subtask.number,
                            &self.service_config.registry_path(),
                        )
                        .map_err(|e| e.to_string())?;
                        pushed_prs.push(serde_json::json!({
                            "subtask": subtask.number,
                            "pr": pr_url,
                        }));
                    }
                }
            }

            Ok(serde_json::json!({
                "started": report.started,
                "completed_coding": report.completed_coding,
                "waiting_for_human": report.waiting_for_human,
                "failed": report.failed,
                "plan_waiting": report.plan_waiting,
                "auto_push": auto_push,
                "pushed_prs": pushed_prs,
            }))
        })();
        self.log_err("run", result)
    }

    /// Requirement 9: Explicit human approval for `plan` or a subtask number.
    pub fn approve(&self, root: PathBuf, task: &str, target: &str) -> Result<Value, String> {
        let result = (|| {
            let clean_target = target.strip_prefix("subtask:").unwrap_or(target);
            if clean_target == "plan" {
                crate::workflow::approve_plan(root, task).map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "ok": true,
                    "task": task,
                    "approved": "plan",
                }))
            } else {
                let number: u32 = clean_target.parse().map_err(|_| {
                    format!("invalid approval target {target:?}; expected 'plan' or subtask number")
                })?;
                let pr_url = crate::workflow::approve_subtask_with_registry(
                    root,
                    task,
                    number,
                    &self.service_config.registry_path(),
                )
                .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "ok": true,
                    "task": task,
                    "approved": number,
                    "url": pr_url,
                    "pr": pr_url,
                }))
            }
        })();
        self.log_err("approve", result)
    }

    /// Explicit human rejection for `plan` or a subtask number.
    pub fn reject(
        &self,
        root: PathBuf,
        task: &str,
        target: &str,
        reason: Option<&str>,
    ) -> Result<Value, String> {
        let result = (|| {
            let clean_target = target.strip_prefix("subtask:").unwrap_or(target);
            if clean_target == "plan" {
                crate::workflow::reject_plan(root, task, reason).map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "ok": true,
                    "task": task,
                    "rejected": "plan",
                    "reason": reason,
                }))
            } else {
                let number: u32 = clean_target.parse().map_err(|_| {
                    format!(
                        "invalid rejection target {target:?}; expected 'plan' or subtask number"
                    )
                })?;
                crate::workflow::reject_subtask(root, task, number, reason)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "ok": true,
                    "task": task,
                    "rejected": number,
                    "reason": reason,
                }))
            }
        })();
        self.log_err("reject", result)
    }
}
