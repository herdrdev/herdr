use crate::api::schema::conversations::{
    AgentConversationReportParams, AgentConversationRespondParams, ConversationAvailability,
    ConversationCapability, ConversationPage, ConversationReadResult, ConversationReasonCode,
    ConversationRespondReason,
};
use crate::api::schema::{AgentConversationReadParams, ResponseResult};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

impl App {
    pub(super) fn handle_agent_conversation_read(
        &mut self,
        id: String,
        params: AgentConversationReadParams,
    ) -> String {
        self.handle_agent_conversation_read_internal(id, params, false)
    }

    pub(super) fn handle_agent_conversation_metadata(
        &mut self,
        id: String,
        params: AgentConversationReadParams,
    ) -> String {
        self.handle_agent_conversation_read_internal(id, params, true)
    }

    fn handle_agent_conversation_read_internal(
        &mut self,
        id: String,
        params: AgentConversationReadParams,
        metadata_only: bool,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(error) => return encode_error_body(id, self.agent_target_error_body(error)),
        };
        let Some(workspace) = self.state.workspaces.get(resolved.ws_idx) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(pane_state) = workspace.pane_state(resolved.pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(terminal) = self.state.terminals.get(&pane_state.attached_terminal_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let display_root = terminal.cwd.clone();
        let Some(agent) = terminal.effective_agent_label() else {
            return encode_error(
                id,
                "conversation_no_session",
                "structured Chat requires an active provider session",
            );
        };
        let Some(provider) = provider_for_label(agent) else {
            return encode_error(
                id,
                "conversation_unsupported",
                "structured Chat is not supported for this provider",
            );
        };
        let Some(session_parts) =
            super::super::creation::terminal_effective_session_parts(terminal)
        else {
            return encode_error(
                id,
                "conversation_no_session",
                "structured Chat requires an active provider session",
            );
        };
        let session_key = crate::app::conversation_sources::session_identity_key(
            &session_parts.0,
            &session_parts.1,
            session_parts.2.kind_name(),
            &session_parts.3,
        );
        let Some(source_entry) = self
            .state
            .conversation_sources
            .current_for(resolved.pane_id, Some(&session_key))
        else {
            return encode_error(
                id,
                "conversation_transcript_missing",
                "the provider transcript is not available yet",
            );
        };
        let Some(transcript) = source_entry.transcript_ref().cloned() else {
            return encode_error(
                id,
                "conversation_transcript_missing",
                "the provider transcript is not available yet",
            );
        };
        let Some(conversation_id) = source_entry.conversation_handle().map(str::to_string) else {
            return encode_error(
                id,
                "conversation_transcript_missing",
                "the provider transcript is not available yet",
            );
        };

        let reader = self
            .conversation_readers
            .entry(resolved.pane_id)
            .or_insert_with(|| {
                crate::agent_conversation::ConversationReader::new(
                    provider,
                    conversation_id.clone(),
                    &conversation_id,
                    1,
                )
            });
        if reader.session_id() != conversation_id {
            *reader = crate::agent_conversation::ConversationReader::new(
                provider,
                conversation_id.clone(),
                &conversation_id,
                1,
            );
        }
        reader.set_display_root(&display_root);
        let outcome = if metadata_only {
            reader.read_metadata(&transcript)
        } else {
            reader.read(
                &transcript,
                params.cursor.as_deref(),
                params.direction,
                params.limit,
            )
        };
        if outcome.reset {
            return encode_success(
                id,
                ResponseResult::AgentConversationRead {
                    read: ConversationReadResult::ResetRequired {
                        session: outcome.session,
                        reader_generation: outcome.generation,
                    },
                },
            );
        }
        if let Some(reason) = outcome.capability_reason {
            return encode_error(id, reason_code(reason), safe_reason_message(reason));
        }
        let Some(page) = outcome.page else {
            return encode_error(
                id,
                "conversation_unavailable",
                "structured Chat is temporarily unavailable",
            );
        };
        let page = ConversationPage {
            provider: provider_label(provider).into(),
            session: outcome.session,
            capability: ConversationCapability {
                availability: ConversationAvailability::Supported,
                reason: ConversationReasonCode::Ready,
                message: None,
            },
            items: page.items,
            next_cursor: page.next_cursor,
            previous_cursor: page.previous_cursor,
            has_older: page.has_older,
            revision: page.revision,
            reader_generation: outcome.generation,
        };
        encode_success(
            id,
            ResponseResult::AgentConversationRead {
                read: ConversationReadResult::Page { page },
            },
        )
    }
    pub(super) fn handle_agent_conversation_report(
        &mut self,
        id: String,
        params: AgentConversationReportParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id).cloned())
        else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let token_ok = self
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.integration_token.as_deref())
            .is_some_and(|expected| expected == params.integration_token);
        if !token_ok {
            return encode_error(
                id,
                "invalid_integration_token",
                "conversation report rejected: missing or stale integration token",
            );
        }
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(agent) = super::super::api_helpers::normalize_reported_agent_label(&params.agent)
        else {
            return encode_error(id, "invalid_agent", "invalid agent label");
        };
        if terminal.effective_agent_label() != Some(agent.as_str()) {
            return encode_error(
                id,
                "conversation_session_mismatch",
                "conversation report belongs to a different active provider session",
            );
        }
        if !matches!(agent.as_str(), "pi" | "omp" | "codex" | "claude") {
            return encode_error(
                id,
                "conversation_unsupported",
                "structured Chat is not supported for this provider",
            );
        }
        let Some(session_ref) = crate::agent_resume::session_ref_from_report(
            &params.source,
            &agent,
            params.agent_session_id.clone(),
            params.agent_session_path.clone(),
        ) else {
            return encode_error(
                id,
                "conversation_session_mismatch",
                "conversation report is missing its active provider session",
            );
        };
        let Some(current_parts) =
            super::super::creation::terminal_effective_session_parts(terminal)
        else {
            return encode_error(
                id,
                "conversation_no_session",
                "structured Chat requires an active provider session",
            );
        };
        let current_identity = crate::app::conversation_sources::session_identity_key(
            &current_parts.0,
            &current_parts.1,
            current_parts.2.kind_name(),
            &current_parts.3,
        );
        let reported_identity = crate::app::conversation_sources::session_identity_key(
            &params.source,
            &agent,
            session_ref.kind.kind_name(),
            &session_ref.value,
        );
        if current_identity != reported_identity {
            return encode_error(
                id,
                "conversation_session_mismatch",
                "conversation report belongs to a different active provider session",
            );
        }
        if !self.accept_conversation_overlay(
            pane_id,
            params.seq,
            params.native_id,
            params.entry_id,
            params.turn_id,
            params.timestamp_ms,
            params.payload,
        ) {
            return encode_error(
                id,
                "conversation_report_rejected",
                "conversation live item exceeded the engine limits or was invalid",
            );
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_agent_conversation_respond(
        &mut self,
        id: String,
        params: AgentConversationRespondParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(error) => return encode_error_body(id, self.agent_target_error_body(error)),
        };
        let Some(workspace) = self.state.workspaces.get(resolved.ws_idx) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(pane_state) = workspace.pane_state(resolved.pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(terminal) = self.state.terminals.get(&pane_state.attached_terminal_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let display_root = terminal.cwd.clone();
        let Some(provider_name) = terminal.effective_agent_label() else {
            return encode_error(
                id,
                "conversation_no_session",
                "structured Chat requires an active provider session",
            );
        };
        let Some(provider) = provider_for_label(provider_name) else {
            return encode_error(
                id,
                "conversation_unsupported",
                "structured Chat is not supported for this provider",
            );
        };
        let Some(parts) = super::super::creation::terminal_effective_session_parts(terminal) else {
            return encode_error(
                id,
                "conversation_no_session",
                "structured Chat requires an active provider session",
            );
        };
        let session_key = crate::app::conversation_sources::session_identity_key(
            &parts.0,
            &parts.1,
            parts.2.kind_name(),
            &parts.3,
        );
        let Some(source_entry) = self
            .state
            .conversation_sources
            .current_for(resolved.pane_id, Some(&session_key))
        else {
            return encode_error(
                id,
                "conversation_transcript_missing",
                "the provider transcript is not available yet",
            );
        };
        let Some(transcript) = source_entry.transcript_ref().cloned() else {
            return encode_error(
                id,
                "conversation_transcript_missing",
                "the provider transcript is not available yet",
            );
        };
        let Some(conversation_id) = source_entry.conversation_handle().map(str::to_string) else {
            return encode_error(
                id,
                "conversation_transcript_missing",
                "the provider transcript is not available yet",
            );
        };
        if params.session.id != conversation_id {
            return respond_result(
                id,
                &params,
                false,
                ConversationRespondReason::SessionMismatch,
            );
        }

        let reader = self
            .conversation_readers
            .entry(resolved.pane_id)
            .or_insert_with(|| {
                crate::agent_conversation::ConversationReader::new(
                    provider,
                    conversation_id.clone(),
                    &conversation_id,
                    1,
                )
            });
        if reader.session_id() != conversation_id {
            *reader = crate::agent_conversation::ConversationReader::new(
                provider,
                conversation_id.clone(),
                &conversation_id,
                1,
            );
        }
        reader.set_display_root(&display_root);
        let outcome = reader.read(
            &transcript,
            None,
            crate::api::schema::ConversationPageDirection::Newest,
            crate::api::schema::CONVERSATION_MAX_LIMIT,
        );
        if outcome.reset || outcome.generation != params.reader_generation {
            return respond_result(
                id,
                &params,
                false,
                ConversationRespondReason::SessionMismatch,
            );
        }
        let Some(page) = outcome.page else {
            return respond_result(
                id,
                &params,
                false,
                ConversationRespondReason::UnknownRequest,
            );
        };
        let Some((status, structured_response, decision_allowed, selected_decision)) =
            page.items.iter().find_map(|item| match &item.payload {
                crate::api::schema::ConversationItemPayload::Approval {
                    request_id,
                    status,
                    decisions,
                    structured_response,
                    selected_decision,
                    ..
                } if request_id == &params.request_id => Some((
                    *status,
                    *structured_response,
                    decisions
                        .iter()
                        .any(|decision| decision.id == params.decision_id),
                    selected_decision.as_deref(),
                )),
                _ => None,
            })
        else {
            return respond_result(
                id,
                &params,
                false,
                ConversationRespondReason::UnknownRequest,
            );
        };
        if status == crate::api::schema::ApprovalStatus::Resolved {
            return respond_result(
                id,
                &params,
                selected_decision == Some(params.decision_id.as_str()),
                ConversationRespondReason::AlreadyResolved,
            );
        }
        if !decision_allowed {
            return respond_result(
                id,
                &params,
                false,
                ConversationRespondReason::ConflictingDecision,
            );
        }
        if !structured_response {
            return encode_error(
                id,
                "conversation_approval_unsupported",
                "this provider approval must be answered in Terminal",
            );
        }
        let _ = provider;
        encode_error(
            id,
            "conversation_approval_unsupported",
            "this provider has no safe structured approval responder",
        )
    }
}
fn respond_result(
    id: String,
    params: &AgentConversationRespondParams,
    accepted: bool,
    reason: ConversationRespondReason,
) -> String {
    encode_success(
        id,
        ResponseResult::AgentConversationRespond {
            result: crate::api::schema::ConversationRespondResult {
                request_id: params.request_id.clone(),
                decision_id: params.decision_id.clone(),
                accepted,
                reason,
            },
        },
    )
}

fn provider_for_label(label: &str) -> Option<crate::detect::Agent> {
    match label {
        "pi" => Some(crate::detect::Agent::Pi),
        "omp" => Some(crate::detect::Agent::Omp),
        "codex" => Some(crate::detect::Agent::Codex),
        "claude" => Some(crate::detect::Agent::Claude),
        _ => None,
    }
}

fn provider_label(provider: crate::detect::Agent) -> &'static str {
    match provider {
        crate::detect::Agent::Pi => "pi",
        crate::detect::Agent::Omp => "omp",
        crate::detect::Agent::Codex => "codex",
        crate::detect::Agent::Claude => "claude",
        _ => "unknown",
    }
}

fn reason_code(reason: ConversationReasonCode) -> &'static str {
    match reason {
        ConversationReasonCode::TranscriptMissing => "conversation_transcript_missing",
        ConversationReasonCode::TranscriptInvalid => "conversation_transcript_invalid",
        ConversationReasonCode::SourceUnreadable => "conversation_source_unreadable",
        ConversationReasonCode::NoSession => "conversation_no_session",
        ConversationReasonCode::AdapterMissing => "conversation_unsupported",
        ConversationReasonCode::Ready => "conversation_ready",
    }
}

fn safe_reason_message(reason: ConversationReasonCode) -> &'static str {
    match reason {
        ConversationReasonCode::TranscriptMissing => "the provider transcript is not available yet",
        ConversationReasonCode::TranscriptInvalid => {
            "the provider transcript is not valid for this provider"
        }
        ConversationReasonCode::SourceUnreadable => {
            "the provider transcript is temporarily unreadable"
        }
        ConversationReasonCode::NoSession => "structured Chat requires an active provider session",
        ConversationReasonCode::AdapterMissing => {
            "structured Chat is not supported for this provider"
        }
        ConversationReasonCode::Ready => "structured Chat is ready",
    }
}
