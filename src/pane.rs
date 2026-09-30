use std::cell::Cell;
use std::io;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};

use bytes::Bytes;
use portable_pty::CommandBuilder;
#[cfg(all(test, unix))]
use portable_pty::{native_pty_system, PtySize};
use ratatui::{layout::Rect, Frame};
use sha2::{Digest, Sha256};
#[cfg(test)]
use tokio::sync::watch;
use tokio::sync::{mpsc, Notify};
#[cfg(not(windows))]
use tracing::debug;
use tracing::{error, info, warn};

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;
#[cfg(unix)]
use crate::pty::actor::{ControlledWriteResult, RemoteOwnerAcquireResult};
use crate::pty::actor::{PtyIoActor, PtyIoActorConfig, PtyIoActorHandle, PtyReadResult};
use crate::render_signal::RenderSignal;

#[cfg(test)]
std::thread_local! {
    static TEST_CWD_QUERY_COUNT: Cell<usize> = const { Cell::new(0) };
}

mod agent_detection;
mod cursor;
mod input;
mod kitty_keyboard;
mod osc;
mod state;
mod terminal;

#[cfg(unix)]
use self::agent_detection::DetectionPublishState;
pub(crate) use self::agent_detection::STABLE_VISIBLE_SIGNAL_REFRESH;
use self::agent_detection::{
    decide_detection_screen_read, decide_screen_detection_publish,
    detection_update_for_publish_with_osc, mark_detection_content_changed, observe_agent_output,
    observe_detection_content_change, recent_agent_output, DetectionPublishDecision,
    DetectionScreenReadDecision, DetectionScreenReadInput, FullLifecycleHookOutputRetirement,
    PendingIdleConfirmation, ScreenDetectionPublishInput, AGENT_PENDING_IDLE_RECHECK,
    AGENT_STARTUP_GRACE_WINDOW,
};
#[cfg(any(unix, test))]
pub use self::terminal::InputState;
use self::terminal::{GhosttyPaneTerminal, PaneTerminal};
pub(crate) use self::terminal::{
    TerminalDirtyPatch, TerminalDirtyPatchOutcome, TerminalReadSnapshot, TerminalTextMatch,
    TerminalTextPoint, TerminalWordMotion,
};
pub use self::{
    state::PaneState,
    terminal::{ScrollMetrics, TerminalCursorState},
};

const RELEASE_REACQUIRE_SUPPRESSION: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const PANE_TERM: &str = "xterm-256color";
const PANE_COLORTERM: &str = "truecolor";

