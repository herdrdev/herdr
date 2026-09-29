use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::schema::{
        AgentSessionInfo, Method, NotificationShowParams, NotificationShowSound, PaneListParams,
        Request,
    },
    watchdog::evidence::{self, ProcSample},
    watchdog::workers::{self as worker_watchdog, ParentState, WorkerMemory, WorkerObservation},
};

const DEFAULT_INTERVAL_SECS: u64 = 30;
const DEFAULT_STALL_MINUTES: u64 = 30;

#[derive(Debug, Clone)]
struct WorkerOptions {
    once: bool,
    dry_run: bool,
    json: bool,
    interval_secs: u64,
    stall_secs: u64,
    runs_dir: PathBuf,
    claude_projects_dir: PathBuf,
    state_file: PathBuf,
    log_file: PathBuf,
    confirm_secs: u64,
    op_deadline_secs: u64,
    history_days: u64,
    all: bool,
    local_hosts: Vec<String>,
    parent_probe: String,
}

#[derive(Debug, Deserialize)]
struct PaneEntry {
    pane_id: String,
    #[serde(default)]
    agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    terminal_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorkerSummary {
    scanned: usize,
    skipped_old: usize,
    scan_ms: u128,
    by_source: SourceClassCounts,
    dry_run: bool,
    notified: usize,
    logged: usize,
}

type SourceClassCounts =
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, usize>>;

#[derive(Debug, Serialize)]
struct SemanticDecision {
    worker_id: String,
    source: String,
    turn_id: Option<String>,
    class: worker_watchdog::WorkerClass,
    parent: &'static str,
    parent_detail: String,
    age_secs: u64,
    evidence: String,
    samples: Vec<Value>,
    incident: worker_watchdog::WorkerIncidentDecision,
}

pub(super) fn run_worker_watchdog_command(args: &[String]) -> io::Result<i32> {
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "--help" | "-h" | "help"))
    {
        println!(
            "usage: herdr watchdog workers [--once] [--dry-run] [--json] [--all] [--history-days N] [--stall-minutes N] [--confirm-secs N] [--op-deadline-minutes N] [--runs-dir PATH] [--claude-projects-dir PATH] [--state-file PATH] [--log-file PATH] [--local-host NAME]... [--parent-probe CMD] [--interval-secs N]"
        );
        return Ok(0);
    }
    let options = match parse_worker_options(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return Ok(2);
        }
    };
    if options.once {
        return run_worker_scan(&options);
    }

    loop {
        if let Err(error) = run_worker_scan(&options) {
            eprintln!("worker watchdog scan failed: {error}");
        }
        thread::sleep(Duration::from_secs(options.interval_secs));
    }
}

