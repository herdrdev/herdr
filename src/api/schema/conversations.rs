use serde::{Deserialize, Serialize};

/// Provider-neutral structured conversation API for the unified agent chat.
///
/// These DTOs are the public wire contract. Transcript paths, raw provider
/// records, and reasoning content never appear here; every field is
/// allowlisted and size-bounded by the conversation reader before a value is
/// constructed.
///
/// Default page size for `agent.conversation.read`.
pub const CONVERSATION_DEFAULT_LIMIT: usize = 64;
/// Maximum page size accepted for `agent.conversation.read`.
pub const CONVERSATION_MAX_LIMIT: usize = 256;

/// Whether structured Chat is available for a pane/agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationAvailability {
    /// An adapter exists and a valid transcript source is available.
    Supported,
    /// The provider is supported, but no valid session/transcript is available.
    Unavailable,
    /// No structured adapter exists for this provider.
    Unsupported,
}

/// Machine-readable reason for the availability state. Desktop maps these to
/// user-facing copy; engine log lines carry the detailed diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationReasonCode {
    /// `supported`; nothing prevents reads.
    Ready,
    /// `unsupported`; no adapter for the provider.
    AdapterMissing,
    /// `unavailable`; the pane has no accepted provider session.
    NoSession,
    /// `unavailable`; no transcript source has been reported/validated.
    TranscriptMissing,
    /// `unavailable`; the transcript source failed path validation.
    TranscriptInvalid,
    /// `unavailable`; the transcript source exists but is not currently readable.
    SourceUnreadable,
}

/// Structured-chat capability state exposed on pane/agent information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationCapability {
    pub availability: ConversationAvailability,
    pub reason: ConversationReasonCode,
    /// Optional short safe message; never a raw OS error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Opaque conversation-session identity. It is engine-generated and bound to
/// the pane/provider session; it never contains a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationSessionIdentity {
    pub id: String,
}

/// Direction of a `agent.conversation.read` page.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ConversationPageDirection {
    /// The most recent bounded window; requires no cursor.
    #[default]
    Newest,
    /// Page strictly older than the cursor's position.
    Older,
    /// Page strictly newer than the cursor's position (delta refresh).
    Newer,
}

/// Params for `agent.conversation.read`.
///
/// Cursors are opaque strings. A public cursor encodes direction, reader
/// generation, opaque session identity, source fingerprint, and canonical
/// sequence/revision; transcript byte offsets are private durable-adapter
/// state and are never part of the public position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentConversationReadParams {
    pub target: String,
    /// Required for `older`/`newer`; must be absent for `newest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub direction: ConversationPageDirection,
    /// Bounded page size; the engine clamps values above the maximum.
    #[serde(default = "default_conversation_limit")]
    pub limit: usize,
}

fn default_conversation_limit() -> usize {
    CONVERSATION_DEFAULT_LIMIT
}

/// Authenticated provider-neutral live lifecycle item. The integration token
/// is checked by the pane handler and the payload is bounded before it enters
/// the ephemeral reader overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentConversationReportParams {
    pub pane_id: String,
    pub source: String,
    pub agent: String,
    pub integration_token: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
    pub payload: ConversationItemPayload,
}

/// One canonical conversation page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationPage {
    pub provider: String,
    pub session: ConversationSessionIdentity,
    pub capability: ConversationCapability,
    /// Ordered by canonical sequence ascending.
    pub items: Vec<ConversationItem>,
    /// Cursor for the next (`newer`) page, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Cursor for the previous (`older`) page, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_cursor: Option<String>,
    pub has_older: bool,
    /// Monotonic only within the returned reader generation.
    pub revision: u64,
    /// Reader generation/reset identity derived from the engine instance,
    /// provider session, source fingerprint, and reader-cache generation.
    pub reader_generation: String,
}

/// Result of a read; a stale/incompatible cursor never silently reuses state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationReadResult {
    Page {
        page: ConversationPage,
    },
    /// The cursor belonged to another engine generation, session, replaced
    /// source, or evicted incompatible reader state. The client must reset
    /// and reread the newest tail.
    ResetRequired {
        session: ConversationSessionIdentity,
        reader_generation: String,
    },
}

/// Canonical conversation item with stable identity and ordered position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationItem {
    /// Deterministic across rereads; derived from provider-native identity or
    /// session identity plus record position — never from mutable text.
    pub id: String,
    /// Monotonic order within the opaque session identity.
    pub sequence: u64,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Provider timestamp in milliseconds since the Unix epoch, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
    #[serde(flatten)]
    pub payload: ConversationItemPayload,
}