#[cfg(test)]
thread_local! {
    static AGGREGATE_INPUT_STATE_READS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_aggregate_input_state_reads() {
    AGGREGATE_INPUT_STATE_READS.set(0);
}

#[cfg(test)]
pub(crate) fn aggregate_input_state_reads() -> usize {
    AGGREGATE_INPUT_STATE_READS.get()
}

fn apply_pane_terminal_env(cmd: &mut CommandBuilder) {
    // Each pane is rendered by herdr's own terminal layer, not the outer terminal
    // that launched the app. Advertising the inherited TERM leaks the host terminal
    // identity into shells and across SSH, which breaks redraw and cursor movement
    // when the remote side lacks matching terminfo entries.
    cmd.env("TERM", PANE_TERM);
    cmd.env("COLORTERM", PANE_COLORTERM);
    cmd.env_remove("WT_SESSION");
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PaneLaunchEnv {
    extra: Vec<(String, String)>,
    identity: PaneLaunchIdentity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum PaneLaunchIdentity {
    #[default]
    Inherit,
    Managed {
        workspace_id: String,
        tab_id: String,
        pane_id: String,
    },
    OmitPane,
}

impl PaneLaunchEnv {
    pub(crate) fn from_extra(extra: Vec<(String, String)>) -> Self {
        Self {
            extra,
            identity: PaneLaunchIdentity::Inherit,
        }
    }

    pub(crate) fn with_identity(
        mut self,
        workspace_id: String,
        tab_id: String,
        pane_id: String,
    ) -> Self {
        self.identity = PaneLaunchIdentity::Managed {
            workspace_id,
            tab_id,
            pane_id,
        };
        self
    }

    pub(crate) fn without_pane_identity(mut self) -> Self {
        self.identity = PaneLaunchIdentity::OmitPane;
        self
    }
}

fn apply_pane_launch_env(cmd: &mut CommandBuilder, launch_env: &PaneLaunchEnv) {
    cmd.env_remove("CODEX_THREAD_ID");
    // A herdr pane is always a top-level agent session; an inherited child-session
    // marker makes Claude Code >=2.1.226 suppress transcript persistence, which
    // starves the closing-block Stop hook and hides turn-end gates.
    cmd.env_remove("CLAUDE_CODE_CHILD_SESSION");
    for (key, value) in &launch_env.extra {
        cmd.env(key, value);
    }
    cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);
    crate::integration::apply_pane_base_env(cmd);
    crate::platform::apply_pane_runtime_marker(cmd);
    match &launch_env.identity {
        PaneLaunchIdentity::Inherit => {}
        PaneLaunchIdentity::Managed {
            workspace_id,
            tab_id,
            pane_id,
        } => {
            cmd.env(crate::integration::HERDR_WORKSPACE_ID_ENV_VAR, workspace_id);
            cmd.env(crate::integration::HERDR_TAB_ID_ENV_VAR, tab_id);
            cmd.env(crate::integration::HERDR_PANE_ID_ENV_VAR, pane_id);
        }
        PaneLaunchIdentity::OmitPane => {
            cmd.env_remove(crate::integration::HERDR_PANE_ID_ENV_VAR);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingAgentRelease {
    agent: Agent,
    until: std::time::Instant,
}

#[derive(Clone, Copy, Default)]
struct SpawnInitialState<'a> {
    detected_agent: Option<Agent>,
    history_ansi: Option<&'a str>,
    windows_powershell_prompt_cwd_reporting: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentDetection {
    Enabled,
    Disabled,
}

fn active_pending_release(
    pending_release: &Mutex<Option<PendingAgentRelease>>,
    now: std::time::Instant,
) -> Option<Agent> {
    let mut pending_release = pending_release.lock().ok()?;
    match *pending_release {
        Some(pending) if now < pending.until => Some(pending.agent),
        Some(_) => {
            *pending_release = None;
            None
        }
        None => None,
    }
}

async fn publish_state_changed_event(
    state_events: mpsc::Sender<AppEvent>,
    pane_id: PaneId,
    agent: Option<Agent>,
    state: AgentState,
    visible_blocker: bool,
    visible_working: bool,
    usage_limited: bool,
    process_exited: bool,
    observed_at: std::time::Instant,
) {
    // This runs on the async detector task, not the PTY reader thread.
    // Waiting for queue space here preserves correctness-critical state transitions
    // without blocking pane I/O.
    if let Err(e) = state_events
        .send(AppEvent::StateChanged {
            pane_id,
            agent,
            state,
            visible_blocker,
            visible_working,
            usage_limited,
            process_exited,
            observed_at,
        })
        .await
    {
        warn!(
            pane = pane_id.raw(),
            err = %e,
            "failed to deliver StateChanged event"
        );
    }
}

async fn publish_agent_process_detected_event(
    state_events: mpsc::Sender<AppEvent>,
    pane_id: PaneId,
    agent: Agent,
    observed_at: std::time::Instant,
) {
    if let Err(e) = state_events
        .send(AppEvent::AgentProcessDetected {
            pane_id,
            agent,
            observed_at,
        })
        .await
    {
        warn!(
            pane = pane_id.raw(),
            err = %e,
            "failed to deliver AgentProcessDetected event"
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct AgentDetectionPublishUpdate {
    state: AgentState,
    visible_idle: bool,
    visible_blocker: bool,
    visible_working: bool,
    usage_limited: bool,
    process_exited: bool,
}

async fn apply_agent_detection_publish_update(
    state_events: mpsc::Sender<AppEvent>,
    pane_id: PaneId,
    agent: Option<Agent>,
    update: AgentDetectionPublishUpdate,
    observed_at: std::time::Instant,
    state: &mut AgentState,
    last_visible_idle: &mut bool,
    last_visible_blocker: &mut bool,
    last_visible_working: &mut bool,
    last_visible_signal_refresh: &mut Option<std::time::Instant>,
    foreground_shell_exit_reported: &mut bool,
) {
    *state = update.state;
    *last_visible_idle = update.visible_idle;
    *last_visible_blocker = update.visible_blocker;
    *last_visible_working = update.visible_working;
    *last_visible_signal_refresh = if update.visible_blocker || update.visible_working {
        Some(observed_at)
    } else {
        None
    };
    if update.process_exited {
        *foreground_shell_exit_reported = true;
    }
    publish_state_changed_event(
        state_events,
        pane_id,
        agent,
        update.state,
        update.visible_blocker,
        update.visible_working,
        update.usage_limited,
        update.process_exited,
        observed_at,
    )
    .await;
}

const AGENT_MISS_CONFIRMATION_ATTEMPTS: u8 = 6;
const PROCESS_RECHECK_IDENTIFIED: std::time::Duration = std::time::Duration::from_secs(5);
const PROCESS_RECHECK_MISSING_FOREGROUND_GROUP: std::time::Duration =
    std::time::Duration::from_secs(30);
const PROCESS_ACQUISITION_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);
const PROCESS_ACQUISITION_FAST_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);
const PROCESS_ACQUISITION_FAST_RECHECK: std::time::Duration = std::time::Duration::from_millis(500);
const PROCESS_ACQUISITION_SLOW_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);
const PROCESS_ACQUISITION_IDLE_RESET: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
struct AgentDetectionPresence {
    current_agent: Option<Agent>,
    consecutive_misses: u8,
}

#[cfg(unix)]
fn absolute_process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    crate::platform::process_cwd(pid).filter(|cwd| cwd.is_absolute())
}

#[cfg(unix)]
fn usable_process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    absolute_process_cwd(pid).filter(|cwd| cwd.is_dir())
}

#[cfg(unix)]
fn foreground_member_cwd_different_from_shell(
    shell_pid: u32,
    shell_cwd: Option<&std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    let job = crate::detect::foreground_job(shell_pid)?;
    for process in job.processes {
        if process.pid == shell_pid {
            continue;
        }
        let Some(cwd) = absolute_process_cwd(process.pid) else {
            continue;
        };
        if shell_cwd != Some(&cwd) {
            return Some(cwd);
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForegroundShellAgentAction {
    ObserveProbe,
    ReportProcessExit,
    ReportReplacementProcess,
    ClearAgent,
}

fn foreground_shell_agent_action(
    previous_agent: Option<Agent>,
    new_agent: Option<Agent>,
    foreground_is_pane_shell: bool,
    process_exit_reported: bool,
) -> ForegroundShellAgentAction {
    let Some(previous_agent) = previous_agent else {
        return ForegroundShellAgentAction::ObserveProbe;
    };
    if process_exit_reported {
        return if new_agent == Some(previous_agent) {
            ForegroundShellAgentAction::ReportReplacementProcess
        } else if new_agent.is_none() {
            ForegroundShellAgentAction::ClearAgent
        } else {
            ForegroundShellAgentAction::ObserveProbe
        };
    }
    if new_agent.is_some() {
        return ForegroundShellAgentAction::ObserveProbe;
    }

    if foreground_is_pane_shell {
        // Do not clear identity immediately. First publish an idle process-exit
        // transition for the previous agent so notifications and wait-agent callers
        // observe completion before the pane becomes unknown.
        return ForegroundShellAgentAction::ReportProcessExit;
    }

    ForegroundShellAgentAction::ObserveProbe
}

/// Drops retained OSC evidence when changing away from an identified agent.
/// First acquisition keeps bytes that the newly identified process may have
/// emitted before the process probe recognized it.
fn clear_osc_evidence_for_agent_transition(terminal: &PaneTerminal, previous_agent: Option<Agent>) {
    if previous_agent.is_some() {
        terminal.clear_agent_osc_state();
    }
}

fn apply_foreground_shell_agent_action(
    agent_presence: &mut AgentDetectionPresence,
    action: ForegroundShellAgentAction,
    previous_agent: Option<Agent>,
    new_agent: Option<Agent>,
    pending_foreground_shell_clear: &mut bool,
    foreground_shell_exit_reported: &mut bool,
) -> bool {
    match action {
        ForegroundShellAgentAction::ReportReplacementProcess => {
            *pending_foreground_shell_clear = false;
            *foreground_shell_exit_reported = false;
            agent_presence.observe_process_probe(previous_agent);
            true
        }
        ForegroundShellAgentAction::ReportProcessExit => {
            *pending_foreground_shell_clear = true;
            false
        }
        ForegroundShellAgentAction::ClearAgent => {
            *pending_foreground_shell_clear = false;
            *foreground_shell_exit_reported = false;
            agent_presence.clear_current_agent()
        }
        ForegroundShellAgentAction::ObserveProbe => {
            *pending_foreground_shell_clear = false;
            *foreground_shell_exit_reported = false;
            agent_presence.observe_process_probe(new_agent)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ProcessProbeInput {
    current_agent: Option<Agent>,
    suppressed_agent: Option<Agent>,
    foreground_pgid: Option<u32>,
    last_foreground_pgid: Option<u32>,
    has_process_probe: bool,
    acquisition_age: Option<std::time::Duration>,
    pending_foreground_shell_clear: bool,
    pending_restore_probe: bool,
    elapsed_since_process_check: std::time::Duration,
    /// The supervisor hook has gone silent past its budget, so its authority is
    /// the one source we can no longer trust.
    supervisor_stale: bool,
}

fn foreground_group_changed(
    foreground_pgid: Option<u32>,
    last_foreground_pgid: Option<u32>,
) -> bool {
    foreground_pgid != last_foreground_pgid
        && (foreground_pgid.is_some() || last_foreground_pgid.is_some())
}

// Only kernel-observed foreground groups drive change detection. Remembering an
// inferred group would look like a change on every tick while the kernel stays silent.
fn process_group_for_change_tracking(
    observed_foreground_pgid: Option<u32>,
    probed_process_group_id: Option<u32>,
) -> Option<u32> {
    observed_foreground_pgid?;
    probed_process_group_id.or(observed_foreground_pgid)
}

#[cfg(any(windows, test))]
fn should_observe_foreground_process_group(
    lifecycle_authority: bool,
    content_changed: bool,
    elapsed: std::time::Duration,
    input: ProcessProbeInput,
) -> bool {
    !input.has_process_probe
        || input.current_agent.is_none()
        || input.suppressed_agent.is_some()
        || input.pending_foreground_shell_clear
        || input.pending_restore_probe
        || content_changed
        || (lifecycle_authority && elapsed >= PROCESS_RECHECK_IDENTIFIED)
        || (!lifecycle_authority && input.elapsed_since_process_check >= PROCESS_RECHECK_IDENTIFIED)
}

fn should_skip_process_probe_for_lifecycle_authority(
    full_lifecycle_authority_active: bool,
    input: ProcessProbeInput,
) -> bool {
    // A stale supervisor is precisely the case the probe exists for. Skipping it
    // here leaves `stale_resolution` empty, and an empty resolution keeps a
    // finished pane from ever reading quiet, so it never settles and the
    // stalled-agent nudge fires against an agent that is already done.
    full_lifecycle_authority_active
        && !input.supervisor_stale
        && input.foreground_pgid.is_some()
        && !input.pending_foreground_shell_clear
        && input.suppressed_agent.is_none()
        && input.has_process_probe
        && !foreground_group_changed(input.foreground_pgid, input.last_foreground_pgid)
}

fn should_skip_screen_detection_for_lifecycle_authority(
    _full_lifecycle_authority_active: bool,
    _full_lifecycle_hook_blocked: bool,
    _process_exited: bool,
) -> bool {
    false
}

fn should_probe_foreground_job(input: ProcessProbeInput) -> bool {
    if input.pending_foreground_shell_clear || input.pending_restore_probe {
        return true;
    }

    let foreground_group_changed =
        foreground_group_changed(input.foreground_pgid, input.last_foreground_pgid);

    if input.suppressed_agent.is_some() {
        return !input.has_process_probe || foreground_group_changed;
    }

    if let Some(acquisition_age) = input.acquisition_age {
        let acquisition_interval = if acquisition_age <= PROCESS_ACQUISITION_FAST_WINDOW {
            PROCESS_ACQUISITION_FAST_RECHECK
        } else {
            PROCESS_ACQUISITION_SLOW_RECHECK
        };
        if acquisition_age <= PROCESS_ACQUISITION_WINDOW
            && input.elapsed_since_process_check >= acquisition_interval
        {
            return true;
        }
    }

    if input.current_agent.is_none() {
        return !input.has_process_probe
            || foreground_group_changed
            || (input.foreground_pgid.is_none()
                && input.elapsed_since_process_check >= PROCESS_RECHECK_MISSING_FOREGROUND_GROUP);
    }

    foreground_group_changed || input.elapsed_since_process_check >= PROCESS_RECHECK_IDENTIFIED
}

fn sync_content_change_acquisition(
    current_agent: Option<Agent>,
    suppressed_agent: Option<Agent>,
    process_group_changed: bool,
    content_changed: bool,
    now: std::time::Instant,
    acquisition_started_at: &mut Option<std::time::Instant>,
    last_content_change_at: &mut Option<std::time::Instant>,
) {
    if current_agent.is_some() || suppressed_agent.is_some() || process_group_changed {
        return;
    }

    if content_changed {
        let should_start = acquisition_started_at.is_none_or(|started| {
            now.duration_since(started) > PROCESS_ACQUISITION_WINDOW
                && last_content_change_at.is_none_or(|last_change| {
                    now.duration_since(last_change) >= PROCESS_ACQUISITION_IDLE_RESET
                })
        });
        if should_start {
            *acquisition_started_at = Some(now);
        }
        *last_content_change_at = Some(now);
        return;
    }

    let Some(acquisition_started) = *acquisition_started_at else {
        return;
    };
    let Some(last_content_change) = *last_content_change_at else {
        return;
    };

    if now.duration_since(acquisition_started) > PROCESS_ACQUISITION_WINDOW
        && now.duration_since(last_content_change) >= PROCESS_ACQUISITION_IDLE_RESET
    {
        *acquisition_started_at = None;
        *last_content_change_at = None;
    }
}

#[derive(Debug, Clone)]
struct ProcessProbeResult {
    process_group_id: Option<u32>,
    foreground_is_pane_shell: bool,
    agent: Option<Agent>,
    agent_process_pid: Option<u32>,
    process_name: Option<String>,
}

fn agent_process_pid_in_job(job: &crate::platform::ForegroundJob, agent: Agent) -> Option<u32> {
    job.processes.iter().find_map(|process| {
        let single = crate::platform::ForegroundJob {
            process_group_id: process.pid,
            processes: vec![process.clone()],
        };
        (crate::detect::identify_agent_in_job(&single)
            .is_some_and(|(identified, _)| identified == agent))
        .then_some(process.pid)
    })
}

fn agent_process_pid_in_job_with_hint(
    job: &crate::platform::ForegroundJob,
    agent: Agent,
    read_hint: impl Fn(u32) -> Option<Agent>,
) -> Option<u32> {
    job.processes
        .iter()
        .find_map(|process| (read_hint(process.pid) == Some(agent)).then_some(process.pid))
        .or_else(|| agent_process_pid_in_job(job, agent))
}

fn agent_hint_for_foreground_job_members(
    job: &crate::platform::ForegroundJob,
    read_hint: impl Fn(u32) -> Option<Agent> + Copy,
) -> Option<Agent> {
    read_hint(job.process_group_id)
        .or_else(|| agent_hint_for_non_leader_foreground_job_members(job, read_hint))
}

fn agent_hint_for_non_leader_foreground_job_members(
    job: &crate::platform::ForegroundJob,
    read_hint: impl Fn(u32) -> Option<Agent> + Copy,
) -> Option<Agent> {
    job.processes
        .iter()
        .filter(|process| process.pid != job.process_group_id)
        .find_map(|process| read_hint(process.pid))
}

fn identify_process_group_leader_in_job(
    job: &crate::platform::ForegroundJob,
) -> Option<(Agent, String)> {
    let leader = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)?;
    let leader_job = crate::platform::ForegroundJob {
        process_group_id: job.process_group_id,
        processes: vec![leader.clone()],
    };
    crate::detect::identify_agent_in_job(&leader_job)
}

fn process_probe_result(
    job: &crate::platform::ForegroundJob,
    pid: u32,
    agent: Agent,
    process_name: String,
) -> ProcessProbeResult {
    ProcessProbeResult {
        process_group_id: Some(job.process_group_id),
        foreground_is_pane_shell: job.processes.iter().any(|process| process.pid == pid),
        agent: Some(agent),
        agent_process_pid: agent_process_pid_in_job(job, agent),
        process_name: Some(process_name),
    }
}

fn hinted_process_probe_result(
    job: &crate::platform::ForegroundJob,
    pid: u32,
    read_hint: impl Fn(u32) -> Option<Agent> + Copy,
) -> Option<ProcessProbeResult> {
    let agent = agent_hint_for_foreground_job_members(job, read_hint)?;
    let mut result = process_probe_result(
        job,
        pid,
        agent,
        crate::detect::agent_label(agent).to_string(),
    );
    result.agent_process_pid = agent_process_pid_in_job_with_hint(job, agent, read_hint);
    Some(result)
}

fn probe_foreground_process_from_jobs(
    pid: u32,
    foreground_pgid: Option<u32>,
    leader_job: Option<crate::platform::ForegroundJob>,
    foreground_job: impl FnOnce() -> Option<crate::platform::ForegroundJob>,
    read_hint: impl Fn(u32) -> Option<Agent> + Copy,
) -> ProcessProbeResult {
    if let Some(job) = leader_job.as_ref() {
        if let Some(hinted) = hinted_process_probe_result(job, pid, read_hint) {
            return hinted;
        }
        if let Some((agent, process_name)) = crate::detect::identify_agent_in_job(job) {
            let mut result = process_probe_result(job, pid, agent, process_name);
            result.agent_process_pid = agent_process_pid_in_job_with_hint(job, agent, read_hint);
            return result;
        }
    }

    let foreground_job = foreground_job();
    if let Some(job) = foreground_job.as_ref() {
        if let Some(agent) = read_hint(job.process_group_id) {
            let mut result = process_probe_result(
                job,
                pid,
                agent,
                crate::detect::agent_label(agent).to_string(),
            );
            result.agent_process_pid = agent_process_pid_in_job_with_hint(job, agent, read_hint);
            return result;
        }
        if let Some((agent, process_name)) = identify_process_group_leader_in_job(job) {
            let mut result = process_probe_result(job, pid, agent, process_name);
            result.agent_process_pid = agent_process_pid_in_job_with_hint(job, agent, read_hint);
            return result;
        }
        if let Some(agent) = agent_hint_for_non_leader_foreground_job_members(job, read_hint) {
            let mut result = process_probe_result(
                job,
                pid,
                agent,
                crate::detect::agent_label(agent).to_string(),
            );
            result.agent_process_pid = agent_process_pid_in_job_with_hint(job, agent, read_hint);
            return result;
        }

        let identified = crate::detect::identify_agent_in_job(job);
        return ProcessProbeResult {
            process_group_id: Some(job.process_group_id),
            foreground_is_pane_shell: job.processes.iter().any(|process| process.pid == pid),
            agent_process_pid: identified
                .as_ref()
                .and_then(|(agent, _)| agent_process_pid_in_job_with_hint(job, *agent, read_hint)),
            agent: identified.as_ref().map(|(agent, _)| *agent),
            process_name: identified.map(|(_, process_name)| process_name),
        };
    }

    ProcessProbeResult {
        process_group_id: foreground_pgid,
        foreground_is_pane_shell: false,
        agent: None,
        agent_process_pid: None,
        process_name: None,
    }
}

fn process_sidebar_observation(
    pane_pid: u32,
    agent_process_pid: Option<u32>,
) -> (bool, Option<(AgentState, bool)>) {
    let session_pids = crate::platform::session_processes_cached(pane_pid);
    let holds_shell = session_pids
        .iter()
        .any(|candidate| *candidate != pane_pid && Some(*candidate) != agent_process_pid);
    let stale_resolution = Some(match agent_process_pid {
        None => (AgentState::Idle, false),
        Some(agent_pid) if crate::platform::session_processes_cached(agent_pid).len() > 1 => {
            (AgentState::Working, true)
        }
        Some(_) => (AgentState::Idle, true),
    });
    (holds_shell, stale_resolution)
}

fn probe_foreground_process(pid: u32, foreground_pgid: Option<u32>) -> ProcessProbeResult {
    probe_foreground_process_from_jobs(
        pid,
        foreground_pgid,
        foreground_pgid.and_then(crate::detect::foreground_group_leader_job),
        || crate::detect::foreground_job(pid),
        crate::platform::process_agent_hint,
    )
}

/// Everything the blocked turn-end gate retirement poll reads or writes outside
/// the detect loop's own locals.
struct FullLifecycleHookRetirementPorts<'a> {
    pane_id: PaneId,
    detection_content_seq: &'a AtomicU64,
    baseline_content_seq: &'a AtomicU64,
    authority_active: &'a AtomicBool,
    blocked: &'a AtomicBool,
    state_events: &'a mpsc::Sender<AppEvent>,
}

async fn publish_agent_links_if_dirty(
    pane_id: PaneId,
    gate: &crate::agent_state::LinkExtractionGate,
    state_events: &mpsc::Sender<AppEvent>,
) {
    let Some(links) = gate.take_links() else {
        return;
    };
    let _ = state_events
        .send(AppEvent::OrderedAgentLinksDetected {
            pane_id,
            links: links.ordered_links,
            observed_at: std::time::SystemTime::now(),
        })
        .await;
}

/// Retire hook authority once sustained output contradicts its latest state.
///
/// Two detect loops run this: the one `spawn_basic_detection_task` starts for a
/// freshly spawned pane, and the one the restored/attached path starts. They
/// have to make the same call, so the wiring lives here rather than in two
/// copies that can drift apart.
async fn poll_full_lifecycle_hook_retirement(
    ports: FullLifecycleHookRetirementPorts<'_>,
    retirement: &mut FullLifecycleHookOutputRetirement,
    lifecycle_authority_active: &mut bool,
    process_exited: bool,
    now: std::time::Instant,
) {
    if ports.blocked.load(Ordering::Acquire) && !process_exited {
        if let Some(observed_at) = retirement.observe(
            ports.detection_content_seq.load(Ordering::Relaxed),
            ports.baseline_content_seq.load(Ordering::Acquire),
            now,
        ) {
            ports.authority_active.store(false, Ordering::Release);
            *lifecycle_authority_active = false;
            let _ = ports
                .state_events
                .send(AppEvent::HookAuthorityRetired {
                    pane_id: ports.pane_id,
                    observed_at,
                    suppress_completion: false,
                    answers_human_gate: false,
                })
                .await;
        }
    } else if !ports.blocked.load(Ordering::Acquire) {
        retirement.clear();
    }
}

#[cfg(unix)]
type SpawnedDetectionTask = (
    tokio::task::AbortHandle,
    Arc<Notify>,
    Arc<Notify>,
    Arc<Mutex<Option<PendingAgentRelease>>>,
);

/// The terminal facts a detection task cannot read for itself.
///
/// Grouped because they are only ever read together: the hook owns the pane
/// until it goes stale, and staleness is what hands authority back to the
/// process probe.
#[cfg(unix)]
#[derive(Clone)]
struct DetectionAuthorityMirrors {
    full_lifecycle_active: Arc<AtomicBool>,
    full_lifecycle_blocked: Arc<AtomicBool>,
    supervisor_stale: Arc<AtomicBool>,
}

#[cfg(unix)]
fn spawn_basic_detection_task(
    pane_id: PaneId,
    child_pid: Arc<AtomicU32>,
    terminal: Arc<PaneTerminal>,
    detection_content_seq: Arc<AtomicU64>,
    full_lifecycle_hook_baseline_content_seq: Arc<AtomicU64>,
    agent_output_seq: Arc<AtomicU64>,
    link_extraction: Arc<crate::agent_state::LinkExtractionGate>,
    authority: DetectionAuthorityMirrors,
    state_events: mpsc::Sender<AppEvent>,
    initial_agent: Option<Agent>,
    initial_publish: DetectionPublishState,
) -> SpawnedDetectionTask {
    let DetectionAuthorityMirrors {
        full_lifecycle_active: full_lifecycle_authority_active,
        full_lifecycle_blocked: full_lifecycle_hook_blocked,
        supervisor_stale,
    } = authority;
    let detect_reset_notify = Arc::new(Notify::new());
    let detect_reset = detect_reset_notify.clone();
    let detect_screen_rescan_notify = Arc::new(Notify::new());
    let detect_screen_rescan = detect_screen_rescan_notify.clone();
    let pending_release = Arc::new(Mutex::new(None));
    let pending_release_for_task = pending_release.clone();

    let handle = tokio::spawn(async move {
        let mut agent_presence = AgentDetectionPresence::from_agent(initial_agent);
        let mut state = initial_publish.state;
        let mut last_visible_idle = initial_publish.visible_idle;
        let mut last_visible_blocker = initial_publish.visible_blocker;
        let mut last_visible_working = initial_publish.visible_working;
        let mut last_visible_signal_refresh = None;
        let mut last_process_check = std::time::Instant::now();
        let mut last_foreground_pgid = None;
        let mut has_process_probe = false;
        let mut acquisition_started_at = None;
        let mut last_content_change_at = None;
        let mut pending_foreground_shell_clear = false;
        let mut foreground_shell_exit_reported = false;
        let mut release_was_active = false;
        let mut last_detection_text = String::new();
        let mut last_screen_scan_detection_content_seq = None;
        let mut hook_output_retirement = FullLifecycleHookOutputRetirement::default();
        let mut last_output_seq = Some(agent_output_seq.load(Ordering::Relaxed));
        let mut last_output_at = None;
        let mut agent_startup_grace_until = None;
        let mut pending_idle = PendingIdleConfirmation::default();
        let mut force_screen_rescan = false;

        loop {
            let sleep_duration = if pending_idle.active() {
                AGENT_PENDING_IDLE_RECHECK
            } else {
                std::time::Duration::from_millis(300)
            };
            tokio::select! {
                _ = tokio::time::sleep(sleep_duration) => {}
                _ = detect_reset.notified() => {
                    force_screen_rescan = false;
                    agent_presence = AgentDetectionPresence::from_agent(None);
                    state = AgentState::Unknown;
                    last_visible_idle = false;
                    last_visible_blocker = false;
                    last_visible_working = false;
                    last_visible_signal_refresh = None;
                    last_process_check = std::time::Instant::now();
                    last_foreground_pgid = None;
                    has_process_probe = false;
                    acquisition_started_at = None;
                    last_content_change_at = None;
                    pending_foreground_shell_clear = false;
                    foreground_shell_exit_reported = false;
                    release_was_active = false;
                    last_detection_text.clear();
                    last_screen_scan_detection_content_seq = None;
                    hook_output_retirement.clear();
                    last_output_at = None;
                    agent_startup_grace_until = None;
                    pending_idle.clear();
                }
                _ = detect_screen_rescan.notified() => {
                    last_screen_scan_detection_content_seq = None;
                    force_screen_rescan = true;
                }
            }

            let now = std::time::Instant::now();
            publish_agent_links_if_dirty(pane_id, &link_extraction, &state_events).await;
            let suppressed_agent = active_pending_release(&pending_release_for_task, now);
            if suppressed_agent.is_none() && release_was_active {
                has_process_probe = false;
                acquisition_started_at = None;
                last_content_change_at = None;
            }
            release_was_active = suppressed_agent.is_some();
            let pid = child_pid.load(Ordering::Acquire);
            let mut agent_changed = false;
            let mut agent = agent_presence.current_agent();
            let mut lifecycle_authority_active =
                full_lifecycle_authority_active.load(Ordering::Acquire);
            let supervisor_is_stale = supervisor_stale.load(Ordering::Acquire);
            let foreground_pgid = (pid > 0)
                .then(|| crate::detect::foreground_process_group_id(pid))
                .flatten();
            let process_group_changed =
                foreground_group_changed(foreground_pgid, last_foreground_pgid);
            let should_check_process = pid > 0 && {
                let process_probe_input = ProcessProbeInput {
                    current_agent: agent,
                    suppressed_agent,
                    foreground_pgid,
                    last_foreground_pgid,
                    has_process_probe,
                    acquisition_age: acquisition_started_at
                        .map(|started| now.duration_since(started)),
                    pending_foreground_shell_clear,
                    pending_restore_probe: false,
                    elapsed_since_process_check: now.duration_since(last_process_check),
                    supervisor_stale: supervisor_is_stale,
                };
                !should_skip_process_probe_for_lifecycle_authority(
                    lifecycle_authority_active,
                    process_probe_input,
                ) && should_probe_foreground_job(process_probe_input)
            };

            if should_check_process {
                last_process_check = now;
                let had_process_probe = has_process_probe;
                has_process_probe = true;
                let probe = probe_foreground_process(pid, foreground_pgid);
                let (holds_shell, stale_resolution) =
                    process_sidebar_observation(pid, probe.agent_process_pid);
                if let Err(error) = state_events
                    .send(AppEvent::PaneProcessStateChanged {
                        pane_id,
                        holds_shell,
                        stale_resolution,
                        observed_at: now,
                    })
                    .await
                {
                    warn!(pane = pane_id.raw(), %error, "failed to deliver process state");
                }
                let process_group_id = probe.process_group_id;
                let tracked_process_group_id =
                    process_group_for_change_tracking(foreground_pgid, process_group_id);
                let foreground_is_pane_shell = probe.foreground_is_pane_shell;
                let mut new_agent = probe.agent;
                if let Some(suppressed_agent) = suppressed_agent {
                    if new_agent == Some(suppressed_agent) {
                        new_agent = None;
                    } else if let Ok(mut pending_release) = pending_release_for_task.lock() {
                        *pending_release = None;
                    }
                }
                let previous_agent = agent_presence.current_agent();
                let foreground_action = foreground_shell_agent_action(
                    previous_agent,
                    new_agent,
                    foreground_is_pane_shell,
                    foreground_shell_exit_reported,
                );
                let changed = apply_foreground_shell_agent_action(
                    &mut agent_presence,
                    foreground_action,
                    previous_agent,
                    new_agent,
                    &mut pending_foreground_shell_clear,
                    &mut foreground_shell_exit_reported,
                );
                last_foreground_pgid = tracked_process_group_id;
                if new_agent.is_some() {
                    acquisition_started_at = None;
                    last_content_change_at = None;
                } else if agent_presence.current_agent().is_none()
                    && had_process_probe
                    && process_group_changed
                {
                    acquisition_started_at = Some(now);
                }
                if changed {
                    agent = agent_presence.current_agent();
                    agent_changed = previous_agent != agent
                        || foreground_action
                            == ForegroundShellAgentAction::ReportReplacementProcess;
                    if agent_changed {
                        pending_idle.clear();
                        last_screen_scan_detection_content_seq = None;
                        // A replacement agent must not inherit OSC evidence
                        // from the previous process; a first acquisition keeps
                        // the evidence its own process already emitted.
                        clear_osc_evidence_for_agent_transition(&terminal, previous_agent);
                        if let Some(agent) = agent {
                            agent_startup_grace_until = Some(now + AGENT_STARTUP_GRACE_WINDOW);
                            state = AgentState::Unknown;
                            last_visible_idle = false;
                            last_visible_blocker = false;
                            last_visible_working = false;
                            last_visible_signal_refresh = None;
                            publish_agent_process_detected_event(
                                state_events.clone(),
                                pane_id,
                                agent,
                                now,
                            )
                            .await;
                        } else {
                            agent_startup_grace_until = None;
                        }
                    }
                }
            }

            let process_exited = pending_foreground_shell_clear
                && agent.is_some()
                && !foreground_shell_exit_reported;

            poll_full_lifecycle_hook_retirement(
                FullLifecycleHookRetirementPorts {
                    pane_id,
                    detection_content_seq: &detection_content_seq,
                    baseline_content_seq: &full_lifecycle_hook_baseline_content_seq,
                    authority_active: &full_lifecycle_authority_active,
                    blocked: &full_lifecycle_hook_blocked,
                    state_events: &state_events,
                },
                &mut hook_output_retirement,
                &mut lifecycle_authority_active,
                process_exited,
                now,
            )
            .await;

            if agent_changed || suppressed_agent.is_some() {
                last_output_at = None;
            }
            let recent_output = recent_agent_output(
                agent,
                agent_output_seq.load(Ordering::Relaxed),
                &mut last_output_seq,
                &mut last_output_at,
                now,
            );

            if should_skip_screen_detection_for_lifecycle_authority(
                lifecycle_authority_active,
                full_lifecycle_hook_blocked.load(Ordering::Acquire),
                process_exited,
            ) {
                pending_idle.clear();
                continue;
            }

            if !force_screen_rescan {
                if let Some(until) = agent_startup_grace_until {
                    if process_exited {
                        agent_startup_grace_until = None;
                        pending_idle.clear();
                    } else {
                        if now < until {
                            pending_idle.clear();
                            continue;
                        }
                        agent_startup_grace_until = None;
                        last_screen_scan_detection_content_seq = None;
                        pending_idle.clear();
                        continue;
                    }
                }
            }

            let current_detection_content_seq = if agent.is_some() {
                Some(detection_content_seq.load(Ordering::Relaxed))
            } else {
                None
            };
            match decide_detection_screen_read(DetectionScreenReadInput {
                state,
                agent,
                pending_idle_active: pending_idle.active(),
                agent_changed,
                process_exited,
                current_detection_content_seq,
                last_screen_scan_detection_content_seq,
            }) {
                DetectionScreenReadDecision::Read => {}
                DetectionScreenReadDecision::Skip => continue,
            }

            let content = terminal.detection_text();
            last_screen_scan_detection_content_seq = current_detection_content_seq;
            let content_changed = content != last_detection_text;
            last_detection_text.clone_from(&content);
            if !process_exited && crate::detect::should_skip_state_update(agent, &content) {
                pending_idle.clear();
                force_screen_rescan = false;
                continue;
            }
            sync_content_change_acquisition(
                agent_presence.current_agent(),
                suppressed_agent,
                process_group_changed,
                content_changed,
                now,
                &mut acquisition_started_at,
                &mut last_content_change_at,
            );

            let osc_title = terminal.agent_osc_title();
            let osc_progress = terminal.agent_osc_progress();
            let Some(screen_detection) = detection_update_for_publish_with_osc(
                agent,
                &content,
                &osc_title,
                &osc_progress,
                process_exited,
            ) else {
                pending_idle.clear();
                continue;
            };
            match decide_screen_detection_publish(
                ScreenDetectionPublishInput {
                    screen_detection,
                    recent_output,
                    current_state: state,
                    last_visible_idle,
                    last_visible_blocker,
                    last_visible_working,
                    last_visible_signal_refresh,
                    process_exited,
                    agent_changed,
                    force_publish: std::mem::take(&mut force_screen_rescan),
                    now,
                },
                &mut pending_idle,
            ) {
                DetectionPublishDecision::NoPublish => {}
                DetectionPublishDecision::Publish {
                    state: new_state,
                    visible_idle,
                    visible_blocker,
                    visible_working,
                    usage_limited,
                    process_exited: publish_process_exited,
                } => {
                    apply_agent_detection_publish_update(
                        state_events.clone(),
                        pane_id,
                        agent,
                        AgentDetectionPublishUpdate {
                            state: new_state,
                            visible_idle,
                            visible_blocker,
                            visible_working,
                            usage_limited,
                            process_exited: publish_process_exited,
                        },
                        now,
                        &mut state,
                        &mut last_visible_idle,
                        &mut last_visible_blocker,
                        &mut last_visible_working,
                        &mut last_visible_signal_refresh,
                        &mut foreground_shell_exit_reported,
                    )
                    .await;
                }
            }
        }
    });

    (
        handle.abort_handle(),
        detect_reset_notify,
        detect_screen_rescan_notify,
        pending_release,
    )
}

impl AgentDetectionPresence {
    fn from_agent(current_agent: Option<Agent>) -> Self {
        Self {
            current_agent,
            consecutive_misses: 0,
        }
    }

    fn current_agent(&self) -> Option<Agent> {
        self.current_agent
    }

    fn clear_current_agent(&mut self) -> bool {
        if self.current_agent.is_none() {
            self.consecutive_misses = 0;
            return false;
        }
        self.current_agent = None;
        self.consecutive_misses = 0;
        true
    }

    fn observe_process_probe(&mut self, identified_agent: Option<Agent>) -> bool {
        match identified_agent {
            Some(agent) => {
                self.consecutive_misses = 0;
                if Some(agent) == self.current_agent {
                    return false;
                }
                self.current_agent = Some(agent);
                true
            }
            None => {
                if self.current_agent.is_none() {
                    self.consecutive_misses = 0;
                    return false;
                }
                self.consecutive_misses = self.consecutive_misses.saturating_add(1);
                if self.consecutive_misses < AGENT_MISS_CONFIRMATION_ATTEMPTS {
                    return false;
                }
                self.current_agent = None;
                self.consecutive_misses = 0;
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PaneRuntime — PTY, parser, channels, background tasks
// ---------------------------------------------------------------------------

/// PTY runtime for a pane. Owns the terminal, I/O channels, and background tasks.
/// Dropping this shuts down all background tasks and closes the PTY.
pub struct PaneRuntime {
    pane_id: PaneId,
    terminal: Arc<PaneTerminal>,
    io: PaneRuntimeIo,
    current_size: Cell<(u16, u16, u32, u32)>,
    #[cfg(test)]
    resize_count: Cell<usize>,
    child_pid: Arc<AtomicU32>,
    tty_name: Option<std::path::PathBuf>,
    reported_cwd: Arc<Mutex<Option<std::path::PathBuf>>>,
    persistence_cwd: Mutex<Option<std::path::PathBuf>>,
    cwd_process_exited: Arc<AtomicBool>,
    child_wait_completed: Option<Arc<AtomicBool>>,
    kitty_keyboard_flags: Arc<AtomicU16>,
    content_seq: Arc<AtomicU64>,
    input_admission: Arc<Mutex<InputAdmissionState>>,
    pending_input_reservations: Arc<AtomicU64>,
    input_delivery_seq: Arc<AtomicU64>,
    reflected_input_seq: Arc<AtomicU64>,
    detection_content_seq: Arc<AtomicU64>,
    full_lifecycle_hook_baseline_content_seq: Arc<AtomicU64>,
    full_lifecycle_authority_active: Arc<AtomicBool>,
    full_lifecycle_hook_blocked: Arc<AtomicBool>,
    supervisor_stale: Arc<AtomicBool>,
    detect_reset_notify: Arc<Notify>,
    detect_screen_rescan_notify: Arc<Notify>,
    pending_release: Arc<Mutex<Option<PendingAgentRelease>>>,
    suspended: bool,
    suppress_pane_died: Arc<AtomicBool>,
    preserve_processes_on_drop: bool,
    // Task handles for deterministic shutdown
    detect_handle: Option<tokio::task::AbortHandle>,
}

#[derive(Debug)]
struct InputAdmissionState {
    generation: u64,
    revision: u64,
}

struct PendingInputReservation {
    pending: Arc<AtomicU64>,
    input_delivery_seq: Arc<AtomicU64>,
    delivered: bool,
}

impl PendingInputReservation {
    fn new(pending: &Arc<AtomicU64>, input_delivery_seq: &Arc<AtomicU64>) -> Self {
        pending.fetch_add(1, Ordering::AcqRel);
        Self {
            pending: Arc::clone(pending),
            input_delivery_seq: Arc::clone(input_delivery_seq),
            delivered: false,
        }
    }

    fn mark_delivered(&mut self) {
        self.delivered = true;
    }
}

impl Drop for PendingInputReservation {
    fn drop(&mut self) {
        if self.delivered {
            self.input_delivery_seq.fetch_add(1, Ordering::AcqRel);
        }
        self.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

fn mark_input_reflected_through(reflected_input_seq: &AtomicU64, input_delivery_seq: u64) {
    reflected_input_seq.fetch_max(input_delivery_seq, Ordering::AcqRel);
}

static NEXT_INPUT_ADMISSION_GENERATION: AtomicU64 = AtomicU64::new(1);

impl Default for InputAdmissionState {
    fn default() -> Self {
        Self {
            generation: NEXT_INPUT_ADMISSION_GENERATION.fetch_add(1, Ordering::Relaxed),
            revision: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputObservation {
    pub(crate) text: String,
    pub(crate) token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConditionalInputResult {
    Sent,
    ConditionMismatch,
    Failed,
}

fn input_observation_token(
    generation: u64,
    input_revision: u64,
    content_sequence: u64,
    content_revision: u64,
    text: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(generation.to_le_bytes());
    digest.update(input_revision.to_le_bytes());
    digest.update(content_sequence.to_le_bytes());
    digest.update(content_revision.to_le_bytes());
    digest.update(text.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Outbound messages from a remote focus proxy pane to its wire transport.
///
/// `SyncResize` carries no dimensions. The transport reads the latest values
/// from the shared resize slot whenever it handles an outbound item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxyOutbound {
    Input(Bytes),
    SyncResize,
    // Sent by the Unix wire transport's detach path; on Windows nothing
    // queues a detach yet.
    #[cfg_attr(not(unix), allow(dead_code))]
    Detach,
}

/// Channel ends a remote proxy runtime hands to the focus transport.
// The Unix wire transport session loop reads these; on Windows the struct is
// created and dropped without a consumer.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct RemoteProxyChannels {
    pub outbound_rx: mpsc::Receiver<ProxyOutbound>,
    /// Sender clone so the transport can queue a detach even after the pane
    /// (and its own sender) is gone.
    pub detach_tx: mpsc::Sender<ProxyOutbound>,
    pub resize_slot: Arc<Mutex<(u16, u16, u32, u32)>>,
    /// Shared with the transport so a failed wire write closes the local
    /// input gate before the proxy pane is torn down.
    pub input_enabled: Arc<AtomicBool>,
    /// Shared with the app and transport so connection loss is terminal even
    /// while already queued app events are still being delivered.
    pub operation_state: Arc<crate::remote::RemoteFocusOperationState>,
}

/// Keystroke bursts (including pastes) queue briefly; a stuck transport drops
/// new input instead of growing memory without bound.
const REMOTE_PROXY_OUTBOUND_CAPACITY: usize = 64;
const KITTY_GRAPHICS_STREAM_MAX_RENDER_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);
const KITTY_GRAPHICS_DIRECT_STREAM_MAX_RENDER_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(20);

/// Keeps the leading render that prevents continuous streams from freezing,
/// then collapses a burst to its newest frame. Long streams still refresh at
/// a bounded cadence so applications remain visibly live without filling the
/// host terminal's input queue with obsolete full-frame images.
#[derive(Debug, Default)]
pub(crate) struct KittyGraphicsRenderThrottle {
    generation: AtomicU64,
    armed: AtomicBool,
}

impl KittyGraphicsRenderThrottle {
    pub(crate) fn request(
        self: &Arc<Self>,
        pane_id: PaneId,
        delay: std::time::Duration,
        render_notify: &Arc<Notify>,
        render_dirty: &Arc<RenderSignal>,
        runtime: &tokio::runtime::Handle,
    ) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if self
            .armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        request_pty_render(pane_id, render_notify, render_dirty);

        let throttle = Arc::clone(self);
        let render_notify = Arc::clone(render_notify);
        let render_dirty = Arc::clone(render_dirty);
        runtime.spawn(async move {
            let mut observed_generation = throttle.generation.load(Ordering::Acquire);
            let mut last_render = std::time::Instant::now();
            loop {
                tokio::time::sleep(delay).await;
                let current_generation = throttle.generation.load(Ordering::Acquire);
                let stream_is_quiet = current_generation == observed_generation;
                let max_render_interval = if crate::kitty_graphics::direct_host_graphics_enabled() {
                    KITTY_GRAPHICS_DIRECT_STREAM_MAX_RENDER_INTERVAL
                } else {
                    KITTY_GRAPHICS_STREAM_MAX_RENDER_INTERVAL
                };
                if !stream_is_quiet && last_render.elapsed() < max_render_interval {
                    observed_generation = current_generation;
                    continue;
                }

                request_pty_render(pane_id, &render_notify, &render_dirty);
                last_render = std::time::Instant::now();
                observed_generation = current_generation;
                if !stream_is_quiet {
                    continue;
                }

                throttle.armed.store(false, Ordering::Release);
                if throttle.generation.load(Ordering::Acquire) == current_generation {
                    break;
                }
                if throttle
                    .armed
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    break;
                }
            }
        });
    }
}

fn request_pty_render(pane_id: PaneId, render_notify: &Notify, render_dirty: &RenderSignal) {
    if render_dirty.request_pty(pane_id) {
        render_notify.notify_one();
    }
}

enum PaneRuntimeIo {
    Actor(PtyIoActorHandle),
    /// A local pane whose screen is fed by a remote focus wire stream. There
    /// is no PTY: user input, resizes, and detach flow to the transport.
    RemoteProxy {
        outbound: mpsc::Sender<ProxyOutbound>,
        /// Input gate. Keystrokes are refused (never buffered) until the first
        /// complete frame and `ControlReady` have both arrived.
        input_enabled: Arc<AtomicBool>,
        operation_state: Arc<crate::remote::RemoteFocusOperationState>,
        resize_slot: Arc<Mutex<(u16, u16, u32, u32)>>,
        /// Terminal responses to queries in the frame stream are discarded:
        /// the remote server already answered them for its own terminal.
        response_sink: mpsc::Sender<Bytes>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    },
    #[cfg(test)]
    TestChannel {
        sender: mpsc::Sender<Bytes>,
        resize_tx: watch::Sender<(u16, u16, u32, u32)>,
        remote_owner: Arc<Mutex<Option<u64>>>,
    },
}

impl PaneRuntimeIo {
    fn shutdown(&self) {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.shutdown(),
            PaneRuntimeIo::RemoteProxy { .. } => {}
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => {}
        }
    }

    #[cfg(unix)]
    fn duplicate_handoff_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.duplicate_for_handoff(),
            PaneRuntimeIo::RemoteProxy { .. } => {
                Err(std::io::Error::other("remote proxy pane has no PTY"))
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => {
                Err(std::io::Error::other("test runtime has no PTY master fd"))
            }
        }
    }

    #[cfg(unix)]
    fn foreground_process_group_id(&self) -> Option<u32> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.foreground_process_group_id(),
            PaneRuntimeIo::RemoteProxy { .. } => None,
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => None,
        }
    }

    #[cfg(unix)]
    fn try_acquire_remote_owner(&self, owner_id: u64) -> RemoteOwnerAcquireResult {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.try_acquire_remote_owner(owner_id),
            PaneRuntimeIo::RemoteProxy { .. } => {
                // The pane's surface is owned by its remote focus lease; a
                // second local controller is rejected, never taken over.
                let _ = owner_id;
                RemoteOwnerAcquireResult::AlreadyControlled
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { remote_owner, .. } => {
                let mut owner = remote_owner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match *owner {
                    None => {
                        *owner = Some(owner_id);
                        RemoteOwnerAcquireResult::Acquired
                    }
                    Some(existing) if existing == owner_id => RemoteOwnerAcquireResult::Acquired,
                    Some(_) => RemoteOwnerAcquireResult::AlreadyControlled,
                }
            }
        }
    }

    #[cfg(all(unix, test))]
    fn acquire_remote_owner(&self, owner_id: u64) -> bool {
        self.try_acquire_remote_owner(owner_id) == RemoteOwnerAcquireResult::Acquired
    }

    #[cfg(unix)]
    fn release_remote_owner(&self, owner_id: u64) {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.release_remote_owner(owner_id),
            PaneRuntimeIo::RemoteProxy { .. } => {
                let _ = owner_id;
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { remote_owner, .. } => {
                let mut owner = remote_owner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *owner == Some(owner_id) {
                    *owner = None;
                }
            }
        }
    }

    #[cfg(unix)]
    fn try_send_controlled_bytes(&self, owner_id: u64, bytes: &[u8]) -> ControlledWriteResult {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.try_write_controlled_user_input(owner_id, bytes),
            PaneRuntimeIo::RemoteProxy { .. } => {
                let _ = (owner_id, bytes);
                ControlledWriteResult::Refused
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel {
                sender,
                remote_owner,
                ..
            } => {
                let owner = remote_owner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *owner != Some(owner_id) {
                    return ControlledWriteResult::Refused;
                }
                match sender.try_send(Bytes::copy_from_slice(bytes)) {
                    Ok(()) => ControlledWriteResult::Written,
                    Err(_) => ControlledWriteResult::DeliveryUnknown { written: 0 },
                }
            }
        }
    }

    #[cfg(unix)]
    fn begin_handoff(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.begin_handoff(timeout),
            PaneRuntimeIo::RemoteProxy { .. } => {
                let _ = timeout;
                Ok(())
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => Ok(()),
        }
    }

    #[cfg(unix)]
    fn set_handoff_paused(&self, paused: bool) -> std::io::Result<()> {
        match self {
            PaneRuntimeIo::Actor(actor) => {
                if paused {
                    actor.begin_handoff(std::time::Duration::from_secs(1))
                } else {
                    actor.rollback_handoff()
                }
            }
            PaneRuntimeIo::RemoteProxy { .. } => Ok(()),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => Ok(()),
        }
    }

    #[cfg(unix)]
    fn release_after_commit(&self) -> std::io::Result<()> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.release_after_commit(),
            PaneRuntimeIo::RemoteProxy { .. } => Ok(()),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => Ok(()),
        }
    }

    fn resize(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        terminal_responses: Vec<Bytes>,
    ) {
        match self {
            PaneRuntimeIo::Actor(actor) => {
                actor.resize(
                    rows,
                    cols,
                    cell_width_px,
                    cell_height_px,
                    terminal_responses,
                );
            }
            PaneRuntimeIo::RemoteProxy {
                outbound,
                resize_slot,
                ..
            } => {
                Self::queue_remote_proxy_resize(
                    outbound,
                    resize_slot,
                    (rows, cols, cell_width_px, cell_height_px),
                );
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { resize_tx, .. } => {
                let _ = resize_tx.send((rows, cols, cell_width_px, cell_height_px));
            }
        }
    }

    fn queue_remote_proxy_resize(
        outbound: &mpsc::Sender<ProxyOutbound>,
        resize_slot: &Mutex<(u16, u16, u32, u32)>,
        size: (u16, u16, u32, u32),
    ) {
        let mut slot = resize_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = size;
        // The marker only wakes the writer. If the queue is full, the writer
        // will compare this slot while handling the items already ahead of it.
        let _ = outbound.try_send(ProxyOutbound::SyncResize);
    }

    #[cfg(unix)]
    fn nudge_child_redraw_after_handoff(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) {
        match self {
            PaneRuntimeIo::Actor(actor) => {
                actor.nudge_child_redraw_after_handoff(rows, cols, cell_width_px, cell_height_px);
            }
            PaneRuntimeIo::RemoteProxy { .. } => {}
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => {}
        }
    }

    async fn send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.write_user_input(bytes).await,
            PaneRuntimeIo::RemoteProxy {
                outbound,
                input_enabled,
                operation_state,
                ..
            } => {
                if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                    return Err(mpsc::error::SendError(bytes));
                }
                let terminal_notification = operation_state.terminal_notification().notified();
                tokio::pin!(terminal_notification);
                terminal_notification.as_mut().enable();
                if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                    return Err(mpsc::error::SendError(bytes));
                }
                operation_state.before_input_send();
                let permit = tokio::select! {
                    permit = outbound.clone().reserve_owned() => permit
                        .map_err(|_| mpsc::error::SendError(bytes.clone()))?,
                    _ = &mut terminal_notification => return Err(mpsc::error::SendError(bytes)),
                };
                let _admission = operation_state.lock_input_admission();
                if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                    return Err(mpsc::error::SendError(bytes));
                }
                permit.send(ProxyOutbound::Input(bytes));
                Ok(())
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel {
                sender,
                remote_owner,
                ..
            } => {
                if remote_owner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_some()
                {
                    return Err(mpsc::error::SendError(bytes));
                }
                sender.send(bytes).await
            }
        }
    }

    fn try_send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.try_write_user_input(bytes),
            PaneRuntimeIo::RemoteProxy {
                outbound,
                input_enabled,
                operation_state,
                ..
            } => {
                if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                    return Err(mpsc::error::TrySendError::Closed(bytes));
                }
                operation_state.before_input_send();
                let _admission = operation_state.lock_input_admission();
                if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                    return Err(mpsc::error::TrySendError::Closed(bytes));
                }
                outbound
                    .try_send(ProxyOutbound::Input(bytes))
                    .map_err(|error| match error {
                        mpsc::error::TrySendError::Full(ProxyOutbound::Input(bytes)) => {
                            mpsc::error::TrySendError::Full(bytes)
                        }
                        mpsc::error::TrySendError::Closed(ProxyOutbound::Input(bytes)) => {
                            mpsc::error::TrySendError::Closed(bytes)
                        }
                        _ => unreachable!("only input is sent as bytes"),
                    })
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel {
                sender,
                remote_owner,
                ..
            } => {
                if remote_owner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_some()
                {
                    return Err(mpsc::error::TrySendError::Closed(bytes));
                }
                sender.try_send(bytes)
            }
        }
    }

    fn remote_proxy_input_enabled(&self) -> Option<bool> {
        let PaneRuntimeIo::RemoteProxy {
            input_enabled,
            operation_state,
            ..
        } = self
        else {
            return None;
        };
        Some(!operation_state.is_terminal() && input_enabled.load(Ordering::Acquire))
    }

    fn write_terminal_response(&self, response: impl FnOnce() -> Option<Bytes>) {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.write_terminal_response(response),
            PaneRuntimeIo::RemoteProxy { .. } => {
                // Responses to queries inside a remote frame stream are
                // dropped: the remote server answered them already.
                let _ = response;
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { sender, .. } => {
                if let Some(bytes) = response() {
                    let _ = sender.try_send(bytes);
                }
            }
        }
    }

    fn send_bytes_after(
        &self,
        bytes: Bytes,
        delay: std::time::Duration,
        mut reservation: PendingInputReservation,
    ) {
        match self {
            PaneRuntimeIo::Actor(actor) => {
                let actor = actor.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    match actor.write_user_input(bytes).await {
                        Ok(()) => reservation.mark_delivered(),
                        Err(err) => warn!(error = %err, "failed to send delayed PTY input"),
                    }
                });
            }
            PaneRuntimeIo::RemoteProxy {
                outbound,
                input_enabled,
                operation_state,
                ..
            } => {
                let outbound = outbound.clone();
                let input_enabled = Arc::clone(input_enabled);
                let operation_state = Arc::clone(operation_state);
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                        return;
                    }
                    let terminal_notification = operation_state.terminal_notification().notified();
                    tokio::pin!(terminal_notification);
                    terminal_notification.as_mut().enable();
                    if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                        return;
                    }
                    operation_state.before_input_send();
                    let permit = match tokio::select! {
                        permit = outbound.reserve_owned() => permit,
                        _ = &mut terminal_notification => return,
                    } {
                        Ok(permit) => permit,
                        Err(_) => return,
                    };
                    let _admission = operation_state.lock_input_admission();
                    if operation_state.is_terminal() || !input_enabled.load(Ordering::Acquire) {
                        return;
                    }
                    permit.send(ProxyOutbound::Input(bytes));
                    reservation.mark_delivered();
                });
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel {
                sender,
                remote_owner,
                ..
            } => {
                let sender = sender.clone();
                let remote_owner = Arc::clone(remote_owner);
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    if remote_owner
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .is_some()
                    {
                        return;
                    }
                    if sender.send(bytes).await.is_ok() {
                        reservation.mark_delivered();
                    }
                });
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