fn parse_worker_options(args: &[String]) -> Result<WorkerOptions, String> {
    let home = home_dir();
    let mut options = WorkerOptions {
        once: false,
        dry_run: false,
        json: false,
        interval_secs: DEFAULT_INTERVAL_SECS,
        stall_secs: DEFAULT_STALL_MINUTES * 60,
        runs_dir: home.join(".agents/runs"),
        claude_projects_dir: home.join(".claude/projects"),
        state_file: crate::config::state_dir().join("worker-watchdog.json"),
        log_file: crate::config::state_dir().join("worker-watchdog.jsonl"),
        confirm_secs: 20,
        op_deadline_secs: 60 * 60,
        local_hosts: Vec::new(),
        parent_probe: "ssh -o BatchMode=yes -o ConnectTimeout=5".into(),
        history_days: 7,
        all: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--once" => options.once = true,
            "--dry-run" => options.dry_run = true,
            "--json" => options.json = true,
            "--all" => options.all = true,
            "--history-days" => {
                options.history_days = super::parse_value(args, &mut index, "--history-days")?;
                if options.history_days == 0 {
                    return Err("--history-days must be greater than zero".into());
                }
            }
            "--interval-secs" => {
                options.interval_secs = super::parse_value(args, &mut index, "--interval-secs")?;
                if options.interval_secs == 0 {
                    return Err("--interval-secs must be greater than zero".into());
                }
            }
            "--stall-minutes" => {
                let minutes = super::parse_value::<u64>(args, &mut index, "--stall-minutes")?;
                if minutes == 0 {
                    return Err("--stall-minutes must be greater than zero".into());
                }
                options.stall_secs = minutes
                    .checked_mul(60)
                    .ok_or_else(|| "--stall-minutes is too large".to_string())?;
            }
            "--confirm-secs" => {
                options.confirm_secs = super::parse_value(args, &mut index, "--confirm-secs")?
            }
            "--op-deadline-minutes" => {
                let minutes = super::parse_value::<u64>(args, &mut index, "--op-deadline-minutes")?;
                options.op_deadline_secs = minutes
                    .checked_mul(60)
                    .ok_or_else(|| "--op-deadline-minutes is too large".to_string())?;
            }
            "--local-host" => {
                options
                    .local_hosts
                    .push(super::parse_string(args, &mut index, "--local-host")?)
            }
            "--parent-probe" => {
                options.parent_probe = super::parse_string(args, &mut index, "--parent-probe")?
            }
            "--runs-dir" => {
                options.runs_dir =
                    PathBuf::from(super::parse_string(args, &mut index, "--runs-dir")?);
            }
            "--claude-projects-dir" => {
                options.claude_projects_dir = PathBuf::from(super::parse_string(
                    args,
                    &mut index,
                    "--claude-projects-dir",
                )?);
            }
            "--state-file" => {
                options.state_file =
                    PathBuf::from(super::parse_string(args, &mut index, "--state-file")?);
            }
            "--log-file" => {
                options.log_file =
                    PathBuf::from(super::parse_string(args, &mut index, "--log-file")?);
            }
            unknown => return Err(format!("unknown worker watchdog option: {unknown}")),
        }
        index += 1;
    }
    Ok(options)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn run_worker_scan(options: &WorkerOptions) -> io::Result<i32> {
    let scan_started = std::time::Instant::now();
    let _scan_controls = (
        options.confirm_secs,
        options.op_deadline_secs,
        &options.local_hosts,
        &options.parent_probe,
    );
    let mut memory = load_memory(&options.state_file)?;
    let discovery = discover_workers(
        &options.runs_dir,
        &options.claude_projects_dir,
        options.history_days,
        options.op_deadline_secs,
        options.stall_secs,
    )?;
    let skipped_old = discovery.skipped_old;
    let mut observations = discovery.workers;
    let pane_list = super::super::send_request(&Request {
        id: super::next_request_id("worker-watchdog-pane-list"),
        method: Method::PaneList(PaneListParams { workspace_id: None }),
    })?;
    super::ensure_api_success(&pane_list)?;
    let panes = parse_pane_entries(&pane_list)?;
    let parents = parent_panes(&panes);
    let mut parent_checks = std::collections::HashMap::new();
    for worker in &mut observations {
        let host_is_local = worker.parent_host.as_deref().is_none_or(|host| {
            local_host_aliases(&options.local_hosts)
                .iter()
                .any(|alias| alias == host)
        });
        let found = if host_is_local {
            parent_matches(worker, &panes)
        } else if options.dry_run {
            None
        } else {
            remote_parent_present(worker, options).unwrap_or(None)
        };
        parent_checks.insert(worker.key(), found);
        worker.parent_scope_local = host_is_local;
        worker.parent_state = match found {
            Some(true) => ParentState::Present,
            Some(false) => ParentState::Absent,
            None => ParentState::Unknown,
        };
    }
    if observations
        .iter()
        .any(|worker| worker.parent_state == ParentState::Absent)
    {
        thread::sleep(Duration::from_secs(options.confirm_secs));
        let local_second = if observations
            .iter()
            .any(|w| w.parent_state == ParentState::Absent && w.parent_scope_local)
        {
            let response = super::super::send_request(&Request {
                id: super::next_request_id("worker-watchdog-parent-confirm"),
                method: Method::PaneList(PaneListParams { workspace_id: None }),
            })?;
            super::ensure_api_success(&response)?;
            Some(parse_pane_entries(&response)?)
        } else {
            None
        };
        for worker in observations
            .iter_mut()
            .filter(|worker| worker.parent_state == ParentState::Absent)
        {
            let found = if worker.parent_scope_local {
                local_second
                    .as_deref()
                    .and_then(|panes| parent_matches(worker, panes))
            } else if options.dry_run {
                None
            } else {
                remote_parent_present(worker, options).unwrap_or(None)
            };
            worker.parent_state =
                confirmed_parent_state(parent_checks.get(&worker.key()).copied().flatten(), found);
        }
    }
    let now = unix_seconds()?;

    let mut semantic_decisions = observations
        .iter()
        .map(|worker| {
            let evidence = worker_watchdog::WorkerEvidence {
                turn_id: worker.turn_id.clone(),
                hash: worker.semantic_hash,
                trace_mtime: worker.trace_mtime,
                progress_at: worker.progress_at,
                trace: worker.trace.clone(),
                out: worker.out.clone(),
                pid: worker.pid,
                pid_alive: worker.pid_alive,
                pid_identity_ok: worker.pid_identity_ok,
                tool_alive: worker.tool_alive,
                outstanding_op: worker.outstanding_op.clone(),
                finished: worker.finished || worker.receipt_status.is_some(),
                exit_code: None,
                receipt_status: worker.receipt_status.clone(),
                gate_verdict: worker.gate_verdict.clone(),
                blocked_reason: worker.blocked_reason.clone(),
                state: worker.state.clone(),
                parent_host: worker.parent_host.clone(),
                parent_session: Some(worker.parent_session.clone()),
                parent_terminal: worker.parent_terminal.clone(),
                parent_pane: worker.parent_pane.clone(),
            };
            let key = worker.key();
            let entry = memory.semantic.workers.entry(key).or_default();
            let (class, age_secs, evidence) = worker_watchdog::classify_worker(
                &evidence,
                entry,
                now,
                options.stall_secs,
                options.stall_secs,
                options.op_deadline_secs,
                evidence::scheduled_retry_secs(&worker.trace),
            );
            SemanticDecision {
                worker_id: worker.worker_id.clone(),
                source: worker.source.clone(),
                turn_id: worker.turn_id.clone(),
                class,
                parent: decision_parent_state(
                    worker.parent_state,
                    parent_checks.get(&worker.key()).copied().flatten(),
                ),
                parent_detail: format!("host={:?} session={} terminal={:?} pane={:?}", worker.parent_host, worker.parent_session, worker.parent_terminal, worker.parent_pane),
                age_secs,
                evidence,
                samples: vec![serde_json::json!({"trace_mtime": worker.trace_mtime, "semantic_hash": worker.semantic_hash, "pid_alive": worker.pid_alive, "pid_identity_ok": worker.pid_identity_ok, "tool_alive": worker.tool_alive})],
                incident: worker_watchdog::WorkerIncidentDecision { key: None, action: worker_watchdog::IncidentAction::None, delivery: worker_watchdog::IncidentDelivery::None, error: None },
            }
        })
        .collect::<Vec<_>>();
    let confirm = semantic_decisions
        .iter()
        .enumerate()
        .filter(|(_, decision)| {
            decision.class == worker_watchdog::WorkerClass::SuspectedStall
                && decision.evidence.starts_with("no semantic progress")
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if !confirm.is_empty() {
        thread::sleep(Duration::from_secs(options.confirm_secs));
        let second = discover_workers(
            &options.runs_dir,
            &options.claude_projects_dir,
            options.history_days,
            options.op_deadline_secs,
            options.stall_secs,
        )?;
        let second_by_key = second
            .workers
            .iter()
            .map(|worker| (worker.key(), worker))
            .collect::<std::collections::HashMap<_, _>>();
        for index in confirm {
            let old = &observations[index];
            let Some(new) = second_by_key.get(&old.key()) else {
                continue;
            };
            if new.semantic_hash != old.semantic_hash {
                if let Some(entry) = memory.semantic.workers.get_mut(&old.key()) {
                    entry.hash = new.semantic_hash;
                    entry.since = now;
                }
                semantic_decisions[index].class = worker_watchdog::WorkerClass::Working;
                semantic_decisions[index].age_secs = 0;
                semantic_decisions[index].evidence =
                    "semantic progress changed during confirmation sample".into();
            }
            semantic_decisions[index].samples.push(serde_json::json!({"semantic_hash": new.semantic_hash, "trace_mtime": new.trace_mtime, "pid_alive": new.pid_alive, "pid_identity_ok": new.pid_identity_ok, "tool_alive": new.tool_alive}));
        }
    }
    let classes = semantic_decisions
        .iter()
        .map(|decision| decision.class)
        .collect::<Vec<_>>();
    let incidents = worker_watchdog::process_semantic_incidents(
        &observations,
        &classes,
        &mut memory.semantic,
        now,
        options.dry_run,
        |worker, class, age| append_incident_log(&options.log_file, worker, class, age),
        |worker, orphaned, age| {
            if orphaned {
                notify_operator(worker, age)
            } else {
                notify_parent(worker, age, &parents)
            }
        },
    );
    for (decision, incident) in semantic_decisions.iter_mut().zip(incidents) {
        decision.incident = incident;
    }
    save_memory(&options.state_file, &memory)?;
    print_worker_scan(
        observations.len(),
        skipped_old,
        scan_started.elapsed().as_millis(),
        &semantic_decisions,
        options,
    )?;
    Ok(
        if semantic_decisions
            .iter()
            .any(|decision| decision.incident.error.is_some())
        {
            1
        } else {
            0
        },
    )
}

#[cfg(unix)]
fn process_table() -> io::Result<Vec<ProcSample>> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,etime=,time=,comm="])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("ps exited unsuccessfully"));
    }
    Ok(evidence::parse_ps_rows(&String::from_utf8_lossy(
        &output.stdout,
    )))
}
#[cfg(windows)]
fn process_table() -> io::Result<Vec<ProcSample>> {
    Ok(Vec::new())
}

