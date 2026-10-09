use super::interaction_profiles as profiles;
use super::responses::{encode_error, encode_success};
use crate::api::{interaction_journal as journal, schema::*};
use crate::app::App;

fn validate_observation(
    expected: &InteractionObservation,
    current: &InteractionObservation,
) -> Result<(), &'static str> {
    if expected.server_instance_id != current.server_instance_id
        || expected.runtime_pid != current.runtime_pid
        || expected.terminal_id != current.terminal_id
        || expected.agent != current.agent
        || expected.agent_session != current.agent_session
    {
        return Err("stale_identity");
    }
    if expected.state_change_seq != current.state_change_seq {
        return Err("stale_state");
    }
    if expected.content_digest != current.content_digest {
        return Err("stale_content");
    }
    if expected.style_digest != current.style_digest {
        return Err("stale_style");
    }
    Ok(())
}

fn validate_custom_parent(
    parent: &journal::ValidatedLineage,
    current: &InteractionObservation,
    dialog: &InteractionDialog,
) -> Result<(), &'static str> {
    let expected = &parent.request.expected;
    if expected.server_instance_id != current.server_instance_id
        || expected.runtime_pid != current.runtime_pid
        || expected.terminal_id != current.terminal_id
        || expected.agent != current.agent
        || expected.agent_session != current.agent_session
    {
        return Err("stale_custom_parent_identity");
    }
    if expected.state_change_seq != current.state_change_seq {
        return Err("stale_custom_parent_state");
    }
    if expected.style_digest.is_none()
        || !matches!(&parent.request.action,InteractionAction::BeginCustom { option_id } if option_id == "choice-4")
        || parent.dialog.profile != dialog.profile
        || parent.dialog.phase != "choose"
        || dialog.phase != "custom_entry"
        || parent.dialog.question != dialog.question
        || parent.dialog.options != dialog.options
    {
        return Err("custom_parent_dialog_mismatch");
    }
    Ok(())
}

