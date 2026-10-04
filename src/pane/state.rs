use crate::detect::AgentState;
use crate::terminal::state::{attention_tier, AttentionTier};
use crate::terminal::{TerminalId, TerminalState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneAgentProjection {
    pub state: AgentState,
    pub seen: bool,
    pub stale: bool,
    pub attention_tier: AttentionTier,
    pub open_blockers: bool,
    pub gate_count: usize,
    pub usage_limited: bool,
    pub waiting_on_agents: bool,
    pub working_while_blocked: bool,
}

impl PaneAgentProjection {
    /// A human-blocked attention tier counts even while independent work keeps
    /// the lifecycle state at `Working`.
    pub(crate) fn counts_as_blocked(self) -> bool {
        self.attention_tier == AttentionTier::Blocked
    }

    pub(crate) fn needs_human_attention(self) -> bool {
        self.attention_tier == AttentionTier::Attention || self.counts_as_blocked()
    }

    pub(crate) fn status_key(self) -> &'static str {
        if self.attention_tier == AttentionTier::Attention {
            return "attention";
        }
        if self.state == AgentState::Working
            && self.attention_tier == AttentionTier::Blocked
            && !self.usage_limited
        {
            return "working";
        }
        if self.counts_as_blocked() {
            return "blocked";
        }
        if self.stale {
            return "stale";
        }
        if self.waiting_on_agents {
            return "waiting_on_agents";
        }
        match (self.state, self.seen) {
            (AgentState::Blocked, _) => "blocked",
            (AgentState::Working, _) => "working",
            (AgentState::Idle, false) => "done",
            (AgentState::Idle, true) => "idle",
            (AgentState::Unknown, _) => "unknown",
        }
    }
}

/// Viewport state for a pane.
///
/// Terminal identity, cwd, labels, and agent metadata live in TerminalState.
pub struct PaneState {
    pub attached_terminal_id: TerminalId,
    /// Marks a non-primary side pane created for an agent tool.
    pub is_companion: bool,
    /// Owner-issued group assignment. It moves with the pane rather than its public locator.
    pub group_membership: crate::groups::PaneGroupMembership,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
    /// Whether unmodified right-click gestures should be forwarded to the pane application.
    pub right_click_passthrough: bool,
    /// When this pane's public lifecycle projection entered Done.
    pub done_since: Option<std::time::Instant>,
    /// Unix timestamp recorded when this pane left the active work set.
    pub settled_at: Option<u64>,
    /// Set once the settled pane's editable grace period has expired.
    pub(crate) settled_locked: bool,
    /// Completed work trigger already consumed by this pane's latest resume.
    pub(crate) settled_work_key: Option<String>,
    /// When the linked work first read as finished for a trigger that has not
    /// settled yet. Runtime-only: a restart re-arms the grace window, which can
    /// only delay settling, never settle a pane early.
    pub(crate) finished_since: Option<std::time::Instant>,
    /// Runtime-only nudge count, carried across server handoff and reset by human input.
    pub(crate) stall_nudges_without_human: u32,
    /// Ignore agent startup transitions after settlement until client input.
    pub(crate) settle_resume_guard: bool,
    pub(crate) activity: Box<crate::activity_age::PaneActivity>,
}

impl PaneState {
    pub fn new(attached_terminal_id: TerminalId) -> Self {
        Self {
            attached_terminal_id,
            is_companion: false,
            group_membership: crate::groups::PaneGroupMembership::default(),
            seen: true,
            right_click_passthrough: false,
            done_since: None,
            settled_at: None,
            settled_locked: false,
            settled_work_key: None,
            finished_since: None,
            stall_nudges_without_human: 0,
            settle_resume_guard: false,
            activity: Box::new(crate::activity_age::PaneActivity::new(
                std::time::Instant::now(),
            )),
        }
    }

    pub(crate) fn snoozed_until(&self) -> Option<u64> {
        self.activity.snoozed_until()
    }

    pub(crate) fn set_snoozed_until(&mut self, deadline: Option<u64>) {
        self.activity.set_snoozed_until(deadline);
    }

    pub(crate) fn take_snoozed_until(&mut self) -> Option<u64> {
        self.activity.take_snoozed_until()
    }