fn parent_panes(panes: &[PaneEntry]) -> std::collections::HashMap<String, String> {
    let mut parents = std::collections::HashMap::new();
    for pane in panes {
        parents.insert(pane.pane_id.clone(), pane.pane_id.clone());
        if let Some(session) = &pane.agent_session {
            parents.insert(session.value.clone(), pane.pane_id.clone());
        }
    }
    parents
}

fn parse_pane_entries(value: &Value) -> io::Result<Vec<PaneEntry>> {
    serde_json::from_value(value["result"]["panes"].clone()).map_err(io::Error::other)
}

fn parent_matches(worker: &WorkerObservation, panes: &[PaneEntry]) -> Option<bool> {
    let session = &worker.parent_session;
    if panes.iter().any(|pane| {
        pane.agent_session
            .as_ref()
            .is_some_and(|s| &s.value == session)
    }) {
        return Some(true);
    }
    if worker.parent_terminal.is_none()
        && worker.parent_pane.is_none()
        && panes
            .iter()
            .any(|pane| &pane.pane_id == session && pane.agent.is_some())
    {
        return Some(true);
    }
    if let Some(terminal) = &worker.parent_terminal {
        if panes
            .iter()
            .any(|pane| pane.terminal_id.as_ref() == Some(terminal))
        {
            return Some(true);
        }
    }
    if let Some(pane_id) = &worker.parent_pane {
        return Some(
            panes
                .iter()
                .any(|pane| &pane.pane_id == pane_id && pane.agent.is_some()),
        );
    }
    Some(false)
}

fn confirmed_parent_state(first: Option<bool>, second: Option<bool>) -> ParentState {
    match (first, second) {
        (Some(true), _) | (_, Some(true)) => ParentState::Present,
        (Some(false), Some(false)) => ParentState::Absent,
        _ => ParentState::Unknown,
    }
}

fn local_host_aliases(extra: &[String]) -> std::collections::HashSet<String> {
    let hostname = std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let mut aliases = extra
        .iter()
        .map(|host| host.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    aliases.insert(hostname.clone());
    if matches!(hostname.as_str(), "ubuntu-direct" | "ub1") {
        aliases.insert("ubuntu-direct".into());
        aliases.insert("ub1".into());
    }
    if matches!(hostname.as_str(), "ubuntu2-direct" | "ub2") {
        aliases.insert("ubuntu2-direct".into());
        aliases.insert("ub2".into());
    }
    if hostname.contains("macbook") && hostname.contains("air") {
        aliases.insert("air".into());
    }
    if hostname.contains("macbook") && hostname.contains("pro") {
        aliases.insert("mac".into());
    }
    aliases
}

fn remote_parent_present(
    worker: &WorkerObservation,
    options: &WorkerOptions,
) -> io::Result<Option<bool>> {
    let Some(host) = worker.parent_host.as_deref() else {
        return Ok(None);
    };
    let mut parts = options.parent_probe.split_whitespace();
    let Some(program) = parts.next() else {
        return Ok(None);
    };
    let mut command = std::process::Command::new(program);
    command
        .args(parts)
        .arg(host)
        .args(["herdr", "pane", "list"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Ok(None);
            }
            let output = child.wait_with_output()?;
            let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
                return Ok(None);
            };
            let Ok(panes) = parse_pane_entries(&value) else {
                return Ok(None);
            };
            return Ok(parent_matches(worker, &panes));
        }
        if started.elapsed() >= Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn notify_parent(
    worker: &WorkerObservation,
    age_secs: u64,
    parents: &std::collections::HashMap<String, String>,
) -> io::Result<()> {
    let pane_id = parents.get(&worker.parent_session);
    if pane_id.is_none() && worker.parent_state != ParentState::Absent {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("parent session {} is not connected", worker.parent_session),
        ));
    }
    let text = format!(
        "Worker {} on {}: no semantic progress for {} minutes; parent session {}.",
        worker.worker_id,
        worker.source,
        age_secs / 60,
        worker.parent_session
    );
    let response = super::super::send_request(&Request {
        id: super::next_request_id("worker-stall-notification"),
        method: Method::NotificationShow(NotificationShowParams {
            title: if worker.parent_state == ParentState::Absent {
                "Orphaned worker".into()
            } else {
                "Worker watchdog".into()
            },
            body: Some(match pane_id {
                Some(pane_id) => format!("Parent {pane_id}: {text}"),
                None => format!("Operator queue: {text}"),
            }),
            position: None,
            sound: NotificationShowSound::None,
        }),
    })?;
    super::ensure_api_success(&response)
}

fn notify_operator(worker: &WorkerObservation, age_secs: u64) -> io::Result<()> {
    let response = super::super::send_request(&Request {
        id: super::next_request_id("worker-watchdog-orphan-notification"),
        method: Method::NotificationShow(NotificationShowParams {
            title: "Orphaned worker".into(),
            body: Some(format!(
                "Worker {} on {} has no parent and no semantic progress for {} minutes.",
                worker.worker_id,
                worker.source,
                age_secs / 60
            )),
            position: None,
            sound: NotificationShowSound::None,
        }),
    })?;
    super::ensure_api_success(&response)
}

fn append_incident_log(
    path: &Path,
    worker: &WorkerObservation,
    class: &str,
    age_secs: u64,
) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let record = serde_json::json!({
        "timestamp": unix_seconds()?,
        "source": worker.source,
        "worker_id": worker.worker_id,
        "turn_id": worker.turn_id,
        "class": class,
        "parent": worker.parent_state,
        "age_secs": age_secs,
    });
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, &record).map_err(io::Error::other)?;
    file.write_all(b"\n")
}

fn print_worker_scan(
    workers: usize,
    skipped_old: usize,
    scan_ms: u128,
    semantic_decisions: &[SemanticDecision],
    options: &WorkerOptions,
) -> io::Result<()> {
    let visible = visible_decisions(semantic_decisions, options.all);
    let mut by_source = SourceClassCounts::new();
    for decision in semantic_decisions {
        *by_source
            .entry(decision.source.clone())
            .or_default()
            .entry(worker_class_name(decision.class).into())
            .or_default() += 1;
    }
    let summary = WorkerSummary {
        scanned: workers,
        skipped_old,
        scan_ms,
        by_source,
        dry_run: options.dry_run,
        notified: semantic_decisions
            .iter()
            .filter(|d| d.incident.delivery == worker_watchdog::IncidentDelivery::Delivered)
            .count(),
        logged: semantic_decisions
            .iter()
            .filter(|d| d.incident.action == worker_watchdog::IncidentAction::Logged)
            .count(),
    };
    if options.json {
        let output = serde_json::json!({
            "decisions": visible,
            "summary": summary,
        });
        println!(
            "{}",
            serde_json::to_string(&output).map_err(io::Error::other)?
        );
        return Ok(());
    }

    for decision in visible {
        println!(
            "worker {} class {:?} parent {} age {}s evidence={}: {}{}",
            decision.worker_id,
            decision.class,
            decision.parent,
            decision.age_secs,
            decision.evidence,
            serde_json::to_string(&decision.incident).unwrap_or_default(),
            decision
                .incident
                .error
                .as_ref()
                .map(|error| format!(" ({error})"))
                .unwrap_or_default()
        );
    }
    println!(
        "worker watchdog summary: scanned={} skipped_old={} scan_ms={} non_finished={} stalled={}{}",
        workers,
        skipped_old,
        scan_ms,
        semantic_decisions.iter().filter(|decision| decision.class != worker_watchdog::WorkerClass::Finished).count(),
        semantic_decisions
            .iter()
            .filter(|decision| decision.class == worker_watchdog::WorkerClass::SuspectedStall)
            .count(),
        if options.dry_run { " dry-run" } else { "" }
    );
    Ok(())
}