impl Drop for PaneRuntime {
    fn drop(&mut self) {
        // Abort detection task immediately and terminate the owned session.
        // The PTY actor shuts down before the process/session policy runs.
        if let Some(handle) = &self.detect_handle {
            handle.abort();
        }
        self.io.shutdown();
        if !self.preserve_processes_on_drop {
            shutdown_pane_processes(
                self.pane_id,
                self.child_pid.load(Ordering::Acquire),
                self.child_wait_completed.as_deref(),
            );
        }
    }
}

fn process_alive_for_shutdown(
    pid: u32,
    child_pid: u32,
    child_wait_completed: bool,
    process_exists: impl FnOnce(u32) -> bool,
) -> bool {
    if pid == child_pid && child_wait_completed {
        return false;
    }
    process_exists(pid)
}

fn wait_for_processes_to_exit(
    pids: &[u32],
    child_pid: u32,
    child_wait_completed: Option<&AtomicBool>,
    timeout: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let child_wait_completed =
            child_wait_completed.is_some_and(|flag| flag.load(Ordering::Acquire));
        if pids.iter().all(|pid| {
            !process_alive_for_shutdown(
                *pid,
                child_pid,
                child_wait_completed,
                crate::platform::process_exists,
            )
        }) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn shutdown_pane_processes(
    pane_id: PaneId,
    child_pid: u32,
    child_wait_completed: Option<&AtomicBool>,
) {
    if child_pid == 0 {
        return;
    }

    let mut pids = crate::platform::session_processes(child_pid);
    if pids.is_empty() {
        pids.push(child_pid);
    }
    pids.sort_unstable();
    pids.dedup();

    for (signal, grace) in [
        (
            crate::platform::Signal::Hangup,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Terminate,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Kill,
            std::time::Duration::from_millis(250),
        ),
    ] {
        crate::platform::signal_processes(&pids, signal);
        if wait_for_processes_to_exit(&pids, child_pid, child_wait_completed, grace) {
            info!(
                pane = pane_id.raw(),
                pid = child_pid,
                ?signal,
                "pane session terminated"
            );
            return;
        }
    }

    warn!(
        pane = pane_id.raw(),
        pid = child_pid,
        pids = ?pids,
        "pane session still alive after forced shutdown"
    );
}

#[cfg(unix)]
fn truncate_handoff_history(history: String, max_bytes: usize) -> String {
    if history.len() <= max_bytes {
        return history;
    }
    let mut start = history.len().saturating_sub(max_bytes);
    while !history.is_char_boundary(start) {
        start += 1;
    }
    let Some(newline_offset) = history[start..].find('\n') else {
        return String::new();
    };
    start += newline_offset + 1;
    history[start..].to_owned()
}

fn pane_shell(configured_shell: &str) -> String {
    pane_shell_from(configured_shell, std::env::var("SHELL").ok())
}

fn pane_shell_from(configured_shell: &str, env_shell: Option<String>) -> String {
    let configured_shell = configured_shell.trim();
    if !configured_shell.is_empty() {
        return configured_shell.to_string();
    }

    #[cfg(windows)]
    {
        let _ = env_shell;
        default_pane_shell()
    }

    #[cfg(not(windows))]
    env_shell
        .map(|shell| shell.trim().to_string())
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(default_pane_shell)
}

#[cfg(windows)]
fn default_pane_shell() -> String {
    "powershell.exe".into()
}

#[cfg(not(windows))]
fn default_pane_shell() -> String {
    "/bin/sh".into()
}

#[derive(Clone, Copy)]
pub(crate) struct PaneShellConfig<'a> {
    pub(crate) default_shell: &'a str,
    pub(crate) mode: crate::config::ShellModeConfig,
}

impl<'a> PaneShellConfig<'a> {
    pub(crate) fn new(default_shell: &'a str, mode: crate::config::ShellModeConfig) -> Self {
        Self {
            default_shell,
            mode,
        }
    }
}

/// Target platform for shell launch policy. Parameterized (instead of raw
/// `cfg!` checks at each decision point) so every branch stays testable on
/// every host platform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShellLaunchTarget {
    Windows,
    Macos,
    OtherUnix,
}

impl ShellLaunchTarget {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::OtherUnix
        }
    }
}

fn shell_mode_uses_login_shell(
    mode: crate::config::ShellModeConfig,
    target: ShellLaunchTarget,
) -> bool {
    match mode {
        crate::config::ShellModeConfig::Auto => target == ShellLaunchTarget::Macos,
        crate::config::ShellModeConfig::Login => true,
        crate::config::ShellModeConfig::NonLogin => false,
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn resolve_shell_for_login_mode(shell: &str) -> io::Result<String> {
    if shell.contains(std::path::MAIN_SEPARATOR) {
        let path = Path::new(shell);
        return is_executable_file(path)
            .then(|| shell.to_string())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("login shell {shell:?} is not executable"),
                )
            });
    }

    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(shell))
                .find(|candidate| is_executable_file(candidate))
        })
        .and_then(|path| path.into_os_string().into_string().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("login shell {shell:?} was not found on PATH"),
            )
        })
}

/// Sourced via `-NoExit -Command` when launching PowerShell on Windows. It
/// wraps whatever `prompt` function the user's profile left behind so each
/// prompt render appends the cwd as OSC 9;9 — the sequence Windows Terminal
/// and ConEmu standardized for shell integration. PowerShell never updates
/// its Win32 process cwd on `Set-Location`, so the prompt hook updates it when
/// possible before reporting the cwd.
///
/// The snippet must not contain double quotes: powershell.exe parses its
/// command line with its own rules that disagree with the ArgvQuote escaping
/// portable-pty applies, and embedded `\"` sequences get corrupted in
/// transit. Single-quoted strings and `[char]` codes keep the round-trip
/// byte-exact, and the OSC 9;9 payload is emitted unquoted (the original
/// ConEmu form, which the cwd tracker accepts).
///
/// The original prompt must be invoked before any other statement in the
/// wrapper: anything that runs first resets `$?`, so a status-aware user
/// prompt would show success after a failed command (verified on 5.1).
pub(crate) const WINDOWS_POWERSHELL_SHELL_INTEGRATION_COMMAND: &str = r"if ($null -eq $global:__HerdrOriginalPrompt) { $global:__HerdrOriginalPrompt = $function:prompt; function global:prompt { $out = @(& $global:__HerdrOriginalPrompt) -join ' '; $loc = $ExecutionContext.SessionState.Path.CurrentLocation; if ($loc.Provider.Name -eq 'FileSystem') { try { [Environment]::CurrentDirectory = $loc.ProviderPath } catch {}; $esc = [string][char]27; $out += $esc + ']9;9;' + $loc.ProviderPath + $esc + '\' }; $out } }";

fn pane_shell_command_builder_for_target(
    shell_config: PaneShellConfig<'_>,
    target: ShellLaunchTarget,
) -> io::Result<CommandBuilder> {
    let shell = pane_shell(shell_config.default_shell);
    if shell_mode_uses_login_shell(shell_config.mode, target) {
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", resolve_shell_for_login_mode(&shell)?);
        Ok(cmd)
    } else {
        let mut cmd = CommandBuilder::new(&shell);
        if uses_windows_powershell_pane_shell_for_target(shell_config, target) {
            cmd.args([
                "-NoExit",
                "-Command",
                WINDOWS_POWERSHELL_SHELL_INTEGRATION_COMMAND,
            ]);
        }
        Ok(cmd)
    }
}

fn pane_shell_command_builder(shell_config: PaneShellConfig<'_>) -> io::Result<CommandBuilder> {
    pane_shell_command_builder_for_target(shell_config, ShellLaunchTarget::current())
}

/// True when panes launch an interactive PowerShell directly on Windows.
/// Gates the prompt-based cwd reporting pipeline and the agent-exit shell
/// respawn recovery.
pub(crate) fn uses_windows_powershell_pane_shell(shell_config: PaneShellConfig<'_>) -> bool {
    uses_windows_powershell_pane_shell_for_target(shell_config, ShellLaunchTarget::current())
}

fn uses_windows_powershell_pane_shell_for_target(
    shell_config: PaneShellConfig<'_>,
    target: ShellLaunchTarget,
) -> bool {
    target == ShellLaunchTarget::Windows
        && !shell_mode_uses_login_shell(shell_config.mode, target)
        && is_powershell_shell(&pane_shell(shell_config.default_shell))
}

fn is_powershell_shell(shell: &str) -> bool {
    // Split on both separators by hand: `Path::file_name` only treats `\` as
    // a separator on Windows hosts, and this predicate must evaluate Windows
    // shell paths correctly from tests on any host.
    let name = shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    )
}

fn usable_reported_cwd(cwd: std::path::PathBuf) -> Option<std::path::PathBuf> {
    (cwd.is_absolute() && cwd.is_dir()).then_some(cwd)
}

fn publish_terminal_bells(pane_id: PaneId, count: u16, events: &mpsc::Sender<AppEvent>) {
    if count == 0 {
        return;
    }
    if let Err(err) = events.try_send(AppEvent::TerminalBell { pane_id, count }) {
        warn!(
            pane = pane_id.raw(),
            count,
            err = %err,
            "failed to queue terminal bell"
        );
    }
}

fn publish_reported_cwd(
    pane_id: PaneId,
    cwd: std::path::PathBuf,
    reported_cwd: &Arc<Mutex<Option<std::path::PathBuf>>>,
    events: &mpsc::Sender<AppEvent>,
) {
    let Some(cwd) = usable_reported_cwd(cwd) else {
        return;
    };
    if let Ok(mut current) = reported_cwd.lock() {
        if current.as_ref() == Some(&cwd) {
            return;
        }
        *current = Some(cwd.clone());
    }
    if let Err(err) = events.try_send(AppEvent::TerminalCwdReported { pane_id, cwd }) {
        warn!(
            pane = pane_id.raw(),
            err = %err,
            "failed to send terminal cwd report"
        );
    }
}

impl PaneRuntime {
    /// A pane runtime with no PTY whose screen is fed by remote focus wire
    /// frames. The returned channels belong to the focus transport: it
    /// receives user input, resize markers, and detach, and it reads the
    /// latest dimensions from the resize slot.
    pub(crate) fn spawn_remote_proxy(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
        operation_state: Arc<crate::remote::RemoteFocusOperationState>,
    ) -> std::io::Result<(Self, RemoteProxyChannels)> {
        let (outbound_tx, outbound_rx) = mpsc::channel(REMOTE_PROXY_OUTBOUND_CAPACITY);
        let detach_tx = outbound_tx.clone();
        let resize_slot = Arc::new(Mutex::new((rows, cols, 0, 0)));
        let input_enabled = Arc::new(AtomicBool::new(false));
        let terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let (response_tx, _response_rx) = mpsc::channel(1);
        let ghostty = GhosttyPaneTerminal::new(terminal, response_tx)?;
        let (response_sink, _sink_rx) = mpsc::channel(1);
        Ok((
            Self {
                pane_id,
                terminal: Arc::new(PaneTerminal::new(ghostty)),
                io: PaneRuntimeIo::RemoteProxy {
                    outbound: outbound_tx,
                    input_enabled: Arc::clone(&input_enabled),
                    operation_state: Arc::clone(&operation_state),
                    resize_slot: Arc::clone(&resize_slot),
                    response_sink,
                    render_notify,
                    render_dirty,
                },
                current_size: Cell::new((rows, cols, 0, 0)),
                #[cfg(test)]
                resize_count: Cell::new(0),
                child_pid: Arc::new(AtomicU32::new(0)),
                tty_name: None,
                reported_cwd: Arc::new(Mutex::new(None)),
                persistence_cwd: Mutex::new(None),
                cwd_process_exited: Arc::new(AtomicBool::new(false)),
                child_wait_completed: None,
                kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
                content_seq: Arc::new(AtomicU64::new(0)),
                input_admission: Arc::new(Mutex::new(InputAdmissionState::default())),
                pending_input_reservations: Arc::new(AtomicU64::new(0)),
                input_delivery_seq: Arc::new(AtomicU64::new(0)),
                reflected_input_seq: Arc::new(AtomicU64::new(0)),
                detection_content_seq: Arc::new(AtomicU64::new(0)),
                full_lifecycle_hook_baseline_content_seq: Arc::new(AtomicU64::new(0)),
                full_lifecycle_authority_active: Arc::new(AtomicBool::new(false)),
                full_lifecycle_hook_blocked: Arc::new(AtomicBool::new(false)),
                detect_reset_notify: Arc::new(Notify::new()),
                detect_screen_rescan_notify: Arc::new(Notify::new()),
                pending_release: Arc::new(Mutex::new(None)),
                suspended: false,
                supervisor_stale: Arc::new(AtomicBool::new(false)),
                suppress_pane_died: Arc::new(AtomicBool::new(true)),
                preserve_processes_on_drop: true,
                detect_handle: None,
            },
            RemoteProxyChannels {
                outbound_rx,
                detach_tx,
                resize_slot,
                input_enabled,
                operation_state,
            },
        ))
    }

    pub(crate) fn is_remote_proxy(&self) -> bool {
        matches!(self.io, PaneRuntimeIo::RemoteProxy { .. })
    }

    /// Opens or closes the input gate. Keystrokes are refused while the gate
    /// is closed; they are never buffered for later delivery.
    pub(crate) fn set_remote_proxy_input_enabled(&self, enabled: bool) -> bool {
        let PaneRuntimeIo::RemoteProxy {
            input_enabled,
            operation_state,
            ..
        } = &self.io
        else {
            return false;
        };
        if enabled && operation_state.is_terminal() {
            return false;
        }
        input_enabled.store(enabled, Ordering::Release);
        true
    }

    pub(crate) fn remote_proxy_input_enabled(&self) -> Option<bool> {
        self.io.remote_proxy_input_enabled()
    }

    /// Feeds one complete remote terminal frame into the local screen.
    /// Frames are ANSI blits of the remote terminal; the parser applies them
    /// like ordinary PTY output. This runs on the app event loop, never in a
    /// render or layout path.
    pub(crate) fn process_remote_frame(&self, bytes: &[u8]) -> bool {
        let PaneRuntimeIo::RemoteProxy {
            response_sink,
            render_notify,
            render_dirty,
            ..
        } = &self.io
        else {
            return false;
        };
        self.content_seq.fetch_add(1, Ordering::AcqRel);
        let result = self
            .terminal
            .process_pty_bytes(self.pane_id, 0, bytes, response_sink);
        self.content_seq.fetch_add(1, Ordering::Release);
        if result.request_render && render_dirty.request_pty(self.pane_id) {
            render_notify.notify_one();
        }
        result.request_render
    }

    pub fn suspend_processes(&mut self) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        self.suppress_pane_died.store(true, Ordering::Release);
        if let Some(handle) = self.detect_handle.take() {
            handle.abort();
        }
        self.io.shutdown();
        shutdown_pane_processes(
            self.pane_id,
            self.child_pid.load(Ordering::Acquire),
            self.child_wait_completed.as_deref(),
        );
        self.preserve_processes_on_drop = true;
        info!(pane = self.pane_id.raw(), "pane processes suspended");
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    pub fn shutdown(mut self) {
        if let Some(handle) = self.detect_handle.take() {
            handle.abort();
        }
        self.io.shutdown();
        shutdown_pane_processes(
            self.pane_id,
            self.child_pid.load(Ordering::Acquire),
            self.child_wait_completed.as_deref(),
        );
        self.preserve_processes_on_drop = true;
    }

