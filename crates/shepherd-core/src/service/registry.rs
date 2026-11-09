//! Agent lifecycle operations (`kill`, `clean`, `focus`), message formatting, and registry helpers.

use super::OrchestrationService;
use crate::bus::Message;
use crate::snapshot::SnapshotReason;
use crate::terminals::TerminalError;
use crate::{Agent, AgentRegistryFile};
use std::path::{Component, Path};

impl OrchestrationService {
    /// Requirement 5 (`kill`):
    /// - Accepts live states (`starting, working, idle, blocked`) and `done`.
    /// - "Pane already closed" (`not_found`) counts as success.
    /// - Rejects already-definitive states (`exited, killed, failed`).
    pub fn kill(&self, id: &str) -> Result<Agent, String> {
        let result = (|| {
            let mut registry = self.load_registry()?;
            let Some(agent) = registry.agents.iter_mut().find(|a| a.id == id) else {
                return Err(format!("unknown agent {id:?}"));
            };
            if agent.gone() {
                return Err(format!(
                    "agent {id} is already in definitive state {:?}",
                    agent.state
                ));
            }
            let close_target = if !agent.tab.is_empty() {
                agent.tab.clone()
            } else if !agent.pane.is_empty() {
                agent.pane.clone()
            } else {
                agent.id.clone()
            };
            let warning = self.pre_action_snapshot(agent, SnapshotReason::PreKill);
            if !close_target.is_empty() {
                match self.terminals.close(&close_target) {
                    Ok(()) => {}
                    Err(err) if is_missing_terminal_error(&err) => {}
                    Err(err) => return Err(err.to_string()),
                }
            }
            agent.state = "killed".into();
            agent.notice = warning.unwrap_or_default();
            agent.updated = self.clock.now_rfc3339();
            let updated = agent.clone();
            self.save_registry(&registry)?;
            Ok(updated)
        })();
        self.log_err("kill", result)
    }

    /// Requirement 5 (`clean`):
    /// - By explicit IDs or all definitive agents of the workspace.
    /// - Closes the pane of a `failed` agent that remained open.
    /// - Kills a `done` agent first if requested by explicit ID.
    /// - Rejects a running agent (`starting, working, idle, blocked`).
    /// - Deletes its session directory after validating that `id` is a plain name (no `/` or `..`).
    pub fn clean(
        &self,
        ids: &[String],
        workspace_root: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let result = (|| {
            for id in ids {
                validate_plain_agent_id(id)?;
            }
            let mut registry = self.load_registry()?;
            let targets: Vec<String> = if !ids.is_empty() {
                for id in ids {
                    let Some(agent) = registry.agents.iter().find(|a| a.id == *id) else {
                        return Err(format!("unknown agent {id:?}"));
                    };
                    if matches!(
                        agent.state.as_str(),
                        "starting" | "working" | "idle" | "blocked"
                    ) {
                        return Err(format!(
                            "cannot clean running agent {id} in state {:?}",
                            agent.state
                        ));
                    }
                }
                ids.to_vec()
            } else {
                registry
                    .agents
                    .iter()
                    .filter(|agent| {
                        agent.gone()
                            && workspace_root
                                .filter(|w| !w.is_empty())
                                .is_none_or(|w| agent.root == w || agent.workspace == w)
                    })
                    .map(|agent| agent.id.clone())
                    .collect()
            };

            let mut cleaned = Vec::with_capacity(targets.len());
            for id in targets {
                validate_plain_agent_id(&id)?;
                if let Some(agent) = registry.agents.iter().find(|a| a.id == id).cloned() {
                    self.pre_action_snapshot(&agent, SnapshotReason::PreClean);
                    let close_target = if !agent.tab.is_empty() {
                        agent.tab.clone()
                    } else {
                        agent.pane.clone()
                    };
                    if ((agent.state == "done" && !ids.is_empty()) || agent.state == "failed")
                        && !close_target.is_empty()
                    {
                        let _ = self.terminals.close(&close_target);
                    }
                    let session_dir = self.service_config.agent_session_dir(&id);
                    if session_dir.exists() {
                        std::fs::remove_dir_all(&session_dir)
                            .map_err(|e| format!("failed to remove session dir for {id}: {e}"))?;
                    }
                    registry.remove(&id);
                    self.removed_agents
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(id.clone());
                    cleaned.push(id);
                }
            }
            self.save_registry(&registry)?;
            Ok(cleaned)
        })();
        self.log_err("clean", result)
    }

