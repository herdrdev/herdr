use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// Effective state arbitration is intentionally centralized here. Full lifecycle
// Herdr hook integrations are authoritative while live, except for a newer
// structurally visible blocker. Weaker reports can yield to newer screen state.
// Process-exit updates clear matching hook authority before recomputing state.

use crate::detect::{Agent, AgentState};
use crate::terminal::TerminalId;

pub(crate) const AGENT_BUSY_STALE_SILENCE: Duration = Duration::from_secs(20 * 60);
pub(crate) const DECLARED_WAIT_GRACE: Duration = Duration::from_secs(30);
#[cfg(test)]
pub(crate) const DEFAULT_SUBAGENT_STALE_SILENCE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SubagentTranscriptActivity {
    #[default]
    Unknown,
    Fresh,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionTier {
    ContractSatisfied,
    DeclaredDone,
    StoppedUndeclared,
}

/// A session with nothing left running and nothing waiting on a human.
///
/// The single source of truth for "this session has stopped". Both the sidebar
/// tier colours and the Done pane lifecycle key off it, so a pane can never be
/// blocking in one place and reapable in the other.
pub(crate) fn session_is_quiet(
    state: AgentState,
    open_blockers: bool,
    active_subagents: Option<u32>,
    holds_shell: bool,
) -> bool {
    state == AgentState::Idle
        && !open_blockers
        && active_subagents.unwrap_or_default() == 0
        && !holds_shell
}

pub(crate) fn derive_completion_tier(
    state: AgentState,
    contract: Option<&str>,
    contract_met: Option<bool>,
    closing_idle: Option<bool>,
    open_blockers: bool,
    active_subagents: Option<u32>,
    holds_shell: bool,
    has_closing_block_tokens: bool,
) -> Option<CompletionTier> {
    if !session_is_quiet(state, open_blockers, active_subagents, holds_shell) {
        return None;
    }
    if contract.is_some_and(|contract| !contract.is_empty()) && contract_met == Some(true) {
        return Some(CompletionTier::ContractSatisfied);
    }
    if closing_idle == Some(true) {
        return Some(CompletionTier::DeclaredDone);
    }
    if !has_closing_block_tokens {
        return Some(CompletionTier::StoppedUndeclared);
    }
    None
}

fn parse_binary_token(value: &str) -> Option<bool> {
    match value {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Severity of a pane's outstanding demand on the human.
///
/// This is shared runtime meaning. The TUI decides how each tier looks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AttentionTier {
    #[default]
    None,
    Attention,
    Blocked,
}

/// Derive human-attention severity from the lifecycle and closing-block facts.
///
/// Every retained Gate, Answer, or Verify item is a human obligation. Active
/// independent work may still keep the lifecycle working. A plain blocked
/// lifecycle remains blocked without structured closing-block detail.
pub(crate) fn attention_tier(
    state: AgentState,
    has_closing_gates: bool,
    has_closing_items: bool,
    usage_limited: bool,
) -> AttentionTier {
    if usage_limited || has_closing_gates || has_closing_items || state == AgentState::Blocked {
        AttentionTier::Blocked
    } else {
        AttentionTier::None
    }
}

#[cfg(test)]
use super::metadata::AgentMetadataReport;
use super::metadata::{AgentMetadata, EffectivePresentation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: AgentState,
    pub message: Option<String>,
    pub reported_at: Instant,
    pub wait: Option<String>,
    pub eta_s: Option<u64>,
    pub reported_at_wire: Option<String>,
    pub session_ref: Option<crate::agent_resume::AgentSessionRef>,
    pub retired_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuppressedFullLifecycleHookReport {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
    observed_at: Instant,
    reason: FullLifecycleHookSuppressionReason,
    replacement_session_ref: Option<crate::agent_resume::AgentSessionRef>,
    pending_replacement_report: Option<PendingFullLifecycleHookReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingFullLifecycleHookReport {
    authority: HookAuthority,
    seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullLifecycleHookSuppressionReason {
    HookClear,
    ProcessExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullLifecycleHookReportRoute {
    Accept { reanchor_sequence: bool },
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaleFullLifecycleHookSession {
    agent_label: String,
    session_ref: crate::agent_resume::AgentSessionRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedAgentPhase {
    Pending {
        ready_after: Option<Instant>,
        deadline: Instant,
        observed_expected: bool,
    },
    Blocked,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManagedAgent {
    kind: Agent,
    phase: ManagedAgentPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveStateChange {
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub previous_presentation: EffectivePresentation,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub presentation: EffectivePresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TerminalTitleChange {
    pub(crate) raw_changed: bool,
    pub(crate) stripped_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalStateMutation {
    pub effective_state_change: Option<EffectiveStateChange>,
    pub session_ref_changed: bool,
    pub session_replaced: bool,
    pub hook_work_context_changed: bool,
    pub agent_released: bool,
    /// A cached sidebar fact changed without changing the four-state lifecycle.
    pub sidebar_projection_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentNameOwner {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AgentActivityOwner {
    Session {
        source: String,
        agent_label: String,
        kind: crate::agent_resume::AgentSessionRefKind,
        value: String,
    },
    Agent {
        agent_label: String,
        previous_session: Option<AgentActivitySession>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockedStateHold {
    owner: AgentActivityOwner,
    since: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentActivitySession {
    source: String,
    kind: crate::agent_resume::AgentSessionRefKind,
    value: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentActivityHandoffState {
    state: AgentActivityHandoffStatus,
    active_elapsed: Option<Duration>,
    last_active_elapsed: Option<Duration>,
    owner: Option<AgentActivityOwner>,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalAgentHandoffState {
    state: AgentActivityHandoffStatus,
    #[serde(default)]
    detected_agent: Option<String>,
    #[serde(default)]
    recent_agent_process_exit: Option<RecentAgentProcessExitHandoffState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_state_hold: Option<BlockedStateHoldHandoffState>,
    hook_authority: Option<HookAuthorityHandoffState>,
    supervisor_stale: bool,
    metadata: Vec<super::metadata::AgentMetadataHandoffState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_report: Option<ClosingReportHandoffState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    closing_gates: Vec<crate::api::schema::ClosingBlockItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    closing_items: Vec<crate::api::schema::ClosingBlockItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    closing_decisions: Vec<crate::api::schema::ClosingBlockDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_report_subagents: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_idle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_contract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_contract_met: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_contract_met_elapsed: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_completion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_wait: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_parse: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closing_workers_unknown: Option<String>,
    #[serde(default)]
    unreported_turn_started_elapsed: Option<Duration>,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BlockedStateHoldHandoffState {
    owner: AgentActivityOwner,
    since_elapsed: Duration,
}

const CLOSING_REPORT_MODEL_VERSION: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ClosingReportScope {
    source: Option<String>,
    session_id: Option<String>,
    turn_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyClosingReportGuard {
    source: String,
    agent_label: String,
    session_id: String,
    report_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct UnansweredClosingBlockers {
    agent_label: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_seq: Option<u64>,
    gates: Vec<crate::api::schema::ClosingBlockItem>,
    items: Vec<crate::api::schema::ClosingBlockItem>,
}

fn same_closing_blocker(
    left: &crate::api::schema::ClosingBlockItem,
    right: &crate::api::schema::ClosingBlockItem,
) -> bool {
    left.label.trim().eq_ignore_ascii_case(right.label.trim())
        && left.text.trim() == right.text.trim()
        && left.pr == right.pr
        && left.ticket == right.ticket
        && left.url == right.url
}

fn merge_unanswered_closing_blockers(
    unanswered: &mut UnansweredClosingBlockers,
    gates: &[crate::api::schema::ClosingBlockItem],
    items: &[crate::api::schema::ClosingBlockItem],
) -> bool {
    let mut changed = false;
    for gate in gates.iter().filter(|item| item.requires_human_input()) {
        if !unanswered
            .gates
            .iter()
            .any(|existing| same_closing_blocker(existing, gate))
        {
            unanswered.gates.push(gate.clone());
            changed = true;
        }
    }
    for item in items.iter().filter(|item| item.requires_human_input()) {
        if !unanswered
            .items
            .iter()
            .any(|existing| same_closing_blocker(existing, item))
        {
            unanswered.items.push(item.clone());
            changed = true;
        }
    }
    changed
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosingReport {
    version: u8,
    scope: ClosingReportScope,
    retired_pending_completion: bool,
    requires_legacy_session_guard: bool,
    legacy_session_guard: Option<LegacyClosingReportGuard>,
    pub(crate) closing_gates: Vec<crate::api::schema::ClosingBlockItem>,
    pub(crate) closing_items: Vec<crate::api::schema::ClosingBlockItem>,
    pub(crate) closing_decisions: Vec<crate::api::schema::ClosingBlockDecision>,
    unanswered: Option<UnansweredClosingBlockers>,
    closing_report_subagents: Option<u32>,
    pub(crate) closing_idle: Option<bool>,
    pub(crate) closing_contract: Option<String>,
    pub(crate) closing_contract_met: Option<bool>,
    pub(crate) closing_contract_met_at: Option<Instant>,
    pub(crate) last_turn_at: Option<String>,
    last_turn_at_instant: Option<Instant>,
    last_turn_at_unix_s: Option<u64>,
    completion: Option<crate::api::schema::ClosingCompletion>,
    external_wait: Option<String>,
    parse_status: Option<crate::api::schema::ClosingParseStatus>,
    workers_unknown: Option<bool>,
    /// A successful human input reached this agent session at least once.
    user_replied: bool,
    user_replied_session_id: Option<String>,
    auto_settle_consumed_turn: Option<(String, u64)>,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClosingLogTokens {
    pub completion: Option<&'static str>,
    pub parse: Option<&'static str>,
    pub workers_unknown: Option<bool>,
    pub idle: Option<bool>,
}

impl Default for ClosingReport {
    fn default() -> Self {
        Self {
            version: CLOSING_REPORT_MODEL_VERSION,
            scope: ClosingReportScope::default(),
            retired_pending_completion: false,
            requires_legacy_session_guard: false,
            legacy_session_guard: None,
            closing_gates: Vec::new(),
            closing_items: Vec::new(),
            closing_decisions: Vec::new(),
            unanswered: None,
            closing_report_subagents: None,
            closing_idle: None,
            closing_contract: None,
            closing_contract_met: None,
            closing_contract_met_at: None,
            last_turn_at: None,
            last_turn_at_instant: None,
            last_turn_at_unix_s: None,
            completion: None,
            external_wait: None,
            parse_status: None,
            workers_unknown: None,
            user_replied: false,
            user_replied_session_id: None,
            auto_settle_consumed_turn: None,
        }
    }
}

#[cfg(test)]
impl std::ops::Deref for TerminalState {
    type Target = ClosingReport;

    fn deref(&self) -> &Self::Target {
        self.closing_report
            .as_ref()
            .expect("test terminal always owns a closing report")
    }
}

#[cfg(test)]
impl std::ops::DerefMut for TerminalState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.closing_report
            .as_mut()
            .expect("test terminal always owns a closing report")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClosingReportHandoffState {
    version: u8,
    scope_source: Option<String>,
    scope_session_id: Option<String>,
    scope_turn_seq: Option<u64>,
    #[serde(default)]
    retired_pending_completion: bool,
    #[serde(default)]
    requires_legacy_session_guard: bool,
    #[serde(default)]
    legacy_session_guard: Option<LegacyClosingReportGuard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unanswered: Option<UnansweredClosingBlockers>,
    gates: Vec<crate::api::schema::ClosingBlockItem>,
    items: Vec<crate::api::schema::ClosingBlockItem>,
    decisions: Vec<crate::api::schema::ClosingBlockDecision>,
    closing_report_subagents: Option<u32>,
    idle: Option<bool>,
    contract: Option<String>,
    contract_met: Option<bool>,
    contract_met_elapsed: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_turn_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_turn_at_elapsed: Option<Duration>,
    completion: Option<crate::api::schema::ClosingCompletion>,
    external_wait: Option<String>,
    parse_status: Option<crate::api::schema::ClosingParseStatus>,
    workers_unknown: Option<bool>,
    #[serde(default)]
    user_replied: bool,
    #[serde(default)]
    user_replied_session_id: Option<String>,
    #[serde(default)]
    auto_settle_consumed_turn: Option<(String, u64)>,
}

impl ClosingReportHandoffState {
    pub(crate) fn capture(report: &ClosingReport, now: Instant) -> Self {
        Self {
            version: report.version,
            scope_source: report.scope.source.clone(),
            scope_session_id: report.scope.session_id.clone(),
            scope_turn_seq: report.scope.turn_seq,
            retired_pending_completion: report.retired_pending_completion,
            requires_legacy_session_guard: report.requires_legacy_session_guard,
            legacy_session_guard: report.legacy_session_guard.clone(),
            unanswered: report.unanswered.clone(),
            gates: report.closing_gates.clone(),
            items: report.closing_items.clone(),
            decisions: report.closing_decisions.clone(),
            closing_report_subagents: report.closing_report_subagents,
            idle: report.closing_idle,
            contract: report.closing_contract.clone(),
            contract_met: report.closing_contract_met,
            contract_met_elapsed: report
                .closing_contract_met_at
                .map(|at| now.saturating_duration_since(at)),
            last_turn_at: report.last_turn_at.clone(),
            last_turn_at_elapsed: report
                .last_turn_at_instant
                .map(|at| now.saturating_duration_since(at)),
            completion: report.completion,
            external_wait: report.external_wait.clone(),
            parse_status: report.parse_status,
            workers_unknown: report.workers_unknown,
            user_replied: report.user_replied,
            user_replied_session_id: report.user_replied_session_id.clone(),
            auto_settle_consumed_turn: report.auto_settle_consumed_turn.clone(),
        }
    }

    pub(crate) fn restore(self, now: Instant) -> ClosingReport {
        ClosingReport {
            version: self.version,
            scope: ClosingReportScope {
                source: self.scope_source,
                session_id: self.scope_session_id,
                turn_seq: self.scope_turn_seq,
            },
            retired_pending_completion: self.retired_pending_completion,
            requires_legacy_session_guard: self.requires_legacy_session_guard,
            legacy_session_guard: self.legacy_session_guard,
            unanswered: self.unanswered,
            closing_gates: self.gates,
            closing_items: self.items,
            closing_decisions: self.decisions,
            closing_report_subagents: self.closing_report_subagents,
            closing_idle: self.idle,
            closing_contract: self.contract,
            closing_contract_met: self.contract_met,
            closing_contract_met_at: self
                .contract_met_elapsed
                .and_then(|elapsed| now.checked_sub(elapsed)),
            last_turn_at: self.last_turn_at.clone(),
            last_turn_at_instant: self
                .last_turn_at_elapsed
                .and_then(|elapsed| now.checked_sub(elapsed)),
            last_turn_at_unix_s: self
                .last_turn_at
                .as_deref()
                .and_then(crate::fleet::parse_utc_timestamp),
            completion: self.completion,
            external_wait: self.external_wait,
            parse_status: self.parse_status,
            workers_unknown: self.workers_unknown,
            user_replied: self.user_replied,
            user_replied_session_id: self.user_replied_session_id,
            auto_settle_consumed_turn: self.auto_settle_consumed_turn,
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecentAgentProcessExitHandoffState {
    agent_label: String,
    observed_elapsed: Duration,
}

#[cfg(unix)]
impl TerminalAgentHandoffState {
    pub(crate) fn handoff_detection_seed(&self) -> (Option<Agent>, AgentState) {
        (
            self.detected_agent
                .as_deref()
                .and_then(crate::detect::parse_canonical_agent_label)
                .or_else(|| {
                    self.hook_authority.as_ref().and_then(|authority| {
                        crate::detect::parse_canonical_agent_label(&authority.agent_label)
                    })
                }),
            self.state.into(),
        )
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HookAuthorityHandoffState {
    source: String,
    agent_label: String,
    state: AgentActivityHandoffStatus,
    message: Option<String>,
    reported_elapsed: Duration,
    wait: Option<String>,
    eta_s: Option<u64>,
    reported_at_wire: Option<String>,
    session_ref: Option<AgentSessionRefHandoffState>,
    retired_elapsed: Option<Duration>,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AgentSessionRefHandoffState {
    kind: crate::agent_resume::AgentSessionRefKind,
    value: String,
}

#[cfg(unix)]
impl HookAuthorityHandoffState {
    fn capture(authority: &HookAuthority, now: Instant) -> Self {
        Self {
            source: authority.source.clone(),
            agent_label: authority.agent_label.clone(),
            state: authority.state.into(),
            message: authority.message.clone(),
            reported_elapsed: now.saturating_duration_since(authority.reported_at),
            wait: authority.wait.clone(),
            eta_s: authority.eta_s,
            reported_at_wire: authority.reported_at_wire.clone(),
            session_ref: authority.session_ref.as_ref().map(|session_ref| {
                AgentSessionRefHandoffState {
                    kind: session_ref.kind,
                    value: session_ref.value.clone(),
                }
            }),
            retired_elapsed: authority
                .retired_at
                .map(|retired_at| now.saturating_duration_since(retired_at)),
        }
    }

    fn restore(self, now: Instant) -> HookAuthority {
        HookAuthority {
            source: self.source,
            agent_label: self.agent_label,
            state: self.state.into(),
            message: self.message,
            reported_at: now.checked_sub(self.reported_elapsed).unwrap_or(now),
            wait: self.wait,
            eta_s: self.eta_s,
            reported_at_wire: self.reported_at_wire,
            session_ref: self
                .session_ref
                .map(|session_ref| crate::agent_resume::AgentSessionRef {
                    kind: session_ref.kind,
                    value: session_ref.value,
                }),
            retired_at: self
                .retired_elapsed
                .and_then(|elapsed| now.checked_sub(elapsed)),
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum AgentActivityHandoffStatus {
    Idle,
    Working,
    Blocked,
    Unknown,
}

#[cfg(unix)]
impl From<AgentState> for AgentActivityHandoffStatus {
    fn from(state: AgentState) -> Self {
        match state {
            AgentState::Idle => Self::Idle,
            AgentState::Working => Self::Working,
            AgentState::Blocked => Self::Blocked,
            AgentState::Unknown => Self::Unknown,
        }
    }
}

#[cfg(unix)]
impl From<AgentActivityHandoffStatus> for AgentState {
    fn from(state: AgentActivityHandoffStatus) -> Self {
        match state {
            AgentActivityHandoffStatus::Idle => Self::Idle,
            AgentActivityHandoffStatus::Working => Self::Working,
            AgentActivityHandoffStatus::Blocked => Self::Blocked,
            AgentActivityHandoffStatus::Unknown => Self::Unknown,
        }
    }
}

impl AgentActivityOwner {
    fn agent_label(&self) -> &str {
        match self {
            Self::Session { agent_label, .. } | Self::Agent { agent_label, .. } => agent_label,
        }
    }

    fn session(&self) -> Option<AgentActivitySession> {
        match self {
            Self::Session {
                source,
                kind,
                value,
                ..
            } => Some(AgentActivitySession {
                source: source.clone(),
                kind: *kind,
                value: value.clone(),
            }),
            Self::Agent {
                previous_session, ..
            } => previous_session.clone(),
        }
    }

    fn inherit_detection_lineage(&mut self, previous: Option<&Self>) {
        let Self::Agent {
            agent_label,
            previous_session,
        } = self
        else {
            return;
        };
        let Some(previous) = previous.filter(|owner| owner.agent_label() == agent_label) else {
            return;
        };
        *previous_session = previous.session();
    }

    /// Detection can identify an agent before a session hook supplies its
    /// stronger identity, and can remain after that hook clears. Those
    /// refinement transitions belong to one live activity interval. A
    /// detection fallback remembers its last authoritative session so a later
    /// different session cannot inherit that interval.
    fn continues_activity_from(&self, previous: &Self) -> bool {
        match (previous, self) {
            (Self::Session { .. }, Self::Session { .. }) => previous == self,
            (
                Self::Agent {
                    previous_session, ..
                },
                Self::Session { .. },
            ) => {
                previous.agent_label() == self.agent_label()
                    && previous_session
                        .as_ref()
                        .is_none_or(|session| Some(session.clone()) == self.session())
            }
            _ => previous.agent_label() == self.agent_label(),
        }
    }

    fn matches_blocked_hold(&self, hold_owner: &Self) -> bool {
        if self.agent_label() != hold_owner.agent_label() {
            return false;
        }
        match (self.session(), hold_owner.session()) {
            (Some(current), Some(held)) => current == held,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecentAgentProcessExit {
    pub(super) agent: Agent,
    observed_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkTitleInitialSubject {
    agent_label: String,
    lifecycle_source: String,
    session_id: String,
    title: String,
}

/// Pure state for a server-owned terminal.
///
/// During the migration this is still one-to-one with a pane-backed PTY, but
/// pane/view state no longer owns terminal identity, cwd, labels, or agent
/// metadata.
pub struct TerminalState {
    pub id: TerminalId,
    pub cwd: PathBuf,
    pub detected_agent: Option<Agent>,
    pub fallback_state: AgentState,
    fallback_visible_blocker: bool,
    blocked_state_hold: Option<BlockedStateHold>,
    /// The last screen detection saw an exhausted plan usage/rate limit. It is
    /// live screen state, never latched: the next detection that is not a
    /// usage-limit screen clears it.
    pub usage_limited: bool,
    fallback_visible_working: bool,
    fallback_visible_working_observed_at: Option<Instant>,
    fallback_working_observed_at: Option<Instant>,
    fallback_observed_at: Option<Instant>,
    pub hook_authority: Option<HookAuthority>,
    pub supervisor_stale: bool,
    pub agent_metadata: HashMap<String, AgentMetadata>,
    pub work_context: crate::work_context::PaneWorkContextState,
    work_title_initial_subject: Option<WorkTitleInitialSubject>,
    pub metadata_tokens: crate::metadata_tokens::MetadataTokens,
    closing_report: Option<ClosingReport>,
    pub persisted_agent_session: Option<crate::agent_resume::PersistedAgentSession>,
    /// Runtime-only Claude JSONL source. Never persisted or exposed through the
    /// API; the accepted session id remains the resume identity.
    pub(crate) claude_transcript_session_id: Option<String>,
    pub(crate) claude_transcript_path: Option<PathBuf>,
    /// Runtime-only file target reported by the current Claude or Codex hook.
    /// It is rebound by hooks after restore rather than persisted as session identity.
    session_name_write_target: Option<crate::work_title::SessionNameWriteTarget>,
    pub terminal_title: Option<String>,
    pub manual_label: Option<String>,
    /// Configured host for a remote-focus proxy. Proxy terminals are excluded
    /// from persistence, so this is runtime metadata only.
    pub(crate) remote_proxy_host: Option<String>,
    /// Label Herdr applied before suspending this terminal, if still owned by settlement.
    pub(crate) settled_auto_label: Option<String>,
    pub agent_name: Option<String>,
    agent_name_owner: Option<AgentNameOwner>,
    managed_agent: Option<ManagedAgent>,
    pub(super) hook_report_sequences: HashMap<String, u64>,
    suppressed_full_lifecycle_hook_reports: HashMap<String, SuppressedFullLifecycleHookReport>,
    stale_full_lifecycle_hook_sessions: HashMap<String, Vec<StaleFullLifecycleHookSession>>,
    pub(super) metadata_report_sequences: HashMap<String, u64>,
    pub(super) metadata_report_agents: HashMap<String, Agent>,
    pub(super) metadata_token_sequence_sources: std::collections::HashSet<String>,
    state: AgentState,
    /// A live child process remains below the pane shell or agent process.
    pub holds_shell: bool,
    /// Process evidence used to project stale hook state in the sidebar.
    pub(crate) stale_resolution: Option<(AgentState, bool)>,
    /// Live direct sub-agents reported by a provider-owned runtime source.
    /// `None` means unavailable or not yet reconstructed.
    pub active_subagents: Option<u32>,
    /// Runtime-only detail from the same Claude transcript scan as
    /// `active_subagents`. It is not persisted or exposed through the API.
    pub(crate) claude_subagent_observations:
        Option<Vec<crate::app::claude_subagents::ClaudeSubagentObservation>>,
    /// Runtime-only liveness evidence collected by the background Claude
    /// subagent transcript refresh. Unknown preserves the declared wait budget.
    claude_subagent_transcript_activity: SubagentTranscriptActivity,
    claude_subagent_activity_observed_at: Option<Instant>,
    /// Last background observation of the pane's distinct foreground process.
    pub(crate) foreground_process_name: Option<String>,
    /// Runtime-only model the pane's agent process was launched with
    /// (`--model`/`-m` argv). `None` when unknown.
    pub(crate) agent_model: Option<String>,
    foreground_process_active: bool,
    pub last_agent_state_change_seq: Option<u64>,
    /// Runtime-only start of the current Working lifecycle, independent of
    /// later status report refreshes.
    working_since: Option<Instant>,
    /// When this pane most recently entered `Blocked`. Cleared on any transition
    /// out of it, so it always measures the current wait rather than a past one.
    pub blocked_since: Option<Instant>,
    /// Visible work can start a turn before the next lifecycle hook fires.
    /// This timestamp owns that gap and its watchdog budget.
    unreported_turn_started_at: Option<Instant>,
    /// Runtime identity for background observations sampled during one agent turn.
    agent_turn_generation: u64,
    agent_active_since: Option<Instant>,
    agent_last_active_at: Option<Instant>,
    agent_activity_owner: Option<AgentActivityOwner>,
    pub revision: u64,
    pub launch_argv: Option<Vec<String>>,
    pub respawn_shell_on_exit: bool,
    pub(super) recent_agent_process_exit: Option<RecentAgentProcessExit>,
    agent_process_acquisition_pending: bool,
    pub pending_agent_resume_plan: Option<crate::agent_resume::AgentResumePlan>,
    pub restore_error: Option<String>,
}

fn normalize_declared_wait(
    state: AgentState,
    wait: Option<String>,
    eta_s: Option<u64>,
) -> (Option<String>, Option<u64>) {
    const MAX_DECLARED_WAIT_S: u64 = 7 * 24 * 60 * 60;
    let wait = wait
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    if state != AgentState::Working || wait.is_none() {
        return (None, None);
    }
    (wait, eta_s.filter(|eta_s| *eta_s <= MAX_DECLARED_WAIT_S))
}

impl TerminalState {
    pub(crate) fn closing_report_persistence_state(
        &self,
        now: Instant,
    ) -> Option<ClosingReportHandoffState> {
        self.closing_report
            .as_ref()
            .filter(|report| *report != &ClosingReport::default())
            .map(|report| ClosingReportHandoffState::capture(report, now))
    }

    pub(crate) fn restore_closing_report_persistence_state(
        &mut self,
        state: ClosingReportHandoffState,
        now: Instant,
    ) {
        self.closing_report = Some(state.restore(now));
    }

    pub fn new(id: TerminalId, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            detected_agent: None,
            fallback_state: AgentState::Unknown,
            fallback_visible_blocker: false,
            blocked_state_hold: None,
            usage_limited: false,
            fallback_visible_working: false,
            fallback_visible_working_observed_at: None,
            fallback_working_observed_at: None,
            fallback_observed_at: None,
            hook_authority: None,
            supervisor_stale: false,
            agent_metadata: HashMap::new(),
            work_context: crate::work_context::PaneWorkContextState::default(),
            work_title_initial_subject: None,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            closing_report: Some(ClosingReport::default()),
            persisted_agent_session: None,
            claude_transcript_session_id: None,
            claude_transcript_path: None,
            session_name_write_target: None,
            terminal_title: None,
            manual_label: None,
            remote_proxy_host: None,
            settled_auto_label: None,
            agent_name: None,
            agent_name_owner: None,
            managed_agent: None,
            hook_report_sequences: HashMap::new(),
            suppressed_full_lifecycle_hook_reports: HashMap::new(),
            stale_full_lifecycle_hook_sessions: HashMap::new(),
            metadata_report_sequences: HashMap::new(),
            metadata_report_agents: HashMap::new(),
            metadata_token_sequence_sources: std::collections::HashSet::new(),
            state: AgentState::Unknown,
            holds_shell: false,
            stale_resolution: None,
            active_subagents: None,
            claude_subagent_observations: None,
            claude_subagent_transcript_activity: SubagentTranscriptActivity::Unknown,
            claude_subagent_activity_observed_at: None,
            foreground_process_name: None,
            agent_model: None,
            foreground_process_active: false,
            last_agent_state_change_seq: None,
            working_since: None,
            blocked_since: None,
            unreported_turn_started_at: None,
            agent_turn_generation: 0,
            agent_active_since: None,
            agent_last_active_at: None,
            agent_activity_owner: None,
            revision: 0,
            launch_argv: None,
            respawn_shell_on_exit: false,
            recent_agent_process_exit: None,
            agent_process_acquisition_pending: false,
            pending_agent_resume_plan: None,
            restore_error: None,
        }
    }

    /// Effective lifecycle before pane settlement and attention policy.
    /// Most callers need `PaneState::agent_projection` instead.
    pub(crate) fn raw_agent_state(&self) -> AgentState {
        self.state
    }

    #[cfg(test)]
    pub(crate) fn working_since(&self) -> Option<Instant> {
        self.working_since
    }

    #[cfg(test)]
    pub(crate) fn set_raw_agent_state_for_test(&mut self, state: AgentState) {
        self.state = state;
    }

    pub fn set_detected_agent_process_at(
        &mut self,
        agent: Agent,
        now: Instant,
    ) -> TerminalStateMutation {
        let finished_report_matches_agent = self.finished_closing_report()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                crate::detect::parse_agent_label(&authority.agent_label) == Some(agent)
            });
        let starts_acquisition = !finished_report_matches_agent
            && !self.should_ignore_detected_state_under_full_lifecycle_hook(Some(agent), false)
            && !self.detected_state_observed_before_release_suppression(Some(agent), now);
        let mutation = self.set_detected_state_with_screen_signals_at(
            Some(agent),
            if finished_report_matches_agent {
                AgentState::Idle
            } else {
                AgentState::Unknown
            },
            false,
            false,
            false,
            false,
            false,
            now,
        );
        if starts_acquisition {
            self.agent_process_acquisition_pending = true;
        }
        mutation
    }

    pub(crate) fn finish_agent_process_acquisition(&mut self) -> bool {
        let held_idle = self.state == AgentState::Blocked
            && self.fallback_state == AgentState::Idle
            && self
                .blocked_state_hold
                .as_ref()
                .is_some_and(|hold| self.blocked_hold_matches_current_activity(hold));
        let reached_idle =
            self.agent_process_acquisition_pending && (self.state == AgentState::Idle || held_idle);
        let suppress_completion = reached_idle
            && self.recent_agent_process_exit.is_none()
            && !self.closing_task_complete();
        if reached_idle {
            self.agent_process_acquisition_pending = false;
        }
        suppress_completion
    }

    pub(crate) fn apply_closing_block_payload(
        &mut self,
        gates: Vec<crate::api::schema::ClosingBlockItem>,
        items: Vec<crate::api::schema::ClosingBlockItem>,
        decisions: Vec<crate::api::schema::ClosingBlockDecision>,
    ) -> bool {
        let has_blockers = gates
            .iter()
            .any(crate::api::schema::ClosingBlockItem::requires_human_input)
            || items
                .iter()
                .any(crate::api::schema::ClosingBlockItem::requires_human_input);
        let (report_source, report_session_id, report_turn_seq) = self
            .closing_report
            .as_ref()
            .map(|report| {
                (
                    report.scope.source.clone(),
                    report.scope.session_id.clone(),
                    report.scope.turn_seq,
                )
            })
            .unwrap_or_default();
        let current_authority = self.hook_authority.as_ref();
        let report_agent_label = current_authority
            .filter(|authority| {
                crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
                    && report_source.as_deref() == Some(authority.source.as_str())
            })
            .map(|authority| authority.agent_label.clone());
        let answered_latch = !has_blockers
            && report_agent_label.is_some()
            && self
                .closing_report
                .as_ref()
                .and_then(|report| report.unanswered.as_ref())
                .is_some_and(|unanswered| {
                    unanswered.source.as_deref() == report_source.as_deref()
                        && unanswered.session_id.is_some()
                        && unanswered.session_id == report_session_id
                        && unanswered.agent_label == report_agent_label
                        && report_turn_seq
                            .zip(unanswered.turn_seq)
                            .is_some_and(|(newer, latched)| newer > latched)
                });
        if answered_latch {
            if let Some(report) = self.closing_report.as_mut() {
                report.unanswered = None;
            }
            self.revision = self.revision.saturating_add(1);
            let reported_at = current_authority.map(|authority| authority.reported_at);
            if let (Some(owner), Some(reported_at)) =
                (self.current_agent_activity_owner(), reported_at)
            {
                self.release_blocked_state_hold_for(&owner, reported_at);
            }
        }
        let has_unanswered_latch = self
            .closing_report
            .as_ref()
            .is_some_and(|report| report.unanswered.is_some());
        if !has_blockers && !has_unanswered_latch {
            let reported_at = self
                .hook_authority
                .as_ref()
                .map(|authority| authority.reported_at)
                .unwrap_or_else(Instant::now);
            if let Some(owner) = self.current_agent_activity_owner() {
                self.release_blocked_state_hold_for(&owner, reported_at);
            }
        }

        let live_closing_block_report = self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
        });
        let report = self.closing_report.get_or_insert_default();
        if report.unanswered.is_none() && has_blockers && live_closing_block_report {
            let agent_label = self
                .hook_authority
                .as_ref()
                .map(|authority| authority.agent_label.clone());
            report.unanswered = Some(UnansweredClosingBlockers {
                agent_label,
                source: report_source.clone(),
                session_id: report_session_id.clone(),
                turn_seq: report_turn_seq,
                ..UnansweredClosingBlockers::default()
            });
        }
        if has_blockers && live_closing_block_report {
            if let Some(unanswered) = report.unanswered.as_mut() {
                if unanswered.source.is_none() && unanswered.turn_seq.is_none() {
                    unanswered.agent_label = report_agent_label.clone();
                    unanswered.source = report_source.clone();
                    unanswered.session_id = report_session_id.clone();
                    unanswered.turn_seq = report_turn_seq;
                } else if unanswered.source.as_deref() == report_source.as_deref()
                    && unanswered.session_id == report_session_id
                    && unanswered.agent_label == report_agent_label
                {
                    unanswered.turn_seq = report_turn_seq.or(unanswered.turn_seq);
                }
            }
        }
        let (display_gates, display_items, latch_changed) =
            if let Some(unanswered) = report.unanswered.as_mut() {
                let latch_changed = merge_unanswered_closing_blockers(unanswered, &gates, &items);
                let display_gates = unanswered.gates.clone();
                let mut display_items = unanswered.items.clone();
                display_items.extend(
                    items
                        .into_iter()
                        .filter(|item| !item.requires_human_input()),
                );
                (display_gates, display_items, latch_changed)
            } else {
                (gates, items, false)
            };
        if report.closing_gates == display_gates
            && report.closing_items == display_items
            && report.closing_decisions == decisions
            && !latch_changed
        {
            return false;
        }
        report.closing_gates = display_gates;
        report.closing_items = display_items;
        report.closing_decisions = decisions;
        self.revision = self.revision.saturating_add(1);
        true
    }

    pub(crate) fn merge_closing_blockers(
        &mut self,
        gates: Vec<crate::api::schema::ClosingBlockItem>,
        items: Vec<crate::api::schema::ClosingBlockItem>,
    ) -> bool {
        let gates: Vec<_> = gates
            .into_iter()
            .filter(crate::api::schema::ClosingBlockItem::requires_human_input)
            .collect();
        let items: Vec<_> = items
            .into_iter()
            .filter(crate::api::schema::ClosingBlockItem::requires_human_input)
            .collect();
        let has_blockers = !gates.is_empty() || !items.is_empty();
        let agent_label = self
            .hook_authority
            .as_ref()
            .map(|authority| authority.agent_label.clone());
        let report = self.closing_report.get_or_insert_default();
        if report.unanswered.is_none() && has_blockers {
            report.unanswered = Some(UnansweredClosingBlockers {
                agent_label,
                ..UnansweredClosingBlockers::default()
            });
        }
        let mut changed = false;
        if let Some(unanswered) = report.unanswered.as_mut() {
            changed |= merge_unanswered_closing_blockers(unanswered, &gates, &items);
        }
        for gate in gates {
            if !report
                .closing_gates
                .iter()
                .any(|existing| same_closing_blocker(existing, &gate))
            {
                report.closing_gates.push(gate);
                changed = true;
            }
        }
        for item in items {
            if !report
                .closing_items
                .iter()
                .any(|existing| same_closing_blocker(existing, &item))
            {
                report.closing_items.push(item);
                changed = true;
            }
        }
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }

    pub(crate) fn apply_closing_report_subagents_at(
        &mut self,
        agents: Option<u32>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        if self
            .closing_report
            .as_ref()
            .and_then(|report| report.closing_report_subagents)
            == agents
        {
            return None;
        }
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        self.closing_report
            .get_or_insert_default()
            .closing_report_subagents = agents;
        self.revision = self.revision.saturating_add(1);
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            sidebar_projection_changed: true,
            ..TerminalStateMutation::default()
        })
    }

    pub(crate) fn has_blocking_closing_items(&self) -> bool {
        self.closing_items()
            .iter()
            .any(crate::api::schema::ClosingBlockItem::requires_human_input)
    }

    pub(crate) fn has_pending_human_input(&self) -> bool {
        !self.closing_gates().is_empty() || self.has_blocking_closing_items()
    }

    fn release_blocked_hold_at(&mut self, observed_at: Instant) -> bool {
        if self
            .blocked_state_hold
            .as_ref()
            .is_some_and(|hold| observed_at >= hold.since)
        {
            self.blocked_state_hold = None;
            true
        } else {
            false
        }
    }

    fn reconcile_blocked_state_hold_owner(
        &mut self,
        owner: &AgentActivityOwner,
        observed_at: Instant,
    ) -> bool {
        let should_release = self.blocked_state_hold.as_ref().is_some_and(|hold| {
            observed_at >= hold.since && !owner.matches_blocked_hold(&hold.owner)
        });
        if should_release {
            self.blocked_state_hold = None;
        }
        should_release
    }

    fn hold_blocked_state_for(&mut self, owner: AgentActivityOwner, observed_at: Instant) {
        match self.blocked_state_hold.as_mut() {
            Some(hold) if owner.matches_blocked_hold(&hold.owner) => {
                hold.owner = owner;
                hold.since = hold.since.max(observed_at);
            }
            _ => {
                self.blocked_state_hold = Some(BlockedStateHold {
                    owner,
                    since: observed_at,
                });
            }
        }
    }

    fn release_blocked_state_hold_for(
        &mut self,
        owner: &AgentActivityOwner,
        observed_at: Instant,
    ) -> bool {
        self.blocked_state_hold.as_ref().is_some_and(|hold| {
            owner.matches_blocked_hold(&hold.owner) && observed_at >= hold.since
        }) && self.release_blocked_hold_at(observed_at)
    }

    fn clear_unanswered_closing_blockers(&mut self) -> bool {
        let Some(report) = self.closing_report.as_mut() else {
            return false;
        };
        // Answering drops only the latch; answered entries stay displayed
        // until the next report replaces them, as queued input always has.
        if report.unanswered.take().is_none() {
            return false;
        }
        self.revision = self.revision.saturating_add(1);
        true
    }

    fn release_blocked_hold_for_visible_working(
        &mut self,
        agent: Option<Agent>,
        observed_at: Instant,
    ) -> bool {
        if self
            .closing_report
            .as_ref()
            .is_some_and(|report| report.unanswered.is_some())
        {
            return false;
        }
        let Some(agent) = agent else {
            return false;
        };
        let owner = self.blocked_hold_owner_for_agent(agent);
        let should_release = self.blocked_state_hold.as_ref().is_some_and(|hold| {
            owner.matches_blocked_hold(&hold.owner)
                && hold
                    .since
                    .checked_add(crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH)
                    .is_some_and(|stable_at| observed_at >= stable_at)
                && matches!(self.fallback_state, AgentState::Idle | AgentState::Blocked)
                && self
                    .fallback_observed_at
                    .is_some_and(|settled_at| settled_at > hold.since)
        });
        if !should_release {
            return false;
        }

        if let Some(authority) = self.hook_authority.as_mut().filter(|authority| {
            authority.state == AgentState::Blocked
                && authority.retired_at.is_none()
                && crate::detect::parse_agent_label(&authority.agent_label) == Some(agent)
        }) {
            authority.retired_at = Some(observed_at);
        }
        self.release_blocked_state_hold_for(&owner, observed_at)
    }

    fn blocked_hold_matches_current_activity(&self, hold: &BlockedStateHold) -> bool {
        self.current_agent_activity_owner()
            .map(|owner| owner.matches_blocked_hold(&hold.owner))
            .unwrap_or_else(|| {
                self.detected_agent.is_none()
                    && self.hook_authority.is_none()
                    && self.recent_agent_process_exit.is_none()
            })
    }

    pub(crate) fn closing_gates(&self) -> &[crate::api::schema::ClosingBlockItem] {
        self.closing_report
            .as_ref()
            .map_or(&[], |report| report.closing_gates.as_slice())
    }

    pub(crate) fn closing_items(&self) -> &[crate::api::schema::ClosingBlockItem] {
        self.closing_report
            .as_ref()
            .map_or(&[], |report| report.closing_items.as_slice())
    }

    pub(crate) fn closing_decisions(&self) -> &[crate::api::schema::ClosingBlockDecision] {
        self.closing_report
            .as_ref()
            .map_or(&[], |report| report.closing_decisions.as_slice())
    }

    #[cfg(test)]
    pub(crate) fn closing_gates_mut(&mut self) -> &mut Vec<crate::api::schema::ClosingBlockItem> {
        &mut self.closing_report.get_or_insert_default().closing_gates
    }

    #[cfg(test)]
    pub(crate) fn closing_items_mut(&mut self) -> &mut Vec<crate::api::schema::ClosingBlockItem> {
        &mut self.closing_report.get_or_insert_default().closing_items
    }

    pub(crate) fn closing_idle(&self) -> Option<bool> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.closing_idle)
    }

    pub(crate) fn closing_contract(&self) -> Option<&str> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.closing_contract.as_deref())
    }

    pub(crate) fn closing_contract_met(&self) -> Option<bool> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.closing_contract_met)
    }

    pub(crate) fn closing_contract_met_at(&self) -> Option<Instant> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.closing_contract_met_at)
    }

    pub(crate) fn last_turn_at(&self) -> Option<&str> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.last_turn_at.as_deref())
    }

    pub(crate) fn last_turn_at_instant(&self) -> Option<Instant> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.last_turn_at_instant)
    }

    pub(crate) fn last_turn_at_unix_s(&self) -> Option<u64> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.last_turn_at_unix_s)
    }

    pub(crate) fn set_last_turn_at(&mut self, last_turn_at: Option<String>) -> bool {
        let Some(last_turn_at) = last_turn_at.filter(|value| !value.trim().is_empty()) else {
            return false;
        };
        let report = self.closing_report.get_or_insert_default();
        if report.last_turn_at.as_deref() != Some(last_turn_at.as_str()) {
            let unix_seconds = crate::fleet::parse_utc_timestamp(&last_turn_at);
            let elapsed = unix_seconds.and_then(|seconds| {
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                now_unix.checked_sub(seconds).map(Duration::from_secs)
            });
            report.last_turn_at = Some(last_turn_at);
            report.last_turn_at_unix_s = unix_seconds;
            report.last_turn_at_instant = elapsed.and_then(|age| Instant::now().checked_sub(age));
            self.revision = self.revision.saturating_add(1);
            true
        } else {
            false
        }
    }

    pub(crate) fn closing_external_wait(&self) -> Option<&str> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.external_wait.as_deref())
    }

    pub(crate) fn metadata_tokens_for_api(&self) -> HashMap<String, String> {
        let mut tokens = self.metadata_tokens.values();
        let Some(report) = self.closing_report.as_ref() else {
            return tokens;
        };
        if let Some(completion) = report.completion {
            tokens.insert(
                "closing_completion".into(),
                match completion {
                    crate::api::schema::ClosingCompletion::Complete => "complete",
                    crate::api::schema::ClosingCompletion::Incomplete => "incomplete",
                    crate::api::schema::ClosingCompletion::Missing => "missing",
                }
                .into(),
            );
        }
        if let Some(parse_status) = report.parse_status {
            tokens.insert(
                "closing_parse".into(),
                match parse_status {
                    crate::api::schema::ClosingParseStatus::Ok => "ok",
                    crate::api::schema::ClosingParseStatus::Missing => "missing",
                    crate::api::schema::ClosingParseStatus::Malformed => "malformed",
                }
                .into(),
            );
        }
        if let Some(wait) = report.external_wait.as_ref() {
            tokens.insert("closing_wait".into(), wait.clone());
        }
        if let Some(workers_unknown) = report.workers_unknown {
            tokens.insert(
                "closing_workers_unknown".into(),
                if workers_unknown { "1" } else { "0" }.into(),
            );
        }
        if let Some(agents) = report.closing_report_subagents {
            tokens.insert("closing_agents".into(), agents.to_string());
        }
        if !report.closing_gates.is_empty() || !report.closing_items.is_empty() {
            let blocking = report.closing_gates.len()
                + report
                    .closing_items
                    .iter()
                    .filter(|item| item.requires_human_input())
                    .count();
            tokens.insert("closing_blocking".into(), blocking.to_string());
            let gates = report
                .closing_gates
                .iter()
                .map(|gate| gate.text.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            if !gates.is_empty() {
                tokens.insert("closing_gates".into(), gates.chars().take(200).collect());
            }
        }
        if let Some(idle) = report.closing_idle {
            tokens.insert("closing_idle".into(), if idle { "1" } else { "0" }.into());
        }
        if let (Some(contract), Some(met)) = (&report.closing_contract, report.closing_contract_met)
        {
            tokens.insert("closing_contract".into(), contract.clone());
            tokens.insert(
                "closing_contract_met".into(),
                if met { "1" } else { "0" }.into(),
            );
        }
        tokens
    }

    pub(crate) fn set_closing_report_scope(
        &mut self,
        source: String,
        session_id: Option<String>,
        turn_seq: Option<u64>,
    ) {
        let report = self.closing_report.get_or_insert_default();
        let scope = &mut report.scope;
        scope.source = Some(source);
        scope.session_id = session_id;
        scope.turn_seq = turn_seq;
    }

    pub(crate) fn closing_task_reported(&self) -> bool {
        self.closing_report.as_ref().is_some_and(|report| {
            report.completion.is_some()
                || report.parse_status.is_some()
                || report.workers_unknown.is_some()
                || report.external_wait.is_some()
        })
    }

    pub(crate) fn has_closing_report(&self) -> bool {
        self.closing_report.as_ref().is_some_and(|report| {
            !report.closing_gates.is_empty()
                || !report.closing_items.is_empty()
                || !report.closing_decisions.is_empty()
                || report.unanswered.is_some()
                || report.closing_report_subagents.is_some()
                || report.closing_idle.is_some()
                || report.closing_contract.is_some()
                || report.closing_contract_met.is_some()
                || report.completion.is_some()
                || report.external_wait.is_some()
                || report.parse_status.is_some()
                || report.workers_unknown.is_some()
        })
    }

    pub(crate) fn closing_task_complete(&self) -> bool {
        self.closing_report.as_ref().is_some_and(|report| {
            report.completion == Some(crate::api::schema::ClosingCompletion::Complete)
                || report.closing_contract_met == Some(true)
        })
    }

    pub(crate) fn closing_completion_is_complete(&self) -> bool {
        self.closing_report.as_ref().is_some_and(|report| {
            report.completion == Some(crate::api::schema::ClosingCompletion::Complete)
        })
    }

    pub(crate) fn auto_settle_persistence_state(&self, session_id: &str) -> (bool, Option<u64>) {
        let Some(report) = self.closing_report.as_ref() else {
            return (false, None);
        };
        let replied =
            report.user_replied && report.user_replied_session_id.as_deref() == Some(session_id);
        let consumed = report
            .auto_settle_consumed_turn
            .as_ref()
            .filter(|(saved_session, _)| saved_session == session_id)
            .map(|(_, seq)| *seq);
        (replied, consumed)
    }

    /// Consume each ready closing turn once, after an actual Herdr client reply.
    pub(crate) fn observe_auto_settle_transition(
        &mut self,
        settle_ready: bool,
        complete_idle_without_blockers: bool,
        session_replaced: bool,
    ) -> bool {
        if session_replaced {
            if let Some(report) = self.closing_report.as_mut() {
                report.user_replied = false;
                report.user_replied_session_id = None;
                report.auto_settle_consumed_turn = None;
            }
        }
        let report = self.closing_report.get_or_insert_default();
        if !settle_ready || !complete_idle_without_blockers {
            return false;
        }
        let Some(turn_seq) = report.scope.turn_seq else {
            return false;
        };
        let session_id = report.scope.session_id.clone().unwrap_or_default();
        let turn = (session_id.clone(), turn_seq);
        if report.auto_settle_consumed_turn.as_ref() == Some(&turn) {
            return false;
        }
        report.auto_settle_consumed_turn = Some(turn);
        report.user_replied && report.user_replied_session_id.as_deref() == Some(&session_id)
    }

    pub(crate) fn note_user_reply(&mut self) {
        let Some(session_id) = self
            .current_session_identity_for_persistence()
            .map(|(_, _, _, value)| value)
        else {
            return;
        };
        let report = self.closing_report.get_or_insert_default();
        report.user_replied = true;
        report.user_replied_session_id = Some(session_id);
    }

    /// Automated nudges may produce a closing turn, but they do not authorize
    /// auto-settlement. Require a fresh successful client input afterward.
    pub(crate) fn clear_auto_settle_user_reply(&mut self) {
        if let Some(report) = self.closing_report.as_mut() {
            report.user_replied = false;
            report.user_replied_session_id = None;
        }
    }

    pub(crate) fn restore_auto_settle_state(
        &mut self,
        session_id: &str,
        user_replied: bool,
        consumed_turn_seq: Option<u64>,
    ) {
        let report = self.closing_report.get_or_insert_default();
        report.user_replied = user_replied;
        report.user_replied_session_id = user_replied.then(|| session_id.to_string());
        report.auto_settle_consumed_turn =
            consumed_turn_seq.map(|seq| (session_id.to_string(), seq));
    }

    pub(crate) fn take_retired_closing_report_completion(&mut self) -> bool {
        let completed = self.closing_report.as_ref().is_some_and(|report| {
            report.retired_pending_completion
                && (report.completion == Some(crate::api::schema::ClosingCompletion::Complete)
                    || report.closing_contract_met == Some(true))
        });
        if completed {
            if let Some(report) = self.closing_report.as_mut() {
                report.retired_pending_completion = false;
            }
        }
        completed
    }

    /// Runtime evidence settles the closing prose: the provider transcript
    /// scan is caught up and trustworthy and reports no live sub-agent. Only
    /// then may an idle pane with an incomplete or worker-claiming closing
    /// block leave `Unknown`. `None` (no transcript, not caught up, or an
    /// untrustworthy scan) keeps the pane uncertain.
    pub(crate) fn runtime_workers_settled(&self) -> bool {
        self.active_subagents == Some(0)
    }

    pub(crate) fn closing_log_tokens(&self) -> ClosingLogTokens {
        let Some(report) = self.closing_report.as_ref() else {
            return ClosingLogTokens::default();
        };
        ClosingLogTokens {
            completion: report.completion.map(|completion| match completion {
                crate::api::schema::ClosingCompletion::Complete => "complete",
                crate::api::schema::ClosingCompletion::Incomplete => "incomplete",
                crate::api::schema::ClosingCompletion::Missing => "missing",
            }),
            parse: report.parse_status.map(|parse| match parse {
                crate::api::schema::ClosingParseStatus::Ok => "ok",
                crate::api::schema::ClosingParseStatus::Missing => "missing",
                crate::api::schema::ClosingParseStatus::Malformed => "malformed",
            }),
            workers_unknown: report.workers_unknown,
            idle: report.closing_idle,
        }
    }

    fn closing_task_projection(
        &self,
        state: AgentState,
        has_pending_human_input: bool,
    ) -> (AgentState, &'static str) {
        if state != AgentState::Working && has_pending_human_input {
            (AgentState::Blocked, "closing_human_input")
        } else if self.closing_external_wait().is_some() && state != AgentState::Blocked {
            (AgentState::Working, "closing_external_wait")
        } else if state == AgentState::Idle
            && (!self.runtime_workers_settled()
                && (self.closing_task_reported() && !self.closing_task_complete()
                    || self.closing_report.as_ref().is_some_and(|report| {
                        report.workers_unknown == Some(true)
                            || report
                                .closing_report_subagents
                                .is_some_and(|count| count > 0)
                    }))
                || self.closing_report.as_ref().is_some_and(|report| {
                    matches!(
                        report.parse_status,
                        Some(
                            crate::api::schema::ClosingParseStatus::Missing
                                | crate::api::schema::ClosingParseStatus::Malformed
                        )
                    )
                }))
        {
            (AgentState::Unknown, "closing_task_uncertain")
        } else {
            (state, "lifecycle")
        }
    }

    pub(crate) fn apply_closing_task_report(
        &mut self,
        completion: Option<crate::api::schema::ClosingCompletion>,
        external_wait: Option<String>,
        parse_status: Option<crate::api::schema::ClosingParseStatus>,
        workers_unknown: Option<bool>,
        _now: Instant,
    ) -> bool {
        if completion.is_none()
            && external_wait.is_none()
            && parse_status.is_none()
            && workers_unknown.is_none()
        {
            return false;
        }
        let preserve_dependencies = matches!(
            parse_status,
            Some(
                crate::api::schema::ClosingParseStatus::Missing
                    | crate::api::schema::ClosingParseStatus::Malformed
            )
        );
        let external_wait = external_wait.and_then(|wait| {
            let wait = wait
                .trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(80)
                .collect::<String>();
            (!wait.is_empty()).then_some(wait)
        });
        let report = self.closing_report.get_or_insert_default();
        let mut changed = report.completion != completion || report.parse_status != parse_status;
        report.completion = completion;
        report.parse_status = parse_status;
        if !preserve_dependencies {
            changed |=
                report.external_wait != external_wait || report.workers_unknown != workers_unknown;
            report.external_wait = external_wait;
            report.workers_unknown = workers_unknown;
        }
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }

    fn clear_closing_task_report(&mut self, now: Instant) -> bool {
        let _ = now;
        let (requires_legacy_session_guard, legacy_session_guard) = self
            .closing_report
            .as_ref()
            .map(|report| {
                (
                    report.requires_legacy_session_guard,
                    report.legacy_session_guard.clone(),
                )
            })
            .unwrap_or_default();
        let unanswered = self
            .closing_report
            .as_ref()
            .and_then(|report| report.unanswered.clone());
        let (user_replied, user_replied_session_id, auto_settle_consumed_turn) = self
            .closing_report
            .as_ref()
            .map(|report| {
                (
                    report.user_replied,
                    report.user_replied_session_id.clone(),
                    report.auto_settle_consumed_turn.clone(),
                )
            })
            .unwrap_or_default();
        let empty = ClosingReport {
            requires_legacy_session_guard,
            legacy_session_guard,
            closing_gates: unanswered
                .as_ref()
                .map_or_else(Vec::new, |latch| latch.gates.clone()),
            closing_items: unanswered
                .as_ref()
                .map_or_else(Vec::new, |latch| latch.items.clone()),
            unanswered,
            user_replied,
            user_replied_session_id,
            auto_settle_consumed_turn,
            ..ClosingReport::default()
        };
        let report_changed = self
            .closing_report
            .as_ref()
            .is_some_and(|report| report != &empty);
        self.closing_report = Some(empty);
        let legacy_tokens_changed = self.metadata_tokens.remove_prefixed("closing_");
        report_changed || legacy_tokens_changed
    }

    fn retire_closing_report_outside_current_session(
        &mut self,
        session_changed: bool,
        session_replaced: bool,
        now: Instant,
    ) -> bool {
        if session_replaced {
            let report = self.closing_report.get_or_insert_default();
            report.requires_legacy_session_guard = true;
            report.legacy_session_guard = None;
            report.user_replied = false;
            report.user_replied_session_id = None;
            report.auto_settle_consumed_turn = None;
        }
        if !session_changed || !self.has_closing_report() {
            return false;
        }
        let current_session_id = self
            .current_session_identity_for_persistence()
            .map(|(_, _, _, value)| value);
        let report_matches_current_session = self
            .closing_report
            .as_ref()
            .and_then(|report| report.scope.session_id.as_deref())
            .zip(current_session_id.as_deref())
            .is_some_and(|(report_session_id, current_session_id)| {
                report_session_id == current_session_id
            });
        if report_matches_current_session {
            return false;
        }
        if let Some(authority) = self.hook_authority.as_mut().filter(|authority| {
            authority.retired_at.is_none()
                && crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
        }) {
            authority.retired_at = Some(now);
        }
        // The gate belonged to a session that no longer owns the pane.
        self.clear_unanswered_closing_blockers();
        let changed = self.clear_closing_task_report(now);
        if changed {
            self.closing_report
                .get_or_insert_default()
                .retired_pending_completion = true;
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn apply_closing_contract_tokens(
        &mut self,
        tokens: &HashMap<String, Option<String>>,
        now: Instant,
    ) -> bool {
        let Some(closing_idle) = tokens.get("closing_idle") else {
            return false;
        };
        let closing_idle = closing_idle.as_deref().and_then(parse_binary_token);
        let contract = tokens
            .get("closing_contract")
            .and_then(|value| value.as_deref())
            .filter(|value| !value.is_empty());
        let contract_met = tokens
            .get("closing_contract_met")
            .and_then(|value| value.as_deref())
            .and_then(parse_binary_token);
        let (closing_contract, closing_contract_met) = match (contract, contract_met) {
            (Some(contract), Some(contract_met)) => {
                (Some(contract.to_string()), Some(contract_met))
            }
            _ => (None, None),
        };
        let closing_contract_met_at = closing_contract_met.is_some_and(|met| met).then_some(now);
        let report = self.closing_report.get_or_insert_default();
        let changed = report.closing_idle != closing_idle
            || report.closing_contract != closing_contract
            || report.closing_contract_met != closing_contract_met
            || report.closing_contract_met_at != closing_contract_met_at;
        report.closing_idle = closing_idle;
        report.closing_contract = closing_contract;
        report.closing_contract_met = closing_contract_met;
        report.closing_contract_met_at = closing_contract_met_at;
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }

    pub fn effective_work_context(&self) -> &crate::work_context::PaneWorkContext {
        self.work_context.effective()
    }

    pub fn manual_work_context(&self) -> &crate::work_context::PaneWorkContext {
        self.work_context.manual()
    }

    pub(crate) fn apply_manual_work_context_patch(
        &mut self,
        patch: crate::work_context::PaneWorkContextPatch,
    ) -> Result<bool, String> {
        let changed = self.work_context.apply_manual_patch(patch)?;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(changed)
    }

    pub(crate) fn replace_prevalidated_manual_work_context(
        &mut self,
        context: crate::work_context::PaneWorkContext,
    ) -> bool {
        let changed = self.work_context.replace_manual_normalized(context);
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn clear_active_work_owner(&mut self) -> bool {
        let changed = self.work_context.clear_active_owner();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn replace_hook_work_context(
        &mut self,
        context: crate::work_context::PaneWorkContext,
    ) -> Result<bool, String> {
        let changed = self.work_context.replace_hook_turn(context)?;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(changed)
    }

    /// Apply an agent-reported session rename without disturbing the rest of
    /// the hook work context.
    pub(crate) fn set_hook_session_name(
        &mut self,
        session_name: Option<String>,
    ) -> Result<bool, String> {
        let changed = self.work_context.set_hook_session_name(session_name)?;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(changed)
    }

    pub(crate) fn replace_git_work_context(
        &mut self,
        context: crate::work_context::PaneWorkContext,
    ) -> Result<bool, String> {
        let changed = self.work_context.replace_git_observation(context)?;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(changed)
    }

    pub(crate) fn set_inferred_pr_url(&mut self, url: String) -> Result<bool, String> {
        let mut context = self.work_context.snapshot_tiers().git_observation;
        context.set_inferred_pr_url(url)?;
        self.replace_git_work_context(context)
    }

    /// The hook tier is persisted for restore fidelity, but any accepted
    /// mutation that tears down or replaces the session identity that authorized guarded
    /// work-title reports must also drop the hook tier, so stale ticket/PR refs
    /// never outlive their session. Manual, git, and restored-fallback tiers are
    /// untouched.
    fn clear_hook_work_context(&mut self) -> bool {
        let changed = self.work_context.clear_hook_turn();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn restore_work_context(
        &mut self,
        context: crate::work_context::PaneWorkContext,
    ) -> Result<(), String> {
        self.work_context = crate::work_context::PaneWorkContextState::from_restored(context)?;
        Ok(())
    }

    pub(crate) fn restore_work_context_with_tiers(
        &mut self,
        flat: crate::work_context::PaneWorkContext,
        tiers: Option<crate::work_context::PaneWorkContextTiers>,
    ) -> Result<(), String> {
        let Some(tiers) = tiers else {
            return self.restore_work_context(flat);
        };
        self.work_context =
            crate::work_context::PaneWorkContextState::from_restored_with_tiers(flat, Some(tiers))?;
        Ok(())
    }

    pub(crate) fn set_process_state(
        &mut self,
        holds_shell: bool,
        stale_resolution: Option<(AgentState, bool)>,
        observed_at: Instant,
    ) -> (bool, Option<TerminalStateMutation>) {
        let stale_resolution = if self
            .hook_authority
            .as_ref()
            .is_none_or(|authority| authority.reported_at < observed_at)
        {
            stale_resolution
        } else {
            self.stale_resolution
        };
        let clears_stale = self.supervisor_stale
            && stale_resolution.is_some_and(|(state, _)| state != AgentState::Working);
        if self.holds_shell == holds_shell
            && self.stale_resolution == stale_resolution
            && !clears_stale
        {
            return (false, None);
        }
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation =
            self.effective_presentation_for_state_at(previous_state, observed_at);
        self.holds_shell = holds_shell;
        self.stale_resolution = stale_resolution;
        if let Some((state, _)) = stale_resolution.filter(|_| clears_stale) {
            if let Some(authority) = self.hook_authority.as_mut() {
                authority.retired_at = Some(observed_at);
            }
            self.state = state;
            self.working_since = (state == AgentState::Working).then_some(observed_at);
            self.fallback_state = state;
            self.fallback_visible_blocker = false;
            self.fallback_visible_working = false;
            self.fallback_visible_working_observed_at = None;
            self.fallback_working_observed_at = None;
            self.fallback_observed_at = Some(observed_at);
            self.supervisor_stale = false;
            self.unreported_turn_started_at = None;
            if state == AgentState::Working {
                if let Some(owner) = self.current_agent_activity_owner() {
                    self.release_blocked_state_hold_for(&owner, observed_at);
                }
            }
        }
        self.revision = self.revision.wrapping_add(1);
        let mutation = clears_stale.then(|| {
            let agent_label = self.effective_agent_label().map(str::to_string);
            let known_agent = self.effective_known_agent();
            let state = self.state;
            TerminalStateMutation {
                effective_state_change: Some(EffectiveStateChange {
                    previous_agent_label,
                    previous_known_agent,
                    previous_state,
                    previous_presentation,
                    agent_label,
                    known_agent,
                    state,
                    presentation: self.effective_presentation_for_state_at(state, observed_at),
                }),
                sidebar_projection_changed: true,
                ..TerminalStateMutation::default()
            }
        });
        (true, mutation)
    }

    pub(crate) fn sidebar_projection(&self, seen: bool) -> (AgentState, bool) {
        self.sidebar_projection_with_pending_human_input(seen, self.has_pending_human_input())
    }

    pub(crate) fn sidebar_projection_with_pending_human_input(
        &self,
        seen: bool,
        has_pending_human_input: bool,
    ) -> (AgentState, bool) {
        let active_subagents = self.verified_active_subagents();
        let (state, seen) = if self.supervisor_stale {
            self.stale_resolution.unwrap_or((self.state, seen))
        } else if active_subagents == Some(0) && self.closing_task_complete() {
            (self.lifecycle_state_and_arbitration().0, seen)
        } else {
            (self.state, seen)
        };
        if state != AgentState::Blocked && active_subagents.is_some_and(|count| count > 0) {
            (AgentState::Working, seen)
        } else {
            let state = self
                .closing_task_projection(state, has_pending_human_input)
                .0;
            (state, seen)
        }
    }

    pub(crate) fn sidebar_projection_reason(&self, has_pending_human_input: bool) -> &'static str {
        if self.supervisor_stale && self.stale_resolution.is_some() {
            return "supervisor_stale";
        }
        let active_subagents = self.verified_active_subagents();
        let state = if self.supervisor_stale {
            self.stale_resolution
                .map(|(state, _)| state)
                .unwrap_or(self.state)
        } else if active_subagents == Some(0) && self.closing_task_complete() {
            self.lifecycle_state_and_arbitration().0
        } else {
            self.state
        };
        if state != AgentState::Blocked && active_subagents.is_some_and(|count| count > 0) {
            "active_subagents"
        } else {
            self.closing_task_projection(state, has_pending_human_input)
                .1
        }
    }

    pub(crate) fn clear_stale_for_settlement(&mut self) -> bool {
        if !self.supervisor_stale && self.stale_resolution.is_none() {
            return false;
        }
        self.supervisor_stale = false;
        self.stale_resolution = None;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn set_active_subagents(&mut self, count: Option<u32>) -> bool {
        if self.active_subagents == count {
            if count.is_some() && self.supervisor_stale {
                self.supervisor_stale = false;
                self.stale_resolution = None;
                self.revision = self.revision.wrapping_add(1);
                return true;
            }
            if count.is_none() {
                let observations_changed = self.claude_subagent_observations.take().is_some();
                let activity_changed = self.clear_claude_subagent_transcript_activity();
                if observations_changed || activity_changed {
                    self.revision = self.revision.wrapping_add(1);
                    return true;
                }
            }
            return false;
        }
        self.active_subagents = count;
        if count.is_none() {
            self.claude_subagent_observations = None;
            self.clear_claude_subagent_transcript_activity();
        } else {
            self.supervisor_stale = false;
            self.stale_resolution = None;
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn set_active_subagents_with_projection_at(
        &mut self,
        count: Option<u32>,
        seen: bool,
        now: Instant,
    ) -> (bool, Option<TerminalStateMutation>) {
        let completion_waiting_on_worker = self
            .verified_active_subagents()
            .is_some_and(|count| count > 0)
            && self.closing_task_complete();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.sidebar_projection(seen).0;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let changed = self.set_active_subagents(count);
        if !changed {
            return (false, None);
        }
        let state = self.sidebar_projection(seen).0;
        let completed_after_last_worker = completion_waiting_on_worker
            && self.verified_active_subagents() == Some(0)
            && previous_state == AgentState::Working
            && state == AgentState::Idle;
        let effective_state_change = completed_after_last_worker.then(|| EffectiveStateChange {
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            agent_label: self.effective_agent_label().map(str::to_string),
            known_agent: self.effective_known_agent(),
            state,
            presentation: self.effective_presentation_for_state_at(state, now),
        });
        (
            true,
            Some(TerminalStateMutation {
                effective_state_change,
                sidebar_projection_changed: true,
                ..TerminalStateMutation::default()
            }),
        )
    }

    pub(crate) fn set_claude_subagent_observations(
        &mut self,
        observations: Option<Vec<crate::app::claude_subagents::ClaudeSubagentObservation>>,
    ) -> bool {
        if self.claude_subagent_observations == observations {
            return false;
        }
        self.claude_subagent_observations = observations;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn set_claude_subagent_transcript_activity_at(
        &mut self,
        activity: SubagentTranscriptActivity,
        observed_at: Instant,
    ) -> bool {
        if activity == SubagentTranscriptActivity::Unknown {
            return false;
        }
        let changed = self.claude_subagent_transcript_activity != activity;
        self.claude_subagent_transcript_activity = activity;
        self.claude_subagent_activity_observed_at =
            (activity == SubagentTranscriptActivity::Fresh).then_some(observed_at);
        changed
    }

    fn clear_claude_subagent_transcript_activity(&mut self) -> bool {
        let changed = self.claude_subagent_transcript_activity
            != SubagentTranscriptActivity::Unknown
            || self.claude_subagent_activity_observed_at.is_some();
        self.claude_subagent_transcript_activity = SubagentTranscriptActivity::Unknown;
        self.claude_subagent_activity_observed_at = None;
        changed
    }

    pub(crate) fn set_claude_transcript_target(
        &mut self,
        session_id: Option<String>,
        path: Option<PathBuf>,
    ) {
        if self.claude_transcript_session_id != session_id || self.claude_transcript_path != path {
            self.clear_claude_subagent_transcript_activity();
        }
        self.claude_transcript_session_id = session_id;
        self.claude_transcript_path = path;
    }

    pub(crate) fn set_session_name_write_target(
        &mut self,
        target: Option<crate::work_title::SessionNameWriteTarget>,
    ) {
        self.session_name_write_target = target;
    }

    pub(crate) fn session_name_write_target(
        &self,
    ) -> Option<&crate::work_title::SessionNameWriteTarget> {
        let target = self.session_name_write_target.as_ref()?;
        let (source, agent, kind, value) = self.current_session_identity_for_persistence()?;
        target
            .matches_session(&source, &agent, kind, &value)
            .then_some(target)
    }

    pub(crate) fn set_foreground_process(
        &mut self,
        name: Option<String>,
        active: bool,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        if self.foreground_process_name == name && self.foreground_process_active == active {
            return None;
        }
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        self.foreground_process_name = name;
        self.foreground_process_active = active;
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            ..TerminalStateMutation::default()
        })
    }

    /// Where this pane's session log names the model in use, when known.
    pub(crate) fn agent_model_log_source(
        &self,
    ) -> Option<crate::app::agent_model_log::ModelLogSource> {
        use crate::app::agent_model_log::ModelLogSource;
        match self.effective_known_agent()? {
            Agent::Claude => self
                .claude_transcript_path
                .clone()
                .map(ModelLogSource::ClaudeTranscript),
            Agent::Codex => {
                let (_, agent, kind, value) = self.current_session_identity_for_persistence()?;
                (agent == "codex" && kind == crate::agent_resume::AgentSessionRefKind::Id)
                    .then_some(ModelLogSource::CodexSession(value))
            }
            _ => None,
        }
    }

    pub(crate) fn foreground_process_active(&self) -> bool {
        self.foreground_process_active
    }

    pub(crate) fn terminal_title_stripped(&self) -> Option<String> {
        self.terminal_title
            .as_deref()
            .and_then(super::stripped_terminal_title)
    }

    pub(crate) fn resolve_work_title_for_session(
        &mut self,
        agent_label: &str,
        lifecycle_source: &str,
        session_id: &str,
        latest_title: Option<String>,
        replace_existing_title: bool,
    ) -> Option<String> {
        let same_session = self
            .work_title_initial_subject
            .as_ref()
            .is_some_and(|initial| {
                initial.agent_label == agent_label
                    && initial.lifecycle_source == lifecycle_source
                    && initial.session_id == session_id
            });
        if same_session {
            if let Some(initial) = self.work_title_initial_subject.as_mut() {
                if replace_existing_title {
                    if let Some(title) = latest_title {
                        initial.title = title;
                    }
                }
                return Some(initial.title.clone());
            }
            return None;
        }
        let title = latest_title?;
        self.work_title_initial_subject = Some(WorkTitleInitialSubject {
            agent_label: agent_label.to_string(),
            lifecycle_source: lifecycle_source.to_string(),
            session_id: session_id.to_string(),
            title: title.clone(),
        });
        Some(title)
    }

    pub(crate) fn set_terminal_title(&mut self, title: Option<String>) -> TerminalTitleChange {
        if self.terminal_title == title {
            return TerminalTitleChange::default();
        }
        let previous_stripped = self.terminal_title_stripped();
        self.terminal_title = title;
        let stripped_changed = previous_stripped != self.terminal_title_stripped();
        if stripped_changed {
            self.revision = self.revision.wrapping_add(1);
        }
        TerminalTitleChange {
            raw_changed: true,
            stripped_changed,
        }
    }

    pub fn with_launch_argv(mut self, argv: Vec<String>) -> Self {
        self.launch_argv = Some(argv);
        self
    }

    pub fn with_respawn_shell_on_exit(mut self) -> Self {
        self.respawn_shell_on_exit = true;
        self
    }

    #[cfg(any(windows, test))]
    pub(crate) fn agent_process_exited_within(&self, now: Instant, max_age: Duration) -> bool {
        self.recent_agent_process_exit
            .is_some_and(|exit| now.saturating_duration_since(exit.observed_at) <= max_age)
    }

    pub fn with_pending_agent_resume_plan(
        mut self,
        plan: crate::agent_resume::AgentResumePlan,
    ) -> Self {
        self.pending_agent_resume_plan = Some(plan);
        self
    }

    #[cfg(test)]
    pub fn set_detected_state(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_visible_blocker(agent, fallback_state, false, false, false)
    }

    #[cfg(test)]
    pub fn set_detected_state_with_mutation(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> TerminalStateMutation {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            false,
            false,
            false,
            false,
            false,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub fn set_detected_state_with_visible_blocker(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        _ignored_screen_idle: bool,
        process_exited: bool,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            visible_blocker,
            false,
            false,
            false,
            process_exited,
            Instant::now(),
        )
        .effective_state_change
    }

    pub fn set_detected_state_with_screen_signals_at(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        _visible_idle: bool,
        visible_working: bool,
        usage_limited: bool,
        process_exited: bool,
        now: Instant,
    ) -> TerminalStateMutation {
        self.usage_limited = usage_limited;
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_detected_agent = self.detected_agent;
        let previous_session = self.current_session_identity_for_persistence();
        let previous_screen_settled_after_report =
            self.hook_authority.as_ref().is_some_and(|authority| {
                self.fallback_state != AgentState::Working
                    && self
                        .fallback_observed_at
                        .is_some_and(|observed_at| observed_at > authority.reported_at)
            });
        let previous_native_screen_settled =
            matches!(self.fallback_state, AgentState::Idle | AgentState::Blocked)
                && self.fallback_observed_at.is_some()
                && self
                    .hook_authority
                    .as_ref()
                    .is_none_or(|authority| !self.hook_authority_is_effective(authority));
        let visible_working_signal = visible_working && fallback_state == AgentState::Working;
        self.fallback_visible_working = visible_working_signal;
        self.fallback_visible_working_observed_at = visible_working_signal.then_some(now);
        self.fallback_working_observed_at = (fallback_state == AgentState::Working).then_some(now);
        let sidebar_projection_changed = self.reconcile_new_turn_or_stale_screen_resolution(
            fallback_state,
            visible_working_signal,
            previous_screen_settled_after_report,
            previous_native_screen_settled,
            now,
        );
        let newer_custom_authority = process_exited
            && self.hook_authority.as_ref().is_some_and(|authority| {
                crate::detect::parse_agent_label(&authority.agent_label) == agent
                    && !crate::agent_resume::is_official_agent_source(
                        &authority.source,
                        &authority.agent_label,
                    )
                    && authority.reported_at > now
            });
        let agent_released = process_exited
            && !newer_custom_authority
            && (previous_agent_label.is_some() || self.agent_name.is_some());
        let latch_owner_changed = agent.is_some_and(|agent| {
            self.closing_report
                .as_ref()
                .and_then(|report| report.unanswered.as_ref())
                .and_then(|latch| latch.agent_label.as_deref())
                .and_then(crate::detect::parse_agent_label)
                .is_some_and(|owner| owner != agent)
        });
        if latch_owner_changed || (process_exited && !newer_custom_authority) {
            self.clear_unanswered_closing_blockers();
        }
        if visible_working_signal {
            self.release_blocked_hold_for_visible_working(agent, now);
        }
        if self.should_ignore_detected_state_under_full_lifecycle_hook(agent, process_exited) {
            if self
                .hook_authority
                .as_ref()
                .and_then(|authority| crate::detect::parse_agent_label(&authority.agent_label))
                == agent
            {
                self.detected_agent = agent;
            }
            self.fallback_state = fallback_state;
            self.fallback_visible_blocker =
                visible_blocker && fallback_state == AgentState::Blocked;
            self.fallback_observed_at = Some(now);
            return TerminalStateMutation {
                effective_state_change: self.recompute_effective_state(
                    previous_agent_label,
                    previous_known_agent,
                    previous_state,
                    previous_presentation,
                    now,
                ),
                session_ref_changed: previous_session
                    != self.current_session_identity_for_persistence(),
                session_replaced: previous_session.is_some()
                    && previous_session != self.current_session_identity_for_persistence(),
                hook_work_context_changed: false,
                agent_released: false,
                sidebar_projection_changed,
            };
        }
        let replacement_process_detected = !process_exited
            && agent.is_some()
            && self
                .recent_agent_process_exit
                .is_some_and(|exit| Some(exit.agent) == agent && exit.observed_at < now);
        if !process_exited && self.detected_state_observed_before_release_suppression(agent, now) {
            return TerminalStateMutation {
                effective_state_change: self.recompute_effective_state(
                    previous_agent_label,
                    previous_known_agent,
                    previous_state,
                    previous_presentation,
                    now,
                ),
                session_ref_changed: previous_session
                    != self.current_session_identity_for_persistence(),
                session_replaced: previous_session.is_some()
                    && previous_session != self.current_session_identity_for_persistence(),
                hook_work_context_changed: false,
                agent_released: false,
                sidebar_projection_changed,
            };
        }
        if let Some(agent) = agent {
            let owner = self.blocked_hold_owner_for_agent(agent);
            self.reconcile_blocked_state_hold_owner(&owner, now);
        }
        if process_exited && !newer_custom_authority {
            self.release_blocked_hold_at(now);
        }
        self.detected_agent = agent;
        if let Some(agent) = agent {
            let agent_label = crate::detect::agent_label(agent);
            self.reconcile_agent_name_owner(agent_label, None);
        }
        if !process_exited {
            self.clear_full_lifecycle_hook_suppression_for_detected_agent(
                if replacement_process_detected {
                    None
                } else {
                    previous_detected_agent
                },
                agent,
            );
        }
        self.fallback_state = fallback_state;
        self.fallback_visible_blocker = visible_blocker && fallback_state == AgentState::Blocked;
        self.fallback_observed_at = Some(now);
        if process_exited {
            if let Some(agent) = agent {
                self.recent_agent_process_exit = Some(RecentAgentProcessExit {
                    agent,
                    observed_at: now,
                });
            }
        } else if agent.is_some() {
            self.recent_agent_process_exit = None;
        }
        if process_exited {
            self.claude_transcript_session_id = None;
            self.claude_transcript_path = None;
            self.session_name_write_target = None;
            self.set_active_subagents(None);
            let mut reset_sources = Vec::new();
            let mut stale_sessions = Vec::new();
            for (source, suppressed) in &mut self.suppressed_full_lifecycle_hook_reports {
                if crate::detect::parse_agent_label(&suppressed.agent_label) != agent
                    || suppressed.reason == FullLifecycleHookSuppressionReason::HookClear
                {
                    continue;
                }
                let exited_session_ref = suppressed
                    .replacement_session_ref
                    .take()
                    .or_else(|| {
                        suppressed
                            .pending_replacement_report
                            .as_ref()
                            .and_then(|pending| pending.authority.session_ref.clone())
                    })
                    .or_else(|| suppressed.session_ref.clone());
                if let (Some(previous), Some(exited)) =
                    (suppressed.session_ref.as_ref(), exited_session_ref.as_ref())
                {
                    if previous != exited {
                        stale_sessions.push((
                            source.clone(),
                            suppressed.agent_label.clone(),
                            previous.clone(),
                        ));
                    }
                }
                suppressed.session_ref = exited_session_ref;
                suppressed.pending_replacement_report = None;
                suppressed.observed_at = now;
                reset_sources.push(source.clone());
            }
            for (source, agent_label, session_ref) in stale_sessions {
                self.remember_stale_full_lifecycle_hook_session(source, agent_label, session_ref);
            }
            for source in reset_sources {
                self.hook_report_sequences.remove(&source);
            }

            let official_session = self
                .hook_authority
                .as_ref()
                .filter(|authority| {
                    crate::agent_resume::is_official_agent_source(
                        &authority.source,
                        &authority.agent_label,
                    ) && crate::detect::parse_agent_label(&authority.agent_label) == agent
                })
                .map(|authority| {
                    (
                        authority.source.clone(),
                        authority.agent_label.clone(),
                        authority.session_ref.clone(),
                    )
                })
                .or_else(|| {
                    self.persisted_agent_session.as_ref().and_then(|session| {
                        (crate::agent_resume::is_official_agent_source(
                            &session.source,
                            &session.agent,
                        ) && crate::detect::parse_agent_label(&session.agent) == agent)
                            .then(|| {
                                (
                                    session.source.clone(),
                                    session.agent.clone(),
                                    Some(session.session_ref.clone()),
                                )
                            })
                    })
                });
            if let Some((source, agent_label, session_ref)) = official_session {
                self.hook_report_sequences.remove(&source);
                self.suppress_full_lifecycle_hook_report_with_session_ref(
                    source,
                    agent_label,
                    session_ref,
                    FullLifecycleHookSuppressionReason::ProcessExit,
                    now,
                );
            }
            let cleared_hook_source = self.hook_authority.as_ref().and_then(|authority| {
                (crate::detect::parse_agent_label(&authority.agent_label) == agent
                    && !newer_custom_authority)
                    .then(|| authority.source.clone())
            });
            if let Some(source) = cleared_hook_source {
                self.hook_report_sequences.remove(&source);
                self.replace_hook_authority(None);
            }
            if !newer_custom_authority
                && self
                    .persisted_agent_session
                    .as_ref()
                    .is_some_and(|session| {
                        crate::detect::parse_agent_label(&session.agent) == agent
                    })
            {
                self.persisted_agent_session = None;
            }
            if let Some(agent) = agent {
                let agent_label = crate::detect::agent_label(agent);
                let mut cleared_metadata_sources = Vec::new();
                self.agent_metadata.retain(|source, metadata| {
                    let official_metadata = crate::agent_resume::is_official_agent_source(
                        &metadata.source,
                        agent_label,
                    ) || metadata.applies_to_source.as_deref().is_some_and(
                        |applies_to| {
                            crate::agent_resume::is_official_agent_source(applies_to, agent_label)
                        },
                    );
                    let matches_agent =
                        metadata.agent_label.as_deref() == Some(agent_label) || official_metadata;
                    let clear = matches_agent && (official_metadata || metadata.reported_at <= now);
                    if clear {
                        cleared_metadata_sources.push(source.clone());
                    }
                    !clear
                });
                for source in cleared_metadata_sources {
                    self.metadata_report_sequences.remove(&source);
                    self.metadata_report_agents.remove(&source);
                    self.metadata_token_sequence_sources.remove(&source);
                }
                let mut exited_generation_sources = Vec::new();
                self.metadata_report_agents.retain(|source, owner| {
                    if *owner == agent {
                        exited_generation_sources.push(source.clone());
                        false
                    } else {
                        true
                    }
                });
                for source in exited_generation_sources {
                    self.metadata_report_sequences.remove(&source);
                    self.metadata_token_sequence_sources.remove(&source);
                }
            }
        }
        if self.hook_authority_not_newer_than(now)
            && (self.hook_authority_conflicts_with_detected_agent(agent)
                || (previous_detected_agent.is_some()
                    && agent != previous_detected_agent
                    && self.hook_authority.as_ref().is_some_and(|authority| {
                        crate::detect::parse_agent_label(&authority.agent_label)
                            == previous_detected_agent
                    })))
        {
            let durable_session = self.hook_authority.as_ref().and_then(|authority| {
                authority.session_ref.as_ref().map(|session_ref| {
                    crate::agent_resume::PersistedAgentSession {
                        source: authority.source.clone(),
                        agent: authority.agent_label.clone(),
                        session_ref: session_ref.clone(),
                    }
                })
            });
            self.suppress_current_full_lifecycle_hook_authority(
                FullLifecycleHookSuppressionReason::HookClear,
            );
            self.replace_hook_authority(None);
            self.persisted_agent_session = durable_session;
        }
        if agent_released {
            self.clear_agent_name();
        }
        let current_session = self.current_session_identity_for_persistence();
        let session_ref_changed = previous_session != current_session;
        let hook_work_context_changed =
            if (process_exited && !newer_custom_authority) || previous_session != current_session {
                self.clear_hook_work_context()
            } else {
                false
            };
        TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed,
            session_replaced: previous_session.is_some() && session_ref_changed,
            hook_work_context_changed,
            agent_released,
            sidebar_projection_changed,
        }
    }

    fn reconcile_new_turn_or_stale_screen_resolution(
        &mut self,
        fallback_state: AgentState,
        visible_working: bool,
        previous_screen_settled_after_report: bool,
        previous_native_screen_settled: bool,
        now: Instant,
    ) -> bool {
        let unanswered_gate_latched = self
            .closing_report
            .as_ref()
            .is_some_and(|report| report.unanswered.is_some());
        let closing_report_is_older = self.hook_authority.as_ref().is_some_and(|authority| {
            authority.reported_at < now
                && self.hook_authority_is_effective(authority)
                && crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
        });
        let current_session = self.current_session_identity_for_persistence();
        // A closing-block source has no session identity of its own. Keep its
        // worker claim only when the agent's integration anchors it to the
        // current session; unanchored legacy evidence is retired whole.
        let closing_report_matches_current_session = self
            .hook_authority
            .as_ref()
            .zip(current_session.as_ref())
            .is_some_and(
                |(authority, (_, session_agent, session_kind, session_value))| {
                    authority.agent_label == *session_agent
                        && self
                            .closing_report
                            .as_ref()
                            .and_then(|report| report.scope.session_id.as_deref())
                            .is_none_or(|report_session_id| {
                                *session_kind == crate::agent_resume::AgentSessionRefKind::Id
                                    && report_session_id == session_value
                            })
                },
            );
        let starts_reported_turn = !unanswered_gate_latched
            && visible_working
            && closing_report_is_older
            && previous_screen_settled_after_report
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority
                    .reported_at
                    .checked_add(crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH)
                    .is_some_and(|stable_at| now >= stable_at)
            });
        let starts_turn = !unanswered_gate_latched
            && (starts_reported_turn || (visible_working && previous_native_screen_settled));
        let resolves_stale = !unanswered_gate_latched
            && self.supervisor_stale
            && matches!(fallback_state, AgentState::Idle | AgentState::Blocked)
            && self
                .hook_authority
                .as_ref()
                .is_none_or(|authority| authority.reported_at < now);
        if !starts_turn && !resolves_stale {
            return false;
        }

        if let Some(authority) = self.hook_authority.as_mut() {
            authority.retired_at = Some(now);
        }
        self.supervisor_stale = false;
        self.stale_resolution = None;
        self.unreported_turn_started_at = starts_turn.then_some(now);
        if starts_turn {
            self.agent_turn_generation = self.agent_turn_generation.wrapping_add(1);
            self.clear_claude_subagent_transcript_activity();
        }
        if closing_report_is_older {
            let active_subagents = closing_report_matches_current_session
                .then(|| self.current_direct_closing_report_subagents())
                .flatten();
            self.clear_closing_task_report(now);
            if let Some(active_subagents) = active_subagents {
                self.closing_report
                    .get_or_insert_default()
                    .closing_report_subagents = Some(active_subagents);
            }
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn replace_hook_authority(&mut self, authority: Option<HookAuthority>) {
        self.hook_authority = authority;
        self.supervisor_stale = false;
        self.stale_resolution = None;
        self.unreported_turn_started_at = None;
    }

    #[cfg(test)]
    pub fn set_hook_authority(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.set_hook_authority_at(
            source,
            agent_label,
            state,
            message,
            None,
            seq,
            Instant::now(),
        )
        .and_then(|mutation| mutation.effective_state_change)
    }

    #[cfg(test)]
    pub fn set_hook_authority_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.set_hook_authority_at(
            source,
            agent_label,
            state,
            message,
            session_ref,
            seq,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub fn set_hook_authority_at(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        self.set_hook_authority_report_at(
            source,
            agent_label,
            state,
            message,
            None,
            None,
            None,
            session_ref,
            seq,
            now,
        )
    }

    pub fn set_hook_authority_report_at(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        wait: Option<String>,
        eta_s: Option<u64>,
        reported_at_wire: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        if crate::detect::session_identity_only_integration(&source, &agent_label) {
            return None;
        }
        if !crate::detect::full_lifecycle_hook_authority(&source, &agent_label)
            && self.recent_agent_process_exit.is_some_and(|exit| {
                crate::detect::parse_agent_label(&agent_label) == Some(exit.agent)
            })
        {
            return None;
        }
        let reanchor_sequence = match self.route_full_lifecycle_hook_report(
            &source,
            &agent_label,
            state,
            message.as_deref(),
            &session_ref,
            seq,
            now,
        ) {
            FullLifecycleHookReportRoute::Accept { reanchor_sequence } => reanchor_sequence,
            FullLifecycleHookReportRoute::Ignore => return None,
        };
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }
        // A closing-block source claims no resume session, so it cannot contend
        // for the pane's session identity — that stays owned by the agent's own
        // integration (`herdr:claude`). Without this exemption the status report
        // is rejected here on every pane where the real agent has already
        // announced itself, which is every pane that matters.
        let owner_conflicts = !crate::detect::is_closing_block_source(&source, &agent_label)
            && self.current_session_owner_conflicts(&source, &agent_label);
        let foreground_takeover_allowed = owner_conflicts
            && self.foreground_agent_confirms_hook_authority_takeover(
                &source,
                &agent_label,
                &session_ref,
            );
        if owner_conflicts && !foreground_takeover_allowed {
            return None;
        }
        let session_ref = session_ref.map(|session_ref| {
            if self.lifecycle_hook_report_replaces_persisted_session(
                &source,
                &agent_label,
                &session_ref,
            ) {
                session_ref
            } else {
                self.conflicting_same_owner_session_ref(&source, &agent_label, &session_ref, None)
                    .unwrap_or(session_ref)
            }
        });
        if self.live_full_lifecycle_hook_authority_conflicts_with_session(
            &source,
            &agent_label,
            &session_ref,
        ) {
            return None;
        }
        if reanchor_sequence {
            self.hook_report_sequences.remove(&source);
        }
        if !self.accept_hook_report(&source, seq) {
            return None;
        }
        let starts_hook_turn = state == AgentState::Working
            && self.unreported_turn_started_at.is_none()
            && self.hook_authority.as_ref().is_none_or(|authority| {
                authority.retired_at.is_some()
                    || authority.state != AgentState::Working
                    || authority.source != source
                    || authority.agent_label != agent_label
                    || (session_ref.is_some() && authority.session_ref != session_ref)
            });
        let closing_report = crate::detect::is_closing_block_source(&source, &agent_label);
        let incoming_agent = crate::detect::parse_agent_label(&agent_label);
        let latch_agent = self
            .closing_report
            .as_ref()
            .and_then(|report| report.unanswered.as_ref())
            .and_then(|unanswered| unanswered.agent_label.as_deref())
            .and_then(crate::detect::parse_agent_label);
        if incoming_agent
            .zip(latch_agent)
            .is_some_and(|(incoming, latched)| incoming != latched)
        {
            self.clear_unanswered_closing_blockers();
        }
        if closing_report && state == AgentState::Blocked {
            let report = self.closing_report.get_or_insert_default();
            let latch_created = report.unanswered.is_none();
            let unanswered = report
                .unanswered
                .get_or_insert_with(|| UnansweredClosingBlockers {
                    agent_label: Some(agent_label.clone()),
                    ..UnansweredClosingBlockers::default()
                });
            let label_changed = unanswered.agent_label.as_deref() != Some(agent_label.as_str());
            if label_changed {
                unanswered.agent_label = Some(agent_label.clone());
            }
            if latch_created || label_changed {
                self.revision = self.revision.saturating_add(1);
            }
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        let hold_owner =
            self.blocked_hold_owner_for_hook(&source, &agent_label, session_ref.as_ref());
        self.reconcile_blocked_state_hold_owner(&hold_owner, now);
        if state == AgentState::Working || (closing_report && !self.has_pending_human_input()) {
            self.release_blocked_state_hold_for(&hold_owner, now);
        } else if state == AgentState::Blocked
            && (crate::detect::full_lifecycle_hook_authority(&source, &agent_label)
                || closing_report)
        {
            // Only persistent lifecycle and closing evidence outlive the next
            // transient state report; custom hook states remain live reports.
            self.hold_blocked_state_for(hold_owner, now);
        }
        self.reconcile_agent_name_owner(&agent_label, session_ref.as_ref());
        if foreground_takeover_allowed {
            self.suppress_current_full_lifecycle_hook_authority(
                FullLifecycleHookSuppressionReason::HookClear,
            );
        }
        if session_ref.is_some() || reanchor_sequence {
            if let Some(suppressed) = self.suppressed_full_lifecycle_hook_reports.remove(&source) {
                if let Some(suppressed_ref) = suppressed.session_ref {
                    self.remember_stale_full_lifecycle_hook_session(
                        source.clone(),
                        suppressed.agent_label,
                        suppressed_ref,
                    );
                }
            }
        }
        if !crate::detect::is_closing_block_source(&source, &agent_label) {
            self.persisted_agent_session = None;
        }
        let (wait, eta_s) = normalize_declared_wait(state, wait, eta_s);
        if closing_report {
            // The preceding screen sample belongs to the turn that just ended.
            // A forced post-report rescan establishes the baseline from which
            // a later visible Working signal can prove a new turn.
            self.fallback_observed_at = None;
        }
        if starts_hook_turn {
            self.agent_turn_generation = self.agent_turn_generation.wrapping_add(1);
            self.clear_claude_subagent_transcript_activity();
        }
        self.replace_hook_authority(Some(HookAuthority {
            source,
            agent_label,
            state,
            message,
            reported_at: now,
            wait,
            eta_s,
            reported_at_wire: reported_at_wire.filter(|value| !value.trim().is_empty()),
            session_ref,
            retired_at: None,
        }));
        let current_session = self.current_session_identity_for_persistence();
        let session_ref_changed = previous_session != current_session;
        let session_replaced = previous_session.is_some() && session_ref_changed;
        let hook_work_context_changed = if previous_session != current_session {
            self.clear_hook_work_context()
        } else {
            false
        };
        let closing_report_retired = self.retire_closing_report_outside_current_session(
            session_ref_changed,
            session_replaced,
            now,
        );
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed,
            session_replaced,
            hook_work_context_changed,
            agent_released: false,
            sidebar_projection_changed: closing_report_retired,
        })
    }

    pub fn status_report_snapshot(&self) -> (Option<String>, Option<u64>, Option<String>, bool) {
        let Some(authority) = self.hook_authority.as_ref() else {
            return (None, None, None, self.supervisor_stale);
        };
        (
            authority.wait.clone(),
            authority.eta_s,
            authority.reported_at_wire.clone(),
            self.supervisor_stale,
        )
    }

    pub fn status_reported_at(&self) -> Option<Instant> {
        self.unreported_turn_started_at.or_else(|| {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.reported_at)
        })
    }

    pub(crate) fn agent_turn_generation(&self) -> u64 {
        self.agent_turn_generation
    }

    /// True while the pane only reads as working because sub-process evidence
    /// outranks the agent's own report.
    ///
    /// The agent said it was finished, so nothing further is coming from it, but
    /// work it started is still running under the pane. Nobody is reporting on
    /// that work, which is exactly the silence the watchdog exists to notice.
    fn subprocess_held_working(&self) -> bool {
        self.state == AgentState::Working
            && self.foreground_process_active
            && self
                .hook_authority
                .as_ref()
                .is_some_and(|authority| authority.state != AgentState::Working)
    }

    /// True while the pane claims subagents are still running.
    ///
    /// The claim comes either from the live transcript scan or from the agent's
    /// own `closing_agents` token. Neither source expires on its own: the token is
    /// written without a TTL, and transcript rows simply stop arriving when the
    /// parent stalls. So a parent that said "3 agents running" keeps reading busy
    /// forever unless the watchdog ages the claim out.
    fn current_direct_closing_report_subagents(&self) -> Option<u32> {
        self.closing_report
            .as_ref()
            .and_then(|report| report.closing_report_subagents)
    }

    pub(crate) fn verified_active_subagents(&self) -> Option<u32> {
        self.active_subagents
            .or_else(|| self.current_direct_closing_report_subagents())
    }

    fn current_closing_report_subagents(&self) -> Option<u32> {
        self.active_subagents
            .or_else(|| self.current_direct_closing_report_subagents())
    }

    pub(crate) fn effective_active_subagents(&self) -> Option<u32> {
        self.current_closing_report_subagents()
    }

    pub(crate) fn declares_running_subagents(&self) -> bool {
        self.effective_active_subagents()
            .is_some_and(|count| count > 0)
    }

    pub(crate) fn waiting_on_agents(&self) -> bool {
        !self.supervisor_stale
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority.state != AgentState::Blocked
                    && self.hook_authority_is_effective(authority)
                    && crate::detect::is_closing_block_source(
                        &authority.source,
                        &authority.agent_label,
                    )
            })
            && self
                .current_closing_report_subagents()
                .is_some_and(|count| count > 0)
    }

    fn finished_closing_report(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            authority.retired_at.is_none()
                && authority.state == AgentState::Idle
                && self.hook_authority_is_effective(authority)
                && crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
                && self.current_closing_report_subagents().unwrap_or_default() == 0
        })
    }

    #[cfg(test)]
    pub fn agent_status_watchdog_deadline(&self, stale_after: Duration) -> Option<Instant> {
        self.agent_status_watchdog_deadline_with_activity(
            stale_after,
            DEFAULT_SUBAGENT_STALE_SILENCE,
            None,
        )
    }

    pub fn agent_status_watchdog_deadline_with_activity(
        &self,
        stale_after: Duration,
        subagent_stale_after: Duration,
        pane_activity_at: Option<Instant>,
    ) -> Option<Instant> {
        if self.supervisor_stale
            || self.state == AgentState::Blocked
            || self.closing_external_wait().is_some()
        {
            return None;
        }
        if self.active_subagents.is_some_and(|count| count > 0)
            && self.claude_subagent_transcript_activity == SubagentTranscriptActivity::Fresh
        {
            let observed_at = self.claude_subagent_activity_observed_at?;
            let quiet_since = pane_activity_at
                .filter(|activity_at| *activity_at > observed_at)
                .unwrap_or(observed_at);
            return quiet_since.checked_add(subagent_stale_after);
        }
        if self.waiting_on_agents() {
            let authority = self.hook_authority.as_ref()?;
            // Retirement ends the hook's lifecycle authority, not its direct
            // claim that child work remains. Give that claim a fresh silence
            // budget from the later lifecycle boundary.
            let claim_started_at = authority
                .retired_at
                .map(|retired_at| retired_at.max(authority.reported_at))
                .unwrap_or(authority.reported_at);
            let mut quiet_since = pane_activity_at
                .filter(|activity_at| *activity_at > claim_started_at)
                .unwrap_or(claim_started_at);
            if self.claude_subagent_transcript_activity == SubagentTranscriptActivity::Fresh {
                if let Some(observed_at) = self
                    .claude_subagent_activity_observed_at
                    .filter(|observed_at| *observed_at > quiet_since)
                {
                    quiet_since = observed_at;
                }
            }
            let age =
                if self.claude_subagent_transcript_activity == SubagentTranscriptActivity::Stale {
                    stale_after
                } else {
                    subagent_stale_after
                };
            return quiet_since.checked_add(age);
        }
        if let Some(turn_started_at) = self.unreported_turn_started_at {
            return turn_started_at.checked_add(AGENT_BUSY_STALE_SILENCE);
        }
        let authority = self.hook_authority.as_ref().filter(|authority| {
            authority.retired_at.is_none() && self.hook_authority_is_effective(authority)
        })?;
        let age = match authority.state {
            AgentState::Working => authority
                .wait
                .as_ref()
                .and(authority.eta_s)
                .map(|eta_s| Duration::from_secs(eta_s).saturating_add(DECLARED_WAIT_GRACE))
                .unwrap_or(AGENT_BUSY_STALE_SILENCE),
            // A declared wait belongs to a working report; a finished report that
            // is still holding sub-processes gets the plain silence budget.
            _ if self.subprocess_held_working() => stale_after,
            _ => return None,
        };
        authority.reported_at.checked_add(age)
    }

    #[cfg(test)]
    pub fn mark_agent_status_stale_at(
        &mut self,
        now: Instant,
        stale_after: Duration,
    ) -> Option<TerminalStateMutation> {
        self.mark_agent_status_stale_at_with_activity(
            now,
            stale_after,
            DEFAULT_SUBAGENT_STALE_SILENCE,
            None,
        )
    }

    pub fn mark_agent_status_stale_at_with_activity(
        &mut self,
        now: Instant,
        stale_after: Duration,
        subagent_stale_after: Duration,
        pane_activity_at: Option<Instant>,
    ) -> Option<TerminalStateMutation> {
        if self.supervisor_stale
            || self
                .agent_status_watchdog_deadline_with_activity(
                    stale_after,
                    subagent_stale_after,
                    pane_activity_at,
                )
                .is_none_or(|deadline| now < deadline)
        {
            return None;
        }
        let expired_subagent_wait = self.waiting_on_agents();
        let expired_retired_direct_claim = expired_subagent_wait
            && self.active_subagents.is_none()
            && self
                .current_direct_closing_report_subagents()
                .is_some_and(|count| count > 0)
            && self
                .hook_authority
                .as_ref()
                .is_some_and(|authority| authority.retired_at.is_some());
        if expired_retired_direct_claim {
            // No live supervisor remains to reconcile this claim. Expiry is
            // therefore its terminal zero report, which lets the pane settle.
            if self.fallback_state != AgentState::Working {
                self.unreported_turn_started_at = None;
            }
            return self.apply_closing_report_subagents_at(Some(0), now);
        }
        self.supervisor_stale = true;
        if expired_subagent_wait {
            self.closing_report
                .get_or_insert_default()
                .closing_report_subagents = Some(0);
            self.revision = self.revision.wrapping_add(1);
        }
        Some(TerminalStateMutation {
            sidebar_projection_changed: expired_subagent_wait,
            ..TerminalStateMutation::default()
        })
    }

    fn hook_authority_not_newer_than(&self, observed_at: Instant) -> bool {
        self.hook_authority
            .as_ref()
            .is_none_or(|authority| authority.reported_at <= observed_at)
    }

    fn hook_authority_conflicts_with_detected_agent(&self, detected_agent: Option<Agent>) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::parse_agent_label(&authority.agent_label)
                .is_some_and(|hook_agent| hook_agent != detected_agent)
        })
    }

    fn should_ignore_detected_state_under_full_lifecycle_hook(
        &self,
        detected_agent: Option<Agent>,
        process_exited: bool,
    ) -> bool {
        self.live_full_lifecycle_hook_authority()
            && !process_exited
            && !self.hook_authority_conflicts_with_detected_agent(detected_agent)
    }

    fn persisted_agent_session_matches(&self, source: &str, agent: &str) -> bool {
        self.persisted_agent_session
            .as_ref()
            .is_some_and(|session| session.source == source && session.agent == agent)
    }

    fn suppress_current_full_lifecycle_hook_authority(
        &mut self,
        reason: FullLifecycleHookSuppressionReason,
    ) {
        if let Some((source, agent_label, session_ref)) =
            self.hook_authority.as_ref().and_then(|authority| {
                crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                )
                .then(|| {
                    (
                        authority.source.clone(),
                        authority.agent_label.clone(),
                        authority.session_ref.clone(),
                    )
                })
            })
        {
            self.suppress_full_lifecycle_hook_report_with_session_ref(
                source,
                agent_label,
                session_ref,
                reason,
                Instant::now(),
            );
        }
    }

    fn suppress_full_lifecycle_hook_report(
        &mut self,
        source: &str,
        agent_label: &str,
        reason: FullLifecycleHookSuppressionReason,
    ) {
        if crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            let session_ref = self
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.clone());
            self.suppress_full_lifecycle_hook_report_with_session_ref(
                source.to_string(),
                agent_label.to_string(),
                session_ref,
                reason,
                Instant::now(),
            );
        }
    }

    fn suppress_full_lifecycle_hook_report_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        reason: FullLifecycleHookSuppressionReason,
        observed_at: Instant,
    ) {
        self.suppressed_full_lifecycle_hook_reports.insert(
            source,
            SuppressedFullLifecycleHookReport {
                agent_label,
                session_ref,
                observed_at,
                reason,
                replacement_session_ref: None,
                pending_replacement_report: None,
            },
        );
    }

    fn route_full_lifecycle_hook_report(
        &mut self,
        source: &str,
        agent_label: &str,
        state: AgentState,
        message: Option<&str>,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        reported_at: Instant,
    ) -> FullLifecycleHookReportRoute {
        if !crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            return FullLifecycleHookReportRoute::Accept {
                reanchor_sequence: false,
            };
        }
        if self.full_lifecycle_hook_report_matches_stale_session(source, agent_label, session_ref) {
            return FullLifecycleHookReportRoute::Ignore;
        }

        let known_agent = crate::detect::parse_agent_label(agent_label);
        let process_present = known_agent.is_some()
            && self.detected_agent == known_agent
            && self.recent_agent_process_exit.is_none();
        let anchored_session_ref = self
            .hook_authority
            .as_ref()
            .filter(|authority| authority.source == source && authority.agent_label == agent_label)
            .and_then(|authority| authority.session_ref.as_ref())
            .or_else(|| {
                self.persisted_agent_session
                    .as_ref()
                    .filter(|session| session.source == source && session.agent == agent_label)
                    .map(|session| &session.session_ref)
            });
        let session_anchored = anchored_session_ref.is_some_and(|anchored| {
            session_ref
                .as_ref()
                .is_none_or(|incoming| incoming == anchored)
        });
        let opencode_cross_talk = (source, agent_label) == ("herdr:opencode", "opencode")
            && process_present
            && anchored_session_ref
                .zip(session_ref.as_ref())
                .is_some_and(|(anchored, incoming)| anchored != incoming);
        if opencode_cross_talk {
            return FullLifecycleHookReportRoute::Ignore;
        }
        // A closing-block source reports what the agent just said, not where to
        // resume it, so it never carries an `AgentSessionRef`
        // (`agent_resume::session_ref_from_report` mints one only for official
        // sources). Every path below admits a report by anchoring it to a
        // session, so without this arm the report is unreachable: it falls to
        // the `session_ref` bail-out and is stored as a pending replacement that
        // nothing ever drains while the agent stays continuously detected.
        //
        // Presence of the agent's own process in the pane is the entire guard
        // this source claims (see `detect::is_closing_block_source`), and
        // `accept_hook_report` still enforces per-source sequence monotonicity
        // downstream, so accepting on presence alone loses no protection.
        // Screen detection falling silent is not evidence that the agent left:
        // at turn end the prompt box frequently matches no rule at all, and the
        // scan is skipped outright while this very authority is live. Only a
        // *different* detected agent, or an observed process exit, contradicts
        // the hook. Suppressed sources keep the stricter presence guard.
        let process_not_contradicted = known_agent.is_some()
            && self.recent_agent_process_exit.is_none()
            && self
                .detected_agent
                .is_none_or(|detected_agent| Some(detected_agent) == known_agent);
        if crate::detect::is_closing_block_source(source, agent_label)
            && (process_present
                || (process_not_contradicted
                    && !self
                        .suppressed_full_lifecycle_hook_reports
                        .contains_key(source)))
        {
            return FullLifecycleHookReportRoute::Accept {
                reanchor_sequence: false,
            };
        }
        if let Some(suppressed) = self.suppressed_full_lifecycle_hook_reports.get(source) {
            if suppressed.agent_label != agent_label {
                return FullLifecycleHookReportRoute::Ignore;
            }
            if suppressed.reason == FullLifecycleHookSuppressionReason::HookClear {
                let reanchor_sequence = matches!(
                    (&suppressed.session_ref, session_ref),
                    (Some(previous), Some(incoming)) if previous != incoming
                );
                return if reanchor_sequence {
                    FullLifecycleHookReportRoute::Accept {
                        reanchor_sequence: true,
                    }
                } else {
                    FullLifecycleHookReportRoute::Ignore
                };
            }
        }

        if process_present
            && session_anchored
            && !self
                .suppressed_full_lifecycle_hook_reports
                .contains_key(source)
        {
            return FullLifecycleHookReportRoute::Accept {
                reanchor_sequence: self
                    .full_lifecycle_hook_report_has_fresh_session_after_stale_session(
                        source,
                        agent_label,
                        session_ref,
                    ),
            };
        }

        let Some(session_ref) = session_ref.clone() else {
            return FullLifecycleHookReportRoute::Ignore;
        };
        let Some(seq) = seq else {
            return FullLifecycleHookReportRoute::Ignore;
        };
        if self
            .hook_report_sequences
            .get(source)
            .is_some_and(|previous| seq <= *previous)
        {
            return FullLifecycleHookReportRoute::Ignore;
        }

        let previous_session_ref = self
            .persisted_agent_session
            .as_ref()
            .filter(|session| session.source == source && session.agent == agent_label)
            .map(|session| session.session_ref.clone());
        let suppressed = self
            .suppressed_full_lifecycle_hook_reports
            .entry(source.to_string())
            .or_insert_with(|| SuppressedFullLifecycleHookReport {
                agent_label: agent_label.to_string(),
                session_ref: previous_session_ref,
                observed_at: reported_at,
                reason: FullLifecycleHookSuppressionReason::ProcessExit,
                replacement_session_ref: None,
                pending_replacement_report: None,
            });
        let replace_pending = suppressed
            .pending_replacement_report
            .as_ref()
            .is_none_or(|pending| seq > pending.seq);
        if replace_pending {
            suppressed.pending_replacement_report = Some(PendingFullLifecycleHookReport {
                authority: HookAuthority {
                    source: source.to_string(),
                    agent_label: agent_label.to_string(),
                    state,
                    message: message.map(str::to_string),
                    reported_at,
                    wait: None,
                    eta_s: None,
                    reported_at_wire: None,
                    session_ref: Some(session_ref),
                    retired_at: None,
                },
                seq,
            });
        }
        FullLifecycleHookReportRoute::Ignore
    }

    fn full_lifecycle_hook_report_matches_stale_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            return false;
        }
        self.stale_full_lifecycle_hook_sessions
            .get(source)
            .is_some_and(|stale_sessions| {
                session_ref.as_ref().is_some_and(|incoming_ref| {
                    stale_sessions.iter().any(|stale| {
                        stale.agent_label == agent_label && incoming_ref == &stale.session_ref
                    })
                })
            })
    }

    fn full_lifecycle_hook_report_has_fresh_session_after_stale_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            return false;
        }
        self.stale_full_lifecycle_hook_sessions
            .get(source)
            .is_some_and(|stale_sessions| {
                stale_sessions
                    .iter()
                    .any(|stale| stale.agent_label == agent_label)
                    && session_ref.as_ref().is_some_and(|incoming_ref| {
                        stale_sessions.iter().all(|stale| {
                            stale.agent_label != agent_label || incoming_ref != &stale.session_ref
                        })
                    })
            })
    }

    fn live_full_lifecycle_hook_authority_conflicts_with_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        let Some(authority) = self.hook_authority.as_ref() else {
            return false;
        };
        if !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
        {
            return false;
        }
        if authority.source != source || authority.agent_label != agent_label {
            return false;
        }
        authority
            .session_ref
            .as_ref()
            .zip(session_ref.as_ref())
            .is_some_and(|(current, incoming)| current != incoming)
    }

    fn same_owner_full_lifecycle_hook_authority_session_ref(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> Option<crate::agent_resume::AgentSessionRef> {
        let authority = self.hook_authority.as_ref()?;
        if !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
            || authority.source != source
            || authority.agent_label != agent_label
        {
            return None;
        }
        authority
            .session_ref
            .as_ref()
            .filter(|current| *current != session_ref)
            .cloned()
    }

    fn clear_full_lifecycle_hook_suppression_for_detected_agent(
        &mut self,
        previous_detected_agent: Option<Agent>,
        detected_agent: Option<Agent>,
    ) {
        let Some(detected_agent) = detected_agent else {
            return;
        };
        if previous_detected_agent == Some(detected_agent) {
            return;
        }
        let detected_label = crate::detect::agent_label(detected_agent);
        let mut stale_sessions = Vec::new();
        let mut validated_replacement_sessions = Vec::new();
        self.suppressed_full_lifecycle_hook_reports
            .retain(|source, suppressed| {
                let should_clear = crate::detect::parse_agent_label(&suppressed.agent_label)
                    == Some(detected_agent);
                if !should_clear {
                    return true;
                }
                if suppressed.reason == FullLifecycleHookSuppressionReason::ProcessExit {
                    if let Some(session_ref) = suppressed.replacement_session_ref.take() {
                        if let Some(exited_session_ref) = suppressed
                            .session_ref
                            .as_ref()
                            .filter(|exited_session_ref| *exited_session_ref != &session_ref)
                            .cloned()
                        {
                            stale_sessions.push((
                                source.clone(),
                                StaleFullLifecycleHookSession {
                                    agent_label: suppressed.agent_label.clone(),
                                    session_ref: exited_session_ref,
                                },
                            ));
                        }
                        let session_start_seq = self.hook_report_sequences.get(source).copied();
                        let pending =
                            suppressed
                                .pending_replacement_report
                                .take()
                                .filter(|pending| {
                                    pending.authority.session_ref.as_ref() == Some(&session_ref)
                                        && session_start_seq.is_none_or(|seq| pending.seq > seq)
                                });
                        validated_replacement_sessions.push((
                            source.clone(),
                            suppressed.agent_label.clone(),
                            session_ref,
                            pending,
                        ));
                        return false;
                    }
                    return true;
                }
                if let Some(session_ref) = suppressed.session_ref.clone() {
                    stale_sessions.push((
                        source.clone(),
                        StaleFullLifecycleHookSession {
                            agent_label: suppressed.agent_label.clone(),
                            session_ref,
                        },
                    ));
                }
                false
            });
        for (source, stale_session) in stale_sessions {
            self.remember_stale_full_lifecycle_hook_session(
                source,
                stale_session.agent_label,
                stale_session.session_ref,
            );
        }
        self.hook_report_sequences.retain(|source, _| {
            validated_replacement_sessions
                .iter()
                .any(|(validated_source, _, _, _)| validated_source == source)
                || !crate::detect::full_lifecycle_hook_authority(source, detected_label)
        });
        for (source, agent_label, session_ref, pending) in validated_replacement_sessions {
            self.forget_stale_full_lifecycle_hook_session(&source, &agent_label, &session_ref);
            self.reconcile_agent_name_owner(&agent_label, Some(&session_ref));
            self.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
                source: source.clone(),
                agent: agent_label,
                session_ref,
            });
            if let Some(pending) = pending {
                self.hook_report_sequences.insert(source, pending.seq);
                self.replace_hook_authority(Some(pending.authority));
            }
        }
    }

    fn remember_stale_full_lifecycle_hook_session(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: crate::agent_resume::AgentSessionRef,
    ) {
        let stale_session = StaleFullLifecycleHookSession {
            agent_label,
            session_ref,
        };
        let source_stale_sessions = self
            .stale_full_lifecycle_hook_sessions
            .entry(source)
            .or_default();
        if !source_stale_sessions
            .iter()
            .any(|existing| existing == &stale_session)
        {
            source_stale_sessions.push(stale_session);
        }
    }

    fn forget_stale_full_lifecycle_hook_session(
        &mut self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) {
        let remove_source = self
            .stale_full_lifecycle_hook_sessions
            .get_mut(source)
            .is_some_and(|stale_sessions| {
                stale_sessions.retain(|stale| {
                    stale.agent_label != agent_label || &stale.session_ref != session_ref
                });
                stale_sessions.is_empty()
            });
        if remove_source {
            self.stale_full_lifecycle_hook_sessions.remove(source);
        }
    }

    fn detected_state_observed_before_release_suppression(
        &self,
        detected_agent: Option<Agent>,
        observed_at: Instant,
    ) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.suppressed_full_lifecycle_hook_reports
            .values()
            .any(|suppressed| {
                crate::detect::parse_agent_label(&suppressed.agent_label) == Some(detected_agent)
                    && observed_at <= suppressed.observed_at
            })
    }

    fn current_session_identity_for_persistence(
        &self,
    ) -> Option<(
        String,
        String,
        crate::agent_resume::AgentSessionRefKind,
        String,
    )> {
        if let Some(authority) = self.hook_authority.as_ref() {
            if let Some(session_ref) = authority.session_ref.as_ref() {
                return Some((
                    authority.source.clone(),
                    authority.agent_label.clone(),
                    session_ref.kind,
                    session_ref.value.clone(),
                ));
            }
        }
        self.persisted_agent_session.as_ref().map(|session| {
            (
                session.source.clone(),
                session.agent.clone(),
                session.session_ref.kind,
                session.session_ref.value.clone(),
            )
        })
    }

    pub(crate) fn current_agent_session_id(&self) -> Option<&str> {
        self.hook_authority
            .as_ref()
            .and_then(|authority| authority.session_ref.as_ref())
            .filter(|session_ref| session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id)
            .map(|session_ref| session_ref.value.as_str())
            .or_else(|| {
                self.persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref)
                    .filter(|session_ref| {
                        session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                    })
                    .map(|session_ref| session_ref.value.as_str())
            })
    }

    fn current_agent_activity_owner(&self) -> Option<AgentActivityOwner> {
        self.current_session_identity_for_persistence()
            .map(
                |(source, agent_label, kind, value)| AgentActivityOwner::Session {
                    source,
                    agent_label,
                    kind,
                    value,
                },
            )
            .or_else(|| {
                self.effective_agent_label()
                    .map(|label| AgentActivityOwner::Agent {
                        agent_label: label.to_string(),
                        previous_session: None,
                    })
            })
    }

    fn blocked_hold_owner_for_agent(&self, agent: Agent) -> AgentActivityOwner {
        let agent_label = crate::detect::agent_label(agent).to_string();
        let session = self
            .current_session_identity_for_persistence()
            .filter(|(_, label, _, _)| label.as_str() == agent_label.as_str())
            .map(
                |(source, agent_label, kind, value)| AgentActivityOwner::Session {
                    source,
                    agent_label,
                    kind,
                    value,
                },
            );
        session.unwrap_or_else(|| AgentActivityOwner::Agent {
            previous_session: self
                .agent_activity_owner
                .as_ref()
                .filter(|owner| owner.agent_label() == agent_label)
                .and_then(AgentActivityOwner::session),
            agent_label,
        })
    }

    fn blocked_hold_owner_for_hook(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: Option<&crate::agent_resume::AgentSessionRef>,
    ) -> AgentActivityOwner {
        if !crate::detect::is_closing_block_source(source, agent_label) {
            if let Some(session_ref) = session_ref {
                return AgentActivityOwner::Session {
                    source: source.to_string(),
                    agent_label: agent_label.to_string(),
                    kind: session_ref.kind,
                    value: session_ref.value.clone(),
                };
            }
        }
        self.current_session_identity_for_persistence()
            .filter(|(_, label, _, _)| label.as_str() == agent_label)
            .map(
                |(source, agent_label, kind, value)| AgentActivityOwner::Session {
                    source,
                    agent_label,
                    kind,
                    value,
                },
            )
            .unwrap_or_else(|| AgentActivityOwner::Agent {
                previous_session: self
                    .agent_activity_owner
                    .as_ref()
                    .filter(|owner| owner.agent_label() == agent_label)
                    .and_then(AgentActivityOwner::session),
                agent_label: agent_label.to_string(),
            })
    }

    pub(crate) fn agent_session_matches(
        &self,
        source: &str,
        agent_label: &str,
        session_id: &str,
    ) -> bool {
        self.current_session_identity_for_persistence().is_some_and(
            |(current_source, current_agent, current_kind, current_value)| {
                current_source == source
                    && current_agent == agent_label
                    && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                    && current_value == session_id
            },
        )
    }

    pub(crate) fn agent_session_id_matches_current_agent(
        &self,
        agent_label: &str,
        session_id: &str,
    ) -> bool {
        self.current_session_identity_for_persistence().is_some_and(
            |(_, current_agent, current_kind, current_value)| {
                current_agent == agent_label
                    && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                    && current_value == session_id
            },
        )
    }

    pub(crate) fn effective_non_retired_hook_authority(&self) -> Option<&HookAuthority> {
        self.hook_authority.as_ref().filter(|authority| {
            authority.retired_at.is_none() && self.hook_authority_is_effective(authority)
        })
    }

    pub(crate) fn effective_non_retired_hook_session_id(&self) -> Option<(&str, &str)> {
        let authority = self.effective_non_retired_hook_authority()?;
        let session_ref = authority.session_ref.as_ref().filter(|session_ref| {
            session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
        })?;
        Some((&authority.agent_label, session_ref.value.as_str()))
    }

    pub(crate) fn record_legacy_closing_report_precursor(
        &mut self,
        source: String,
        agent_label: String,
        session_id: Option<String>,
        precursor_seq: Option<u64>,
    ) {
        if !crate::detect::is_closing_block_source(&source, &agent_label) {
            return;
        }
        let Some((session_id, report_seq)) = session_id
            .zip(precursor_seq.and_then(|seq| seq.checked_add(1)))
            .filter(|(session_id, _)| {
                self.agent_session_id_matches_current_agent(&agent_label, session_id)
            })
        else {
            return;
        };
        self.closing_report
            .get_or_insert_default()
            .legacy_session_guard = Some(LegacyClosingReportGuard {
            source,
            agent_label,
            session_id,
            report_seq,
        });
    }

    pub(crate) fn correlate_legacy_closing_report(
        &self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Result<Option<String>, ()> {
        let Some(report) = self.closing_report.as_ref() else {
            return Ok(None);
        };
        if let Some(session_id) = report
            .legacy_session_guard
            .as_ref()
            .filter(|guard| {
                guard.source == source
                    && guard.agent_label == agent_label
                    && Some(guard.report_seq) == seq
                    && self.agent_session_id_matches_current_agent(agent_label, &guard.session_id)
            })
            .map(|guard| guard.session_id.clone())
        {
            return Ok(Some(session_id));
        }
        if report.requires_legacy_session_guard {
            Err(())
        } else {
            Ok(None)
        }
    }

    #[cfg(test)]
    pub(crate) fn legacy_closing_report_requires_guard(&self) -> bool {
        self.closing_report
            .as_ref()
            .is_some_and(|report| report.requires_legacy_session_guard)
    }

    pub(crate) fn accepts_legacy_closing_metadata(
        &self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> bool {
        let Some(report) = self.closing_report.as_ref() else {
            return true;
        };
        if !report.requires_legacy_session_guard {
            return true;
        }
        report.scope.source.as_deref() == Some(source)
            && report.scope.turn_seq == seq
            && report
                .scope
                .session_id
                .as_deref()
                .is_some_and(|session_id| {
                    self.agent_session_id_matches_current_agent(agent_label, session_id)
                })
    }

    fn current_session_owner_conflicts(&self, source: &str, agent_label: &str) -> bool {
        self.current_session_identity_for_persistence().is_some_and(
            |(current_source, current_agent, _, _)| {
                current_source != source || current_agent != agent_label
            },
        )
    }

    fn conflicting_same_owner_session_ref(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
        session_start_source: Option<&str>,
    ) -> Option<crate::agent_resume::AgentSessionRef> {
        self.current_session_identity_for_persistence().and_then(
            |(current_source, current_agent, current_kind, current_value)| {
                (current_source == source
                    && current_agent == agent_label
                    && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                    && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                    && current_value != session_ref.value
                    && !Self::session_report_allows_session_replacement(
                        source,
                        agent_label,
                        session_start_source,
                    ))
                .then_some(crate::agent_resume::AgentSessionRef {
                    kind: current_kind,
                    value: current_value,
                })
            },
        )
    }

    fn lifecycle_hook_report_replaces_persisted_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> bool {
        self.hook_authority.is_none()
            && (source, agent_label) == ("herdr:mastracode", "mastracode")
            && self
                .persisted_agent_session
                .as_ref()
                .is_some_and(|session| {
                    session.source == source
                        && session.agent == agent_label
                        && session.session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                        && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                        && session.session_ref.value != session_ref.value
                })
    }

    fn session_report_allows_session_replacement(
        source: &str,
        agent_label: &str,
        session_start_source: Option<&str>,
    ) -> bool {
        matches!(
            (source, agent_label, session_start_source),
            (
                "herdr:claude",
                "claude",
                Some("clear" | "resume" | "compact")
            ) | (
                "herdr:codex",
                "codex",
                Some("startup" | "clear" | "resume" | "compact")
            ) | ("herdr:mastracode", "mastracode", Some("startup"))
                | ("herdr:hermes", "hermes", Some("startup" | "new" | "resume"))
                | ("herdr:opencode", "opencode", Some("select"))
                | ("herdr:pi", "pi", Some("new" | "resume" | "fork"))
                | (
                    "herdr:omp",
                    "omp",
                    Some("startup" | "new" | "resume" | "fork")
                )
                | ("herdr:antigravity_cli", "agy", None)
        )
    }

    fn session_start_source_is_recognized(session_start_source: Option<&str>) -> bool {
        matches!(
            session_start_source,
            Some("startup" | "clear" | "resume" | "compact" | "new" | "fork" | "select")
        )
    }

    fn is_unsequenced_opencode_selection(
        source: &str,
        agent_label: &str,
        session_start_source: Option<&str>,
        seq: Option<u64>,
    ) -> bool {
        (source, agent_label, session_start_source, seq)
            == ("herdr:opencode", "opencode", Some("select"), None)
    }

    pub fn set_persisted_agent_session(
        &mut self,
        session: crate::agent_resume::PersistedAgentSession,
    ) {
        self.persisted_agent_session = Some(session);
    }

    pub fn set_agent_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.set_agent_session_ref_for_session_start(source, agent_label, session_ref, seq, None)
    }

    pub fn set_agent_session_ref_for_session_start(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        session_start_source: Option<String>,
    ) -> Option<TerminalStateMutation> {
        let session_ref = session_ref?;
        let known_agent = crate::detect::parse_agent_label(&agent_label);
        let process_present = known_agent.is_some()
            && self.detected_agent == known_agent
            && self.recent_agent_process_exit.is_none();
        let full_lifecycle_source =
            crate::detect::full_lifecycle_hook_authority(&source, &agent_label);
        let generation_gated = self
            .suppressed_full_lifecycle_hook_reports
            .get(&source)
            .is_some_and(|suppressed| {
                suppressed.agent_label == agent_label
                    && suppressed.reason != FullLifecycleHookSuppressionReason::HookClear
            });
        let session_anchored = self.hook_authority.as_ref().is_some_and(|authority| {
            authority.source == source
                && authority.agent_label == agent_label
                && authority.session_ref.is_some()
        }) || self.persisted_agent_session_matches(&source, &agent_label);
        let unsequenced_selection = Self::is_unsequenced_opencode_selection(
            &source,
            &agent_label,
            session_start_source.as_deref(),
            seq,
        );
        let selection_can_reconcile = unsequenced_selection && process_present;
        if selection_can_reconcile {
            self.suppressed_full_lifecycle_hook_reports.remove(&source);
        } else if full_lifecycle_source && unsequenced_selection {
            let previous_session_ref = self
                .hook_authority
                .as_ref()
                .filter(|authority| {
                    authority.source == source && authority.agent_label == agent_label
                })
                .and_then(|authority| authority.session_ref.clone())
                .or_else(|| {
                    self.persisted_agent_session
                        .as_ref()
                        .filter(|session| session.source == source && session.agent == agent_label)
                        .map(|session| session.session_ref.clone())
                });
            let suppressed = self
                .suppressed_full_lifecycle_hook_reports
                .entry(source)
                .or_insert_with(|| SuppressedFullLifecycleHookReport {
                    agent_label,
                    session_ref: previous_session_ref,
                    observed_at: Instant::now(),
                    reason: FullLifecycleHookSuppressionReason::ProcessExit,
                    replacement_session_ref: None,
                    pending_replacement_report: None,
                });
            suppressed.replacement_session_ref = Some(session_ref);
            suppressed.pending_replacement_report = None;
            return None;
        }
        if full_lifecycle_source
            && !selection_can_reconcile
            && (!process_present || generation_gated || !session_anchored)
        {
            if !Self::session_start_source_is_recognized(session_start_source.as_deref()) {
                return None;
            }
            let seq = seq?;
            if self
                .hook_report_sequences
                .get(&source)
                .is_some_and(|previous| seq <= *previous)
            {
                return None;
            }

            let previous_agent_label = self.effective_agent_label().map(str::to_string);
            let previous_known_agent = self.effective_known_agent();
            let previous_state = self.state;
            let now = Instant::now();
            let previous_presentation =
                self.effective_presentation_for_state_at(previous_state, now);
            let previous_session = self.current_session_identity_for_persistence();
            let suppressed = self
                .suppressed_full_lifecycle_hook_reports
                .entry(source.clone())
                .or_insert_with(|| SuppressedFullLifecycleHookReport {
                    agent_label: agent_label.clone(),
                    session_ref: None,
                    observed_at: now,
                    reason: FullLifecycleHookSuppressionReason::ProcessExit,
                    replacement_session_ref: None,
                    pending_replacement_report: None,
                });
            if suppressed.replacement_session_ref.as_ref() != Some(&session_ref) {
                if suppressed
                    .pending_replacement_report
                    .as_ref()
                    .is_some_and(|pending| {
                        pending.authority.session_ref.as_ref() != Some(&session_ref)
                    })
                {
                    suppressed.pending_replacement_report = None;
                }
                suppressed.replacement_session_ref = Some(session_ref);
            }
            self.hook_report_sequences.insert(source.clone(), seq);

            if process_present {
                self.clear_full_lifecycle_hook_suppression_for_detected_agent(None, known_agent);
                let current_session = self.current_session_identity_for_persistence();
                let session_ref_changed = previous_session != current_session;
                let session_replaced = previous_session.is_some() && session_ref_changed;
                if session_replaced {
                    self.release_blocked_hold_at(now);
                }
                let hook_work_context_changed = if session_ref_changed {
                    self.clear_hook_work_context()
                } else {
                    false
                };
                let closing_report_retired = self.retire_closing_report_outside_current_session(
                    session_ref_changed,
                    session_replaced,
                    now,
                );
                return Some(TerminalStateMutation {
                    effective_state_change: self.recompute_effective_state(
                        previous_agent_label,
                        previous_known_agent,
                        previous_state,
                        previous_presentation,
                        now,
                    ),
                    session_ref_changed,
                    session_replaced,
                    hook_work_context_changed,
                    agent_released: false,
                    sidebar_projection_changed: closing_report_retired,
                });
            }
            return None;
        }
        if !unsequenced_selection && !self.accept_hook_report(&source, seq) {
            return None;
        }
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }
        let session_replacement_allowed = Self::session_report_allows_session_replacement(
            &source,
            &agent_label,
            session_start_source.as_deref(),
        );
        let replacing_identity_only_session =
            crate::detect::session_identity_only_integration(&source, &agent_label)
                && session_replacement_allowed
                && self.current_session_identity_for_persistence().is_some_and(
                    |(current_source, current_agent, current_kind, current_value)| {
                        current_source == source
                            && current_agent == agent_label
                            && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                            && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                            && current_value != session_ref.value
                    },
                );
        if replacing_identity_only_session && !process_present {
            return None;
        }
        let owner_conflicts = self.current_session_owner_conflicts(&source, &agent_label);
        let foreground_takeover_allowed = owner_conflicts
            && self.foreground_agent_confirms_different_owner_takeover(
                &source,
                &agent_label,
                &session_ref,
                session_start_source.as_deref(),
            );
        if owner_conflicts && !foreground_takeover_allowed {
            return None;
        }
        if self
            .conflicting_same_owner_session_ref(
                &source,
                &agent_label,
                &session_ref,
                session_start_source.as_deref(),
            )
            .is_some()
        {
            return None;
        }
        let replaced_hook_session = self.same_owner_full_lifecycle_hook_authority_session_ref(
            &source,
            &agent_label,
            &session_ref,
        );
        if replaced_hook_session.is_some() && !session_replacement_allowed {
            return None;
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        if session_replacement_allowed || foreground_takeover_allowed {
            self.forget_stale_full_lifecycle_hook_session(&source, &agent_label, &session_ref);
        }
        if let Some(replaced_hook_session) = replaced_hook_session {
            self.remember_stale_full_lifecycle_hook_session(
                source.clone(),
                agent_label.clone(),
                replaced_hook_session,
            );
            self.replace_hook_authority(None);
        } else if foreground_takeover_allowed {
            self.suppress_current_full_lifecycle_hook_authority(
                FullLifecycleHookSuppressionReason::HookClear,
            );
            self.replace_hook_authority(None);
        }
        self.reconcile_agent_name_owner(&agent_label, Some(&session_ref));
        self.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
            source,
            agent: agent_label,
            session_ref,
        });
        let current_session = self.current_session_identity_for_persistence();
        let session_ref_changed = previous_session != current_session;
        let session_replaced = previous_session.is_some() && session_ref_changed;
        if session_replaced {
            self.release_blocked_hold_at(now);
        }
        let hook_work_context_changed = if previous_session != current_session {
            self.clear_hook_work_context()
        } else {
            false
        };
        let closing_report_retired = self.retire_closing_report_outside_current_session(
            session_ref_changed,
            session_replaced,
            now,
        );
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed,
            session_replaced,
            hook_work_context_changed,
            agent_released: false,
            sidebar_projection_changed: closing_report_retired,
        })
    }

    fn known_agent_label_conflicts_with_detected_agent(&self, agent_label: &str) -> bool {
        let Some(detected_agent) = self.detected_agent else {
            return false;
        };
        crate::detect::parse_agent_label(agent_label)
            .is_some_and(|hook_agent| hook_agent != detected_agent)
    }

    fn foreground_agent_confirms_different_owner_takeover(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
        session_start_source: Option<&str>,
    ) -> bool {
        Self::session_start_source_is_recognized(session_start_source)
            && self.foreground_agent_confirms_session_owner(source, agent_label, session_ref)
    }

    fn foreground_agent_confirms_hook_authority_takeover(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        session_ref.as_ref().is_some_and(|session_ref| {
            self.foreground_agent_confirms_session_owner(source, agent_label, session_ref)
        })
    }

    fn foreground_agent_confirms_session_owner(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> bool {
        let Some(detected_agent) = self.detected_agent else {
            return false;
        };
        crate::detect::parse_agent_label(agent_label) == Some(detected_agent)
            && crate::agent_resume::plan(source, agent_label, session_ref).is_some()
    }

    fn accept_hook_report(&mut self, source: &str, seq: Option<u64>) -> bool {
        let Some(seq) = seq else {
            return !self.hook_report_sequences.contains_key(source);
        };

        if self
            .hook_report_sequences
            .get(source)
            .is_some_and(|last_seq| seq <= *last_seq)
        {
            return false;
        }

        self.hook_report_sequences.insert(source.to_string(), seq);
        true
    }

    #[cfg(test)]
    pub fn clear_hook_authority(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.clear_hook_authority_with_mutation(source, seq)
            .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn clear_hook_authority_with_mutation(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        let sequence_source = source.map(str::to_string).or_else(|| {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.source.clone())
        });
        let should_clear = self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| source.is_none_or(|source| authority.source == source));
        if !should_clear {
            return None;
        }
        if let Some(source) = sequence_source.as_deref() {
            if !self.accept_hook_report(source, seq) {
                return None;
            }
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.suppress_current_full_lifecycle_hook_authority(
            FullLifecycleHookSuppressionReason::HookClear,
        );
        self.replace_hook_authority(None);
        self.persisted_agent_session = None;
        let hook_work_context_changed = self.clear_hook_work_context();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session.is_some(),
            session_replaced: previous_session.is_some(),
            hook_work_context_changed,
            agent_released: false,
            sidebar_projection_changed: false,
        })
    }

    pub fn retire_output_inconsistent_hook_authority_at(
        &mut self,
        observed_at: Instant,
    ) -> Option<TerminalStateMutation> {
        self.retire_hook_authority_at(observed_at, true, false, false)
    }

    fn retire_hook_authority_at(
        &mut self,
        observed_at: Instant,
        clear_report: bool,
        release_blocked_hold: bool,
        answer_human_gate: bool,
    ) -> Option<TerminalStateMutation> {
        let should_retire = self.hook_authority.as_ref().is_some_and(|authority| {
            authority.state != AgentState::Working
                && authority.retired_at.is_none()
                && authority.reported_at <= observed_at
                && self.hook_authority_is_effective(authority)
                && (crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                ) || (authority.state == AgentState::Blocked
                    && crate::detect::is_closing_block_source(
                        &authority.source,
                        &authority.agent_label,
                    )))
        });
        let should_release_blocked_hold = release_blocked_hold
            && self
                .blocked_state_hold
                .as_ref()
                .is_some_and(|hold| observed_at >= hold.since);
        let should_answer_human_gate = answer_human_gate
            && self
                .closing_report
                .as_ref()
                .is_some_and(|report| report.unanswered.is_some());
        if !should_retire && !should_release_blocked_hold && !should_answer_human_gate {
            return None;
        }

        let retiring_closing_report = should_retire
            && self.hook_authority.as_ref().is_some_and(|authority| {
                crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
            });

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        if let Some(authority) = self.hook_authority.as_mut().filter(|_| should_retire) {
            authority.retired_at = Some(observed_at);
        }
        if should_release_blocked_hold {
            self.release_blocked_hold_at(observed_at);
        }
        if should_answer_human_gate {
            self.clear_unanswered_closing_blockers();
        }
        let task_report_cleared =
            clear_report && retiring_closing_report && self.clear_closing_task_report(now);
        if task_report_cleared {
            self.revision = self.revision.wrapping_add(1);
        }

        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: false,
            session_replaced: false,
            hook_work_context_changed: false,
            agent_released: false,
            sidebar_projection_changed: task_report_cleared || should_answer_human_gate,
        })
    }

    pub fn retire_blocked_full_lifecycle_hook_authority_at(
        &mut self,
        observed_at: Instant,
    ) -> Option<TerminalStateMutation> {
        self.retire_hook_authority_at(observed_at, false, true, true)
    }

    pub fn retire_blocked_hook_authority_for_automated_input_at(
        &mut self,
        observed_at: Instant,
    ) -> Option<TerminalStateMutation> {
        self.retire_hook_authority_at(observed_at, false, true, false)
    }

    pub fn full_lifecycle_hook_authority_deadline(&self, timeout: Duration) -> Option<Instant> {
        self.hook_authority.as_ref().and_then(|authority| {
            (authority.retired_at.is_none()
                && self.hook_authority_is_effective(authority)
                && !(authority.state == AgentState::Blocked && self.has_pending_human_input())
                && crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                ))
            .then(|| authority.reported_at + timeout + Duration::from_nanos(1))
        })
    }

    pub fn expire_full_lifecycle_hook_authority_at(
        &mut self,
        now: Instant,
        timeout: Duration,
    ) -> Option<TerminalStateMutation> {
        let deadline = self.full_lifecycle_hook_authority_deadline(timeout)?;
        if now < deadline {
            return None;
        }
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        self.hook_authority.as_mut()?.retired_at = Some(now);
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: false,
            session_replaced: false,
            hook_work_context_changed: false,
            agent_released: false,
            sidebar_projection_changed: false,
        })
    }

    pub fn release_agent_with_mutation(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        if self.hook_authority.as_ref().is_some_and(|authority| {
            authority.agent_label != agent_label || authority.source != source
        }) {
            return None;
        }

        let matches_current_agent = self.effective_agent_label() == Some(agent_label);
        let matches_persisted_session = self.persisted_agent_session_matches(source, agent_label);
        if !matches_current_agent && !matches_persisted_session {
            return None;
        }
        if !self.accept_hook_report(source, seq) {
            return None;
        }
        let preserve_foreign_persisted_session = self
            .persisted_agent_session
            .as_ref()
            .is_some_and(|session| session.source != source || session.agent != agent_label);
        let process_owns_agent =
            crate::detect::parse_agent_label(agent_label).is_some_and(|agent| {
                self.detected_agent == Some(agent) && self.recent_agent_process_exit.is_none()
            });

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.suppress_full_lifecycle_hook_report(
            source,
            agent_label,
            FullLifecycleHookSuppressionReason::HookClear,
        );
        if !process_owns_agent {
            self.detected_agent = None;
            self.fallback_state = AgentState::Unknown;
            self.fallback_visible_blocker = false;
            self.fallback_visible_working = false;
            self.fallback_visible_working_observed_at = None;
            self.fallback_working_observed_at = None;
            self.fallback_observed_at = None;
            self.clear_agent_name();
        }
        self.replace_hook_authority(None);
        if !preserve_foreign_persisted_session {
            self.persisted_agent_session = None;
        }
        let current_session = self.current_session_identity_for_persistence();
        let hook_work_context_changed = self.clear_hook_work_context();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
            session_replaced: previous_session.is_some() && previous_session != current_session,
            hook_work_context_changed,
            agent_released: !process_owns_agent,
            sidebar_projection_changed: false,
        })
    }

    /// A full-lifecycle hook owns the pane unless the pane contradicts it.
    ///
    /// Only two things contradict it: the agent process is gone, or screen
    /// detection is looking at a *different* agent. Detection returning nothing
    /// is not a contradiction -- it is the normal state at the end of a turn,
    /// when the prompt box matches no rule or the scan is skipped -- and it must
    /// not demote the hook back to the screen-scraped fallback, because that is
    /// exactly when the hook knows something the screen cannot: a gate is
    /// waiting on a human.
    fn hook_authority_is_effective(&self, authority: &HookAuthority) -> bool {
        !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
            || crate::detect::parse_agent_label(&authority.agent_label).is_none_or(|agent| {
                self.recent_agent_process_exit.is_none()
                    && self
                        .detected_agent
                        .is_none_or(|detected_agent| detected_agent == agent)
            })
    }

    pub fn effective_agent_label(&self) -> Option<&str> {
        self.hook_authority
            .as_ref()
            .filter(|authority| self.hook_authority_is_effective(authority))
            .map(|authority| authority.agent_label.as_str())
            .or_else(|| {
                self.recent_agent_process_exit
                    .is_none()
                    .then(|| self.detected_agent.map(crate::detect::agent_label))
                    .flatten()
            })
    }

    pub fn effective_known_agent(&self) -> Option<Agent> {
        self.effective_agent_label()
            .and_then(crate::detect::parse_agent_label)
    }

    pub(crate) fn agent_lifecycle_context(&self) -> Option<Agent> {
        self.effective_known_agent()
            .or_else(|| self.recent_agent_process_exit.map(|exit| exit.agent))
    }

    /// Authoritative runtime timestamp behind the sidebar activity-age field.
    ///
    /// Working agents expose the start of their current active interval.
    /// Non-working agents expose when their most recent active interval ended.
    pub(crate) fn agent_activity_at(&self) -> Option<Instant> {
        if self.state == AgentState::Working {
            self.agent_active_since
        } else {
            self.agent_last_active_at
        }
    }

    #[cfg(unix)]
    pub(crate) fn agent_activity_handoff_state(
        &self,
        now: Instant,
    ) -> Option<AgentActivityHandoffState> {
        if self.agent_activity_owner.is_none()
            && self.agent_active_since.is_none()
            && self.agent_last_active_at.is_none()
        {
            return None;
        }
        Some(AgentActivityHandoffState {
            state: self.state.into(),
            active_elapsed: self
                .agent_active_since
                .map(|started| now.saturating_duration_since(started)),
            last_active_elapsed: self
                .agent_last_active_at
                .map(|ended| now.saturating_duration_since(ended)),
            owner: self.agent_activity_owner.clone(),
        })
    }

    #[cfg(unix)]
    pub(crate) fn terminal_agent_handoff_state(
        &self,
        now: Instant,
    ) -> Option<TerminalAgentHandoffState> {
        let metadata = self.agent_metadata_handoff_state(now);
        let closing_report = self
            .closing_report
            .as_ref()
            .filter(|report| *report != &ClosingReport::default());
        if self.state == AgentState::Unknown
            && self.detected_agent.is_none()
            && self.recent_agent_process_exit.is_none()
            && self.hook_authority.is_none()
            && !self.supervisor_stale
            && metadata.is_empty()
            && closing_report.is_none()
        {
            return None;
        }
        Some(TerminalAgentHandoffState {
            state: self.state.into(),
            detected_agent: self
                .detected_agent
                .map(crate::detect::agent_label)
                .map(str::to_string),
            recent_agent_process_exit: self.recent_agent_process_exit.map(|exit| {
                RecentAgentProcessExitHandoffState {
                    agent_label: crate::detect::agent_label(exit.agent).to_string(),
                    observed_elapsed: now.saturating_duration_since(exit.observed_at),
                }
            }),
            blocked_state_hold: self.blocked_state_hold.as_ref().map(|hold| {
                BlockedStateHoldHandoffState {
                    owner: hold.owner.clone(),
                    since_elapsed: now.saturating_duration_since(hold.since),
                }
            }),
            hook_authority: self
                .hook_authority
                .as_ref()
                .map(|authority| HookAuthorityHandoffState::capture(authority, now)),
            supervisor_stale: self.supervisor_stale,
            metadata,
            closing_report: closing_report
                .map(|report| ClosingReportHandoffState::capture(report, now)),
            closing_gates: Vec::new(),
            closing_items: Vec::new(),
            closing_decisions: Vec::new(),
            closing_report_subagents: None,
            closing_idle: None,
            closing_contract: None,
            closing_contract_met: None,
            closing_contract_met_elapsed: None,
            closing_completion: None,
            closing_wait: None,
            closing_parse: None,
            closing_workers_unknown: None,
            unreported_turn_started_elapsed: self
                .unreported_turn_started_at
                .map(|started_at| now.saturating_duration_since(started_at)),
        })
    }

    #[cfg(unix)]
    pub(crate) fn restore_terminal_agent_handoff_state(
        &mut self,
        handoff: TerminalAgentHandoffState,
        now: Instant,
    ) {
        let closing_report = handoff
            .closing_report
            .map(|report| report.restore(now))
            .unwrap_or_else(|| {
                let completion = match handoff.closing_completion.as_deref() {
                    Some("complete") => Some(crate::api::schema::ClosingCompletion::Complete),
                    Some("incomplete") => Some(crate::api::schema::ClosingCompletion::Incomplete),
                    Some("missing") => Some(crate::api::schema::ClosingCompletion::Missing),
                    _ => None,
                };
                let parse_status = match handoff.closing_parse.as_deref() {
                    Some("ok") => Some(crate::api::schema::ClosingParseStatus::Ok),
                    Some("missing") => Some(crate::api::schema::ClosingParseStatus::Missing),
                    Some("malformed") => Some(crate::api::schema::ClosingParseStatus::Malformed),
                    _ => None,
                };
                ClosingReport {
                    closing_gates: handoff.closing_gates,
                    closing_items: handoff.closing_items,
                    closing_decisions: handoff.closing_decisions,
                    closing_report_subagents: handoff.closing_report_subagents,
                    closing_idle: handoff.closing_idle,
                    closing_contract: handoff.closing_contract,
                    closing_contract_met: handoff.closing_contract_met,
                    closing_contract_met_at: handoff
                        .closing_contract_met_elapsed
                        .and_then(|elapsed| now.checked_sub(elapsed)),
                    completion,
                    external_wait: handoff.closing_wait,
                    parse_status,
                    workers_unknown: handoff
                        .closing_workers_unknown
                        .as_deref()
                        .and_then(parse_binary_token),
                    ..ClosingReport::default()
                }
            });
        self.state = handoff.state.into();
        self.working_since = None;
        self.fallback_state = self.state;
        self.detected_agent = handoff
            .detected_agent
            .as_deref()
            .and_then(crate::detect::parse_canonical_agent_label);
        self.recent_agent_process_exit = handoff.recent_agent_process_exit.and_then(|exit| {
            crate::detect::parse_canonical_agent_label(&exit.agent_label).map(|agent| {
                RecentAgentProcessExit {
                    agent,
                    observed_at: now.checked_sub(exit.observed_elapsed).unwrap_or(now),
                }
            })
        });
        self.hook_authority = handoff
            .hook_authority
            .map(|authority| authority.restore(now));
        self.blocked_state_hold = handoff.blocked_state_hold.map(|hold| BlockedStateHold {
            owner: hold.owner,
            since: now.checked_sub(hold.since_elapsed).unwrap_or(now),
        });
        self.supervisor_stale = handoff.supervisor_stale;
        self.restore_agent_metadata_handoff_state(handoff.metadata, now);
        self.closing_report = Some(closing_report);
        self.unreported_turn_started_at = handoff
            .unreported_turn_started_elapsed
            .and_then(|elapsed| now.checked_sub(elapsed));
    }

    #[cfg(unix)]
    pub(crate) fn restore_agent_activity_handoff_state(
        &mut self,
        handoff: AgentActivityHandoffState,
        now: Instant,
    ) {
        self.state = handoff.state.into();
        self.working_since = None;
        self.fallback_state = self.state;
        self.agent_active_since = handoff
            .active_elapsed
            .and_then(|elapsed| now.checked_sub(elapsed));
        self.agent_last_active_at = handoff
            .last_active_elapsed
            .and_then(|elapsed| now.checked_sub(elapsed));
        self.agent_activity_owner = handoff.owner;
    }

    pub(crate) fn unchanged_effective_state_change_at(&self, now: Instant) -> EffectiveStateChange {
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();
        let state = self.state;
        let presentation = self.effective_presentation_for_state_at(state, now);
        EffectiveStateChange {
            previous_agent_label: agent_label.clone(),
            previous_known_agent: known_agent,
            previous_state: state,
            previous_presentation: presentation.clone(),
            agent_label,
            known_agent,
            state,
            presentation,
        }
    }

    pub(crate) fn recompute_effective_state_from_current_at(
        &mut self,
        now: Instant,
    ) -> EffectiveStateChange {
        let current = self.unchanged_effective_state_change_at(now);
        self.recompute_effective_state(
            current.agent_label.clone(),
            current.known_agent,
            current.state,
            current.presentation.clone(),
            now,
        )
        .unwrap_or(current)
    }

    pub fn full_lifecycle_hook_authority_active(&self) -> bool {
        self.live_full_lifecycle_hook_authority()
    }

    #[cfg(test)]
    pub(crate) fn blocked_state_hold_active_for_test(&self) -> bool {
        self.blocked_state_hold.is_some()
    }

    pub fn hook_authority_output_retirement_eligible(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            authority.retired_at.is_none()
                && authority.state != AgentState::Working
                && self.hook_authority_is_effective(authority)
                && crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                )
        })
    }

    fn visible_blocker_overrides_hook(&self) -> bool {
        self.fallback_visible_blocker
            && self
                .fallback_observed_at
                .zip(self.hook_authority.as_ref())
                .is_some_and(|(observed_at, authority)| observed_at > authority.reported_at)
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority.state != AgentState::Blocked
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            })
    }

    fn closing_block_gate_authority(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            authority.retired_at.is_none()
                && authority.state == AgentState::Blocked
                && self.hook_authority_is_effective(authority)
                && crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
        })
    }

    fn closing_block_non_gate_yields_to_screen(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            authority.retired_at.is_none()
                && self.hook_authority_is_effective(authority)
                && authority.state != AgentState::Blocked
                && !self.waiting_on_agents()
                && crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
                && self
                    .fallback_observed_at
                    .is_some_and(|observed_at| observed_at > authority.reported_at)
        })
    }

    pub(crate) fn closing_block_non_gate_waits_for_screen_refresh(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            authority.retired_at.is_none()
                && self.hook_authority_is_effective(authority)
                && authority.state != AgentState::Blocked
                && crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
                && self
                    .fallback_observed_at
                    .is_none_or(|observed_at| observed_at <= authority.reported_at)
        })
    }

    /// Names the evidence that currently owns `self.state`.
    ///
    /// Diagnostic manifest evaluation may observe newer terminal bytes before
    /// the detector publishes them. API consumers must use this applied-state
    /// arbitration instead of promoting diagnostic-only evidence.
    pub(crate) fn effective_state_arbitration(&self) -> &'static str {
        self.effective_state_and_arbitration().1
    }

    fn effective_state_and_arbitration(&self) -> (AgentState, &'static str) {
        let (state, arbitration) = self.lifecycle_state_and_arbitration();
        let (projected, task_arbitration) =
            self.closing_task_projection(state, self.has_pending_human_input());
        if projected != AgentState::Blocked
            && self
                .closing_report
                .as_ref()
                .is_some_and(|report| report.unanswered.is_some())
        {
            return (AgentState::Blocked, "closing_human_input");
        }
        if projected != AgentState::Blocked
            && self
                .blocked_state_hold
                .as_ref()
                .is_some_and(|hold| self.blocked_hold_matches_current_activity(hold))
        {
            return (AgentState::Blocked, "blocked_hold");
        }
        if projected == state {
            (state, arbitration)
        } else {
            (projected, task_arbitration)
        }
    }

    fn lifecycle_state_and_arbitration(&self) -> (AgentState, &'static str) {
        let (detected_state, arbitration) = self.detected_state_and_arbitration();
        if detected_state == AgentState::Idle
            && self.effective_agent_label().is_some()
            && self.foreground_process_active
            && !self
                .blocked_state_hold
                .as_ref()
                .is_some_and(|hold| self.blocked_hold_matches_current_activity(hold))
            && !(self.detected_agent == Some(Agent::Codex)
                && self.fallback_state == AgentState::Idle)
            && !self.finished_closing_report()
        {
            (AgentState::Working, "foreground_process")
        } else {
            (detected_state, arbitration)
        }
    }

    fn detected_state_and_arbitration(&self) -> (AgentState, &'static str) {
        if self.visible_blocker_overrides_hook() {
            (AgentState::Blocked, "visible_blocker_over_hook")
        } else if self.closing_block_gate_authority() {
            (AgentState::Blocked, "closing_block_gate")
        } else if self.waiting_on_agents() {
            (AgentState::Working, "closing_block_subagents")
        } else if self.closing_block_non_gate_yields_to_screen() {
            (self.fallback_state, "screen")
        } else if self.visible_working_overrides_idle_hook()
            || self.detected_working_overrides_idle_hook()
        {
            (AgentState::Working, "screen")
        } else if let Some(authority) = self.hook_authority.as_ref().filter(|authority| {
            authority.retired_at.is_none() && self.hook_authority_is_effective(authority)
        }) {
            let arbitration = if self.live_full_lifecycle_hook_authority() {
                "fresh_full_lifecycle_hook"
            } else if self.closing_block_non_gate_waits_for_screen_refresh() {
                "closing_block_report"
            } else {
                "hook_report"
            };
            (authority.state, arbitration)
        } else {
            (self.fallback_state, "screen")
        }
    }

    fn visible_working_overrides_idle_hook(&self) -> bool {
        let Some(authority) = self.hook_authority.as_ref() else {
            return false;
        };
        authority.retired_at.is_none()
            && self.hook_authority_is_effective(authority)
            && !crate::detect::full_lifecycle_hook_authority(
                &authority.source,
                &authority.agent_label,
            )
            && authority.state == AgentState::Idle
            && crate::detect::parse_agent_label(&authority.agent_label) == self.detected_agent
            && self.fallback_visible_working
            && self
                .fallback_visible_working_observed_at
                .is_some_and(|observed_at| {
                    // Reuse the detector's stable-signal interval so one frame
                    // left over from the hook report cannot start a new turn.
                    authority
                        .reported_at
                        .checked_add(crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH)
                        .is_some_and(|settled_at| observed_at >= settled_at)
                })
    }

    fn detected_working_overrides_idle_hook(&self) -> bool {
        let Some(authority) = self.hook_authority.as_ref() else {
            return false;
        };
        authority.retired_at.is_none()
            && self.hook_authority_is_effective(authority)
            && !crate::detect::full_lifecycle_hook_authority(
                &authority.source,
                &authority.agent_label,
            )
            && authority.state == AgentState::Idle
            && crate::detect::parse_agent_label(&authority.agent_label) == self.detected_agent
            && self
                .fallback_working_observed_at
                .is_some_and(|observed_at| {
                    // The same settled interval covers output-promoted working
                    // evidence, so a stale frame cannot start a new turn.
                    authority
                        .reported_at
                        .checked_add(crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH)
                        .is_some_and(|settled_at| observed_at >= settled_at)
                })
    }

    fn live_full_lifecycle_hook_authority(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            authority.retired_at.is_none()
                && self.hook_authority_is_effective(authority)
                && crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                )
        })
    }

    pub fn set_manual_label(&mut self, label: String) {
        let label = label.trim().to_string();
        self.manual_label = (!label.is_empty()).then_some(label);
    }

    pub fn clear_manual_label(&mut self) {
        self.manual_label = None;
    }

    pub fn set_agent_name(&mut self, name: String) {
        self.agent_name = (!name.is_empty()).then_some(name);
        self.agent_name_owner = self.agent_name.as_ref().and_then(|_| {
            self.hook_authority
                .as_ref()
                .map(|authority| AgentNameOwner {
                    agent_label: authority.agent_label.clone(),
                    session_ref: authority.session_ref.clone(),
                })
                .or_else(|| {
                    self.persisted_agent_session
                        .as_ref()
                        .map(|session| AgentNameOwner {
                            agent_label: session.agent.clone(),
                            session_ref: Some(session.session_ref.clone()),
                        })
                })
                .or_else(|| {
                    self.effective_agent_label()
                        .map(|agent_label| AgentNameOwner {
                            agent_label: agent_label.to_string(),
                            session_ref: None,
                        })
                })
        });
    }

    pub fn begin_managed_agent(
        &mut self,
        name: String,
        kind: Agent,
        now: Instant,
        settle_delay: Duration,
        timeout: Duration,
    ) {
        self.set_agent_name(name);
        self.agent_name_owner = Some(AgentNameOwner {
            agent_label: crate::detect::agent_label(kind).to_string(),
            session_ref: None,
        });
        self.managed_agent = Some(ManagedAgent {
            kind,
            phase: ManagedAgentPhase::Pending {
                ready_after: Some(now.checked_add(settle_delay).unwrap_or(now)),
                deadline: now.checked_add(timeout).unwrap_or(now),
                observed_expected: false,
            },
        });
    }

    pub fn managed_agent_launch_pending(&self) -> bool {
        self.managed_agent.is_some_and(|managed| {
            matches!(
                managed.phase,
                ManagedAgentPhase::Pending { .. } | ManagedAgentPhase::Blocked
            )
        })
    }

    pub fn managed_agent_interactive_ready(&self) -> bool {
        self.managed_agent
            .is_some_and(|managed| matches!(managed.phase, ManagedAgentPhase::Active))
    }

    /// Remote control may answer a managed agent while it is blocked. This is
    /// separate from ordinary prompt readiness so other API paths keep their
    /// stricter active-only behavior.
    // Windows keeps the state helper for cross-platform test fixtures; only
    // Unix can use it to authorize a PTY control lease.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn managed_agent_control_ready(&self) -> bool {
        self.managed_agent.is_some_and(|managed| {
            matches!(
                managed.phase,
                ManagedAgentPhase::Active | ManagedAgentPhase::Blocked
            )
        })
    }

    pub fn managed_agent_kind(&self) -> Option<Agent> {
        self.managed_agent.map(|managed| managed.kind)
    }

    pub fn next_managed_agent_deadline(&self) -> Option<Instant> {
        let ManagedAgentPhase::Pending {
            ready_after,
            deadline,
            ..
        } = self.managed_agent?.phase
        else {
            return None;
        };
        Some(ready_after.unwrap_or(deadline).min(deadline))
    }

    pub fn reconcile_managed_agent_at(&mut self, now: Instant, process_exited: bool) -> bool {
        let Some(managed) = self.managed_agent else {
            return false;
        };
        let known_agent = self.effective_known_agent();
        let observed_expected = match managed.phase {
            ManagedAgentPhase::Pending {
                observed_expected, ..
            } => observed_expected || known_agent == Some(managed.kind),
            ManagedAgentPhase::Blocked | ManagedAgentPhase::Active => false,
        };
        let clear = process_exited
            || known_agent.is_some_and(|agent| agent != managed.kind)
            || matches!(managed.phase, ManagedAgentPhase::Pending { .. })
                && observed_expected
                && known_agent.is_none();
        if clear {
            self.clear_agent_name();
            return true;
        }
        if managed.phase == ManagedAgentPhase::Blocked {
            if known_agent == Some(managed.kind) && self.state == AgentState::Idle {
                self.managed_agent = Some(ManagedAgent {
                    kind: managed.kind,
                    phase: ManagedAgentPhase::Active,
                });
                return true;
            }
            return false;
        }
        if let ManagedAgentPhase::Pending {
            ready_after,
            deadline,
            observed_expected: previous_observed_expected,
        } = managed.phase
        {
            if known_agent == Some(managed.kind) && self.state == AgentState::Blocked {
                self.managed_agent = Some(ManagedAgent {
                    kind: managed.kind,
                    phase: ManagedAgentPhase::Blocked,
                });
                return true;
            }
            if now >= deadline {
                self.clear_agent_name();
                return true;
            }
            if ready_after.is_none_or(|ready_after| now >= ready_after) {
                if known_agent == Some(managed.kind) && self.state == AgentState::Idle {
                    self.managed_agent = Some(ManagedAgent {
                        kind: managed.kind,
                        phase: ManagedAgentPhase::Active,
                    });
                    return true;
                }
                if ready_after.is_some() {
                    self.managed_agent = Some(ManagedAgent {
                        kind: managed.kind,
                        phase: ManagedAgentPhase::Pending {
                            ready_after: None,
                            deadline,
                            observed_expected,
                        },
                    });
                    return true;
                }
            }
            if observed_expected != previous_observed_expected {
                self.managed_agent = Some(ManagedAgent {
                    kind: managed.kind,
                    phase: ManagedAgentPhase::Pending {
                        ready_after,
                        deadline,
                        observed_expected,
                    },
                });
                return true;
            }
        }
        false
    }

    pub fn restore_managed_agent(&mut self, name: String, kind: Agent) {
        self.set_agent_name(name);
        self.agent_name_owner = Some(AgentNameOwner {
            agent_label: crate::detect::agent_label(kind).to_string(),
            session_ref: None,
        });
        self.managed_agent = Some(ManagedAgent {
            kind,
            phase: ManagedAgentPhase::Active,
        });
    }

    pub fn clear_agent_name(&mut self) {
        self.agent_name = None;
        self.agent_name_owner = None;
        self.managed_agent = None;
    }

    pub fn clear_agent_runtime_identity_after_respawn(&mut self) -> bool {
        let hook_work_context_changed = self.clear_hook_work_context();
        self.blocked_state_hold = None;
        if let Some(report) = self.closing_report.as_mut() {
            report.unanswered = None;
        }
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_visible_working = false;
        self.fallback_visible_working_observed_at = None;
        self.fallback_working_observed_at = None;
        self.fallback_observed_at = None;
        self.replace_hook_authority(None);
        self.closing_report
            .get_or_insert_default()
            .closing_report_subagents = None;
        self.persisted_agent_session = None;
        self.claude_transcript_session_id = None;
        self.claude_transcript_path = None;
        self.session_name_write_target = None;
        self.set_active_subagents(None);
        self.agent_metadata.clear();
        self.metadata_report_agents.clear();
        self.suppressed_full_lifecycle_hook_reports.clear();
        self.stale_full_lifecycle_hook_sessions.clear();
        self.state = AgentState::Unknown;
        self.last_agent_state_change_seq = None;
        self.working_since = None;
        self.agent_active_since = None;
        self.agent_last_active_at = None;
        self.agent_activity_owner = None;
        self.launch_argv = None;
        self.respawn_shell_on_exit = false;
        self.recent_agent_process_exit = None;
        self.agent_process_acquisition_pending = false;
        self.pending_agent_resume_plan = None;
        self.clear_agent_name();
        hook_work_context_changed
    }

    pub fn is_agent_terminal(&self) -> bool {
        self.agent_name.is_some() || self.effective_agent_label().is_some()
    }

    fn reconcile_agent_name_owner(
        &mut self,
        agent_label: &str,
        session_ref: Option<&crate::agent_resume::AgentSessionRef>,
    ) {
        if self.agent_name.is_none() {
            return;
        }
        if self.managed_agent.is_some_and(|managed| {
            crate::detect::parse_agent_label(agent_label) == Some(managed.kind)
        }) {
            return;
        }
        match self.agent_name_owner.as_mut() {
            Some(owner)
                if owner.agent_label != agent_label
                    || owner
                        .session_ref
                        .as_ref()
                        .zip(session_ref)
                        .is_some_and(|(current, incoming)| current != incoming) =>
            {
                self.agent_name = None;
                self.agent_name_owner = None;
            }
            Some(owner) if owner.session_ref.is_none() && session_ref.is_some() => {
                owner.session_ref = session_ref.cloned();
            }
            None => {
                self.agent_name_owner = Some(AgentNameOwner {
                    agent_label: agent_label.to_string(),
                    session_ref: session_ref.cloned(),
                })
            }
            _ => {}
        }
    }

    pub fn border_label(&self, show_agent_labels: bool) -> Option<String> {
        self.effective_title().or_else(|| {
            self.manual_label.clone().or_else(|| {
                show_agent_labels
                    .then(|| {
                        self.effective_display_agent()
                            .or_else(|| self.effective_agent_label().map(str::to_string))
                    })
                    .flatten()
            })
        })
    }

    pub(super) fn recompute_effective_state(
        &mut self,
        previous_agent_label: Option<String>,
        previous_known_agent: Option<Agent>,
        previous_state: AgentState,
        previous_presentation: EffectivePresentation,
        now: Instant,
    ) -> Option<EffectiveStateChange> {
        let state = self.effective_state_and_arbitration().0;
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();
        let mut activity_owner = self.current_agent_activity_owner();
        if let Some(owner) = activity_owner.as_mut() {
            owner.inherit_detection_lineage(self.agent_activity_owner.as_ref());
        }
        let activity_owner_changed =
            match (self.agent_activity_owner.as_ref(), activity_owner.as_ref()) {
                (Some(previous), Some(current)) => !current.continues_activity_from(previous),
                (None, None) => false,
                _ => true,
            };

        let presentation = self.effective_presentation_for_state_at(state, now);
        self.clear_expiry_pending_for_hidden_metadata();

        if state == AgentState::Working {
            if previous_state != AgentState::Working {
                self.working_since = Some(now);
            }
        } else {
            self.working_since = None;
        }

        if activity_owner_changed {
            if state == AgentState::Working && activity_owner.is_some() {
                self.agent_active_since = Some(now);
                self.agent_last_active_at = None;
            } else {
                self.agent_active_since = None;
                self.agent_last_active_at = None;
            }
        } else if previous_state != state {
            if state == AgentState::Working {
                self.agent_active_since = Some(now);
            } else if previous_state == AgentState::Working {
                self.agent_active_since = None;
                self.agent_last_active_at = Some(now);
            }
        }
        self.agent_activity_owner = activity_owner;

        if previous_agent_label == agent_label
            && previous_state == state
            && previous_presentation == presentation
        {
            return None;
        }

        self.state = state;
        Some(EffectiveStateChange {
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            agent_label,
            known_agent,
            state,
            presentation,
        })
    }
}

