use std::time::Instant;

use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind, PersistedAgentSession};

use super::TerminalState;
#[cfg(unix)]
use super::{FullLifecycleHookSuppressionReason, RecentAgentProcessExit};

/// A native integration observed a termination signal. This is recovery data,
/// never evidence that an agent still owns the terminal.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct InterruptedAgentSession {
    session: PersistedAgentSession,
    sequence: u64,
    #[serde(skip, default = "Instant::now")]
    reported_at: Instant,
}

impl TerminalState {
    pub(crate) fn report_agent_interruption(
        &mut self,
        source: &str,
        agent: &str,
        session_ref: AgentSessionRef,
        sequence: u64,
    ) -> bool {
        // These integrations await shutdown hooks after catching TERM/HUP.
        // Never infer interruption from a generic process exit or exit code.
        if !matches!((source, agent), ("herdr:pi", "pi") | ("herdr:omp", "omp"))
            || session_ref.kind != AgentSessionRefKind::Path
            || self.recent_agent_process_exit.is_some()
            || self.current_session_identity_for_persistence()
                != Some((
                    source.into(),
                    agent.into(),
                    session_ref.kind,
                    session_ref.value.clone(),
                ))
            || !self.accept_hook_report(source, Some(sequence))
        {
            return false;
        }
        self.interrupted_agent_session = Some(InterruptedAgentSession {
            session: PersistedAgentSession {
                source: source.into(),
                agent: agent.into(),
                session_ref,
            },
            sequence,
            reported_at: Instant::now(),
        });
        self.interrupted_session_revision = self.interrupted_session_revision.wrapping_add(1);
        true
    }

    pub(crate) fn interrupted_agent_session(&self) -> Option<&PersistedAgentSession> {
        self.interrupted_agent_session
            .as_ref()
            .map(|interrupted| &interrupted.session)
    }

    pub(crate) fn interrupted_session_revision(&self) -> u64 {
        self.interrupted_session_revision
    }

    pub(super) fn clear_interrupted_session(&mut self) {
        if self.interrupted_agent_session.take().is_some() {
            self.interrupted_session_revision = self.interrupted_session_revision.wrapping_add(1);
        }
    }

    pub(super) fn clear_interrupted_session_before_process(&mut self, observed_at: Instant) {
        if self
            .interrupted_agent_session
            .as_ref()
            .is_some_and(|interrupted| interrupted.reported_at < observed_at)
        {
            self.clear_interrupted_session();
        }
    }

    pub(super) fn clear_interrupted_session_for_report(
        &mut self,
        source: &str,
        agent: &str,
        session_ref: Option<&AgentSessionRef>,
        new_session: bool,
    ) {
        if self
            .interrupted_agent_session
            .as_ref()
            .is_some_and(|interrupted| {
                new_session
                    || interrupted.session.source != source
                    || interrupted.session.agent != agent
                    || session_ref
                        .is_some_and(|session_ref| interrupted.session.session_ref != *session_ref)
            })
        {
            self.clear_interrupted_session();
        }
    }

    #[cfg(unix)]
    pub(crate) fn handoff_interrupted_agent_session(&self) -> Option<InterruptedAgentSession> {
        self.interrupted_agent_session.clone()
    }

    #[cfg(unix)]
    pub(crate) fn restore_interrupted_agent_session(
        &mut self,
        interrupted: InterruptedAgentSession,
    ) {
        if self
            .current_session_identity_for_persistence()
            .is_some_and(|current| {
                current
                    != (
                        interrupted.session.source.clone(),
                        interrupted.session.agent.clone(),
                        interrupted.session.session_ref.kind,
                        interrupted.session.session_ref.value.clone(),
                    )
            })
        {
            return;
        }
        if !self.live_full_lifecycle_hook_authority() {
            // A live handoff imports the existing shell. Its saved recovery ref
            // must not acquire authority or an agent label in the new server.
            self.persisted_agent_session = None;
            self.detected_agent = None;
            self.state = crate::detect::AgentState::Unknown;
            self.fallback_state = crate::detect::AgentState::Unknown;
            self.clear_agent_name();
            // Deserialization happens before imported detector tasks start.
            // A queued fresh process observation must remain newer than this
            // suppression boundary when restoration finishes.
            let now = interrupted.reported_at;
            self.recent_agent_process_exit =
                crate::detect::parse_agent_label(&interrupted.session.agent).map(|agent| {
                    RecentAgentProcessExit {
                        agent,
                        observed_at: now,
                    }
                });
            self.suppress_full_lifecycle_hook_report_with_session_ref(
                interrupted.session.source.clone(),
                interrupted.session.agent.clone(),
                Some(interrupted.session.session_ref.clone()),
                FullLifecycleHookSuppressionReason::ProcessExit,
                now,
            );
            self.hook_report_sequences
                .insert(interrupted.session.source.clone(), interrupted.sequence);
        }
        self.interrupted_agent_session = Some(interrupted);
        self.interrupted_session_revision = self.interrupted_session_revision.wrapping_add(1);
    }
}