    /// Requirement 5 (`focus`):
    /// Only selects/focuses the tab or pane in Shepherd's native UI (`agent.focus` / `tab.focus` / `pane.focus`).
    pub fn focus(&self, id: &str) -> Result<Agent, String> {
        let result = (|| {
            let registry = self.load_registry()?;
            let Some(agent) = registry.result(id).cloned() else {
                return Err(format!("unknown agent {id:?}"));
            };
            let primary = if !agent.pane.is_empty() {
                agent.pane.as_str()
            } else if !agent.tab.is_empty() {
                agent.tab.as_str()
            } else {
                agent.id.as_str()
            };
            if self.terminals.focus(&agent.id).is_err() {
                self.terminals
                    .focus(primary)
                    .map_err(|e| format!("failed to focus agent {id}: {e}"))?;
            }
            Ok(agent.public())
        })();
        self.log_err("focus", result)
    }
}

pub const MAX_DELIVERED_MESSAGE_LEN: usize = 500;

pub fn format_shep_sender(from: &str, registry: &AgentRegistryFile) -> String {
    if let Some(agent) = registry.agents.iter().find(|a| a.id == from) {
        if let Some((role, num)) = agent.id.rsplit_once('-') {
            if !role.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                return format!("{role}#{num}");
            }
        }
        return format!("{}#{}", agent.role, agent.seq);
    }
    if let Some((role, num)) = from.rsplit_once('-') {
        if !role.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            return format!("{role}#{num}");
        }
    }
    from.to_owned()
}

pub fn format_shep_content(content: &str) -> String {
    let replaced = content.replace("\r\n", " ").replace(['\n', '\r'], " ");
    let char_count = replaced.chars().count();
    if char_count > MAX_DELIVERED_MESSAGE_LEN {
        let truncated: String = replaced.chars().take(MAX_DELIVERED_MESSAGE_LEN).collect();
        format!("{truncated}…")
    } else {
        replaced
    }
}

pub fn format_shep_message(from: &str, content: &str, registry: &AgentRegistryFile) -> String {
    let sender = format_shep_sender(from, registry);
    let clean_content = format_shep_content(content);
    format!("[shep] from {sender}: {clean_content}")
}

pub fn format_shep_topic_message(msg: &Message, registry: &AgentRegistryFile) -> String {
    let sender = format_shep_sender(&msg.from, registry);
    let clean_content = format_shep_content(&msg.content);
    format!(
        "[shep] topic {}[{}]#{} from {}: {}",
        msg.topic, msg.partition, msg.offset, sender, clean_content
    )
}

pub(crate) fn status_query_target(agent: &Agent) -> String {
    if !agent.pane.is_empty() {
        agent.pane.clone()
    } else {
        agent.id.clone()
    }
}

pub fn is_missing_terminal_error(err: &TerminalError) -> bool {
    match err {
        TerminalError::Api { code, message } => {
            matches!(
                code.as_str(),
                "not_found" | "agent_not_found" | "pane_not_found" | "tab_not_found"
            ) || message.contains("not found")
        }
        TerminalError::Protocol(msg) => msg.contains("not found"),
        TerminalError::Io(io) => io.kind() == std::io::ErrorKind::NotFound,
        TerminalError::Config(_) => false,
    }
}

pub fn validate_plain_agent_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains('\0')
    {
        return Err(format!(
            "invalid agent id {id:?}: must be a plain name without '/' or '..'"
        ));
    }
    let mut components = Path::new(id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(format!(
            "invalid agent id {id:?}: must be a single normal path component"
        )),
    }
}
