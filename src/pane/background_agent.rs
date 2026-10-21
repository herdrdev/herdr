//! Keeps an agent's identity while job control moves it out of the foreground.
//!
//! Suspending an agent with ctrl-z, or running a command in front of it, hands
//! the terminal to another job while the agent process lives on. Treating that
//! as an exit would drop the agent's session, name, and hook authority, and
//! nothing would restore them when it returns.

use crate::detect::Agent;

/// Where the process that identified as the current agent is now, when the
/// foreground probe no longer finds that agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentJobStatus {
    /// Untracked, still in the foreground, or gone without having been held.
    Unknown,
    /// Alive outside the foreground, such as a job stopped with ctrl-z.
    Background,
    /// Was held in the background and has since exited.
    ExitedInBackground,
    /// Was held, and a different process of the same agent kind is in front.
    ReplacedInFront,
}

/// The identified agent process. The start token tells it apart from a later
/// process that reuses its pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentProcess {
    pid: u32,
    start_token: u64,
}

#[derive(Debug, Default)]
pub(super) struct AgentJobTracker {
    process: Option<AgentProcess>,
    in_background: bool,
}

impl AgentJobTracker {
    /// A held agent must be rechecked even when lifecycle-hook authority would
    /// otherwise skip probing, so a job killed in the background is noticed.
    pub(super) fn in_background(&self) -> bool {
        self.in_background
    }

    /// `agent_missing` means the probe lost a current agent whose exit has not
    /// been reported yet. Liveness is checked only then, never per render.
    ///
    /// `live_group(shell_pid, pid, start_token)` returns the process group of
    /// that exact process while it is alive in the pane's terminal session.
    pub(super) fn status(
        &self,
        agent_missing: bool,
        shell_pid: u32,
        foreground_group: Option<u32>,
        live_group: impl FnOnce(u32, u32, u64) -> Option<u32>,
    ) -> AgentJobStatus {
        if !agent_missing {
            return AgentJobStatus::Unknown;
        }
        let Some(process) = self.process else {
            return AgentJobStatus::Unknown;
        };
        match live_group(shell_pid, process.pid, process.start_token) {
            // An agent in the shell's own group was never a separate job, and
            // one still in the foreground group is a plain miss.
            Some(group) if group != shell_pid && Some(group) != foreground_group => {
                AgentJobStatus::Background
            }
            Some(_) => AgentJobStatus::Unknown,
            None if self.in_background => AgentJobStatus::ExitedInBackground,
            None => AgentJobStatus::Unknown,
        }
    }

    /// While an agent is held, a same-kind agent in front is that agent only
    /// when its pid and start token match the held process.
    pub(super) fn replaced_in_front(
        &self,
        identified_pid: Option<u32>,
        start_token: impl FnOnce(u32) -> Option<u64>,
    ) -> bool {
        let Some(held) = self.process.filter(|_| self.in_background) else {
            return false;
        };
        identified_pid
            .is_none_or(|pid| pid != held.pid || start_token(pid) != Some(held.start_token))
    }