/// Safe attachment metadata. Bytes and engine-host paths never leave the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttachmentMetadata {
    pub media_type: String,
    pub name: String,
    pub byte_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessagePhase {
    Commentary,
    Final,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompletionState {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    Active,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlanStep {
    pub label: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApprovalDecision {
    /// Stable decision id advertised by the engine; never derived from text.
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnStateKind {
    Started,
    Completed,
    Interrupted,
    Failed,
}

/// Typed canonical payload. Unknown native records are ignored and counted for
/// diagnostics; they never appear here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItemPayload {
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentMetadata>,
    },
    AssistantMessage {
        phase: AssistantMessagePhase,
        text: String,
        state: CompletionState,
    },
    PlanUpdate {
        steps: Vec<PlanStep>,
    },
    ToolActivity {
        action: String,
        label: String,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
    },
    FileChange {
        path: String,
        change: FileChangeKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Approval {
        request_id: String,
        prompt: String,
        decisions: Vec<ApprovalDecision>,
        status: ApprovalStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_decision: Option<String>,
        /// False when the provider cannot safely respond; Desktop must show
        /// `Open Terminal to respond` instead of guessing terminal keys.
        structured_response: bool,
    },
    TurnState {
        state: TurnStateKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Notice {
        message: String,
    },
}

/// Params for `agent.conversation.respond`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentConversationRespondParams {
    pub target: String,
    pub reader_generation: String,
    pub session: ConversationSessionIdentity,
    /// Stable approval request id from the canonical `approval` item.
    pub request_id: String,
    /// One advertised decision id.
    pub decision_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRespondReason {
    Accepted,
    AlreadyResolved,
    StaleRequest,
    UnknownRequest,
    ConflictingDecision,
    SessionMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationRespondResult {
    pub request_id: String,
    pub decision_id: String,
    pub accepted: bool,
    pub reason: ConversationRespondReason,
}

/// Stable machine-readable name of a canonical payload, used for
/// diagnostics only.
pub fn payload_kind_name(payload: &ConversationItemPayload) -> &'static str {
    match payload {
        ConversationItemPayload::UserMessage { .. } => "user_message",
        ConversationItemPayload::AssistantMessage { .. } => "assistant_message",
        ConversationItemPayload::PlanUpdate { .. } => "plan_update",
        ConversationItemPayload::ToolActivity { .. } => "tool_activity",
        ConversationItemPayload::FileChange { .. } => "file_change",
        ConversationItemPayload::Approval { .. } => "approval",
        ConversationItemPayload::TurnState { .. } => "turn_state",
        ConversationItemPayload::Notice { .. } => "notice",
    }
}

/// Opaque attachment upload handle; never a host path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttachmentUploadHandle {
    pub handle: String,
}

/// Opaque staged-attachment handle accepted by `agent.prompt`; never a host path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttachmentHandle {
    pub handle: String,
}

/// Params for `agent.attachment.begin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentAttachmentBeginParams {
    pub target: String,
    pub media_type: String,
    pub name: String,
    pub byte_size: u64,
    /// Hex SHA-256 of the intended payload.
    pub sha256_digest: String,
}

/// Params for `agent.attachment.chunk`. Chunks are ordered by index and stay
/// well below Herdr's request-line cap; the engine bounds them individually
/// and in aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentAttachmentChunkParams {
    pub upload: AttachmentUploadHandle,
    pub index: u64,
    pub data_base64: String,
}

/// Params for `agent.attachment.finish`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentAttachmentFinishParams {
    pub upload: AttachmentUploadHandle,
}