    #[cfg(unix)]
    pub fn duplicate_handoff_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        self.io.duplicate_handoff_fd()
    }

    #[cfg(unix)]
    pub fn preserve_for_handoff(mut self) {
        if let Err(err) = self.io.release_after_commit() {
            warn!(
                pane = self.pane_id.raw(),
                err = %err,
                "failed to release PTY actor after handoff commit; dropping runtime will still close the actor handle"
            );
        }
        if let Some(handle) = self.detect_handle.take() {
            handle.abort();
        }
        self.preserve_processes_on_drop = true;
    }

    #[cfg(unix)]
    pub fn assume_handoff_ownership(&mut self) {
        self.preserve_processes_on_drop = false;
    }

    #[cfg(unix)]
    pub fn set_handoff_reader_paused(&self, paused: bool) {
        if let Err(err) = self.io.set_handoff_paused(paused) {
            warn!(
                pane = self.pane_id.raw(),
                err = %err,
                paused,
                "failed to update PTY actor handoff pause state"
            );
        }
    }

    #[cfg(unix)]
    pub fn pause_handoff_reader(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.io.begin_handoff(timeout)
    }

    #[cfg(unix)]
    pub fn handoff_runtime_state(
        &self,
        pane_id: u32,
    ) -> crate::handoff_runtime::HandoffRuntimeState {
        let child_pid = self.child_pid.load(Ordering::Acquire);
        let (rows, cols, cell_width_px, cell_height_px) = self.current_size.get();
        crate::handoff_runtime::HandoffRuntimeState {
            pane_id,
            actor_closed: false,
            child_pid,
            tty_name: self.tty_name.clone(),
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            keyboard_protocol_flags: match self.keyboard_protocol() {
                crate::input::KeyboardProtocol::Legacy => 0,
                crate::input::KeyboardProtocol::Kitty { flags } => flags,
            },
            keyboard_protocol_ansi: self.terminal.kitty_keyboard_state_ansi(),
            input_state: self.input_state(),
            terminal_title: self.terminal_title(),
            initial_history_ansi: None,
            agent_activity: None,
            agent_state: None,
            stall_nudge: None,
            human_draft: None,
            pane_seen: None,
            pane_done_for_ms: None,
        }
    }

    #[cfg(unix)]
    pub fn handoff_history_ansi(&self) -> Option<String> {
        if self.terminal.alternate_screen_active() {
            return None;
        }
        self.snapshot_history().map(|history| {
            truncate_handoff_history(history, crate::server::handoff::MAX_REPLAY_BYTES_PER_PANE)
        })
    }

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        self.terminal.apply_host_terminal_theme(theme);
    }

    pub fn apply_host_terminal_appearance(
        &self,
        appearance: Option<crate::terminal_theme::HostAppearance>,
    ) {
        self.io
            .write_terminal_response(|| self.terminal.apply_host_terminal_appearance(appearance));
    }

    // Runtime construction threads PTY geometry, host context, launch policy, and render hooks.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        Self::spawn_with_initial_history(
            pane_id,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            None,
            events,
            render_notify,
            render_dirty,
        )
    }

    // Runtime construction needs to thread PTY size, environment, theme, and render hooks together.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_with_initial_history(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        initial_history_ansi: Option<&str>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let windows_powershell_prompt_cwd_reporting =
            uses_windows_powershell_pane_shell(shell_config);
        let mut cmd = pane_shell_command_builder(shell_config)?;
        cmd.cwd(cwd);
        apply_pane_terminal_env(&mut cmd);
        apply_pane_launch_env(&mut cmd, launch_env);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn shell",
            SpawnInitialState {
                detected_agent: None,
                history_ansi: initial_history_ansi,
                windows_powershell_prompt_cwd_reporting,
            },
            AgentDetection::Enabled,
        )
    }

    // Runtime construction needs to thread PTY size, environment, theme, and render hooks together.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_shell_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        command: &str,
        launch_env: &PaneLaunchEnv,
        agent_detection: AgentDetection,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let mut cmd = crate::platform::pane_custom_command_pty_builder(command);
        cmd.cwd(cwd);
        apply_pane_terminal_env(&mut cmd);
        apply_pane_launch_env(&mut cmd, launch_env);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn command pane",
            SpawnInitialState::default(),
            agent_detection,
        )
    }

    // Runtime construction needs to thread PTY size, environment, theme, render hooks, and detection policy together.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_argv_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        argv: &[String],
        launch_env: &PaneLaunchEnv,
        agent_detection: AgentDetection,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let Some((program, args)) = argv.split_first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "argv must not be empty",
            ));
        };
        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        apply_pane_terminal_env(&mut cmd);
        apply_pane_launch_env(&mut cmd, launch_env);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn argv command pane",
            SpawnInitialState::default(),
            agent_detection,
        )
    }

    #[cfg(unix)]
    pub fn from_handoff_fd(
        import: crate::handoff_runtime::ImportedHandoffRuntime,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let crate::handoff_runtime::ImportedHandoffRuntime { master_fd, state } = import;
        let Some(master_fd) = master_fd else {
            return Err(std::io::Error::other("closed pane actor has no handoff fd"));
        };
        let (initial_agent, initial_state) = state
            .agent_state
            .as_ref()
            .map(crate::terminal::TerminalAgentHandoffState::handoff_detection_seed)
            .unwrap_or((None, AgentState::Unknown));
        let crate::handoff_runtime::HandoffRuntimeState {
            pane_id,
            actor_closed: _,
            child_pid,
            tty_name,
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            keyboard_protocol_flags,
            keyboard_protocol_ansi,
            input_state,
            terminal_title,
            initial_history_ansi,
            agent_activity: _,
            agent_state: _,
            stall_nudge: _,
            human_draft: _,
            pane_seen: _,
            pane_done_for_ms: _,
        } = state;
        let pane_id = PaneId::from_raw(pane_id);
        use std::os::fd::FromRawFd;

        let master_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(master_fd) };

        let (response_tx, _response_rx) = mpsc::channel::<Bytes>(1);
        let mut terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        terminal
            .resize(cols, rows, cell_width_px, cell_height_px)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if crate::kitty_graphics::is_enabled() {
            terminal
                .enable_kitty_graphics()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        let pane_terminal = GhosttyPaneTerminal::new(terminal, response_tx.clone())?;
        pane_terminal.apply_host_terminal_theme(host_terminal_theme);
        let _ = pane_terminal.apply_host_terminal_appearance(host_terminal_appearance);
        pane_terminal.seed_terminal_title(terminal_title);
        if let Some(input_state) = input_state {
            pane_terminal.seed_handoff_input_state(input_state);
        }
        if let Some(ansi) = keyboard_protocol_ansi.as_deref() {
            pane_terminal.seed_keyboard_protocol_ansi(ansi);
        } else {
            pane_terminal.seed_keyboard_protocol_flags(keyboard_protocol_flags);
        }
        if let Some(ansi) = initial_history_ansi.as_deref() {
            pane_terminal.seed_history_ansi(ansi);
        }
        let terminal = Arc::new(PaneTerminal::new(pane_terminal));
        let child_pid = Arc::new(AtomicU32::new(child_pid));
        let reported_cwd = Arc::new(Mutex::new(None));
        let cwd_process_exited = Arc::new(AtomicBool::new(false));
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(keyboard_protocol_flags));
        let content_seq = Arc::new(AtomicU64::new(0));
        let input_admission = Arc::new(Mutex::new(InputAdmissionState::default()));
        let pending_input_reservations = Arc::new(AtomicU64::new(0));
        let input_delivery_seq = Arc::new(AtomicU64::new(0));
        let reflected_input_seq = Arc::new(AtomicU64::new(0));
        let detection_content_seq = Arc::new(AtomicU64::new(0));
        let link_extraction = Arc::new(crate::agent_state::LinkExtractionGate::default());
        terminal.install_link_extraction(link_extraction.clone());
        let agent_output_seq = Arc::new(AtomicU64::new(0));
        let suppress_pane_died = Arc::new(AtomicBool::new(false));

        let io = {
            let terminal = terminal.clone();
            let response_writer = response_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let content_seq = content_seq.clone();
            let pending_input_for_read = Arc::clone(&pending_input_reservations);
            let input_delivery_seq_for_read = Arc::clone(&input_delivery_seq);
            let reflected_input_seq_for_read = Arc::clone(&reflected_input_seq);
            let detection_content_seq = detection_content_seq.clone();
            let agent_output_seq = agent_output_seq.clone();
            let child_pid = child_pid.clone();
            let read_events = events.clone();
            let reported_cwd = reported_cwd.clone();
            let rt = tokio::runtime::Handle::current();
            let delay_rt = rt.clone();
            let graphics_render_throttle = Arc::new(KittyGraphicsRenderThrottle::default());
            let on_read = Box::new(move |bytes: &[u8]| {
                let input_delivery_seq_at_read_start =
                    input_delivery_seq_for_read.load(Ordering::Acquire);
                let input_was_pending = pending_input_for_read.load(Ordering::Acquire) != 0;
                content_seq.fetch_add(1, Ordering::AcqRel);
                let shell_pid = child_pid.load(Ordering::Acquire);
                let result =
                    terminal.process_pty_bytes(pane_id, shell_pid, bytes, &response_writer);
                content_seq.fetch_add(1, Ordering::Release);
                if !input_was_pending {
                    mark_input_reflected_through(
                        &reflected_input_seq_for_read,
                        input_delivery_seq_at_read_start,
                    );
                }
                publish_terminal_bells(pane_id, result.terminal_bells, &read_events);
                observe_detection_content_change(bytes, &detection_content_seq);
                observe_agent_output(bytes, &agent_output_seq);
                if result.coalesce_graphics_render {
                    if let Some(delay) = result.render_delay {
                        graphics_render_throttle.request(
                            pane_id,
                            delay,
                            &render_notify,
                            &render_dirty,
                            &delay_rt,
                        );
                    }
                } else {
                    if result.request_render && render_dirty.request_pty(pane_id) {
                        render_notify.notify_one();
                    }
                }
                if !result.coalesce_graphics_render {
                    if let Some(delay) = result.render_delay {
                        let render_notify = render_notify.clone();
                        let render_dirty = render_dirty.clone();
                        delay_rt.spawn(async move {
                            tokio::time::sleep(delay).await;
                            if render_dirty.request_pty(pane_id) {
                                render_notify.notify_one();
                            }
                        });
                    }
                }
                if let Some(cwd) = result.reported_cwd.clone() {
                    publish_reported_cwd(pane_id, cwd, &reported_cwd, &read_events);
                }
                for content in result.clipboard_writes {
                    if let Err(err) = read_events.try_send(AppEvent::ClipboardWrite { content }) {
                        warn!(
                            pane = pane_id.raw(),
                            err = %err,
                            "failed to queue OSC 52 clipboard write"
                        );
                    }
                }
                PtyReadResult {
                    terminal_responses: result.terminal_responses,
                }
            });
            let exit_events = events.clone();
            let poison_events = events.clone();
            let suppress_pane_died_on_exit = suppress_pane_died.clone();
            let cwd_process_exited = cwd_process_exited.clone();
            let on_reader_exit = Box::new(move || {
                cwd_process_exited.store(true, Ordering::Release);
                if !suppress_pane_died_on_exit.load(Ordering::Acquire) {
                    let _ = rt.block_on(exit_events.send(AppEvent::PaneDied { pane_id }));
                }
                debug!(pane = pane_id.raw(), "handoff PTY actor exiting");
            });
            PaneRuntimeIo::Actor(PtyIoActor::spawn(PtyIoActorConfig {
                pane_id: pane_id.raw(),
                master_fd,
                initially_quiesced: true,
                on_read,
                on_reader_exit: Some(on_reader_exit),
                on_user_writes_poisoned: Some(Arc::new(move || {
                    if let Err(err) =
                        poison_events.try_send(AppEvent::RemoteControlGatePoisoned { pane_id })
                    {
                        warn!(pane = pane_id.raw(), err = %err, "failed to report poisoned PTY user-write gate");
                    }
                })),
                pending_user_input: Arc::clone(&pending_input_reservations),
            })?)
        };

        let full_lifecycle_authority_active = Arc::new(AtomicBool::new(false));
        let full_lifecycle_hook_baseline_content_seq = Arc::new(AtomicU64::new(0));
        let full_lifecycle_hook_blocked = Arc::new(AtomicBool::new(false));
        let supervisor_stale = Arc::new(AtomicBool::new(false));
        let (detect_handle, detect_reset_notify, detect_screen_rescan_notify, pending_release) =
            spawn_basic_detection_task(
                pane_id,
                child_pid.clone(),
                terminal.clone(),
                detection_content_seq.clone(),
                full_lifecycle_hook_baseline_content_seq.clone(),
                agent_output_seq.clone(),
                link_extraction.clone(),
                DetectionAuthorityMirrors {
                    full_lifecycle_active: full_lifecycle_authority_active.clone(),
                    full_lifecycle_blocked: full_lifecycle_hook_blocked.clone(),
                    supervisor_stale: supervisor_stale.clone(),
                },
                events,
                initial_agent,
                DetectionPublishState {
                    state: initial_state,
                    visible_idle: false,
                    visible_blocker: false,
                    visible_working: false,
                },
            );

        Ok(Self {
            pane_id,
            terminal,
            io,
            current_size: Cell::new((rows, cols, cell_width_px, cell_height_px)),
            #[cfg(test)]
            resize_count: Cell::new(0),
            child_pid,
            tty_name,
            reported_cwd,
            persistence_cwd: Mutex::new(None),
            cwd_process_exited,
            child_wait_completed: None,
            kitty_keyboard_flags,
            content_seq,
            input_admission,
            pending_input_reservations,
            input_delivery_seq,
            reflected_input_seq,
            detection_content_seq,
            full_lifecycle_hook_baseline_content_seq,
            full_lifecycle_authority_active,
            full_lifecycle_hook_blocked,
            supervisor_stale,
            detect_reset_notify,
            detect_screen_rescan_notify,
            pending_release,
            suspended: false,
            suppress_pane_died,
            preserve_processes_on_drop: true,
            detect_handle: Some(detect_handle),
        })
    }

    // Runtime construction needs to thread PTY size, environment, theme, render hooks, and detection policy together.
    #[allow(clippy::too_many_arguments)]
    fn spawn_command_builder(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
        cmd: CommandBuilder,
        spawn_error_message: &'static str,
        initial_state: SpawnInitialState<'_>,
        agent_detection: AgentDetection,
    ) -> std::io::Result<Self> {
        crate::logging::pane_spawn_started(pane_id.raw(), rows, cols, scrollback_limit_bytes);

        let (response_tx, _response_rx) = mpsc::channel::<Bytes>(1);
        let mut terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if crate::kitty_graphics::is_enabled() {
            terminal
                .enable_kitty_graphics()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        let pane_terminal = GhosttyPaneTerminal::new(terminal, response_tx.clone())?;
        pane_terminal.apply_host_terminal_theme(host_terminal_theme);
        let _ = pane_terminal.apply_host_terminal_appearance(host_terminal_appearance);
        pane_terminal.set_windows_powershell_prompt_cwd_reporting(
            initial_state.windows_powershell_prompt_cwd_reporting,
        );
        if let Some(ansi) = initial_state.history_ansi {
            pane_terminal.seed_history_ansi(ansi);
        }
        let terminal = Arc::new(PaneTerminal::new(pane_terminal));
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(0));

        let spawned = crate::pty::backend::spawn_with_portable_pty(rows, cols, cmd)
            .inspect_err(|err| error!(pane = pane_id.raw(), err = %err, "{spawn_error_message}"))?;
        tracing::info!(
            event = "pane.pty_spawn.complete",
            subsystem = "pane",
            outcome = "ok",
            pane_id = pane_id.raw(),
            "pty spawn returned"
        );
        let tty_name = spawned.tty_name;

        // --- Child watcher task ---
        let child_pid = Arc::new(AtomicU32::new(0));
        let reported_cwd = Arc::new(Mutex::new(None));
        let child_wait_completed = Arc::new(AtomicBool::new(false));
        let content_seq = Arc::new(AtomicU64::new(0));
        let input_admission = Arc::new(Mutex::new(InputAdmissionState::default()));
        let pending_input_reservations = Arc::new(AtomicU64::new(0));
        let input_delivery_seq = Arc::new(AtomicU64::new(0));
        let reflected_input_seq = Arc::new(AtomicU64::new(0));
        let detection_content_seq = Arc::new(AtomicU64::new(0));
        let link_extraction = Arc::new(crate::agent_state::LinkExtractionGate::default());
        terminal.install_link_extraction(link_extraction.clone());
        let full_lifecycle_hook_baseline_content_seq = Arc::new(AtomicU64::new(0));
        let agent_output_seq = Arc::new(AtomicU64::new(0));
        let full_lifecycle_authority_active = Arc::new(AtomicBool::new(false));
        let full_lifecycle_hook_blocked = Arc::new(AtomicBool::new(false));
        let supervisor_stale = Arc::new(AtomicBool::new(false));
        let suppress_pane_died = Arc::new(AtomicBool::new(false));
        {
            let child_pid = child_pid.clone();
            let child_wait_completed = child_wait_completed.clone();
            let events = events.clone();
            let suppress_pane_died_on_exit = suppress_pane_died.clone();
            let rt = tokio::runtime::Handle::current();
            let mut child = spawned.child;
            if let Some(pid) = child.process_id() {
                child_pid.store(pid, Ordering::Release);
                crate::logging::pane_spawned(pane_id.raw(), pid);
            }
            tokio::task::spawn_blocking(move || {
                let checkpoint_before_exit = match child.wait() {
                    Ok(status) => {
                        let status_text = format!("{status:?}");
                        crate::logging::pane_exited(pane_id.raw(), &status_text);
                        crate::platform::classify_child_exit(&status).requires_session_checkpoint()
                    }
                    Err(e) => {
                        crate::logging::pane_exit_failed(pane_id.raw(), &e.to_string());
                        false
                    }
                };
                child_wait_completed.store(true, Ordering::Release);
                // Normal exits are critical and must not be dropped. Suspension is
                // handled synchronously by the settlement lifecycle instead.
                if !suppress_pane_died_on_exit.load(Ordering::Acquire) {
                    if checkpoint_before_exit {
                        let _ = rt.block_on(events.send(AppEvent::PaneExitCheckpoint { pane_id }));
                    }
                    if let Err(e) = rt.block_on(events.send(AppEvent::PaneDied { pane_id })) {
                        error!(pane = pane_id.raw(), err = %e, "failed to send PaneDied event");
                    }
                }
            });
        }

        let io = {
            let terminal = terminal.clone();
            let response_writer = response_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let content_seq = content_seq.clone();
            let pending_input_for_read = Arc::clone(&pending_input_reservations);
            let input_delivery_seq_for_read = Arc::clone(&input_delivery_seq);
            let reflected_input_seq_for_read = Arc::clone(&reflected_input_seq);
            let detection_content_seq = detection_content_seq.clone();
            let agent_output_seq = agent_output_seq.clone();
            let first_output = Arc::new(AtomicBool::new(false));
            let first_output_for_read = first_output.clone();
            let child_pid = child_pid.clone();
            let events = events.clone();
            #[cfg(unix)]
            let poison_events = events.clone();
            let reported_cwd = reported_cwd.clone();
            let rt = tokio::runtime::Handle::current();
            let graphics_render_throttle = Arc::new(KittyGraphicsRenderThrottle::default());
            let on_read = Box::new(move |bytes: &[u8]| {
                let input_delivery_seq_at_read_start =
                    input_delivery_seq_for_read.load(Ordering::Acquire);
                let input_was_pending = pending_input_for_read.load(Ordering::Acquire) != 0;
                content_seq.fetch_add(1, Ordering::AcqRel);
                if !bytes.is_empty() && !first_output_for_read.swap(true, Ordering::AcqRel) {
                    crate::logging::pane_first_output(pane_id.raw(), bytes.len());
                }
                let shell_pid = child_pid.load(Ordering::Acquire);
                let result =
                    terminal.process_pty_bytes(pane_id, shell_pid, bytes, &response_writer);
                content_seq.fetch_add(1, Ordering::Release);
                if !input_was_pending {
                    mark_input_reflected_through(
                        &reflected_input_seq_for_read,
                        input_delivery_seq_at_read_start,
                    );
                }
                publish_terminal_bells(pane_id, result.terminal_bells, &events);
                if agent_detection == AgentDetection::Enabled {
                    observe_detection_content_change(bytes, &detection_content_seq);
                    observe_agent_output(bytes, &agent_output_seq);
                }
                let title_requested =
                    result.terminal_title_changed && render_dirty.request_terminal_title(pane_id);
                let render_requested = if result.coalesce_graphics_render {
                    if let Some(delay) = result.render_delay {
                        graphics_render_throttle.request(
                            pane_id,
                            delay,
                            &render_notify,
                            &render_dirty,
                            &rt,
                        );
                    }
                    false
                } else {
                    result.request_render && render_dirty.request_pty(pane_id)
                };
                if title_requested || render_requested {
                    render_notify.notify_one();
                }
                if !result.coalesce_graphics_render {
                    if let Some(delay) = result.render_delay {
                        let render_notify = render_notify.clone();
                        let render_dirty = render_dirty.clone();
                        rt.spawn(async move {
                            tokio::time::sleep(delay).await;
                            if render_dirty.request_pty(pane_id) {
                                render_notify.notify_one();
                            }
                        });
                    }
                }
                if let Some(cwd) = result.reported_cwd.clone() {
                    publish_reported_cwd(pane_id, cwd, &reported_cwd, &events);
                }
                for content in result.clipboard_writes {
                    if let Err(err) = events.try_send(AppEvent::ClipboardWrite { content }) {
                        warn!(
                            pane = pane_id.raw(),
                            err = %err,
                            "failed to send OSC 52 clipboard write"
                        );
                    }
                }
                PtyReadResult {
                    terminal_responses: result.terminal_responses,
                }
            });
            PaneRuntimeIo::Actor(PtyIoActor::spawn(PtyIoActorConfig {
                pane_id: pane_id.raw(),
                #[cfg(unix)]
                master_fd: spawned.master_fd,
                #[cfg(windows)]
                master: spawned.master,
                initially_quiesced: false,
                on_read,
                on_reader_exit: None,
                #[cfg(unix)]
                on_user_writes_poisoned: Some(Arc::new(move || {
                    if let Err(err) =
                        poison_events.try_send(AppEvent::RemoteControlGatePoisoned { pane_id })
                    {
                        warn!(pane = pane_id.raw(), err = %err, "failed to report poisoned PTY user-write gate");
                    }
                })),
                pending_user_input: Arc::clone(&pending_input_reservations),
            })?)
        };

        // --- Detection task ---
        let detection_task = if agent_detection == AgentDetection::Enabled {
            use crate::detect;
            use std::time::{Duration, Instant};

            const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
            const TICK_IDENTIFIED: Duration = Duration::from_millis(300);
            const TICK_PENDING_RELEASE: Duration = Duration::from_millis(50);

            let child_pid = child_pid.clone();
            let terminal = terminal.clone();
            let state_events = events.clone();
            let detection_content_seq = detection_content_seq.clone();
            let link_extraction_for_task = link_extraction.clone();
            let full_lifecycle_hook_baseline_content_seq_for_task =
                full_lifecycle_hook_baseline_content_seq.clone();
            let agent_output_seq = agent_output_seq.clone();
            let full_lifecycle_authority_active_for_task = full_lifecycle_authority_active.clone();
            let supervisor_stale_for_task = supervisor_stale.clone();
            let full_lifecycle_hook_blocked_for_task = full_lifecycle_hook_blocked.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let detect_reset_notify = Arc::new(Notify::new());
            let detect_reset = detect_reset_notify.clone();
            let detect_screen_rescan_notify = Arc::new(Notify::new());
            let detect_screen_rescan = detect_screen_rescan_notify.clone();
            let pending_release = Arc::new(Mutex::new(None));
            let pending_release_for_task = pending_release.clone();

            let handle = tokio::spawn(async move {
                let mut agent_presence =
                    AgentDetectionPresence::from_agent(initial_state.detected_agent);
                let mut state = AgentState::Idle;
                let mut last_visible_idle = initial_state.detected_agent.is_some();
                let mut last_process_check = Instant::now();
                #[cfg(windows)]
                let mut last_observation = (Instant::now(), Some(0));
                let mut last_foreground_pgid = None;
                let mut has_process_probe = false;
                let mut acquisition_started_at = None;
                let mut last_content_change_at = None;
                let mut pending_foreground_shell_clear = false;
                let mut foreground_shell_exit_reported = false;
                let mut release_was_active = false;
                let mut pending_restore_probe = initial_state.detected_agent.is_some();
                let mut last_visible_blocker = false;
                let mut last_visible_working = false;
                let mut last_visible_signal_refresh = None;
                let mut last_detection_text = String::new();
                let mut last_screen_scan_detection_content_seq = None;
                let mut hook_output_retirement = FullLifecycleHookOutputRetirement::default();
                let mut last_output_seq = Some(agent_output_seq.load(Ordering::Relaxed));
                let mut last_output_at = None;
                let mut agent_startup_grace_until = None;
                let mut pending_idle = PendingIdleConfirmation::default();
                let mut force_screen_rescan = false;

                tokio::time::sleep(Duration::from_millis(50)).await;

                loop {
                    let now_for_tick = Instant::now();
                    let tick = if active_pending_release(&pending_release_for_task, now_for_tick)
                        .is_some()
                        || terminal.has_transient_default_color_override()
                    {
                        TICK_PENDING_RELEASE
                    } else if pending_idle.active() {
                        AGENT_PENDING_IDLE_RECHECK
                    } else if agent_presence.current_agent().is_none() {
                        TICK_UNIDENTIFIED
                    } else {
                        TICK_IDENTIFIED
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(tick) => {}
                        _ = detect_reset.notified() => {
                            force_screen_rescan = false;
                            agent_presence = AgentDetectionPresence::from_agent(None);
                            state = AgentState::Unknown;
                            last_visible_idle = false;
                            last_foreground_pgid = None;
                            has_process_probe = false;
                            acquisition_started_at = None;
                            last_content_change_at = None;
                            pending_foreground_shell_clear = false;
                            foreground_shell_exit_reported = false;
                            release_was_active = false;
                            pending_restore_probe = false;
                            last_visible_blocker = false;
                            last_visible_working = false;
                            last_visible_signal_refresh = None;
                            last_detection_text.clear();
                            last_screen_scan_detection_content_seq = None;
                            hook_output_retirement.clear();
                            last_output_at = None;
                            agent_startup_grace_until = None;
                            pending_idle.clear();
                        }
                        _ = detect_screen_rescan.notified() => {
                            last_screen_scan_detection_content_seq = None;
                            force_screen_rescan = true;
                        }
                    }

                    let now = Instant::now();
                    publish_agent_links_if_dirty(pane_id, &link_extraction_for_task, &state_events)
                        .await;
                    let suppressed_agent = active_pending_release(&pending_release_for_task, now);
                    if suppressed_agent.is_none() && release_was_active {
                        has_process_probe = false;
                        acquisition_started_at = None;
                        last_content_change_at = None;
                    }
                    release_was_active = suppressed_agent.is_some();
                    let pid = child_pid.load(Ordering::Acquire);
                    let mut agent = agent_presence.current_agent();
                    let mut lifecycle_authority_active =
                        full_lifecycle_authority_active_for_task.load(Ordering::Acquire);
                    let supervisor_is_stale = supervisor_stale_for_task.load(Ordering::Acquire);
                    let process_probe_input = ProcessProbeInput {
                        current_agent: agent,
                        suppressed_agent,
                        foreground_pgid: last_foreground_pgid,
                        last_foreground_pgid,
                        has_process_probe,
                        acquisition_age: acquisition_started_at
                            .map(|started| now.duration_since(started)),
                        pending_foreground_shell_clear,
                        pending_restore_probe,
                        elapsed_since_process_check: now.duration_since(last_process_check),
                        supervisor_stale: supervisor_is_stale,
                    };
                    #[cfg(windows)]
                    let content_seq = detection_content_seq.load(Ordering::Relaxed);
                    #[cfg(windows)]
                    let last_content_seq = last_observation.1;
                    #[cfg(windows)]
                    let foreground_observation_due = should_observe_foreground_process_group(
                        lifecycle_authority_active,
                        last_content_seq != Some(content_seq)
                            && (last_content_seq.is_some()
                                || now.duration_since(last_observation.0) >= TICK_IDENTIFIED),
                        now.duration_since(last_observation.0),
                        process_probe_input,
                    );
                    #[cfg(not(windows))]
                    let foreground_observation_due = true;
                    let foreground_pgid = match (pid, foreground_observation_due) {
                        (0, _) => None,
                        (_, true) => detect::foreground_process_group_id(pid),
                        _ => last_foreground_pgid,
                    };
                    #[cfg(windows)]
                    if pid > 0 && foreground_observation_due {
                        let retry =
                            last_content_seq.is_some() && last_content_seq != Some(content_seq);
                        last_observation = (now, (!retry).then_some(content_seq));
                    }
                    let process_group_changed =
                        foreground_group_changed(foreground_pgid, last_foreground_pgid);
                    let should_check_process = pid > 0 && {
                        let process_probe_input = ProcessProbeInput {
                            foreground_pgid,
                            ..process_probe_input
                        };
                        !should_skip_process_probe_for_lifecycle_authority(
                            lifecycle_authority_active,
                            process_probe_input,
                        ) && should_probe_foreground_job(process_probe_input)
                    };

                    let mut agent_changed = false;
                    if should_check_process {
                        last_process_check = now;
                        let had_process_probe = has_process_probe;
                        has_process_probe = true;
                        if pid > 0 {
                            let probe = probe_foreground_process(pid, foreground_pgid);
                            let (holds_shell, stale_resolution) =
                                process_sidebar_observation(pid, probe.agent_process_pid);
                            if let Err(error) = state_events
                                .send(AppEvent::PaneProcessStateChanged {
                                    pane_id,
                                    holds_shell,
                                    stale_resolution,
                                    observed_at: now,
                                })
                                .await
                            {
                                warn!(pane = pane_id.raw(), %error, "failed to deliver process state");
                            }
                            let process_name = probe.process_name;
                            let process_group_id = probe.process_group_id;
                            let tracked_process_group_id = process_group_for_change_tracking(
                                foreground_pgid,
                                process_group_id,
                            );
                            let foreground_is_pane_shell = probe.foreground_is_pane_shell;
                            let mut new_agent = probe.agent;

                            if let Some(suppressed_agent) = suppressed_agent {
                                if new_agent == Some(suppressed_agent) {
                                    new_agent = None;
                                } else if let Ok(mut pending_release) =
                                    pending_release_for_task.lock()
                                {
                                    *pending_release = None;
                                }
                            }

                            let previous_agent = agent_presence.current_agent();
                            let foreground_action = foreground_shell_agent_action(
                                previous_agent,
                                new_agent,
                                foreground_is_pane_shell,
                                foreground_shell_exit_reported,
                            );
                            let changed = apply_foreground_shell_agent_action(
                                &mut agent_presence,
                                foreground_action,
                                previous_agent,
                                new_agent,
                                &mut pending_foreground_shell_clear,
                                &mut foreground_shell_exit_reported,
                            );
                            last_foreground_pgid = tracked_process_group_id;
                            if new_agent.is_some() {
                                acquisition_started_at = None;
                                last_content_change_at = None;
                            } else if agent_presence.current_agent().is_none()
                                && had_process_probe
                                && process_group_changed
                            {
                                acquisition_started_at = Some(now);
                            }
                            pending_restore_probe = false;
                            if changed {
                                agent = agent_presence.current_agent();
                                if agent != previous_agent
                                    || foreground_action
                                        == ForegroundShellAgentAction::ReportReplacementProcess
                                {
                                    pending_idle.clear();
                                    last_screen_scan_detection_content_seq = None;
                                    // A replacement agent must not inherit OSC
                                    // evidence from the previous process; a first
                                    // acquisition keeps the evidence its own
                                    // process already emitted.
                                    clear_osc_evidence_for_agent_transition(
                                        &terminal,
                                        previous_agent,
                                    );
                                    if let Some(agent) = agent {
                                        agent_startup_grace_until =
                                            Some(now + AGENT_STARTUP_GRACE_WINDOW);
                                        state = AgentState::Unknown;
                                        last_visible_idle = false;
                                        last_visible_blocker = false;
                                        last_visible_working = false;
                                        last_visible_signal_refresh = None;
                                        publish_agent_process_detected_event(
                                            state_events.clone(),
                                            pane_id,
                                            agent,
                                            now,
                                        )
                                        .await;
                                    } else {
                                        agent_startup_grace_until = None;
                                    }
                                }
                                if let Some(process_name) = process_name {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        process = %process_name,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                } else {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                }
                                agent_changed = true;
                            }
                        }
                    }

                    let pid = child_pid.load(Ordering::Acquire);
                    // Keep the terminal restore side effect separate from render notification state.
                    #[allow(clippy::collapsible_if)]
                    if pid > 0 && terminal.maybe_restore_host_terminal_theme(pane_id, pid) {
                        if render_dirty.request_pty(pane_id) {
                            render_notify.notify_one();
                        }
                    }

                    let process_exited = pending_foreground_shell_clear
                        && agent.is_some()
                        && !foreground_shell_exit_reported;

                    poll_full_lifecycle_hook_retirement(
                        FullLifecycleHookRetirementPorts {
                            pane_id,
                            detection_content_seq: &detection_content_seq,
                            baseline_content_seq:
                                &full_lifecycle_hook_baseline_content_seq_for_task,
                            authority_active: &full_lifecycle_authority_active_for_task,
                            blocked: &full_lifecycle_hook_blocked_for_task,
                            state_events: &state_events,
                        },
                        &mut hook_output_retirement,
                        &mut lifecycle_authority_active,
                        process_exited,
                        now,
                    )
                    .await;

                    if agent_changed || suppressed_agent.is_some() {
                        last_output_at = None;
                    }
                    let recent_output = recent_agent_output(
                        agent,
                        agent_output_seq.load(Ordering::Relaxed),
                        &mut last_output_seq,
                        &mut last_output_at,
                        now,
                    );

                    if should_skip_screen_detection_for_lifecycle_authority(
                        lifecycle_authority_active,
                        full_lifecycle_hook_blocked_for_task.load(Ordering::Acquire),
                        process_exited,
                    ) {
                        pending_idle.clear();
                        continue;
                    }

                    if !force_screen_rescan {
                        if let Some(until) = agent_startup_grace_until {
                            if process_exited {
                                agent_startup_grace_until = None;
                                last_screen_scan_detection_content_seq = None;
                                pending_idle.clear();
                            } else {
                                if now < until {
                                    pending_idle.clear();
                                    continue;
                                }
                                agent_startup_grace_until = None;
                                pending_idle.clear();
                                continue;
                            }
                        }
                    }

                    let current_detection_content_seq = if agent.is_some() {
                        Some(detection_content_seq.load(Ordering::Relaxed))
                    } else {
                        None
                    };
                    match decide_detection_screen_read(DetectionScreenReadInput {
                        state,
                        agent,
                        pending_idle_active: pending_idle.active(),
                        agent_changed,
                        process_exited,
                        current_detection_content_seq,
                        last_screen_scan_detection_content_seq,
                    }) {
                        DetectionScreenReadDecision::Read => {}
                        DetectionScreenReadDecision::Skip => continue,
                    }

                    let content = terminal.detection_text();
                    last_screen_scan_detection_content_seq = current_detection_content_seq;
                    let content_changed = content != last_detection_text;
                    last_detection_text.clone_from(&content);
                    if detect::should_skip_state_update(agent, &content) {
                        pending_idle.clear();
                        force_screen_rescan = false;
                        continue;
                    }
                    sync_content_change_acquisition(
                        agent_presence.current_agent(),
                        suppressed_agent,
                        process_group_changed,
                        content_changed,
                        now,
                        &mut acquisition_started_at,
                        &mut last_content_change_at,
                    );

                    let osc_title = terminal.agent_osc_title();
                    let osc_progress = terminal.agent_osc_progress();
                    let Some(screen_detection) = detection_update_for_publish_with_osc(
                        agent,
                        &content,
                        &osc_title,
                        &osc_progress,
                        process_exited,
                    ) else {
                        pending_idle.clear();
                        continue;
                    };
                    match decide_screen_detection_publish(
                        ScreenDetectionPublishInput {
                            screen_detection,
                            recent_output,
                            current_state: state,
                            last_visible_idle,
                            last_visible_blocker,
                            last_visible_working,
                            last_visible_signal_refresh,
                            process_exited,
                            agent_changed,
                            force_publish: std::mem::take(&mut force_screen_rescan),
                            now,
                        },
                        &mut pending_idle,
                    ) {
                        DetectionPublishDecision::NoPublish => {}
                        DetectionPublishDecision::Publish {
                            state: new_state,
                            visible_idle,
                            visible_blocker,
                            visible_working,
                            usage_limited,
                            process_exited: publish_process_exited,
                        } => {
                            apply_agent_detection_publish_update(
                                state_events.clone(),
                                pane_id,
                                agent,
                                AgentDetectionPublishUpdate {
                                    state: new_state,
                                    visible_idle,
                                    visible_blocker,
                                    visible_working,
                                    usage_limited,
                                    process_exited: publish_process_exited,
                                },
                                now,
                                &mut state,
                                &mut last_visible_idle,
                                &mut last_visible_blocker,
                                &mut last_visible_working,
                                &mut last_visible_signal_refresh,
                                &mut foreground_shell_exit_reported,
                            )
                            .await;
                        }
                    }
                }
            });
            (
                Some(handle.abort_handle()),
                detect_reset_notify,
                detect_screen_rescan_notify,
                pending_release,
            )
        } else {
            (
                None,
                Arc::new(Notify::new()),
                Arc::new(Notify::new()),
                Arc::new(Mutex::new(None)),
            )
        };
        let (detect_handle, detect_reset_notify, detect_screen_rescan_notify, pending_release) =
            detection_task;

        Ok(Self {
            pane_id,
            terminal,
            io,
            current_size: Cell::new((rows, cols, 0, 0)),
            #[cfg(test)]
            resize_count: Cell::new(0),
            child_pid,
            tty_name,
            reported_cwd,
            persistence_cwd: Mutex::new(None),
            cwd_process_exited: child_wait_completed.clone(),
            child_wait_completed: Some(child_wait_completed),
            kitty_keyboard_flags,
            content_seq,
            input_admission,
            pending_input_reservations,
            input_delivery_seq,
            reflected_input_seq,
            detection_content_seq,
            full_lifecycle_hook_baseline_content_seq,
            full_lifecycle_authority_active,
            full_lifecycle_hook_blocked,
            supervisor_stale,
            detect_reset_notify,
            detect_screen_rescan_notify,
            pending_release,
            suspended: false,
            suppress_pane_died,
            preserve_processes_on_drop: false,
            detect_handle,
        })
    }

    pub fn begin_graceful_release(&self, agent: Agent) {
        if let Ok(mut pending_release) = self.pending_release.lock() {
            *pending_release = Some(PendingAgentRelease {
                agent,
                until: std::time::Instant::now() + RELEASE_REACQUIRE_SUPPRESSION,
            });
        }
        self.detect_reset_notify.notify_one();
    }

    pub fn reset_agent_detection(&self) {
        self.detect_reset_notify.notify_one();
    }

    /// Re-evaluate the current detection buffer without forgetting process or
    /// detector state. Used when arbitration changes while terminal bytes stay
    /// unchanged.
    pub fn request_agent_screen_rescan(&self) {
        self.detect_screen_rescan_notify.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn agent_detection_reset_notify_for_test(&self) -> Arc<Notify> {
        self.detect_reset_notify.clone()
    }

    #[cfg(test)]
    pub(crate) fn agent_detection_screen_rescan_notify_for_test(&self) -> Arc<Notify> {
        self.detect_screen_rescan_notify.clone()
    }

    #[cfg(all(test, unix))]
    pub(crate) fn agent_detection_enabled_for_test(&self) -> bool {
        self.detect_handle.is_some()
    }

    pub fn set_full_lifecycle_authority_state(
        &self,
        active: bool,
        output_retirement_eligible: bool,
    ) {
        let previous = self
            .full_lifecycle_authority_active
            .swap(active, Ordering::AcqRel);
        let was_blocked = self
            .full_lifecycle_hook_blocked
            .swap(output_retirement_eligible, Ordering::AcqRel);
        if output_retirement_eligible && !was_blocked {
            self.rebaseline_full_lifecycle_hook_content();
        }
        if previous && !active {
            self.request_agent_screen_rescan();
        }
    }

    /// Mirror the supervisor-stale mark into the detection task.
    ///
    /// While the mark is set the process probe has to keep running even under a
    /// full-lifecycle hook: the hook is the source that went silent, and the
    /// probe is what tells a finished agent apart from a wedged one.
    pub fn set_supervisor_stale(&self, stale: bool) {
        self.supervisor_stale.store(stale, Ordering::Release);
    }

    pub fn rebaseline_hook_authority_output(&self) {
        self.rebaseline_full_lifecycle_hook_content();
    }

    #[cfg(test)]
    pub(crate) fn hook_authority_output_baseline_for_test(&self) -> u64 {
        self.full_lifecycle_hook_baseline_content_seq
            .load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn hook_authority_runtime_state_for_test(&self) -> (bool, bool) {
        (
            self.full_lifecycle_authority_active.load(Ordering::Acquire),
            self.full_lifecycle_hook_blocked.load(Ordering::Acquire),
        )
    }

    /// Declare every content change so far to be *not* a new turn's output.
    ///
    /// Retirement of a blocked turn-end gate only counts detection content
    /// changes after this baseline, so anything herdr itself writes to the pane
    /// has to move the baseline with it.
    fn rebaseline_full_lifecycle_hook_content(&self) {
        self.full_lifecycle_hook_baseline_content_seq.store(
            self.detection_content_seq.load(Ordering::Acquire),
            Ordering::Release,
        );
    }

    pub(crate) fn current_size(&self) -> (u16, u16) {
        let (rows, cols, _, _) = self.current_size.get();
        (rows, cols)
    }

    #[cfg(test)]
    pub(crate) fn test_resize_count(&self) -> usize {
        self.resize_count.get()
    }

    /// Resize if the dimensions actually changed.
    pub fn resize(&self, rows: u16, cols: u16, cell_width_px: u32, cell_height_px: u32) {
        self.resize_inner(rows, cols, cell_width_px, cell_height_px);
    }

    fn resize_inner(&self, rows: u16, cols: u16, cell_width_px: u32, cell_height_px: u32) {
        let rows = rows.max(2);
        let cols = cols.max(4);
        let (_, _, known_cell_width_px, known_cell_height_px) = self.current_size.get();
        let cell_width_px = if cell_width_px > 0 {
            cell_width_px
        } else {
            known_cell_width_px
        };
        let cell_height_px = if cell_height_px > 0 {
            cell_height_px
        } else {
            known_cell_height_px
        };
        let size = (rows, cols, cell_width_px, cell_height_px);
        if self.current_size.get() == size {
            return;
        }
        let mut admission = self
            .input_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        #[cfg(test)]
        self.resize_count
            .set(self.resize_count.get().saturating_add(1));
        self.current_size.set(size);
        let terminal_responses = self
            .terminal
            .resize(rows, cols, cell_width_px, cell_height_px);
        // A resize repaints the whole screen, so the idle scan skip has to be
        // invalidated -- but it is herdr's own write, not the agent resuming a
        // turn, so it must not count toward retiring a blocked turn-end gate.
        // Excluding it here, at the one callsite that is not agent output, keeps
        // that distinction out of the time-based retirement window entirely.
        mark_detection_content_changed(&self.detection_content_seq);
        self.rebaseline_full_lifecycle_hook_content();
        self.io.resize(
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            terminal_responses,
        );
        admission.revision = admission.revision.saturating_add(1);
    }

    #[cfg(unix)]
    pub fn nudge_child_redraw_after_handoff(&self) {
        let (rows, cols, cell_width_px, cell_height_px) = self.current_size.get();
        self.io
            .nudge_child_redraw_after_handoff(rows, cols, cell_width_px, cell_height_px);
    }

    /// Scroll up by N lines (into scrollback history).
    pub fn scroll_up(&self, lines: usize) {
        self.terminal.scroll_up(lines);
    }

    /// Scroll down by N lines (toward live output).
    pub fn scroll_down(&self, lines: usize) {
        self.terminal.scroll_down(lines);
    }

    /// Reset scroll to live view (offset = 0).
    pub fn scroll_reset(&self) {
        self.terminal.scroll_reset();
    }

    /// Set scrollback offset measured from the live bottom of the terminal.
    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        self.terminal.set_scroll_offset_from_bottom(lines);
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.terminal.scroll_metrics()
    }

    pub(crate) fn search_text_matches(
        &self,
        query: &str,
        case_sensitive: bool,
    ) -> Vec<crate::pane::TerminalTextMatch> {
        self.terminal.search_text_matches(query, case_sensitive)
    }

    pub(crate) fn text_match_is_current(&self, text_match: crate::pane::TerminalTextMatch) -> bool {
        self.terminal.text_match_is_current(text_match)
    }

    pub(crate) fn text_matches_are_current(
        &self,
        text_matches: &[crate::pane::TerminalTextMatch],
    ) -> Vec<bool> {
        self.terminal.text_matches_are_current(text_matches)
    }

    pub(crate) fn word_motion_target(
        &self,
        row: u32,
        col: u16,
        motion: crate::pane::TerminalWordMotion,
    ) -> Option<crate::pane::TerminalTextPoint> {
        self.terminal.word_motion_target(row, col, motion)
    }

    #[cfg(any(unix, test))]
    pub fn input_state(&self) -> Option<InputState> {
        #[cfg(test)]
        AGGREGATE_INPUT_STATE_READS.set(AGGREGATE_INPUT_STATE_READS.get() + 1);
        self.terminal.input_state()
    }

    pub fn keyboard_report_all_requested(&self) -> bool {
        self.terminal.keyboard_report_all_requested()
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.terminal.bracketed_paste_enabled()
    }

    pub fn focus_reporting_enabled(&self) -> bool {
        self.terminal.focus_reporting_enabled()
    }

    pub fn mouse_reporting_enabled(&self) -> bool {
        self.terminal.mouse_reporting_enabled()
    }

    pub fn sgr_pixel_mouse_enabled(&self) -> bool {
        self.terminal.sgr_pixel_mouse_enabled()
    }

    pub fn plain_page_keys_use_host_scrollback(&self) -> Option<bool> {
        self.terminal.plain_page_keys_use_host_scrollback()
    }

    pub fn alternate_screen_active(&self) -> bool {
        self.terminal.alternate_screen_active()
    }

    pub fn cursor_state(&self, area: Rect, show_cursor: bool) -> Option<TerminalCursorState> {
        if !show_cursor {
            return None;
        }
        let cursor = self.terminal.cursor_state()?;
        if cursor.x >= area.width || cursor.y >= area.height {
            return None;
        }
        Some(TerminalCursorState {
            x: area.x + cursor.x,
            y: area.y + cursor.y,
            visible: cursor.visible,
            shape: cursor.shape,
        })
    }

    pub fn synchronized_output_active(&self) -> bool {
        self.terminal.synchronized_output_active()
    }

    pub fn content_revision(&self) -> u64 {
        self.terminal.content_revision()
    }

    pub(crate) fn content_seq(&self) -> u64 {
        self.content_seq.load(Ordering::Acquire)
    }

    pub fn visible_text(&self) -> String {
        self.terminal.visible_text()
    }

    pub fn visible_ansi(&self) -> String {
        self.terminal.visible_ansi()
    }

    pub fn detection_text(&self) -> String {
        self.terminal.detection_text()
    }

    pub fn terminal_title(&self) -> Option<String> {
        self.terminal.terminal_title()
    }

    pub fn agent_osc_title(&self) -> String {
        self.terminal.agent_osc_title()
    }

    pub fn agent_osc_progress(&self) -> String {
        self.terminal.agent_osc_progress()
    }

    pub(crate) fn recent_text_snapshot(&self, lines: usize) -> TerminalReadSnapshot {
        self.terminal.recent_text_snapshot(lines)
    }

    pub(crate) fn recent_ansi_snapshot(&self, lines: usize) -> TerminalReadSnapshot {
        self.terminal.recent_ansi_snapshot(lines)
    }

    pub(crate) fn recent_unwrapped_text_snapshot(&self, lines: usize) -> TerminalReadSnapshot {
        self.terminal.recent_unwrapped_text_snapshot(lines)
    }

    pub fn recent_unwrapped_ansi(&self, lines: usize) -> String {
        self.terminal.recent_unwrapped_ansi(lines)
    }

    pub(crate) fn recent_unwrapped_ansi_snapshot(&self, lines: usize) -> TerminalReadSnapshot {
        self.terminal.recent_unwrapped_ansi_snapshot(lines)
    }

    pub fn snapshot_history(&self) -> Option<String> {
        let ansi = self.recent_unwrapped_ansi(usize::MAX);
        (!ansi.trim().is_empty()).then_some(ansi)
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.terminal.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.terminal.render(frame, area, show_cursor);
    }

    pub(crate) fn collect_dirty_patch(
        &self,
        area_width: u16,
        area_height: u16,
    ) -> TerminalDirtyPatchOutcome {
        self.terminal.collect_dirty_patch(area_width, area_height)
    }

    pub fn visible_hyperlinks(&self, area: Rect) -> Vec<((u16, u16), String, String)> {
        self.terminal.visible_hyperlinks(area)
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        needs_data: F,
    ) -> Vec<crate::ghostty::KittyImagePlacement>
    where
        F: FnMut(crate::ghostty::KittyImageDescriptor) -> bool,
    {
        self.terminal
            .kitty_image_placements_with_data_filter(needs_data)
    }

    pub fn keyboard_protocol(&self) -> crate::input::KeyboardProtocol {
        let fallback = crate::input::KeyboardProtocol::from_kitty_flags(
            self.kitty_keyboard_flags.load(Ordering::Relaxed),
        );
        self.terminal.keyboard_protocol(fallback)
    }

    pub fn encode_terminal_key(&self, key: crate::input::TerminalKey) -> Vec<u8> {
        self.terminal
            .encode_terminal_key(key, self.keyboard_protocol())
    }

    pub async fn send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        if self.suspended {
            return Ok(());
        }
        let mut reservation = {
            let mut admission = self
                .input_admission
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Reserve this write before awaiting queue capacity. A conditional
            // sender must treat pending input as intervening input.
            admission.revision = admission.revision.saturating_add(1);
            PendingInputReservation::new(&self.pending_input_reservations, &self.input_delivery_seq)
        };
        let result = self.io.send_bytes(bytes).await;
        if result.is_ok() {
            reservation.mark_delivered();
        }
        result
    }

    pub fn try_send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        if self.suspended {
            return Ok(());
        }
        let mut admission = self
            .input_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = self.io.try_send_bytes(bytes);
        if result.is_ok() {
            admission.revision = admission.revision.saturating_add(1);
            self.input_delivery_seq.fetch_add(1, Ordering::AcqRel);
        }
        result
    }

    pub(crate) fn input_observation(&self) -> Option<InputObservation> {
        if self.is_remote_proxy() {
            return None;
        }
        let admission = self.input_admission.lock().ok()?;
        if self.pending_input_reservations.load(Ordering::Acquire) != 0
            || self.input_delivery_seq.load(Ordering::Acquire)
                != self.reflected_input_seq.load(Ordering::Acquire)
        {
            return None;
        }
        let before = self.content_seq();
        if !before.is_multiple_of(2) {
            return None;
        }
        self.terminal
            .with_detection_text(|text| {
                let content_revision = self.content_revision();
                let after = self.content_seq();
                (before == after).then(|| InputObservation {
                    token: input_observation_token(
                        admission.generation,
                        admission.revision,
                        after,
                        content_revision,
                        text,
                    ),
                    text: text.to_string(),
                })
            })
            .flatten()
    }

    pub(crate) fn try_send_bytes_if_observation(
        &self,
        observation_token: &str,
        bytes: Bytes,
    ) -> ConditionalInputResult {
        if self.suspended || self.is_remote_proxy() {
            return ConditionalInputResult::Failed;
        }
        let mut admission = self
            .input_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.pending_input_reservations.load(Ordering::Acquire) != 0
            || self.input_delivery_seq.load(Ordering::Acquire)
                != self.reflected_input_seq.load(Ordering::Acquire)
        {
            return ConditionalInputResult::ConditionMismatch;
        }
        let before = self.content_seq();
        if !before.is_multiple_of(2) {
            return ConditionalInputResult::ConditionMismatch;
        }
        self.terminal
            .with_detection_text(|text| {
                let content_revision = self.content_revision();
                let after = self.content_seq();
                if before != after
                    || input_observation_token(
                        admission.generation,
                        admission.revision,
                        after,
                        content_revision,
                        text,
                    ) != observation_token
                {
                    return ConditionalInputResult::ConditionMismatch;
                }
                match self.io.try_send_bytes(bytes) {
                    Ok(()) => {
                        admission.revision = admission.revision.saturating_add(1);
                        self.input_delivery_seq.fetch_add(1, Ordering::AcqRel);
                        ConditionalInputResult::Sent
                    }
                    Err(_) => ConditionalInputResult::Failed,
                }
            })
            .unwrap_or(ConditionalInputResult::ConditionMismatch)
    }

    #[cfg(unix)]
    pub(crate) fn try_acquire_remote_owner(&self, owner_id: u64) -> RemoteOwnerAcquireResult {
        self.io.try_acquire_remote_owner(owner_id)
    }

    #[cfg(all(unix, test))]
    pub(crate) fn acquire_remote_owner(&self, owner_id: u64) -> bool {
        self.io.acquire_remote_owner(owner_id)
    }

    #[cfg(unix)]
    pub(crate) fn release_remote_owner(&self, owner_id: u64) {
        self.io.release_remote_owner(owner_id);
    }

    #[cfg(unix)]
    pub(crate) fn try_send_controlled_bytes(
        &self,
        owner_id: u64,
        bytes: &[u8],
    ) -> ControlledWriteResult {
        if self.suspended {
            return ControlledWriteResult::Refused;
        }
        let mut admission = self
            .input_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = self.io.try_send_controlled_bytes(owner_id, bytes);
        if matches!(
            result,
            ControlledWriteResult::Written | ControlledWriteResult::DeliveryUnknown { .. }
        ) {
            admission.revision = admission.revision.saturating_add(1);
            self.input_delivery_seq.fetch_add(1, Ordering::AcqRel);
        }
        result
    }

    pub fn send_bytes_after(&self, bytes: Bytes, delay: std::time::Duration) {
        if self.suspended {
            return;
        }
        let reservation = {
            let mut admission = self
                .input_admission
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Scheduling is already pending input, even before the delayed write
            // reaches the PTY queue.
            admission.revision = admission.revision.saturating_add(1);
            PendingInputReservation::new(&self.pending_input_reservations, &self.input_delivery_seq)
        };
        self.io.send_bytes_after(bytes, delay, reservation);
    }

    pub async fn send_paste(&self, text: String) -> Result<(), mpsc::error::SendError<Bytes>> {
        self.send_bytes(self.paste_payload(text)).await
    }

    pub fn try_send_paste(&self, text: String) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        self.try_send_bytes(self.paste_payload(text))
    }

    fn paste_payload(&self, text: String) -> Bytes {
        let bracketed = self.bracketed_paste_enabled();
        let payload = if bracketed {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text
        };
        Bytes::from(payload)
    }

    pub fn try_send_focus_event(&self, event: crate::ghostty::FocusEvent) -> bool {
        if !self.focus_reporting_enabled() {
            return false;
        }

        let Ok(bytes) = crate::ghostty::encode_focus(event) else {
            return false;
        };
        if let Err(err) = self.try_send_bytes(Bytes::from(bytes)) {
            warn!(err = %err, ?event, "failed to forward pane focus event");
        }
        true
    }

    pub fn wheel_routing(&self) -> Option<WheelRouting> {
        self.terminal.wheel_routing()
    }

    pub(crate) fn screen_text_snapshot(
        &self,
    ) -> Option<(
        crate::ghostty::ActiveScreen,
        u16,
        Vec<crate::ghostty::ScreenTextRow>,
    )> {
        self.terminal.screen_text_snapshot()
    }

    pub fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        position: crate::input::mouse::Position,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if !self.mouse_reporting_enabled() {
            return None;
        }
        self.terminal.encode_mouse_button(kind, position, modifiers)
    }

    pub(crate) fn encode_mouse_motion(
        &self,
        kind: crossterm::event::MouseEventKind,
        position: crate::input::mouse::Position,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.terminal.encode_mouse_motion(kind, position, modifiers)
    }

    pub(crate) fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        position: crate::input::mouse::Position,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if self.wheel_routing()? != WheelRouting::MouseReport {
            return None;
        }
        self.terminal.encode_mouse_wheel(kind, position, modifiers)
    }

    pub(crate) fn pixel_size(&self) -> Option<(u32, u32)> {
        let (rows, cols, cell_width_px, cell_height_px) = self.current_size.get();
        let width = u32::from(cols).checked_mul(cell_width_px)?;
        let height = u32::from(rows).checked_mul(cell_height_px)?;
        (width > 0 && height > 0).then_some((width, height))
    }

    pub fn encode_alternate_scroll(
        &self,
        kind: crossterm::event::MouseEventKind,
    ) -> Option<Vec<u8>> {
        if self.wheel_routing()? != WheelRouting::AlternateScroll {
            return None;
        }
        let key = match kind {
            crossterm::event::MouseEventKind::ScrollUp => crossterm::event::KeyCode::Up,
            crossterm::event::MouseEventKind::ScrollDown => crossterm::event::KeyCode::Down,
            _ => return None,
        };
        Some(self.encode_terminal_key(crate::input::TerminalKey::new(
            key,
            crossterm::event::KeyModifiers::empty(),
        )))
    }

    /// Get the current working directory of the child shell process.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        #[cfg(test)]
        TEST_CWD_QUERY_COUNT.with(|count| count.set(count.get() + 1));

        if let Some(cwd) = self
            .reported_cwd
            .lock()
            .ok()
            .and_then(|reported_cwd| reported_cwd.clone())
        {
            return Some(cwd);
        }

        let pid = self.child_pid.load(Ordering::Relaxed);
        crate::platform::process_cwd(pid)
    }

    pub fn cwd_for_persistence(&self) -> Option<std::path::PathBuf> {
        let pid = self.child_pid.load(Ordering::Acquire);
        let exited = self.cwd_process_exited.load(Ordering::Acquire);
        if let Some(cwd) = (!exited)
            .then(|| crate::platform::process_cwd(pid))
            .flatten()
            .filter(|cwd| cwd.is_absolute())
        {
            // Persistence observations must not change OSC authority or follow-cwd behavior.
            if let Ok(mut known) = self.persistence_cwd.lock() {
                *known = Some(cwd.clone());
            }
            return Some(cwd);
        }
        self.persistence_cwd
            .lock()
            .ok()
            .and_then(|cwd| cwd.clone())
            .or_else(|| self.reported_cwd.lock().ok().and_then(|cwd| cwd.clone()))
    }

    pub fn child_pid(&self) -> Option<u32> {
        let pid = self.child_pid.load(Ordering::Acquire);
        (pid > 0).then_some(pid)
    }

    pub fn tty_name(&self) -> Option<&Path> {
        self.tty_name.as_deref()
    }

    pub fn follow_cwd(&self) -> Option<std::path::PathBuf> {
        #[cfg(unix)]
        {
            // Runtimes without a child process (remote proxies) have no
            // process tree to scan; the reported cwd fallback still applies.
            let leader_cwd = if self.child_pid.load(Ordering::Acquire) == 0 {
                None
            } else {
                self.io
                    .foreground_process_group_id()
                    .and_then(usable_process_cwd)
            };
            leader_cwd.or_else(|| self.cwd())
        }

        #[cfg(not(unix))]
        {
            self.cwd()
        }
    }

    /// Get the current working directory of the process group controlling the pane PTY.
    pub fn foreground_cwd(&self) -> Option<std::path::PathBuf> {
        #[cfg(unix)]
        {
            let pid = self.child_pid.load(Ordering::Acquire);
            if pid == 0 {
                return None;
            }
            let shell_cwd = absolute_process_cwd(pid);
            let foreground_pgid = self
                .io
                .foreground_process_group_id()
                .or_else(|| crate::platform::foreground_process_group_id(pid));
            let leader_cwd = foreground_pgid.and_then(absolute_process_cwd);

            // The group leader's cwd is authoritative (issue #3270): a helper
            // process that chdirs elsewhere inside the same foreground group
            // must not override it. Scan other members only when the leader's
            // cwd cannot be read at all.
            leader_cwd
                .or_else(|| foreground_member_cwd_different_from_shell(pid, shell_cwd.as_ref()))
        }

        #[cfg(not(unix))]
        {
            None
        }
    }
}

