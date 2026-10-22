use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::{AgentStatus, ReadFormat, ReadSource};
use crate::cli::agent::bindings;
use crate::cli::projection::{cli_params, Field};

/// Controls whether `agent.list` returns the coordinator's cached remote agents.
///
/// Ordinary callers get the aggregate view. Federation pollers request the
/// local-only view so peers never re-export an already aggregated directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentListParams {
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub local_only: bool,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentReadParams => AgentReadProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        pub source: ReadSource => Field::new(ReadSource::Recent).input(bindings::source()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub lines: Option<u32> => Field::new(None).input(bindings::lines()),
        #[serde(default)]
        pub format: ReadFormat => Field::new(ReadFormat::Text)
            .input(bindings::format()).input(bindings::ansi()),
        #[serde(default = "super::common::default_true")]
        pub strip_ansi: bool => Field::new(true),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentSendKeysParams => AgentSendKeysProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        pub keys: Vec<String> => Field::new(Vec::new()).input(bindings::keys()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentWaitParams => AgentWaitProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub until: Vec<AgentStatus> => Field::new(Vec::new()).input(bindings::until()
            .help("State to match; repeat for more than one state")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64> => Field::new(None).input(bindings::timeout()
            .help("Fail after this many milliseconds")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentPromptWaitOptions => AgentPromptWaitProjection {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub until: Vec<AgentStatus> => Field::new(Vec::new()).input(bindings::until()
            .help("State to match after --wait; repeat for more than one state")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64> => Field::new(None).input(bindings::timeout()
            .help("Set a deadline for observable submission and requested state")),
        #[serde(skip)]
        #[schemars(skip)]
        pub(crate) submission_deadline: Option<std::time::Instant> => Field::new(None::<std::time::Instant>),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentRenameParams => AgentRenameProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String> => Field::new(None)
            .input(bindings::rename_name()).input(bindings::clear_name()),
    }
}

/// `agent.archive` — take an agent out of active rotation (issue #173). The pane
/// is released but the session ref is preserved, so `agent.unarchive` can resume
/// (not recreate) it later. Rejected when the agent is mid-turn unless `force`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentArchiveParams {
    pub target: String,
    /// Optional free-text note recorded on the archived record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Who requested the archive; recorded verbatim. Defaults server-side to
    /// `"api"` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Opaque open-work list, stored and returned verbatim (gitmoot supplies and
    /// renders it; herdr does not interpret it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_work: Vec<serde_json::Value>,
    /// Archive even when the agent is currently working / mid-turn.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub force: bool,
}

/// `agent.unarchive` — resume a previously archived agent (issue #173). The
/// stored session ref is resumed into a fresh pane, preserving the agent's
/// terminal identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentUnarchiveParams {
    /// The archived agent's name or terminal id.
    pub target: String,
    /// Start a clean agent for the preserved terminal identity instead of
    /// resuming the archived session. The operator escape hatch when the
    /// session is gone or unwanted.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub fresh: bool,
}

/// `agent.forget` — permanently drop an archived agent's record (issue #291).
/// Only herdr's resume pointer is removed; the harness transcript and session
/// files on disk are left untouched. Live agents are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentForgetParams {
    /// The archived agent's name or terminal id.
    pub target: String,
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
    InputPending,
    InputPromptKind,
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

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentStartParams => AgentStartProjection {
        pub name: String => Field::new(String::new()).input(bindings::text_position("name", "NAME", 0)),
        pub kind: String => Field::new(String::new()).input(bindings::kind()),
        pub pane_id: String => Field::new(String::new()).input(bindings::pane()),
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub args: Vec<String> => Field::new(Vec::new()).input(bindings::agent_args()),
        /// Startup timeout in milliseconds. Values must be greater than 3000 and at most 300000.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64> => Field::new(None).input(bindings::timeout()
            .help("Wait for interactive readiness (default: 30000; max: 300000)")),
        /// Optional credential/config-home account id (from the `[[accounts]]` config
        /// registry) to launch this agent under. Points the harness at that account's
        /// config-home directory. Must match the agent's kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub account: Option<String> => Field::new(None).input(bindings::account()
            .help("Launch under this configured account")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentRestartParams {
    pub target: String,
    /// Optional credential/config-home account id to resume under. Absent keeps
    /// the agent's remembered account (a plain restart); present swaps to the
    /// named account for this and subsequent restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentTransferSessionParams {
    pub target: String,
    pub to: AgentSessionTransferHarness,
    /// Optional target credential/config-home account. Absent selects the
    /// target harness default; it never reuses an account from another kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The id returned by the prepare call. Required when `confirm` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_id: Option<String>,
    /// Replace the source runtime only after the staged transcript has been
    /// reviewed through `agent.session_transfer` on the returned agent.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub confirm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionTransferHarness {
    Claude,
    Codex,
    Omp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionTransferPhase {
    Preparing,
    Ready,
    VerifyingCutover,
    LaunchingTarget,
    AwaitingTarget,
    Completed,
    RollingBack,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionTransferOmissions {
    pub tool_records: u64,
    pub reasoning_records: u64,
    pub system_records: u64,
    pub attachment_records: u64,
    pub metadata_records: u64,
    pub unsupported_blocks: u64,
    pub sidechain_records: u64,
    /// Older records dropped because the transcript exceeded the transfer window.
    /// Unlike the other fields this is not a record CLASS — it is history left
    /// behind, and a non-zero value means the transfer is lossy and needs a
    /// person's approval before cutover.
    #[serde(default)]
    pub windowed_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionTransferInfo {
    pub id: String,
    pub source: AgentSessionTransferHarness,
    pub target: AgentSessionTransferHarness,
    /// The selected target account id, or `None` for the target harness default.
    /// Surfacing this lets a client safely reopen a prepared confirmation without
    /// guessing which account must be echoed back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_account: Option<String>,
    pub phase: AgentSessionTransferPhase,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub omissions: AgentSessionTransferOmissions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentPromptParams => AgentPromptProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        pub text: String => Field::new(String::new()).input(bindings::text_position("text", "TEXT", 1).verbatim()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub wait: Option<AgentPromptWaitOptions> => Field::new(None).input(bindings::wait_presence()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentPromptDelivery {
    WrittenToPty,
    Submitted,
}

// `parked_work` holds arbitrary JSON (`serde_json::Value`), which is `PartialEq`
// but not `Eq`, so `AgentInfo` can no longer derive `Eq`. Nothing keys a
// map/set on it, so only the derive is dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentInfo {
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title_stripped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub input_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_prompt_kind: Option<crate::detect::InputPromptKind>,
    #[serde(default)]
    pub composer: super::panes::ComposerInfo,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub screen_detection_skipped: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[schemars(schema_with = "super::common::metadata_token_values_schema")]
    pub tokens: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_turn: Option<super::panes::LastCompletedTurn>,
    /// Advisory hint only; use `pane.turns` replay as the completeness authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    /// Advisory hint only; use `pane.turns` replay as the completeness authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_epoch: Option<u64>,
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
    /// Wall-clock ms when the agent entered its CURRENT status (its last transition).
    /// `None` until the first detected transition, and after a restore/respawn (time in
    /// state restarts). The app derives a compact "5m/2h/3d" badge from `now - this` (#173).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_since_unix_ms: Option<u64>,
    /// The current idle transition completed work, independently of who has viewed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    pub revision: u64,
    /// Federation compatibility field: the home-chosen routing alias for the
    /// remote peer. This is not the peer's persisted machine identity. `None`
    /// for a local agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// Immutable saved-machine profile id used as the routing namespace once
    /// this agent comes from the upstream machine catalog. `None` for local
    /// agents and legacy explicit federation peers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_profile_id: Option<String>,
    /// Mutable display label for the remote machine. Never used for routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_label: Option<String>,
    /// Persisted install identity reported by the daemon that owns this agent.
    /// Kept separate from the home-chosen routing namespace and mutable display
    /// label. It is an identity pin, not an authentication secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_machine_id: Option<String>,
    /// Federation: reachability of the remote peer as of the last poll. `None`
    /// for a local agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachability: Option<crate::api::federation_store::Reachability>,
    /// Federation: the peer's last-known agent status, preserved when the peer is
    /// unreachable and `agent_status` is surfaced as `unknown`. `None` for a
    /// local agent or a reachable peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known_status: Option<AgentStatus>,
    /// Present only for archived agents (issue #173). Its presence is the
    /// load-bearing signal that this agent is archived; absent means active, so
    /// older clients that ignore the field see a normal (active) agent list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<AgentArchivedInfo>,
    /// Opaque open-work list carried on an archived agent, returned verbatim.
    /// Absent (empty) for active agents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_work: Vec<serde_json::Value>,
    /// The credential/config-home account id this agent runs under, or `None` when it
    /// runs on the harness default. Reported because a pane silently coming back on the
    /// WRONG account is what this fleet experienced as hours of lost history, and the
    /// only evidence was in each child's `/proc/<pid>/environ`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The config-home directory `account` resolves to in the registry (a PATH, never a
    /// credential). `None` when no account is recorded, or when the recorded one no
    /// longer resolves — `account_unresolved` distinguishes those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_config_dir: Option<String>,
    /// The recorded account is NOT in the registry, so this agent will refuse to resume
    /// rather than come back on the default account and append to the wrong transcript.
    /// An error state to surface, not a transient: it persists until the account is
    /// re-registered.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub account_unresolved: bool,
    /// Present while an agent session is being moved between Claude Code and
    /// Codex, and retained with the final completed/rolled-back outcome until a
    /// later transfer replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_transfer: Option<AgentSessionTransferInfo>,
}

/// The `archived { at, by, reason }` provenance surfaced on an archived
/// [`AgentInfo`] in `agent.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentArchivedInfo {
    /// RFC3339 timestamp of when the agent was archived.
    pub at: String,
    /// Who archived it.
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}
