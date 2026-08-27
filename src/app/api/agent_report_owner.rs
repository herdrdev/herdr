//! Binds a pane's agent-state reporting to one process.
//!
//! Integrations that run inside a long-lived agent process report through the
//! pane's inherited environment, which every descendant of the pane also has.
//! A session nested inside the pane's agent — an `omp --mode=rpc` started by a
//! bash tool call, for example — would otherwise anchor its own session and own
//! the pane's state for the rest of the pane's life (#3246).
//!
//! The pane therefore latches the first reporting pid and refuses reports from
//! other pids while that owner still holds the pane. Only a process the owner
//! started loses: any reporter that reaches the pane root without passing
//! through the owner — the pane's own agent, the pane shell, or a second agent
//! started beside the owner — takes the pane, so no gate here can silence a
//! pane's real agent permanently.

use crate::app::App;
use crate::layout::PaneId;

/// Upper bound for the owner's parent-chain climb. Real pane trees are a
/// handful of processes deep; the cap only stops a corrupted or recycled
/// parent chain from looping.
const MAX_ANCESTRY_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReportOwnership {
    /// The reporter already owns the pane, or ownership does not apply.
    Unchanged,
    /// The reporter takes the pane, replacing any previous owner.
    Latch(u32),
    /// Another process still holds the pane.
    Refused { owner_pid: u32 },
}

impl App {
    /// Decides whether `peer_pid` may report agent state for this pane, and
    /// latches it as the pane's report owner when it may.
    ///
    /// Returns the owning pid when the report must be refused.
    pub(super) fn claim_agent_report_owner(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        source: &str,
        agent_label: &str,
        peer_pid: Option<u32>,
    ) -> Result<(), u32> {
        if !crate::detect::in_process_hook_reporter(source, agent_label) {
            return Ok(());
        }
        // Without the reporter pid or the pane's process, two reporters cannot
        // be told apart, so the pane keeps pre-#3246 behavior.
        let Some(reporter_pid) = peer_pid else {
            return Ok(());
        };
        let Some(pane_tree_root) = self
            .lookup_runtime(ws_idx, pane_id)
            .and_then(|(runtime, _)| runtime.child_pid())
        else {
            return Ok(());
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return Ok(());
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return Ok(());
        };

        let owner_pid = terminal.agent_report_owner();
        match resolve_report_ownership(
            owner_pid,
            reporter_pid,
            pane_tree_root,
            crate::platform::process_parent_pid,
        ) {
            ReportOwnership::Unchanged => Ok(()),
            ReportOwnership::Latch(pid) => {
                // The latch also drops the replaced owner's report-sequence
                // baseline, which the new owner would otherwise never beat.
                terminal.set_agent_report_owner(pid, source);
                if owner_pid.is_some_and(|previous| previous != pid) {
                    tracing::debug!(
                        event = "agent.report.owner_replaced",
                        subsystem = "agent",
                        source,
                        previous_owner_pid = owner_pid,
                        owner_pid = pid,
                        "pane agent report owner replaced, sequence baseline reset"
                    );
                }
                Ok(())
            }
            ReportOwnership::Refused { owner_pid } => {
                tracing::warn!(
                    event = "agent.report.refused",
                    subsystem = "agent",
                    source,
                    agent = agent_label,
                    owner_pid,
                    reporter_pid,
                    "refused a pane agent report from a process that does not own the pane"
                );
                Err(owner_pid)
            }
        }
    }
}

/// Pure ownership rule. `parent_of` returns the parent pid of a live process,
/// and `None` once the process is gone, including one that has exited but has
/// not been reaped yet.
fn resolve_report_ownership(
    owner_pid: Option<u32>,
    reporter_pid: u32,
    pane_tree_root: u32,
    parent_of: impl Fn(u32) -> Option<u32>,
) -> ReportOwnership {
    match owner_pid {
        Some(owner_pid) if owner_pid == reporter_pid => ReportOwnership::Unchanged,
        Some(owner_pid) if owner_keeps_pane(owner_pid, reporter_pid, pane_tree_root, parent_of) => {
            ReportOwnership::Refused { owner_pid }
        }
        _ => ReportOwnership::Latch(reporter_pid),
    }
}

