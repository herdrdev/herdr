use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerObservation {
    pub source: String,
    pub worker_id: String,
    pub parent_session: String,
    pub parent_host: Option<String>,
    pub parent_terminal: Option<String>,
    pub parent_pane: Option<String>,
    pub last_activity: u64,
    pub finished: bool,
    pub parent_scope_local: bool,
    pub parent_state: ParentState,
    pub turn_id: Option<String>,
    pub semantic_hash: u64,
    pub trace_mtime: u64,
    pub trace: String,
    pub out: String,
    pub pid: Option<u32>,
    pub pid_alive: bool,
    pub pid_identity_ok: bool,
    pub tool_alive: bool,
    pub outstanding_op: Option<String>,
    pub progress_at: Option<u64>,
    pub state: String,
    pub blocked_reason: Option<String>,
    pub receipt_status: Option<String>,
    pub gate_verdict: Option<String>,
}

/// Semantic evidence gathered for one Codex turn or Claude subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerEvidence {
    pub turn_id: Option<String>,
    pub hash: u64,
    pub trace_mtime: u64,
    pub progress_at: Option<u64>,
    pub trace: String,
    pub out: String,
    pub pid: Option<u32>,
    pub pid_alive: bool,
    pub pid_identity_ok: bool,
    pub tool_alive: bool,
    pub outstanding_op: Option<String>,
    pub finished: bool,
    pub exit_code: Option<i64>,
    pub receipt_status: Option<String>,
    pub gate_verdict: Option<String>,
    pub blocked_reason: Option<String>,
    pub state: String,
    pub parent_host: Option<String>,
    pub parent_session: Option<String>,
    pub parent_terminal: Option<String>,
    pub parent_pane: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerClass {
    Working,
    ToolWait,
    WaitingToolInput,
    WaitingApproval,
    WaitingRetry,
    SuspectedStall,
    Finished,
    Dead,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SemanticWorkerMemory {
    #[serde(default)]
    pub(crate) workers: HashMap<String, SemanticWorkerEntry>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SemanticWorkerEntry {
    #[serde(default)]
    pub(crate) turn_id: Option<String>,
    #[serde(default)]
    pub(crate) hash: u64,
    #[serde(default)]
    pub(crate) since: u64,
    #[serde(default)]
    pub(crate) op_signature: Option<String>,
    #[serde(default)]
    pub(crate) op_since: u64,
    #[serde(default)]
    pub(crate) retry_since: Option<u64>,
    #[serde(default)]
    pub(crate) episode: u64,
    #[serde(default)]
    pub(crate) open_incident: Option<String>,
    #[serde(default)]
    pub(crate) logged: HashSet<String>,
    #[serde(default)]
    pub(crate) delivered: HashSet<String>,
    #[serde(default)]
    pub(crate) parent_absent_samples: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IncidentAction {
    Logged,
    AlreadyLogged,
    None,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IncidentDelivery {
    Delivered,
    Failed,
    DeferredRemote,
    None,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WorkerIncidentDecision {
    pub key: Option<String>,
    pub action: IncidentAction,
    pub delivery: IncidentDelivery,
    pub error: Option<String>,
}

pub(crate) fn process_semantic_incidents<L, D>(
    observations: &[WorkerObservation],
    classes: &[WorkerClass],
    memory: &mut SemanticWorkerMemory,
    now: u64,
    dry_run: bool,
    mut log: L,
    mut deliver: D,
) -> Vec<WorkerIncidentDecision>
where
    L: FnMut(&WorkerObservation, &str, u64) -> std::io::Result<()>,
    D: FnMut(&WorkerObservation, bool, u64) -> std::io::Result<()>,
{
    let mut output = Vec::with_capacity(observations.len());
    for (worker, class) in observations.iter().zip(classes) {
        let entry = memory.workers.entry(worker.key()).or_default();
        let orphaned = worker.parent_state == ParentState::Absent && !worker.finished;
        let worthy = orphaned
            || matches!(
                class,
                WorkerClass::SuspectedStall
                    | WorkerClass::WaitingToolInput
                    | WorkerClass::WaitingApproval
                    | WorkerClass::Dead
            );
        if !worthy {
            entry.open_incident = None;
            output.push(WorkerIncidentDecision {
                key: None,
                action: IncidentAction::None,
                delivery: IncidentDelivery::None,
                error: None,
            });
            continue;
        }
        let name = if orphaned {
            "orphaned"
        } else {
            class_name(*class)
        };
        let turn = worker.turn_id.as_deref().unwrap_or("unknown-turn");
        let key = format!(
            "{}:{}:{turn}:{name}:{}",
            worker.source, worker.worker_id, entry.episode
        );
        if entry.open_incident.as_deref() != Some(&key) {
            entry.episode = entry.episode.saturating_add(1);
            entry.open_incident = Some(format!(
                "{}:{}:{turn}:{name}:{}",
                worker.source, worker.worker_id, entry.episode
            ));
        }
        let key = entry.open_incident.clone().unwrap_or(key);
        let age_secs = now.saturating_sub(entry.since);
        if dry_run {
            output.push(WorkerIncidentDecision {
                key: Some(key),
                action: IncidentAction::None,
                delivery: IncidentDelivery::None,
                error: None,
            });
            continue;
        }
        let action = if entry.logged.contains(&key) {
            IncidentAction::AlreadyLogged
        } else {
            match log(worker, name, age_secs) {
                Ok(()) => {
                    entry.logged.insert(key.clone());
                    IncidentAction::Logged
                }
                Err(error) => {
                    output.push(WorkerIncidentDecision {
                        key: Some(key),
                        action: IncidentAction::None,
                        delivery: IncidentDelivery::Failed,
                        error: Some(format!("incident persistence failed: {error}")),
                    });
                    continue;
                }
            }
        };
        if entry.delivered.contains(&key) {
            output.push(WorkerIncidentDecision {
                key: Some(key),
                action,
                delivery: IncidentDelivery::None,
                error: None,
            });
        } else if !orphaned && !worker.parent_scope_local {
            output.push(WorkerIncidentDecision {
                key: Some(key),
                action,
                delivery: IncidentDelivery::DeferredRemote,
                error: None,
            });
        } else {
            match deliver(worker, orphaned, age_secs) {
                Ok(()) => {
                    entry.delivered.insert(key.clone());
                    output.push(WorkerIncidentDecision {
                        key: Some(key),
                        action,
                        delivery: IncidentDelivery::Delivered,
                        error: None,
                    });
                }
                Err(error) => output.push(WorkerIncidentDecision {
                    key: Some(key),
                    action,
                    delivery: IncidentDelivery::Failed,
                    error: Some(error.to_string()),
                }),
            }
        }
    }
    output
}

fn class_name(class: WorkerClass) -> &'static str {
    match class {
        WorkerClass::Working => "working",
        WorkerClass::ToolWait => "tool_wait",
        WorkerClass::WaitingToolInput => "waiting_tool_input",
        WorkerClass::WaitingApproval => "waiting_approval",
        WorkerClass::WaitingRetry => "waiting_retry",
        WorkerClass::SuspectedStall => "suspected_stall",
        WorkerClass::Finished => "finished",
        WorkerClass::Dead => "dead",
    }
}

pub(crate) fn classify_worker(
    evidence: &WorkerEvidence,
    memory: &mut SemanticWorkerEntry,
    now: u64,
    stall_secs: u64,
    retry_window_secs: u64,
    op_deadline_secs: u64,
    retry_secs: Option<u64>,
) -> (WorkerClass, u64, String) {
    let first_sight = memory.since == 0 && memory.turn_id.is_none();
    if evidence.finished {
        return (
            WorkerClass::Finished,
            0,
            if evidence.trace.starts_with("inactive: ") {
                evidence.trace.clone()
            } else if evidence.trace.starts_with("interrupted by user:") {
                "interrupted by user".into()
            } else {
                format!(
                    "finished; receipt={:?}; gate_verdict={:?}",
                    evidence.receipt_status, evidence.gate_verdict
                )
            },
        );
    }
    if evidence.pid.is_some() && (!evidence.pid_alive || !evidence.pid_identity_ok) {
        return (
            WorkerClass::Dead,
            0,
            "pid absent or process identity does not match started_at".into(),
        );
    }
    if let Some(turn) = &evidence.turn_id {
        if memory.turn_id.as_ref() != Some(turn) {
            memory.turn_id = Some(turn.clone());
            if !first_sight {
                memory.since = now;
            }
            if memory.op_signature.as_deref() != evidence.outstanding_op.as_deref() {
                memory.op_since = now;
            }
        }
    }
    if evidence.hash != memory.hash {
        memory.hash = evidence.hash;
        memory.since = if first_sight && evidence.trace_mtime > 0 {
            evidence.trace_mtime
        } else {
            now
        };
    }
    if evidence.progress_at.is_some_and(|p| p > memory.since) {
        memory.since = evidence.progress_at.unwrap_or(memory.since);
    }
    if evidence.trace_mtime > 0 && memory.since == 0 {
        memory.since = evidence.trace_mtime;
    }
    let age = now.saturating_sub(memory.since);
    if memory.op_signature != evidence.outstanding_op {
        memory.op_signature = evidence.outstanding_op.clone();
        memory.op_since = now;
    }
    if evidence.blocked_reason.is_some()
        || evidence.state.eq_ignore_ascii_case("blocked")
        || crate::watchdog::evidence::active_prompt(&evidence.trace)
            .is_some_and(|p| p.kind == crate::watchdog::evidence::PromptKind::Dialog)
    {
        return (
            WorkerClass::WaitingApproval,
            age,
            "worker reports blocked or has an active approval dialog".into(),
        );
    }
    let prompt =
        crate::watchdog::evidence::active_prompt(&format!("{}\n{}", evidence.trace, evidence.out));
    if prompt.is_some_and(|p| p.kind == crate::watchdog::evidence::PromptKind::YesNo)
        && evidence.tool_alive
    {
        return (
            WorkerClass::WaitingToolInput,
            age,
            "active yes/no prompt with a live tool descendant".into(),
        );
    }
    if let Some(secs) = retry_secs {
        let since = *memory.retry_since.get_or_insert(now);
        if now.saturating_sub(since) > retry_window_secs {
            return (
                WorkerClass::SuspectedStall,
                age,
                format!(
                    "retries renewed for {}s without progress",
                    now.saturating_sub(since)
                ),
            );
        }
        if now <= evidence.trace_mtime.saturating_add(secs).saturating_add(60) {
            return (
                WorkerClass::WaitingRetry,
                age,
                format!("scheduled retry in {secs}s"),
            );
        }
    } else {
        memory.retry_since = None;
    }
    if let Some(op) = &evidence.outstanding_op {
        if now.saturating_sub(memory.op_since) > op_deadline_secs {
            return (
                WorkerClass::SuspectedStall,
                age,
                format!(
                    "operation {op} outstanding {}s across resumes",
                    now.saturating_sub(memory.op_since)
                ),
            );
        }
    }
    if age < stall_secs {
        return (
            WorkerClass::Working,
            age,
            "semantic progress is within stall window".into(),
        );
    }
    if evidence.tool_alive {
        (
            WorkerClass::ToolWait,
            age,
            "live tool descendant; worker trace is quiet".into(),
        )
    } else {
        (
            WorkerClass::SuspectedStall,
            age,
            format!("no semantic progress for {age}s"),
        )
    }
}

/// Return the command from the last Codex `exec` block that has no completion marker.
pub(crate) fn outstanding_codex_operation(trace: &str) -> Option<String> {
    let mut last_exec: Option<String> = None;
    let mut in_exec = false;
    for line in trace.lines() {
        let trimmed = line.trim();
        if matches!(trimmed, "codex" | "exec") {
            in_exec = trimmed == "exec";
            if !in_exec {
                last_exec = None;
            }
            continue;
        }
        if in_exec {
            if trimmed.starts_with("succeeded in ")
                || trimmed.starts_with("exited ") && trimmed.contains(" in ")
            {
                last_exec = None;
                in_exec = false;
            } else if !trimmed.is_empty() {
                last_exec = Some(trimmed.to_owned());
                in_exec = false;
            }
        } else if trimmed.starts_with("succeeded in ")
            || trimmed.starts_with("exited ") && trimmed.contains(" in ")
        {
            last_exec = None;
        }
    }
    last_exec
}

impl WorkerObservation {
    pub(crate) fn key(&self) -> String {
        format!("{}:{}", self.source, self.worker_id)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ParentState {
    Present,
    Absent,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct WorkerMemory {
    #[serde(default)]
    pub(crate) semantic: SemanticWorkerMemory,
    #[serde(default)]
    recorded_activity: HashMap<String, u64>,
    #[serde(default)]
    notified_activity: HashMap<String, u64>,
    #[serde(default)]
    parent_absence: HashMap<String, ParentAbsence>,
    #[serde(default)]
    orphaned_notified: HashSet<String>,
    #[serde(default)]
    orphaned_logged: HashSet<String>,
    #[serde(default)]
    stale_candidates: HashMap<String, StaleCandidate>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ParentAbsence {
    first_seen: u64,
    last_seen: u64,
    samples: u8,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct StaleCandidate {
    activity: u64,
    first_seen: u64,
    last_seen: u64,
    samples: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg(test)]
pub(crate) enum StallAction {
    ParentChecking,
    ParentUnknown,
    Orphaned,
    Confirming,
    WouldNotify,
    Notified,
    AlreadyNotified,
    NotifyFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg(test)]
pub(crate) struct WorkerStallDecision {
    pub worker_id: String,
    pub parent_session: String,
    pub age_secs: u64,
    pub action: StallAction,
    pub error: Option<String>,
    pub parent_state: ParentState,
}

/// Persist one incident per stale progress episode, then retry delivery until it succeeds.
/// This path only notifies; it never types into, resumes, or terminates a worker.
#[cfg(test)]
pub(crate) fn process_stalls<N, L>(
    observations: &[WorkerObservation],
    memory: &mut WorkerMemory,
    now: u64,
    stall_after_secs: u64,
    dry_run: bool,
    mut notify_parent: N,
    mut log_stall: L,
) -> Vec<WorkerStallDecision>
where
    N: FnMut(&WorkerObservation, u64) -> std::io::Result<()>,
    L: FnMut(&WorkerObservation, u64) -> std::io::Result<()>,
{
    let live_keys = observations
        .iter()
        .filter(|worker| !worker.finished)
        .map(WorkerObservation::key)
        .collect::<HashSet<_>>();
    memory
        .notified_activity
        .retain(|worker_key, _| live_keys.contains(worker_key));
    memory
        .recorded_activity
        .retain(|worker_key, _| live_keys.contains(worker_key));
    memory
        .parent_absence
        .retain(|key, _| live_keys.contains(key));
    memory
        .orphaned_notified
        .retain(|key| live_keys.contains(key));
    memory.orphaned_logged.retain(|key| live_keys.contains(key));
    memory
        .stale_candidates
        .retain(|key, _| live_keys.contains(key));

    let mut decisions = Vec::new();
    for worker in observations {
        if worker.finished {
            continue;
        }
        let key = worker.key();
        match worker.parent_state {
            ParentState::Unknown => {
                memory.parent_absence.remove(&key);
                memory.orphaned_notified.remove(&key);
                memory.orphaned_logged.remove(&key);
                decisions.push(WorkerStallDecision {
                    worker_id: worker.worker_id.clone(),
                    parent_session: worker.parent_session.clone(),
                    age_secs: 0,
                    action: StallAction::ParentUnknown,
                    error: None,
                    parent_state: ParentState::Unknown,
                });
                continue;
            }
            ParentState::Absent => {
                let absence = memory
                    .parent_absence
                    .entry(key.clone())
                    .or_insert(ParentAbsence {
                        first_seen: now,
                        last_seen: now,
                        samples: 0,
                    });
                if absence.samples == 0 {
                    absence.samples = 1;
                    absence.last_seen = now;
                    decisions.push(parent_decision(worker, 0, StallAction::ParentChecking));
                    continue;
                }
                absence.samples = 2;
                absence.last_seen = now;
                if memory.orphaned_notified.contains(&key) {
                    decisions.push(parent_decision(
                        worker,
                        now.saturating_sub(absence.first_seen),
                        StallAction::Orphaned,
                    ));
                    continue;
                }
                if dry_run {
                    decisions.push(parent_decision(
                        worker,
                        now.saturating_sub(absence.first_seen),
                        StallAction::Orphaned,
                    ));
                    continue;
                }
                let age = now.saturating_sub(absence.first_seen);
                if !memory.orphaned_logged.contains(&key) {
                    if let Err(error) = log_stall(worker, age) {
                        decisions.push(WorkerStallDecision {
                            worker_id: worker.worker_id.clone(),
                            parent_session: worker.parent_session.clone(),
                            age_secs: age,
                            action: StallAction::NotifyFailed,
                            error: Some(format!("orphan incident log failed: {error}")),
                            parent_state: ParentState::Absent,
                        });
                        continue;
                    }
                    memory.orphaned_logged.insert(key.clone());
                }
                match notify_parent(worker, age) {
                    Ok(()) => {
                        memory.orphaned_notified.insert(key);
                        decisions.push(parent_decision(worker, age, StallAction::Orphaned));
                    }
                    Err(error) => decisions.push(WorkerStallDecision {
                        worker_id: worker.worker_id.clone(),
                        parent_session: worker.parent_session.clone(),
                        age_secs: age,
                        action: StallAction::NotifyFailed,
                        error: Some(error.to_string()),
                        parent_state: ParentState::Absent,
                    }),
                }
                continue;
            }
            ParentState::Present => {
                memory.parent_absence.remove(&key);
                memory.orphaned_notified.remove(&key);
                memory.orphaned_logged.remove(&key);
            }
        }
        if worker.last_activity == 0 {
            continue;
        }
        let age_secs = now.saturating_sub(worker.last_activity);
        if age_secs < stall_after_secs {
            memory.stale_candidates.remove(&key);
            continue;
        }
        let candidate = memory.stale_candidates.entry(key.clone()).or_default();
        if candidate.activity != worker.last_activity {
            *candidate = StaleCandidate {
                activity: worker.last_activity,
                first_seen: now,
                last_seen: now,
                samples: 1,
            };
            decisions.push(parent_decision(worker, age_secs, StallAction::Confirming));
            continue;
        }
        if candidate.samples < 2 {
            candidate.samples = 2;
            candidate.last_seen = now;
        }
        if dry_run {
            decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::WouldNotify,
                error: None,
                parent_state: worker.parent_state,
            });
            continue;
        }

        if memory.recorded_activity.get(&key) != Some(&worker.last_activity) {
            if let Err(error) = log_stall(worker, age_secs) {
                decisions.push(WorkerStallDecision {
                    worker_id: worker.worker_id.clone(),
                    parent_session: worker.parent_session.clone(),
                    age_secs,
                    action: StallAction::NotifyFailed,
                    error: Some(format!("incident log failed: {error}")),
                    parent_state: worker.parent_state,
                });
                continue;
            }
            memory
                .recorded_activity
                .insert(key.clone(), worker.last_activity);
        }

        if memory.notified_activity.get(&key) == Some(&worker.last_activity) {
            decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::AlreadyNotified,
                error: None,
                parent_state: worker.parent_state,
            });
            continue;
        }

        match notify_parent(worker, age_secs) {
            Ok(()) => {
                memory.notified_activity.insert(key, worker.last_activity);
                decisions.push(WorkerStallDecision {
                    worker_id: worker.worker_id.clone(),
                    parent_session: worker.parent_session.clone(),
                    age_secs,
                    action: StallAction::Notified,
                    error: None,
                    parent_state: worker.parent_state,
                });
            }
            Err(error) => decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::NotifyFailed,
                error: Some(error.to_string()),
                parent_state: worker.parent_state,
            }),
        }
    }
    decisions
}

#[cfg(test)]
fn parent_decision(
    worker: &WorkerObservation,
    age_secs: u64,
    action: StallAction,
) -> WorkerStallDecision {
    WorkerStallDecision {
        worker_id: worker.worker_id.clone(),
        parent_session: worker.parent_session.clone(),
        age_secs,
        action,
        error: None,
        parent_state: worker.parent_state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(last_activity: u64, finished: bool) -> WorkerObservation {
        WorkerObservation {
            source: "codex".into(),
            worker_id: "run-1".into(),
            parent_session: "parent-1".into(),
            parent_host: None,
            parent_terminal: None,
            parent_pane: None,
            last_activity,
            finished,
            parent_scope_local: true,
            parent_state: ParentState::Present,
            turn_id: None,
            semantic_hash: last_activity,
            trace_mtime: last_activity,
            trace: String::new(),
            out: String::new(),
            pid: None,
            pid_alive: false,
            pid_identity_ok: false,
            tool_alive: false,
            outstanding_op: None,
            progress_at: Some(last_activity),
            state: String::new(),
            blocked_reason: None,
            receipt_status: None,
            gate_verdict: None,
        }
    }

    fn semantic() -> WorkerEvidence {
        WorkerEvidence {
            turn_id: Some("turn-1".into()),
            hash: 10,
            trace_mtime: 900,
            progress_at: None,
            trace: "Compiling target".into(),
            out: String::new(),
            pid: Some(10),
            pid_alive: true,
            pid_identity_ok: true,
            tool_alive: false,
            outstanding_op: None,
            finished: false,
            exit_code: None,
            receipt_status: None,
            gate_verdict: None,
            blocked_reason: None,
            state: "active".into(),
            parent_host: None,
            parent_session: None,
            parent_terminal: None,
            parent_pane: None,
        }
    }

    #[test]
    fn semantic_worker_classification_covers_terminal_dead_prompt_retry_tool_and_stall() {
        let mut memory = SemanticWorkerEntry::default();
        let mut evidence = semantic();
        assert_eq!(
            classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, None).0,
            WorkerClass::Working
        );
        evidence.finished = true;
        assert_eq!(
            classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, None).0,
            WorkerClass::Finished
        );
        evidence.finished = false;
        evidence.pid_alive = false;
        assert_eq!(
            classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, None).0,
            WorkerClass::Dead
        );
        evidence.pid_alive = true;
        evidence.blocked_reason = Some("approval".into());
        assert_eq!(
            classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, None).0,
            WorkerClass::WaitingApproval
        );
        evidence.blocked_reason = None;
        evidence.trace = "Overwrite? [y/n]".into();
        evidence.tool_alive = true;
        assert_eq!(
            classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, None).0,
            WorkerClass::WaitingToolInput
        );
        evidence.trace.clear();
        evidence.tool_alive = false;
        assert_eq!(
            classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, Some(120)).0,
            WorkerClass::WaitingRetry
        );
        memory.since = 1;
        evidence.hash = memory.hash;
        evidence.trace_mtime = 1;
        evidence.outstanding_op = Some("cargo test".into());
        let _ = classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, None);
        memory.op_since = 1;
        assert_eq!(
            classify_worker(&evidence, &mut memory, 2_000, 300, 300, 600, None).0,
            WorkerClass::SuspectedStall
        );
    }

    #[test]
    fn claude_live_worker_is_working_and_live_parent_pending_tool_can_stall() {
        let mut evidence = semantic();
        evidence.pid = None;
        evidence.progress_at = Some(998);
        evidence.trace_mtime = 998;
        evidence.outstanding_op = None;
        let mut memory = SemanticWorkerEntry::default();
        assert_eq!(
            classify_worker(&evidence, &mut memory, 1_000, 300, 300, 600, None).0,
            WorkerClass::Working
        );

        evidence.progress_at = None;
        evidence.trace_mtime = 100;
        evidence.hash = 1;
        evidence.outstanding_op = Some("Bash".into());
        let mut memory = SemanticWorkerEntry {
            hash: 1,
            since: 100,
            op_signature: Some("Bash".into()),
            op_since: 100,
            ..SemanticWorkerEntry::default()
        };
        assert_eq!(
            classify_worker(&evidence, &mut memory, 5_000, 300, 300, 600, None).0,
            WorkerClass::SuspectedStall
        );
    }

    #[test]
    fn inactive_claude_subagent_evidence_classifies_finished() {
        let mut evidence = semantic();
        evidence.finished = true;
        evidence.trace = "inactive: transcript idle 20d, parent session not live".into();
        let (_, _, reason) = classify_worker(
            &evidence,
            &mut SemanticWorkerEntry::default(),
            5_000,
            300,
            300,
            600,
            None,
        );
        assert_eq!(
            reason,
            "inactive: transcript idle 20d, parent session not live"
        );
    }

    #[test]
    fn codex_trace_selects_only_an_uncompleted_exec_operation() {
        assert_eq!(
            outstanding_codex_operation("codex\nexec\ncargo test\n"),
            Some("cargo test".into())
        );
        assert_eq!(
            outstanding_codex_operation("codex\nexec\ncargo test\n succeeded in 10ms:\n"),
            None
        );
        assert_eq!(
            outstanding_codex_operation("exec\nfirst\n succeeded in 1ms:\nexec\nsecond\n"),
            Some("second".into())
        );
    }

    #[test]
    fn resumed_turn_preserves_age_for_same_operation() {
        let mut memory = SemanticWorkerEntry::default();
        let mut evidence = semantic();
        evidence.outstanding_op = Some("cargo test".into());
        let _ = classify_worker(&evidence, &mut memory, 100, 300, 300, 600, None);
        memory.op_since = 5;
        evidence.turn_id = Some("turn-2".into());
        let _ = classify_worker(&evidence, &mut memory, 200, 300, 300, 600, None);
        assert_eq!(memory.op_since, 5);
        assert_eq!(memory.since, 200);
        evidence.outstanding_op = Some("cargo check".into());
        let _ = classify_worker(&evidence, &mut memory, 210, 300, 300, 600, None);
        assert_eq!(memory.op_since, 210);
    }

    #[test]
    fn semantic_incidents_persist_before_delivery_retry_and_rearm_after_recovery() {
        let worker = worker(100, false);
        let mut memory = SemanticWorkerMemory::default();
        let events = std::cell::RefCell::new(Vec::new());
        let first = process_semantic_incidents(
            std::slice::from_ref(&worker),
            &[WorkerClass::SuspectedStall],
            &mut memory,
            500,
            false,
            |_, _, _| {
                events.borrow_mut().push("log");
                Ok(())
            },
            |_, _, _| {
                events.borrow_mut().push("deliver");
                Err(std::io::Error::other("offline"))
            },
        );
        assert_eq!(first[0].action, IncidentAction::Logged);
        assert_eq!(*events.borrow(), ["log", "deliver"]);
        let second = process_semantic_incidents(
            std::slice::from_ref(&worker),
            &[WorkerClass::SuspectedStall],
            &mut memory,
            530,
            false,
            |_, _, _| panic!("an incident is logged only once"),
            |_, _, _| Ok(()),
        );
        assert_eq!(second[0].action, IncidentAction::AlreadyLogged);
        assert_eq!(second[0].delivery, IncidentDelivery::Delivered);
        let healthy = process_semantic_incidents(
            std::slice::from_ref(&worker),
            &[WorkerClass::Working],
            &mut memory,
            600,
            false,
            |_, _, _| Ok(()),
            |_, _, _| Ok(()),
        );
        assert_eq!(healthy[0].action, IncidentAction::None);
        let reopened = process_semantic_incidents(
            &[worker],
            &[WorkerClass::SuspectedStall],
            &mut memory,
            700,
            false,
            |_, _, _| Ok(()),
            |_, _, _| Ok(()),
        );
        assert_ne!(first[0].key, reopened[0].key);
    }

    #[test]
    fn dry_run_saves_episode_state_without_logging_or_delivery() {
        let worker = worker(100, false);
        let mut memory = SemanticWorkerMemory::default();
        let result = process_semantic_incidents(
            &[worker],
            &[WorkerClass::Dead],
            &mut memory,
            500,
            true,
            |_, _, _| panic!("dry-run must not append"),
            |_, _, _| panic!("dry-run must not deliver"),
        );
        assert_eq!(result[0].action, IncidentAction::None);
        assert!(memory
            .workers
            .values()
            .any(|entry| entry.open_incident.is_some()));
    }

    #[test]
    fn recent_and_finished_workers_are_not_notified() {
        let observations = [worker(950, false), worker(100, true)];
        let mut memory = WorkerMemory::default();
        let mut notifications = 0;
        let decisions = process_stalls(
            &observations,
            &mut memory,
            1_000,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| Ok(()),
        );
        assert!(decisions.is_empty());
        assert_eq!(notifications, 0);
    }

    #[test]
    fn worker_is_notified_once_per_stalled_activity_timestamp() {
        let mut memory = WorkerMemory::default();
        let stale = worker(100, false);
        let mut notifications = 0;
        let mut logs = 0;
        let first = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            500,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| {
                logs += 1;
                Ok(())
            },
        );
        let second = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            700,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| {
                logs += 1;
                Ok(())
            },
        );
        assert_eq!(first[0].action, StallAction::Confirming);
        assert_eq!(second[0].action, StallAction::Notified);
        assert_eq!(notifications, 1);
        assert_eq!(logs, 1);

        let already = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            730,
            300,
            false,
            |_, _| panic!("the same incident is not delivered twice"),
            |_, _| panic!("the same incident is not logged twice"),
        );
        assert_eq!(already[0].action, StallAction::AlreadyNotified);

        let resumed = worker(600, false);
        let _ = process_stalls(
            std::slice::from_ref(&resumed),
            &mut memory,
            1_000,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| Ok(()),
        );
        let after_new_stall = process_stalls(
            &[resumed],
            &mut memory,
            1_030,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| Ok(()),
        );
        assert_eq!(after_new_stall[0].action, StallAction::Notified);
        assert_eq!(notifications, 2);
    }

    #[test]
    fn dry_run_reports_stalls_without_notifying_logging_or_deduplicating() {
        let stale = worker(100, false);
        let mut memory = WorkerMemory::default();
        let mut notifications = 0;
        let mut logs = 0;
        let _ = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            500,
            300,
            true,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| {
                logs += 1;
                Ok(())
            },
        );
        let decisions = process_stalls(
            &[stale],
            &mut memory,
            530,
            300,
            true,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| {
                logs += 1;
                Ok(())
            },
        );
        assert_eq!(decisions[0].action, StallAction::WouldNotify);
        assert_eq!(notifications, 0);
        assert_eq!(logs, 0);
        assert!(memory.notified_activity.is_empty());
    }

    #[test]
    fn incident_is_logged_before_delivery_and_delivery_retries_without_duplicate_log() {
        let stale = worker(100, false);
        let mut memory = WorkerMemory::default();
        let events = std::cell::RefCell::new(Vec::new());
        let _ = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            500,
            300,
            false,
            |_, _| panic!("first stale sample only starts confirmation"),
            |_, _| panic!("confirmation must precede incident persistence"),
        );
        let first = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            530,
            300,
            false,
            |_, _| {
                events.borrow_mut().push("notify");
                Err(std::io::Error::other("temporarily unavailable"))
            },
            |_, _| {
                events.borrow_mut().push("log");
                Ok(())
            },
        );
        assert_eq!(first[0].action, StallAction::NotifyFailed);
        assert_eq!(*events.borrow(), ["log", "notify"]);

        let second = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            560,
            300,
            false,
            |_, _| {
                events.borrow_mut().push("notify");
                Ok(())
            },
            |_, _| panic!("the persisted incident must not be logged twice"),
        );
        assert_eq!(second[0].action, StallAction::Notified);
        assert_eq!(*events.borrow(), ["log", "notify", "notify"]);

        let third = process_stalls(
            &[stale],
            &mut memory,
            590,
            300,
            false,
            |_, _| panic!("successful delivery must be deduplicated"),
            |_, _| panic!("the incident must remain deduplicated"),
        );
        assert_eq!(third[0].action, StallAction::AlreadyNotified);
    }
}
