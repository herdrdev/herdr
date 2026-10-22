use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

// Effective state arbitration is intentionally centralized here. Full lifecycle
// Herdr hook integrations are hook-authoritative while live; screen recovery
// remains only for session-only/custom hook paths and fallback detection.
// Process-exit updates clear matching hook authority before recomputing state.

use crate::detect::{Agent, AgentState};
use crate::terminal::TerminalId;

pub(crate) const TURN_RECORD_LIMIT: usize = 64;
pub(crate) const TURN_MESSAGE_MAX_BYTES: usize = 8 * 1024;
pub(crate) const TURN_ABORT_WINDOW: Duration = Duration::from_secs(15);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// The agent became idle without a recent interrupt.
    Completed,
    /// The agent became idle within 15 seconds of an Esc or Ctrl+C interrupt.
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnRecord {
    pub turn: u64,
    pub turn_epoch: u64,
    pub outcome: TurnOutcome,
    pub completed_unix_ms: u64,
    pub message: Option<String>,
    pub message_truncated: bool,
    pub agent_session_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportedAgentSessionPath {
    source: String,
    agent: String,
    session_id: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportedAgentSessionRuntime {
    source: String,
    agent: String,
    session_ref: crate::agent_resume::AgentSessionRef,
    pub(crate) cursor: Option<String>,
    pub(crate) process_pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum TurnCounterResetPath {
    ServerBoot = 0,
    PaneRespawn = 1,
    SessionRestore = 2,
    // The only non-test construction site is `restore_handoff`, which is `#[cfg(unix)]`;
    // the `#[cfg(test)]` epoch-tag decoder below constructs it on every platform, which is
    // why Windows `cargo test` compiles while `clippy --bin herdr` does not.
    #[cfg_attr(windows, allow(dead_code))]
    SelfUpdateHandoff = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnReplay {
    pub turn_epoch: u64,
    pub records: Vec<TurnRecord>,
    pub truncated: bool,
    pub oldest_available: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnReplayError {
    EpochMismatch { expected: u64, actual: u64 },
    SinceAhead { since: u64, newest: u64 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TurnCompletionContext {
    message: Option<String>,
    agent_session_path: Option<String>,
}

fn fresh_turn_epoch_for(path: TurnCounterResetPath) -> u64 {
    static LAST_EPOCH: AtomicU64 = AtomicU64::new(0);
    const PATH_BITS: u32 = 2;
    const PATH_MASK: u64 = (1 << PATH_BITS) - 1;

    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(1)
        .max(1)
        & !PATH_MASK;
    let mut observed = LAST_EPOCH.load(Ordering::Relaxed);
    loop {
        let next_sequence = (observed & !PATH_MASK).saturating_add(1 << PATH_BITS);
        let candidate = clock.max(next_sequence) | path as u64;
        match LAST_EPOCH.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return candidate,
            Err(current) => observed = current,
        }
    }
}

#[cfg(test)]
pub(crate) fn turn_epoch_reset_path_for_test(epoch: u64) -> TurnCounterResetPath {
    match epoch & 0b11 {
        0 => TurnCounterResetPath::ServerBoot,
        1 => TurnCounterResetPath::PaneRespawn,
        2 => TurnCounterResetPath::SessionRestore,
        3 => TurnCounterResetPath::SelfUpdateHandoff,
        _ => unreachable!("two-bit turn epoch path tag"),
    }
}

fn truncate_turn_message(message: String) -> (String, bool) {
    if message.len() <= TURN_MESSAGE_MAX_BYTES {
        return (message, false);
    }
    let mut end = TURN_MESSAGE_MAX_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    (message[..end].to_string(), true)
}

#[path = "metadata.rs"]
mod metadata;
pub use metadata::{AgentMetadata, AgentMetadataReport, EffectivePresentation};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: AgentState,
    pub message: Option<String>,
    #[serde(skip, default = "Instant::now")]
    pub reported_at: Instant,
    pub session_ref: Option<crate::agent_resume::AgentSessionRef>,
}

#[cfg(unix)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct HandoffAgentState {
    authority: HookAuthority,
    sequence: Option<u64>,
    acquisition_pending: bool,
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
    pub agent_released: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentNameOwner {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecentAgentProcessExit {
    agent: Agent,
    observed_at: Instant,
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
    fallback_observed_at: Option<Instant>,
    pub hook_authority: Option<HookAuthority>,
    pub agent_metadata: HashMap<String, AgentMetadata>,
    pub metadata_tokens: crate::metadata_tokens::MetadataTokens,
    pub persisted_agent_session: Option<crate::agent_resume::PersistedAgentSession>,
    pub(crate) reported_agent_session_path: Option<ReportedAgentSessionPath>,
    pub(crate) reported_agent_session_runtime: Option<ReportedAgentSessionRuntime>,
    reported_resume: Option<crate::agent_resume::ReportedAgentResume>,
    reported_resume_revision: u64,
    pub terminal_title: Option<String>,
    pub manual_label: Option<String>,
    pub agent_name: Option<String>,
    agent_name_owner: Option<AgentNameOwner>,
    managed_agent: Option<ManagedAgent>,
    codex_prompt_ready: bool,
    managed_agent_launch_session: Option<crate::agent_resume::PersistedAgentSession>,
    hook_report_sequences: HashMap<String, u64>,
    accepted_session_report_generation: u64,
    suppressed_full_lifecycle_hook_reports: HashMap<String, SuppressedFullLifecycleHookReport>,
    stale_full_lifecycle_hook_sessions: HashMap<String, Vec<StaleFullLifecycleHookSession>>,
    metadata_report_sequences: HashMap<String, u64>,
    metadata_report_agents: HashMap<String, Agent>,
    metadata_token_sequence_sources: std::collections::HashSet<String>,
    pub state: AgentState,
    pub input_pending: bool,
    pub input_prompt_kind: Option<crate::detect::InputPromptKind>,
    pub(crate) composer_write: Option<super::ComposerWrite>,
    /// Raised by a delayed agent-prompt submission whose guard refused to write
    /// because the pane's occupant changed inside the submit delay. The prompt
    /// text is in the composer and no turn was started, so the caller's earlier
    /// success response is now known to be incomplete.
    pub(crate) prompt_submit_abandoned: Option<std::sync::Arc<super::PromptSubmitWatch>>,
    /// A prompt watch was displaced while still armed, so its outcome is
    /// unknowable. Reported as abandonment because "we cannot tell" must never
    /// read as "it submitted".
    pub(crate) displaced_submit_unknown: bool,
    /// A submitted prompt's turn has completed, so that prompt can no longer be
    /// the author of composer text. Durable because the fact is only observable
    /// at the moment the watch is discarded.
    pub(crate) prompt_claim_retired: bool,
    pub last_agent_state_change_seq: Option<u64>,
    /// Wall-clock ms when the agent's CURRENT status began — set at each transition to
    /// a DIFFERENT state. None until the first detected transition. NOT persisted, so it
    /// resets on restore/respawn (time-in-state restarts for a freshly relaunched agent,
    /// which is correct). Drives the app's compact "5m/2h/3d" time-in-state badge (#173).
    pub agent_status_since_unix_ms: Option<u64>,
    pub turn: u64,
    pub turn_epoch: u64,
    /// Monotonic count of how many times a DIFFERENT agent has seized this
    /// terminal (a foreground hook-authority takeover). Unlike `turn_epoch`,
    /// which is deliberately reset per boot, this is persisted and rehydrated on
    /// restore, so `machine_id / terminal_id / occupant_generation` forms a
    /// stable global identity that survives a daemon restart. Bumped only at the
    /// genuine-takeover site in `set_hook_authority_at`, never on an ordinary
    /// same-occupant hook update.
    pub occupant_generation: u64,
    turn_records: VecDeque<TurnRecord>,
    turn_abort_pending_at: Option<Instant>,
    pub last_agent_completion_seq: Option<u64>,
    pub revision: u64,
    pub launch_argv: Option<Vec<String>>,
    pub respawn_shell_on_exit: bool,
    recent_agent_process_exit: Option<RecentAgentProcessExit>,
    agent_process_acquisition_pending: bool,
    pub pending_agent_resume_plan: Option<crate::agent_resume::AgentResumePlan>,
    /// Extra `(env_var, value)` pairs to inject into the next deferred
    /// shell spawn for this terminal (the account config-home env selected by an
    /// `agent.restart`). Consumed and cleared at the resume/respawn spawn site so
    /// a relaunch runs under the chosen account. Holds only a config-dir path,
    /// never a credential value.
    pub pending_launch_env: Vec<(String, String)>,
    /// The credential/config-home account id this terminal's agent runs under, if
    /// any. Remembered across an in-place restart so a plain `agent.restart`
    /// keeps the same account; an explicit swap overrides it. Independent of the
    /// session-ref churn from hook reports.
    pub agent_account: Option<String>,
    /// Set when a resume was REFUSED because `agent_account` no longer resolves to a
    /// registered account. Holds the unresolved account id.
    ///
    /// Two jobs. It stops the refusal retrying every tick — the resume plan is kept (so
    /// re-registering the account can still bring the agent back) but the pane is skipped
    /// as a resume candidate, instead of re-refusing and re-logging forever. And it is the
    /// fact the API surfaces, so a seat that did not come back says WHY rather than
    /// looking like an ordinary idle shell.
    ///
    /// Not persisted: re-derived on the next restore attempt, and cleared when the
    /// accounts registry reloads so a re-added account is picked straight up.
    pub account_resume_blocked: Option<String>,
    /// Server-owned Claude Code <-> Codex session-transfer transaction. Kept
    /// separate from presentation state; the API projects it into `AgentInfo`.
    pub(crate) session_transfer: Option<crate::session_transfer::RuntimeSessionTransfer>,
    pub restore_error: Option<String>,
}

impl TerminalState {
    pub fn new(id: TerminalId, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            detected_agent: None,
            fallback_state: AgentState::Unknown,
            fallback_visible_blocker: false,
            fallback_observed_at: None,
            hook_authority: None,
            agent_metadata: HashMap::new(),
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            persisted_agent_session: None,
            reported_agent_session_path: None,
            reported_agent_session_runtime: None,
            reported_resume: None,
            reported_resume_revision: 0,
            terminal_title: None,
            manual_label: None,
            agent_name: None,
            agent_name_owner: None,
            managed_agent: None,
            codex_prompt_ready: false,
            managed_agent_launch_session: None,
            hook_report_sequences: HashMap::new(),
            accepted_session_report_generation: 0,
            suppressed_full_lifecycle_hook_reports: HashMap::new(),
            stale_full_lifecycle_hook_sessions: HashMap::new(),
            metadata_report_sequences: HashMap::new(),
            metadata_report_agents: HashMap::new(),
            metadata_token_sequence_sources: std::collections::HashSet::new(),
            state: AgentState::Unknown,
            input_pending: false,
            input_prompt_kind: None,
            composer_write: None,
            prompt_submit_abandoned: None,
            displaced_submit_unknown: false,
            prompt_claim_retired: false,
            last_agent_state_change_seq: None,
            agent_status_since_unix_ms: None,
            turn: 0,
            turn_epoch: fresh_turn_epoch_for(TurnCounterResetPath::ServerBoot),
            occupant_generation: 0,
            turn_records: VecDeque::with_capacity(TURN_RECORD_LIMIT),
            turn_abort_pending_at: None,
            last_agent_completion_seq: None,
            revision: 0,
            launch_argv: None,
            respawn_shell_on_exit: false,
            recent_agent_process_exit: None,
            agent_process_acquisition_pending: false,
            pending_agent_resume_plan: None,
            pending_launch_env: Vec::new(),
            account_resume_blocked: None,
            agent_account: None,
            session_transfer: None,
            restore_error: None,
        }
    }

    pub fn set_detected_agent_process_at(
        &mut self,
        agent: Agent,
        now: Instant,
    ) -> TerminalStateMutation {
        let starts_acquisition = !self
            .should_ignore_detected_state_under_full_lifecycle_hook(Some(agent), false)
            && !self.detected_state_observed_before_release_suppression(Some(agent), now);
        let mutation = self.set_detected_state_with_screen_signals_at(
            Some(agent),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now,
        );
        if starts_acquisition {
            self.codex_prompt_ready = false;
            self.agent_process_acquisition_pending = true;
        }
        mutation
    }

    #[cfg(unix)]
    pub(crate) fn handoff_agent_state(&self) -> Option<HandoffAgentState> {
        if !self.live_full_lifecycle_hook_authority() {
            return None;
        }
        let authority = self.hook_authority.as_ref()?;
        Some(HandoffAgentState {
            authority: authority.clone(),
            sequence: self.hook_report_sequences.get(&authority.source).copied(),
            acquisition_pending: self.agent_process_acquisition_pending,
        })
    }

    #[cfg(unix)]
    pub(crate) fn restore_handoff_agent_state(&mut self, snapshot: HandoffAgentState) {
        if let Some(sequence) = snapshot.sequence {
            self.hook_report_sequences
                .insert(snapshot.authority.source.clone(), sequence);
        }
        self.detected_agent = crate::detect::parse_agent_label(&snapshot.authority.agent_label);
        self.state = snapshot.authority.state;
        self.hook_authority = Some(snapshot.authority);
        self.agent_process_acquisition_pending = snapshot.acquisition_pending;
    }

    pub(crate) fn finish_agent_process_acquisition(&mut self) -> bool {
        let reached_idle = self.agent_process_acquisition_pending && self.state == AgentState::Idle;
        let suppress_completion = reached_idle && self.recent_agent_process_exit.is_none();
        if reached_idle {
            self.agent_process_acquisition_pending = false;
        }
        suppress_completion
    }

    pub(crate) fn record_composer_write(
        &mut self,
        source: super::ComposerInputSource,
        baseline_content_seq: u64,
        force_new_attempt: bool,
    ) -> String {
        if !force_new_attempt
            && self
                .composer_write
                .as_ref()
                .is_some_and(|write| write.source == source)
        {
            if let Some(write) = self.composer_write.as_mut() {
                write.baseline_content_seq = baseline_content_seq;
                return write.attempt_id.clone();
            }
        }
        let attempt_id = super::composer::fresh_composer_attempt_id();
        self.composer_write = Some(super::ComposerWrite {
            attempt_id: attempt_id.clone(),
            source,
            baseline_content_seq,
        });
        attempt_id
    }
    pub(crate) fn reset_turn_counter(&mut self, path: TurnCounterResetPath) {
        self.turn = 0;
        self.turn_epoch = fresh_turn_epoch_for(path);
        self.turn_records.clear();
        self.turn_abort_pending_at = None;
    }

    pub(crate) fn mark_turn_aborted(&mut self) {
        self.mark_turn_aborted_at(Instant::now());
    }

    pub(crate) fn mark_turn_aborted_at(&mut self, now: Instant) {
        if matches!(self.state, AgentState::Working | AgentState::Blocked) {
            self.turn_abort_pending_at = Some(now);
        }
    }

    pub(crate) fn turn_completion_context(&self) -> TurnCompletionContext {
        let message = self
            .hook_authority
            .as_ref()
            .and_then(|authority| authority.message.clone());
        let agent_session_path =
            self.current_session_identity_for_persistence()
                .and_then(|(_, _, kind, value)| {
                    (kind == crate::agent_resume::AgentSessionRefKind::Path).then_some(value)
                });
        TurnCompletionContext {
            message,
            agent_session_path,
        }
    }

    #[cfg(test)]
    pub(crate) fn record_completed_turn(
        &mut self,
        completed_unix_ms: u64,
        fallback_context: TurnCompletionContext,
    ) -> TurnRecord {
        self.record_completed_turn_at(completed_unix_ms, fallback_context, Instant::now())
    }

    pub(crate) fn record_completed_turn_at(
        &mut self,
        completed_unix_ms: u64,
        fallback_context: TurnCompletionContext,
        now: Instant,
    ) -> TurnRecord {
        self.turn = self.turn.saturating_add(1);
        // A completed turn is the recovery point: whatever the last prompt's
        // fate was, it is no longer the pane's current state. Without this a
        // stale `true` outlives operator recovery forever (#26 review F3).
        //
        // RESOLVE HERE, not at report time. The previous design asked the
        // reporting path for "watch present AND turn > turn_at_write", but this
        // function increments the turn and destroys the watch in the same
        // synchronous call — so mid-turn the predicate was false by design and
        // post-completion the watch was gone. The state it needed was
        // unreachable in production, and the #31 incident still reproduced.
        // The submitted fact is only knowable at the instant it is discarded, so
        // it is converted into a durable one right here.
        if let Some(watch) = self.prompt_submit_abandoned.take() {
            if watch.submitted.load(std::sync::atomic::Ordering::SeqCst) {
                self.prompt_claim_retired = true;
            }
        }
        self.displaced_submit_unknown = false;
        let current_context = self.turn_completion_context();
        let message = current_context.message.or(fallback_context.message);
        let (message, message_truncated) = message
            .map(truncate_turn_message)
            .map_or((None, false), |(message, truncated)| {
                (Some(message), truncated)
            });
        let interrupted_within_window =
            self.turn_abort_pending_at
                .take()
                .is_some_and(|interrupted_at| {
                    now.saturating_duration_since(interrupted_at) <= TURN_ABORT_WINDOW
                });
        let record = TurnRecord {
            turn: self.turn,
            turn_epoch: self.turn_epoch,
            outcome: if interrupted_within_window {
                TurnOutcome::Aborted
            } else {
                TurnOutcome::Completed
            },
            completed_unix_ms,
            message,
            message_truncated,
            agent_session_path: current_context
                .agent_session_path
                .or(fallback_context.agent_session_path),
        };
        if self.turn_records.len() == TURN_RECORD_LIMIT {
            self.turn_records.pop_front();
        }
        self.turn_records.push_back(record.clone());
        record
    }

    pub(crate) fn last_completed_turn(&self) -> Option<&TurnRecord> {
        self.turn_records.back()
    }

    pub(crate) fn replay_turns(
        &self,
        since: Option<u64>,
        expected_epoch: Option<u64>,
    ) -> Result<TurnReplay, TurnReplayError> {
        if let Some(expected) = expected_epoch {
            if expected != self.turn_epoch {
                return Err(TurnReplayError::EpochMismatch {
                    expected,
                    actual: self.turn_epoch,
                });
            }
        }
        if let Some(since) = since {
            if since > self.turn {
                return Err(TurnReplayError::SinceAhead {
                    since,
                    newest: self.turn,
                });
            }
        }

        let oldest_available = self.turn_records.front().map(|record| record.turn);
        let since = since.unwrap_or(0);
        let truncated = oldest_available.is_some_and(|oldest| oldest > since.saturating_add(1));
        let records = if truncated {
            self.turn_records.iter().cloned().collect()
        } else {
            self.turn_records
                .iter()
                .filter(|record| record.turn > since)
                .cloned()
                .collect()
        };
        Ok(TurnReplay {
            turn_epoch: self.turn_epoch,
            records,
            truncated,
            oldest_available,
        })
    }

    pub(crate) fn terminal_title_stripped(&self) -> Option<String> {
        self.terminal_title
            .as_deref()
            .and_then(super::stripped_terminal_title)
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

    /// Rehydrate the persisted occupant generation on restore, so the terminal's
    /// global identity survives a daemon restart. Additive — old snapshots supply
    /// 0, the same as a freshly created terminal.
    pub fn with_occupant_generation(mut self, occupant_generation: u64) -> Self {
        self.occupant_generation = occupant_generation;
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
        _visible_working: bool,
        process_exited: bool,
        now: Instant,
    ) -> TerminalStateMutation {
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_detected_agent = self.detected_agent;
        let previous_session = self.current_session_identity_for_persistence();
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
        if self.should_ignore_detected_state_under_full_lifecycle_hook(agent, process_exited) {
            if self
                .hook_authority
                .as_ref()
                .and_then(|authority| crate::detect::parse_agent_label(&authority.agent_label))
                == agent
            {
                self.detected_agent = agent;
            }
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
                agent_released: false,
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
                agent_released: false,
            };
        }
        if previous_detected_agent != agent {
            self.composer_write = None;
        }
        self.detected_agent = agent;
        if process_exited || agent != Some(Agent::Codex) || fallback_state == AgentState::Blocked {
            self.codex_prompt_ready = false;
        }
        if let Some(agent) = agent {
            let agent_label = crate::detect::agent_label(agent);
            let transfer_is_changing_owner = process_exited
                && self
                    .session_transfer
                    .as_ref()
                    .is_some_and(|transfer| transfer.preserves_agent_name_on_process_exit());
            if !transfer_is_changing_owner {
                self.reconcile_agent_name_owner(agent_label, None);
            }
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
                self.hook_authority = None;
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
            if !newer_custom_authority
                && agent.is_some()
                && self
                    .reported_resume
                    .as_ref()
                    .is_some_and(|resume| crate::detect::parse_agent_label(&resume.agent) == agent)
            {
                self.set_reported_resume(None);
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
            self.hook_authority = None;
            self.persisted_agent_session = durable_session;
        }
        if agent_released {
            self.composer_write = None;
        }
        // A process-exit notification is not enough to release a name: the
        // agent may still occupy the pane. Once a later observation confirms
        // absence, preserve deliberate restart/transfer identity while
        // releasing its old session anchor; all other exits free the name.
        if agent.is_none() && self.recent_agent_process_exit.is_some() {
            if self.pending_agent_resume_plan.is_some()
                || self
                    .session_transfer
                    .as_ref()
                    .is_some_and(|transfer| transfer.preserves_agent_name_on_process_exit())
            {
                self.release_agent_name_session_anchor();
            } else {
                self.clear_agent_name();
            }
        }
        let effective_state_change = self.recompute_effective_state(
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            now,
        );
        if fallback_state == AgentState::Working && self.state == AgentState::Working {
            self.agent_process_acquisition_pending = false;
        }
        TerminalStateMutation {
            effective_state_change,
            session_ref_changed: previous_session
                != self.current_session_identity_for_persistence(),
            agent_released,
        }
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
        let owner_conflicts = self.current_session_owner_conflicts(&source, &agent_label);
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

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.reconcile_agent_name_owner(&agent_label, session_ref.as_ref());
        if foreground_takeover_allowed {
            // A genuinely different occupant is seizing the terminal (owner
            // conflict + confirmed takeover). Bump the occupant generation here
            // and ONLY here — an ordinary same-owner hook update never reaches
            // this branch, so it does not advance the generation.
            self.occupant_generation = self.occupant_generation.wrapping_add(1);
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
        self.persisted_agent_session = None;
        self.hook_authority = Some(HookAuthority {
            source,
            agent_label,
            state,
            message,
            reported_at: now,
            session_ref,
        });
        let current_session = self.current_session_identity_for_persistence();
        let effective_state_change = self.recompute_effective_state(
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            now,
        );
        if state == AgentState::Working && self.state == AgentState::Working {
            self.agent_process_acquisition_pending = false;
        }
        Some(TerminalStateMutation {
            effective_state_change,
            session_ref_changed: previous_session != current_session,
            agent_released: false,
        })
    }

    fn hook_authority_not_newer_than(&self, observed_at: Instant) -> bool {
        self.hook_authority
            .as_ref()
            .is_none_or(|authority| authority.reported_at <= observed_at)
    }

    fn fallback_not_older_than_hook(&self) -> bool {
        self.hook_authority.as_ref().is_none_or(|authority| {
            self.fallback_observed_at
                .is_some_and(|observed_at| authority.reported_at <= observed_at)
        })
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
                    session_ref: Some(session_ref),
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
                self.hook_authority = Some(pending.authority);
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
                | ("herdr:grok", "grok", Some("new"))
                | (
                    "herdr:omp",
                    "omp",
                    Some("startup" | "new" | "resume" | "fork")
                )
                | (
                    "herdr:qwen",
                    "qwen",
                    Some("startup" | "clear" | "resume" | "compact" | "branch")
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

    pub fn set_managed_agent_launch_session(
        &mut self,
        session: crate::agent_resume::PersistedAgentSession,
    ) {
        self.persisted_agent_session = Some(session.clone());
        self.managed_agent_launch_session = Some(session);
    }

    pub(crate) fn set_reported_agent_session_path(
        &mut self,
        source: &str,
        agent: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
        path: Option<String>,
    ) {
        self.reported_agent_session_path = match (session_ref.kind, path) {
            (crate::agent_resume::AgentSessionRefKind::Id, Some(path)) => {
                Some(ReportedAgentSessionPath {
                    source: source.to_string(),
                    agent: agent.to_string(),
                    session_id: session_ref.value.clone(),
                    path,
                })
            }
            _ => None,
        };
    }

    pub(crate) fn reported_agent_session_path_for(
        &self,
        source: &str,
        agent: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> Option<&str> {
        self.reported_agent_session_path
            .as_ref()
            .filter(|reported| {
                session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                    && reported.source == source
                    && reported.agent == agent
                    && reported.session_id == session_ref.value
            })
            .map(|reported| reported.path.as_str())
    }

    pub(crate) fn set_reported_agent_session_runtime(
        &mut self,
        source: &str,
        agent: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
        cursor: Option<String>,
        process_pid: Option<u32>,
    ) {
        self.reported_agent_session_runtime = Some(ReportedAgentSessionRuntime {
            source: source.to_string(),
            agent: agent.to_string(),
            session_ref: session_ref.clone(),
            cursor,
            process_pid,
        });
    }

    pub(crate) fn reported_agent_session_runtime_for(
        &self,
        source: &str,
        agent: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> Option<&ReportedAgentSessionRuntime> {
        self.reported_agent_session_runtime
            .as_ref()
            .filter(|reported| {
                reported.source == source
                    && reported.agent == agent
                    && &reported.session_ref == session_ref
            })
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
                let mutation = TerminalStateMutation {
                    effective_state_change: self.recompute_effective_state(
                        previous_agent_label,
                        previous_known_agent,
                        previous_state,
                        previous_presentation,
                        now,
                    ),
                    session_ref_changed: previous_session != current_session,
                    agent_released: false,
                };
                self.accepted_session_report_generation =
                    self.accepted_session_report_generation.wrapping_add(1);
                return Some(mutation);
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
            self.hook_authority = None;
        } else if foreground_takeover_allowed {
            self.suppress_current_full_lifecycle_hook_authority(
                FullLifecycleHookSuppressionReason::HookClear,
            );
            self.hook_authority = None;
        }
        self.reconcile_agent_name_owner(&agent_label, Some(&session_ref));
        let persisted_session = crate::agent_resume::PersistedAgentSession {
            source,
            agent: agent_label,
            session_ref,
        };
        if self.managed_agent_launch_session.as_ref() == Some(&persisted_session) {
            self.managed_agent_launch_session = None;
        }
        self.persisted_agent_session = Some(persisted_session);
        let current_session = self.current_session_identity_for_persistence();
        if previous_session.is_some() && previous_session != current_session {
            // Rebinding can expose a cached Working screen; only a fresh report ends acquisition.
            self.agent_process_acquisition_pending = true;
        }
        let mutation = TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
            agent_released: false,
        };
        self.accepted_session_report_generation =
            self.accepted_session_report_generation.wrapping_add(1);
        Some(mutation)
    }

    pub(crate) fn accepted_session_report_generation(&self) -> u64 {
        self.accepted_session_report_generation
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
        (source, agent_label) != ("herdr:grok", "grok")
            && Self::session_start_source_is_recognized(session_start_source)
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
        if !self.hook_report_is_newer(source, seq) {
            return false;
        }
        if let Some(seq) = seq {
            self.hook_report_sequences.insert(source.to_string(), seq);
        }
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
        // Keep detected_agent: clearing hook authority hands control back to
        // the detector for the same live process. Only the stale fallback
        // activity state may not survive — it would resurrect and mint a
        // phantom detector turn.
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_observed_at = None;
        if let Some(authority) = self.hook_authority.take() {
            self.forget_reported_resume_of(&authority.source, &authority.agent_label);
        }
        self.persisted_agent_session = None;
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session.is_some(),
            agent_released: false,
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
        // A release invalidates detector activity evidence even when the live process still
        // owns the agent identity. Fresh detector evidence must restore the fallback state.
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_observed_at = None;
        if !process_owns_agent {
            self.detected_agent = None;
            self.clear_agent_name();
        }
        self.hook_authority = None;
        self.composer_write = None;
        self.forget_reported_resume_of(source, agent_label);
        if !preserve_foreign_persisted_session {
            self.persisted_agent_session = None;
        }
        let current_session = self.current_session_identity_for_persistence();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
            agent_released: !process_owns_agent,
        })
    }

    /// A reporter that Herdr cannot also identify by process holds this pane,
    /// so only the pane returning to its shell can tell Herdr it exited.
    pub fn self_reported_agent_active(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::parse_agent_label(&authority.agent_label).is_none()
        })
    }

    /// `observed_at` is when the idle shell was seen; a claim made after that
    /// belongs to a newer agent and must survive the delayed signal.
    pub fn clear_self_reported_agent(
        &mut self,
        observed_at: Instant,
    ) -> Option<TerminalStateMutation> {
        if !self.self_reported_agent_active() || !self.hook_authority_not_newer_than(observed_at) {
            return None;
        }
        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.hook_authority = None;
        self.set_reported_resume(None);
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_observed_at = None;
        self.clear_agent_name();
        let current_session = self.current_session_identity_for_persistence();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
            agent_released: true,
        })
    }

    pub fn session_ref_is_current(
        &self,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> bool {
        self.current_session_identity_for_persistence()
            .is_some_and(|(_, _, kind, value)| {
                kind == session_ref.kind && value == session_ref.value
            })
    }

    /// A reporter may own the resume command when it holds the pane, or when it
    /// is an agent Herdr currently sees running there and can therefore see exit.
    pub fn can_record_reported_resume(&self, source: &str, agent_label: &str) -> bool {
        match self.hook_authority.as_ref() {
            Some(authority) => authority.source == source && authority.agent_label == agent_label,
            None => {
                self.recent_agent_process_exit.is_none()
                    && self.detected_agent.is_some()
                    && crate::detect::parse_agent_label(agent_label) == self.detected_agent
            }
        }
    }

    /// Same ordering rule as lifecycle reports, evaluated before the report is
    /// applied so a duplicate cannot pass as the report that was just accepted.
    pub fn hook_report_is_newer(&self, source: &str, seq: Option<u64>) -> bool {
        let last_seq = self.hook_report_sequences.get(source).copied();
        match seq {
            Some(seq) => last_seq.is_none_or(|last| seq > last),
            None => last_seq.is_none(),
        }
    }

    /// Callers must first check `hook_report_is_newer` against the state
    /// before the carrying report was applied.
    pub fn record_reported_resume(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
        argv: Vec<String>,
    ) -> bool {
        if !self.can_record_reported_resume(source, agent_label) {
            return false;
        }
        if let Some(seq) = seq {
            let last = self
                .hook_report_sequences
                .entry(source.to_string())
                .or_insert(seq);
            *last = (*last).max(seq);
        }
        let resume = crate::agent_resume::ReportedAgentResume {
            source: source.to_string(),
            agent: agent_label.to_string(),
            argv,
        };
        if self.reported_resume.as_ref() == Some(&resume) {
            return false;
        }
        self.set_reported_resume(Some(resume));
        true
    }

    pub fn reported_resume(&self) -> Option<&crate::agent_resume::ReportedAgentResume> {
        self.reported_resume.as_ref()
    }

    /// Changes whenever the stored resume command changes, so callers can
    /// persist removals as well as new commands.
    pub fn reported_resume_revision(&self) -> u64 {
        self.reported_resume_revision
    }

    /// Drops the command once a different agent holds the pane.
    pub fn reconcile_reported_resume(&mut self) {
        let Some(resume) = self.reported_resume.as_ref() else {
            return;
        };
        let other_authority = self.hook_authority.as_ref().is_some_and(|authority| {
            authority.source != resume.source || authority.agent_label != resume.agent
        });
        let other_process = self
            .detected_agent
            .is_some_and(|agent| crate::detect::agent_label(agent) != resume.agent);
        if other_authority || other_process {
            self.set_reported_resume(None);
        }
    }

    pub fn restore_reported_resume(&mut self, resume: crate::agent_resume::ReportedAgentResume) {
        self.set_reported_resume(Some(resume));
    }

    fn set_reported_resume(&mut self, resume: Option<crate::agent_resume::ReportedAgentResume>) {
        if self.reported_resume != resume {
            self.reported_resume = resume;
            self.reported_resume_revision += 1;
        }
    }

    fn forget_reported_resume_of(&mut self, source: &str, agent_label: &str) {
        if self
            .reported_resume
            .as_ref()
            .is_some_and(|resume| resume.source == source && resume.agent == agent_label)
        {
            self.set_reported_resume(None);
        }
    }

    fn hook_authority_is_effective(&self, authority: &HookAuthority) -> bool {
        !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
            || crate::detect::parse_agent_label(&authority.agent_label).is_none_or(|agent| {
                self.detected_agent == Some(agent) && self.recent_agent_process_exit.is_none()
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

    pub fn set_input_prompt_kind(&mut self, kind: Option<crate::detect::InputPromptKind>) -> bool {
        let pending = kind.is_some();
        if self.input_pending == pending && self.input_prompt_kind == kind {
            return false;
        }
        self.input_pending = pending;
        self.input_prompt_kind = kind;
        self.revision = self.revision.saturating_add(1);
        true
    }

    pub fn full_lifecycle_hook_authority_active(&self) -> bool {
        self.live_full_lifecycle_hook_authority()
    }

    fn visible_blocker_overrides_hook(&self) -> bool {
        if self.live_full_lifecycle_hook_authority() {
            return false;
        }
        self.fallback_visible_blocker
            && self.fallback_not_older_than_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority.state != AgentState::Blocked
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            })
    }

    fn live_full_lifecycle_hook_authority(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            self.hook_authority_is_effective(authority)
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
        self.codex_prompt_ready = false;
        self.set_agent_name(name);
        self.agent_process_acquisition_pending = true;
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

    pub fn observe_codex_prompt_ready(&mut self, ready: bool) -> Option<TerminalStateMutation> {
        if self.detected_agent != Some(Agent::Codex)
            || self.recent_agent_process_exit.is_some()
            || !self.managed_agent.is_some_and(|managed| {
                managed.kind == Agent::Codex && managed.phase != ManagedAgentPhase::Active
            })
            || self.codex_prompt_ready == ready
        {
            return None;
        }
        self.codex_prompt_ready = ready;
        Some(TerminalStateMutation::default())
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
        let transfer_preserves_name = self
            .session_transfer
            .as_ref()
            .is_some_and(|transfer| transfer.preserves_agent_name_on_process_exit());
        let known_agent = self.effective_known_agent();
        let observed_expected = match managed.phase {
            ManagedAgentPhase::Pending {
                observed_expected, ..
            } => observed_expected || known_agent == Some(managed.kind),
            ManagedAgentPhase::Blocked | ManagedAgentPhase::Active => false,
        };
        let clear = !transfer_preserves_name
            && (process_exited
                || known_agent.is_some_and(|agent| agent != managed.kind)
                || matches!(managed.phase, ManagedAgentPhase::Pending { .. })
                    && observed_expected
                    && known_agent.is_none());
        if clear {
            self.clear_agent_name();
            return true;
        }
        if managed.phase == ManagedAgentPhase::Blocked {
            if known_agent == Some(managed.kind)
                && (self.state == AgentState::Idle
                    || managed.kind == Agent::Codex
                        && self.state == AgentState::Unknown
                        && self.codex_prompt_ready)
            {
                self.managed_agent = Some(ManagedAgent {
                    kind: managed.kind,
                    phase: ManagedAgentPhase::Active,
                });
                self.managed_agent_launch_session = None;
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
            if now >= deadline && !transfer_preserves_name {
                self.clear_agent_name();
                return true;
            }
            if ready_after.is_none_or(|ready_after| now >= ready_after) {
                if known_agent == Some(managed.kind)
                    && (self.state == AgentState::Idle
                        || managed.kind == Agent::Codex
                            && self.state == AgentState::Unknown
                            && self.codex_prompt_ready)
                {
                    self.managed_agent = Some(ManagedAgent {
                        kind: managed.kind,
                        phase: ManagedAgentPhase::Active,
                    });
                    self.managed_agent_launch_session = None;
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

    /// Keep the seat's name but forget WHICH session owned it.
    ///
    /// Used when this pane is deliberately restarted into a new session of the SAME seat
    /// (an `agent.restart`, including an account swap). `reconcile_agent_name_owner`
    /// adopts an incoming session ref when the owner holds none, so the name transfers to
    /// the resumed session instead of being disowned as a mismatch.
    ///
    /// Deliberately does NOT touch `managed_agent`: the seat is still running the same
    /// kind of agent, and that field is what exempts a managed agent from the
    /// replacement check at all.
    pub fn release_agent_name_session_anchor(&mut self) {
        if let Some(owner) = self.agent_name_owner.as_mut() {
            owner.session_ref = None;
        }
    }

    pub fn clear_agent_name(&mut self) {
        self.codex_prompt_ready = false;
        if self
            .managed_agent_launch_session
            .take()
            .as_ref()
            .is_some_and(|session| self.persisted_agent_session.as_ref() == Some(session))
        {
            self.persisted_agent_session = None;
        }
        self.agent_name = None;
        self.agent_name_owner = None;
        self.managed_agent = None;
    }

    pub fn clear_agent_runtime_identity_after_respawn(&mut self) {
        self.reset_turn_counter(TurnCounterResetPath::PaneRespawn);
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_observed_at = None;
        self.hook_authority = None;
        self.persisted_agent_session = None;
        self.agent_metadata.clear();
        self.metadata_report_agents.clear();
        self.suppressed_full_lifecycle_hook_reports.clear();
        self.stale_full_lifecycle_hook_sessions.clear();
        self.state = AgentState::Unknown;
        self.input_pending = false;
        self.input_prompt_kind = None;
        self.composer_write = None;
        self.last_agent_state_change_seq = None;
        self.agent_status_since_unix_ms = None;
        self.last_agent_completion_seq = None;
        self.launch_argv = None;
        self.respawn_shell_on_exit = false;
        self.recent_agent_process_exit = None;
        self.agent_process_acquisition_pending = false;
        self.pending_agent_resume_plan = None;
        self.clear_agent_name();
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
        if self
            .session_transfer
            .as_ref()
            .and_then(crate::session_transfer::RuntimeSessionTransfer::expected_agent_name_owner)
            == Some(agent_label)
        {
            match self.agent_name_owner.as_mut() {
                Some(owner) => {
                    owner.agent_label = agent_label.to_string();
                    if session_ref.is_some() {
                        owner.session_ref = session_ref.cloned();
                    }
                }
                None => {
                    self.agent_name_owner = Some(AgentNameOwner {
                        agent_label: agent_label.to_string(),
                        session_ref: session_ref.cloned(),
                    });
                }
            }
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

    fn recompute_effective_state(
        &mut self,
        previous_agent_label: Option<String>,
        previous_known_agent: Option<Agent>,
        previous_state: AgentState,
        previous_presentation: EffectivePresentation,
        now: Instant,
    ) -> Option<EffectiveStateChange> {
        let state = if self.visible_blocker_overrides_hook() {
            AgentState::Blocked
        } else {
            self.hook_authority
                .as_ref()
                .filter(|authority| self.hook_authority_is_effective(authority))
                .map(|authority| authority.state)
                .unwrap_or(self.fallback_state)
        };
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();

        let presentation = self.effective_presentation_for_state_at(state, now);
        self.clear_expiry_pending_for_hidden_metadata();

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

    /// A platform-absolute session path.
    ///
    /// `AgentSessionRef::path` requires `Path::is_absolute`, and "/tmp/x.jsonl" is
    /// NOT absolute on Windows (no drive), so the literal returned `None` there and
    /// the fixture's `unwrap` panicked in CI while passing on unix.
    fn absolute_session_path(name: &str) -> String {
        std::env::temp_dir().join(name).display().to_string()
    }
    use super::*;
    use crate::detect::AgentDetection;

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
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
    fn managed_agent_readiness_tracks_detection_state() {
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
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(100), false));
        assert!(terminal.managed_agent_launch_pending());

        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        assert!(!terminal.reconcile_managed_agent_at(now + Duration::from_millis(101), false));

        terminal.set_detected_state(Some(Agent::Pi), AgentState::Blocked);
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(102), false));
        assert!(terminal.managed_agent_launch_pending());
        assert!(!terminal.managed_agent_interactive_ready());
        assert_eq!(terminal.next_managed_agent_deadline(), None);
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(!terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), false));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), false));
        assert!(!terminal.managed_agent_launch_pending());
        assert!(terminal.managed_agent_interactive_ready());

        terminal.set_detected_state(None, AgentState::Unknown);
        assert!(terminal.managed_agent_interactive_ready());
        assert!(!terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), false));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_secs(2), true));
        assert_eq!(terminal.agent_name, None);
    }

    #[test]
    fn codex_managed_readiness_requires_current_prompt_without_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::Codex,
            now,
            Duration::from_millis(100),
            Duration::from_secs(1),
        );
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Unknown);
        terminal.observe_codex_prompt_ready(true);
        terminal.reconcile_managed_agent_at(now, false);
        assert!(!terminal.managed_agent_interactive_ready());
        terminal.observe_codex_prompt_ready(false);
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(100), false));
        assert!(!terminal.managed_agent_interactive_ready());

        terminal.observe_codex_prompt_ready(true);
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(101), false));
        assert!(terminal.managed_agent_interactive_ready());
        assert_eq!(terminal.state, AgentState::Unknown);
        terminal.observe_codex_prompt_ready(false);
        assert!(terminal.managed_agent_interactive_ready());

        terminal.begin_managed_agent(
            "next".into(),
            Agent::Codex,
            now,
            Duration::ZERO,
            Duration::from_secs(1),
        );
        terminal.reconcile_managed_agent_at(now, false);
        assert!(!terminal.managed_agent_interactive_ready());

        terminal.set_detected_state(Some(Agent::Codex), AgentState::Blocked);
        assert!(terminal.reconcile_managed_agent_at(now, false));
        terminal.observe_codex_prompt_ready(false);
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Unknown);
        assert!(!terminal.reconcile_managed_agent_at(now, false));
        terminal.observe_codex_prompt_ready(true);
        assert!(terminal.reconcile_managed_agent_at(now, false));
        assert!(terminal.managed_agent_interactive_ready());
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
        timed_out.set_managed_agent_launch_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        assert!(timed_out.reconcile_managed_agent_at(now + Duration::from_millis(20), false));
        assert_eq!(timed_out.agent_name, None);
        assert_eq!(timed_out.managed_agent_kind(), None);
        assert!(timed_out.persisted_agent_session.is_none());
    }

    #[test]
    fn stabilization_uses_raw_policy_state() {
        let detection = AgentDetection {
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
    fn omp_hook_authority_overrides_detected_fallback() {
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

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
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
            fresh_process_seen_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
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
            process_exit_at + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
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
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
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
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
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
            now + Duration::from_millis(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Omp),
            AgentState::Unknown,
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
    fn visible_blocker_does_not_override_full_lifecycle_hook_authority() {
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

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
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
            now + Duration::from_secs(10),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
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
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
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
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
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
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
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
            now + Duration::from_millis(1),
        );

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
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
    fn qwen_lifecycle_session_ref_replaces_existing_session_ref() {
        for session_start_source in ["startup", "clear", "resume", "compact", "branch"] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(Agent::Qwen), AgentState::Idle);
            terminal
                .set_agent_session_ref(
                    "herdr:qwen".into(),
                    "qwen".into(),
                    crate::agent_resume::AgentSessionRef::id("qwen-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("qwen-{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:qwen".into(),
                    "qwen".into(),
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
    fn qwen_session_ref_does_not_replace_without_foreground_qwen() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:qwen".into(),
                "qwen".into(),
                crate::agent_resume::AgentSessionRef::id("qwen-parent"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:qwen".into(),
            "qwen".into(),
            crate::agent_resume::AgentSessionRef::id("qwen-branch"),
            Some(21),
            Some("branch".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("qwen-parent")
        );
    }

    #[test]
    fn grok_new_session_ref_replaces_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:grok".into(),
                "grok".into(),
                crate::agent_resume::AgentSessionRef::id("grok-old"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:grok".into(),
                "grok".into(),
                crate::agent_resume::AgentSessionRef::id("grok-new"),
                Some(21),
                Some("new".into()),
            )
            .expect("new should replace the grok session");

        assert!(mutation.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("grok-new")
        );
    }

    #[test]
    fn opencode_server_new_does_not_replace_existing_session_ref() {
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
    fn opencode_child_prompt_reports_with_root_id_preserve_lifecycle_authority() {
        let mut terminal = test_terminal();
        let root = crate::agent_resume::AgentSessionRef::id("opencode-root").unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            root.clone(),
        );

        // The plugin projects child permission/question events onto their root.
        for (seq, state) in [
            (20, AgentState::Working),
            (21, AgentState::Blocked),
            (22, AgentState::Working),
            (23, AgentState::Idle),
        ] {
            let mutation = terminal
                .set_hook_authority_with_session_ref(
                    "herdr:opencode".into(),
                    "opencode".into(),
                    state,
                    None,
                    Some(root.clone()),
                    Some(seq),
                )
                .expect("root-scoped lifecycle report should be accepted");
            assert!(!mutation.session_ref_changed);
            assert_eq!(terminal.state, state);
            assert_eq!(
                terminal
                    .hook_authority
                    .as_ref()
                    .unwrap()
                    .session_ref
                    .as_ref(),
                Some(&root)
            );
        }

        let foreign_child_prompt = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Blocked,
            None,
            crate::agent_resume::AgentSessionRef::id("opencode-other-root"),
            Some(24),
        );
        assert!(foreign_child_prompt.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
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
    fn grok_new_session_does_not_replace_a_different_owner() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Grok), AgentState::Idle);

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:grok".into(),
            "grok".into(),
            crate::agent_resume::AgentSessionRef::id("grok-session"),
            Some(21),
            Some("new".into()),
        );

        assert!(mutation.is_none());
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
            crate::agent_resume::AgentSessionRef::path(absolute_session_path("pi-session.jsonl")),
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
    fn a_process_exit_observation_alone_does_not_free_the_name() {
        // The pane keeps reporting the same agent throughout: this models a
        // process-exit observation that is wrong (the agent is still running),
        // which is what `agent_alias_survives_detection_uncertainty_...`
        // already refuses to let destroy the name through the detection path.
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());

        // Contract today: losing detection does not cost the pane its name.
        terminal.set_detected_state(None, AgentState::Unknown);
        assert_eq!(
            terminal.agent_name.as_deref(),
            Some("reviewer"),
            "detection uncertainty must not release the name"
        );
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);

        // A single process-exit observation for that same agent destroys it.
        let exit = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            now,
        );
        assert!(exit.agent_released);
        assert_eq!(
            terminal.agent_name.as_deref(),
            Some("reviewer"),
            "a process-exit observation for the still-detected agent must not \
             destroy a name that detection uncertainty is allowed to keep"
        );

        // And the agent proving it is alive again must not leave the pane
        // permanently unreachable by the name its owner assigned.
        terminal.set_detected_agent_process_at(Agent::Pi, now + Duration::from_secs(1));
        assert_eq!(
            terminal.agent_name.as_deref(),
            Some("reviewer"),
            "the name must still resolve once the agent is observed alive again"
        );
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
    }

    #[test]
    fn a_confirmed_agent_exit_still_frees_the_name_for_reuse() {
        // The other side of `a_process_exit_observation_alone_does_not_free_the_name`:
        // once the agent is actually gone from the pane the name must be
        // released, so `agent start` can reuse it.
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            now,
        );
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        // The agent really is gone: the next observation finds no agent while
        // the recorded exit still stands.
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_secs(1),
        );
        assert_eq!(
            terminal.agent_name, None,
            "a confirmed exit must release the name"
        );
        assert!(!terminal.is_agent_terminal());
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
    fn custom_release_preserves_process_owned_identity_but_resets_activity() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_agent_name("reviewer".into());
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
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(terminal.state, AgentState::Unknown);
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
    fn occupant_generation_starts_at_zero() {
        assert_eq!(test_terminal().occupant_generation, 0);
    }

    #[test]
    fn occupant_generation_bumps_on_a_different_agent_takeover() {
        let mut terminal = test_terminal();
        assert_eq!(terminal.occupant_generation, 0, "starts at 0");

        // Occupant A: a persisted codex session establishes the current owner.
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        // Detection now matches the INCOMING agent B (claude), so a foreground
        // takeover by claude is confirmed.
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        terminal
            .set_hook_authority_at(
                "herdr:claude".into(),
                "claude".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(21),
                Instant::now(),
            )
            .expect("foreground claude should take over the terminal");

        assert_eq!(
            terminal.effective_known_agent(),
            Some(Agent::Claude),
            "the terminal should now be owned by claude"
        );
        assert_eq!(
            terminal.occupant_generation, 1,
            "a genuine different-occupant takeover bumps the occupant generation"
        );
    }

    #[test]
    fn occupant_generation_unchanged_on_same_occupant_hook_update() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
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
        assert_eq!(terminal.occupant_generation, 0);

        // A later update from the SAME occupant is an ordinary hook update, not a
        // takeover: it must not advance the occupant generation.
        terminal
            .set_hook_authority_at(
                "custom:agent".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                None,
                Some(21),
                Instant::now(),
            )
            .expect("same-owner update should be accepted");
        assert_eq!(
            terminal.occupant_generation, 0,
            "an ordinary same-occupant hook update must not bump the generation"
        );
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

    /// A DELIBERATE RESTART KEEPS THE SEAT'S NAME (issue #128).
    ///
    /// `agent.restart` — which is how an account swap is done — arms a resume plan and
    /// kills the process itself. The resulting exit was being read as "the agent released
    /// this pane", wiping the name; gitmoot resolves role -> pane by NAME, so the seat
    /// went quietly unaddressable. The replacement then resumes under a NEW session ref,
    /// which is the second way the name was lost, so both are exercised here.
    #[test]
    fn a_deliberate_same_seat_restart_keeps_the_name_across_a_new_session() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("before.jsonl")).unwrap(),
        );
        terminal.set_agent_name("keephair".into());
        assert_eq!(terminal.agent_name.as_deref(), Some("keephair"));

        // What `agent.restart` does before killing the process.
        terminal.pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "pi".into(),
            argv: vec!["pi".into(), "--resume".into()],
            dedupe_key: "k".into(),
        });

        // The kill we asked for.
        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            true, // process_exited
        );
        // The next detection pass confirms the old process is gone.
        terminal.set_detected_state_with_visible_blocker(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
        );
        assert_eq!(
            terminal.agent_name.as_deref(),
            Some("keephair"),
            "our own restart must not be read as the agent letting go of the pane"
        );
        // The MECHANISM, asserted directly rather than inferred. The replacement resumes
        // under a new session ref, and only an owner holding NO anchor gets adopted
        // instead of being treated as a takeover. Asserting just "the name survived"
        // passed even with the anchor left in place, because the adoption path is not
        // always reached from here — so that assertion alone could not see this go wrong.
        assert!(
            terminal
                .agent_name_owner
                .as_ref()
                .is_some_and(|owner| owner.session_ref.is_none()),
            "the session anchor must be released so the resumed session is adopted"
        );

        // The replacement comes back under a DIFFERENT session ref, via the SessionStart
        // the resumed harness actually emits. The released anchor must let it be adopted
        // rather than treated as a takeover.
        terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(test_session_path("after.jsonl")),
            Some(40),
            None,
        );

        assert_eq!(
            terminal.agent_name.as_deref(),
            Some("keephair"),
            "the name must transfer to the resumed session"
        );
    }

    /// A session-transfer resume plan is consumed before the replacement process starts.
    /// The source PaneDied event may arrive in that interval, so the active transaction—not
    /// only the one-shot plan—must keep ownership of the durable pane name.
    #[test]
    fn an_active_session_transfer_keeps_the_name_after_its_plan_is_consumed() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Claude,
            "herdr:claude",
            "claude",
            crate::agent_resume::AgentSessionRef::id("source-session").unwrap(),
        );
        let managed_at = Instant::now();
        terminal.begin_managed_agent(
            "jarvis".into(),
            Agent::Codex,
            managed_at,
            Duration::from_secs(3),
            Duration::from_secs(30),
        );
        terminal.pending_agent_resume_plan = None;
        terminal.session_transfer = Some(crate::session_transfer::RuntimeSessionTransfer {
            id: "transfer-1".into(),
            source_kind: crate::session_transfer::HarnessKind::Claude,
            source_session: crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("source-session").unwrap(),
            },
            source_account: Some("claude-source".into()),
            source_config_home: PathBuf::from("/tmp/claude-source"),
            source_sessions_root: PathBuf::from("/tmp/claude-source"),
            source_cursor: None,
            source_process_pid: None,
            target_kind: crate::session_transfer::HarnessKind::Codex,
            target_account: Some("codex-target".into()),
            target_config_home: PathBuf::from("/tmp/codex-target"),
            target_sessions_root: PathBuf::from("/tmp/codex-target"),
            phase: crate::api::schema::AgentSessionTransferPhase::AwaitingTarget,
            message_count: 2,
            omissions: Default::default(),
            error: None,
            source_path: None,
            source_fingerprint: None,
            target_session_ref: crate::agent_resume::AgentSessionRef::id("target-session"),
            target_cursor: None,
            target_transcript_path: None,
            target_fingerprint: None,
            target_deadline: None,
            target_process: None,
            source_rollback_process: None,
            verification_in_flight: None,
            verification_observation_deadline: None,
            awaiting_deferred_target_report: false,
            target_report_accepted: false,
        });

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            false,
            true,
        );

        assert_eq!(terminal.agent_name.as_deref(), Some("jarvis"));
        assert!(
            terminal
                .agent_name_owner
                .as_ref()
                .is_some_and(|owner| owner.session_ref.is_none()),
            "the target session must be able to adopt the preserved name"
        );

        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert_eq!(terminal.agent_name.as_deref(), Some("jarvis"));
        assert!(terminal
            .agent_name_owner
            .as_ref()
            .is_some_and(|owner| { owner.agent_label == "codex" && owner.session_ref.is_none() }));

        terminal.set_detected_state(None, AgentState::Unknown);
        terminal.reconcile_managed_agent_at(managed_at + Duration::from_secs(31), false);
        assert_eq!(
            terminal.agent_name.as_deref(),
            Some("jarvis"),
            "a vanished target must leave the name available for rollback even after the managed-agent deadline"
        );
    }

    /// THE MIRROR, and the half that keeps the fix scoped rather than merely disabled.
    ///
    /// With no armed resume plan, a process exit is a genuine release and must still drop
    /// the name. Without this, "never clear on exit" would pass the test above too, and a
    /// name would follow a pane that some other agent had taken over.
    #[test]
    fn an_exit_without_an_armed_restart_still_releases_the_name() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("only.jsonl")).unwrap(),
        );
        terminal.set_agent_name("keephair".into());
        assert!(terminal.pending_agent_resume_plan.is_none());

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            true, // process_exited
        );
        // The next detection pass confirms the process is actually gone.
        terminal.set_detected_state_with_visible_blocker(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
        );
        assert!(
            terminal.agent_name.is_none(),
            "an unasked-for exit is a real release and must still clear the name"
        );
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
    fn agent_process_exit_tracks_recent_respawn_window() {
        let mut terminal = test_terminal();
        let now = std::time::Instant::now();

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Idle,
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
    fn turn_ring_eviction_and_since_semantics_are_explicit() {
        let mut terminal = test_terminal();
        let epoch = terminal.turn_epoch;
        for completed_unix_ms in 1..=(TURN_RECORD_LIMIT as u64 + 1) {
            terminal.record_completed_turn(completed_unix_ms, TurnCompletionContext::default());
        }

        let replay = terminal.replay_turns(Some(1), Some(epoch)).unwrap();
        assert!(!replay.truncated);
        assert_eq!(replay.oldest_available, Some(2));
        assert_eq!(replay.records.len(), TURN_RECORD_LIMIT);
        assert_eq!(replay.records.first().map(|record| record.turn), Some(2));
        assert_eq!(
            replay.records.last().map(|record| record.turn),
            Some(TURN_RECORD_LIMIT as u64 + 1)
        );

        let replay = terminal
            .replay_turns(Some(TURN_RECORD_LIMIT as u64), Some(epoch))
            .unwrap();
        assert!(!replay.truncated);
        assert_eq!(replay.records.len(), 1);

        let replay = terminal
            .replay_turns(Some(terminal.turn), Some(epoch))
            .unwrap();
        assert!(!replay.truncated);
        assert!(replay.records.is_empty());

        let from_zero = terminal.replay_turns(Some(0), Some(epoch)).unwrap();
        let from_none = terminal.replay_turns(None, Some(epoch)).unwrap();
        assert!(from_zero.truncated);
        assert_eq!(from_none, from_zero);

        assert_eq!(
            terminal.replay_turns(Some(terminal.turn + 1), Some(epoch)),
            Err(TurnReplayError::SinceAhead {
                since: terminal.turn + 1,
                newest: terminal.turn,
            })
        );
        assert_eq!(
            terminal.replay_turns(None, Some(epoch + 1)),
            Err(TurnReplayError::EpochMismatch {
                expected: epoch + 1,
                actual: epoch,
            })
        );
    }

    #[test]
    fn ignored_esc_then_normal_completion_is_completed_after_abort_window() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        let interrupted_at = Instant::now();
        terminal.mark_turn_aborted_at(interrupted_at);

        let record = terminal.record_completed_turn_at(
            42,
            TurnCompletionContext::default(),
            interrupted_at + TURN_ABORT_WINDOW + Duration::from_millis(1),
        );

        assert_eq!(record.outcome, TurnOutcome::Completed);
    }

    #[test]
    fn esc_then_idle_within_abort_window_is_aborted() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        let interrupted_at = Instant::now();
        terminal.mark_turn_aborted_at(interrupted_at);

        let record = terminal.record_completed_turn_at(
            42,
            TurnCompletionContext::default(),
            interrupted_at + TURN_ABORT_WINDOW,
        );

        assert_eq!(record.outcome, TurnOutcome::Aborted);
    }

    #[test]
    fn turn_message_is_utf8_safe_and_bounded_with_truncation_flag() {
        let mut terminal = test_terminal();
        terminal.hook_authority = Some(HookAuthority {
            source: "herdr:pi".into(),
            agent_label: "pi".into(),
            state: AgentState::Idle,
            message: Some(format!("{}é", "x".repeat(TURN_MESSAGE_MAX_BYTES - 1))),
            reported_at: Instant::now(),
            session_ref: Some(
                crate::agent_resume::AgentSessionRef::path(absolute_session_path(
                    "pi-session.jsonl",
                ))
                .unwrap(),
            ),
        });

        let record = terminal.record_completed_turn(42, TurnCompletionContext::default());

        assert!(record.message_truncated);
        assert!(record
            .message
            .as_ref()
            .is_some_and(|message| message.len() <= TURN_MESSAGE_MAX_BYTES));
        assert_eq!(
            record.agent_session_path,
            Some(absolute_session_path("pi-session.jsonl"))
        );
    }

    #[test]
    fn input_tuple_mutation_preserves_lifecycle_sequence_and_invariant() {
        let mut terminal = test_terminal();
        terminal.last_agent_state_change_seq = Some(41);

        assert!(terminal.set_input_prompt_kind(Some(crate::detect::InputPromptKind::Select)));
        assert!(terminal.input_pending);
        assert_eq!(
            terminal.input_prompt_kind,
            Some(crate::detect::InputPromptKind::Select)
        );
        assert_eq!(terminal.last_agent_state_change_seq, Some(41));

        assert!(terminal.set_input_prompt_kind(None));
        assert!(!terminal.input_pending);
        assert_eq!(terminal.input_prompt_kind, None);
        assert_eq!(terminal.last_agent_state_change_seq, Some(41));
    }
}