pub(crate) fn stabilize_agent_detection(detection: crate::detect::AgentDetection) -> AgentState {
    detection.state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_idle_report_stays_acknowledged_after_viewing() {
        let mut terminal = test_terminal();
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        terminal.apply_closing_report_subagents_at(Some(0), now);

        assert_eq!(
            terminal.sidebar_projection(false),
            (AgentState::Idle, false)
        );
        assert_eq!(terminal.sidebar_projection(true), (AgentState::Idle, true));
        assert_eq!(terminal.sidebar_projection(true), (AgentState::Idle, true));
    }
    use crate::{app::AppState, detect::AgentDetection, workspace::Workspace};

    #[test]
    fn action_point_labels_require_human_attention_regardless_of_legacy_flag() {
        assert_eq!(
            attention_tier(AgentState::Blocked, true, false, false),
            AttentionTier::Blocked
        );
        assert_eq!(
            attention_tier(AgentState::Blocked, false, true, false),
            AttentionTier::Blocked
        );
        assert_eq!(
            attention_tier(AgentState::Blocked, true, true, false),
            AttentionTier::Blocked
        );
        assert_eq!(
            attention_tier(AgentState::Working, false, true, false),
            AttentionTier::Blocked
        );

        let mut terminal = test_terminal();
        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Answer".into(),
            text: "Choose one".into(),
            blocking: false,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        assert!(terminal.has_blocking_closing_items());

        terminal.closing_items_mut()[0].label = "What to test".into();
        terminal.closing_items_mut()[0].blocking = true;
        assert!(!terminal.has_blocking_closing_items());
        assert!(!terminal.has_pending_human_input());
    }

    #[test]
    fn decide_closing_item_projects_blocked_on_an_idle_pane() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Decide".into(),
            text: "Pick the rollout lane".into(),
            blocking: false,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];

        assert!(terminal.has_pending_human_input());
        assert_eq!(
            terminal.effective_state_and_arbitration(),
            (AgentState::Blocked, "closing_human_input")
        );
        assert_eq!(
            terminal
                .metadata_tokens_for_api()
                .get("closing_blocking")
                .map(String::as_str),
            Some("1")
        );
    }

    const TEST_AGENT_STALE_AFTER: Duration = Duration::from_secs(5 * 60);
    const TEST_SUBAGENT_STALE_AFTER: Duration = Duration::from_secs(30 * 60);

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
    }

    fn terminal_with_restored_git_pr(url: &str) -> TerminalState {
        let mut terminal = test_terminal();
        let repo = crate::work_context::repo_slug_from_pr_url(url).expect("PR repository");
        terminal
            .restore_work_context_with_tiers(
                crate::work_context::PaneWorkContext::default(),
                Some(crate::work_context::PaneWorkContextTiers {
                    git_observation: crate::work_context::PaneWorkContext {
                        pr_urls: vec![url.into()],
                        repo: Some(repo),
                        role: Some(crate::work_context::PaneWorkRole::Ship),
                        active_owner: true,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            )
            .expect("restore git tier");
        terminal
    }

    #[test]
    fn inferred_pr_change_rebinds_repo_only_when_the_repository_changes() {
        for (next_pr, expected_repo) in [
            ("https://github.com/o/r/pull/2", "o/r"),
            ("https://github.com/new/repo/pull/2", "new/repo"),
        ] {
            let mut terminal = terminal_with_restored_git_pr("https://github.com/o/r/pull/1");

            assert!(terminal
                .set_inferred_pr_url(next_pr.into())
                .expect("infer different PR"));

            let git = terminal.work_context.snapshot_tiers().git_observation;
            assert_eq!(git.pr_urls, [next_pr]);
            assert_eq!(git.repo.as_deref(), Some(expected_repo));
            assert_eq!(git.role, None);
            assert!(!git.active_owner);
        }
    }

    #[test]
    fn inferred_pr_change_rebinds_case_variant_derived_repo() {
        let mut terminal = test_terminal();
        terminal
            .restore_work_context_with_tiers(
                crate::work_context::PaneWorkContext::default(),
                Some(crate::work_context::PaneWorkContextTiers {
                    git_observation: crate::work_context::PaneWorkContext {
                        repo: Some("owner/repo".into()),
                        pr_urls: vec!["https://github.com/Owner/Repo/pull/1".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            )
            .expect("restore git tier");

        assert!(terminal
            .set_inferred_pr_url("https://github.com/new/repo/pull/2".into())
            .expect("infer cross-repository pull request"));

        let git = terminal.work_context.snapshot_tiers().git_observation;
        assert_eq!(git.repo.as_deref(), Some("new/repo"));
    }

    #[test]
    fn inferring_same_pr_keeps_git_role_and_owner() {
        let mut terminal = terminal_with_restored_git_pr("https://github.com/o/r/pull/1");

        assert!(!terminal
            .set_inferred_pr_url("https://github.com/o/r/pull/1".into())
            .expect("re-infer same PR"));

        let git = terminal.work_context.snapshot_tiers().git_observation;
        assert_eq!(git.role, Some(crate::work_context::PaneWorkRole::Ship));
        assert!(git.active_owner);
    }

    fn closing_tokens(items: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        items
            .iter()
            .map(|(key, value)| ((*key).into(), value.map(str::to_string)))
            .collect()
    }

    #[test]
    fn completion_tier_follows_the_three_tier_table() {
        let cases = [
            (
                Some("tests pass"),
                Some(true),
                Some(true),
                true,
                CompletionTier::ContractSatisfied,
            ),
            (
                Some("tests pass"),
                Some(false),
                Some(true),
                true,
                CompletionTier::DeclaredDone,
            ),
            (None, None, None, false, CompletionTier::StoppedUndeclared),
        ];
        for (contract, met, idle, has_tokens, expected) in cases {
            assert_eq!(
                derive_completion_tier(
                    AgentState::Idle,
                    contract,
                    met,
                    idle,
                    false,
                    None,
                    false,
                    has_tokens,
                ),
                Some(expected)
            );
        }
    }

    #[test]
    fn completion_tier_requires_a_stopped_quiet_session() {
        let base = || {
            derive_completion_tier(
                AgentState::Idle,
                Some("tests pass"),
                Some(true),
                Some(true),
                false,
                None,
                false,
                true,
            )
        };
        assert_eq!(base(), Some(CompletionTier::ContractSatisfied));
        assert_eq!(
            derive_completion_tier(
                AgentState::Working,
                Some("tests pass"),
                Some(true),
                Some(true),
                false,
                None,
                false,
                true,
            ),
            None
        );
        for (gates, agents, shells) in [
            (true, None, false),
            (false, Some(1), false),
            (false, None, true),
        ] {
            assert_eq!(
                derive_completion_tier(
                    AgentState::Idle,
                    Some("tests pass"),
                    Some(true),
                    Some(true),
                    gates,
                    agents,
                    shells,
                    true,
                ),
                None
            );
        }
    }

    #[test]
    fn auto_settle_requires_ready_complete_idle_and_consumes_each_turn_once() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("session-a").unwrap(),
        });
        terminal.restore_auto_settle_state("session-a", true, None);
        terminal.set_closing_report_scope("herdr:codex".into(), Some("session-a".into()), Some(1));
        assert!(!terminal.observe_auto_settle_transition(false, true, false));
        assert!(!terminal.observe_auto_settle_transition(true, false, false));
        assert!(terminal.observe_auto_settle_transition(true, true, false));
        assert!(!terminal.observe_auto_settle_transition(false, true, false));
        terminal.set_closing_report_scope("herdr:codex".into(), Some("session-a".into()), Some(2));
        assert!(terminal.observe_auto_settle_transition(true, true, false));
    }

    #[test]
    fn automated_nudge_clears_auto_settle_reply_until_new_human_input() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("session-a").unwrap(),
        });
        terminal.note_user_reply();
        terminal.clear_auto_settle_user_reply();
        terminal.set_closing_report_scope("herdr:claude".into(), Some("session-a".into()), Some(1));
        assert!(!terminal.observe_auto_settle_transition(true, true, false));
        terminal.note_user_reply();
        terminal.set_closing_report_scope("herdr:claude".into(), Some("session-a".into()), Some(2));
        assert!(terminal.observe_auto_settle_transition(true, true, false));
    }

    #[test]
    fn auto_settle_does_not_fire_until_the_user_has_replied() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("session-a").unwrap(),
        });
        terminal.restore_auto_settle_state("session-a", false, None);
        terminal.set_closing_report_scope("herdr:codex".into(), Some("session-a".into()), Some(1));
        assert!(!terminal.observe_auto_settle_transition(true, true, false));
        terminal.note_user_reply();
        assert!(!terminal.observe_auto_settle_transition(true, true, false));
        terminal.set_closing_report_scope("herdr:codex".into(), Some("session-a".into()), Some(2));
        assert!(terminal.observe_auto_settle_transition(true, true, false));
    }

    #[test]
    fn user_input_without_an_agent_session_does_not_arm_auto_settle() {
        let mut terminal = test_terminal();
        terminal.note_user_reply();
        terminal.set_closing_report_scope("herdr:codex".into(), Some("session-a".into()), Some(1));
        assert!(!terminal.observe_auto_settle_transition(true, true, false));
        assert_eq!(
            terminal.auto_settle_persistence_state("session-a"),
            (false, Some(1))
        );
    }

    #[test]
    fn new_session_clears_pending_auto_settle_evidence() {
        let mut terminal = test_terminal();
        terminal.restore_auto_settle_state("session-a", true, None);
        terminal.set_closing_report_scope("herdr:codex".into(), Some("session-b".into()), Some(2));
        assert!(!terminal.observe_auto_settle_transition(true, true, true));
        assert_eq!(
            terminal.auto_settle_persistence_state("session-b"),
            (false, Some(2))
        );
    }

    #[test]
    fn closing_contract_tokens_replace_clear_and_reject_malformed_pairs() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        assert!(terminal.apply_closing_contract_tokens(
            &closing_tokens(&[
                ("closing_idle", Some("1")),
                ("closing_contract", Some("tests pass")),
                ("closing_contract_met", Some("1")),
            ]),
            now,
        ));
        assert_eq!(terminal.closing_idle, Some(true));
        assert_eq!(terminal.closing_contract.as_deref(), Some("tests pass"));
        assert_eq!(terminal.closing_contract_met, Some(true));
        assert_eq!(terminal.closing_contract_met_at, Some(now));

        let later = now + Duration::from_secs(2);
        assert!(
            terminal.apply_closing_contract_tokens(
                &closing_tokens(&[("closing_idle", Some("1"))]),
                later,
            )
        );
        assert!(terminal.closing_contract.is_none());
        assert!(terminal.closing_contract_met.is_none());
        assert!(terminal.closing_contract_met_at.is_none());

        assert!(terminal.apply_closing_contract_tokens(
            &closing_tokens(&[
                ("closing_idle", Some("invalid")),
                ("closing_contract", Some("tests pass")),
                ("closing_contract_met", Some("invalid")),
            ]),
            later,
        ));
        assert!(terminal.closing_idle.is_none());
        assert!(terminal.closing_contract.is_none());
        assert!(terminal.closing_contract_met.is_none());
    }

    #[test]
    fn incomplete_task_report_cannot_project_as_done() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );

        assert_eq!(
            terminal.sidebar_projection(false),
            (AgentState::Unknown, false)
        );
    }

    #[test]
    fn incomplete_claude_report_without_runtime_worker_count_is_unknown() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(true),
            Instant::now(),
        );

        assert_eq!(terminal.active_subagents, None);
        assert_eq!(terminal.sidebar_projection(false).0, AgentState::Unknown);
        assert_eq!(
            terminal.sidebar_projection_reason(false),
            "closing_task_uncertain"
        );
    }

    #[test]
    fn incomplete_claude_report_with_settled_runtime_workers_projects_idle() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(true),
            Instant::now(),
        );
        terminal.set_active_subagents(Some(0));

        assert_eq!(terminal.sidebar_projection(false).0, AgentState::Idle);
        assert_eq!(terminal.sidebar_projection_reason(false), "lifecycle");
    }

    #[test]
    fn incomplete_claude_report_with_settled_workers_and_needs_you_projects_blocked() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(true),
            Instant::now(),
        );
        terminal.set_active_subagents(Some(0));
        terminal.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose one".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(terminal.sidebar_projection(false).0, AgentState::Blocked);
        assert_eq!(
            terminal.sidebar_projection_reason(true),
            "closing_human_input"
        );
    }

    #[test]
    fn malformed_claude_report_stays_unknown_with_settled_runtime_workers() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Malformed),
            Some(true),
            Instant::now(),
        );
        terminal.set_active_subagents(Some(0));

        assert_eq!(terminal.sidebar_projection(false).0, AgentState::Unknown);
        assert_eq!(
            terminal.sidebar_projection_reason(false),
            "closing_task_uncertain"
        );
    }

    #[test]
    fn task_report_without_completion_cannot_project_as_done() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_task_report(
            None,
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );

        assert_eq!(
            terminal.sidebar_projection(false),
            (AgentState::Unknown, false)
        );
    }

    #[test]
    fn explicit_completion_projects_an_idle_task_as_done() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );

        assert_eq!(terminal.sidebar_projection(true), (AgentState::Idle, true));
    }

    #[test]
    fn idle_closing_report_without_completion_is_not_done() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_task_report(
            None,
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            Some(1),
            now,
        );

        assert_eq!(
            terminal.sidebar_projection(false),
            (AgentState::Unknown, false)
        );
    }

    #[test]
    fn declared_wait_watchdog_uses_deadline_and_fresh_report_clears_stale() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                Some("waiting for CI".into()),
                Some("CI run 4123".into()),
                Some(120),
                Some("2026-08-10T10:00:00Z".into()),
                None,
                Some(1),
                started,
            )
            .expect("declared wait accepted");

        assert_eq!(
            terminal.status_report_snapshot(),
            (
                Some("CI run 4123".into()),
                Some(120),
                Some("2026-08-10T10:00:00Z".into()),
                false,
            )
        );
        assert!(terminal
            .mark_agent_status_stale_at(started + Duration::from_secs(149), TEST_AGENT_STALE_AFTER,)
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(started + Duration::from_secs(150), TEST_AGENT_STALE_AFTER,)
            .is_some());
        assert!(terminal.status_report_snapshot().3);

        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some("2026-08-10T10:03:00Z".into()),
                None,
                Some(2),
                started + Duration::from_secs(151),
            )
            .expect("fresh report accepted");
        assert!(!terminal.status_report_snapshot().3);
        assert_eq!(terminal.status_report_snapshot().0, None);
        assert_eq!(terminal.status_report_snapshot().1, None);
    }

    #[test]
    fn process_exit_clears_supervisor_stale_with_hook_authority() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                None,
                None,
                None,
                None,
                Some(1),
                started,
            )
            .expect("working report accepted");
        terminal
            .mark_agent_status_stale_at(started + AGENT_BUSY_STALE_SILENCE, TEST_AGENT_STALE_AFTER)
            .expect("watchdog should mark the report stale");

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            started + AGENT_BUSY_STALE_SILENCE + Duration::from_secs(1),
        );

        assert!(terminal.hook_authority.is_none());
        assert!(!terminal.supervisor_stale);
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn no_declared_wait_watchdog_stales_after_silence_only_while_working() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some("2026-08-10T10:00:00Z".into()),
                None,
                Some(1),
                started,
            )
            .expect("working report accepted");
        assert!(terminal
            .mark_agent_status_stale_at(
                started + AGENT_BUSY_STALE_SILENCE - Duration::from_secs(1),
                TEST_AGENT_STALE_AFTER,
            )
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(started + AGENT_BUSY_STALE_SILENCE, TEST_AGENT_STALE_AFTER,)
            .is_some());

        let mut waiting = test_terminal();
        waiting.set_detected_state(Some(Agent::Claude), AgentState::Working);
        waiting
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                Some("CI".into()),
                Some(3600),
                None,
                None,
                Some(1),
                started,
            )
            .expect("waiting report accepted");
        assert!(waiting
            .mark_agent_status_stale_at(started + TEST_AGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER,)
            .is_none());
    }

    #[test]
    fn working_report_uses_the_busy_budget() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            started,
        );
        assert_eq!(
            terminal.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            started.checked_add(AGENT_BUSY_STALE_SILENCE),
        );
        assert!(terminal
            .mark_agent_status_stale_at(
                started + AGENT_BUSY_STALE_SILENCE - Duration::from_secs(1),
                TEST_AGENT_STALE_AFTER,
            )
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(started + AGENT_BUSY_STALE_SILENCE, TEST_AGENT_STALE_AFTER,)
            .is_some());
    }

    #[test]
    fn declared_wait_is_ignored_for_non_working_reports() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Blocked);
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Blocked,
                None,
                Some("not a live wait".into()),
                Some(10),
                None,
                None,
                Some(1),
                started,
            )
            .expect("blocked report accepted");
        assert_eq!(terminal.status_report_snapshot().0, None);
        assert_eq!(terminal.status_report_snapshot().1, None);
        assert!(terminal
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_none());
    }

    #[test]
    fn idle_detected_agent_with_foreground_process_is_working() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        let mutation = terminal
            .set_foreground_process(Some("codex".into()), true, Instant::now())
            .expect("foreground process changed");

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            mutation
                .effective_state_change
                .expect("effective state changed")
                .state,
            AgentState::Working
        );
    }

    #[test]
    fn idle_detected_agent_returns_idle_when_foreground_process_clears() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_foreground_process(Some("cargo".into()), true, Instant::now());

        terminal.set_foreground_process(None, false, Instant::now());

        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn idle_detected_agent_without_active_child_stays_idle() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        terminal.set_foreground_process(Some("claude".into()), false, Instant::now());

        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn foreground_process_does_not_override_non_idle_detection_states() {
        for state in [AgentState::Working, AgentState::Blocked] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(Agent::Claude), state);

            terminal.set_foreground_process(Some("cargo".into()), true, Instant::now());

            assert_eq!(terminal.state, state);
        }
    }

    #[test]
    fn activity_timestamp_tracks_working_interval_without_observation_jitter() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            started,
        );
        assert_eq!(terminal.agent_activity_at(), Some(started));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            started + Duration::from_secs(12),
        );
        assert_eq!(terminal.agent_activity_at(), Some(started));

        let finished = started + Duration::from_secs(20);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            finished,
        );
        assert_eq!(terminal.agent_activity_at(), Some(finished));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            finished + Duration::from_secs(30),
        );
        assert_eq!(terminal.agent_activity_at(), Some(finished));
    }

    #[test]
    fn review_findings_activity_owner_refinement_preserves_working_interval() {
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            started,
        );

        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-root").unwrap();
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                "herdr:kimi".into(),
                "kimi".into(),
                Some(session_ref.clone()),
                Some(10),
                Some("startup".into()),
            )
            .is_some());
        assert_eq!(terminal.agent_activity_at(), Some(started));

        assert!(terminal
            .set_hook_authority_with_session_ref(
                "herdr:kimi".into(),
                "kimi".into(),
                AgentState::Working,
                None,
                Some(session_ref),
                Some(11),
            )
            .is_some());
        assert_eq!(terminal.agent_activity_at(), Some(started));
    }

    #[test]
    #[cfg(unix)]
    fn review_fix_handoff_preserves_working_activity_age_and_owner() {
        let started = Instant::now().checked_sub(Duration::from_secs(45)).unwrap();
        let captured_at = Instant::now();
        let mut source = test_terminal();
        source.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            true,
            false,
            false,
            false,
            started,
        );
        let handoff = source
            .agent_activity_handoff_state(captured_at)
            .expect("working activity should transfer");

        let restored_at = captured_at + Duration::from_secs(2);
        let mut restored = test_terminal();
        restored.restore_agent_activity_handoff_state(handoff, restored_at);
        restored.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            true,
            false,
            false,
            false,
            restored_at + Duration::from_secs(1),
        );

        assert_eq!(
            restored.agent_activity_at(),
            Some(started + Duration::from_secs(2))
        );
        assert_eq!(restored.agent_activity_owner, source.agent_activity_owner);
    }

    #[test]
    #[cfg(unix)]
    fn handoff_preserves_latched_gate_without_a_followup_report() {
        let captured_at = Instant::now();
        let mut source = test_terminal();
        source.set_last_turn_at(Some("2026-09-29T12:34:56Z".into()));
        source.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                blocking: true,
                n: 1,
                label: "Gate".into(),
                text: "Choose the release path".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );
        source.apply_closing_contract_tokens(
            &closing_tokens(&[
                ("closing_idle", Some("1")),
                ("closing_contract", Some("tests pass")),
                ("closing_contract_met", Some("1")),
            ]),
            captured_at - Duration::from_secs(4),
        );

        let encoded = serde_json::to_string(
            &source
                .terminal_agent_handoff_state(captured_at)
                .expect("latched gate should create handoff state"),
        )
        .unwrap();
        let decoded: TerminalAgentHandoffState = serde_json::from_str(&encoded).unwrap();
        let mut restored = test_terminal();
        restored
            .restore_terminal_agent_handoff_state(decoded, captured_at + Duration::from_secs(1));

        assert_eq!(restored.last_turn_at(), Some("2026-09-29T12:34:56Z"));
        assert_eq!(restored.closing_gates, source.closing_gates);
        assert_eq!(
            restored.closing_report.as_ref().unwrap().unanswered,
            source.closing_report.as_ref().unwrap().unanswered
        );
        assert_eq!(restored.closing_contract.as_deref(), Some("tests pass"));
        assert_eq!(restored.closing_contract_met, Some(true));
        assert_eq!(
            restored.closing_contract_met_at.map(|reported_at| {
                (captured_at + Duration::from_secs(1)).saturating_duration_since(reported_at)
            }),
            Some(Duration::from_secs(4))
        );
        assert_eq!(restored.state, AgentState::Unknown);
        assert!(restored.hook_authority.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn handoff_preserves_agent_scoped_blocked_hold() {
        let blocked_at = Instant::now();
        let mut source = test_terminal();
        source.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("pi-handoff-hold").unwrap();
        anchor_full_lifecycle_session(
            &mut source,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        source.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref.clone()),
            Some(1),
            blocked_at,
        );
        let captured_at = blocked_at + Duration::from_secs(1);
        let encoded = serde_json::to_string(
            &source
                .terminal_agent_handoff_state(captured_at)
                .expect("blocked hold should create handoff state"),
        )
        .unwrap();
        let decoded: TerminalAgentHandoffState = serde_json::from_str(&encoded).unwrap();
        let mut restored = test_terminal();
        restored
            .restore_terminal_agent_handoff_state(decoded, captured_at + Duration::from_secs(1));
        assert_eq!(restored.state, AgentState::Blocked);
        assert!(restored.blocked_state_hold.is_some());

        restored.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            captured_at + Duration::from_secs(2),
        );
        assert_eq!(restored.state, AgentState::Blocked);
        assert!(restored.blocked_state_hold.is_some());

        restored.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(session_ref.clone()),
            Some(2),
            captured_at + Duration::from_secs(3),
        );
        assert_eq!(restored.state, AgentState::Working);
        assert!(restored.blocked_state_hold.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn handoff_preserves_legacy_guard_for_v2_report_and_done_projection() {
        let captured_at = Instant::now();
        let mut source = test_terminal();
        let session_id = "claude-session";
        source.set_hook_authority_with_session_ref(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            Some(crate::agent_resume::AgentSessionRef::id(session_id).unwrap()),
            Some(10),
        );
        source.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                blocking: true,
                n: 1,
                label: "Gate".into(),
                text: "Approve the release".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );
        source.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            captured_at,
        );
        source.record_legacy_closing_report_precursor(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            Some(session_id.into()),
            Some(10),
        );
        source
            .closing_report
            .as_mut()
            .expect("closing report")
            .requires_legacy_session_guard = true;

        let encoded = serde_json::to_string(
            &source
                .terminal_agent_handoff_state(captured_at)
                .expect("guarded report should create handoff state"),
        )
        .unwrap();
        let decoded: TerminalAgentHandoffState = serde_json::from_str(&encoded).unwrap();
        let mut restored = test_terminal();
        restored
            .restore_terminal_agent_handoff_state(decoded, captured_at + Duration::from_secs(1));

        assert_eq!(restored.closing_gates.len(), 1);
        assert!(restored.closing_task_complete());
        assert_eq!(
            restored.correlate_legacy_closing_report(
                "herdr:claude-closing-block",
                "claude",
                Some(11),
            ),
            Ok(Some(session_id.into()))
        );
    }

    #[test]
    #[cfg(unix)]
    fn handoff_preserves_task_lifecycle_tokens_and_wait_projection() {
        let captured_at = Instant::now();
        let mut source = test_terminal();
        source.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        source.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            Some("CI checks".into()),
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(true),
            captured_at,
        );

        let encoded = serde_json::to_string(
            &source
                .terminal_agent_handoff_state(captured_at)
                .expect("task lifecycle tokens should create handoff state"),
        )
        .unwrap();
        let decoded: TerminalAgentHandoffState = serde_json::from_str(&encoded).unwrap();
        let mut restored = test_terminal();
        restored
            .restore_terminal_agent_handoff_state(decoded, captured_at + Duration::from_secs(1));

        assert_eq!(
            restored
                .metadata_tokens_for_api()
                .get("closing_completion")
                .map(String::as_str),
            Some("incomplete")
        );
        assert_eq!(
            restored
                .metadata_tokens_for_api()
                .get("closing_wait")
                .map(String::as_str),
            Some("CI checks")
        );
        assert_eq!(
            restored
                .metadata_tokens_for_api()
                .get("closing_parse")
                .map(String::as_str),
            Some("ok")
        );
        assert_eq!(
            restored
                .metadata_tokens_for_api()
                .get("closing_workers_unknown")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            restored.sidebar_projection(false),
            (AgentState::Working, false)
        );
    }

    #[test]
    #[cfg(unix)]
    fn handoff_preserves_stale_lifecycle_and_guarded_state_labels() {
        let reported_at = Instant::now();
        let source_name = "herdr:codex-closing-block";
        let mut source = test_terminal();
        source
            .set_hook_authority_report_at(
                source_name.into(),
                "codex".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some("2026-08-27T10:00:00Z".into()),
                None,
                Some(1),
                reported_at,
            )
            .expect("working report accepted");
        source
            .set_agent_metadata(AgentMetadataReport {
                source: source_name.into(),
                agent_label: Some("codex".into()),
                applies_to_source: Some(source_name.into()),
                title: None,
                display_agent: None,
                state_labels: HashMap::from([("stale".into(), "overdue".into())]),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                ttl: None,
                seq: None,
            })
            .expect("metadata report accepted");
        source.supervisor_stale = true;

        let captured_at = reported_at + Duration::from_secs(30);
        let handoff = source
            .terminal_agent_handoff_state(captured_at)
            .expect("stale lifecycle should create handoff state");
        let mut restored = test_terminal();
        restored
            .restore_terminal_agent_handoff_state(handoff, captured_at + Duration::from_secs(1));

        assert_eq!(restored.state, AgentState::Working);
        assert!(restored.supervisor_stale);
        assert_eq!(
            restored.effective_presentation().state_labels.get("stale"),
            Some(&"overdue".to_string())
        );
        assert_eq!(
            restored
                .hook_authority
                .as_ref()
                .map(|authority| authority.reported_at),
            Some(reported_at + Duration::from_secs(1))
        );
    }

    #[test]
    #[cfg(unix)]
    fn handoff_preserves_recent_agent_exit_context_and_detection_seed() {
        let captured_at = Instant::now();
        let mut source = test_terminal();
        source.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            true,
        );

        let handoff = source
            .terminal_agent_handoff_state(captured_at)
            .expect("recent agent exit should create handoff state");
        assert_eq!(
            handoff.handoff_detection_seed(),
            (Some(Agent::Codex), AgentState::Idle)
        );
        let mut restored = test_terminal();
        restored
            .restore_terminal_agent_handoff_state(handoff, captured_at + Duration::from_secs(1));

        assert_eq!(restored.agent_lifecycle_context(), Some(Agent::Codex));
        assert_eq!(restored.effective_agent_label(), None);
        assert_eq!(restored.state, AgentState::Idle);
    }

    #[test]
    fn review_findings_activity_owner_demotion_records_idle_transition() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-root").unwrap();
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                "herdr:kimi".into(),
                "kimi".into(),
                Some(session_ref.clone()),
                Some(10),
                Some("startup".into()),
            )
            .is_some());
        assert!(terminal
            .set_hook_authority_with_session_ref(
                "herdr:kimi".into(),
                "kimi".into(),
                AgentState::Working,
                None,
                Some(session_ref),
                Some(11),
            )
            .is_some());
        let working_started = terminal.agent_activity_at().unwrap();

        assert!(terminal
            .clear_hook_authority_with_mutation(Some("herdr:kimi"), Some(12))
            .is_some());

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(terminal
            .agent_activity_at()
            .is_some_and(|at| at >= working_started));
    }

    #[test]
    fn review_findings_same_label_replacement_after_fallback_resets_activity() {
        let mut terminal = test_terminal();
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            started,
        );
        let first_session =
            crate::agent_resume::AgentSessionRef::path(test_session_path("first.jsonl")).unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            first_session.clone(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(first_session),
            Some(20),
        );
        assert_eq!(terminal.agent_activity_at(), Some(started));

        terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(21))
            .expect("hook clear should be accepted");
        assert_eq!(terminal.agent_activity_at(), Some(started));

        let replacement_observed = Instant::now();
        terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(test_session_path("second.jsonl")),
                Some(22),
                replacement_observed,
            )
            .expect("replacement hook should be accepted");

        assert_eq!(terminal.agent_activity_at(), Some(replacement_observed));
    }

    #[test]
    fn review_findings_activity_timestamp_resets_for_replacement_session() {
        let mut terminal = test_terminal();
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            started,
        );
        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("first"),
            Some(1),
            Some("startup".into()),
        );
        terminal.agent_active_since = Some(started);

        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("second"),
            Some(2),
            Some("resume".into()),
        );

        assert!(terminal.agent_activity_at().is_some_and(|at| at > started));
    }

    #[test]
    fn review_findings_activity_timestamp_resets_for_detected_agent_owner() {
        let mut terminal = test_terminal();
        let started = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            started,
        );
        let replaced = started + Duration::from_secs(10);

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            replaced,
        );

        assert_eq!(terminal.agent_activity_at(), Some(replaced));
    }

    #[test]
    fn review_findings_idle_activity_does_not_cross_session_owner() {
        let mut terminal = test_terminal();
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            started,
        );
        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("first"),
            Some(1),
            Some("startup".into()),
        );
        let finished = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            finished,
        );
        assert_eq!(terminal.agent_activity_at(), Some(finished));

        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("second"),
            Some(2),
            Some("resume".into()),
        );

        assert_eq!(terminal.agent_activity_at(), None);
    }

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    fn anchor_full_lifecycle_session(
        terminal: &mut TerminalState,
        agent: Agent,
        source: &str,
        agent_label: &str,
        session_ref: crate::agent_resume::AgentSessionRef,
    ) {
        terminal.set_detected_state(Some(agent), terminal.fallback_state);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: source.into(),
            agent: agent_label.into(),
            session_ref,
        });
    }

    #[test]
    // AC8: constructed Blocked and Active managed-agent states are both control-admissible.
    fn managed_agent_control_admits_constructed_blocked_and_active_states() {
        let mut terminal = test_terminal();
        let now = Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::Pi,
            now,
            Duration::from_millis(100),
            Duration::from_secs(1),
        );
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Unknown);

        assert!(terminal.managed_agent_launch_pending());
        assert!(!terminal.managed_agent_interactive_ready());
        assert!(!terminal.managed_agent_control_ready());
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(100), false));
        assert!(terminal.managed_agent_launch_pending());

        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        assert!(!terminal.reconcile_managed_agent_at(now + Duration::from_millis(101), false));

        terminal.set_detected_state(Some(Agent::Pi), AgentState::Blocked);
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(102), false));
        assert!(terminal.managed_agent_launch_pending());
        assert!(!terminal.managed_agent_interactive_ready());
        assert!(terminal.managed_agent_control_ready());
        assert_eq!(terminal.next_managed_agent_deadline(), None);
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(!terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), false));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), false));
        assert!(!terminal.managed_agent_launch_pending());
        assert!(terminal.managed_agent_interactive_ready());
        assert!(terminal.managed_agent_control_ready());

        terminal.set_detected_state(None, AgentState::Unknown);
        assert!(terminal.managed_agent_interactive_ready());
        assert!(!terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), false));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), true));
        assert_eq!(terminal.agent_name, None);
    }

    #[test]
    fn managed_agent_mismatch_and_timeout_release_name() {
        let now = Instant::now();
        let mut mismatch = test_terminal();
        mismatch.begin_managed_agent(
            "reviewer".into(),
            Agent::Pi,
            now,
            Duration::ZERO,
            Duration::from_secs(1),
        );
        mismatch.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert!(mismatch.reconcile_managed_agent_at(now, false));
        assert_eq!(mismatch.agent_name, None);
        assert_eq!(mismatch.managed_agent_kind(), None);

        let mut timed_out = test_terminal();
        timed_out.begin_managed_agent(
            "reviewer".into(),
            Agent::Pi,
            now,
            Duration::from_millis(10),
            Duration::from_millis(20),
        );
        assert!(timed_out.reconcile_managed_agent_at(now + Duration::from_millis(20), false));
        assert_eq!(timed_out.agent_name, None);
        assert_eq!(timed_out.managed_agent_kind(), None);
    }

    #[test]
    fn stabilization_uses_raw_policy_state() {
        let detection = AgentDetection {
            usage_limited: false,
            state: AgentState::Idle,
            skip_state_update: false,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        };

        assert_eq!(stabilize_agent_detection(detection), AgentState::Idle);
    }

    #[test]
    fn hook_authority_overrides_fallback_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.effective_agent_label(), Some("pi"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn hook_authority_can_override_with_unknown_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.effective_known_agent(), None);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn omp_visible_blocker_overrides_fresh_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Omp), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Omp,
            "herdr:omp",
            "omp",
            crate::agent_resume::AgentSessionRef::id("omp-root").unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Omp));
        assert_eq!(terminal.effective_agent_label(), Some("omp"));
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Omp));
        assert_eq!(terminal.state, AgentState::Working);

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Omp),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(change.is_some());
    }

    #[test]
    fn session_only_report_does_not_create_hook_authority() {
        for (agent, source, label, session_id) in [
            (Agent::Codex, "herdr:codex", "codex", "codex-session"),
            (Agent::Devin, "herdr:devin", "devin", "devin-session"),
        ] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(agent), AgentState::Idle);

            let mutation = terminal.set_agent_session_ref(
                source.into(),
                label.into(),
                crate::agent_resume::AgentSessionRef::id(session_id),
                Some(1),
            );

            assert!(mutation.is_some());
            assert!(terminal.hook_authority.is_none());
            assert!(!terminal.full_lifecycle_hook_authority_active());
            assert_eq!(terminal.state, AgentState::Idle);

            terminal.set_detected_state_with_screen_signals_at(
                Some(agent),
                AgentState::Working,
                false,
                false,
                false,
                false,
                false,
                Instant::now(),
            );

            assert_eq!(terminal.state, AgentState::Working);
        }
    }

    #[test]
    fn startup_session_claim_activates_full_lifecycle_integrations() {
        for (agent, source, label) in [
            (Agent::Kimi, "herdr:kimi", "kimi"),
            (Agent::Kilo, "herdr:kilo", "kilo"),
        ] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(agent), AgentState::Idle);
            let session_ref = crate::agent_resume::AgentSessionRef::id(format!("{label}-root"));

            let session = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                session_ref.clone(),
                Some(10),
                Some("startup".into()),
            );
            let working = terminal.set_hook_authority_with_session_ref(
                source.into(),
                label.into(),
                AgentState::Working,
                None,
                session_ref,
                Some(11),
            );

            assert!(
                session.is_some(),
                "{label} should accept its startup session"
            );
            assert!(
                working.is_some(),
                "{label} should accept state after startup"
            );
            assert_eq!(terminal.state, AgentState::Working);
        }
    }

    #[test]
    fn session_identity_claims_leave_state_to_detection() {
        for (source, label, agent, start_source, replacement_source) in [
            (
                "herdr:hermes",
                "hermes",
                Agent::Hermes,
                Some("startup"),
                Some("resume"),
            ),
            (
                "herdr:antigravity_cli",
                "agy",
                Agent::Antigravity,
                None,
                None,
            ),
        ] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(agent), AgentState::Idle);
            let first_ref =
                crate::agent_resume::AgentSessionRef::id(format!("{label}-root")).unwrap();
            let first = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(first_ref.clone()),
                Some(10),
                start_source.map(str::to_string),
            );

            assert!(first.is_some(), "{label} should accept its session");
            assert!(terminal.hook_authority.is_none());
            assert_eq!(terminal.state, AgentState::Idle);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&first_ref)
            );

            terminal.set_detected_state(Some(agent), AgentState::Working);
            let replacement_ref =
                crate::agent_resume::AgentSessionRef::id(format!("{label}-replacement")).unwrap();
            let replacement = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(replacement_ref.clone()),
                Some(11),
                start_source.map(str::to_string),
            );

            assert!(
                replacement.is_some_and(|mutation| mutation.session_ref_changed),
                "{label} should replace its detected session"
            );
            assert!(terminal.hook_authority.is_none());
            assert_eq!(terminal.state, AgentState::Working);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&replacement_ref)
            );

            let legacy_state = terminal.set_hook_authority_with_session_ref(
                source.into(),
                label.into(),
                AgentState::Blocked,
                None,
                Some(replacement_ref.clone()),
                Some(12),
            );
            assert!(legacy_state.is_none());
            assert!(terminal.hook_authority.is_none());
            assert_eq!(terminal.state, AgentState::Working);

            terminal.set_detected_state(None, AgentState::Unknown);
            let background_ref =
                crate::agent_resume::AgentSessionRef::id(format!("{label}-background")).unwrap();
            let background_replacement = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(background_ref.clone()),
                Some(13),
                replacement_source.map(str::to_string),
            );
            assert!(
                background_replacement.is_none(),
                "{label} should reject a background replacement"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&replacement_ref)
            );

            terminal.set_detected_state(Some(agent), AgentState::Idle);
            let retried_replacement = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(background_ref.clone()),
                Some(14),
                replacement_source.map(str::to_string),
            );
            assert!(
                retried_replacement.is_some_and(|mutation| mutation.session_ref_changed),
                "{label} should replace the session once detected"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&background_ref)
            );
        }
    }

    #[test]
    fn pi_session_replacement_reports_reanchor_full_lifecycle_authority() {
        for reason in ["new", "resume", "fork"] {
            let mut terminal = test_terminal();
            let old_session = test_session_path(&format!("pi-{reason}-old.jsonl"));
            let new_session = test_session_path(&format!("pi-{reason}-new.jsonl"));
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
            terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                crate::agent_resume::AgentSessionRef::path(old_session),
                Some(10),
            );

            let session_report = terminal.set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(11),
                Some(reason.into()),
            );

            assert!(
                session_report.is_some(),
                "{reason} should replace the previous Pi session"
            );
            assert!(terminal.hook_authority.is_none());

            let working = terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(12),
            );

            assert!(
                working.is_some(),
                "{reason} should accept working for the replacement session"
            );
            assert_eq!(terminal.state, AgentState::Working);
            assert_eq!(
                terminal.hook_authority.as_ref().unwrap().session_ref,
                crate::agent_resume::AgentSessionRef::path(new_session)
            );
        }
    }

    #[test]
    fn pi_resume_reactivates_a_previously_stale_session() {
        let mut terminal = test_terminal();
        let session_a = test_session_path("pi-session-a.jsonl");
        let session_b = test_session_path("pi-session-b.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(10),
        );

        terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_b.clone()),
            Some(11),
            Some("new".into()),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b.clone()),
            Some(12),
        );

        let resumed = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(13),
            Some("resume".into()),
        );
        let working = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(14),
        );

        assert!(resumed.is_some());
        assert!(working.is_some());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().session_ref,
            crate::agent_resume::AgentSessionRef::path(session_a)
        );

        let late_session_b = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b),
            Some(15),
        );
        assert!(late_session_b.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn pi_startup_adopts_persisted_session_without_live_authority() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("pi-startup-old.jsonl");
        let new_session = test_session_path("pi-startup-new.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(old_session)
                .expect("test session path should be valid"),
        });

        let startup = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(11),
            Some("startup".into()),
        );

        assert!(startup.is_some());
        assert_eq!(
            terminal.current_session_identity_for_persistence(),
            Some((
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRefKind::Path,
                new_session,
            ))
        );
    }

    #[test]
    fn pi_non_replacement_reports_preserve_full_lifecycle_authority() {
        for reason in [None, Some("reload"), Some("startup")] {
            let mut terminal = test_terminal();
            let old_session = test_session_path("pi-current.jsonl");
            let new_session = test_session_path("pi-unexpected.jsonl");
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
            anchor_full_lifecycle_session(
                &mut terminal,
                Agent::Pi,
                "herdr:pi",
                "pi",
                crate::agent_resume::AgentSessionRef::path(old_session.clone()).unwrap(),
            );
            terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                crate::agent_resume::AgentSessionRef::path(old_session.clone()),
                Some(10),
            );

            let session_report = terminal.set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(11),
                reason.map(str::to_string),
            );
            let working = terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(new_session),
                Some(12),
            );

            assert!(session_report.is_none());
            assert!(working.is_none());
            assert_eq!(terminal.state, AgentState::Idle);
            assert_eq!(
                terminal.hook_authority.as_ref().unwrap().session_ref,
                crate::agent_resume::AgentSessionRef::path(old_session),
                "{reason:?} must not replace the current Pi session"
            );
        }
    }

    #[test]
    fn omp_resume_session_report_reanchors_full_lifecycle_authority() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("omp-old.jsonl");
        let new_session = test_session_path("omp-new.jsonl");
        terminal.set_detected_state(Some(Agent::Omp), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session.clone()),
            Some(10),
        );

        let session_report = terminal.set_agent_session_ref_for_session_start(
            "herdr:omp".into(),
            "omp".into(),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(11),
            Some("resume".into()),
        );

        assert!(session_report.is_some());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .unwrap()
                .session_ref,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()).unwrap()
        );

        let blocked = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Blocked,
            Some("waiting".into()),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(12),
        );

        assert!(blocked.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().session_ref,
            crate::agent_resume::AgentSessionRef::path(new_session)
        );

        let stale = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(13),
        );

        assert!(stale.is_none());
        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn late_full_lifecycle_hook_with_same_session_after_process_exit_does_not_reacquire_authority()
    {
        let now = Instant::now();
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_millis(1),
        );
        let late = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(21),
        );

        assert!(late.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn live_full_lifecycle_hook_rejects_different_session_ref_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("one.jsonl")).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("one.jsonl")),
            Some(20),
        );

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("two.jsonl")),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some(test_session_path("one.jsonl").as_str())
        );
    }

    #[test]
    fn fresh_detected_process_keeps_old_session_suppressed_after_process_exit() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-process-exit.jsonl");
        let new_session = test_session_path("new-process-exit.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session.clone()),
            Some(1000),
        );
        let process_exit_seen_at = Instant::now() + Duration::from_secs(1);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            process_exit_seen_at,
        );

        let fresh_process_seen_at = process_exit_seen_at + Duration::from_millis(1);
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            fresh_process_seen_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            fresh_process_seen_at + Duration::from_millis(1),
        );

        let late_old = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(500),
        );
        let fresh_new = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(501),
        );

        assert!(late_old.is_none());
        assert!(fresh_new.is_none());
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session),
                Some(400),
                Some("startup".into()),
            )
            .expect("fresh session should activate the buffered report");
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    /// The closing-block reporter must outrank the screen scraper.
    ///
    /// When a Claude turn ends, `manifests/claude.toml` `live_prompt_box` sees the
    /// `❯` box and calls the pane idle. That is right about the harness and wrong
    /// about the work: agents may still be running, or a Gate may be waiting on a
    /// human. The `Stop` hook knows both, so its report has to win.
    #[test]
    fn closing_block_authority_outranks_visible_idle_prompt_box() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("closing-block.jsonl");
        let now = Instant::now();

        // A real claude process is in the pane -- the precondition every
        // full-lifecycle source must satisfy.
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        // The pane's session identity is already owned by claude's own
        // integration, as it is on any pane running a real agent. The
        // closing-block source must report *alongside* that owner, not fight it.
        terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(999),
            Some("startup".into()),
        );

        // Exactly what the RPC produces: no session ref. `session_ref_from_report`
        // mints one only for official sources, so a closing-block report always
        // arrives with `None`. An earlier version of this test passed `Some(..)`
        // here and stayed green while the live path silently dropped every report.
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            Some("Gate 1: merge #30".into()),
            None,
            Some(1000),
            now,
        );

        // Screen scraping now reports the idle prompt box. It must not win.
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_millis(1),
        );
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(!terminal.hook_authority_output_retirement_eligible());
        assert_eq!(
            terminal.state,
            AgentState::Blocked,
            "visible idle prompt box must not clear a reported Gate"
        );

        // And once the process is gone, the stale authority must not outlive it.
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            true,
            now + Duration::from_millis(2),
        );
        assert_ne!(terminal.state, AgentState::Blocked);
    }

    /// A finished closing report describes the whole parent turn. A lingering
    /// child after the idle prompt is not evidence that the parent resumed.
    #[test]
    fn a_finished_closing_report_stays_idle_with_a_lingering_child() {
        let mut terminal = test_terminal();
        let now = Instant::now();

        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::path(test_session_path("subprocess.jsonl")),
            Some(999),
            Some("startup".into()),
        );
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        assert_eq!(terminal.state, AgentState::Idle);

        terminal.set_foreground_process(Some("codex".into()), true, now);
        assert_eq!(
            terminal.state,
            AgentState::Idle,
            "the finished turn-end report must outrank a lingering child"
        );

        terminal.set_foreground_process(None, false, now);
        assert_eq!(
            terminal.state,
            AgentState::Idle,
            "the reported idle stands once the sub-process tree is quiet"
        );
    }

    #[test]
    fn codex_idle_screen_outranks_a_persistent_helper_process() {
        let mut terminal = test_terminal();
        let now = Instant::now();

        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_foreground_process(Some("sh".into()), true, now);

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(terminal.effective_state_arbitration(), "screen");
    }

    #[test]
    fn visible_working_after_a_closing_gate_starts_a_new_turn_after_human_input() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1000),
            now,
        );
        terminal.supervisor_stale = true;

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(1),
        );
        let output_at = now + Duration::from_secs(2);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            output_at,
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.hook_authority.as_ref().unwrap().retired_at, None);

        let input_at = now + Duration::from_secs(3);
        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(input_at)
            .expect("explicit input retires the gate");
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(4),
        );
        let turn_started = now + Duration::from_secs(5);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            turn_started,
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert!(!terminal.supervisor_stale);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().retired_at,
            Some(turn_started),
            "the stable visible Working signal retires the answered closing authority"
        );
        assert_eq!(
            terminal.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            turn_started.checked_add(AGENT_BUSY_STALE_SILENCE)
        );
    }

    #[test]
    fn resumed_turn_cannot_inherit_completion_from_the_previous_turn() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        assert_eq!(terminal.sidebar_projection(true), (AgentState::Idle, true));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(3),
        );

        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Idle, true),
            "the resumed turn needs fresh closing evidence before it can be done"
        );
        assert!(terminal
            .metadata_tokens_for_api()
            .get("closing_completion")
            .map(String::as_str)
            .is_none());
    }

    #[test]
    fn resumed_turn_retires_legacy_completion_and_worker_evidence() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let legacy_tokens = HashMap::from([
            ("closing_idle".into(), Some("1".into())),
            ("closing_contract".into(), Some("finish the repair".into())),
            ("closing_contract_met".into(), Some("1".into())),
            ("closing_agents".into(), Some("2".into())),
        ]);
        assert!(terminal.apply_closing_contract_tokens(&legacy_tokens, now,));
        assert!(terminal.metadata_tokens.patch(legacy_tokens, None, now));
        terminal
            .apply_closing_report_subagents_at(Some(2), now)
            .expect("legacy direct worker evidence should be present");
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(3),
        );

        assert!(terminal.closing_contract_met.is_none());
        assert!(terminal.closing_report_subagents.is_none());
        for key in [
            "closing_idle",
            "closing_contract",
            "closing_contract_met",
            "closing_agents",
        ] {
            assert!(terminal.metadata_tokens.get(key).is_none(), "stale {key}");
        }
        assert!(!terminal.closing_task_complete());
        assert!(!terminal.declares_running_subagents());
        assert_eq!(terminal.sidebar_projection(true), (AgentState::Idle, true));
    }

    #[test]
    fn a_retired_report_never_arms_the_watchdog_through_a_child() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        terminal.set_foreground_process(Some("codex".into()), true, now);
        terminal.hook_authority.as_mut().unwrap().retired_at = Some(now + Duration::from_secs(1));

        assert!(terminal
            .hook_authority
            .as_ref()
            .unwrap()
            .retired_at
            .is_some());
        assert!(terminal
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_none());
    }

    /// Builds a pane whose agent has reported idle but is still holding a live
    /// sub-process tree, which is the shape the watchdog has to cover.
    fn subprocess_held_terminal(now: Instant) -> TerminalState {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "custom:watchdog".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        terminal.set_foreground_process(Some("codex".into()), true, now);
        assert_eq!(terminal.state, AgentState::Working);
        terminal
    }

    #[test]
    fn a_subprocess_held_pane_arms_the_status_watchdog() {
        let now = Instant::now();
        let terminal = subprocess_held_terminal(now);

        assert_eq!(
            terminal.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            now.checked_add(TEST_AGENT_STALE_AFTER),
            "work nobody is reporting on must still be watched"
        );

        let mut quiet = test_terminal();
        quiet.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        quiet.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        quiet.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        assert!(
            quiet
                .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
                .is_none(),
            "a pane that is genuinely finished has nothing to wait for"
        );
    }

    #[test]
    fn a_subprocess_held_pane_goes_stale_when_the_silence_runs_out() {
        let now = Instant::now();
        let mut terminal = subprocess_held_terminal(now);

        assert!(terminal
            .mark_agent_status_stale_at(
                now + TEST_AGENT_STALE_AFTER - Duration::from_secs(1),
                TEST_AGENT_STALE_AFTER,
            )
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(now + TEST_AGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER,)
            .is_some());
        assert!(terminal.supervisor_stale);

        // The mark outlives the sub-process tree, so the pane does not quietly
        // settle into done as if the agent had signed off on it.
        terminal.set_foreground_process(None, false, now + TEST_AGENT_STALE_AFTER);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(terminal.status_report_snapshot().3);
    }

    #[test]
    fn a_fresh_report_clears_a_subprocess_held_stale_mark() {
        let now = Instant::now();
        let mut terminal = subprocess_held_terminal(now);
        terminal
            .mark_agent_status_stale_at(now + TEST_AGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER)
            .expect("watchdog should mark the silence stale");
        assert!(terminal.supervisor_stale);

        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1001),
            now + TEST_AGENT_STALE_AFTER + Duration::from_secs(1),
        );
        assert!(!terminal.supervisor_stale);
        assert!(terminal
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_some());
    }

    /// Builds a pane whose parent has ended its turn while declaring that
    /// subagents remain active.
    fn subagent_claim_terminal(now: Instant) -> TerminalState {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1000),
            now,
        );
        terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("claude-subagent-watchdog"),
            Some(999),
            Some("startup".into()),
        );
        terminal
            .apply_closing_report_subagents_at(Some(3), now)
            .expect("observed closing report declares three agents");
        assert_eq!(terminal.state, AgentState::Working);
        terminal
    }

    /// Pins the live active-subagent count as a watchdog claim, while zero stays unarmed.
    #[test]
    fn live_active_subagent_count_arms_watchdog_only_when_positive() {
        let now = Instant::now();
        let mut active = test_terminal();
        active.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        active.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        active.set_active_subagents(Some(1));

        assert!(active
            .metadata_tokens_for_api()
            .get("closing_agents")
            .map(String::as_str)
            .is_none());
        assert!(active.declares_running_subagents());
        assert_eq!(
            active.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            now.checked_add(TEST_SUBAGENT_STALE_AFTER)
        );
        assert!(active
            .mark_agent_status_stale_at(
                now + TEST_SUBAGENT_STALE_AFTER - Duration::from_secs(1),
                TEST_AGENT_STALE_AFTER,
            )
            .is_none());
        assert!(active
            .mark_agent_status_stale_at(now + TEST_SUBAGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER,)
            .is_some());
        assert!(active.supervisor_stale);

        let mut zero = test_terminal();
        zero.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        zero.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        zero.set_active_subagents(Some(0));

        assert!(!zero.declares_running_subagents());
        assert!(zero
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_none());
        assert!(zero
            .mark_agent_status_stale_at(now + TEST_AGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER,)
            .is_none());
        assert!(!zero.supervisor_stale);
    }

    #[test]
    fn live_active_subagent_keeps_an_idle_parent_in_progress() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_active_subagents(Some(1));

        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Working, true)
        );
    }

    #[test]
    fn last_verified_worker_completion_recomputes_the_parent_status() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(true),
            now,
        );
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            Some(1),
            now,
        );
        terminal.set_active_subagents(Some(1));
        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Working, true)
        );

        terminal.supervisor_stale = true;
        terminal.stale_resolution = Some((AgentState::Unknown, true));
        assert!(terminal.set_active_subagents(Some(1)));
        assert!(
            !terminal.supervisor_stale,
            "a live scan reconciles stale state"
        );
        assert!(terminal.set_active_subagents(Some(0)));
        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Idle, true),
            "native zero and explicit completion settle the earlier unknown worker claim"
        );
    }

    #[test]
    fn a_subagent_claim_goes_stale_when_the_silence_runs_out() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);

        assert!(terminal
            .mark_agent_status_stale_at(
                now + TEST_SUBAGENT_STALE_AFTER - Duration::from_secs(1),
                TEST_AGENT_STALE_AFTER,
            )
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(now + TEST_SUBAGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER,)
            .is_some());
        assert!(terminal.supervisor_stale);
    }

    #[test]
    fn transcript_declared_claude_subagent_claim_uses_the_thirty_minute_default_budget() {
        let now = Instant::now();
        let stale_after = Duration::from_secs(30 * 60);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_active_subagents_with_projection_at(Some(3), true, now);
        terminal.set_claude_subagent_transcript_activity_at(SubagentTranscriptActivity::Fresh, now);

        assert_eq!(
            terminal.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            now.checked_add(stale_after)
        );
        assert!(terminal
            .mark_agent_status_stale_at(
                now + stale_after - Duration::from_secs(1),
                TEST_AGENT_STALE_AFTER,
            )
            .is_none());
        assert_eq!(terminal.effective_active_subagents(), Some(3));

        assert!(terminal
            .mark_agent_status_stale_at(now + stale_after, TEST_AGENT_STALE_AFTER)
            .is_some());
        assert!(terminal.supervisor_stale);
        assert_eq!(terminal.active_subagents, Some(3));
    }

    #[test]
    fn unreconciled_reported_worker_claim_is_unknown_not_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1000),
            now,
        );
        terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("claude-unverified-worker-claim"),
            Some(999),
            Some("startup".into()),
        );
        assert!(terminal.metadata_tokens.patch(
            HashMap::from([("closing_agents".into(), Some("3".into()))]),
            None,
            now,
        ));
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(true),
            now,
        );

        assert_eq!(
            terminal.sidebar_projection(false),
            (AgentState::Unknown, false)
        );
    }

    #[test]
    fn a_subagent_closing_report_waits_through_an_idle_screen_on_the_long_budget() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(1),
        );
        terminal.set_foreground_process(Some("codex".into()), true, now + Duration::from_secs(1));

        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.waiting_on_agents());
        assert_eq!(
            terminal.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                Duration::from_secs(60 * 60),
                Some(now),
            ),
            now.checked_add(Duration::from_secs(60 * 60))
        );
    }

    #[test]
    fn a_fresh_subagent_transcript_keeps_the_long_watchdog_budget() {
        let now = Instant::now();
        let witness_at = now + Duration::from_secs(4 * 60);
        let mut terminal = subagent_claim_terminal(now);
        terminal.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Fresh,
            witness_at,
        );

        assert_eq!(
            terminal.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            ),
            witness_at.checked_add(TEST_SUBAGENT_STALE_AFTER)
        );
    }

    #[test]
    fn a_fresh_transcript_observed_subagent_gets_the_long_watchdog_budget_without_a_hook() {
        let now = Instant::now();
        let witness_at = now + Duration::from_secs(4 * 60);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_active_subagents_with_projection_at(Some(2), true, now);
        terminal.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Fresh,
            witness_at,
        );

        assert!(terminal.hook_authority.is_none());
        assert!(!terminal.waiting_on_agents());
        assert_eq!(
            terminal.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            ),
            witness_at.checked_add(TEST_SUBAGENT_STALE_AFTER)
        );
    }

    #[test]
    fn stale_subagent_transcripts_fall_back_to_the_ordinary_budget() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        terminal.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Stale,
            now + Duration::from_secs(4 * 60),
        );

        assert_eq!(
            terminal.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            ),
            now.checked_add(TEST_AGENT_STALE_AFTER)
        );
        assert!(terminal
            .mark_agent_status_stale_at_with_activity(
                now + TEST_AGENT_STALE_AFTER,
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            )
            .is_some());
        assert!(terminal.supervisor_stale);
    }

    #[test]
    fn a_stale_transcript_observed_subagent_keeps_the_ordinary_watchdog_budget() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.unreported_turn_started_at = Some(now);
        terminal.set_active_subagents_with_projection_at(Some(2), true, now);
        terminal.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Stale,
            now + Duration::from_secs(4 * 60),
        );

        assert!(terminal.hook_authority.is_none());
        assert!(!terminal.waiting_on_agents());
        assert_eq!(
            terminal.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            ),
            now.checked_add(AGENT_BUSY_STALE_SILENCE)
        );
    }

    #[test]
    fn every_fresh_transcript_observed_subagent_has_a_watchdog_except_documented_exits() {
        fn transcript_observed_terminal(now: Instant) -> TerminalState {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.set_active_subagents_with_projection_at(Some(2), true, now);
            terminal
                .set_claude_subagent_transcript_activity_at(SubagentTranscriptActivity::Fresh, now);
            terminal
        }

        fn assert_watchdog_invariant(terminal: &TerminalState) {
            assert!(terminal.active_subagents.is_some_and(|count| count > 0));
            let documented_exit = terminal.supervisor_stale
                || terminal.state == AgentState::Blocked
                || terminal.closing_external_wait().is_some();
            assert_eq!(
                terminal
                    .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
                    .is_none(),
                documented_exit
            );
        }

        let now = Instant::now();
        let live = transcript_observed_terminal(now);
        assert_watchdog_invariant(&live);

        let mut stale = transcript_observed_terminal(now);
        stale.supervisor_stale = true;
        assert_watchdog_invariant(&stale);

        let mut blocked = transcript_observed_terminal(now);
        blocked.state = AgentState::Blocked;
        assert_watchdog_invariant(&blocked);

        let mut external_wait = transcript_observed_terminal(now);
        external_wait.apply_closing_task_report(
            None,
            Some("CI".into()),
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        assert_watchdog_invariant(&external_wait);
    }

    #[test]
    fn stale_evidence_does_not_restart_the_parent_silence_clock() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        let witness_at = now + Duration::from_secs(4 * 60);
        terminal.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Fresh,
            witness_at,
        );
        terminal.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Stale,
            witness_at + Duration::from_secs(1),
        );

        assert_eq!(
            terminal.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            ),
            now.checked_add(TEST_AGENT_STALE_AFTER)
        );
        assert_eq!(terminal.effective_active_subagents(), Some(3));
        assert!(!terminal.supervisor_stale);
    }

    #[test]
    fn unknown_transcript_evidence_cannot_change_an_existing_budget_decision() {
        let now = Instant::now();
        let witness_at = now + Duration::from_secs(4 * 60);
        let mut fresh = subagent_claim_terminal(now);
        fresh.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Fresh,
            witness_at,
        );
        assert!(!fresh.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Unknown,
            witness_at + Duration::from_secs(1),
        ));
        assert_eq!(
            fresh.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            ),
            witness_at.checked_add(TEST_SUBAGENT_STALE_AFTER)
        );

        let mut stale = subagent_claim_terminal(now);
        stale.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Stale,
            witness_at,
        );
        assert!(!stale.set_claude_subagent_transcript_activity_at(
            SubagentTranscriptActivity::Unknown,
            witness_at + Duration::from_secs(1),
        ));
        assert_eq!(
            stale.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                TEST_SUBAGENT_STALE_AFTER,
                None,
            ),
            now.checked_add(TEST_AGENT_STALE_AFTER)
        );
    }

    #[test]
    fn pane_output_restarts_the_subagent_wait_budget() {
        let now = Instant::now();
        let terminal = subagent_claim_terminal(now);
        let activity_at = now + Duration::from_secs(10 * 60);

        assert_eq!(
            terminal.agent_status_watchdog_deadline_with_activity(
                TEST_AGENT_STALE_AFTER,
                Duration::from_secs(60 * 60),
                Some(activity_at),
            ),
            activity_at.checked_add(Duration::from_secs(60 * 60))
        );
    }

    #[test]
    fn an_omitted_agent_count_never_reads_the_legacy_metadata_mirror() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        terminal
            .apply_closing_report_subagents_at(Some(0), now + Duration::from_secs(1))
            .expect("explicit zero overrides the previous declaration");
        assert!(terminal.metadata_tokens.patch(
            HashMap::from([("closing_agents".into(), Some("4".into()))]),
            None,
            now + Duration::from_secs(2),
        ));
        assert_eq!(terminal.effective_active_subagents(), Some(0));

        terminal
            .apply_closing_report_subagents_at(None, now + Duration::from_secs(3))
            .expect("legacy report omits the direct count");

        assert_eq!(terminal.effective_active_subagents(), None);
        assert!(!terminal.waiting_on_agents());
    }

    #[test]
    fn a_later_closing_report_without_agents_ends_the_wait() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        assert!(terminal.waiting_on_agents());
        terminal.apply_closing_task_report(
            None,
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now + Duration::from_secs(1),
        );

        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1001),
            now + Duration::from_secs(1),
        );
        terminal
            .apply_closing_report_subagents_at(Some(0), now + Duration::from_secs(1))
            .expect("later closing report declares no agents");

        assert!(!terminal.waiting_on_agents());
        assert_eq!(terminal.state, AgentState::Unknown);
        assert!(terminal
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_none());
    }

    #[test]
    fn an_idle_screen_clears_a_stale_working_report() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1000),
            now,
        );
        terminal
            .mark_agent_status_stale_at(now + AGENT_BUSY_STALE_SILENCE, TEST_AGENT_STALE_AFTER)
            .expect("silent report becomes stale");

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + AGENT_BUSY_STALE_SILENCE + Duration::from_secs(1),
        );

        assert!(!terminal.supervisor_stale);
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn a_blocked_screen_clears_a_stale_working_report() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1000),
            now,
        );
        terminal
            .mark_agent_status_stale_at(now + AGENT_BUSY_STALE_SILENCE, TEST_AGENT_STALE_AFTER)
            .expect("silent report becomes stale");

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Blocked,
            true,
            false,
            false,
            false,
            false,
            now + AGENT_BUSY_STALE_SILENCE + Duration::from_secs(1),
        );

        assert!(!terminal.supervisor_stale);
        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn a_repeated_idle_process_projection_clears_a_later_stale_mark() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        let (changed, mutation) =
            terminal.set_process_state(false, Some((AgentState::Idle, false)), now);
        assert!(changed);
        assert!(mutation.is_none());
        terminal.supervisor_stale = true;

        let (changed, mutation) = terminal.set_process_state(
            false,
            Some((AgentState::Idle, false)),
            now + Duration::from_secs(1),
        );
        assert!(changed);
        assert!(mutation.is_some());
        assert!(!terminal.supervisor_stale);
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn an_old_process_sample_cannot_resolve_a_newer_stale_report() {
        let old_observation = Instant::now();
        let reported_at = old_observation + Duration::from_secs(1);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1000),
            reported_at,
        );
        terminal.supervisor_stale = true;

        let (changed, mutation) =
            terminal.set_process_state(false, Some((AgentState::Idle, false)), old_observation);

        assert!(!changed);
        assert!(mutation.is_none());
        assert!(terminal.supervisor_stale);
        assert_eq!(terminal.raw_agent_state(), AgentState::Working);
        assert_eq!(terminal.hook_authority.as_ref().unwrap().retired_at, None);
        assert_eq!(terminal.stale_resolution, None);
    }

    #[test]
    fn a_process_idle_baseline_then_visible_working_arms_a_new_turn_watchdog() {
        let reported_at = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1000),
            reported_at,
        );
        terminal
            .mark_agent_status_stale_at(
                reported_at + AGENT_BUSY_STALE_SILENCE,
                TEST_AGENT_STALE_AFTER,
            )
            .expect("silent report becomes stale");

        let baseline_at = reported_at + AGENT_BUSY_STALE_SILENCE + Duration::from_secs(1);
        let (_, mutation) =
            terminal.set_process_state(false, Some((AgentState::Idle, false)), baseline_at);
        assert!(mutation.is_some());
        assert!(!terminal.supervisor_stale);

        let turn_started_at = baseline_at + Duration::from_secs(1);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            turn_started_at,
        );

        assert_eq!(terminal.raw_agent_state(), AgentState::Working);
        assert_eq!(
            terminal.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            turn_started_at.checked_add(AGENT_BUSY_STALE_SILENCE)
        );
    }

    #[test]
    fn repeated_working_report_does_not_reset_working_since() {
        let started = Instant::now();
        let refreshed = started + Duration::from_secs(90);
        let mut terminal = test_terminal();

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            false,
            false,
            false,
            started,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            false,
            false,
            false,
            refreshed,
        );

        assert_eq!(terminal.working_since(), Some(started));
        assert_eq!(
            refreshed.saturating_duration_since(
                terminal.working_since().expect("working start timestamp")
            ),
            Duration::from_secs(90)
        );
    }

    #[test]
    fn a_blocked_closing_gate_with_subagents_never_arms_the_stale_watchdog() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1000),
            now,
        );
        assert!(terminal.metadata_tokens.patch(
            HashMap::from([("closing_agents".into(), Some("1".into()))]),
            None,
            now,
        ));

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(now + TEST_SUBAGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER)
            .is_none());
        assert!(!terminal.supervisor_stale);
    }

    #[test]
    fn blocked_lifecycle_outranks_external_wait_without_structured_gate() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            Some("CI run 4123".into()),
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1000),
            now,
        );

        assert_eq!(terminal.raw_agent_state(), AgentState::Blocked);
        assert_eq!(terminal.sidebar_projection(true).0, AgentState::Blocked);
    }

    #[test]
    fn pending_human_input_outranks_external_wait_on_an_idle_screen() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Incomplete),
            Some("CI run 4123".into()),
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );

        assert_eq!(
            terminal.sidebar_projection_with_pending_human_input(false, true),
            (AgentState::Blocked, false)
        );
        assert_eq!(
            terminal.sidebar_projection_with_pending_human_input(false, false),
            (AgentState::Working, false)
        );
    }

    /// Pins that a stale unverified subagent claim blocks the armed Done auto-settle trigger.
    #[test]
    fn a_stale_subagent_claim_never_becomes_done_or_auto_settles() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        terminal
            .mark_agent_status_stale_at(now + TEST_SUBAGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER)
            .expect("watchdog should mark the subagent claim stale");
        assert!(terminal.supervisor_stale);
        assert!(crate::app::settled::pane_has_resume_plan(&terminal));

        let active_subagents = terminal
            .metadata_tokens
            .get("closing_agents")
            .and_then(|value| value.parse::<u32>().ok());
        assert!(!session_is_quiet(
            terminal.state,
            false,
            active_subagents,
            terminal.holds_shell,
        ));
        assert_eq!(
            derive_completion_tier(
                terminal.state,
                terminal.closing_contract.as_deref(),
                terminal.closing_contract_met,
                terminal.closing_idle,
                false,
                active_subagents,
                terminal.holds_shell,
                true,
            ),
            None
        );

        let mut state = AppState::test_new();
        let workspace = Workspace::test_new("stale-subagent-claim");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .pane_state(pane_id)
            .expect("root pane")
            .attached_terminal_id
            .clone();
        terminal.id = terminal_id.clone();
        state.terminals.insert(terminal_id, terminal);
        state.workspaces.push(workspace);
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        state.settle_done_after = Duration::ZERO;
        let pane = state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.seen = false;
        pane.done_since = Some(now);

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_000), 0);
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn an_expired_adapter_wait_never_suppresses_a_live_native_subagent_count() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        terminal.set_active_subagents(Some(3));
        terminal
            .mark_agent_status_stale_at(now + TEST_SUBAGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER)
            .expect("watchdog expires the subagent wait");
        assert_eq!(terminal.effective_active_subagents(), Some(3));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + TEST_SUBAGENT_STALE_AFTER + Duration::from_secs(1),
        );

        assert!(!terminal.supervisor_stale);
        assert_eq!(terminal.effective_active_subagents(), Some(3));
        assert!(!session_is_quiet(
            terminal.state,
            false,
            terminal.effective_active_subagents(),
            terminal.holds_shell,
        ));
    }

    #[test]
    fn a_fresh_report_resets_the_subagent_claim_watchdog() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        terminal
            .mark_agent_status_stale_at(now + TEST_SUBAGENT_STALE_AFTER, TEST_AGENT_STALE_AFTER)
            .expect("watchdog should mark the subagent claim stale");

        let refreshed_at = now + TEST_SUBAGENT_STALE_AFTER + Duration::from_secs(1);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1001),
            refreshed_at,
        );
        terminal
            .apply_closing_report_subagents_at(Some(3), refreshed_at)
            .expect("fresh report restores its direct subagent count");

        assert!(!terminal.supervisor_stale);
        assert_eq!(
            terminal.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            refreshed_at.checked_add(TEST_SUBAGENT_STALE_AFTER)
        );
    }

    #[test]
    fn a_retired_direct_subagent_claim_keeps_its_watchdog_until_expiry() {
        let reported_at = Instant::now();
        let mut terminal = subagent_claim_terminal(reported_at);

        let settled_at = reported_at + Duration::from_secs(1);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            settled_at,
        );
        let retired_at = settled_at + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH;
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            retired_at,
        );

        assert_eq!(terminal.verified_active_subagents(), Some(3));
        assert_eq!(terminal.effective_active_subagents(), Some(3));
        assert!(terminal.declares_running_subagents());
        assert!(terminal.waiting_on_agents());
        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Working, true)
        );
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().retired_at,
            Some(retired_at)
        );
        assert_eq!(
            terminal.agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER),
            retired_at.checked_add(TEST_SUBAGENT_STALE_AFTER)
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            retired_at + Duration::from_secs(1),
        );
        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Working, true)
        );

        terminal
            .mark_agent_status_stale_at(
                retired_at + TEST_SUBAGENT_STALE_AFTER,
                TEST_AGENT_STALE_AFTER,
            )
            .expect("watchdog expires the retired direct claim");

        assert_eq!(terminal.effective_active_subagents(), Some(0));
        assert!(!terminal.declares_running_subagents());
        assert!(!terminal.waiting_on_agents());
        assert!(!terminal.supervisor_stale);
        assert_eq!(terminal.sidebar_projection(true), (AgentState::Idle, true));
        assert!(terminal
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_none());
    }

    #[test]
    fn every_running_declaration_has_a_watchdog_except_documented_exits() {
        fn assert_watchdog_invariant(terminal: &TerminalState) {
            assert!(terminal.declares_running_subagents());
            let authority = terminal
                .hook_authority
                .as_ref()
                .expect("direct declaration has hook authority");
            let documented_exit = terminal.supervisor_stale
                || terminal.state == AgentState::Blocked
                || terminal.closing_external_wait().is_some()
                || !terminal.hook_authority_is_effective(authority);
            assert_eq!(
                terminal
                    .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
                    .is_none(),
                documented_exit
            );
        }

        let now = Instant::now();
        let live = subagent_claim_terminal(now);
        assert_watchdog_invariant(&live);

        let mut stale = subagent_claim_terminal(now);
        stale.supervisor_stale = true;
        assert_watchdog_invariant(&stale);

        let mut blocked = subagent_claim_terminal(now);
        blocked.state = AgentState::Blocked;
        assert_watchdog_invariant(&blocked);

        let mut external_wait = subagent_claim_terminal(now);
        external_wait.apply_closing_task_report(
            None,
            Some("CI".into()),
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            now,
        );
        assert_watchdog_invariant(&external_wait);

        let mut ineffective = subagent_claim_terminal(now);
        ineffective.recent_agent_process_exit = Some(RecentAgentProcessExit {
            agent: Agent::Claude,
            observed_at: now,
        });
        assert_watchdog_invariant(&ineffective);
    }

    #[test]
    fn a_non_working_authority_without_a_subagent_claim_has_no_watchdog() {
        let now = Instant::now();
        let mut terminal = subagent_claim_terminal(now);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1001),
            now + Duration::from_secs(1),
        );
        terminal
            .apply_closing_report_subagents_at(Some(0), now + Duration::from_secs(1))
            .expect("later closing report declares no agents");

        assert!(terminal
            .agent_status_watchdog_deadline(TEST_AGENT_STALE_AFTER)
            .is_none());
    }

    #[test]
    fn rapid_restart_replays_reports_that_arrive_before_process_evidence() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("reports-before-process-evidence.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let lower_sequence = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(1001),
            now + Duration::from_millis(2),
        );
        let missing_sequence = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            None,
            now + Duration::from_millis(3),
        );
        let buffered_working = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(2001),
            now + Duration::from_millis(4),
        );
        let startup = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(2000),
            Some("startup".into()),
        );
        assert!(startup.is_none());
        assert!(lower_sequence.is_none());
        assert!(missing_sequence.is_none());
        assert!(buffered_working.is_none());
        assert!(!terminal.full_lifecycle_hook_authority_active());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_millis(5),
        );

        assert!(terminal.full_lifecycle_hook_authority_active());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn process_exit_discards_unclaimed_buffered_state_from_that_generation() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("buffered-exit-old.jsonl");
        let shared_session = test_session_path("buffered-exit-shared.jsonl");
        let now = Instant::now();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(old_session.clone()).unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_millis(2),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(shared_session.clone()),
            Some(500),
            now + Duration::from_millis(3),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_millis(4),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_millis(5),
        );
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(shared_session),
                Some(100),
                Some("startup".into()),
            )
            .expect("new generation session claim");

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn queued_fresh_process_evidence_uses_process_exit_observation_time() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("queued-after-process-exit.jsonl");
        let process_exit_at = Instant::now() - Duration::from_secs(1);
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(1000),
            process_exit_at - Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            process_exit_at,
        );

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            process_exit_at + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            process_exit_at + Duration::from_millis(2),
        );
        let startup = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(2000),
            Some("startup".into()),
        );

        assert!(startup.is_some());
    }

    #[test]
    fn different_session_after_process_exit_waits_for_fresh_process_evidence() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-before-process-exit.jsonl");
        let new_session = test_session_path("new-after-process-exit.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let early_new = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(500),
            now + Duration::from_millis(2),
        );

        assert!(early_new.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(4),
        );
        let fresh_new = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(new_session),
            Some(400),
            Some("startup".into()),
        );

        assert!(fresh_new.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn missing_session_after_process_exit_waits_for_fresh_process_evidence() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-before-nosession-process-exit.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let early_without_session = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(500),
            now + Duration::from_millis(2),
        );

        assert!(early_without_session.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(4),
        );
        let fresh_without_session = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(500),
            now + Duration::from_millis(5),
        );
        assert!(fresh_without_session.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(test_session_path(
                    "fresh-after-nosession-process-exit.jsonl",
                )),
                Some(600),
                Some("startup".into()),
            )
            .expect("fresh root session should claim the process generation");
        let child_update = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(601),
            now + Duration::from_millis(6),
        );

        assert!(child_update.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn mastracode_session_start_replaces_current_root_session() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Mastracode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:mastracode".into(),
                "mastracode".into(),
                crate::agent_resume::AgentSessionRef::id("mastracode-old"),
                Some(20),
                Some("startup".into()),
            )
            .expect("initial root session");

        let replacement = terminal.set_agent_session_ref_for_session_start(
            "herdr:mastracode".into(),
            "mastracode".into(),
            crate::agent_resume::AgentSessionRef::id("mastracode-new"),
            Some(21),
            Some("startup".into()),
        );

        assert!(replacement.is_some_and(|mutation| mutation.session_ref_changed));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("mastracode-new")
        );
    }

    #[test]
    fn omp_reacquires_full_lifecycle_hook_after_process_exit_with_fresh_process_and_session_ref() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Omp), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-old"),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Omp),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let stale = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-old"),
            Some(500),
        );
        assert!(stale.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Omp),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:omp".into(),
                "omp".into(),
                crate::agent_resume::AgentSessionRef::id("omp-new"),
                Some(400),
                Some("startup".into()),
            )
            .expect("fresh process and session should claim the pane");
        let fresh = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-new"),
            Some(500),
        );

        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn screen_blocker_does_not_create_a_persistent_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Blocked,
            true,
            false,
            false,
            false,
            false,
            observed,
        );
        assert!(terminal.blocked_state_hold.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(terminal.blocked_state_hold.is_none());
    }

    #[test]
    fn transient_idle_screen_does_not_clear_hook_blocked_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            observed,
        );
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn blocked_hold_releases_when_a_different_agent_takes_over() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Grok),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.blocked_state_hold.is_none());
    }

    #[test]
    fn blocked_hold_releases_when_a_different_hook_agent_takes_over() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_hook_authority_at(
            "custom:grok".into(),
            "grok".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            observed + Duration::from_secs(2),
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.blocked_state_hold.is_none());
    }

    #[test]
    fn blocked_hold_releases_when_the_agent_session_changes() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Mastracode), AgentState::Idle);
        let old_session = crate::agent_resume::AgentSessionRef::id("blocked-hold-old-session")
            .expect("session id should be valid");
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:mastracode".into(),
                "mastracode".into(),
                Some(old_session.clone()),
                Some(1),
                Some("startup".into()),
            )
            .expect("initial session should be accepted");
        terminal.set_hook_authority_at(
            "herdr:mastracode".into(),
            "mastracode".into(),
            AgentState::Blocked,
            None,
            Some(old_session),
            Some(2),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        let replacement = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:mastracode".into(),
                "mastracode".into(),
                crate::agent_resume::AgentSessionRef::id("blocked-hold-new-session"),
                Some(3),
                Some("startup".into()),
            )
            .expect("replacement session should be accepted");

        assert!(replacement.session_replaced);
        assert!(terminal.blocked_state_hold.is_none());
        assert_ne!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn custom_hook_idle_clears_its_blocked_report() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            observed,
        );
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.blocked_state_hold.is_none());

        terminal.set_hook_authority_at(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
            Some(2),
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(terminal.blocked_state_hold.is_none());
    }

    #[test]
    fn transient_idle_hook_report_does_not_clear_hook_blocked_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(2),
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn idle_agent_report_does_not_clear_a_closing_block_report() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn explicit_input_releases_full_lifecycle_blocked_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("pi-explicit-input").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref.clone()),
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(observed + Duration::from_secs(1))
            .expect("input releases the full-lifecycle blocked hold");

        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn explicit_input_releases_hook_hold_but_preserves_visible_screen_blocker() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("pi-visible-blocker").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            observed,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Blocked,
            true,
            false,
            false,
            false,
            false,
            observed + Duration::from_millis(1),
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(observed + Duration::from_secs(1))
            .expect("input releases the full-lifecycle blocked hold");

        assert!(terminal.blocked_state_hold.is_none());
        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert!(terminal.fallback_visible_blocker);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.effective_state_arbitration(), "screen");
    }

    #[test]
    fn working_report_releases_full_lifecycle_blocked_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("pi-working-report").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref.clone()),
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(session_ref.clone()),
            Some(2),
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_working_releases_same_owner_full_lifecycle_blocked_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("pi-visible-working").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            observed + Duration::from_millis(1),
        );

        let working_at = observed + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH;
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            working_at,
        );

        assert!(terminal.blocked_state_hold.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().retired_at,
            Some(working_at)
        );
    }

    #[test]
    fn continuous_visible_working_keeps_same_owner_full_lifecycle_blocked_hold() {
        let observed = Instant::now() + Duration::from_secs(10);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref =
            crate::agent_resume::AgentSessionRef::id("pi-continuous-visible-working").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed - Duration::from_millis(1),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        for working_at in [
            observed + Duration::from_millis(1),
            observed + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH / 2,
            observed + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH + Duration::from_millis(1),
        ] {
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Working,
                false,
                false,
                true,
                false,
                false,
                working_at,
            );
        }

        assert!(terminal.blocked_state_hold.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn recent_visible_working_does_not_release_full_lifecycle_blocked_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref =
            crate::agent_resume::AgentSessionRef::id("pi-recent-visible-working").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed + Duration::from_millis(1),
        );

        assert!(terminal.blocked_state_hold.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn continuous_visible_working_after_full_lifecycle_horizon_keeps_blocked_hold() {
        let reported_at = Instant::now() + Duration::from_secs(10);
        let timeout = Duration::from_secs(10 * 60);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref =
            crate::agent_resume::AgentSessionRef::id("pi-working-after-block").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            reported_at - Duration::from_millis(1),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            reported_at,
        );
        assert!(terminal.blocked_state_hold.is_some());

        let after_horizon = reported_at + timeout + Duration::from_secs(1);
        terminal
            .expire_full_lifecycle_hook_authority_at(after_horizon, timeout)
            .expect("full-lifecycle authority should expire");
        for working_at in [
            after_horizon + Duration::from_millis(1),
            after_horizon + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH + Duration::from_millis(1),
        ] {
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Working,
                false,
                false,
                true,
                false,
                false,
                working_at,
            );
        }

        assert!(terminal.blocked_state_hold.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.effective_state_arbitration(), "blocked_hold");
    }

    #[test]
    fn continuous_visible_working_without_a_settled_frame_keeps_closing_block_hold() {
        let reported_at = Instant::now() + Duration::from_secs(10);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            reported_at - Duration::from_millis(1),
        );
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            reported_at,
        );
        assert!(terminal.blocked_state_hold.is_some());

        for working_at in [
            reported_at + Duration::from_millis(1),
            reported_at + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH + Duration::from_millis(1),
        ] {
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Claude),
                AgentState::Working,
                false,
                false,
                true,
                false,
                false,
                working_at,
            );
        }

        assert!(terminal.blocked_state_hold.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn settled_screen_then_visible_working_releases_hold_after_full_lifecycle_horizon() {
        let reported_at = Instant::now() + Duration::from_secs(10);
        let timeout = Duration::from_secs(10 * 60);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref =
            crate::agent_resume::AgentSessionRef::id("pi-settled-after-block").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            reported_at,
        );

        let after_horizon = reported_at + timeout + Duration::from_secs(1);
        terminal
            .expire_full_lifecycle_hook_authority_at(after_horizon, timeout)
            .expect("full-lifecycle authority should expire");
        let settled_at = after_horizon + Duration::from_secs(1);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            settled_at,
        );
        let working_at = settled_at + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH;
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            working_at,
        );

        assert!(terminal.blocked_state_hold.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn newer_empty_closing_report_clears_gate_after_human_input() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(2),
            observed + Duration::from_secs(1),
        );
        assert_eq!(terminal.state, AgentState::Blocked);

        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(observed + Duration::from_secs(2))
            .expect("human input answers the gate");
        terminal.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());
        terminal.recompute_effective_state_from_current_at(observed + Duration::from_secs(1));

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(!terminal.has_pending_human_input());
    }

    #[test]
    fn process_exit_releases_full_lifecycle_blocked_hold() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("pi-process-exit").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref.clone()),
            Some(1),
            observed,
        );
        assert!(terminal.blocked_state_hold.is_some());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            observed + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn visible_blocker_overrides_non_blocked_hook_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(change.unwrap().previous_state, AgentState::Working);
    }

    #[test]
    fn visible_blocker_overrides_full_lifecycle_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Pi),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(change.is_some());
    }

    #[test]
    fn weak_blocked_fallback_does_not_override_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            false,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn hook_blocked_wins_over_visible_blocker() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn activity_retires_a_blocked_hook_and_returns_to_screen_state() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );
        assert_eq!(terminal.state, AgentState::Blocked);

        let mutation = terminal
            .retire_blocked_full_lifecycle_hook_authority_at(
                observed + std::time::Duration::from_secs(1),
            )
            .expect("activity should retire the blocked report");

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(
            mutation
                .effective_state_change
                .expect("retirement changes the effective state")
                .state,
            AgentState::Idle
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed + std::time::Duration::from_secs(2),
        );
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn accepted_resumed_turn_is_immediately_working_with_an_unrelated_cap_pending() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 2,
                label: "Answer".into(),
                text: "Choose the independent release lane".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );

        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(observed + Duration::from_millis(1))
            .expect("submitted input retires the old blocked report");
        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Blocked, true),
            "queued input is not an accepted resumed turn"
        );

        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(8),
            observed + Duration::from_millis(2),
        );

        assert_eq!(
            terminal.sidebar_projection(true),
            (AgentState::Working, true)
        );
        assert_eq!(terminal.closing_items().len(), 1);
    }

    #[test]
    fn a_blocked_closing_block_report_survives_unavailable_screen_detection() {
        // Turn end on a claude pane: the prompt box matches no detection rule,
        // so the screen scan yields no agent while a stale spinner title still
        // reads Working. The gate must outrank that.
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed,
        );
        assert_eq!(terminal.detected_agent, None);

        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed + std::time::Duration::from_millis(10),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(!terminal.hook_authority_output_retirement_eligible());
        assert_eq!(terminal.effective_agent_label(), Some("claude"));
    }

    #[test]
    fn a_fresh_closing_block_report_only_ends_a_gate_after_input_without_screen_detection() {
        // The claude prompt box matches no detection rule, so human input is the
        // only event that can answer this gate before its next report.
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed + std::time::Duration::from_millis(10),
        );
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.detected_agent, None);

        terminal.apply_closing_task_report(
            Some(crate::api::schema::ClosingCompletion::Complete),
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            observed + std::time::Duration::from_secs(30),
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(8),
            observed + std::time::Duration::from_secs(30),
        );

        assert_eq!(terminal.state, AgentState::Blocked);

        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(
                observed + std::time::Duration::from_secs(31),
            )
            .expect("human input answers the gate");
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(9),
            observed + std::time::Duration::from_secs(32),
        );
        terminal.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(terminal.detected_agent, None);
    }

    #[test]
    fn a_working_closing_block_report_only_ends_a_gate_after_input_without_screen_detection() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(3),
            observed + std::time::Duration::from_millis(10),
        );
        assert_eq!(terminal.state, AgentState::Blocked);

        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(4),
            observed + std::time::Duration::from_secs(30),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(
                observed + std::time::Duration::from_secs(31),
            )
            .expect("human input answers the gate");
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(5),
            observed + std::time::Duration::from_secs(32),
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    fn test_closing_item(n: u32, label: &str, text: &str) -> crate::api::schema::ClosingBlockItem {
        crate::api::schema::ClosingBlockItem {
            n,
            label: label.into(),
            text: text.into(),
            blocking: true,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }
    }

    fn apply_scoped_closing_report(
        terminal: &mut TerminalState,
        state: AgentState,
        session_id: &str,
        turn_seq: u64,
        observed_at: Instant,
        gates: Vec<crate::api::schema::ClosingBlockItem>,
    ) {
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            state,
            None,
            None,
            Some(turn_seq),
            observed_at,
        );
        terminal.set_closing_report_scope(
            "herdr:claude-closing-block".into(),
            Some(session_id.into()),
            Some(turn_seq),
        );
        terminal.apply_closing_block_payload(gates, Vec::new(), Vec::new());
    }

    #[test]
    fn newer_empty_report_from_same_session_answers_latched_gate() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        apply_scoped_closing_report(
            &mut terminal,
            AgentState::Blocked,
            "session-a",
            1,
            now,
            vec![test_closing_item(1, "Approve", "Choose the release path")],
        );
        assert!(terminal
            .closing_report
            .as_ref()
            .unwrap()
            .unanswered
            .is_some());
        let owner = terminal.blocked_hold_owner_for_agent(Agent::Claude);
        terminal.hold_blocked_state_for(owner, now);
        assert!(terminal.blocked_state_hold.is_some());

        apply_scoped_closing_report(
            &mut terminal,
            AgentState::Idle,
            "session-a",
            2,
            now + Duration::from_secs(1),
            Vec::new(),
        );
        terminal.recompute_effective_state_from_current_at(now + Duration::from_secs(1));

        assert!(terminal
            .closing_report
            .as_ref()
            .unwrap()
            .unanswered
            .is_none());
        assert!(terminal.closing_gates.is_empty());
        assert!(terminal.closing_items.is_empty());
        assert!(terminal.blocked_state_hold.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn empty_report_from_another_session_does_not_answer_latched_gate() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let gate = test_closing_item(1, "Approve", "Choose the release path");
        apply_scoped_closing_report(
            &mut terminal,
            AgentState::Blocked,
            "session-a",
            1,
            now,
            vec![gate.clone()],
        );
        let owner = terminal.blocked_hold_owner_for_agent(Agent::Claude);
        terminal.hold_blocked_state_for(owner, now);

        apply_scoped_closing_report(
            &mut terminal,
            AgentState::Idle,
            "session-b",
            2,
            now + Duration::from_secs(1),
            Vec::new(),
        );

        assert!(terminal
            .closing_report
            .as_ref()
            .unwrap()
            .unanswered
            .is_some());
        assert_eq!(terminal.closing_gates, vec![gate]);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.blocked_state_hold.is_some());
    }

    #[test]
    fn newer_report_with_blockers_keeps_latch_and_merges_new_gate() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        let first = test_closing_item(1, "Approve", "Choose the release path");
        let second = test_closing_item(2, "Decide", "Confirm the migration plan");
        apply_scoped_closing_report(
            &mut terminal,
            AgentState::Blocked,
            "session-a",
            1,
            now,
            vec![first.clone()],
        );
        let owner = terminal.blocked_hold_owner_for_agent(Agent::Claude);
        terminal.hold_blocked_state_for(owner, now);

        apply_scoped_closing_report(
            &mut terminal,
            AgentState::Blocked,
            "session-a",
            2,
            now + Duration::from_secs(1),
            vec![second.clone()],
        );

        assert!(terminal
            .closing_report
            .as_ref()
            .unwrap()
            .unanswered
            .is_some());
        assert_eq!(terminal.closing_gates, vec![first, second]);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.blocked_state_hold.is_some());
    }

    #[test]
    fn agent_output_and_empty_closing_reports_keep_unanswered_gate_blocked() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        let gate = test_closing_item(1, "Gate", "Choose the release path");
        terminal.apply_closing_block_payload(vec![gate.clone()], Vec::new(), Vec::new());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(1),
        );
        terminal.clear_closing_task_report(now + Duration::from_millis(1500));
        assert_eq!(terminal.closing_gates, vec![gate.clone()]);
        assert_eq!(terminal.state, AgentState::Blocked);

        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(2),
            now + Duration::from_secs(2),
        );
        terminal.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(3),
            now + Duration::from_secs(3),
        );
        terminal.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());

        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.closing_gates, vec![gate]);
    }

    #[test]
    fn persisted_closing_report_keeps_the_human_latch_until_answered() {
        let now = Instant::now();
        let gate = test_closing_item(1, "Gate", "Choose the release path");
        let mut source = test_terminal();
        source.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        source.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        source.apply_closing_block_payload(vec![gate.clone()], Vec::new(), Vec::new());

        let encoded = serde_json::to_vec(
            &source
                .closing_report_persistence_state(now)
                .expect("blocked closing report should be persisted"),
        )
        .expect("closing report should serialize");
        let saved: ClosingReportHandoffState =
            serde_json::from_slice(&encoded).expect("saved report should deserialize");
        let mut restored = test_terminal();
        restored.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        restored.restore_closing_report_persistence_state(saved, now + Duration::from_secs(1));
        restored.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(2),
            now + Duration::from_secs(1),
        );
        restored.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());

        assert_eq!(restored.closing_gates, vec![gate.clone()]);
        assert_eq!(restored.sidebar_projection(false).0, AgentState::Blocked);

        restored
            .retire_blocked_full_lifecycle_hook_authority_at(now + Duration::from_secs(2))
            .expect("human input should answer the restored gate");
        restored.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(3),
            now + Duration::from_secs(3),
        );
        restored.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());
        restored.recompute_effective_state_from_current_at(now + Duration::from_secs(3));

        assert!(restored.closing_gates.is_empty());
        assert_ne!(restored.sidebar_projection(false).0, AgentState::Blocked);
    }

    #[test]
    fn human_input_then_nonblocking_report_clears_unanswered_gate() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        terminal.apply_closing_block_payload(
            vec![test_closing_item(1, "Gate", "Choose the release path")],
            Vec::new(),
            Vec::new(),
        );

        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(now + Duration::from_secs(1))
            .expect("human input answers the gate");
        assert_eq!(
            terminal.closing_gates,
            vec![test_closing_item(1, "Gate", "Choose the release path")]
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(2),
            now + Duration::from_secs(2),
        );
        terminal.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());
        terminal.recompute_effective_state_from_current_at(now + Duration::from_secs(2));

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(terminal.closing_gates.is_empty());
        assert!(terminal.closing_items.is_empty());
    }

    #[test]
    fn a_new_blocked_report_does_not_reseed_answered_items_before_its_payload() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        let old_gate = test_closing_item(1, "Gate", "Choose the previous path");
        terminal.apply_closing_block_payload(vec![old_gate], Vec::new(), Vec::new());
        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(now + Duration::from_secs(1))
            .expect("human input answers the previous gate");

        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(2),
            now + Duration::from_secs(2),
        );
        terminal.clear_closing_task_report(now + Duration::from_secs(3));
        assert!(terminal.closing_gates.is_empty());

        terminal.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.closing_gates.is_empty());
    }

    #[test]
    fn later_closing_reports_union_blockers_without_duplicate_identity() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        let first = test_closing_item(1, "Gate", "Choose the release path");
        let second = test_closing_item(2, "Answer", "Confirm the migration plan");
        terminal.apply_closing_block_payload(vec![first.clone()], Vec::new(), Vec::new());
        terminal.apply_closing_block_payload(
            vec![first.clone(), second.clone()],
            Vec::new(),
            Vec::new(),
        );

        let mut repeated = first.clone();
        repeated.n = 99;
        repeated.label = " gate ".into();
        repeated.text = " Choose the release path ".into();
        terminal.apply_closing_block_payload(vec![repeated], Vec::new(), Vec::new());

        assert_eq!(terminal.closing_gates, vec![first, second]);
    }

    #[test]
    fn automated_input_does_not_answer_unanswered_gate() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        let gate = test_closing_item(1, "Gate", "Choose the release path");
        terminal.apply_closing_block_payload(vec![gate.clone()], Vec::new(), Vec::new());

        terminal.retire_blocked_hook_authority_for_automated_input_at(now + Duration::from_secs(1));

        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.closing_gates, vec![gate]);
    }

    #[test]
    fn process_exit_or_different_agent_drops_unanswered_gate_latch() {
        let now = Instant::now();
        let mut exited = test_terminal();
        exited.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        exited.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        exited.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            true,
            now + Duration::from_secs(1),
        );
        assert!(exited.closing_report.as_ref().unwrap().unanswered.is_none());

        let mut replaced = test_terminal();
        replaced.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(1),
            now,
        );
        replaced.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(1),
        );
        assert_eq!(replaced.state, AgentState::Working);
        assert!(replaced
            .closing_report
            .as_ref()
            .unwrap()
            .unanswered
            .is_none());
    }

    #[test]
    fn a_blocked_closing_block_report_yields_to_a_different_detected_agent() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed + std::time::Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert!(!terminal.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn a_blocked_closing_block_report_yields_to_the_agent_process_exiting() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );
        assert_eq!(terminal.state, AgentState::Blocked);

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            observed + std::time::Duration::from_secs(1),
        );

        assert_ne!(terminal.state, AgentState::Blocked);
        assert!(!terminal.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn explicit_input_retires_a_blocked_closing_block_report() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );
        assert_eq!(terminal.state, AgentState::Blocked);

        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(
                observed + std::time::Duration::from_secs(6),
            )
            .expect("explicit input retires the gate");

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn a_silent_blocked_hook_remains_authoritative_without_activity() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed + std::time::Duration::from_secs(30),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(!terminal.hook_authority_output_retirement_eligible());
        assert_eq!(terminal.hook_authority.as_ref().unwrap().retired_at, None);
    }

    #[test]
    fn a_fresh_hook_report_after_activity_retirement_is_honoured() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );
        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(
                observed + std::time::Duration::from_secs(1),
            )
            .expect("first report should retire");

        let fresh = terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(8),
            observed + std::time::Duration::from_secs(2),
        );

        assert!(fresh.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(!terminal.hook_authority_output_retirement_eligible());
        assert_eq!(terminal.hook_authority.as_ref().unwrap().retired_at, None);
    }

    #[test]
    fn visible_blocker_does_not_override_different_agent_hook() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(None, AgentState::Unknown);
        terminal.set_hook_authority(
            "custom:agent".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn fallback_idle_does_not_override_hook_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(10),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn fallback_idle_does_not_override_full_lifecycle_hook_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            crate::agent_resume::AgentSessionRef::id("opencode-root").unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            None,
            None,
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            now + Duration::from_secs(10),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_working_does_not_override_hook_idle_for_same_agent() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn effective_state_arbitration_tracks_working_screen_and_foreground_owners() {
        let now = Instant::now();
        let hook_idle_terminal = || {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.set_hook_authority_at(
                "herdr:claude".into(),
                "claude".into(),
                AgentState::Idle,
                None,
                None,
                None,
                now,
            );
            terminal
        };

        let mut visible_working = hook_idle_terminal();
        visible_working.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );

        let mut detected_working = hook_idle_terminal();
        detected_working.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            false,
            false,
            false,
            now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );

        let mut foreground_working = hook_idle_terminal();
        foreground_working.set_foreground_process(
            Some("cargo".into()),
            true,
            now + Duration::from_millis(1),
        );

        for (case, terminal, owner) in [
            ("visible working", visible_working, "screen"),
            ("detected working", detected_working, "screen"),
            (
                "foreground process",
                foreground_working,
                "foreground_process",
            ),
        ] {
            assert_eq!(terminal.state, AgentState::Working, "{case}");
            assert_eq!(terminal.effective_state_arbitration(), owner, "{case}");
        }
    }

    #[test]
    fn visible_working_does_not_override_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            crate::agent_resume::AgentSessionRef::id("kimi-root").unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn settled_visible_working_overrides_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn newer_visible_working_overrides_non_gate_closing_block_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn settled_detected_working_overrides_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            false,
            false,
            false,
            now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_working_fallback_is_ignored_under_full_lifecycle_hook_authority() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kilo), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kilo,
            "herdr:kilo",
            "kilo",
            crate::agent_resume::AgentSessionRef::id("kilo-root").unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:kilo".into(),
            "kilo".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kilo),
            AgentState::Working,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn visible_working_does_not_hold_against_newer_claude_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now,
        );

        let change = terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now + Duration::from_millis(100),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            change
                .unwrap()
                .effective_state_change
                .unwrap()
                .previous_state,
            AgentState::Working
        );
    }

    #[test]
    fn refreshed_visible_working_does_not_override_newer_hook_blocked() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now,
        );
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            None,
            now + Duration::from_millis(1201),
        );

        assert_eq!(terminal.state, AgentState::Blocked);

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(2000),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn fallback_idle_does_not_override_other_agent_hook_working() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn known_hook_authority_does_not_override_different_detected_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);
        let change = terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        assert!(change.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_agent_clears_conflicting_known_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn border_label_prefers_manual_label_over_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        assert_eq!(terminal.border_label(false), None);
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label(" reviewer ".into());
        assert_eq!(terminal.border_label(false).as_deref(), Some("reviewer"));
        assert_eq!(terminal.border_label(true).as_deref(), Some("reviewer"));

        terminal.set_manual_label("   ".into());
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label("reviewer".into());
        terminal.clear_manual_label();
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));
    }

    #[test]
    fn hook_authority_survives_unrelated_detected_agent_clear() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn full_lifecycle_hook_authority_ignores_detected_agent_clear_without_process_exit() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn detected_agent_clear_clears_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Cursor), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:cursor".into(),
            "cursor".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn detected_agent_clear_clears_matching_working_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn process_exit_clears_matching_hook_authority_before_reporting_idle() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            true,
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Codex));
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn stale_visible_screen_signal_does_not_override_newer_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            observed,
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_process_exit_preserves_newer_custom_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(100),
            observed + Duration::from_secs(1),
        );

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            observed,
        );

        assert!(!mutation.agent_released);
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .map(|hook| hook.source.as_str()),
            Some("custom:pi")
        );
    }

    #[test]
    fn custom_authority_reanchors_sequence_after_process_restart() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(100),
            observed,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            observed + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            false,
            observed + Duration::from_millis(2),
        );

        assert!(terminal
            .release_agent_with_mutation("custom:pi", "pi", Some(200))
            .is_none());
        assert!(terminal
            .clear_hook_authority_with_mutation(Some("custom:pi"), Some(201))
            .is_none());
        assert!(terminal
            .set_hook_authority(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(1),
            )
            .is_none());
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            observed + Duration::from_millis(3),
        );
        assert!(terminal
            .set_hook_authority(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(1),
            )
            .is_some());
    }

    #[test]
    fn process_exit_clears_newer_same_agent_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            false,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(2),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            observed,
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(terminal.effective_agent_label(), None);
    }

    #[test]
    fn detected_agent_change_clears_previous_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::OpenCode));
        assert_eq!(terminal.effective_agent_label(), Some("opencode"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_hook_report_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().state,
            AgentState::Working
        );
    }

    #[test]
    fn accepted_hook_report_stores_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(session_path.clone()),
                Some(20),
            )
            .expect("accepted report");

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| (&session_ref.kind, session_ref.value.as_str())),
            Some((
                &crate::agent_resume::AgentSessionRefKind::Path,
                session_path.as_str()
            ))
        );
    }

    #[test]
    fn stale_hook_report_cannot_overwrite_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        let new_session_path = test_session_path("new.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session_path),
            Some(19),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some(session_path.as_str())
        );
    }

    #[test]
    fn accepted_hook_report_without_session_ref_clears_previous_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(20),
        );

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                Some(21),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal
            .hook_authority
            .as_ref()
            .unwrap()
            .session_ref
            .is_none());
    }

    #[test]
    fn different_same_agent_session_ref_is_ignored_until_current_session_clears() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal.hook_report_sequences.get("herdr:claude"),
            Some(&21)
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn claude_startup_session_ref_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
            Some("startup".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn claude_lifecycle_session_ref_replaces_existing_session_ref() {
        for session_start_source in ["clear", "resume", "compact"] {
            let mut terminal = test_terminal();
            terminal
                .set_agent_session_ref(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id("claude-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id(&next_session),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| panic!("{session_start_source} should replace the session"));

            assert!(
                mutation.session_ref_changed,
                "{session_start_source} should mark the session changed"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some(next_session.as_str()),
                "{session_start_source} should store the replacement session"
            );
        }
    }

    #[test]
    fn codex_lifecycle_session_ref_replaces_existing_session_ref() {
        for session_start_source in ["startup", "clear", "resume", "compact"] {
            let mut terminal = test_terminal();
            terminal
                .set_agent_session_ref(
                    "herdr:codex".into(),
                    "codex".into(),
                    crate::agent_resume::AgentSessionRef::id("codex-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("codex-{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:codex".into(),
                    "codex".into(),
                    crate::agent_resume::AgentSessionRef::id(&next_session),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| panic!("{session_start_source} should replace the session"));

            assert!(mutation.session_ref_changed);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some(next_session.as_str())
            );
        }
    }

    #[test]
    fn agent_session_replacement_preserves_unguarded_user_metadata() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-old"),
                Some(1),
            )
            .expect("initial session should be accepted");
        for (source, title) in [("user:title", "User title"), ("user:jj", "User note")] {
            terminal
                .set_agent_metadata(AgentMetadataReport {
                    source: source.into(),
                    agent_label: None,
                    applies_to_source: None,
                    title: Some(title.into()),
                    display_agent: None,
                    state_labels: std::collections::HashMap::new(),
                    clear_title: false,
                    clear_display_agent: false,
                    clear_state_labels: false,
                    ttl: None,
                    seq: None,
                })
                .expect("user metadata should be accepted");
        }

        let mutation = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-new"),
                Some(2),
                Some("clear".into()),
            )
            .expect("replacement session should be accepted");

        assert!(mutation.session_ref_changed);
        assert_eq!(terminal.agent_metadata.len(), 2);
        assert_eq!(
            terminal.agent_metadata["user:jj"].title.as_deref(),
            Some("User note")
        );
        assert!(terminal.agent_metadata.contains_key("user:title"));
        assert!(terminal.agent_metadata.contains_key("user:jj"));
    }

    #[test]
    fn agent_session_replacement_preserves_surviving_metadata_watermarks() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-old"),
                Some(1),
            )
            .expect("initial session should be accepted");
        terminal
            .set_agent_metadata(AgentMetadataReport {
                source: "user:jj".into(),
                agent_label: None,
                applies_to_source: None,
                title: Some("Keep me".into()),
                display_agent: None,
                state_labels: std::collections::HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                ttl: None,
                seq: Some(5),
            })
            .expect("user metadata should be accepted");

        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-new"),
                Some(2),
                Some("clear".into()),
            )
            .expect("replacement session should be accepted");

        assert!(!terminal.metadata_report_sequence_is_fresh("user:jj", Some(1)));
        assert!(terminal
            .set_agent_metadata(AgentMetadataReport {
                source: "user:jj".into(),
                agent_label: Some("claude".into()),
                applies_to_source: Some("herdr:claude".into()),
                title: Some("Stale report".into()),
                display_agent: None,
                state_labels: std::collections::HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                ttl: None,
                seq: Some(1),
            })
            .is_none());
        assert_eq!(
            terminal.agent_metadata["user:jj"].title.as_deref(),
            Some("Keep me")
        );
    }

    #[test]
    fn opencode_new_session_ref_replaces_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-visible"),
                None,
                Some("select".into()),
            )
            .expect("local selection should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-attached-client"),
            Some(21),
            Some("new".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-visible")
        );
    }

    #[test]
    fn opencode_server_resume_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-visible"),
                None,
                Some("select".into()),
            )
            .expect("local selection should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-attached-client"),
            Some(21),
            Some("resume".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-visible")
        );
    }

    #[test]
    fn opencode_tui_selection_anchors_after_process_detection() {
        let mut terminal = test_terminal();
        let startup_selection = terminal.set_agent_session_ref_for_session_start(
            "herdr:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-startup-selection"),
            None,
            Some("select".into()),
        );
        assert!(startup_selection.is_none());
        assert_eq!(
            terminal
                .suppressed_full_lifecycle_hook_reports
                .get("herdr:opencode")
                .and_then(|suppressed| suppressed.replacement_session_ref.as_ref())
                .map(|session| session.value.as_str()),
            Some("opencode-startup-selection")
        );

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-startup-selection")
        );
        assert!(!terminal
            .suppressed_full_lifecycle_hook_reports
            .contains_key("herdr:opencode"));

        terminal.suppressed_full_lifecycle_hook_reports.insert(
            "herdr:opencode".into(),
            SuppressedFullLifecycleHookReport {
                agent_label: "opencode".into(),
                session_ref: None,
                observed_at: Instant::now(),
                reason: FullLifecycleHookSuppressionReason::ProcessExit,
                replacement_session_ref: None,
                pending_replacement_report: None,
            },
        );
        let selected = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-reselected"),
                None,
                Some("select".into()),
            )
            .expect("local TUI selection should reconcile generation suppression");

        assert!(selected.session_ref_changed);
        assert!(!terminal
            .suppressed_full_lifecycle_hook_reports
            .contains_key("herdr:opencode"));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-reselected")
        );
        assert!(!terminal
            .hook_report_sequences
            .contains_key("herdr:opencode"));
    }

    #[test]
    fn opencode_tui_selection_reanchors_full_lifecycle_authority() {
        let mut terminal = test_terminal();
        let old_session = crate::agent_resume::AgentSessionRef::id("opencode-newer").unwrap();
        let selected_session =
            crate::agent_resume::AgentSessionRef::id("opencode-selected-older").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            old_session.clone(),
        );
        terminal
            .set_hook_authority_with_session_ref(
                "herdr:opencode".into(),
                "opencode".into(),
                AgentState::Idle,
                None,
                Some(old_session.clone()),
                Some(20),
            )
            .expect("initial session should own lifecycle state");
        let attached_session =
            crate::agent_resume::AgentSessionRef::id("opencode-attached-client").unwrap();
        let attached = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            Some(attached_session.clone()),
            Some(21),
        );
        assert!(attached.is_none());
        assert!(!terminal
            .suppressed_full_lifecycle_hook_reports
            .contains_key("herdr:opencode"));

        let selected = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                Some(selected_session.clone()),
                None,
                Some("select".into()),
            )
            .expect("selected session should replace the previous session");

        assert!(selected.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        assert!(!terminal
            .suppressed_full_lifecycle_hook_reports
            .contains_key("herdr:opencode"));
        assert_eq!(
            terminal.hook_report_sequences.get("herdr:opencode"),
            Some(&20)
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&selected_session)
        );

        terminal
            .set_hook_authority_with_session_ref(
                "herdr:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                Some(selected_session.clone()),
                Some(21),
            )
            .expect("selected session should regain lifecycle authority");
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref()),
            Some(&selected_session)
        );

        let late_old_session = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Idle,
            None,
            Some(old_session),
            Some(22),
        );
        assert!(late_old_session.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref()),
            Some(&selected_session)
        );

        let late_attached_session = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Blocked,
            None,
            Some(attached_session),
            Some(23),
        );
        assert!(late_attached_session.is_none());
        assert_eq!(terminal.state, AgentState::Working);

        let final_session =
            crate::agent_resume::AgentSessionRef::id("opencode-final-selection").unwrap();
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                Some(final_session.clone()),
                None,
                Some("select".into()),
            )
            .expect("another local selection should remain authoritative");
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&final_session)
        );
        assert!(!terminal
            .suppressed_full_lifecycle_hook_reports
            .contains_key("herdr:opencode"));
    }

    #[test]
    fn pi_session_replacement_clears_the_previous_sessions_alias() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::id("pi-old"),
                Some(20),
                Some("new".into()),
            )
            .expect("initial session should be accepted");
        terminal.set_agent_name("reviewer".into());

        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::id("pi-new"),
                Some(21),
                Some("new".into()),
            )
            .expect("new should replace the session");

        assert!(terminal.agent_name.is_none());
    }

    #[test]
    fn managed_agent_name_survives_native_session_replacement() {
        let mut terminal = test_terminal();
        let now = Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::OpenCode,
            now,
            Duration::ZERO,
            Duration::from_secs(1),
        );
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        assert!(terminal.reconcile_managed_agent_at(now, false));

        for session in ["opencode-old", "opencode-new"] {
            terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:opencode".into(),
                    "opencode".into(),
                    crate::agent_resume::AgentSessionRef::id(session),
                    None,
                    Some("select".into()),
                )
                .expect("managed session should be accepted");
        }

        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.managed_agent_interactive_ready());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-new")
        );
    }

    #[test]
    fn opencode_session_ref_without_start_source_does_not_replace_existing() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-old"),
                None,
                Some("select".into()),
            )
            .expect("local selection should be accepted");

        // session.updated reports carry no session_start_source, so a different
        // id must not displace the established session (cross-talk guard).
        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-other"),
            Some(21),
            None,
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-old")
        );
    }

    #[test]
    fn different_owner_session_ref_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:droid".into(),
                "droid".into(),
                crate::agent_resume::AgentSessionRef::id("droid-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(21),
            Some("resume".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:droid", "droid", "droid-session"))
        );
    }

    #[test]
    fn foreground_agent_session_replaces_stale_different_owner_session_ref() {
        for session_start_source in ["resume", "startup"] {
            let mut terminal = test_terminal();
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
            });
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id("claude-session"),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| {
                    panic!("{session_start_source} should replace stale codex session")
                });

            assert!(mutation.session_ref_changed);
            assert_eq!(
                terminal.persisted_agent_session.as_ref().map(|session| (
                    session.source.as_str(),
                    session.agent.as_str(),
                    session.session_ref.value.as_str()
                )),
                Some(("herdr:claude", "claude", "claude-session")),
                "{session_start_source} should store claude session"
            );
        }
    }

    #[test]
    fn foreground_agent_session_requires_lifecycle_source_to_replace_different_owner() {
        for session_start_source in [None, Some("other")] {
            let mut terminal = test_terminal();
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
            });
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

            let mutation = terminal.set_agent_session_ref_for_session_start(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(21),
                session_start_source.map(str::to_string),
            );

            assert!(
                mutation.is_none(),
                "{session_start_source:?} should not replace"
            );
            assert_eq!(
                terminal.persisted_agent_session.as_ref().map(|session| (
                    session.source.as_str(),
                    session.agent.as_str(),
                    session.session_ref.value.as_str()
                )),
                Some(("herdr:codex", "codex", "codex-session"))
            );
        }
    }

    #[test]
    fn different_owner_session_ref_requires_matching_detected_agent() {
        for session_start_source in ["startup", "resume"] {
            for detected_agent in [None, Some(Agent::Codex)] {
                let mut terminal = test_terminal();
                terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                    source: "herdr:codex".into(),
                    agent: "codex".into(),
                    session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
                });
                terminal.set_detected_state(detected_agent, AgentState::Idle);

                let mutation = terminal.set_agent_session_ref_for_session_start(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id("claude-session"),
                    Some(21),
                    Some(session_start_source.into()),
                );

                assert!(
                    mutation.is_none(),
                    "{session_start_source} with {detected_agent:?} should not replace"
                );
                assert_eq!(
                    terminal.persisted_agent_session.as_ref().map(|session| (
                        session.source.as_str(),
                        session.agent.as_str(),
                        session.session_ref.value.as_str()
                    )),
                    Some(("herdr:codex", "codex", "codex-session"))
                );
            }
        }
    }

    #[test]
    fn custom_session_report_does_not_replace_different_owner_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "custom:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(21),
            Some("resume".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:codex", "codex", "codex-session"))
        );
    }

    #[test]
    fn foreground_agent_session_replaces_stale_different_owner_hook_authority() {
        let mut terminal = test_terminal();
        let now = std::time::Instant::now();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        );
        terminal
            .set_hook_authority_at(
                "herdr:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::id("opencode-session"),
                Some(20),
                now + Duration::from_millis(1),
            )
            .expect("initial hook authority should be accepted");
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            false,
            now,
        );

        let mutation = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:codex".into(),
                "codex".into(),
                crate::agent_resume::AgentSessionRef::id("codex-session"),
                Some(21),
                Some("startup".into()),
            )
            .expect("foreground codex should replace stale hook authority");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.current_session_identity_for_persistence(),
            Some((
                "herdr:codex".into(),
                "codex".into(),
                crate::agent_resume::AgentSessionRefKind::Id,
                "codex-session".into()
            ))
        );
        let late_old_session = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("opencode-session"),
            Some(22),
        );
        assert!(late_old_session.is_none());

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-new-session"),
                None,
                Some("select".into()),
            )
            .expect("fresh local selection");
        let fresh_session = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("opencode-new-session"),
            Some(24),
        );
        assert!(fresh_session.is_some());
    }

    #[test]
    fn different_owner_full_lifecycle_hook_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:droid".into(),
                "droid".into(),
                crate::agent_resume::AgentSessionRef::id("droid-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/pi-session.jsonl"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:droid", "droid", "droid-session"))
        );
    }

    #[test]
    fn repeated_same_agent_session_ref_is_accepted_without_session_change() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(21),
            )
            .expect("same session should be accepted");

        assert!(!mutation.session_ref_changed);
    }

    #[test]
    fn hook_authority_rejects_state_from_a_different_session() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        );
        terminal
            .set_hook_authority_with_session_ref(
                "herdr:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::id("opencode-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Blocked,
            Some("needs approval".into()),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some("opencode-session")
        );
    }

    #[test]
    fn detected_agent_clear_does_not_clear_current_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let clear = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!clear.session_ref_changed);

        let mutation = terminal.set_agent_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("new-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn clearing_hook_authority_clears_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(20),
        );

        let mutation = terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(21))
            .expect("accepted clear");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn agent_alias_survives_detection_uncertainty_and_reported_release_but_not_replacement() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());

        terminal.set_detected_state(None, AgentState::Unknown);
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert!(terminal.agent_name.is_none());

        terminal.set_agent_name("replacement".into());
        let mutation = terminal
            .release_agent_with_mutation("herdr:codex", "codex", None)
            .expect("detected agent release should be accepted");
        assert!(!mutation.agent_released);
        assert_eq!(terminal.agent_name.as_deref(), Some("replacement"));
        assert_eq!(terminal.detected_agent, Some(Agent::Codex));
    }

    #[test]
    fn custom_release_preserves_process_owned_agent_state() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal
            .set_hook_authority(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(10),
            )
            .expect("custom state should be accepted");

        let mutation = terminal
            .release_agent_with_mutation("custom:pi", "pi", Some(11))
            .expect("custom release should be accepted");

        assert!(!mutation.agent_released);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.effective_agent_label(), Some("pi"));
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn agent_replacement_clears_alias_owned_by_hook_identity() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some(20),
        );
        terminal.set_agent_name("reviewer".into());

        terminal.set_detected_state(Some(Agent::Grok), AgentState::Idle);

        assert!(terminal.agent_name.is_none());
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Grok));
    }

    #[test]
    fn accepted_hook_replacement_clears_the_previous_agents_alias() {
        let mut terminal = test_terminal();
        terminal
            .set_hook_authority_at(
                "custom:agent".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                Some(20),
                Instant::now(),
            )
            .expect("initial hook should be accepted");
        terminal.set_agent_name("reviewer".into());

        terminal
            .set_hook_authority_at(
                "custom:agent".into(),
                "claude".into(),
                AgentState::Idle,
                None,
                None,
                Some(21),
                Instant::now(),
            )
            .expect("replacement hook should be accepted");

        assert!(terminal.agent_name.is_none());
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Claude));
    }

    #[test]
    fn accepted_same_kind_hook_owner_replacement_clears_the_alias() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("first.jsonl")).unwrap(),
        );
        terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(test_session_path("first.jsonl")),
                Some(20),
                Instant::now(),
            )
            .expect("initial hook should be accepted");
        terminal.set_agent_name("reviewer".into());
        terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(21))
            .expect("hook clear should be accepted");
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                crate::agent_resume::AgentSessionRef::path(test_session_path("second.jsonl")),
                Some(22),
                Instant::now(),
            )
            .expect("replacement hook should be accepted");

        assert!(terminal.agent_name.is_none());
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Pi));
    }

    #[test]
    fn launch_command_alone_does_not_make_a_terminal_an_agent() {
        let terminal = test_terminal().with_launch_argv(vec!["just".into(), "dev".into()]);

        assert!(!terminal.is_agent_terminal());
    }

    #[test]
    fn release_agent_clears_matching_restored_session_ref_before_detection() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal
            .release_agent_with_mutation("herdr:hermes", "hermes", Some(21))
            .expect("accepted release");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn release_agent_preserves_foreign_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);

        let mutation = terminal
            .release_agent_with_mutation("herdr:pi", "pi", Some(21))
            .expect("visible agent release should be accepted");

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:claude", "claude", "claude-session"))
        );
    }

    #[test]
    fn process_exit_clears_matching_persisted_session_ref() {
        let mut terminal = test_terminal();
        let session_ref =
            crate::agent_resume::AgentSessionRef::path(test_session_path("pi.jsonl")).unwrap();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: session_ref.clone(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            std::time::Instant::now(),
        );

        assert!(mutation.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_none());

        let delayed = terminal.set_agent_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            Some(session_ref),
            Some(21),
        );
        assert!(delayed.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn process_exit_preserves_foreign_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            std::time::Instant::now(),
        );

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn respawn_cleanup_resets_restored_agent_status() {
        let mut terminal = test_terminal();
        terminal.respawn_shell_on_exit = true;
        terminal.set_agent_name("codex".into());
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_detected_agent_process_at(Agent::Codex, Instant::now());

        terminal.clear_agent_runtime_identity_after_respawn();

        assert_eq!(terminal.state, AgentState::Unknown);
        assert!(terminal.detected_agent.is_none());
        assert!(terminal.agent_name.is_none());
        assert!(terminal.persisted_agent_session.is_none());
        assert!(!terminal.respawn_shell_on_exit);
        assert!(!terminal.finish_agent_process_acquisition());
    }

    #[test]
    fn closing_block_status_retains_live_claude_subagent_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        let transcript =
            std::path::PathBuf::from("/profiles/team-a/projects/-tmp-repro/claude-session.jsonl");
        terminal
            .set_claude_transcript_target(Some("claude-session".into()), Some(transcript.clone()));
        terminal.set_active_subagents(Some(2));

        terminal.set_hook_authority(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            Some(1),
        );

        assert_eq!(terminal.claude_transcript_path, Some(transcript));
        assert_eq!(
            terminal.claude_transcript_session_id.as_deref(),
            Some("claude-session")
        );
        assert_eq!(terminal.active_subagents, Some(2));
    }

    #[test]
    fn agent_process_exit_tracks_recent_respawn_window() {
        let mut terminal = test_terminal();
        let now = std::time::Instant::now();

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            true,
            now,
        );

        assert!(terminal.agent_process_exited_within(now, Duration::from_secs(2)));
        assert!(!terminal
            .agent_process_exited_within(now + Duration::from_secs(3), Duration::from_secs(2)));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(4),
        );

        assert!(!terminal
            .agent_process_exited_within(now + Duration::from_secs(4), Duration::from_secs(2)));
    }

    #[test]
    fn detected_conflict_clears_live_hook_but_preserves_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(20),
        );

        let mutation =
            terminal.set_detected_state_with_mutation(Some(Agent::Grok), AgentState::Idle);

        assert!(!mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:claude", "claude", "claude-session"))
        );
    }

    #[test]
    fn detected_agent_disappearance_does_not_clear_full_lifecycle_hook_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            crate::agent_resume::AgentSessionRef::id("kimi-session").unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("kimi-session"),
            Some(20),
        );

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);

        assert!(!mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_some());
        assert!(terminal.persisted_agent_session.is_none());
        assert_eq!(terminal.effective_agent_label(), Some("kimi"));
    }

    #[test]
    fn detected_agent_disappearance_preserves_matching_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:opencode".into(),
            agent: "opencode".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        });

        let first =
            terminal.set_detected_state_with_mutation(Some(Agent::OpenCode), AgentState::Idle);
        assert!(!first.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());

        let second = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!second.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn initial_unknown_detection_preserves_restored_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!mutation.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn unsequenced_hook_report_is_ignored_after_source_uses_sequence() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_clear_all_sequence_is_checked_against_current_authority_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.clear_hook_authority(None, Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn same_sequence_from_different_sources_is_independent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        terminal.set_hook_authority(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().source,
            "custom:pi"
        );
    }

    #[test]
    fn stale_full_lifecycle_hook_authority_falls_back_to_screen() {
        const CONFIGURED_TIMEOUT: Duration = Duration::from_secs(10 * 60);
        let reported_at = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Working);
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-stale").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Working,
            None,
            Some(session_ref),
            Some(10),
            reported_at,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            reported_at + CONFIGURED_TIMEOUT,
        );

        assert_eq!(
            terminal.state,
            AgentState::Working,
            "authority is fresh at the threshold"
        );
        assert!(terminal.full_lifecycle_hook_authority_active());
        let mutation = terminal.expire_full_lifecycle_hook_authority_at(
            reported_at + CONFIGURED_TIMEOUT + Duration::from_nanos(1),
            CONFIGURED_TIMEOUT,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(mutation.is_some());
    }

    #[test]
    fn blocked_full_lifecycle_authority_does_not_expire_with_open_items() {
        let reported_at = Instant::now();
        let timeout = Duration::from_secs(10 * 60);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Needs you".into(),
                text: "Choose a release path".into(),
                blocking: true,
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-blocked").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            reported_at,
        );
        assert_eq!(terminal.state, AgentState::Blocked);

        let after_timeout = reported_at + timeout + Duration::from_secs(1);
        let mutation = terminal.expire_full_lifecycle_hook_authority_at(after_timeout, timeout);

        assert!(mutation.is_none());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.has_pending_human_input());
        assert!(terminal.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn blocked_hold_survives_full_lifecycle_hook_horizon_without_open_items() {
        let reported_at = Instant::now();
        let timeout = Duration::from_secs(10 * 60);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-unanswered").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            reported_at,
        );

        let after_timeout = reported_at + timeout + Duration::from_secs(1);
        let mutation = terminal.expire_full_lifecycle_hook_authority_at(after_timeout, timeout);

        assert!(mutation.is_some());
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.effective_state_arbitration(), "blocked_hold");
    }

    #[test]
    fn blocked_hold_outranks_foreground_process_after_full_lifecycle_horizon() {
        let reported_at = Instant::now();
        let timeout = Duration::from_secs(10 * 60);
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-held-foreground").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            reported_at,
        );
        terminal.set_foreground_process(
            Some("cargo".into()),
            true,
            reported_at + Duration::from_secs(1),
        );

        let after_timeout = reported_at + timeout + Duration::from_secs(1);
        let mutation = terminal.expire_full_lifecycle_hook_authority_at(after_timeout, timeout);

        assert!(mutation.is_some());
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(terminal.foreground_process_active());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.effective_state_arbitration(), "blocked_hold");
    }

    #[test]
    fn unmatched_output_retires_hook_authority() {
        let reported_at = Instant::now();
        let mut idle_hook = test_terminal();
        idle_hook.set_detected_state(Some(Agent::Kimi), AgentState::Working);
        let idle_session = crate::agent_resume::AgentSessionRef::id("kimi-idle").unwrap();
        anchor_full_lifecycle_session(
            &mut idle_hook,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            idle_session.clone(),
        );
        idle_hook.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Idle,
            None,
            Some(idle_session.clone()),
            Some(10),
            reported_at,
        );

        let retired = idle_hook
            .retire_output_inconsistent_hook_authority_at(reported_at + Duration::from_secs(5));
        assert!(
            retired.is_some(),
            "output contradicts a hook-reported idle state"
        );
        assert_eq!(idle_hook.state, AgentState::Working);
        idle_hook.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Idle,
            None,
            Some(idle_session),
            Some(11),
            reported_at + Duration::from_secs(6),
        );
        assert!(idle_hook.full_lifecycle_hook_authority_active());
        assert_eq!(idle_hook.state, AgentState::Idle);
        assert_eq!(idle_hook.hook_authority.as_ref().unwrap().retired_at, None);

        let mut working_hook = test_terminal();
        working_hook.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let working_session = crate::agent_resume::AgentSessionRef::id("kimi-working").unwrap();
        anchor_full_lifecycle_session(
            &mut working_hook,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            working_session.clone(),
        );
        working_hook.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Working,
            None,
            Some(working_session),
            Some(10),
            reported_at,
        );

        assert!(working_hook
            .retire_output_inconsistent_hook_authority_at(reported_at + Duration::from_secs(5))
            .is_none());
        assert_eq!(working_hook.state, AgentState::Working);
        assert!(working_hook.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn fresh_hook_state_wins_over_non_blocker_screen() {
        let reported_at = Instant::now();
        for hook_state in [AgentState::Idle, AgentState::Working, AgentState::Unknown] {
            for screen_state in [AgentState::Idle, AgentState::Working, AgentState::Unknown] {
                let mut terminal = test_terminal();
                terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
                let session_ref =
                    crate::agent_resume::AgentSessionRef::id("kimi-fresh-matrix").unwrap();
                anchor_full_lifecycle_session(
                    &mut terminal,
                    Agent::Kimi,
                    "herdr:kimi",
                    "kimi",
                    session_ref.clone(),
                );
                terminal.set_hook_authority_at(
                    "herdr:kimi".into(),
                    "kimi".into(),
                    hook_state,
                    None,
                    Some(session_ref),
                    Some(10),
                    reported_at,
                );

                terminal.set_detected_state_with_screen_signals_at(
                    Some(Agent::Kimi),
                    screen_state,
                    false,
                    screen_state == AgentState::Idle,
                    screen_state == AgentState::Working,
                    false,
                    false,
                    reported_at + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
                );
                assert_eq!(
                    terminal.state, hook_state,
                    "fresh {hook_state:?} hook must beat ordinary {screen_state:?} screen"
                );
                assert!(terminal.full_lifecycle_hook_authority_active());
            }
        }
    }

    #[test]
    fn visible_blocker_overrides_fresh_hook_authority() {
        let reported_at = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-blocker").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            session_ref.clone(),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Working,
            None,
            Some(session_ref),
            Some(10),
            reported_at,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Blocked,
            true,
            false,
            false,
            false,
            false,
            reported_at + Duration::from_secs(1),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn closing_block_authority_is_limited_to_live_blocked_gates() {
        assert!(crate::detect::is_closing_block_source(
            "herdr:claude-closing-block",
            "claude"
        ));
        assert!(!crate::detect::full_lifecycle_hook_authority(
            "herdr:claude-closing-block",
            "claude"
        ));

        let reported_at = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.apply_closing_task_report(
            None,
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            reported_at,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            Some(1),
            reported_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            reported_at + Duration::from_secs(1),
        );
        assert_eq!(
            terminal.state,
            AgentState::Unknown,
            "a closing report without completion remains missing evidence"
        );

        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(2),
            reported_at + Duration::from_secs(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            false,
            reported_at + Duration::from_secs(700),
        );
        assert_eq!(
            terminal.state,
            AgentState::Blocked,
            "live fleet gate ignores general lifecycle timeout"
        );
        assert!(terminal
            .retire_output_inconsistent_hook_authority_at(reported_at + Duration::from_secs(705))
            .is_some());
        assert_eq!(
            terminal.state,
            AgentState::Blocked,
            "sustained new-turn output cannot answer a human gate"
        );
    }
}