#[cfg(test)]
impl PaneRuntime {
    pub(crate) fn test_reset_cwd_query_count() {
        TEST_CWD_QUERY_COUNT.with(|count| count.set(0));
    }

    pub(crate) fn test_cwd_query_count() -> usize {
        TEST_CWD_QUERY_COUNT.with(Cell::get)
    }

    pub(crate) fn test_with_channel(cols: u16, rows: u16) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[], 4)
    }

    pub(crate) fn test_with_channel_capacity(
        cols: u16,
        rows: u16,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[], capacity)
    }

    pub(crate) fn test_with_screen_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Self {
        Self::test_with_scrollback_bytes(cols, rows, 0, bytes)
    }

    #[cfg(unix)]
    pub(crate) fn test_with_live_detection_screen_bytes(
        pane_id: PaneId,
        agent: Agent,
        state: AgentState,
        visible_idle: bool,
        visible_blocker: bool,
        visible_working: bool,
        bytes: &[u8],
    ) -> (Self, mpsc::Receiver<AppEvent>) {
        let (mut runtime, _input_rx) =
            Self::test_with_channel_and_scrollback_bytes(80, 24, 0, bytes, 4);
        runtime.pane_id = pane_id;
        let (state_events, state_rx) = mpsc::channel(16);
        let agent_output_seq = Arc::new(AtomicU64::new(0));
        let (handle, detect_reset_notify, detect_screen_rescan_notify, pending_release) =
            spawn_basic_detection_task(
                pane_id,
                runtime.child_pid.clone(),
                runtime.terminal.clone(),
                runtime.detection_content_seq.clone(),
                runtime.full_lifecycle_hook_baseline_content_seq.clone(),
                agent_output_seq,
                Arc::new(crate::agent_state::LinkExtractionGate::default()),
                DetectionAuthorityMirrors {
                    full_lifecycle_active: runtime.full_lifecycle_authority_active.clone(),
                    full_lifecycle_blocked: runtime.full_lifecycle_hook_blocked.clone(),
                    supervisor_stale: runtime.supervisor_stale.clone(),
                },
                state_events,
                Some(agent),
                DetectionPublishState {
                    state,
                    visible_idle,
                    visible_blocker,
                    visible_working,
                },
            );
        runtime.detect_handle = Some(handle);
        runtime.detect_reset_notify = detect_reset_notify;
        runtime.detect_screen_rescan_notify = detect_screen_rescan_notify;
        runtime.pending_release = pending_release;
        (runtime, state_rx)
    }

    pub(crate) fn test_process_pty_bytes(&self, bytes: &[u8]) {
        let input_delivery_seq_at_read_start = self.input_delivery_seq.load(Ordering::Acquire);
        let input_was_pending = self.pending_input_reservations.load(Ordering::Acquire) != 0;
        self.content_seq.fetch_add(1, Ordering::AcqRel);
        let (tx, _rx) = mpsc::channel(1);
        let _ = self.terminal.process_pty_bytes(self.pane_id, 0, bytes, &tx);
        self.content_seq.fetch_add(1, Ordering::Release);
        if !input_was_pending {
            mark_input_reflected_through(
                &self.reflected_input_seq,
                input_delivery_seq_at_read_start,
            );
        }
    }

    pub(crate) fn test_mark_detection_content_changed(&self) {
        mark_detection_content_changed(&self.detection_content_seq);
    }

    pub(crate) fn test_with_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> Self {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, scrollback_limit_bytes, bytes, 4).0
    }

    pub(crate) fn test_with_channel_and_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
        channel_capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(channel_capacity);
        let (resize_tx, _resize_rx) = watch::channel((rows, cols, 0, 0));
        let mut terminal =
            crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes).unwrap();
        terminal.write(bytes);

        (
            Self {
                pane_id: PaneId::from_raw(0),
                terminal: Arc::new(PaneTerminal::new(
                    GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
                )),
                io: PaneRuntimeIo::TestChannel {
                    sender: tx,
                    resize_tx,
                    remote_owner: Arc::new(Mutex::new(None)),
                },
                current_size: Cell::new((rows, cols, 0, 0)),
                resize_count: Cell::new(0),
                child_pid: Arc::new(AtomicU32::new(0)),
                tty_name: None,
                reported_cwd: Arc::new(Mutex::new(None)),
                persistence_cwd: Mutex::new(None),
                cwd_process_exited: Arc::new(AtomicBool::new(false)),
                child_wait_completed: None,
                kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
                content_seq: Arc::new(AtomicU64::new(0)),
                input_admission: Arc::new(Mutex::new(InputAdmissionState::default())),
                pending_input_reservations: Arc::new(AtomicU64::new(0)),
                input_delivery_seq: Arc::new(AtomicU64::new(0)),
                reflected_input_seq: Arc::new(AtomicU64::new(0)),
                detection_content_seq: Arc::new(AtomicU64::new(0)),
                full_lifecycle_hook_baseline_content_seq: Arc::new(AtomicU64::new(0)),
                full_lifecycle_authority_active: Arc::new(AtomicBool::new(false)),
                supervisor_stale: Arc::new(AtomicBool::new(false)),
                full_lifecycle_hook_blocked: Arc::new(AtomicBool::new(false)),
                detect_reset_notify: Arc::new(Notify::new()),
                detect_screen_rescan_notify: Arc::new(Notify::new()),
                pending_release: Arc::new(Mutex::new(None)),
                suspended: false,
                suppress_pane_died: Arc::new(AtomicBool::new(false)),
                preserve_processes_on_drop: true,
                detect_handle: Some(tokio::spawn(async {}).abort_handle()),
            },
            rx,
        )
    }
}

