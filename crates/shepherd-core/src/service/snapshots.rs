//! Automatic snapshots (F11b): state transitions, periodic captures, pre-kill/pre-clean and
//! recovery after a restart. All git work runs on a worker thread; callers never wait on it
//! except `capture_blocking`, which is bounded by [`SNAPSHOT_TIMEOUT`].

use super::OrchestrationService;
use crate::snapshot::{self, SnapshotInfo, SnapshotReason};
use crate::{Agent, AgentRegistryFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A capture running longer than this is considered stuck: callers stop waiting for it and
/// a new one may start.
pub(crate) const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the per-repo snapshot counts shown in `agents` stay cached.
const SUMMARY_TTL: Duration = Duration::from_secs(5);

type Summary = HashMap<String, (u32, String)>;

#[derive(Default)]
pub(crate) struct AutoSnapshots {
    /// Agent -> start of its running capture (at most one per agent).
    inflight: Mutex<HashMap<String, Instant>>,
    /// Agent -> state seen by the previous tick (transitions are edges, not levels).
    last_state: Mutex<HashMap<String, String>>,
    /// Agent -> clock ms of its last periodic-timer reset (start of `working`, or last capture).
    working_since: Mutex<HashMap<String, u64>>,
    /// Repo root -> (when read, agent -> (count, newest id)).
    summaries: Mutex<HashMap<String, (Instant, Summary)>>,
}

fn live_worktree(agent: &Agent) -> Option<PathBuf> {
    let wt = PathBuf::from(&agent.worktree);
    (!agent.worktree.is_empty() && wt.is_dir()).then_some(wt)
}

impl OrchestrationService {
    fn snapshot_limits(&self) -> snapshot::SnapshotLimits {
        self.shepherd_config.snapshots.into()
    }

    /// Run one capture on a worker thread unless one is already running for this agent.
    fn capture_async(&self, agent_id: &str, root: PathBuf, reason: SnapshotReason) {
        {
            let mut inflight = self.auto_snapshots.inflight.lock().unwrap();
            if inflight
                .get(agent_id)
                .is_some_and(|t| t.elapsed() < SNAPSHOT_TIMEOUT)
            {
                return;
            }
            inflight.insert(agent_id.to_owned(), Instant::now());
        }
        let svc = self.clone();
        let id = agent_id.to_owned();
        std::thread::spawn(move || {
            let limits = svc.snapshot_limits();
            if let Err(err) = snapshot::capture(&root, &id, reason, &limits) {
                svc.record_failure(
                    "snapshot",
                    &format!("{} snapshot of {id} failed: {err}", reason.as_str()),
                );
            }
            svc.auto_snapshots.summaries.lock().unwrap().clear();
            svc.auto_snapshots.inflight.lock().unwrap().remove(&id);
        });
    }

    /// Capture and wait (at most [`SNAPSHOT_TIMEOUT`]). `Err` carries a human-readable warning;
    /// callers continue with their action either way.
    fn capture_blocking(
        &self,
        agent: &Agent,
        root: PathBuf,
        reason: SnapshotReason,
    ) -> Result<Option<SnapshotInfo>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (limits, id) = (self.snapshot_limits(), agent.id.clone());
        std::thread::spawn(move || {
            let _ = tx.send(snapshot::capture(&root, &id, reason, &limits));
        });
        let res = match rx.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(res) => res,
            Err(_) => Err(format!("timed out after {}s", SNAPSHOT_TIMEOUT.as_secs())),
        };
        self.auto_snapshots.summaries.lock().unwrap().clear();
        match res {
            Ok(r) => Ok(r.created.then_some(r.snapshot)),
            Err(err) => {
                let msg = format!("snapshot {} of {} failed: {err}", reason.as_str(), agent.id);
                self.record_failure("snapshot", &msg);
                Err(msg)
            }
        }
    }

    /// `pre-kill` / `pre-clean`: returns the warning when the capture failed (the action goes on).
    pub(crate) fn pre_action_snapshot(
        &self,
        agent: &Agent,
        reason: SnapshotReason,
    ) -> Option<String> {
        let wt = live_worktree(agent)?;
        self.capture_blocking(agent, wt, reason).err()
    }

    /// Called after every reconciliation: fire idle/done on transitions and periodic captures
    /// for long `working` stretches.
    pub(crate) fn auto_snapshot_tick(&self, registry: &AgentRegistryFile) {
        let interval_ms = self
            .shepherd_config
            .snapshots
            .interval_secs
            .saturating_mul(1000);
        let now = self.clock.now_ms();
        let mut last_state = self.auto_snapshots.last_state.lock().unwrap();
        let mut since = self.auto_snapshots.working_since.lock().unwrap();
        last_state.retain(|id, _| registry.agents.iter().any(|a| a.id == *id && !a.gone()));
        since.retain(|id, _| last_state.contains_key(id));
        for agent in registry.agents.iter().filter(|a| !a.gone()) {
            let prev = last_state.insert(agent.id.clone(), agent.state.clone());
            let reason = match agent.state.as_str() {
                "idle" if prev.as_deref().is_some_and(|p| p != "idle") => {
                    Some(SnapshotReason::Idle)
                }
                "done" if prev.as_deref().is_some_and(|p| p != "done") => {
                    Some(SnapshotReason::Done)
                }
                "working" if interval_ms > 0 => {
                    let start = *since.entry(agent.id.clone()).or_insert(now);
                    (now.saturating_sub(start) >= interval_ms).then_some(SnapshotReason::Periodic)
                }
                _ => None,
            };
            if agent.state != "working" {
                since.remove(&agent.id);
            }
            if let (Some(reason), Some(wt)) = (reason, live_worktree(agent)) {
                since.insert(agent.id.clone(), now);
                self.capture_async(&agent.id, wt, reason);
            }
        }
    }

    /// After a restart: snapshot every restored agent whose worktree still exists, and leave a
    /// note in the service log for worktrees that vanished (their snapshots live in the base repo).
    pub(crate) fn recover_snapshots(&self, registry: &AgentRegistryFile) {
        for agent in &registry.agents {
            if agent.worktree.is_empty() {
                continue;
            }
            if let Some(wt) = live_worktree(agent) {
                if agent.gone() {
                    continue;
                }
                let reason = if agent.state == "done" {
                    SnapshotReason::Done
                } else {
                    SnapshotReason::Idle
                };
                self.capture_async(&agent.id, wt, reason);
                continue;
            }
            let root = if agent.root.is_empty() {
                Path::new(&agent.worktree)
            } else {
                Path::new(&agent.root)
            };
            let n = snapshot::list(root, Some(&agent.id), true).map_or(0, |s| s.len());
            if n > 0 {
                self.record_failure(
                    "snapshot",
                    &format!(
                        "worktree of {} is gone ({}); {n} snapshot(s) kept in the base repo (shepherd snapshot list --all)",
                        agent.id, agent.worktree
                    ),
                );
            }
        }
    }

    /// Fill `snapshots` / `last_snapshot` on public copies (cached per repo for a few seconds).
    pub(crate) fn annotate_snapshots(&self, agents: &mut [Agent]) {
        let mut cache = self.auto_snapshots.summaries.lock().unwrap();
        for agent in agents.iter_mut() {
            let root = if agent.root.is_empty() {
                &agent.worktree
            } else {
                &agent.root
            };
            if root.is_empty() {
                continue;
            }
            if cache
                .get(root)
                .is_none_or(|(t, _)| t.elapsed() > SUMMARY_TTL)
            {
                let mut summary = Summary::new();
                for s in snapshot::list(Path::new(root), None, true).unwrap_or_default() {
                    let e = summary.entry(s.agent).or_default();
                    *e = (e.0 + 1, s.id); // sorted by n: the last one is the newest
                }
                cache.insert(root.clone(), (Instant::now(), summary));
            }
            if let Some((n, last)) = cache[root].1.get(&agent.id) {
                agent.snapshots = *n;
                agent.last_snapshot = last.clone();
            }
        }
    }

    /// Block until no automatic capture is running (tests).
    #[cfg(test)]
    pub(crate) fn wait_snapshots_idle(&self) {
        let start = Instant::now();
        while !self.auto_snapshots.inflight.lock().unwrap().is_empty() {
            assert!(start.elapsed() < Duration::from_secs(30), "capture stuck");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Pretend a capture started `age` ago (tests).
    #[cfg(test)]
    pub(crate) fn mark_snapshot_inflight_for_test(&self, agent_id: &str, age: Duration) {
        let started = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        self.auto_snapshots
            .inflight
            .lock()
            .unwrap()
            .insert(agent_id.to_owned(), started);
    }
}
