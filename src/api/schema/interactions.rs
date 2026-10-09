use serde::{Deserialize, Serialize};

/// Exact runtime identity and bottom-buffer observation; names/pane aliases are not identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractionObservation {
    pub terminal_id: String,
    pub server_instance_id: String,
    pub runtime_pid: u32,
    pub agent: String,
    pub agent_session: super::AgentSessionInfo,
    pub state_change_seq: u64,
    /// SHA-256 of the complete UTF-8 detection buffer, without trimming or normalization.
    pub content_digest: String,
    /// SHA-256 of the paired locked bottom-buffer ANSI snapshot; required by styled profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InteractionAction {
    Choose {
        option_id: String,
    },
    FreeText {
        text: String,
    },
    BeginCustom {
        option_id: String,
    },
    SubmitCustom {
        text: String,
        parent_operation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractionSubmitParams {
    pub operation_id: String,
    /// SHA-256 of compact serde JSON for [operation_id, expected, action], in that order.
    pub payload_digest: String,
    pub expected: InteractionObservation,
    pub action: InteractionAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractionReceiptParams {
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InteractionOutcome {
    Rejected,
    /// Accepted by the PTY queue; no assertion about delivery or application acceptance.
    Enqueued,
    /// An intent exists without a reliable receipt; retry must never dispatch again.
    UnknownDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InteractionReceipt {
    pub operation_id: String,
    pub payload_digest: String,
    pub outcome: InteractionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InteractionOption {
    pub option_id: String,
    pub label: String,
    pub custom: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InteractionDialog {
    pub profile: String,
    pub phase: String,
    pub question: String,
    pub options: Vec<InteractionOption>,
    pub selected_option_id: String,
    pub supported_actions: Vec<String>,
}