struct WorkerDiscovery {
    workers: Vec<WorkerObservation>,
    skipped_old: usize,
}

fn discover_workers(
    runs_dir: &Path,
    claude_projects_dir: &Path,
    history_days: u64,
    op_deadline_secs: u64,
    stall_secs: u64,
) -> io::Result<WorkerDiscovery> {
    let cutoff = unix_seconds()?.saturating_sub(history_days.saturating_mul(86_400));
    let mut skipped_old = 0;
    let mut workers = discover_codex_runs(runs_dir, cutoff, &mut skipped_old)?;
    let process_args = process_args_table()?;
    workers.extend(discover_claude_subagents(
        claude_projects_dir,
        cutoff,
        op_deadline_secs,
        stall_secs,
        &process_args,
        &mut skipped_old,
    )?);
    Ok(WorkerDiscovery {
        workers,
        skipped_old,
    })
}

fn discover_codex_runs(
    runs_dir: &Path,
    cutoff: u64,
    skipped_old: &mut usize,
) -> io::Result<Vec<WorkerObservation>> {
    let mut workers = Vec::new();
    let process_samples = process_table()?;
    let now = unix_seconds()?;
    let entries = match fs::read_dir(runs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(workers),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_dir = entry.path();
        let newest = newest_mtime(&run_dir, MtimeScope::Run)?;
        if newest.is_some_and(|mtime| mtime < cutoff) {
            *skipped_old += 1;
            continue;
        }
        let state_path = run_dir.join("state.json");
        let state: Value = match fs::read(&state_path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(path = %state_path.display(), %error, "skipping malformed worker state");
                    continue;
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let parent_session = json_string(&state, &["parent_session"])
            .or_else(|| json_string(&state, &["parent", "session"]))
            .or_else(|| json_string(&state, &["parent", "pane"]))
            .or_else(|| json_string(&state, &["parent", "terminal"]))
            .or_else(|| json_string(&state, &["parent"]))
            .or_else(|| launch_parent_session(&run_dir));
        let Some(parent_session) = parent_session else {
            continue;
        };
        let worker_id = entry.file_name().to_string_lossy().into_owned();
        let last_activity = codex_last_activity(&run_dir, &state)?;
        let (turn_id, turn_dir) =
            current_turn(&run_dir)?.map_or((None, run_dir.clone()), |(id, path)| (Some(id), path));
        let trace = read_tail(&turn_dir.join("trace.log"), 16 * 1024)?;
        let out = read_tail(&run_dir.join("out.log"), 16 * 1024)?;
        let trace_mtime =
            modified_unix_seconds(&turn_dir.join("trace.log"))?.unwrap_or(last_activity);
        let pid = state
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok());
        let pid_sample =
            pid.and_then(|pid| process_samples.iter().find(|sample| sample.pid == pid));
        let started_at = state.get("started_at").and_then(json_timestamp);
        let pid_identity_ok = pid_sample.zip(started_at).is_some_and(|(sample, started)| {
            sample.elapsed_secs.abs_diff(now.saturating_sub(started)) <= 120
        });
        let descendants = pid
            .map(|pid| evidence::descendants(&process_samples, pid))
            .unwrap_or_default();
        let tool_alive = !evidence::current_tool_processes(
            &descendants,
            pid,
            now.saturating_sub(started_at.unwrap_or(last_activity)),
        )
        .is_empty();
        let outstanding_op = worker_watchdog::outstanding_codex_operation(&trace);
        let receipt: Option<Value> = fs::read(turn_dir.join("receipt.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let state_name = json_string(&state, &["state"]).unwrap_or_default();
        let worker_host = json_string(&state, &["host"]);
        let parent_host = json_string(&state, &["parent", "host"]);
        let parent_scope_local = parent_host
            .as_deref()
            .is_none_or(|parent_host| worker_host.as_deref() == Some(parent_host));
        let finished = matches!(
            state_name.to_ascii_lowercase().as_str(),
            "complete"
                | "completed"
                | "done"
                | "failed"
                | "error"
                | "cancelled"
                | "stopped"
                | "success"
        ) || state.get("exit_code").is_some_and(|value| !value.is_null())
            || state
                .get("finished_at")
                .is_some_and(|value| !value.is_null());
        workers.push(WorkerObservation {
            source: "codex".into(),
            worker_id,
            parent_session,
            parent_host: json_string(&state, &["parent", "host"]),
            parent_terminal: json_string(&state, &["parent", "terminal"]),
            parent_pane: json_string(&state, &["parent", "pane"]),
            last_activity,
            finished,
            parent_scope_local,
            parent_state: ParentState::Unknown,
            turn_id,
            semantic_hash: evidence::semantic_hash(&format!("{trace}\n{out}")),
            trace_mtime,
            trace,
            out,
            pid,
            pid_alive: pid_sample.is_some(),
            pid_identity_ok,
            tool_alive,
            outstanding_op,
            progress_at: state.get("progress_at").and_then(json_timestamp),
            state: state_name,
            blocked_reason: json_string(&state, &["blocked_reason"]),
            receipt_status: receipt
                .as_ref()
                .and_then(|value| json_string(value, &["status"])),
            gate_verdict: json_string(&state, &["gate_verdict"]),
        });
    }
    Ok(workers)
}

fn launch_parent_session(run_dir: &Path) -> Option<String> {
    let metadata = fs::read_to_string(run_dir.join("launch-metadata")).ok()?;
    if let Ok(value) = serde_json::from_str::<Value>(&metadata) {
        if let Some(parent) = json_string(&value, &["parent_session"])
            .or_else(|| json_string(&value, &["parent", "session"]))
            .or_else(|| json_string(&value, &["parent", "pane"]))
        {
            return Some(parent);
        }
    }
    metadata.lines().find_map(|line| {
        line.trim()
            .strip_prefix("parent_session=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn codex_last_activity(run_dir: &Path, state: &Value) -> io::Result<u64> {
    // Heartbeat and atomic state rewrites prove liveness, not semantic progress.
    let mut last_activity = state
        .get("progress_at")
        .and_then(json_timestamp)
        .unwrap_or_default();
    for name in ["trace.log", "out.log"] {
        if let Some(modified) = modified_unix_seconds(&run_dir.join(name))? {
            last_activity = last_activity.max(modified);
        }
    }
    if let Some((_, turn_dir)) = current_turn(run_dir)? {
        for name in ["trace.log", "out.log"] {
            if let Some(modified) = modified_unix_seconds(&turn_dir.join(name))? {
                last_activity = last_activity.max(modified);
            }
        }
    }
    Ok(last_activity)
}

fn current_turn(run_dir: &Path) -> io::Result<Option<(String, PathBuf)>> {
    let turns_dir = run_dir.join("turns");
    let turns = match fs::read_dir(turns_dir) {
        Ok(turns) => turns,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut newest: Option<(u64, String, PathBuf)> = None;
    for entry in turns {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let started = fs::read(entry.path().join("start.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|v| v.get("started_at").and_then(json_timestamp))
            .or(modified_unix_seconds(&entry.path())?)
            .unwrap_or(0);
        let id = entry.file_name().to_string_lossy().into_owned();
        if newest.as_ref().is_none_or(|(time, _, _)| started > *time) {
            newest = Some((started, id, entry.path()));
        }
    }
    Ok(newest.map(|(_, id, path)| (id, path)))
}

fn discover_claude_subagents(
    projects_dir: &Path,
    cutoff: u64,
    op_deadline_secs: u64,
    stall_secs: u64,
    process_args: &[(u32, String)],
    skipped_old: &mut usize,
) -> io::Result<Vec<WorkerObservation>> {
    let mut workers = Vec::new();
    let projects = match fs::read_dir(projects_dir) {
        Ok(projects) => projects,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(workers),
        Err(error) => return Err(error),
    };
    for project in projects {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        if newest_mtime(&project.path(), MtimeScope::Project)?.is_some_and(|mtime| mtime < cutoff) {
            *skipped_old += 1;
            continue;
        }
        for session in fs::read_dir(project.path())? {
            let session = session?;
            if !session.file_type()?.is_dir() {
                continue;
            }
            let Some(parent_session) = session.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let parent_transcript = project.path().join(format!("{parent_session}.jsonl"));
            let parent_mtime = modified_unix_seconds(&parent_transcript)?.unwrap_or_default();
            let session_mtime = newest_mtime(&session.path(), MtimeScope::Session)?
                .unwrap_or_default()
                .max(parent_mtime);
            if session_mtime < cutoff {
                *skipped_old += 1;
                continue;
            }
            let subagents_dir = session.path().join("subagents");
            let subagents = match fs::read_dir(subagents_dir) {
                Ok(subagents) => subagents,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for subagent in subagents {
                let subagent = subagent?;
                if !subagent.file_type()?.is_file()
                    || subagent.path().extension().is_none_or(|ext| ext != "jsonl")
                {
                    continue;
                }
                let worker_id = subagent
                    .path()
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                let Some(last_activity) = modified_unix_seconds(&subagent.path())? else {
                    continue;
                };
                let trace = read_tail(&subagent.path(), 16 * 1024)?;
                let finish_reason = claude_subagent_finished(&subagent.path())?;
                let final_turn = finish_reason.is_some();
                let pending_tool = evidence::claude_pending_tool(&trace);
                let now = unix_seconds()?;
                let own_recent = last_activity >= now.saturating_sub(op_deadline_secs);
                let parent_live = parent_mtime >= now.saturating_sub(stall_secs)
                    || claude_parent_process_live(process_args, &parent_session);
                let idle_secs = unix_seconds()?.saturating_sub(last_activity);
                let inactive = claude_subagent_inactive(final_turn, own_recent, parent_live);
                workers.push(WorkerObservation {
                    source: "claude".into(),
                    worker_id,
                    parent_session: parent_session.clone(),
                    parent_host: None,
                    parent_terminal: None,
                    parent_pane: None,
                    last_activity,
                    finished: final_turn || inactive,
                    parent_scope_local: true,
                    parent_state: ParentState::Unknown,
                    turn_id: None,
                    semantic_hash: evidence::semantic_hash(&trace),
                    trace_mtime: last_activity,
                    trace: if inactive {
                        format!(
                            "inactive: transcript idle {}d, parent session not live",
                            idle_secs / 86_400
                        )
                    } else if finish_reason == Some("interrupted by user") {
                        format!("interrupted by user: {trace}")
                    } else {
                        trace
                    },
                    out: String::new(),
                    pid: None,
                    pid_alive: true,
                    pid_identity_ok: true,
                    tool_alive: false,
                    outstanding_op: pending_tool,
                    progress_at: Some(last_activity),
                    state: if inactive {
                        String::from("finished")
                    } else {
                        String::from("active")
                    },
                    blocked_reason: None,
                    receipt_status: None,
                    gate_verdict: None,
                });
            }
        }
    }
    Ok(workers)
}

fn claude_subagent_inactive(final_turn: bool, own_recent: bool, parent_live: bool) -> bool {
    !final_turn && !own_recent && !parent_live
}

fn claude_parent_process_live(process_args: &[(u32, String)], session: &str) -> bool {
    process_args.iter().any(|(_, args)| {
        args.split_whitespace().any(|arg| {
            arg == session
                || arg.strip_prefix("--session-id=") == Some(session)
                || arg.strip_prefix("--resume=") == Some(session)
        })
    })
}

fn process_args_table() -> io::Result<Vec<(u32, String)>> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,args="])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("ps -A -o pid=,args= failed"));
        }
        Ok(evidence::parse_process_args(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }
    #[cfg(not(unix))]
    {
        Ok(Vec::new())
    }
}

#[derive(Clone, Copy)]
enum MtimeScope {
    Run,
    Turns,
    Turn,
    Project,
    Session,
    Subagents,
}

fn newest_mtime(path: &Path, scope: MtimeScope) -> io::Result<Option<u64>> {
    let mut newest = None;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            let child_scope = match scope {
                MtimeScope::Run if name == "turns" => Some(MtimeScope::Turns),
                MtimeScope::Turns => Some(MtimeScope::Turn),
                MtimeScope::Project => Some(MtimeScope::Session),
                MtimeScope::Session if name == "subagents" => Some(MtimeScope::Subagents),
                _ => None,
            };
            if let Some(child_scope) = child_scope {
                newest = newest_mtime(&entry.path(), child_scope)?
                    .into_iter()
                    .chain(newest)
                    .max();
            }
        } else {
            let relevant = match scope {
                MtimeScope::Run => matches!(
                    name.as_ref(),
                    "state.json" | "out.log" | "trace.log" | "launch-metadata"
                ),
                MtimeScope::Turn => matches!(
                    name.as_ref(),
                    "start.json" | "receipt.json" | "trace.log" | "out.log"
                ),
                MtimeScope::Project | MtimeScope::Subagents => entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "jsonl"),
                MtimeScope::Turns | MtimeScope::Session => false,
            };
            if relevant {
                let modified = match modified_unix_seconds(&entry.path()) {
                    Ok(modified) => modified,
                    Err(error) if error.kind() == io::ErrorKind::PermissionDenied => None,
                    Err(error) => return Err(error),
                };
                newest = modified.into_iter().chain(newest).max();
            }
        }
    }
    Ok(newest)
}

fn parent_state_label(state: ParentState) -> &'static str {
    match state {
        ParentState::Present => "present",
        ParentState::Absent => "orphaned",
        ParentState::Unknown => "parent_unknown",
    }
}

fn decision_parent_state(state: ParentState, first_check: Option<bool>) -> &'static str {
    if state == ParentState::Unknown && first_check == Some(false) {
        "absent_unconfirmed"
    } else {
        parent_state_label(state)
    }
}

fn visible_decisions(
    decisions: &[SemanticDecision],
    include_finished: bool,
) -> Vec<&SemanticDecision> {
    decisions
        .iter()
        .filter(|decision| {
            include_finished || decision.class != worker_watchdog::WorkerClass::Finished
        })
        .collect()
}

fn worker_class_name(class: worker_watchdog::WorkerClass) -> &'static str {
    match class {
        worker_watchdog::WorkerClass::Working => "working",
        worker_watchdog::WorkerClass::ToolWait => "tool_wait",
        worker_watchdog::WorkerClass::WaitingToolInput => "waiting_tool_input",
        worker_watchdog::WorkerClass::WaitingApproval => "waiting_approval",
        worker_watchdog::WorkerClass::WaitingRetry => "waiting_retry",
        worker_watchdog::WorkerClass::SuspectedStall => "suspected_stall",
        worker_watchdog::WorkerClass::Finished => "finished",
        worker_watchdog::WorkerClass::Dead => "dead",
    }
}

fn claude_subagent_finished(path: &Path) -> io::Result<Option<&'static str>> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    file.seek_read_tail(length)?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let tail = String::from_utf8_lossy(&tail);
    let Some(record) = tail
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| match record.get("type").and_then(Value::as_str) {
            Some("assistant" | "user") => record.get("message").is_some(),
            _ => false,
        })
    else {
        return Ok(None);
    };
    if record.get("type").and_then(Value::as_str) == Some("user") {
        let content = record
            .get("message")
            .and_then(|message| message.get("content"));
        let interrupted = content.and_then(Value::as_str).is_some_and(|s| {
            matches!(
                s,
                "[Request interrupted by user]" | "[Request interrupted by user for tool use]"
            )
        }) || content.and_then(Value::as_array).is_some_and(|parts| {
            parts.iter().any(|part| {
                part.get("text").and_then(Value::as_str).is_some_and(|s| {
                    matches!(
                        s,
                        "[Request interrupted by user]"
                            | "[Request interrupted by user for tool use]"
                    )
                })
            })
        });
        return Ok(interrupted.then_some("interrupted by user"));
    }
    let stop_reason = record
        .get("message")
        .and_then(|message| message.get("stop_reason"))
        .and_then(Value::as_str);
    let content = record
        .get("message")
        .and_then(|message| message.get("content"));
    let text_only = content.and_then(Value::as_array).is_some_and(|parts| {
        !parts.is_empty()
            && parts
                .iter()
                .all(|part| part.get("type").and_then(Value::as_str) == Some("text"))
    });
    let finished = text_only
        || (stop_reason == Some("end_turn") && evidence::claude_pending_tool(&tail).is_none());
    Ok(finished.then_some("assistant reply complete"))
}

