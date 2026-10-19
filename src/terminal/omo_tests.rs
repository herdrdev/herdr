use super::{TerminalId, TerminalState};
use crate::detect::{Agent, AgentState};
use std::time::Instant;

#[test]
fn omo_reports_apply_after_restore_clear_without_native_session_anchor() {
    for state in [AgentState::Idle, AgentState::Working, AgentState::Blocked] {
        let now = Instant::now();
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_detected_agent_process_at(Agent::Omo, now);
        terminal.release_agent_with_mutation("custom:omo", "omo", Some(1));

        let mutation = terminal.set_hook_authority_at(
            "custom:omo".into(),
            "omo".into(),
            state,
            None,
            None,
            Some(2),
            now,
        );

        assert!(mutation.is_some());
        assert_eq!(terminal.effective_agent_label(), Some("omo"));
        assert_eq!(terminal.state, state);
    }
}