#[cfg(test)]
mod tests {
    use self::agent_detection::FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE;
    use super::*;

    #[tokio::test]
    async fn kitty_graphics_burst_schedules_only_leading_and_latest_renders() {
        const FRAME_COUNT: usize = 64;
        let pane_id = PaneId::from_raw(42);
        let delay = Duration::from_millis(20);
        let notify = Arc::new(Notify::new());
        let dirty = Arc::new(RenderSignal::new());
        let throttle = Arc::new(KittyGraphicsRenderThrottle::default());
        let runtime = tokio::runtime::Handle::current();

        for _ in 0..FRAME_COUNT {
            throttle.request(pane_id, delay, &notify, &dirty, &runtime);
        }

        tokio::time::timeout(Duration::from_millis(250), notify.notified())
            .await
            .expect("leading frame render");
        assert_eq!(
            dirty.take().pty_sources,
            std::collections::HashSet::from([pane_id])
        );

        tokio::time::timeout(Duration::from_millis(250), notify.notified())
            .await
            .expect("newest frame render");
        assert_eq!(
            dirty.take().pty_sources,
            std::collections::HashSet::from([pane_id])
        );

        assert!(
            tokio::time::timeout(delay.saturating_mul(2), notify.notified())
                .await
                .is_err(),
            "stale frames must not queue more renders"
        );
        assert!(!dirty.is_pending());
    }

    fn detected_urls(
        links: &[crate::agent_state::DetectedAgentLink],
        source: crate::agent_state::AgentLinkSource,
    ) -> Vec<&str> {
        links
            .iter()
            .filter(|link| link.source == source)
            .map(|link| link.url.as_str())
            .collect()
    }

    // PTY parse throughput is a multiplicative path: bytes x panes. The gate
    // adds classification to it, so measure the parse loop itself rather than
    // inferring it from render scaling.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "manual PTY parse scaling profile"]
    async fn parse_scale_profile() {
        const WARMUP: usize = 3;
        const SAMPLES: usize = 21;

        fn chunk(osc8_heavy: bool) -> Vec<u8> {
            let mut out = Vec::new();
            for index in 0..64 {
                if osc8_heavy {
                    out.extend_from_slice(
                        format!(
                            "\x1b]8;;https://example.test/file-{index}\x1b\\file-{index}\x1b]8;;\x1b\\  "
                        )
                        .as_bytes(),
                    );
                } else {
                    out.extend_from_slice(
                        format!("line {index} plain output without any link at all\r\n").as_bytes(),
                    );
                }
            }
            out.push(b'\n');
            out
        }

        fn profile(panes: usize, with_gate: bool, osc8_heavy: bool) -> u128 {
            let runtimes: Vec<_> = (0..panes)
                .map(|_| {
                    let runtime = PaneRuntime::test_with_screen_bytes(120, 24, b"");
                    if with_gate {
                        let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
                        runtime.terminal.install_link_extraction(gate);
                    }
                    runtime
                })
                .collect();
            let bytes = chunk(osc8_heavy);

            for _ in 0..WARMUP {
                for runtime in &runtimes {
                    runtime.test_process_pty_bytes(&bytes);
                }
            }
            let mut samples = Vec::with_capacity(SAMPLES);
            for _ in 0..SAMPLES {
                let started = std::time::Instant::now();
                for runtime in &runtimes {
                    runtime.test_process_pty_bytes(&bytes);
                }
                samples.push(started.elapsed().as_micros());
            }
            samples.sort_unstable();
            samples[SAMPLES / 2]
        }

        for (label, osc8_heavy) in [("plain output", false), ("OSC 8 per item", true)] {
            println!("{label}: median microseconds per batch across all panes");
            println!("     panes  no_gate  with_gate  gate_cost");
            for panes in [1usize, 15, 50] {
                let without = profile(panes, false, osc8_heavy);
                let with = profile(panes, true, osc8_heavy);
                println!(
                    "{panes:>10}  {without:>7}  {with:>9}  {:>8.2}x",
                    with as f64 / without.max(1) as f64
                );
            }
        }
    }

    #[tokio::test]
    async fn screen_motion_between_runs_does_not_join_one_url() {
        // Two runs the screen never showed as one link must not be published
        // as one. Cursor motion, scrolling, erasure and margin changes all
        // break adjacency.
        for (label, motion) in [
            ("CUP", b"\x1b[10;40H".as_slice()),
            ("CUF", b"\x1b[20C".as_slice()),
            ("CUD", b"\x1b[3B".as_slice()),
            ("CUU", b"\x1b[2A".as_slice()),
            ("SU", b"\x1b[1S".as_slice()),
            ("SD", b"\x1b[1T".as_slice()),
            ("IL", b"\x1b[1L".as_slice()),
            ("DL", b"\x1b[1M".as_slice()),
            ("ED", b"\x1b[J".as_slice()),
            ("EL", b"\x1b[K".as_slice()),
            ("DECSTBM", b"\x1b[1;5r".as_slice()),
        ] {
            let mut stream = b"\x1b[12;1Hhttps://shown.example.test/path".to_vec();
            stream.extend_from_slice(motion);
            stream.extend_from_slice(b"@attacker.example.test\n");

            let runtime = PaneRuntime::test_with_screen_bytes(120, 24, b"");
            let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
            runtime.terminal.install_link_extraction(gate.clone());
            runtime.test_process_pty_bytes(&stream);

            let links = gate
                .take_links()
                .unwrap_or_else(|| panic!("visible URL before {label}"));
            assert_eq!(
                links.output_urls,
                vec!["https://shown.example.test/path"],
                "{label} moved rendered text, so the runs must stay separate"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_osc_capture_matches_ghostty_visible_output() {
        let uri = "https://after-cancel.example.test/path";
        for (name, cancellation) in [
            ("CAN", b"\x18".as_slice()),
            ("SUB", b"\x1a".as_slice()),
            ("ESC", b"\x1bc".as_slice()),
        ] {
            let mut stream = b"\x1b]8;;https://cancelled.example.test/hidden".to_vec();
            stream.extend_from_slice(cancellation);
            stream.extend_from_slice(uri.as_bytes());
            stream.push(b'\n');

            let runtime = PaneRuntime::test_with_screen_bytes(120, 24, b"");
            let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
            runtime.terminal.install_link_extraction(gate.clone());
            runtime.test_process_pty_bytes(&stream);
            assert!(runtime.visible_text().contains(uri), "Ghostty {name}");

            let links = gate.take_links().expect("visible URL after cancelled OSC");
            assert_eq!(links.output_urls, vec![uri], "link gate {name}");
            assert!(links.osc8_urls.is_empty(), "link gate {name}");
        }
    }

    fn assert_parser_classified_link_capture(
        label: &str,
        stream: &[u8],
        hidden: &str,
        visible: &str,
        rendered_fragment: Option<&str>,
    ) {
        for split in 0..=stream.len() {
            let runtime = PaneRuntime::test_with_screen_bytes(160, 24, b"");
            let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
            runtime.terminal.install_link_extraction(gate.clone());
            runtime.test_process_pty_bytes(&stream[..split]);
            runtime.test_process_pty_bytes(&stream[split..]);

            let rendered = runtime.visible_text();
            assert!(
                !rendered.contains(hidden),
                "Ghostty hid {label} payload at split {split}; rendered={rendered:?}"
            );
            if let Some(fragment) = rendered_fragment {
                assert!(
                    rendered.contains(fragment),
                    "Ghostty rendered the post-control fragment for {label} at split {split}; rendered={rendered:?}"
                );
            }
            assert!(
                rendered.contains(visible),
                "Ghostty rendered the visible URL after {label} at split {split}; rendered={rendered:?}"
            );

            let links = gate
                .take_links()
                .unwrap_or_else(|| panic!("visible URL after {label} at split {split}"));
            let mut expected_urls = vec![visible];
            if let Some(fragment) = rendered_fragment {
                expected_urls.push(fragment);
            }
            expected_urls.sort_unstable();
            assert_eq!(
                links.output_urls, expected_urls,
                "link capture must match Ghostty-rendered text for {label} at split {split}"
            );
            assert!(links.osc8_urls.is_empty(), "{label} at split {split}");
        }
    }

    #[tokio::test]
    async fn c1_introducers_and_st_match_ghostty_rendered_text() {
        for (label, introducer, slug) in [
            ("SOS", 0x98, "sos"),
            ("PM", 0x9e, "pm"),
            ("APC", 0x9f, "apc"),
        ] {
            let hidden = format!("https://hidden-{slug}.example.test/path");
            let visible = format!("https://visible-{slug}.example.test/path");
            let mut stream = vec![b'\x1b', introducer];
            stream.extend_from_slice(hidden.as_bytes());
            stream.push(0x9c);
            stream.extend_from_slice(visible.as_bytes());
            stream.push(b'\n');

            assert_parser_classified_link_capture(label, &stream, &hidden, &visible, None);
        }

        for (label, prefix, terminator) in [
            (
                "DCS payload 0x9C",
                b"\x1bPq".as_slice(),
                b"\x1b\\".as_slice(),
            ),
            (
                "OSC payload 0x9C",
                b"\x1b]2;".as_slice(),
                b"\x07".as_slice(),
            ),
        ] {
            let hidden = "https://hidden-after-9c.example.test/path";
            let visible = "https://visible-after-string.example.test/path";
            let mut stream = prefix.to_vec();
            stream.push(0x9c);
            stream.extend_from_slice(hidden.as_bytes());
            stream.extend_from_slice(terminator);
            stream.extend_from_slice(format!("{visible}\n").as_bytes());
            assert_parser_classified_link_capture(label, &stream, hidden, visible, None);
        }
    }

    #[tokio::test]
    async fn escape_followed_by_c0_matches_ghostty_rendered_text() {
        let hidden = "https://hidden-escape-control.example.test/path";
        let rendered_fragment = "ttps://hidden-escape-control.example.test/path";
        let visible = "https://visible-escape-control.example.test/path";
        let stream = format!("\x1b\x07{hidden} {visible}\n");

        assert_parser_classified_link_capture(
            "ESC plus BEL",
            stream.as_bytes(),
            hidden,
            visible,
            Some(rendered_fragment),
        );
    }

    #[tokio::test]
    async fn bell_inside_url_matches_ghostty_rendered_text() {
        let visible = "https://example.com/path";
        for (label, control) in [("BEL", 0x07), ("ENQ", 0x05)] {
            let mut stream = b"https://exam".to_vec();
            stream.push(control);
            stream.extend_from_slice(b"ple.com/path\n");

            for split in 0..=stream.len() {
                let runtime = PaneRuntime::test_with_screen_bytes(160, 24, b"");
                let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
                runtime.terminal.install_link_extraction(gate.clone());
                runtime.test_process_pty_bytes(&stream[..split]);
                runtime.test_process_pty_bytes(&stream[split..]);

                let rendered = runtime.visible_text();
                assert!(
                    rendered.contains(visible),
                    "Ghostty rendered one continuous URL across {label} at split {split}; rendered={rendered:?}"
                );
                let links = gate
                    .take_links()
                    .unwrap_or_else(|| panic!("visible URL across {label} at split {split}"));
                assert_eq!(links.output_urls, vec![visible], "{label} split {split}");
                assert!(links.osc8_urls.is_empty(), "{label} split {split}");
            }
        }
    }

    #[tokio::test]
    async fn dec_special_charset_matches_ghostty_rendered_text() {
        let mapped = "https://mapped.example.test/path";
        let visible = "https://visible-after-reset.example.test/path";
        let stream = format!("\x1b(0{mapped}\x1b(B {visible}\n");

        for split in 0..=stream.len() {
            let runtime = PaneRuntime::test_with_screen_bytes(160, 24, b"");
            let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
            runtime.terminal.install_link_extraction(gate.clone());
            runtime.test_process_pty_bytes(&stream.as_bytes()[..split]);
            runtime.test_process_pty_bytes(&stream.as_bytes()[split..]);

            let rendered = runtime.visible_text();
            assert!(
                !rendered.contains(mapped),
                "Ghostty mapped DEC-special text at split {split}; rendered={rendered:?}"
            );
            assert!(
                rendered.contains(visible),
                "Ghostty rendered text after ASCII reset at split {split}; rendered={rendered:?}"
            );
            let links = gate
                .take_links()
                .unwrap_or_else(|| panic!("visible URL after charset reset at split {split}"));
            assert_eq!(links.output_urls, vec![visible], "split {split}");
            assert!(links.osc8_urls.is_empty(), "split {split}");
        }
    }

    #[tokio::test]
    async fn status_display_text_matches_ghostty_rendered_text() {
        let hidden = "https://hidden-status-display.example.test/path";
        let visible = "https://visible-main-display.example.test/path";
        let stream = format!("\x1b[1$}}{hidden}\x1b[0$}}{visible}\n");

        assert_parser_classified_link_capture(
            "status display",
            stream.as_bytes(),
            hidden,
            visible,
            None,
        );
    }

    #[tokio::test]
    async fn ignored_zero_width_text_matches_ghostty_rendered_text() {
        let rendered_uri = "https://zero-width.example.test/path";
        let stream = "https://zero-\u{200b}width.example.test/path\n";

        for split in 0..=stream.len() {
            let runtime = PaneRuntime::test_with_screen_bytes(160, 24, b"");
            let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
            runtime.terminal.install_link_extraction(gate.clone());
            runtime.test_process_pty_bytes(&stream.as_bytes()[..split]);
            runtime.test_process_pty_bytes(&stream.as_bytes()[split..]);

            let rendered = runtime.visible_text();
            assert!(
                rendered.contains(rendered_uri),
                "Ghostty omitted the zero-width codepoint at split {split}; rendered={rendered:?}"
            );
            assert!(
                !rendered.contains('\u{200b}'),
                "Ghostty rendered the zero-width codepoint at split {split}; rendered={rendered:?}"
            );

            let links = gate
                .take_links()
                .unwrap_or_else(|| panic!("rendered URL at split {split}"));
            assert_eq!(
                links.output_urls,
                vec![rendered_uri],
                "link capture must match Ghostty-rendered text at split {split}"
            );
        }
    }

    #[tokio::test]
    async fn dirty_link_snapshot_includes_osc8_hyperlink_target() {
        let uri = "https://osc.example.test/target";
        let screen = format!("\x1b]8;;{uri}\x1b\\label\x1b]8;;\x1b\\");
        let runtime = PaneRuntime::test_with_screen_bytes(80, 24, b"");
        let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
        runtime.terminal.install_link_extraction(gate.clone());
        runtime.test_process_pty_bytes(screen.as_bytes());
        let (tx, mut rx) = mpsc::channel(1);

        publish_agent_links_if_dirty(PaneId::from_raw(71), &gate, &tx).await;

        let event = rx.recv().await.expect("link event");
        let AppEvent::OrderedAgentLinksDetected { links, .. } = event else {
            panic!("expected agent link event");
        };
        assert!(detected_urls(&links, crate::agent_state::AgentLinkSource::Output).is_empty());
        assert_eq!(
            detected_urls(&links, crate::agent_state::AgentLinkSource::Osc8),
            vec![uri]
        );
    }

    #[tokio::test]
    async fn dirty_link_snapshot_keeps_url_before_eighty_following_lines() {
        let uri = "https://scrollback.example.test/kept";
        let mut screen = format!("{uri}\r\n");
        for index in 0..80 {
            screen.push_str(&format!("ordinary line {index}\r\n"));
        }
        let _runtime =
            PaneRuntime::test_with_scrollback_bytes(80, 24, 64 * 1024, screen.as_bytes());
        let gate = crate::agent_state::LinkExtractionGate::default();
        gate.observe_chunk(format!("{uri}\n").as_bytes());
        let (tx, mut rx) = mpsc::channel(1);

        publish_agent_links_if_dirty(PaneId::from_raw(72), &gate, &tx).await;

        let event = rx.recv().await.expect("link event");
        let AppEvent::OrderedAgentLinksDetected { links, .. } = event else {
            panic!("expected agent link event");
        };
        assert!(detected_urls(&links, crate::agent_state::AgentLinkSource::Output).contains(&uri));
    }

    #[tokio::test]
    async fn link_capture_does_not_restamp_urls_from_earlier_output() {
        let pane_id = PaneId::from_raw(73);
        let first_uri = "https://first.example.test/a";
        let second_uri = "https://second.example.test/b";
        let runtime = PaneRuntime::test_with_scrollback_bytes(
            80,
            24,
            64 * 1024,
            format!("{first_uri}\r\n").as_bytes(),
        );
        let gate = crate::agent_state::LinkExtractionGate::default();
        let (tx, mut rx) = mpsc::channel(2);
        gate.observe_chunk(format!("{first_uri}\n").as_bytes());
        publish_agent_links_if_dirty(pane_id, &gate, &tx).await;
        let first_event = rx.recv().await.expect("first link event");

        runtime.test_process_pty_bytes(format!("{second_uri}\r\n").as_bytes());
        gate.observe_chunk(format!("{second_uri}\n").as_bytes());
        publish_agent_links_if_dirty(pane_id, &gate, &tx).await;
        let second_event = rx.recv().await.expect("second link event");

        let mut store = crate::agent_state::AgentStateStore::default();
        for event in [first_event, second_event] {
            let AppEvent::OrderedAgentLinksDetected {
                links, observed_at, ..
            } = event
            else {
                panic!("expected agent link event");
            };
            for link in links {
                store.observe_links(pane_id, [link.url], link.source, observed_at);
            }
        }
        let links = store
            .snapshot(pane_id, crate::api::schema::AgentStatus::Idle)
            .links;
        let first = links
            .iter()
            .find(|link| link.url == first_uri)
            .expect("first URL retained");
        assert_eq!(first.first_seen, first.last_seen);
    }

    #[tokio::test]
    async fn link_capture_keeps_url_before_two_hundred_following_lines_in_one_chunk() {
        let uri = "https://burst.example.test/lost";
        let mut output = format!("{uri}\r\n");
        for index in 1..=200 {
            output.push_str(&format!("{index}\r\n"));
        }
        let runtime = PaneRuntime::test_with_scrollback_bytes(80, 24, 64 * 1024, b"");
        let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
        runtime.terminal.install_link_extraction(gate.clone());
        runtime.test_process_pty_bytes(output.as_bytes());
        let (tx, mut rx) = mpsc::channel(1);

        publish_agent_links_if_dirty(PaneId::from_raw(74), &gate, &tx).await;

        let AppEvent::OrderedAgentLinksDetected { links, .. } =
            rx.recv().await.expect("link event")
        else {
            panic!("expected agent link event");
        };
        assert_eq!(
            detected_urls(&links, crate::agent_state::AgentLinkSource::Output),
            vec![uri]
        );
    }

    #[tokio::test]
    async fn link_capture_joins_soft_wrapped_url() {
        let uri = "https://wrapped.example.test/a/path/that/is/wider/than/the/pane";
        let output = format!("{uri}\r\n");
        let _runtime =
            PaneRuntime::test_with_scrollback_bytes(20, 24, 64 * 1024, output.as_bytes());
        let gate = crate::agent_state::LinkExtractionGate::default();
        gate.observe_chunk(output.as_bytes());
        let (tx, mut rx) = mpsc::channel(1);

        publish_agent_links_if_dirty(PaneId::from_raw(75), &gate, &tx).await;

        let AppEvent::OrderedAgentLinksDetected { links, .. } =
            rx.recv().await.expect("link event")
        else {
            panic!("expected agent link event");
        };
        assert_eq!(
            detected_urls(&links, crate::agent_state::AgentLinkSource::Output),
            vec![uri]
        );
    }

    #[tokio::test]
    async fn hidden_osc8_link_capture_does_not_depend_on_visible_hyperlinks() {
        let uri = "https://hidden.example.test/target";
        let mut output = format!("\x1b]8;;{uri}\x1b\\label\x1b]8;;\x1b\\\r\n");
        for index in 1..=200 {
            output.push_str(&format!("{index}\r\n"));
        }
        let runtime = PaneRuntime::test_with_scrollback_bytes(80, 24, 64 * 1024, b"");
        let gate = Arc::new(crate::agent_state::LinkExtractionGate::default());
        runtime.terminal.install_link_extraction(gate.clone());
        runtime.test_process_pty_bytes(output.as_bytes());
        let (tx, mut rx) = mpsc::channel(1);

        publish_agent_links_if_dirty(PaneId::from_raw(76), &gate, &tx).await;

        let AppEvent::OrderedAgentLinksDetected { links, .. } =
            rx.recv().await.expect("link event")
        else {
            panic!("expected agent link event");
        };
        assert!(detected_urls(&links, crate::agent_state::AgentLinkSource::Output).is_empty());
        assert_eq!(
            detected_urls(&links, crate::agent_state::AgentLinkSource::Osc8),
            vec![uri]
        );
    }

    #[tokio::test]
    async fn link_capture_publishes_url_with_scheme_split_across_chunks() {
        let uri = "https://split.example.test/path";
        let gate = crate::agent_state::LinkExtractionGate::default();
        gate.observe_chunk(b"https:");
        gate.observe_chunk(b"//split.example.test/path\r\n");
        let (tx, mut rx) = mpsc::channel(1);

        publish_agent_links_if_dirty(PaneId::from_raw(77), &gate, &tx).await;

        let AppEvent::OrderedAgentLinksDetected { links, .. } =
            rx.recv().await.expect("link event")
        else {
            panic!("expected agent link event");
        };
        assert_eq!(
            detected_urls(&links, crate::agent_state::AgentLinkSource::Output),
            vec![uri]
        );
    }

    #[tokio::test]
    async fn conditional_input_token_sends_once_and_replay_sends_nothing() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        let observation = runtime.input_observation().expect("coherent observation");

        assert_eq!(
            runtime
                .try_send_bytes_if_observation(&observation.token, Bytes::from_static(b"answer")),
            ConditionalInputResult::Sent
        );
        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"answer")));

