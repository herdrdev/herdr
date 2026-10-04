use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::{AgentStatus, ReadFormat, ReadSource};
use super::panes::{ClosingBlockDecision, ClosingBlockItem};

pub use crate::agent_state::{AgentReportPayload, AgentStateSnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentStateParams {
    pub target: String,
}

/// Flattening filters unknown fields instead of rejecting them, so a typo
/// through the socket API would report success and record nothing. The payload
/// is kept whole and deserialized in one step to keep both paths strict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentReportParams {
    pub target: String,
    #[serde(default)]
    pub status_text: Option<Option<String>>,
    #[serde(default)]
    pub goal: Option<Option<String>>,
    #[serde(default)]
    pub tasks: Option<Vec<crate::agent_state::AgentTask>>,
    #[serde(default)]
    pub subagents: Option<Vec<crate::agent_state::AgentSubagent>>,
}

impl AgentReportParams {
    pub fn payload(&self) -> AgentReportPayload {
        AgentReportPayload {
            status_text: self.status_text.clone(),
            goal: self.goal.clone(),
            tasks: self.tasks.clone(),
            subagents: self.subagents.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentReadParams {
    pub target: String,
    pub source: ReadSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(default)]
    pub format: ReadFormat,
    #[serde(default = "super::common::default_true")]
    pub strip_ansi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSearchParams {
    pub query: String,
    #[serde(default = "super::common::default_true")]
    pub include_session: bool,
    #[serde(default = "default_agent_search_limit")]
    pub limit: u16,
}

fn default_agent_search_limit() -> u16 {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSearchSource {
    Title,
    Tail,
    Session,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSearchHit {
    pub target: String,
    pub title: String,
    pub source: AgentSearchSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub context: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview: Vec<String>,
    pub agent: AgentInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSendKeysParams {
    pub target: String,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentWaitParams {
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptWaitOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) submission_deadline: Option<std::time::Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentRenameParams {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentViewSetParams {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<AgentViewFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<AgentViewSort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentViewClearParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentViewFilter {
    All {
        filters: Vec<AgentViewFilter>,
    },
    Any {
        filters: Vec<AgentViewFilter>,
    },
    Not {
        filter: Box<AgentViewFilter>,
    },
    Eq {
        field: AgentViewField,
        value: AgentViewValue,
    },
    In {
        field: AgentViewField,
        values: Vec<AgentViewValue>,
    },
    Exists {
        field: AgentViewField,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewField {
    Builtin(AgentViewBuiltinField),
    Token { token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewBuiltinField {
    Status,
    WorkspaceId,
    TabId,
    PaneId,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewValue {
    String(String),
    Bool(bool),
    Number(u64),
    Context { context: AgentViewContext },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewContext {
    CurrentWorkspaceId,
    CurrentTabId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentViewSort {
    pub field: AgentViewSortField,
    #[serde(default)]
    pub order: AgentViewSortOrder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewSortField {
    Builtin(AgentViewBuiltinSortField),
    Token { token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewBuiltinSortField {
    WorkspaceOrder,
    TabOrder,
    PaneOrder,
    Attention,
    Status,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewSortOrder {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentStartParams {
    pub name: String,
    pub kind: String,
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Startup timeout in milliseconds. Values must be greater than 3000 and at most 300000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptParams {
    pub target: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<AgentPromptWaitOptions>,
}

/// The additive request shape for `agent.focus`. Exactly one of `target` and
/// `agent_ref` must be present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(transform = require_one_focus_target)]
pub struct AgentFocusParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_ref: Option<super::AgentRef>,
}

fn require_one_focus_target(schema: &mut schemars::Schema) {
    let _ = schema.insert(
        "oneOf".into(),
        serde_json::json!([
            {
                "properties": {"target": {"type": "string"}},
                "required": ["target"],
                "not": {"required": ["agent_ref"]}
            },
            {
                "properties": {"agent_ref": {"not": {"type": "null"}}},
                "required": ["agent_ref"],
                "not": {"required": ["target"]}
            }
        ]),
    );
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentFocusStatusParams {
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFocusState {
    Connecting,
    Active,
    Failed,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoteControlContext {
    pub host: String,
    pub user: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub terminal_id: String,
    pub cwd: String,
    pub foreground_cwd: String,
    pub tty: String,
    pub foreground_process: RemoteForegroundProcess,
    pub detected_agent: String,
    pub interactive_ready: bool,
    pub human_draft: bool,
    pub state_change_seq: u64,
    pub revision: u64,
    pub context_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoteForegroundProcess {
    pub pid: u32,
    pub process_group_id: u32,
    pub name: String,
    pub argv: Vec<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentInfo {
    /// Cross-host identity. Older remote servers may omit this while fleet
    /// inventory remains version-skew tolerant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_ref: Option<super::AgentRef>,
    pub terminal_id: String,
    #[serde(default)]
    pub work_context: crate::work_context::PaneWorkContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Exact session title resolved by the host that owns this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title_stripped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub usage_limited: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub waiting_on_agents: bool,
    /// A blocked agent still has runtime evidence of active work or an external wait.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub working_while_blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_at: Option<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub screen_detection_skipped: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[schemars(schema_with = "super::common::metadata_token_values_schema")]
    pub tokens: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<ClosingBlockItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ClosingBlockItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<ClosingBlockDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionInfo>,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub launch_pending: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub interactive_ready: bool,
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentInfoProjection {
    pub(crate) state: crate::detect::AgentState,
    pub(crate) seen: bool,
    pub(crate) stale: bool,
    pub(crate) attention_tier: crate::terminal::state::AttentionTier,
    pub(crate) open_blockers: bool,
    pub(crate) usage_limited: bool,
    pub(crate) settled: bool,
    pub(crate) waiting_on_agents: bool,
    pub(crate) working_while_blocked: bool,
}

impl AgentInfoProjection {
    pub(crate) fn counts_as_blocked(self) -> bool {
        self.attention_tier == crate::terminal::state::AttentionTier::Blocked
    }
}

impl AgentInfo {
    /// Pane-aware meaning of lifecycle fields received over the stable wire.
    /// The raw status stays serialized unchanged; consumers use this projection.
    pub(crate) fn agent_projection(&self) -> AgentInfoProjection {
        if self.settled_at.is_some() {
            return AgentInfoProjection {
                state: crate::detect::AgentState::Unknown,
                seen: true,
                stale: false,
                attention_tier: crate::terminal::state::AttentionTier::None,
                open_blockers: false,
                usage_limited: false,
                settled: true,
                waiting_on_agents: false,
                working_while_blocked: false,
            };
        }
        let (state, seen, stale) = match self.agent_status {
            AgentStatus::Idle => (crate::detect::AgentState::Idle, true, false),
            AgentStatus::Working => (crate::detect::AgentState::Working, true, false),
            AgentStatus::Blocked => (crate::detect::AgentState::Blocked, true, false),
            AgentStatus::Done => (crate::detect::AgentState::Idle, false, false),
            AgentStatus::Stale => (crate::detect::AgentState::Unknown, true, true),
            AgentStatus::Unknown => (crate::detect::AgentState::Unknown, true, false),
        };
        let blocking_items = self
            .items
            .iter()
            .filter(|item| item.requires_human_input())
            .count();
        let open_blockers = !self.gates.is_empty() || blocking_items > 0;
        AgentInfoProjection {
            state,
            seen,
            stale,
            attention_tier: crate::terminal::state::attention_tier(
                state,
                !self.gates.is_empty(),
                blocking_items > 0,
                self.usage_limited,
            ),
            open_blockers,
            usage_limited: self.usage_limited,
            settled: false,
            waiting_on_agents: self.waiting_on_agents
                && !stale
                && state != crate::detect::AgentState::Blocked,
            working_while_blocked: self.working_while_blocked
                && !stale
                && state == crate::detect::AgentState::Blocked,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}