/// Decides whether the current owner keeps the pane against `reporter_pid`.
///
/// The owner keeps the pane while it is alive, still inside the pane's process
/// tree, and the reporter is a process it started. A dead owner, an owner that
/// has left the pane's tree, and an owner descended from the reporter all give
/// the pane up, and so does an owner whose reporter sits beside it in the pane
/// tree rather than under it — a second agent started by the pane shell is the
/// pane's real agent too, and last report wins between the two.
///
/// A reporter outside the pane's process tree never takes the pane from a live
/// owner, which is the forged-report case in #3246.
fn owner_keeps_pane(
    owner_pid: u32,
    reporter_pid: u32,
    pane_tree_root: u32,
    parent_of: impl Fn(u32) -> Option<u32>,
) -> bool {
    chain_reaches_pane_root(owner_pid, pane_tree_root, reporter_pid, &parent_of)
        && !chain_reaches_pane_root(reporter_pid, pane_tree_root, owner_pid, &parent_of)
}

/// Climbs `pid`'s parent chain toward the pane root.
///
/// `true` when the chain reaches `pane_tree_root`, so `pid` is a live process
/// inside the pane's tree and `halt_pid` is not one of its ancestors. `false`
/// when the chain meets `halt_pid` first, leaves the pane's tree, or reaches a
/// process that is gone.
fn chain_reaches_pane_root(
    pid: u32,
    pane_tree_root: u32,
    halt_pid: u32,
    parent_of: &impl Fn(u32) -> Option<u32>,
) -> bool {
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if current == halt_pid {
            return false;
        }
        if current == pane_tree_root {
            return true;
        }
        match parent_of(current) {
            // pid 1 is never a pane process, so the chain has left the pane.
            Some(parent) if parent > 1 => current = parent,
            _ => return false,
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// `child -> parent` chain for the pure rule tests.
    fn chain(pairs: &[(u32, u32)]) -> impl Fn(u32) -> Option<u32> + '_ {
        let parents: HashMap<u32, u32> = pairs.iter().copied().collect();
        move |pid| parents.get(&pid).copied()
    }

    #[test]
    fn first_reporter_takes_the_pane() {
        assert_eq!(
            resolve_report_ownership(None, 300, 100, chain(&[(300, 100)])),
            ReportOwnership::Latch(300)
        );
    }

    #[test]
    fn the_owner_reporting_again_changes_nothing() {
        assert_eq!(
            resolve_report_ownership(Some(300), 300, 100, chain(&[(300, 100)])),
            ReportOwnership::Unchanged
        );
    }

    #[test]
    fn a_session_nested_in_the_owner_is_refused() {
        // pane shell 100 -> agent 300 (owner) -> bash tool 310 -> nested agent 320
        let parents = chain(&[(300, 100), (310, 300), (320, 310)]);
        assert_eq!(
            resolve_report_ownership(Some(300), 320, 100, parents),
            ReportOwnership::Refused { owner_pid: 300 }
        );
    }

    #[test]
    fn an_unrelated_process_outside_the_pane_is_refused() {
        let parents = chain(&[(300, 100), (900, 1)]);
        assert_eq!(
            resolve_report_ownership(Some(300), 900, 100, parents),
            ReportOwnership::Refused { owner_pid: 300 }
        );
    }

    #[test]
    fn the_process_closer_to_the_pane_root_takes_the_pane_back() {
        // The nested session owns the pane; the pane's own agent reports next.
        let parents = chain(&[(300, 100), (310, 300), (320, 310)]);
        assert_eq!(
            resolve_report_ownership(Some(320), 300, 100, parents),
            ReportOwnership::Latch(300)
        );
    }

    #[test]
    fn a_dead_owner_gives_the_pane_up() {
        // 300 has no parent entry, so it is gone.
        assert_eq!(
            resolve_report_ownership(Some(300), 400, 100, chain(&[(400, 100)])),
            ReportOwnership::Latch(400)
        );
    }

    #[test]
    fn an_owner_that_left_the_pane_tree_gives_the_pane_up() {
        // The owner is alive but its chain reaches init, not the pane shell.
        let parents = chain(&[(300, 1), (400, 100)]);
        assert_eq!(
            resolve_report_ownership(Some(300), 400, 100, parents),
            ReportOwnership::Latch(400)
        );
    }

    #[test]
    fn a_parent_chain_loop_cannot_keep_the_pane() {
        let parents = chain(&[(300, 301), (301, 300)]);
        assert_eq!(
            resolve_report_ownership(Some(300), 400, 100, parents),
            ReportOwnership::Latch(400)
        );
    }

    #[test]
    fn the_pane_process_itself_can_own_the_pane() {
        assert_eq!(
            resolve_report_ownership(Some(100), 300, 100, chain(&[(300, 100)])),
            ReportOwnership::Refused { owner_pid: 100 }
        );
    }

    #[test]
    fn a_second_agent_started_beside_the_owner_takes_the_pane() {
        // The pane shell 100 started agent 300, then a second agent 400 next to
        // it: both are the pane's own agents, so the later report wins.
        let parents = chain(&[(300, 100), (400, 100)]);
        assert_eq!(
            resolve_report_ownership(Some(300), 400, 100, parents),
            ReportOwnership::Latch(400)
        );
    }

    #[test]
    fn a_session_under_a_sibling_of_the_owner_takes_the_pane() {
        // pane shell 100 -> agent 300 (owner), and 100 -> wrapper 400 -> agent
        // 410. 410 is not a session 300 started, so 300 does not hold it off.
        let parents = chain(&[(300, 100), (400, 100), (410, 400)]);
        assert_eq!(
            resolve_report_ownership(Some(300), 410, 100, parents),
            ReportOwnership::Latch(410)
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_process_ancestry_decides_between_two_live_reporters() {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a child process");
        let child_pid = child.id();
        let own_pid = std::process::id();

        // This process stands in for the pane's agent, the child for a nested
        // session started by it.
        assert_eq!(
            resolve_report_ownership(
                Some(own_pid),
                child_pid,
                own_pid,
                crate::platform::process_parent_pid
            ),
            ReportOwnership::Refused { owner_pid: own_pid }
        );
        assert_eq!(
            resolve_report_ownership(
                Some(child_pid),
                own_pid,
                own_pid,
                crate::platform::process_parent_pid
            ),
            ReportOwnership::Latch(own_pid)
        );

        child.kill().expect("kill the child process");
        child.wait().expect("reap the child process");
    }

    #[cfg(unix)]
    #[test]
    fn real_process_ancestry_hands_the_pane_to_a_live_sibling() {
        let spawn_child = || {
            std::process::Command::new("sh")
                .arg("-c")
                .arg("sleep 30")
                .spawn()
                .expect("spawn a child process")
        };
        let mut first = spawn_child();
        let mut second = spawn_child();
        // This process is the pane root; both children hang off it directly,
        // the way a pane shell holds a suspended agent and its replacement.
        let pane_tree_root = std::process::id();

        assert_eq!(
            resolve_report_ownership(
                Some(first.id()),
                second.id(),
                pane_tree_root,
                crate::platform::process_parent_pid
            ),
            ReportOwnership::Latch(second.id())
        );

        first.kill().expect("kill the first child process");
        first.wait().expect("reap the first child process");
        second.kill().expect("kill the second child process");
        second.wait().expect("reap the second child process");
    }
}