        assert_eq!(
            runtime
                .try_send_bytes_if_observation(&observation.token, Bytes::from_static(b"replay")),
            ConditionalInputResult::ConditionMismatch
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pending_async_input_invalidates_a_conditional_input_token() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        let observation = runtime.input_observation().expect("coherent observation");

        runtime
            .send_bytes(Bytes::from_static(b"human"))
            .await
            .expect("ordinary input");
        assert_eq!(
            runtime.try_send_bytes_if_observation(
                &observation.token,
                Bytes::from_static(b"automation")
            ),
            ConditionalInputResult::ConditionMismatch
        );
        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"human")));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn delayed_input_invalidates_a_conditional_input_token_before_delivery() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        let observation = runtime.input_observation().expect("coherent observation");

        runtime.send_bytes_after(Bytes::from_static(b"later"), Duration::from_secs(60));
        assert_eq!(
            runtime.try_send_bytes_if_observation(
                &observation.token,
                Bytes::from_static(b"automation")
            ),
            ConditionalInputResult::ConditionMismatch
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn delayed_input_prevents_a_new_observation_while_pending() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);

        runtime.send_bytes_after(Bytes::from_static(b"later"), Duration::from_secs(60));

        assert!(runtime.input_observation().is_none());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn capacity_blocked_input_prevents_a_new_observation_until_settled() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel_capacity(80, 24, 1);
        let PaneRuntimeIo::TestChannel { sender, .. } = &runtime.io else {
            panic!("test runtime must use a channel");
        };
        sender
            .try_send(Bytes::from_static(b"fill"))
            .expect("fill input queue");

        let send = runtime.send_bytes(Bytes::from_static(b"waiting"));
        tokio::pin!(send);
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut send)
            .await
            .is_err());
        assert!(runtime.input_observation().is_none());

        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"fill")));
        send.await.expect("pending send settles");
        assert!(runtime.input_observation().is_none());
        runtime.test_process_pty_bytes(b"reflected");
        assert!(runtime.input_observation().is_some());
        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"waiting")));
    }

    #[tokio::test]
    async fn observation_token_is_not_reusable_across_runtimes() {
        let (first, _first_rx) = PaneRuntime::test_with_channel(80, 24);
        let observation = first.input_observation().expect("first observation");
        let (second, mut second_rx) = PaneRuntime::test_with_channel(80, 24);

        assert_eq!(
            second.try_send_bytes_if_observation(
                &observation.token,
                Bytes::from_static(b"automation")
            ),
            ConditionalInputResult::ConditionMismatch
        );
        assert!(second_rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn controlled_input_invalidates_an_observation_before_writing() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        let observation = runtime.input_observation().expect("coherent observation");
        assert!(runtime.acquire_remote_owner(7));

        assert_eq!(
            runtime.try_send_controlled_bytes(7, b"controlled"),
            ControlledWriteResult::Written
        );
        assert_eq!(
            runtime.try_send_bytes_if_observation(
                &observation.token,
                Bytes::from_static(b"automation")
            ),
            ConditionalInputResult::ConditionMismatch
        );
        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"controlled")));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn stale_output_completion_cannot_reflect_newer_input() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        let input_sequence_when_output_started = runtime.input_delivery_seq.load(Ordering::Acquire);

        runtime
            .try_send_bytes(Bytes::from_static(b"newer-input"))
            .expect("input queues");
        mark_input_reflected_through(
            &runtime.reflected_input_seq,
            input_sequence_when_output_started,
        );

        assert!(runtime.input_observation().is_none());
        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"newer-input")));
    }

    #[tokio::test]
    async fn terminal_resize_invalidates_a_conditional_input_token() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        let observation = runtime.input_observation().expect("coherent observation");

        runtime.resize(30, 100, 0, 0);
        assert_eq!(
            runtime.try_send_bytes_if_observation(
                &observation.token,
                Bytes::from_static(b"automation")
            ),
            ConditionalInputResult::ConditionMismatch
        );
        assert!(rx.try_recv().is_err());
    }
    use std::time::Duration;

    #[test]
    fn proxy_resize_queues_a_marker_only_when_geometry_changed() {
        let (runtime, mut channels) = PaneRuntime::spawn_remote_proxy(
            PaneId::alloc(),
            24,
            80,
            0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");

        // The Hello handshake carries the spawn geometry, so an unchanged
        // resize does not queue a marker.
        runtime.resize(24, 80, 0, 0);
        assert!(
            channels.outbound_rx.try_recv().is_err(),
            "an unchanged proxy geometry must not touch the wire"
        );

        // A geometry change queues exactly one marker and updates the slot
        // the writer reads.
        runtime.resize(30, 100, 9, 18);
        assert_eq!(
            channels.outbound_rx.try_recv(),
            Ok(ProxyOutbound::SyncResize)
        );
        assert_eq!(
            *channels.resize_slot.lock().expect("resize slot lock"),
            (30, 100, 9, 18)
        );

        // Steady state stays silent.
        runtime.resize(30, 100, 9, 18);
        assert!(
            channels.outbound_rx.try_recv().is_err(),
            "unchanged geometry must stay off the wire"
        );
    }

    #[tokio::test]
    async fn full_proxy_resize_queue_does_not_schedule_a_retry() {
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());
        let (runtime, channels) = PaneRuntime::spawn_remote_proxy(
            PaneId::alloc(),
            24,
            80,
            0,
            Arc::clone(&render_notify),
            Arc::clone(&render_dirty),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        let mut channels = channels;
        for _ in 0..REMOTE_PROXY_OUTBOUND_CAPACITY {
            channels
                .detach_tx
                .try_send(ProxyOutbound::Input(Bytes::from_static(b"busy")))
                .expect("queue has capacity");
        }

        let no_render_wakeup = render_notify.notified();
        runtime.resize(30, 100, 9, 18);
        assert_eq!(
            *channels.resize_slot.lock().expect("resize slot lock"),
            (30, 100, 9, 18)
        );
        assert!(!render_dirty.is_pending());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), no_render_wakeup)
                .await
                .is_err(),
            "a full queue must not wake a render retry"
        );
        for _ in 0..REMOTE_PROXY_OUTBOUND_CAPACITY {
            assert_eq!(
                channels.outbound_rx.try_recv(),
                Ok(ProxyOutbound::Input(Bytes::from_static(b"busy")))
            );
        }
        assert!(channels.outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn suspended_runtime_discards_direct_try_and_delayed_input() {
        let (mut runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        assert!(!runtime.is_suspended());

        runtime.suspend_processes();
        runtime.suspend_processes();

        assert!(runtime.is_suspended());
        runtime
            .send_bytes(Bytes::from_static(b"async"))
            .await
            .expect("suspended async input is accepted and discarded");
        runtime
            .try_send_bytes(Bytes::from_static(b"try"))
            .expect("suspended try input is accepted and discarded");
        runtime.send_bytes_after(
            Bytes::from_static(b"delayed"),
            std::time::Duration::from_millis(1),
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn remote_proxy_try_send_does_not_cross_terminal_loss() {
        let operation_state = crate::remote::RemoteFocusOperationState::new();
        let (runtime, mut channels) = PaneRuntime::spawn_remote_proxy(
            PaneId::alloc(),
            24,
            80,
            0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
            Arc::clone(&operation_state),
        )
        .expect("proxy runtime");
        runtime.set_remote_proxy_input_enabled(true);
        let state_for_hook = Arc::clone(&operation_state);
        operation_state.set_input_send_hook(Arc::new(move || {
            assert!(
                state_for_hook.terminate(),
                "the hook must simulate first loss"
            );
        }));

        assert!(runtime
            .try_send_bytes(Bytes::from_static(b"racing try"))
            .is_err());
        assert!(channels.outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn remote_proxy_async_send_does_not_cross_terminal_loss() {
        let operation_state = crate::remote::RemoteFocusOperationState::new();
        let (runtime, mut channels) = PaneRuntime::spawn_remote_proxy(
            PaneId::alloc(),
            24,
            80,
            0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
            Arc::clone(&operation_state),
        )
        .expect("proxy runtime");
        runtime.set_remote_proxy_input_enabled(true);
        let state_for_hook = Arc::clone(&operation_state);
        operation_state.set_input_send_hook(Arc::new(move || {
            assert!(
                state_for_hook.terminate(),
                "the hook must simulate first loss"
            );
        }));

        assert!(runtime
            .send_bytes(Bytes::from_static(b"racing async"))
            .await
            .is_err());
        assert!(channels.outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn remote_proxy_async_send_wakes_if_loss_occurs_while_queue_is_full() {
        let operation_state = crate::remote::RemoteFocusOperationState::new();
        let (runtime, mut channels) = PaneRuntime::spawn_remote_proxy(
            PaneId::alloc(),
            24,
            80,
            0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
            Arc::clone(&operation_state),
        )
        .expect("proxy runtime");
        runtime.set_remote_proxy_input_enabled(true);
        for _ in 0..REMOTE_PROXY_OUTBOUND_CAPACITY {
            runtime
                .try_send_bytes(Bytes::from_static(b"queued"))
                .expect("queue has capacity");
        }

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let ready_tx = Arc::new(Mutex::new(Some(ready_tx)));
        operation_state.set_input_send_hook(Arc::new(move || {
            if let Some(ready_tx) = ready_tx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                let _ = ready_tx.send(());
            }
        }));

        let send = runtime.send_bytes(Bytes::from_static(b"blocked"));
        tokio::pin!(send);
        tokio::select! {
            result = &mut send => assert!(result.is_err(), "input is rejected after loss"),
            _ = ready_rx => {
                assert!(operation_state.terminate(), "loss terminates the operation");
                let result = tokio::time::timeout(Duration::from_secs(1), &mut send)
                    .await
                    .expect("terminal loss wakes a blocked send");
                assert!(result.is_err(), "blocked input is rejected after loss");
            }
        }
        assert_eq!(
            (0..)
                .take_while(|_| channels.outbound_rx.try_recv().is_ok())
                .count(),
            REMOTE_PROXY_OUTBOUND_CAPACITY
        );
    }

    #[tokio::test]
    async fn remote_proxy_delayed_send_does_not_cross_terminal_loss() {
        let operation_state = crate::remote::RemoteFocusOperationState::new();
        let (runtime, mut channels) = PaneRuntime::spawn_remote_proxy(
            PaneId::alloc(),
            24,
            80,
            0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
            Arc::clone(&operation_state),
        )
        .expect("proxy runtime");
        runtime.set_remote_proxy_input_enabled(true);
        let state_for_hook = Arc::clone(&operation_state);
        operation_state.set_input_send_hook(Arc::new(move || {
            assert!(
                state_for_hook.terminate(),
                "the hook must simulate first loss"
            );
        }));

        runtime.send_bytes_after(
            Bytes::from_static(b"racing delayed"),
            Duration::from_millis(1),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(operation_state.is_terminal());
        assert!(channels.outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_resize_does_not_arm_retirement_of_a_blocked_gate() {
        // Resizing the herdr window repaints the pane, which has to invalidate
        // the idle scan skip -- but it is herdr's own write, and it used to be
        // enough on its own to clear a blocked badge the human was reading.
        let (runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        runtime.set_full_lifecycle_authority_state(true, true);
        let armed_baseline = runtime
            .full_lifecycle_hook_baseline_content_seq
            .load(Ordering::Acquire);

        runtime.resize(40, 100, 0, 0);

        let content_seq = runtime.detection_content_seq.load(Ordering::Acquire);
        assert!(
            content_seq > armed_baseline,
            "a resize must still invalidate the idle scan skip"
        );
        assert_eq!(
            runtime
                .full_lifecycle_hook_baseline_content_seq
                .load(Ordering::Acquire),
            content_seq,
            "a resize must not read as the agent resuming a turn"
        );

        let mut retirement = FullLifecycleHookOutputRetirement::default();
        let now = std::time::Instant::now();
        assert_eq!(retirement.observe(content_seq, content_seq, now), None);
        assert_eq!(
            retirement.observe(
                content_seq,
                content_seq,
                now + FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE * 3,
            ),
            None
        );
    }

    /// One detect loop's worth of retirement state, so the shared poll both
    /// loops make can be exercised without spawning a pane.
    struct RetirementHarness {
        detection_content_seq: AtomicU64,
        baseline_content_seq: AtomicU64,
        authority_active: AtomicBool,
        blocked: AtomicBool,
        state_events: mpsc::Sender<AppEvent>,
        retirement: FullLifecycleHookOutputRetirement,
        lifecycle_authority_active: bool,
    }

    impl RetirementHarness {
        fn blocked_at(baseline: u64, state_events: mpsc::Sender<AppEvent>) -> Self {
            Self {
                detection_content_seq: AtomicU64::new(baseline),
                baseline_content_seq: AtomicU64::new(baseline),
                authority_active: AtomicBool::new(true),
                blocked: AtomicBool::new(true),
                state_events,
                retirement: FullLifecycleHookOutputRetirement::default(),
                lifecycle_authority_active: true,
            }
        }

        fn from_terminal(
            baseline: u64,
            terminal: &crate::terminal::TerminalState,
            state_events: mpsc::Sender<AppEvent>,
        ) -> Self {
            let authority_active = terminal.full_lifecycle_hook_authority_active();
            Self {
                detection_content_seq: AtomicU64::new(baseline),
                baseline_content_seq: AtomicU64::new(baseline),
                authority_active: AtomicBool::new(authority_active),
                blocked: AtomicBool::new(terminal.hook_authority_output_retirement_eligible()),
                state_events,
                retirement: FullLifecycleHookOutputRetirement::default(),
                lifecycle_authority_active: authority_active,
            }
        }

        async fn poll(&mut self, content_seq: u64, at: std::time::Instant) {
            self.detection_content_seq
                .store(content_seq, Ordering::Release);
            poll_full_lifecycle_hook_retirement(
                FullLifecycleHookRetirementPorts {
                    pane_id: PaneId::from_raw(0),
                    detection_content_seq: &self.detection_content_seq,
                    baseline_content_seq: &self.baseline_content_seq,
                    authority_active: &self.authority_active,
                    blocked: &self.blocked,
                    state_events: &self.state_events,
                },
                &mut self.retirement,
                &mut self.lifecycle_authority_active,
                false,
                at,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn unmatched_output_retires_hook_authority_through_observer() {
        let (state_events, mut events) = mpsc::channel(4);
        let mut harness = RetirementHarness::blocked_at(10, state_events);
        let started = std::time::Instant::now();

        // Turn-end repaint, then a long quiet pane, then one isolated write.
        harness.poll(11, started).await;
        harness
            .poll(12, started + std::time::Duration::from_secs(30))
            .await;
        assert!(harness.lifecycle_authority_active);
        assert!(harness.authority_active.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());

        // A resumed turn writes on every 300ms detect tick.
        let resumed_at = started + std::time::Duration::from_secs(60);
        let mut seq = 12;
        for tick in 0..32 {
            seq += 1;
            harness
                .poll(
                    seq,
                    resumed_at + std::time::Duration::from_millis(300) * tick,
                )
                .await;
            if !harness.lifecycle_authority_active {
                break;
            }
        }

        assert!(!harness.lifecycle_authority_active);
        assert!(!harness.authority_active.load(Ordering::Acquire));
        assert!(matches!(
            events.try_recv(),
            Ok(AppEvent::HookAuthorityRetired { observed_at, .. }) if observed_at == resumed_at
        ));
    }

    #[tokio::test]
    async fn steady_output_does_not_retire_a_blocked_closing_gate() {
        let reported_at = std::time::Instant::now();
        let mut terminal = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            "/tmp".into(),
        );
        terminal.set_detected_state(Some(crate::detect::Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            reported_at,
        );
        let (state_events, mut events) = mpsc::channel(4);
        let mut harness = RetirementHarness::from_terminal(10, &terminal, state_events);

        for tick in 0..18 {
            harness
                .poll(
                    11 + tick,
                    reported_at + std::time::Duration::from_millis(300) * tick as u32,
                )
                .await;
        }

        assert_eq!(terminal.raw_agent_state(), AgentState::Blocked);
        assert!(!harness.blocked.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn shared_retirement_poll_clears_when_the_gate_goes_away() {
        let (state_events, _events) = mpsc::channel(4);
        let mut harness = RetirementHarness::blocked_at(10, state_events);
        let started = std::time::Instant::now();

        harness.poll(11, started).await;
        harness
            .poll(12, started + std::time::Duration::from_millis(300))
            .await;

        // The gate is answered: retirement state must not survive into the next
        // one, or a stale run would retire it early.
        harness.blocked.store(false, Ordering::Release);
        harness
            .poll(13, started + std::time::Duration::from_millis(600))
            .await;
        harness.blocked.store(true, Ordering::Release);
        harness.baseline_content_seq.store(13, Ordering::Release);

        harness
            .poll(14, started + std::time::Duration::from_millis(900))
            .await;
        harness
            .poll(
                15,
                started
                    + std::time::Duration::from_millis(900)
                    + FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE,
            )
            .await;

        assert!(harness.lifecycle_authority_active);
    }

    #[test]
    fn pane_launch_env_removes_outer_codex_thread_id() {
        let mut cmd = CommandBuilder::new("shell");
        cmd.env("CODEX_THREAD_ID", "outer-session");

        apply_pane_launch_env(&mut cmd, &PaneLaunchEnv::default());

        assert!(cmd.get_env("CODEX_THREAD_ID").is_none());
    }

    #[test]
    fn pane_launch_env_removes_outer_claude_child_session_marker() {
        let mut cmd = CommandBuilder::new("shell");
        cmd.env("CLAUDE_CODE_CHILD_SESSION", "1");

        apply_pane_launch_env(&mut cmd, &PaneLaunchEnv::default());

        assert!(cmd.get_env("CLAUDE_CODE_CHILD_SESSION").is_none());
    }

    #[test]
    fn pane_launch_env_binds_hooks_to_owning_herdr_binary() {
        let mut cmd = CommandBuilder::new("shell");

        apply_pane_launch_env(&mut cmd, &PaneLaunchEnv::default());

        assert_eq!(
            cmd.get_env(crate::integration::HERDR_BIN_PATH_ENV_VAR),
            std::env::current_exe()
                .ok()
                .as_deref()
                .map(std::path::Path::as_os_str)
        );
    }

    #[test]
    fn pane_terminal_identity_removes_outer_windows_terminal_session() {
        let mut cmd = CommandBuilder::new("shell");
        cmd.env("WT_SESSION", "outer-session");

        apply_pane_terminal_env(&mut cmd);

        assert!(cmd.get_env("WT_SESSION").is_none());
    }

    #[tokio::test]
    async fn cwd_returns_accepted_report_without_rechecking_filesystem() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        let cwd = std::env::temp_dir().join(format!(
            "herdr-reported-cwd-cache-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&cwd).expect("create reported cwd");

        let (runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        let (events, _event_rx) = mpsc::channel(1);
        publish_reported_cwd(runtime.pane_id, cwd.clone(), &runtime.reported_cwd, &events);
        assert_eq!(
            runtime.reported_cwd.lock().unwrap().as_ref(),
            Some(&cwd),
            "test setup must pass cache admission"
        );

        std::fs::remove_dir(&cwd).expect("remove reported cwd after admission");

        assert_eq!(runtime.cwd(), Some(cwd));
    }

    #[cfg(unix)]
    #[test]
    fn process_cwd_does_not_require_traversing_the_directory_path() {
        use std::os::unix::fs::PermissionsExt;

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "herdr-process-cwd-no-stat-{}-{stamp}",
            std::process::id()
        ));
        let private = base.join("private");
        let cwd = private.join("cwd");
        std::fs::create_dir_all(&cwd).expect("create process cwd");

        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .current_dir(&cwd)
            .spawn()
            .expect("spawn process in cwd");
        let expected_cwd = crate::platform::process_cwd(child.id())
            .expect("resolve process cwd before restricting traversal");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o000))
            .expect("make cwd path untraversable");

        let path_is_traversable = cwd.is_dir();
        let observed = (!path_is_traversable)
            .then(|| absolute_process_cwd(child.id()))
            .flatten();

        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755))
            .expect("restore cwd path permissions");
        let _ = child.kill();
        let _ = child.wait();
        std::fs::remove_dir_all(&base).expect("remove process cwd");

        if path_is_traversable {
            eprintln!("skipping untraversable cwd assertion for privileged test process");
            return;
        }
        assert_eq!(observed, Some(expected_cwd));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn follow_cwd_falls_back_to_reported_pane_cwd_without_foreground_group() {
        let (runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        let cwd = std::env::temp_dir();
        *runtime.reported_cwd.lock().unwrap() = Some(cwd.clone());

        assert_eq!(runtime.follow_cwd(), Some(cwd));
    }

    #[test]
    fn shutdown_liveness_treats_reaped_direct_child_as_gone() {
        assert!(!process_alive_for_shutdown(42, 42, true, |_| true));
    }

    #[test]
    fn shutdown_liveness_keeps_unreaped_direct_child_alive() {
        assert!(process_alive_for_shutdown(42, 42, false, |_| true));
    }

    #[test]
    fn shutdown_liveness_keeps_other_session_processes_alive() {
        assert!(process_alive_for_shutdown(43, 42, true, |_| true));
    }

    #[test]
    fn shutdown_liveness_treats_missing_process_as_gone() {
        assert!(!process_alive_for_shutdown(43, 42, false, |_| false));
    }

    #[cfg(unix)]
    fn capture_shell_output(command: &str, extra_env: &[(&str, &str)]) -> String {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let output_path = std::env::temp_dir().join(format!(
            "herdr-pane-term-test-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(format!("{command} > '{}'", output_path.display()));
        cmd.cwd(std::env::current_dir().unwrap());
        cmd.env("TERM", "xterm-ghostty");
        cmd.env("COLORTERM", "falsecolor");
        apply_pane_terminal_env(&mut cmd);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "shell command failed: {status:?}");

        let output = std::fs::read_to_string(&output_path).unwrap();
        let _ = std::fs::remove_file(output_path);
        output
    }

    #[test]
    fn pane_shell_prefers_configured_shell() {
        assert_eq!(
            pane_shell_from("/usr/bin/nu", Some("/bin/bash".to_string())),
            "/usr/bin/nu"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn pane_shell_falls_back_to_shell_env() {
        assert_eq!(
            pane_shell_from("", Some("/bin/bash".to_string())),
            "/bin/bash"
        );
    }

    #[cfg(windows)]
    #[test]
    fn pane_shell_ignores_shell_env_on_windows() {
        assert_eq!(
            pane_shell_from("", Some("c:\\windows\\system32\\cmd.exe".to_string())),
            default_pane_shell()
        );
    }

    #[test]
    fn pane_shell_ignores_empty_values() {
        assert_eq!(
            pane_shell_from("   ", Some("  ".to_string())),
            default_pane_shell()
        );
        assert_eq!(pane_shell_from("", None), default_pane_shell());
    }

    #[test]
    fn shell_mode_auto_uses_login_shell_only_on_macos() {
        assert!(shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::Auto,
            ShellLaunchTarget::Macos
        ));
        assert!(!shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::Auto,
            ShellLaunchTarget::OtherUnix
        ));
        assert!(!shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::Auto,
            ShellLaunchTarget::Windows
        ));
        assert!(shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::Login,
            ShellLaunchTarget::OtherUnix
        ));
        assert!(!shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::NonLogin,
            ShellLaunchTarget::Macos
        ));
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_builder_uses_default_prog_with_resolved_shell_env() {
        let cmd = pane_shell_command_builder_for_target(
            PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::Login),
            ShellLaunchTarget::OtherUnix,
        )
        .unwrap();
        assert!(cmd.is_default_prog());
        assert_eq!(
            cmd.get_env("SHELL").and_then(std::ffi::OsStr::to_str),
            Some("/bin/sh")
        );
    }

    #[cfg(unix)]
    #[test]
    fn auto_shell_builder_uses_login_shell_on_macos_target() {
        let cmd = pane_shell_command_builder_for_target(
            PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::Auto),
            ShellLaunchTarget::Macos,
        )
        .unwrap();
        assert!(cmd.is_default_prog());
        assert_eq!(
            cmd.get_env("SHELL").and_then(std::ffi::OsStr::to_str),
            Some("/bin/sh")
        );
    }

    #[test]
    fn auto_shell_builder_keeps_direct_shell_on_non_macos_target() {
        let cmd = pane_shell_command_builder_for_target(
            PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::Auto),
            ShellLaunchTarget::OtherUnix,
        )
        .unwrap();
        assert!(!cmd.is_default_prog());
        assert_eq!(cmd.get_argv(), &[std::ffi::OsString::from("/bin/sh")]);
    }

    #[test]
    fn windows_powershell_builder_injects_prompt_cwd_shell_integration() {
        for shell in [
            "powershell.exe",
            "pwsh.exe",
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
        ] {
            let cmd = pane_shell_command_builder_for_target(
                PaneShellConfig::new(shell, crate::config::ShellModeConfig::NonLogin),
                ShellLaunchTarget::Windows,
            )
            .unwrap();

            assert_eq!(
                cmd.get_argv(),
                &[
                    std::ffi::OsString::from(shell),
                    std::ffi::OsString::from("-NoExit"),
                    std::ffi::OsString::from("-Command"),
                    std::ffi::OsString::from(WINDOWS_POWERSHELL_SHELL_INTEGRATION_COMMAND),
                ]
            );
        }

        let script = WINDOWS_POWERSHELL_SHELL_INTEGRATION_COMMAND;
        let cwd_sync = script
            .find("[Environment]::CurrentDirectory = $loc.ProviderPath")
            .expect("wrapper must synchronize the Win32 process cwd");
        let osc_report = script.find("]9;9;").expect("wrapper must emit OSC 9;9");
        assert!(cwd_sync < osc_report, "cwd sync must precede OSC report");
        assert!(
            script.contains("$global:__HerdrOriginalPrompt = $function:prompt"),
            "must wrap the profile-defined prompt: {script}"
        );
        assert!(
            script.contains("$null -eq $global:__HerdrOriginalPrompt"),
            "wrap must be idempotent for nested sessions: {script}"
        );
        assert!(
            script.contains("'FileSystem'"),
            "must not report non-filesystem provider paths: {script}"
        );
        assert!(
            !script.contains('"'),
            "double quotes corrupt the powershell.exe command-line round-trip: {script}"
        );
        let invoke_original = script
            .find("@(& $global:__HerdrOriginalPrompt)")
            .expect("wrapper must invoke the original prompt");
        let cwd_lookup = script
            .find("$loc =")
            .expect("wrapper must look up the current location");
        assert!(
            invoke_original < cwd_lookup,
            "original prompt must run first or $? is reset before a status-aware prompt reads it: {script}"
        );
    }

    #[test]
    fn windows_non_powershell_builder_launches_plain_shell() {
        let cmd = pane_shell_command_builder_for_target(
            PaneShellConfig::new("cmd.exe", crate::config::ShellModeConfig::NonLogin),
            ShellLaunchTarget::Windows,
        )
        .unwrap();

        assert_eq!(cmd.get_argv(), &[std::ffi::OsString::from("cmd.exe")]);
    }

    #[test]
    fn unix_powershell_builder_launches_plain_shell() {
        let cmd = pane_shell_command_builder_for_target(
            PaneShellConfig::new("pwsh", crate::config::ShellModeConfig::NonLogin),
            ShellLaunchTarget::OtherUnix,
        )
        .unwrap();

        assert_eq!(cmd.get_argv(), &[std::ffi::OsString::from("pwsh")]);
    }

    #[test]
    fn windows_powershell_pane_shell_predicate_requires_windows_and_non_login() {
        let pwsh = PaneShellConfig::new("pwsh.exe", crate::config::ShellModeConfig::NonLogin);
        assert!(uses_windows_powershell_pane_shell_for_target(
            pwsh,
            ShellLaunchTarget::Windows
        ));
        assert!(!uses_windows_powershell_pane_shell_for_target(
            pwsh,
            ShellLaunchTarget::OtherUnix
        ));
        assert!(!uses_windows_powershell_pane_shell_for_target(
            pwsh,
            ShellLaunchTarget::Macos
        ));
        assert!(!uses_windows_powershell_pane_shell_for_target(
            PaneShellConfig::new("pwsh.exe", crate::config::ShellModeConfig::Login),
            ShellLaunchTarget::Windows
        ));
        assert!(!uses_windows_powershell_pane_shell_for_target(
            PaneShellConfig::new("cmd.exe", crate::config::ShellModeConfig::NonLogin),
            ShellLaunchTarget::Windows
        ));
    }

    #[test]
    fn login_shell_builder_rejects_missing_shell_instead_of_falling_back() {
        let err = pane_shell_command_builder_for_target(
            PaneShellConfig::new(
                "/__herdr_missing_shell__",
                crate::config::ShellModeConfig::Login,
            ),
            ShellLaunchTarget::OtherUnix,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_builder_resolves_bare_shell_names_from_path() {
        let _lock = crate::integration::integration_env_lock();
        let base = std::env::temp_dir().join(format!(
            "herdr-login-shell-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shell = bin.join("fake-shell");
        std::fs::write(&shell, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set("PATH", &bin);

        let cmd = pane_shell_command_builder_for_target(
            PaneShellConfig::new("fake-shell", crate::config::ShellModeConfig::Login),
            ShellLaunchTarget::OtherUnix,
        )
        .unwrap();

        assert!(cmd.is_default_prog());
        assert_eq!(
            cmd.get_env("SHELL").and_then(std::ffi::OsStr::to_str),
            shell.to_str()
        );
        drop(env);
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_resolution_preserves_shell_paths() {
        assert_eq!(resolve_shell_for_login_mode("/bin/sh").unwrap(), "/bin/sh");
    }

    #[test]
    fn non_login_shell_builder_execs_resolved_shell_directly() {
        let cmd = pane_shell_command_builder(PaneShellConfig::new(
            "/bin/sh",
            crate::config::ShellModeConfig::NonLogin,
        ))
        .unwrap();
        assert!(!cmd.is_default_prog());
        assert_eq!(cmd.get_argv(), &[std::ffi::OsString::from("/bin/sh")]);
    }

    #[cfg(unix)]
    #[test]
    fn pane_terminal_identity_overrides_outer_terminal_env() {
        let output = capture_shell_output("printf '%s\\n%s\\n' \"$TERM\" \"$COLORTERM\"", &[]);
        assert_eq!(output, "xterm-256color\ntruecolor\n");
    }

    #[cfg(unix)]
    #[test]
    fn pane_terminal_identity_allows_explicit_override() {
        let output = capture_shell_output(
            "printf '%s\\n%s\\n' \"$TERM\" \"$COLORTERM\"",
            &[("TERM", "vt100"), ("COLORTERM", "24bit")],
        );
        assert_eq!(output, "vt100\n24bit\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn handoff_history_ansi_captures_primary_screen() {
        let runtime =
            PaneRuntime::test_with_scrollback_bytes(40, 5, 4096, b"handoff-primary-history\r\n");

        let history = runtime.handoff_history_ansi().unwrap();

        assert!(history.contains("handoff-primary-history"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn handoff_history_ansi_skips_alternate_screen() {
        let runtime = PaneRuntime::test_with_scrollback_bytes(
            40,
            5,
            4096,
            b"primary\r\n\x1b[?1049halt-screen",
        );

        assert!(runtime.handoff_history_ansi().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn handoff_runtime_state_captures_terminal_input_and_title_state() {
        let runtime = PaneRuntime::test_with_screen_bytes(
            80,
            24,
            b"\x1b[>5u\x1b[>4;2m\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?1002h\x1b[?1006h\x1b[?2031h",
        );

        runtime.test_process_pty_bytes("\x1b]2;✳ 修复🙂标题\x1b\\".as_bytes());
        runtime.terminal.clear_agent_osc_state();
        assert_eq!(runtime.agent_osc_title(), "");
        let pane = runtime.handoff_runtime_state(12);

        assert_eq!(pane.keyboard_protocol_flags, 5);
        assert_eq!(pane.terminal_title.as_deref(), Some("✳ 修复🙂标题"));
        assert_eq!(
            pane.input_state,
            Some(InputState {
                alternate_screen: false,
                application_cursor: true,
                bracketed_paste: true,
                focus_reporting: true,
                mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
                mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
                mouse_alternate_scroll: true,
                modify_other_keys: true,
                color_scheme_reporting: true,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn truncate_handoff_history_keeps_recent_utf8_boundary() {
        let history = format!("old\n{}\nrecent\n", "é".repeat(8));

        let truncated = truncate_handoff_history(history, 20);

        assert_eq!(truncated, "recent\n");
        assert!(truncated.is_char_boundary(0));
    }

    #[cfg(unix)]
    #[test]
    fn truncate_handoff_history_drops_partial_long_line() {
        let history = format!("old\n{}", "x".repeat(64));

        let truncated = truncate_handoff_history(history, 12);

        assert!(truncated.is_empty());
    }

    #[tokio::test]
    async fn focus_events_are_forwarded_when_enabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = watch::channel((80, 24, 0, 0));
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal
            .mode_set(crate::ghostty::MODE_FOCUS_EVENT, true)
            .unwrap();
        let runtime = PaneRuntime {
            pane_id: PaneId::from_raw(0),
            terminal: Arc::new(PaneTerminal::new(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
            io: PaneRuntimeIo::TestChannel {
                sender: tx,
                resize_tx,
                remote_owner: Arc::new(Mutex::new(None)),
            },
            current_size: Cell::new((80, 24, 0, 0)),
            resize_count: Cell::new(0),
            child_pid: Arc::new(AtomicU32::new(0)),
            tty_name: None,
            reported_cwd: Arc::new(Mutex::new(None)),
            persistence_cwd: Mutex::new(None),
            cwd_process_exited: Arc::new(AtomicBool::new(false)),
            child_wait_completed: None,
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            content_seq: Arc::new(AtomicU64::new(0)),
            input_admission: Arc::new(Mutex::new(InputAdmissionState::default())),
            pending_input_reservations: Arc::new(AtomicU64::new(0)),
            input_delivery_seq: Arc::new(AtomicU64::new(0)),
            reflected_input_seq: Arc::new(AtomicU64::new(0)),
            detection_content_seq: Arc::new(AtomicU64::new(0)),
            full_lifecycle_hook_baseline_content_seq: Arc::new(AtomicU64::new(0)),
            full_lifecycle_authority_active: Arc::new(AtomicBool::new(false)),
            supervisor_stale: Arc::new(AtomicBool::new(false)),
            full_lifecycle_hook_blocked: Arc::new(AtomicBool::new(false)),
            detect_reset_notify: Arc::new(Notify::new()),
            detect_screen_rescan_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            suspended: false,
            suppress_pane_died: Arc::new(AtomicBool::new(false)),
            preserve_processes_on_drop: true,
            detect_handle: Some(tokio::spawn(async {}).abort_handle()),
        };

        assert!(runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"\x1b[I"));
    }

    #[tokio::test]
    async fn focus_events_are_suppressed_when_disabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = watch::channel((80, 24, 0, 0));
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let runtime = PaneRuntime {
            pane_id: PaneId::from_raw(0),
            terminal: Arc::new(PaneTerminal::new(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
            io: PaneRuntimeIo::TestChannel {
                sender: tx,
                resize_tx,
                remote_owner: Arc::new(Mutex::new(None)),
            },
            current_size: Cell::new((80, 24, 0, 0)),
            resize_count: Cell::new(0),
            child_pid: Arc::new(AtomicU32::new(0)),
            tty_name: None,
            reported_cwd: Arc::new(Mutex::new(None)),
            persistence_cwd: Mutex::new(None),
            cwd_process_exited: Arc::new(AtomicBool::new(false)),
            child_wait_completed: None,
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            content_seq: Arc::new(AtomicU64::new(0)),
            input_admission: Arc::new(Mutex::new(InputAdmissionState::default())),
            pending_input_reservations: Arc::new(AtomicU64::new(0)),
            input_delivery_seq: Arc::new(AtomicU64::new(0)),
            reflected_input_seq: Arc::new(AtomicU64::new(0)),
            detection_content_seq: Arc::new(AtomicU64::new(0)),
            full_lifecycle_hook_baseline_content_seq: Arc::new(AtomicU64::new(0)),
            full_lifecycle_authority_active: Arc::new(AtomicBool::new(false)),
            supervisor_stale: Arc::new(AtomicBool::new(false)),
            full_lifecycle_hook_blocked: Arc::new(AtomicBool::new(false)),
            detect_reset_notify: Arc::new(Notify::new()),
            detect_screen_rescan_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            suspended: false,
            suppress_pane_died: Arc::new(AtomicBool::new(false)),
            preserve_processes_on_drop: true,
            detect_handle: Some(tokio::spawn(async {}).abort_handle()),
        };

        assert!(!runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn subscribed_idle_child_receives_color_scheme_transition() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        runtime.apply_host_terminal_appearance(Some(crate::terminal_theme::HostAppearance::Dark));
        runtime.test_process_pty_bytes(b"\x1b[?2031h");

        runtime.apply_host_terminal_appearance(Some(crate::terminal_theme::HostAppearance::Light));

        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"\x1b[?997;2n")));
    }

    #[test]
    fn foreground_shell_reports_process_exit_before_clearing_agent() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Codex), None, true, false),
            ForegroundShellAgentAction::ReportProcessExit
        );
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Codex), None, true, true),
            ForegroundShellAgentAction::ClearAgent
        );
    }

    #[test]
    fn same_agent_after_reported_exit_is_a_replacement_process() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Pi), Some(Agent::Pi), false, true),
            ForegroundShellAgentAction::ReportReplacementProcess
        );
    }

    #[test]
    fn unknown_non_shell_foreground_job_is_not_immediate_clear_signal() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Claude), None, false, false),
            ForegroundShellAgentAction::ObserveProbe
        );
    }

    #[tokio::test]
    async fn first_agent_acquisition_keeps_osc_evidence_replacement_clears_it() {
        let runtime = PaneRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes(b"\x1b]2;startup title\x1b\\\x1b]9;4;1;\x1b\\");

        clear_osc_evidence_for_agent_transition(&runtime.terminal, None);
        assert_eq!(runtime.agent_osc_title(), "startup title");
        assert_eq!(runtime.agent_osc_progress(), "4;1;");

        clear_osc_evidence_for_agent_transition(&runtime.terminal, Some(Agent::Claude));
        assert_eq!(runtime.agent_osc_title(), "");
        assert_eq!(runtime.agent_osc_progress(), "");
    }

    #[test]
    fn reported_process_exit_clears_before_unknown_foreground_probe() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Claude), None, false, true),
            ForegroundShellAgentAction::ClearAgent
        );
    }

    #[test]
    fn foreground_agent_job_is_not_clear_signal() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Claude), Some(Agent::OpenCode), true, false,),
            ForegroundShellAgentAction::ObserveProbe
        );
    }

    fn foreground_process(pid: u32, name: &str) -> crate::platform::ForegroundProcess {
        crate::platform::ForegroundProcess {
            pid,
            name: name.to_string(),
            argv0: None,
            argv: None,
            cmdline: None,
        }
    }

    #[test]
    fn foreground_agent_hint_accepts_pane_shell_environment() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![foreground_process(42, "bash")],
        };

        assert_eq!(
            agent_hint_for_foreground_job_members(&job, |pid| {
                (pid == 42).then_some(Agent::Claude)
            }),
            Some(Agent::Claude)
        );
    }

    #[test]
    fn foreground_agent_hint_accepts_non_leader_foreground_process_environment() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![
                foreground_process(99, "fence"),
                foreground_process(100, "pi"),
            ],
        };

        assert_eq!(
            agent_hint_for_foreground_job_members(&job, |pid| {
                (pid == 100).then_some(Agent::Codex)
            }),
            Some(Agent::Codex)
        );
    }

    #[test]
    fn foreground_agent_hint_wins_over_process_name_detection() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![foreground_process(99, "codex")],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            Some(job),
            || None,
            |pid| (pid == 99).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Claude));
        assert_eq!(result.process_name.as_deref(), Some("claude"));
    }

    #[test]
    fn foreground_agent_hint_on_inherited_child_environment_is_authoritative() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![foreground_process(99, "vim")],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            None,
            || Some(job),
            |pid| (pid == 99).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Claude));
        assert_eq!(result.process_name.as_deref(), Some("claude"));
    }

    #[test]
    fn non_leader_agent_hint_does_not_override_identifiable_leader() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![
                foreground_process(99, "codex"),
                foreground_process(100, "vim"),
            ],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            None,
            || Some(job),
            |pid| (pid == 100).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Codex));
        assert_eq!(result.process_name.as_deref(), Some("codex"));
    }

    #[test]
    fn non_leader_agent_hint_wins_when_leader_is_unidentified() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![
                foreground_process(99, "some_vm"),
                foreground_process(100, "vim"),
            ],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            None,
            || Some(job),
            |pid| (pid == 100).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Claude));
        assert_eq!(result.process_name.as_deref(), Some("claude"));
    }

    fn process_probe_input() -> ProcessProbeInput {
        ProcessProbeInput {
            current_agent: None,
            suppressed_agent: None,
            foreground_pgid: Some(42),
            last_foreground_pgid: Some(42),
            has_process_probe: true,
            acquisition_age: None,
            pending_foreground_shell_clear: false,
            pending_restore_probe: false,
            elapsed_since_process_check: std::time::Duration::from_secs(1),
            supervisor_stale: false,
        }
    }

    #[test]
    fn windows_foreground_observation_schedule_preserves_lifecycle_checks() {
        let quiet = ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            ..process_probe_input()
        };
        let before_safety_bound = PROCESS_RECHECK_IDENTIFIED - std::time::Duration::from_millis(1);
        let content_retry = std::time::Duration::from_millis(300);
        let observe = |lifecycle, content_changed, elapsed, input| {
            should_observe_foreground_process_group(lifecycle, content_changed, elapsed, input)
        };
        let content_due = |last: Option<u64>, current, elapsed| {
            last != Some(current) && (last.is_some() || elapsed >= content_retry)
        };

        assert!(!observe(false, false, before_safety_bound, quiet));
        assert!(observe(false, true, before_safety_bound, quiet));
        assert!(observe(true, true, before_safety_bound, quiet));
        assert!(content_due(Some(0), 1, std::time::Duration::ZERO));
        assert!(!content_due(
            None,
            1,
            content_retry - std::time::Duration::from_millis(1)
        ));
        assert!(content_due(None, 1, content_retry));
        assert!(observe(
            false,
            false,
            before_safety_bound,
            ProcessProbeInput {
                elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
                ..quiet
            }
        ));
        assert!(observe(true, false, PROCESS_RECHECK_IDENTIFIED, quiet));

        for immediate in [
            ProcessProbeInput {
                has_process_probe: false,
                ..quiet
            },
            ProcessProbeInput {
                current_agent: None,
                acquisition_age: Some(std::time::Duration::ZERO),
                ..quiet
            },
            ProcessProbeInput {
                pending_restore_probe: true,
                ..quiet
            },
            ProcessProbeInput {
                suppressed_agent: Some(Agent::Codex),
                ..quiet
            },
            ProcessProbeInput {
                pending_foreground_shell_clear: true,
                ..quiet
            },
        ] {
            assert!(observe(false, false, std::time::Duration::ZERO, immediate));
        }
    }

    #[test]
    fn unchanged_unidentified_foreground_group_skips_full_process_probe() {
        assert!(!should_probe_foreground_job(process_probe_input()));
    }

    #[test]
    fn unidentified_foreground_group_change_runs_full_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: Some(43),
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_gets_initial_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn stable_unidentified_foreground_group_has_no_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            elapsed_since_process_check: PROCESS_RECHECK_MISSING_FOREGROUND_GROUP,
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_without_foreground_group_uses_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: None,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_MISSING_FOREGROUND_GROUP,
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_probes_when_foreground_group_disappears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: Some(42),
            ..process_probe_input()
        }));
    }

    #[test]
    fn inferred_group_does_not_trigger_a_probe_on_every_tick() {
        let tracked = process_group_for_change_tracking(None, Some(300));
        assert_eq!(tracked, None);
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Claude),
            foreground_pgid: None,
            last_foreground_pgid: tracked,
            elapsed_since_process_check: std::time::Duration::from_millis(300),
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_shell_clear_and_restore_force_process_probes() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            pending_foreground_shell_clear: true,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            pending_restore_probe: true,
            ..process_probe_input()
        }));
    }

    #[test]
    fn lifecycle_authority_skips_stable_routine_process_probe() {
        assert!(should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
                ..process_probe_input()
            }
        ));
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            false,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
                ..process_probe_input()
            }
        ));
    }

    /// A stale supervisor is the case the probe exists to answer. Skipping it
    /// leaves `stale_resolution` empty, which keeps a finished pane from ever
    /// reading quiet, so it never settles and gets nudged forever.
    #[test]
    fn a_stale_supervisor_keeps_the_probe_running_under_lifecycle_authority() {
        let stable = ProcessProbeInput {
            current_agent: Some(Agent::Pi),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        };
        assert!(should_skip_process_probe_for_lifecycle_authority(
            true, stable
        ));
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                supervisor_stale: true,
                ..stable
            }
        ));
    }

    #[test]
    fn lifecycle_authority_keeps_periodic_probes_without_an_observed_group() {
        let input = ProcessProbeInput {
            current_agent: Some(Agent::Pi),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        };
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true, input
        ));
        assert!(should_probe_foreground_job(input));
    }

    #[test]
    fn lifecycle_authority_preserves_process_exit_and_release_probes() {
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                pending_foreground_shell_clear: true,
                ..process_probe_input()
            }
        ));
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                suppressed_agent: Some(Agent::Pi),
                ..process_probe_input()
            }
        ));
    }

    #[test]
    fn lifecycle_authority_preserves_initial_and_foreground_group_change_probes() {
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: None,
                has_process_probe: false,
                ..process_probe_input()
            }
        ));
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                foreground_pgid: Some(43),
                ..process_probe_input()
            }
        ));
    }

    #[test]
    fn active_unblocked_lifecycle_authority_reads_the_screen() {
        assert!(!should_skip_screen_detection_for_lifecycle_authority(
            true, false, false
        ));
    }

    #[test]
    fn active_blocked_lifecycle_authority_reads_visible_blockers() {
        assert!(!should_skip_screen_detection_for_lifecycle_authority(
            true, true, false
        ));
    }

    #[test]
    fn pending_release_forces_initial_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            suppressed_agent: Some(Agent::Codex),
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_forces_process_probe_after_runtime_identity_clears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_skips_repeated_probe_when_foreground_group_is_stable() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_probes_when_foreground_group_changes() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            foreground_pgid: Some(43),
            ..process_probe_input()
        }));
    }

    #[test]
    fn acquisition_window_catches_delayed_same_group_wrapper_startup() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_millis(1250)),
            elapsed_since_process_check: PROCESS_ACQUISITION_FAST_RECHECK
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_millis(1250)),
            elapsed_since_process_check: PROCESS_ACQUISITION_FAST_RECHECK,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_secs(5)),
            elapsed_since_process_check: PROCESS_ACQUISITION_SLOW_RECHECK,
            ..process_probe_input()
        }));
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(PROCESS_ACQUISITION_WINDOW + std::time::Duration::from_millis(1),),
            elapsed_since_process_check: PROCESS_ACQUISITION_SLOW_RECHECK,
            ..process_probe_input()
        }));
    }

    #[test]
    fn content_change_starts_bounded_unidentified_acquisition_window() {
        let now = std::time::Instant::now();
        let mut acquisition_started_at = None;
        let mut last_content_change_at = None;

        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, Some(now));
        assert_eq!(last_content_change_at, Some(now));

        let later = now + std::time::Duration::from_secs(1);
        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            later,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(
            acquisition_started_at,
            Some(now),
            "changed frames should not refresh the acquisition window"
        );
        assert_eq!(last_content_change_at, Some(later));

        let quiet_after_window =
            later + PROCESS_ACQUISITION_WINDOW + PROCESS_ACQUISITION_IDLE_RESET;
        sync_content_change_acquisition(
            None,
            None,
            false,
            false,
            quiet_after_window,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);

        let next_burst = quiet_after_window + std::time::Duration::from_secs(1);
        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            next_burst,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, Some(next_burst));
        assert_eq!(last_content_change_at, Some(next_burst));
    }

    #[test]
    fn content_change_does_not_start_acquisition_when_process_probe_has_other_signal() {
        let now = std::time::Instant::now();
        let mut acquisition_started_at = None;
        let mut last_content_change_at = None;

        sync_content_change_acquisition(
            Some(Agent::Codex),
            None,
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);

        sync_content_change_acquisition(
            None,
            Some(Agent::Codex),
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);

        sync_content_change_acquisition(
            None,
            None,
            true,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);
    }

    #[test]
    fn content_change_restarts_stale_process_group_acquisition_window() {
        let now = std::time::Instant::now();
        let stale_start = now - PROCESS_ACQUISITION_WINDOW - std::time::Duration::from_millis(1);
        let mut acquisition_started_at = Some(stale_start);
        let mut last_content_change_at = None;

        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );

        assert_eq!(acquisition_started_at, Some(now));
        assert_eq!(last_content_change_at, Some(now));
    }

    #[test]
    fn release_expiry_can_force_reacquire_probe_by_resetting_probe_state() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn identified_agent_uses_shorter_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        }));
    }

    #[test]
    fn identified_agent_probes_when_foreground_group_disappears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: Some(42),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
    }

    #[test]
    fn stable_missing_foreground_group_uses_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        }));
    }

    #[test]
    fn transient_process_miss_keeps_current_agent_detected() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        let changed = presence.observe_process_probe(None);

        assert!(!changed, "one miss should not clear the detected agent");
        assert_eq!(presence.current_agent(), Some(Agent::Pi));
    }

    #[test]
    fn agent_only_clears_after_confirmation_misses() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        for attempt in 1..AGENT_MISS_CONFIRMATION_ATTEMPTS {
            let changed = presence.observe_process_probe(None);
            assert!(
                !changed,
                "miss {attempt} should stay in the confirmation window"
            );
            assert_eq!(presence.current_agent(), Some(Agent::Pi));
        }

        let changed = presence.observe_process_probe(None);
        assert!(changed, "last confirmation miss should clear the agent");
        assert_eq!(presence.current_agent(), None);
    }

    #[tokio::test]
    async fn authority_expiry_requests_screen_rescan_without_resetting_detector() {
        let runtime = PaneRuntime::test_with_screen_bytes(80, 24, b"");
        let reset_notify = runtime.agent_detection_reset_notify_for_test();
        let rescan_notify = runtime.agent_detection_screen_rescan_notify_for_test();

        runtime.set_full_lifecycle_authority_state(true, false);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                rescan_notify.notified()
            )
            .await
            .is_err(),
            "entering authority should preserve the current detector evidence"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                reset_notify.notified()
            )
            .await
            .is_err(),
            "entering authority must not reset detector state"
        );

        runtime.set_full_lifecycle_authority_state(true, false);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                rescan_notify.notified()
            )
            .await
            .is_err(),
            "repeated true-to-true sync should not request a rescan"
        );

        runtime.set_full_lifecycle_authority_state(false, false);
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            rescan_notify.notified(),
        )
        .await
        .expect("authority expiry should wake screen detection");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                reset_notify.notified()
            )
            .await
            .is_err(),
            "authority expiry must preserve detector process and cache state"
        );

        runtime.set_full_lifecycle_authority_state(false, false);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                rescan_notify.notified()
            )
            .await
            .is_err(),
            "repeated false-to-false sync should not request a rescan"
        );

        runtime.set_full_lifecycle_authority_state(true, false);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                rescan_notify.notified()
            )
            .await
            .is_err(),
            "a new accepted report rearms authority without discarding screen evidence"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_pty_reader_aggregates_terminal_bells() {
        let (events, mut event_rx) = mpsc::channel(8);
        let pane_id = PaneId::from_raw(42);
        let runtime = PaneRuntime::spawn_shell_command(
            pane_id,
            24,
            80,
            std::env::temp_dir(),
            "printf '\\a\\a'; sleep 0.05",
            &PaneLaunchEnv::default(),
            AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();

        let bell = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(AppEvent::TerminalBell {
                    pane_id: delivered_pane,
                    count,
                }) = event_rx.recv().await
                {
                    break (delivered_pane, count);
                }
            }
        })
        .await
        .expect("PTY reader should publish terminal bells");

        assert_eq!(bell, (pane_id, 2));
        runtime.shutdown();
    }

    #[tokio::test]
    async fn state_changed_event_waits_for_queue_space_instead_of_dropping() {
        let (tx, mut rx) = mpsc::channel(1);
        let pane_id = PaneId::from_raw(42);

        tx.try_send(AppEvent::UpdateReady {
            version: "9.9.9".into(),
            install_command: "herdr update".into(),
        })
        .unwrap();

        let publish = publish_state_changed_event(
            tx.clone(),
            pane_id,
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            std::time::Instant::now(),
        );
        tokio::pin!(publish);

        let blocked = tokio::time::timeout(std::time::Duration::from_millis(20), async {
            (&mut publish).await;
        })
        .await;
        assert!(
            blocked.is_err(),
            "publisher should wait for queue space instead of dropping StateChanged"
        );

        let first = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield first event")
            .expect("sender still alive");
        assert!(matches!(first, AppEvent::UpdateReady { .. }));

        tokio::time::timeout(std::time::Duration::from_millis(50), async {
            (&mut publish).await;
        })
        .await
        .expect("publisher should complete once queue space is available");

        let second = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield second event")
            .expect("sender still alive");
        assert!(matches!(
            second,
            AppEvent::StateChanged {
                pane_id: delivered_pane,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                usage_limited: false,
                process_exited: false,
                observed_at: _,
            } if delivered_pane == pane_id
        ));
    }
}