trait ReadTail {
    fn seek_read_tail(&mut self, length: u64) -> io::Result<()>;
}

impl ReadTail for fs::File {
    fn seek_read_tail(&mut self, length: u64) -> io::Result<()> {
        use std::io::Seek;
        self.seek(std::io::SeekFrom::Start(length.saturating_sub(16 * 1024)))
            .map(|_| ())
    }
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    let mut nested = value;
    for key in keys {
        nested = nested.get(*key)?;
    }
    nested
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn json_timestamp(value: &Value) -> Option<u64> {
    if let Some(timestamp) = value.as_u64() {
        return Some(timestamp);
    }
    let raw = value.as_str()?;
    let timestamp =
        time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
            .ok()?
            .unix_timestamp();
    u64::try_from(timestamp).ok()
}

fn modified_unix_seconds(path: &Path) -> io::Result<Option<u64>> {
    match fs::metadata(path) {
        Ok(metadata) => metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|duration| Some(duration.as_secs()))
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_tail(path: &Path, max_bytes: u64) -> io::Result<String> {
    use std::io::Seek;
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    file.seek(std::io::SeekFrom::Start(length.saturating_sub(max_bytes)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn load_memory(path: &Path) -> io::Result<WorkerMemory> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(WorkerMemory::default()),
        Err(error) => Err(error),
    }
}

fn save_memory(path: &Path, memory: &WorkerMemory) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(memory).map_err(io::Error::other)?;
    bytes.push(b'\n');
    fs::write(path, bytes)
}

fn unix_seconds() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "herdr-worker-watchdog-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn codex_discovery_uses_parent_and_trace_activity() {
        let dir = TestDir::new();
        let run_dir = dir.path().join("ra-run-1");
        fs::create_dir_all(run_dir.join("turns/turn-1")).expect("create run files");
        fs::write(
            run_dir.join("state.json"),
            r#"{"parent_session":"parent-1","state":"active","exit_code":null}"#,
        )
        .expect("write run state");
        fs::write(run_dir.join("turns/turn-1/trace.log"), "tool call").expect("write trace");
        let mut skipped = 0;
        let workers = discover_codex_runs(dir.path(), 0, &mut skipped).expect("discover workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "parent-1");
        assert_eq!(workers[0].source, "codex");
        assert!(!workers[0].finished);
        assert!(workers[0].last_activity > 0);
    }

    #[test]
    fn codex_discovery_reads_nested_parent_and_rfc3339_progress() {
        let dir = TestDir::new();
        let run_dir = dir.path().join("ra-run-nested");
        fs::create_dir_all(&run_dir).expect("create run directory");
        fs::write(
            run_dir.join("state.json"),
            r#"{"parent":{"session":"parent-nested"},"state":"active","last_heartbeat":"2026-09-29T07:00:00Z","progress_at":"2026-09-29T06:45:00Z"}"#,
        )
        .expect("write run state");
        let mut skipped = 0;
        let workers = discover_codex_runs(dir.path(), 0, &mut skipped).expect("discover workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "parent-nested");
        assert_eq!(workers[0].last_activity, 1_790_664_300);
    }

    #[test]
    fn codex_discovery_ignores_non_directory_turn_entries() {
        let dir = TestDir::new();
        let run_dir = dir.path().join("ra-run-file-turn");
        fs::create_dir_all(run_dir.join("turns")).expect("create turns directory");
        fs::write(
            run_dir.join("state.json"),
            r#"{"parent_session":"parent-1","state":"active","exit_code":null}"#,
        )
        .expect("write run state");
        fs::write(run_dir.join("turns/receipt.json"), "{}").expect("write receipt");
        let mut skipped = 0;
        let workers = discover_codex_runs(dir.path(), 0, &mut skipped).expect("discover workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "parent-1");
    }

    #[test]
    fn current_turn_prefers_newest_rfc3339_start_and_skips_files() {
        let dir = TestDir::new();
        let turns = dir.path().join("turns");
        fs::create_dir_all(turns.join("old")).expect("old turn");
        fs::create_dir_all(turns.join("new")).expect("new turn");
        fs::write(turns.join("stray"), "not a directory").expect("stray file");
        fs::write(
            turns.join("old/start.json"),
            r#"{"started_at":"2026-09-28T00:00:00Z"}"#,
        )
        .expect("old start");
        fs::write(
            turns.join("new/start.json"),
            r#"{"started_at":"2026-09-29T00:00:00Z"}"#,
        )
        .expect("new start");
        assert_eq!(
            current_turn(dir.path())
                .expect("select turn")
                .map(|(id, _)| id)
                .as_deref(),
            Some("new")
        );
    }

    #[test]
    fn numeric_and_rfc3339_timestamps_are_accepted() {
        assert_eq!(json_timestamp(&Value::from(1234_u64)), Some(1234));
        assert_eq!(
            json_timestamp(&Value::from("2026-09-29T06:45:00Z")),
            Some(1_790_664_300)
        );
    }

    #[test]
    fn parent_absence_requires_two_authoritative_checks() {
        assert_eq!(
            confirmed_parent_state(Some(false), None),
            ParentState::Unknown
        );
        assert_eq!(
            confirmed_parent_state(Some(false), Some(false)),
            ParentState::Absent
        );
        assert_eq!(
            confirmed_parent_state(Some(false), Some(true)),
            ParentState::Present
        );
        assert_eq!(
            confirmed_parent_state(None, Some(false)),
            ParentState::Unknown
        );
    }

    #[test]
    fn local_parent_matches_legacy_pane_id_only_for_agent_panes() {
        let worker = WorkerObservation {
            source: "codex".into(),
            worker_id: "w".into(),
            parent_session: "p1".into(),
            parent_host: None,
            parent_terminal: None,
            parent_pane: None,
            last_activity: 1,
            finished: false,
            parent_scope_local: true,
            parent_state: ParentState::Unknown,
            turn_id: None,
            semantic_hash: 0,
            trace_mtime: 1,
            trace: String::new(),
            out: String::new(),
            pid: None,
            pid_alive: false,
            pid_identity_ok: false,
            tool_alive: false,
            outstanding_op: None,
            progress_at: None,
            state: String::new(),
            blocked_reason: None,
            receipt_status: None,
            gate_verdict: None,
        };
        assert_eq!(
            parent_matches(
                &worker,
                &[PaneEntry {
                    pane_id: "p1".into(),
                    agent: Some("codex".into()),
                    agent_session: None,
                    terminal_id: None
                }]
            ),
            Some(true)
        );
        assert_eq!(
            parent_matches(
                &worker,
                &[PaneEntry {
                    pane_id: "p1".into(),
                    agent: None,
                    agent_session: None,
                    terminal_id: None
                }]
            ),
            Some(false)
        );
    }

    #[test]
    fn claude_discovery_ignores_finished_subagent_transcripts() {
        let dir = TestDir::new();
        let transcript = dir.path().join("project/session-1/subagents/agent-1.jsonl");
        fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("create subagent directory");
        fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}\n",
        )
        .expect("write transcript");
        let mut skipped = 0;
        let workers = discover_claude_subagents(dir.path(), 0, 3600, 1800, &[], &mut skipped)
            .expect("discover Claude workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "session-1");
        assert!(workers[0].finished);
    }

    #[test]
    fn interrupted_subagent_transcripts_are_finished() {
        for message in [
            "[Request interrupted by user]",
            "[Request interrupted by user for tool use]",
        ] {
            let dir = TestDir::new();
            let path = dir.path().join("agent.jsonl");
            fs::write(
                &path,
                format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{message}\"}}}}\n"),
            )
            .expect("write transcript");
            assert_eq!(
                claude_subagent_finished(&path).expect("read transcript"),
                Some("interrupted by user")
            );
        }
    }

    #[test]
    fn assistant_text_before_attachment_is_finished_without_stop_reason() {
        let dir = TestDir::new();
        let path = dir.path().join("agent.jsonl");
        fs::write(&path, concat!(
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":null,\"content\":[{\"type\":\"text\",\"text\":\"{\\\"ok\\\":true}\"}]}}\n",
            "{\"type\":\"attachment\",\"data\":{\"type\":\"total_tokens_reminder\"}}\n",
            "{\"type\":\"system\",\"message\":{\"content\":\"metadata\"}}\n",
            "{\"type\":\"summary\",\"summary\":\"brief\"}\n",
            "{\"type\":\"progress\",\"data\":{}}\n",
            "{\"type\":\"result\"}\n"
        )).expect("write transcript");
        assert_eq!(
            claude_subagent_finished(&path).expect("read transcript"),
            Some("assistant reply complete")
        );
    }

    #[test]
    fn worker_summary_keeps_notification_and_logging_counts() {
        let summary = WorkerSummary {
            scanned: 1,
            skipped_old: 0,
            scan_ms: 1,
            by_source: SourceClassCounts::new(),
            dry_run: true,
            notified: 0,
            logged: 0,
        };
        let value = serde_json::to_value(summary).expect("serialize summary");
        assert_eq!(value["notified"], 0);
        assert_eq!(value["logged"], 0);
        assert_eq!(value["dry_run"], true);
    }

    #[test]
    fn worker_options_support_the_separate_dry_run_and_stall_threshold() {
        let options = parse_worker_options(&[
            "--once".into(),
            "--dry-run".into(),
            "--stall-minutes".into(),
            "7".into(),
        ])
        .expect("parse worker options");
        assert!(options.once);
        assert!(options.dry_run);
        assert_eq!(options.interval_secs, 30);
        assert_eq!(options.stall_secs, 420);
        assert_eq!(options.confirm_secs, 20);
        assert_eq!(options.op_deadline_secs, 3600);
        assert_eq!(options.history_days, 7);
        assert!(!options.all);
        let options = parse_worker_options(&["--history-days".into(), "14".into(), "--all".into()])
            .expect("parse output controls");
        assert_eq!(options.history_days, 14);
        assert!(options.all);
        let options = parse_worker_options(&[
            "--confirm-secs".into(),
            "5".into(),
            "--op-deadline-minutes".into(),
            "9".into(),
            "--local-host".into(),
            "ub1".into(),
            "--local-host".into(),
            "air".into(),
            "--parent-probe".into(),
            "ssh -F cfg".into(),
        ])
        .expect("parse worker controls");
        assert_eq!(options.confirm_secs, 5);
        assert_eq!(options.op_deadline_secs, 540);
        assert_eq!(options.local_hosts, ["ub1", "air"]);
        assert_eq!(options.parent_probe, "ssh -F cfg");
    }

    #[test]
    fn history_cutoff_skips_old_sessions_before_reading_subagents() {
        let dir = TestDir::new();
        let session = dir.path().join("project/session-old");
        fs::create_dir_all(session.join("subagents")).expect("session directory");
        fs::write(session.join("session.jsonl"), "parent").expect("parent transcript");
        fs::write(session.join("subagents/agent-old.jsonl"), "not json").expect("subagent");
        let mut skipped = 0;
        let result = discover_claude_subagents(dir.path(), u64::MAX, 3600, 1800, &[], &mut skipped)
            .expect("discover Claude workers");
        assert!(result.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn history_cutoff_skips_old_ra_runs_before_reading_state() {
        let dir = TestDir::new();
        let run = dir.path().join("ra-run-old");
        fs::create_dir_all(&run).expect("run directory");
        fs::write(run.join("state.json"), "malformed on purpose").expect("state file");
        let mut skipped = 0;
        let workers = discover_codex_runs(dir.path(), u64::MAX, &mut skipped)
            .expect("skip old run before parsing");
        assert!(workers.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn run_history_ignores_unrelated_nested_files() {
        let dir = TestDir::new();
        let run = dir.path().join("ra-run-old");
        fs::create_dir_all(run.join("skill-state/tmp")).expect("nested unrelated directory");
        let state = run.join("state.json");
        fs::write(&state, "{}").expect("run state");
        fs::File::open(&state)
            .expect("open state")
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(100)))
            .expect("age state");
        fs::write(
            run.join("skill-state/tmp/recent.jsonl"),
            "not a worker transcript",
        )
        .expect("unrelated file");
        assert_eq!(
            newest_mtime(&run, MtimeScope::Run).expect("mtime"),
            Some(100)
        );
    }

    #[test]
    fn claude_subagent_liveness_requires_parent_or_recent_own_transcript() {
        assert!(claude_subagent_inactive(false, false, false));
        assert!(!claude_subagent_inactive(false, false, true));
        assert!(!claude_subagent_inactive(false, true, false));
        assert!(!claude_subagent_inactive(true, false, false));
    }

    #[test]
    fn claude_discovery_classifies_old_and_live_parent_transcripts() {
        let dir = TestDir::new();
        let transcript = dir.path().join("project/session-1/subagents/agent-1.jsonl");
        fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("create subagent directory");
        fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Bash\"}]}}\n",
        )
        .expect("write pending tool");
        let old = SystemTime::now() - Duration::from_secs(2 * 3600);
        fs::File::open(&transcript)
            .expect("open transcript")
            .set_times(fs::FileTimes::new().set_modified(old))
            .expect("age transcript");

        let discover = |processes: &[(u32, String)]| {
            let mut skipped = 0;
            discover_claude_subagents(dir.path(), 0, 3600, 1800, processes, &mut skipped)
                .expect("discover subagent")
                .remove(0)
        };
        let inactive = discover(&[]);
        assert!(inactive.finished);
        assert!(inactive
            .trace
            .starts_with("inactive: transcript idle 0d, parent session not live"));

        let live = discover(&[(123, "claude --session-id session-1".into())]);
        assert!(!live.finished);
        assert_eq!(live.outstanding_op.as_deref(), Some("Bash"));
        let evidence = worker_watchdog::WorkerEvidence {
            turn_id: None,
            hash: live.semantic_hash,
            trace_mtime: live.trace_mtime,
            progress_at: live.progress_at,
            trace: live.trace,
            out: live.out,
            pid: None,
            pid_alive: true,
            pid_identity_ok: true,
            tool_alive: false,
            outstanding_op: live.outstanding_op,
            finished: live.finished,
            exit_code: None,
            receipt_status: None,
            gate_verdict: None,
            blocked_reason: None,
            state: live.state,
            parent_host: None,
            parent_session: Some("session-1".into()),
            parent_terminal: None,
            parent_pane: None,
        };
        assert_eq!(
            worker_watchdog::classify_worker(
                &evidence,
                &mut worker_watchdog::SemanticWorkerEntry::default(),
                unix_seconds().expect("clock"),
                1800,
                1800,
                3600,
                None,
            )
            .0,
            worker_watchdog::WorkerClass::SuspectedStall
        );
    }

    #[test]
    fn claude_process_session_match_requires_a_whole_argument() {
        let processes = [(123, "claude --session-id other-session-1".into())];
        assert!(!claude_parent_process_live(&processes, "session-1"));
        assert!(claude_parent_process_live(
            &[(123, "claude --session-id=session-1".into())],
            "session-1"
        ));
    }

    #[test]
    fn default_output_filters_finished_and_all_restores_it() {
        let make = |class| SemanticDecision {
            worker_id: "agent".into(),
            source: "claude".into(),
            turn_id: None,
            class,
            parent: "parent_unknown",
            parent_detail: String::new(),
            age_secs: 0,
            evidence: String::new(),
            samples: Vec::new(),
            incident: worker_watchdog::WorkerIncidentDecision {
                key: None,
                action: worker_watchdog::IncidentAction::None,
                delivery: worker_watchdog::IncidentDelivery::None,
                error: None,
            },
        };
        let decisions = [
            make(worker_watchdog::WorkerClass::Finished),
            make(worker_watchdog::WorkerClass::Dead),
        ];
        assert_eq!(visible_decisions(&decisions, false).len(), 1);
        assert_eq!(visible_decisions(&decisions, true).len(), 2);
    }

    #[test]
    fn dead_run_retains_present_parent_in_its_decision() {
        let worker = WorkerObservation {
            source: "codex".into(),
            worker_id: "ra-run".into(),
            parent_session: "parent".into(),
            parent_host: None,
            parent_terminal: None,
            parent_pane: None,
            last_activity: 1,
            finished: false,
            parent_scope_local: true,
            parent_state: ParentState::Present,
            turn_id: None,
            semantic_hash: 0,
            trace_mtime: 1,
            trace: String::new(),
            out: String::new(),
            pid: Some(123),
            pid_alive: false,
            pid_identity_ok: false,
            tool_alive: false,
            outstanding_op: None,
            progress_at: None,
            state: "active".into(),
            blocked_reason: None,
            receipt_status: None,
            gate_verdict: None,
        };
        let evidence = worker_watchdog::WorkerEvidence {
            turn_id: None,
            hash: 0,
            trace_mtime: 1,
            progress_at: None,
            trace: String::new(),
            out: String::new(),
            pid: worker.pid,
            pid_alive: false,
            pid_identity_ok: false,
            tool_alive: false,
            outstanding_op: None,
            finished: false,
            exit_code: None,
            receipt_status: None,
            gate_verdict: None,
            blocked_reason: None,
            state: "active".into(),
            parent_host: None,
            parent_session: Some("parent".into()),
            parent_terminal: None,
            parent_pane: None,
        };
        let (class, _, _) = worker_watchdog::classify_worker(
            &evidence,
            &mut worker_watchdog::SemanticWorkerEntry::default(),
            100,
            10,
            10,
            10,
            None,
        );
        assert_eq!(class, worker_watchdog::WorkerClass::Dead);
        assert_eq!(parent_state_label(worker.parent_state), "present");
    }

    #[test]
    fn parent_decision_distinguishes_unconfirmed_absence() {
        assert_eq!(decision_parent_state(ParentState::Present, None), "present");
        assert_eq!(decision_parent_state(ParentState::Absent, None), "orphaned");
        assert_eq!(
            decision_parent_state(ParentState::Unknown, None),
            "parent_unknown"
        );
        assert_eq!(
            decision_parent_state(ParentState::Unknown, Some(false)),
            "absent_unconfirmed"
        );
    }
}