impl App {
    fn interaction_snapshot(
        &self,
        target: &str,
    ) -> Result<(InteractionObservation, String, String), &'static str> {
        let resolved = self
            .resolve_terminal_target(target)
            .map_err(|_| "agent_not_found")?;
        let agent = self
            .agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or("agent_not_found")?;
        let (runtime, _) = self
            .lookup_runtime(resolved.ws_idx, resolved.pane_id)
            .ok_or("agent_not_found")?;
        let (text, ansi) = runtime
            .interaction_snapshot()
            .ok_or("runtime_snapshot_unavailable")?;
        Ok((
            InteractionObservation {
                terminal_id: agent.terminal_id,
                server_instance_id: journal::server_instance_id().into(),
                runtime_pid: runtime.child_pid().ok_or("runtime_identity_unavailable")?,
                agent: agent.agent.ok_or("agent_identity_unavailable")?,
                agent_session: agent.agent_session.ok_or("agent_session_unavailable")?,
                state_change_seq: agent.state_change_seq,
                content_digest: journal::digest(text.as_bytes()),
                style_digest: Some(journal::digest(ansi.as_bytes())),
            },
            text,
            ansi,
        ))
    }

    fn interaction_dialog(
        &self,
        target: &str,
        text: &str,
        ansi: &str,
    ) -> Option<InteractionDialog> {
        if !profiles::enabled() {
            return None;
        }
        let resolved = self.resolve_terminal_target(target).ok()?;
        let agent = self.agent_info(resolved.ws_idx, resolved.pane_id)?;
        if agent.agent.as_deref() != Some("claude") || agent.agent_status != AgentStatus::Blocked {
            return None;
        }
        profiles::recognize(text, ansi)
    }

    pub(super) fn handle_interaction_get(&mut self, id: String, params: AgentTarget) -> String {
        match self.interaction_snapshot(&params.target) {
            Ok((observation, text, ansi)) => {
                let dialog = self.interaction_dialog(&params.target, &text, &ansi);
                encode_success(
                    id,
                    ResponseResult::AgentInteraction {
                        observation,
                        supported: dialog.is_some(),
                        unsupported_reason: if dialog.is_some() {
                            String::new()
                        } else {
                            "no_verified_native_interaction_profile".into()
                        },
                        dialog,
                    },
                )
            }
            Err(code) => encode_error(
                id,
                code,
                "exact runtime identity and agent session are required",
            ),
        }
    }

    pub(super) fn handle_interaction_submit(
        &mut self,
        id: String,
        params: InteractionSubmitParams,
    ) -> String {
        let root = crate::session::data_dir().join("interaction-operations-v1");
        let receipt = journal::dispatch_guarded(
            &root,
            &params,
            || {
                // This handler runs on the app's serialized API dispatch. PTY output, humans and
                // other terminal writers remain independent; this is an observation check, not
                // a transaction with the external agent's input processing.
                let (current, text, ansi) =
                    self.interaction_snapshot(&params.expected.terminal_id)?;
                validate_observation(&params.expected, &current)?;
                let dialog = self
                    .interaction_dialog(&params.expected.terminal_id, &text, &ansi)
                    .ok_or("unsupported_interaction_profile")?;
                let compiled = profiles::compile(&dialog, &params.action)?;
                let resolved = self
                    .resolve_terminal_target(&params.expected.terminal_id)
                    .map_err(|_| "agent_not_found")?;
                let (runtime, _) = self
                    .lookup_runtime(resolved.ws_idx, resolved.pane_id)
                    .ok_or("agent_not_found")?;
                let bytes = match compiled {
                    profiles::CompiledInteraction::Key(key) => {
                        runtime.encode_terminal_key(key.into())
                    }
                    profiles::CompiledInteraction::CustomText(text) => {
                        if !runtime.bracketed_paste_enabled() {
                            return Err("bracketed_paste_required");
                        }
                        let InteractionAction::SubmitCustom {
                            parent_operation_id,
                            ..
                        } = &params.action
                        else {
                            return Err("unsupported_interaction_phase");
                        };
                        let parent = journal::validated_lineage(&root, parent_operation_id)
                            .map_err(|_| "invalid_custom_parent")?
                            .ok_or("invalid_custom_parent")?;
                        let receipt = journal::lookup(&root, parent_operation_id)
                            .map_err(|_| "invalid_custom_parent")?
                            .ok_or("invalid_custom_parent")?;
                        if receipt.outcome != InteractionOutcome::Enqueued {
                            return Err("unresolved_custom_parent");
                        }
                        validate_custom_parent(&parent, &current, &dialog)?;
                        let bytes = crate::app::api_helpers::encode_api_submission(runtime, &text);
                        journal::claim_custom_child(&root, parent_operation_id, &params)
                            .map_err(|_| "custom_parent_consumed")?;
                        bytes
                    }
                };
                Ok((bytes, dialog))
            },
            |bytes| {
                let resolved = self
                    .resolve_terminal_target(&params.expected.terminal_id)
                    .map_err(|_| std::io::Error::other("agent target disappeared"))?;
                let runtime = self
                    .lookup_runtime_sender(resolved.ws_idx, resolved.pane_id)
                    .ok_or_else(|| std::io::Error::other("agent runtime disappeared"))?;
                runtime
                    .try_send_bytes(bytes::Bytes::from(bytes))
                    .map_err(std::io::Error::other)
            },
        );
        match receipt {
            Ok(receipt) => encode_success(id, ResponseResult::AgentInteractionReceipt { receipt }),
            Err(error) => encode_error(id, "interaction_journal_error", error.to_string()),
        }
    }

    pub(super) fn handle_interaction_receipt(
        &mut self,
        id: String,
        params: InteractionReceiptParams,
    ) -> String {
        if !journal::valid_operation_id(&params.operation_id) {
            return encode_error(
                id,
                "invalid_operation_id",
                "operation_id must contain 1..128 ASCII letters, digits, underscores or hyphens",
            );
        }
        match journal::lookup(
            &crate::session::data_dir().join("interaction-operations-v1"),
            &params.operation_id,
        ) {
            Ok(Some(receipt)) => {
                encode_success(id, ResponseResult::AgentInteractionReceipt { receipt })
            }
            Ok(None) => encode_error(id, "operation_not_found", "no durable intent exists"),
            Err(error) => encode_error(id, "interaction_journal_error", error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation() -> InteractionObservation {
        InteractionObservation {
            terminal_id: "t1".into(),
            server_instance_id: "instance1".into(),
            runtime_pid: 123,
            agent: "codex".into(),
            state_change_seq: 7,
            content_digest: journal::digest(b"dialog"),
            style_digest: None,
            agent_session: AgentSessionInfo {
                source: "hook".into(),
                agent: "codex".into(),
                kind: crate::agent_resume::AgentSessionRefKind::Id,
                value: "session1".into(),
            },
        }
    }
    #[test]
    fn interaction_guard_checks_session_terminal_state_and_content_independently() {
        let expected = observation();
        assert_eq!(validate_observation(&expected, &expected), Ok(()));
        let mut current = expected.clone();
        current.server_instance_id = "restarted".into();
        assert_eq!(
            validate_observation(&expected, &current),
            Err("stale_identity")
        );
        let mut current = expected.clone();
        current.runtime_pid += 1;
        assert_eq!(
            validate_observation(&expected, &current),
            Err("stale_identity")
        );
        let mut current = expected.clone();
        current.agent_session.value = "new-session".into();
        assert_eq!(
            validate_observation(&expected, &current),
            Err("stale_identity")
        );
        let mut current = expected.clone();
        current.terminal_id = "t2".into();
        assert_eq!(
            validate_observation(&expected, &current),
            Err("stale_identity")
        );
        let mut current = expected.clone();
        current.agent = "claude".into();
        assert_eq!(
            validate_observation(&expected, &current),
            Err("stale_identity")
        );
        let mut current = expected.clone();
        current.state_change_seq += 1;
        assert_eq!(
            validate_observation(&expected, &current),
            Err("stale_state")
        );
        let mut current = expected.clone();
        current.content_digest = journal::digest(b"changed dialog");
        assert_eq!(
            validate_observation(&expected, &current),
            Err("stale_content")
        );
    }
    #[test]
    fn interaction_schema_rejects_raw_keys_and_unknown_action_fields() {
        for value in [
            serde_json::json!({"type":"keys", "keys":["enter"]}),
            serde_json::json!({"type":"choose", "option_id":"1", "keys":["enter"]}),
            serde_json::json!({"type":"submit_custom", "text":"answer"}),
        ] {
            assert!(serde_json::from_value::<InteractionAction>(value).is_err());
        }
    }
}