    /// Public agent state after pane-level lifecycle policy is applied.
    /// Settling retires every outstanding demand without rewriting detector state.
    pub(crate) fn agent_projection(&self, terminal: &TerminalState) -> PaneAgentProjection {
        if self.settled_at.is_some() {
            return PaneAgentProjection {
                state: AgentState::Unknown,
                seen: true,
                stale: false,
                attention_tier: AttentionTier::None,
                open_blockers: false,
                gate_count: 0,
                usage_limited: false,
                waiting_on_agents: false,
                working_while_blocked: false,
            };
        }
        let blocking_item_count = terminal
            .closing_items()
            .iter()
            .filter(|item| item.requires_human_input())
            .count();
        let has_closing_gates = !terminal.closing_gates().is_empty();
        let (state, seen) = terminal.sidebar_projection_with_pending_human_input(
            self.seen,
            has_closing_gates || blocking_item_count > 0,
        );
        PaneAgentProjection {
            state,
            seen,
            stale: terminal.supervisor_stale,
            attention_tier: attention_tier(
                state,
                has_closing_gates,
                blocking_item_count > 0,
                terminal.usage_limited,
            ),
            open_blockers: has_closing_gates || blocking_item_count > 0,
            gate_count: terminal.closing_gates().len() + blocking_item_count,
            usage_limited: terminal.usage_limited,
            waiting_on_agents: terminal.waiting_on_agents(),
            working_while_blocked: working_while_blocked(
                state,
                terminal.supervisor_stale,
                terminal.declares_running_subagents(),
                terminal.closing_external_wait().is_some(),
            ),
        }
    }
}

fn working_while_blocked(
    state: AgentState,
    stale: bool,
    raw_waiting_on_agents: bool,
    external_wait: bool,
) -> bool {
    state == AgentState::Blocked && !stale && (raw_waiting_on_agents || external_wait)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_while_blocked_requires_fresh_blocked_state_and_runtime_evidence() {
        assert!(working_while_blocked(
            AgentState::Blocked,
            false,
            true,
            false
        ));
        assert!(working_while_blocked(
            AgentState::Blocked,
            false,
            false,
            true
        ));
        assert!(!working_while_blocked(
            AgentState::Blocked,
            true,
            true,
            false
        ));
        assert!(!working_while_blocked(
            AgentState::Working,
            false,
            true,
            false
        ));
        assert!(!working_while_blocked(
            AgentState::Blocked,
            false,
            false,
            false
        ));
    }

    fn projection(state: AgentState, attention_tier: AttentionTier) -> PaneAgentProjection {
        PaneAgentProjection {
            state,
            seen: true,
            stale: false,
            attention_tier,
            open_blockers: attention_tier == AttentionTier::Blocked,
            gate_count: usize::from(attention_tier == AttentionTier::Blocked),
            usage_limited: false,
            waiting_on_agents: false,
            working_while_blocked: false,
        }
    }

    #[test]
    fn status_key_uses_attention_projection_instead_of_raw_lifecycle() {
        assert_eq!(
            projection(AgentState::Blocked, AttentionTier::Attention).status_key(),
            "attention"
        );
        assert_eq!(
            projection(AgentState::Idle, AttentionTier::Blocked).status_key(),
            "blocked"
        );
        assert_eq!(
            projection(AgentState::Working, AttentionTier::Blocked).status_key(),
            "working"
        );
    }

    #[test]
    fn action_point_items_have_no_nonblocking_attention_category() {
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal.set_raw_agent_state_for_test(AgentState::Idle);
        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Answer".into(),
            text: "Optional preference".into(),
            blocking: false,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let pane = PaneState::new(terminal_id);

        assert_eq!(
            pane.agent_projection(&terminal).attention_tier,
            AttentionTier::Blocked
        );
    }

    #[test]
    fn attention_tier_keeps_blocking_items_and_gates() {
        // MAT-147 AC5
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal.set_raw_agent_state_for_test(AgentState::Idle);
        let pane = PaneState::new(terminal_id);

        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Answer".into(),
            text: "Confirm the requested rollout mode".into(),
            blocking: true,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        assert_eq!(
            pane.agent_projection(&terminal).attention_tier,
            AttentionTier::Blocked
        );

        terminal.closing_items = vec![];
        terminal.closing_gates = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Gate".into(),
            text: "Choose the release path".into(),
            blocking: true,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        assert_eq!(
            pane.agent_projection(&terminal).attention_tier,
            AttentionTier::Blocked
        );
    }
}