    pub(super) fn observe(
        &mut self,
        status: AgentJobStatus,
        current_agent: Option<Agent>,
        identified_agent: Option<Agent>,
        identified_pid: Option<u32>,
        start_token: impl FnOnce(u32) -> Option<u64>,
    ) {
        if current_agent.is_none() {
            *self = Self::default();
        } else if status == AgentJobStatus::ReplacedInFront {
            // Keep holding until the held agent's exit has been reported.
        } else if identified_agent == current_agent {
            self.in_background = false;
            // Reread every time: a new process may have reused the pid.
            self.process = identified_pid.and_then(|pid| {
                start_token(pid).map(|start_token| AgentProcess { pid, start_token })
            });
        } else if status == AgentJobStatus::Background {
            self.in_background = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHELL: u32 = 10;
    const WRAPPER_JOB: u32 = 20;
    const AGENT_PID: u32 = 21;
    const AGENT_START: u64 = 7_000;

    fn acquired(agent: Agent) -> AgentJobTracker {
        let mut tracker = AgentJobTracker::default();
        tracker.observe(
            AgentJobStatus::Unknown,
            Some(agent),
            Some(agent),
            Some(AGENT_PID),
            |pid| (pid == AGENT_PID).then_some(AGENT_START),
        );
        tracker
    }

    fn live_agent(shell: u32, pid: u32, start_token: u64) -> Option<u32> {
        assert_eq!(shell, SHELL);
        (pid == AGENT_PID && start_token == AGENT_START).then_some(WRAPPER_JOB)
    }

    #[test]
    fn stopped_agent_behind_the_shell_is_in_background() {
        let tracker = acquired(Agent::Claude);

        assert_eq!(
            tracker.status(true, SHELL, Some(SHELL), live_agent),
            AgentJobStatus::Background
        );
    }

    #[test]
    fn agent_that_exited_in_front_is_unknown() {
        let tracker = acquired(Agent::Claude);

        assert_eq!(
            tracker.status(true, SHELL, Some(SHELL), |_, _, _| None),
            AgentJobStatus::Unknown
        );
    }

    #[test]
    fn liveness_is_only_checked_when_the_agent_went_missing() {
        let tracker = acquired(Agent::Claude);
        let no_check = |_: u32, _: u32, _: u64| -> Option<u32> { panic!("liveness checked") };

        assert_eq!(
            tracker.status(false, SHELL, Some(SHELL), no_check),
            AgentJobStatus::Unknown
        );
        assert_eq!(
            AgentJobTracker::default().status(true, SHELL, Some(SHELL), no_check),
            AgentJobStatus::Unknown
        );
    }

    #[test]
    fn agent_still_in_the_foreground_group_is_not_held() {
        let tracker = acquired(Agent::Claude);

        assert_eq!(
            tracker.status(true, SHELL, Some(WRAPPER_JOB), live_agent),
            AgentJobStatus::Unknown
        );
    }

    #[test]
    fn agent_in_the_shell_group_is_never_held() {
        let tracker = acquired(Agent::Pi);

        assert_eq!(
            tracker.status(true, SHELL, Some(30), |_, _, _| Some(SHELL)),
            AgentJobStatus::Unknown
        );
    }

    #[test]
    fn agent_without_a_start_token_is_never_held() {
        let mut tracker = AgentJobTracker::default();
        tracker.observe(
            AgentJobStatus::Unknown,
            Some(Agent::Pi),
            Some(Agent::Pi),
            Some(AGENT_PID),
            |_| None,
        );

        assert_eq!(
            tracker.status(true, SHELL, Some(SHELL), |_, _, _| Some(WRAPPER_JOB)),
            AgentJobStatus::Unknown
        );
    }

    #[test]
    fn reidentified_agent_refreshes_its_start_token() {
        const REUSED_START: u64 = 9_000;
        let mut tracker = acquired(Agent::Pi);
        // A new Pi reused the pid between two probes.
        tracker.observe(
            AgentJobStatus::Unknown,
            Some(Agent::Pi),
            Some(Agent::Pi),
            Some(AGENT_PID),
            |_| Some(REUSED_START),
        );

        assert_eq!(
            tracker.status(true, SHELL, Some(SHELL), |_, pid, token| {
                (pid == AGENT_PID && token == REUSED_START).then_some(WRAPPER_JOB)
            }),
            AgentJobStatus::Background
        );
    }

    #[test]
    fn held_agent_is_replaced_only_by_a_different_process() {
        let mut tracker = acquired(Agent::Claude);
        assert!(
            !tracker.replaced_in_front(Some(31), |_| Some(1)),
            "not held"
        );
        let status = tracker.status(true, SHELL, Some(SHELL), live_agent);
        tracker.observe(status, Some(Agent::Claude), None, None, |_| None);

        assert!(!tracker.replaced_in_front(Some(AGENT_PID), |_| Some(AGENT_START)));
        assert!(tracker.replaced_in_front(Some(31), |_| Some(AGENT_START)));
        assert!(tracker.replaced_in_front(Some(AGENT_PID), |_| Some(9_000)));
        assert!(tracker.replaced_in_front(Some(AGENT_PID), |_| None));
        assert!(tracker.replaced_in_front(None, |_| Some(AGENT_START)));
    }

    #[test]
    fn returning_agent_ends_the_hold() {
        let mut tracker = acquired(Agent::Claude);
        let status = tracker.status(true, SHELL, Some(SHELL), live_agent);
        tracker.observe(status, Some(Agent::Claude), None, None, |_| None);
        assert!(tracker.in_background());

        tracker.observe(
            AgentJobStatus::Unknown,
            Some(Agent::Claude),
            Some(Agent::Claude),
            Some(AGENT_PID),
            |_| None,
        );

        assert!(!tracker.in_background());
    }

    #[test]
    fn cleared_agent_resets_the_tracker() {
        let mut tracker = acquired(Agent::Pi);
        let status = tracker.status(true, SHELL, Some(SHELL), live_agent);
        tracker.observe(status, Some(Agent::Pi), None, None, |_| None);

        tracker.observe(AgentJobStatus::Unknown, None, None, None, |_| None);

        assert!(!tracker.in_background());
        assert_eq!(
            tracker.status(true, SHELL, Some(SHELL), live_agent),
            AgentJobStatus::Unknown
        );
    }
}
