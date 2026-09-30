use std::collections::HashMap;
use std::time::Instant;

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::terminal::state::AttentionTier;
use crate::terminal::{TerminalId, TerminalState};

use super::{Tab, Workspace};

#[cfg(test)]
thread_local! {
    static AGGREGATE_PANE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_aggregate_pane_visit() {
    AGGREGATE_PANE_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[cfg(test)]
pub(crate) fn take_aggregate_pane_visits() -> usize {
    AGGREGATE_PANE_VISITS.with(|visits| visits.replace(0))
}

/// Detail info for a single pane, used by the agent detail panel.
pub struct PaneDetail {
    pub pane_id: PaneId,
    pub tab_idx: usize,
    pub pane_label: Option<String>,
    pub pane_label_is_agent_identity: bool,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: String,
    pub agent_kind_label: Option<String>,
    pub agent: Option<Agent>,
    pub agent_context: Option<Agent>,
    pub has_agent: bool,
    pub state: AgentState,
    pub attention_tier: AttentionTier,
    /// The last closing-block report still names at least one gate. Outlives
    /// the blocked lifecycle state: output retirement may flip the pane back
    /// to working while the human decision is still open.
    pub open_blockers: bool,
    pub gate_count: usize,
    pub closing_idle: Option<bool>,
    pub closing_contract: Option<String>,
    pub closing_contract_met: Option<bool>,
    /// Whether the terminal's ClosingReport contains any projected closing
    /// fact. Sidebar completion must not infer this from metadata tokens.
    pub has_closing_report: bool,
    /// The pane's agent is refusing to work because its plan usage/rate limit
    /// is exhausted. Live screen state, never latched.
    pub usage_limited: bool,
    pub holds_shell: bool,
    pub active_subagents: Option<u32>,
    pub waiting_on_agents: bool,
    pub working_while_blocked: bool,
    pub foreground_process_name: Option<String>,
    pub seen: bool,
    pub done_since: Option<Instant>,
    pub stale: bool,
    pub reported_at: Option<Instant>,
    pub last_agent_state_change_seq: Option<u64>,
    pub activity_at: Option<Instant>,
    pub state_labels: HashMap<String, String>,
    pub tokens: HashMap<String, String>,
}

impl Tab {
    pub(crate) fn aggregate_state_and_attention(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> (AgentState, bool, AttentionTier) {
        aggregate_projections(self.panes.values().filter_map(|pane| {
            #[cfg(test)]
            record_aggregate_pane_visit();
            terminals.get(&pane.attached_terminal_id).map(|terminal| {
                let projection = pane.agent_projection(terminal);
                let attention_tier = if projection.counts_as_blocked() {
                    AttentionTier::Blocked
                } else if projection.attention_tier == AttentionTier::Attention {
                    AttentionTier::Attention
                } else {
                    AttentionTier::None
                };
                (projection.state, projection.seen, attention_tier)
            })
        }))
    }

    fn pane_details(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        tab_idx: usize,
    ) -> Vec<PaneDetail> {
        self.layout
            .pane_ids()
            .iter()
            .enumerate()
            .filter_map(|(pane_idx, id)| {
                let pane = self.panes.get(id)?;
                let terminal = terminals.get(&pane.attached_terminal_id)?;
                let agent_kind_label = terminal.effective_agent_label().map(str::to_string);
                let display_agent = terminal.effective_display_agent();
                let fallback_agent_label = terminal
                    .agent_name
                    .as_deref()
                    .or(agent_kind_label.as_deref())
                    .map(str::to_string);
                let agent_label = display_agent
                    .clone()
                    .or(fallback_agent_label)
                    .unwrap_or_else(|| ">_".to_string());
                let presentation = terminal.effective_presentation();
                let projection = pane.agent_projection(terminal);
                let state = projection.state;
                let seen = projection.seen;
                let (pane_label, pane_label_is_agent_identity) = if let Some(label) =
                    terminal.manual_label.clone()
                {
                    (Some(label), false)
                } else if let Some(label) = terminal.effective_work_context().work_title.clone() {
                    (Some(label), false)
                } else if let Some(label) = display_agent {
                    (Some(label), true)
                } else if let Some(label) = terminal.agent_name.clone() {
                    (Some(label), true)
                } else if let Some(label) = agent_kind_label.clone() {
                    (Some(label), true)
                } else if let Some(label) = terminal.terminal_title_stripped() {
                    (Some(label), false)
                } else {
                    (Some(format!("Pane {}", pane_idx + 1)), false)
                };
                Some(PaneDetail {
                    pane_id: *id,
                    tab_idx,
                    // A pane is a child of the tab in every sidebar projection.
                    // Never derive its label from the effective tab title: that
                    // would render the persisted title twice. Keep this order
                    // deterministic for agentless panes as well.
                    pane_label,
                    pane_label_is_agent_identity,
                    terminal_title: terminal.terminal_title.clone(),
                    terminal_title_stripped: terminal.terminal_title_stripped(),
                    agent_label,
                    agent_kind_label,
                    agent: terminal.effective_known_agent(),
                    agent_context: terminal.agent_lifecycle_context(),
                    has_agent: terminal.agent_lifecycle_context().is_some(),
                    state,
                    attention_tier: projection.attention_tier,
                    open_blockers: projection.open_blockers,
                    gate_count: projection.gate_count,
                    closing_idle: terminal.closing_idle(),
                    closing_contract: terminal.closing_contract().map(str::to_string),
                    closing_contract_met: terminal.closing_contract_met(),
                    has_closing_report: terminal.has_closing_report(),
                    usage_limited: projection.usage_limited,
                    holds_shell: terminal.holds_shell,
                    active_subagents: terminal.verified_active_subagents(),
                    waiting_on_agents: projection.waiting_on_agents,
                    working_while_blocked: projection.working_while_blocked,
                    foreground_process_name: terminal.foreground_process_name.clone(),
                    seen,
                    done_since: pane.done_since,
                    stale: projection.stale,
                    reported_at: terminal.status_reported_at(),
                    last_agent_state_change_seq: terminal.last_agent_state_change_seq,
                    activity_at: terminal.agent_activity_at(),
                    state_labels: presentation.state_labels,
                    tokens: terminal.metadata_tokens.values(),
                })
            })
            .collect()
    }
}

fn pane_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

fn aggregate_projections(
    projections: impl IntoIterator<Item = (AgentState, bool, AttentionTier)>,
) -> (AgentState, bool, AttentionTier) {
    projections.into_iter().fold(
        (AgentState::Unknown, true, AttentionTier::None),
        |aggregate, candidate| {
            let state = if pane_attention_priority(candidate.0, candidate.1)
                >= pane_attention_priority(aggregate.0, aggregate.1)
            {
                (candidate.0, candidate.1)
            } else {
                (aggregate.0, aggregate.1)
            };
            (state.0, state.1, aggregate.2.max(candidate.2))
        },
    )
}

impl Workspace {
    pub fn aggregate_state(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> (AgentState, bool) {
        let (state, seen, _) = self.aggregate_state_and_attention(terminals);
        (state, seen)
    }

    /// Project lifecycle state and human-attention severity in one pane pass.
    pub(crate) fn aggregate_state_and_attention(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> (AgentState, bool, AttentionTier) {
        aggregate_projections(
            self.tabs
                .iter()
                .map(|tab| tab.aggregate_state_and_attention(terminals)),
        )
    }

    pub fn pane_details(&self, terminals: &HashMap<TerminalId, TerminalState>) -> Vec<PaneDetail> {
        self.tabs
            .iter()
            .enumerate()
            .flat_map(|(tab_idx, tab)| tab.pane_details(terminals, tab_idx).into_iter())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Direction;

    use super::*;
    use crate::detect::Agent;

    fn terminal_for_pane(ws: &Workspace, pane_id: PaneId) -> TerminalState {
        TerminalState::new(ws.terminal_id(pane_id).unwrap().clone(), "/tmp".into())
    }

    #[test]
    fn aggregate_state_all_unknown() {
        let ws = Workspace::test_new("test");
        let mut terminals = HashMap::new();
        let root = ws.tabs[0].root_pane;
        let terminal = terminal_for_pane(&ws, root);
        terminals.insert(terminal.id.clone(), terminal);
        let (state, seen) = ws.aggregate_state(&terminals);
        assert_eq!(state, AgentState::Unknown);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_and_attention_preserves_missing_tab_unknown_seen_tie() {
        let mut ws = Workspace::test_new("test");
        let root = ws.tabs[0].root_pane;
        ws.tabs[0].panes.get_mut(&root).unwrap().seen = false;
        ws.test_add_tab(None);

        let mut terminals = HashMap::new();
        let terminal = terminal_for_pane(&ws, root);
        terminals.insert(terminal.id.clone(), terminal);

        let expected = ws.aggregate_state(&terminals);
        let (state, seen, attention) = ws.aggregate_state_and_attention(&terminals);

        assert_eq!(expected, (AgentState::Unknown, true));
        assert_eq!((state, seen), expected);
        assert_eq!(attention, AttentionTier::None);
    }

    #[test]
    fn aggregate_state_priority() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.set_raw_agent_state_for_test(AgentState::Idle);
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.set_raw_agent_state_for_test(AgentState::Working);
        terminals.insert(second_terminal.id.clone(), second_terminal);

        let (state, seen) = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Working);
        assert!(seen);
    }

    #[test]
    fn aggregate_methods_each_reduce_every_pane_once() {
        let mut ws = Workspace::test_new("test");
        ws.test_split(Direction::Horizontal);
        let terminals = ws.tabs[0]
            .panes
            .values()
            .map(|pane| {
                let terminal = TerminalState::new(pane.attached_terminal_id.clone(), "/tmp".into());
                (terminal.id.clone(), terminal)
            })
            .collect::<HashMap<_, _>>();
        let pane_count = ws.tabs[0].panes.len();

        take_aggregate_pane_visits();
        ws.aggregate_state(&terminals);
        assert_eq!(take_aggregate_pane_visits(), pane_count);

        ws.aggregate_state_and_attention(&terminals);
        assert_eq!(take_aggregate_pane_visits(), pane_count);
    }

    #[test]
    fn aggregate_state_done_unseen_beats_working() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.set_raw_agent_state_for_test(AgentState::Idle);
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.set_raw_agent_state_for_test(AgentState::Working);
        terminals.insert(second_terminal.id.clone(), second_terminal);
        let root = ws.tabs[0].panes.get_mut(&root_id).unwrap();
        root.seen = false;

        let (state, seen) = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Idle);
        assert!(!seen);
    }

    #[test]
    fn pane_details_prefers_agent_name_over_detected_agent_label() {
        let ws = Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, root_pane);
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("planner".into());
        terminals.insert(terminal.id.clone(), terminal);

        let labels: Vec<_> = ws
            .pane_details(&terminals)
            .into_iter()
            .map(|detail| (detail.agent_label, detail.agent))
            .collect();

        assert_eq!(labels, vec![("planner".into(), Some(Agent::Pi))]);
    }

    #[test]
    fn pane_details_keeps_pane_labels_independent_from_tab_titles() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].custom_name = Some("main".into());
        let root_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("review"));
        let review_pane = ws.tabs[second_tab].root_pane;
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_pane);
        root_terminal.set_hook_authority(
            "test".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut review_terminal = terminal_for_pane(&ws, review_pane);
        review_terminal.set_hook_authority(
            "test".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
        );
        terminals.insert(review_terminal.id.clone(), review_terminal);

        let labels: Vec<_> = ws
            .pane_details(&terminals)
            .into_iter()
            .map(|detail| (detail.agent_label, detail.agent))
            .collect();

        assert_eq!(
            labels,
            vec![
                ("pi".into(), Some(Agent::Pi)),
                ("claude".into(), Some(Agent::Claude)),
            ]
        );
    }

    #[test]
    fn pane_details_use_tab_vector_index_not_stable_public_tab_number() {
        let mut ws = Workspace::test_new("test");
        let removed_tab = ws.test_add_tab(Some("removed"));
        let survivor_tab = ws.test_add_tab(Some("survivor"));
        let survivor_pane = ws.tabs[survivor_tab].root_pane;
        assert!(ws.close_tab(removed_tab));

        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, survivor_pane);
        terminal.detected_agent = Some(Agent::Codex);
        terminals.insert(terminal.id.clone(), terminal);

        let details = ws.pane_details(&terminals);
        let survivor = details
            .iter()
            .find(|detail| detail.pane_id == survivor_pane)
            .expect("surviving tab agent should be listed");

        assert_eq!(ws.tabs[1].number, 3);
        assert_eq!(survivor.tab_idx, 1);
    }

    #[test]
    fn pane_details_include_agentless_panes_in_layout_order_with_fallback_labels() {
        let mut ws = Workspace::test_new("test");
        let second = ws.test_split(Direction::Horizontal);
        let mut terminals = HashMap::new();
        for pane_id in ws.tabs[0].layout.pane_ids() {
            let terminal = terminal_for_pane(&ws, pane_id);
            terminals.insert(terminal.id.clone(), terminal);
        }

        let details = ws.pane_details(&terminals);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].pane_id, ws.tabs[0].layout.pane_ids()[0]);
        assert_eq!(details[1].pane_id, second);
        assert_eq!(details[0].pane_label.as_deref(), Some("Pane 1"));
        assert_eq!(details[1].pane_label.as_deref(), Some("Pane 2"));
    }

    #[test]
    fn settled_pane_projects_as_neutral_seen_and_without_attention() {
        let mut ws = Workspace::test_new("test");
        let pane = ws.tabs[0].root_pane;
        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, pane);
        terminal.set_raw_agent_state_for_test(AgentState::Blocked);
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                blocking: true,
                n: 1,
                label: "Answer".into(),
                text: "Choose one".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        terminals.insert(terminal.id.clone(), terminal);

        assert_eq!(
            ws.pane_details(&terminals)[0].attention_tier,
            AttentionTier::Blocked
        );

        ws.tabs[0].panes.get_mut(&pane).unwrap().settled_at = Some(1);
        let detail = &ws.pane_details(&terminals)[0];
        assert_eq!(detail.state, AgentState::Unknown);
        assert!(detail.seen);
        assert_eq!(detail.attention_tier, AttentionTier::None);
        assert_eq!(
            ws.aggregate_state_and_attention(&terminals),
            (AgentState::Unknown, true, AttentionTier::None)
        );
    }

    #[test]
    fn ac2_pane_details_prefer_manual_label_then_durable_work_title() {
        let ws = Workspace::test_new("test");
        let pane = ws.tabs[0].root_pane;
        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, pane);
        terminal.agent_name = Some("Claude".into());
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                work_title: Some("repair login".into()),
                ..Default::default()
            })
            .unwrap();
        terminals.insert(terminal.id.clone(), terminal);

        assert_eq!(
            ws.pane_details(&terminals)[0].pane_label.as_deref(),
            Some("repair login")
        );

        terminals
            .values_mut()
            .next()
            .unwrap()
            .set_manual_label("manual pane".into());
        assert_eq!(
            ws.pane_details(&terminals)[0].pane_label.as_deref(),
            Some("manual pane")
        );
    }
}