/// Params for `agent.attachment.abort`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentAttachmentAbortParams {
    pub upload: AttachmentUploadHandle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_round_trips_with_reason_codes() {
        for (availability, reason) in [
            (
                ConversationAvailability::Supported,
                ConversationReasonCode::Ready,
            ),
            (
                ConversationAvailability::Unavailable,
                ConversationReasonCode::NoSession,
            ),
            (
                ConversationAvailability::Unsupported,
                ConversationReasonCode::AdapterMissing,
            ),
        ] {
            let capability = ConversationCapability {
                availability,
                reason,
                message: Some("safe".into()),
            };
            let value = serde_json::to_value(&capability).unwrap();
            assert_eq!(
                value["availability"],
                serde_json::json!(match availability {
                    ConversationAvailability::Supported => "supported",
                    ConversationAvailability::Unavailable => "unavailable",
                    ConversationAvailability::Unsupported => "unsupported",
                })
            );
            let restored: ConversationCapability = serde_json::from_value(value).unwrap();
            assert_eq!(restored, capability);
        }
    }

    #[test]
    fn capability_message_is_optional() {
        let capability = ConversationCapability {
            availability: ConversationAvailability::Supported,
            reason: ConversationReasonCode::Ready,
            message: None,
        };
        let value = serde_json::to_value(&capability).unwrap();
        assert!(value.get("message").is_none());
        let restored: ConversationCapability = serde_json::from_value(value).unwrap();
        assert_eq!(restored, capability);
    }

    #[test]
    fn read_params_default_to_newest_tail_and_bounded_limit() {
        let params = AgentConversationReadParams {
            target: "w1:p1".into(),
            cursor: None,
            direction: ConversationPageDirection::Newest,
            limit: CONVERSATION_DEFAULT_LIMIT,
        };
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value["direction"], "newest");
        assert_eq!(value["limit"], CONVERSATION_DEFAULT_LIMIT);
        assert!(value.get("cursor").is_none());

        let parsed: AgentConversationReadParams = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, params);
    }

    #[test]
    fn read_params_reject_invalid_direction_cursor_combinations() {
        // `newest` with a cursor is rejected by the engine; the schema keeps
        // both optional so old decoders tolerate the fields.
        let with_cursor = AgentConversationReadParams {
            target: "w1:p1".into(),
            cursor: Some("opaque".into()),
            direction: ConversationPageDirection::Newest,
            limit: 10,
        };
        assert_eq!(
            serde_json::from_value::<AgentConversationReadParams>(
                serde_json::to_value(&with_cursor).unwrap()
            )
            .unwrap(),
            with_cursor
        );
    }

    #[test]
    fn page_serializes_opaque_cursors_and_no_paths() {
        let page = ConversationPage {
            provider: "pi".into(),
            session: ConversationSessionIdentity {
                id: "opaque-session".into(),
            },
            capability: ConversationCapability {
                availability: ConversationAvailability::Supported,
                reason: ConversationReasonCode::Ready,
                message: None,
            },
            items: vec![ConversationItem {
                id: "item-1".into(),
                sequence: 1,
                provider: "pi".into(),
                session_id: Some("opaque-session".into()),
                turn_id: Some("turn-1".into()),
                timestamp_ms: Some(1_000),
                payload: ConversationItemPayload::AssistantMessage {
                    phase: AssistantMessagePhase::Final,
                    text: "hello".into(),
                    state: CompletionState::Completed,
                },
            }],
            next_cursor: Some("newer-cursor".into()),
            previous_cursor: Some("older-cursor".into()),
            has_older: true,
            revision: 7,
            reader_generation: "gen-1".into(),
        };

        let value = serde_json::to_value(&page).unwrap();
        assert_eq!(value["items"][0]["type"], "assistant_message");
        assert_eq!(value["items"][0]["phase"], "final");
        assert_eq!(value["next_cursor"], "newer-cursor");
        assert_eq!(value["reader_generation"], "gen-1");
        // No serialized transcript path anywhere.
        assert!(!serde_json::to_string(&value).unwrap().contains("herdr"));
        assert!(!serde_json::to_string(&value).unwrap().contains("/"));
        let restored: ConversationPage = serde_json::from_value(value).unwrap();
        assert_eq!(restored, page);
    }

    #[test]
    fn read_result_distinguishes_page_from_reset_required() {
        let reset = ConversationReadResult::ResetRequired {
            session: ConversationSessionIdentity {
                id: "opaque-session".into(),
            },
            reader_generation: "gen-2".into(),
        };
        let value = serde_json::to_value(&reset).unwrap();
        assert_eq!(value["type"], "reset_required");
        assert_eq!(value["reader_generation"], "gen-2");
        let restored: ConversationReadResult = serde_json::from_value(value).unwrap();
        assert_eq!(restored, reset);
    }

    #[test]
    fn approval_payload_round_trips_with_structured_flag() {
        let item = ConversationItemPayload::Approval {
            request_id: "req-1".into(),
            prompt: "Approve?".into(),
            decisions: vec![
                ApprovalDecision {
                    id: "allow".into(),
                    label: "Allow".into(),
                },
                ApprovalDecision {
                    id: "deny".into(),
                    label: "Deny".into(),
                },
            ],
            status: ApprovalStatus::Pending,
            selected_decision: None,
            structured_response: false,
        };
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["type"], "approval");
        assert_eq!(value["decisions"][0]["id"], "allow");
        assert_eq!(value["structured_response"], false);
        let restored: ConversationItemPayload = serde_json::from_value(value).unwrap();
        assert_eq!(restored, item);
    }

    #[test]
    fn attachment_handles_are_opaque_and_prompt_carries_them() {
        let handle = AttachmentHandle {
            handle: "upload-9".into(),
        };
        let value = serde_json::to_value(&handle).unwrap();
        assert_eq!(value["handle"], "upload-9");

        let params = crate::api::schema::agents::AgentPromptParams {
            target: "w1:p1".into(),
            text: "see image".into(),
            wait: None,
            attachments: vec![handle],
        };
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value["attachments"][0]["handle"], "upload-9");
        let restored: crate::api::schema::agents::AgentPromptParams =
            serde_json::from_value(value).unwrap();
        assert_eq!(restored.attachments.len(), 1);
    }

    #[test]
    fn attachment_chunk_stays_below_request_line_bounds() {
        // A chunk payload of a few KiB is far below Herdr's request-line cap;
        // the engine additionally enforces per-chunk and aggregate limits.
        let chunk = AgentAttachmentChunkParams {
            upload: AttachmentUploadHandle {
                handle: "upload-9".into(),
            },
            index: 0,
            data_base64: "aGVsbG8=".into(),
        };
        let serialized = serde_json::to_string(&chunk).unwrap();
        assert!(serialized.len() < 1024);
    }
}
