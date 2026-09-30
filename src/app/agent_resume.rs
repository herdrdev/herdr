use std::time::{Duration, Instant};

use bytes::Bytes;
use ratatui::layout::Rect;

use super::App;

/// How long the resume nudge waits for the agent to finish booting before it
/// gives up. A native resume replays the whole conversation, so a large session
/// can take a while to reach its prompt.
const RESUME_NUDGE_READY_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the agent has to hold `Idle` before the nudge is submitted. Boot
/// output can read as idle for a frame before the agent settles on its prompt.
pub(crate) const RESUME_NUDGE_IDLE_HOLD: Duration = Duration::from_millis(1_500);
/// Gap between the nudge text and its Enter, matching `agent prompt`.
const RESUME_NUDGE_SUBMIT_DELAY: Duration = Duration::from_millis(300);
/// Poll interval while a nudge is armed and the agent is still coming up.
const RESUME_NUDGE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MIN_RESUME_COLS: u16 = 4;
const MIN_RESUME_ROWS: u16 = 2;
const MAX_PENDING_RESUME_ATTEMPTS: u8 = 5;
const MAX_PENDING_RESUME_BACKOFF: Duration = Duration::from_secs(30);

/// A resumed agent that still owes us a "continue".
#[derive(Debug, Clone)]
pub(crate) struct ResumeNudge {
    pane_id: crate::layout::PaneId,
    agent: crate::detect::Agent,
    expires_at: Instant,
    idle_since: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingAgentResumeRetry {
    dedupe_key: String,
    attempts: u8,
    retry_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingAgentResumeFailure {
    RetryAt(Instant),
    Exhausted,
    AlreadyExhausted,
}

impl PendingAgentResumeRetry {
    fn new(dedupe_key: String, now: Instant) -> Self {
        Self {
            dedupe_key,
            attempts: 0,
            retry_at: Some(now),
        }
    }

    fn can_attempt_at(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|retry_at| now >= retry_at)
    }

    fn record_failure_at(&mut self, now: Instant) -> PendingAgentResumeFailure {
        if self.retry_at.is_none() {
            return PendingAgentResumeFailure::AlreadyExhausted;
        }
        self.attempts = self.attempts.saturating_add(1);
        if self.attempts >= MAX_PENDING_RESUME_ATTEMPTS {
            self.retry_at = None;
            return PendingAgentResumeFailure::Exhausted;
        }
        let retry_at = now + pending_resume_retry_delay(self.attempts);
        self.retry_at = Some(retry_at);
        PendingAgentResumeFailure::RetryAt(retry_at)
    }
}

struct PendingAgentResumeCandidate {
    pane_id: crate::layout::PaneId,
    terminal_id: crate::terminal::TerminalId,
    cwd: std::path::PathBuf,
    plan: crate::agent_resume::AgentResumePlan,
    rows: u16,
    cols: u16,
}

fn persisted_session_matches_resume_key(
    terminal: &crate::terminal::TerminalState,
    key: &str,
) -> bool {
    terminal
        .persisted_agent_session
        .as_ref()
        .is_some_and(|session| {
            crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
                .is_some_and(|plan| plan.dedupe_key == key)
        })
}

impl App {
    pub(crate) fn has_pending_agent_resumes(&self) -> bool {
        self.state
            .terminals
            .values()
            .any(|terminal| terminal.pending_agent_resume_plan.is_some())
    }

    pub(crate) fn sync_pending_agent_resume_deadline(&mut self, now: Instant) {
        if !self.has_pending_agent_resumes() {
            self.pending_agent_resume_retries.clear();
            self.pending_agent_resume_deadline = None;
            return;
        }
        let candidates = self.pending_agent_resume_candidates();
        let terminals = &self.state.terminals;
        self.pending_agent_resume_retries
            .retain(|terminal_id, retry| {
                terminals
                    .get(terminal_id)
                    .and_then(|terminal| terminal.pending_agent_resume_plan.as_ref())
                    .is_some_and(|plan| plan.dedupe_key == retry.dedupe_key)
            });
        if candidates.is_empty() {
            self.pending_agent_resume_deadline = None;
            return;
        }

        let mut next_retry = None;
        let mut has_first_attempt = false;
        for candidate in &candidates {
            match self
                .pending_agent_resume_retries
                .get(&candidate.terminal_id)
            {
                Some(retry) => {
                    if let Some(retry_at) = retry.retry_at {
                        next_retry = Some(
                            next_retry.map_or(retry_at, |deadline: Instant| deadline.min(retry_at)),
                        );
                    }
                }
                None => has_first_attempt = true,
            }
        }

        if has_first_attempt {
            let theme_deadline = self
                .pending_agent_resume_deadline
                .unwrap_or(now + super::PENDING_AGENT_RESUME_THEME_WAIT);
            next_retry =
                Some(next_retry.map_or(theme_deadline, |deadline| deadline.min(theme_deadline)));
        }
        self.pending_agent_resume_deadline = next_retry;
    }

    pub(crate) fn pending_agent_resume_due(&self, now: Instant) -> bool {
        self.pending_agent_resume_deadline
            .is_some_and(|deadline| now >= deadline)
    }

    pub(crate) fn start_pending_agent_resumes(&mut self, allow_empty_theme: bool) -> bool {
        self.start_pending_agent_resumes_at(allow_empty_theme, Instant::now())
    }

    pub(crate) fn start_pending_agent_resumes_at(
        &mut self,
        allow_empty_theme: bool,
        now: Instant,
    ) -> bool {
        let pending = self.pending_agent_resume_candidates();
        let mut changed = false;
        for PendingAgentResumeCandidate {
            pane_id,
            terminal_id,
            cwd,
            plan,
            rows,
            cols,
        } in pending
        {
            if self.terminal_runtimes.get(&terminal_id).is_some() {
                continue;
            }
            if self
                .pending_agent_resume_retries
                .get(&terminal_id)
                .is_some_and(|retry| {
                    retry.dedupe_key == plan.dedupe_key && !retry.can_attempt_at(now)
                })
            {
                continue;
            }
            changed |= self.start_pending_agent_resume(
                pane_id,
                terminal_id,
                cwd,
                plan,
                rows,
                cols,
                allow_empty_theme,
                now,
            );
        }

        if changed {
            self.schedule_session_save();
        }
        self.sync_pending_agent_resume_deadline(now);
        changed
    }

    fn pending_agent_resume_candidates(&self) -> Vec<PendingAgentResumeCandidate> {
        let terminal_area = self.state.view.terminal_area;
        if terminal_area.width == 0 || terminal_area.height == 0 {
            return Vec::new();
        };

        let mut pending = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                for info in
                    self.pending_agent_resume_pane_infos(ws_idx, tab_idx, tab, terminal_area)
                {
                    let Some(pane) = tab.panes.get(&info.id) else {
                        continue;
                    };
                    if pane.settled_at.is_some() {
                        continue;
                    }
                    if self
                        .terminal_runtimes
                        .get(&pane.attached_terminal_id)
                        .is_some()
                    {
                        continue;
                    }
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    let Some(plan) = terminal.pending_agent_resume_plan.clone() else {
                        continue;
                    };
                    pending.push(PendingAgentResumeCandidate {
                        pane_id: info.id,
                        terminal_id: pane.attached_terminal_id.clone(),
                        cwd: terminal.cwd.clone(),
                        plan,
                        rows: info.inner_rect.height,
                        cols: info.inner_rect.width,
                    });
                }
            }
        }
        pending
    }

    fn pending_agent_resume_pane_infos(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        tab: &crate::workspace::Tab,
        terminal_area: Rect,
    ) -> Vec<crate::layout::PaneInfo> {
        let mut pane_infos = derived_pending_agent_resume_pane_infos(
            tab,
            terminal_area,
            self.state.pane_borders,
            self.state.pane_gaps,
            self.state.pane_outer_borders,
        );

        if self.state.active == Some(ws_idx)
            && self
                .state
                .workspaces
                .get(ws_idx)
                .is_some_and(|ws| tab_idx == ws.active_tab_index())
        {
            for visible_info in &self.state.view.pane_infos {
                if let Some(info) = pane_infos
                    .iter_mut()
                    .find(|info| info.id == visible_info.id)
                {
                    *info = visible_info.clone();
                } else {
                    pane_infos.push(visible_info.clone());
                }
            }
        }

        pane_infos
    }

    pub(crate) fn start_pending_agent_resume_for_terminal(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        rows: u16,
        cols: u16,
        allow_empty_theme: bool,
    ) -> bool {
        if self.terminal_runtimes.get(terminal_id).is_some() {
            return false;
        }
        let Some((pane_id, cwd, plan)) = self.state.workspaces.iter().find_map(|ws| {
            ws.tabs.iter().find_map(|tab| {
                tab.layout.pane_ids().into_iter().find_map(|pane_id| {
                    let pane = tab.panes.get(&pane_id)?;
                    if &pane.attached_terminal_id != terminal_id {
                        return None;
                    }
                    let terminal = self.state.terminals.get(terminal_id)?;
                    Some((
                        pane_id,
                        terminal.cwd.clone(),
                        terminal.pending_agent_resume_plan.clone()?,
                    ))
                })
            })
        }) else {
            return false;
        };

        let now = Instant::now();
        if self
            .pending_agent_resume_retries
            .get(terminal_id)
            .is_some_and(|retry| retry.dedupe_key == plan.dedupe_key && !retry.can_attempt_at(now))
        {
            return false;
        }

        let changed = self.start_pending_agent_resume(
            pane_id,
            terminal_id.clone(),
            cwd,
            plan,
            rows,
            cols,
            allow_empty_theme,
            now,
        );
        if changed {
            self.schedule_session_save();
        }
        self.sync_pending_agent_resume_deadline(now);
        changed
    }

    fn start_pending_agent_resume(
        &mut self,
        pane_id: crate::layout::PaneId,
        terminal_id: crate::terminal::TerminalId,
        cwd: std::path::PathBuf,
        plan: crate::agent_resume::AgentResumePlan,
        rows: u16,
        cols: u16,
        allow_empty_theme: bool,
        now: Instant,
    ) -> bool {
        let duplicate_session = self.state.terminals.iter().any(|(other_id, terminal)| {
            other_id != &terminal_id
                && self.terminal_runtimes.get(other_id).is_some()
                && persisted_session_matches_resume_key(terminal, &plan.dedupe_key)
        });
        if duplicate_session {
            tracing::warn!(
                pane = pane_id.raw(),
                terminal = %terminal_id,
                session = %plan.dedupe_key,
                "skipping agent resume because another live pane already runs this session"
            );
            return false;
        }
        if self.state.host_terminal_theme.is_empty() && !allow_empty_theme {
            return false;
        }
        let host_terminal_theme = self.state.pane_terminal_theme();

        let Some(resume_command) = shell_command_from_argv(&plan.argv) else {
            self.record_pending_agent_resume_failure(
                pane_id,
                &terminal_id,
                &plan.agent,
                &plan.dedupe_key,
                "resume command has empty argv".into(),
                now,
            );
            return false;
        };
        let Some(launch_env) = self
            .find_pane(pane_id)
            .and_then(|(ws_idx, _)| self.pane_launch_env(ws_idx, pane_id, Vec::new()))
        else {
            self.record_pending_agent_resume_failure(
                pane_id,
                &terminal_id,
                &plan.agent,
                &plan.dedupe_key,
                "pane launch environment is unavailable".into(),
                now,
            );
            return false;
        };

        let (rows, cols) = pending_resume_spawn_size(rows, cols);
        let runtime = match crate::terminal::TerminalRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            host_terminal_theme,
            Some(self.state.pane_terminal_appearance()),
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            &launch_env,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(err) => {
                let hook_work_context_changed = self
                    .state
                    .terminals
                    .get_mut(&terminal_id)
                    .is_some_and(|terminal| {
                        let changed = terminal.clear_agent_runtime_identity_after_respawn();
                        terminal.pending_agent_resume_plan = Some(plan.clone());
                        changed
                    });
                if hook_work_context_changed {
                    self.schedule_session_save();
                    if let Some((ws_idx, _)) = self.find_pane(pane_id) {
                        self.emit_pane_updated(ws_idx, pane_id);
                    }
                }
                self.record_pending_agent_resume_failure(
                    pane_id,
                    &terminal_id,
                    &plan.agent,
                    &plan.dedupe_key,
                    err.to_string(),
                    now,
                );
                return false;
            }
        };

        let mut input = resume_command;
        input.push('\r');
        if let Err(err) = runtime.try_send_bytes(Bytes::from(input)) {
            runtime.shutdown();
            self.record_pending_agent_resume_failure(
                pane_id,
                &terminal_id,
                &plan.agent,
                &plan.dedupe_key,
                err.to_string(),
                now,
            );
            return false;
        }

        self.pending_agent_resume_retries.remove(&terminal_id);
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.pending_agent_resume_plan = None;
            terminal.respawn_shell_on_exit = false;
        }
        self.arm_resume_nudge(pane_id, &terminal_id, &plan.agent);
        true
    }

    fn record_pending_agent_resume_failure(
        &mut self,
        pane_id: crate::layout::PaneId,
        terminal_id: &crate::terminal::TerminalId,
        agent: &str,
        dedupe_key: &str,
        error: String,
        now: Instant,
    ) {
        let retry = self.pending_agent_resume_retries.entry(terminal_id.clone());
        let retry = match retry {
            std::collections::hash_map::Entry::Occupied(mut entry)
                if entry.get().dedupe_key != dedupe_key =>
            {
                entry.insert(PendingAgentResumeRetry::new(dedupe_key.into(), now));
                entry.into_mut()
            }
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(PendingAgentResumeRetry::new(dedupe_key.into(), now))
            }
        };
        match retry.record_failure_at(now) {
            PendingAgentResumeFailure::RetryAt(retry_at) => {
                tracing::debug!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    agent,
                    attempt = retry.attempts,
                    retry_in_ms = retry_at.saturating_duration_since(now).as_millis(),
                    err = %error,
                    "deferred agent resume failed; retry scheduled"
                );
                self.pending_agent_resume_deadline = Some(retry_at);
            }
            PendingAgentResumeFailure::Exhausted => {
                tracing::warn!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    agent,
                    attempts = retry.attempts,
                    err = %error,
                    "deferred agent resume stopped after retry limit"
                );
                self.pending_agent_resume_deadline = None;
            }
            PendingAgentResumeFailure::AlreadyExhausted => {}
        }
    }

    /// Queue a "continue" for a pane that was just resumed into a native agent
    /// session. The resume only replays the conversation; without this the
    /// agent sits at an idle prompt and the work it was doing stops there.
    pub(crate) fn arm_resume_nudge(
        &mut self,
        pane_id: crate::layout::PaneId,
        terminal_id: &crate::terminal::TerminalId,
        agent_label: &str,
    ) {
        if !self.state.nudge_resumed_agents {
            return;
        }
        if self.state.resume_nudge_message.trim().is_empty() {
            return;
        }
        let Some(agent) = crate::detect::parse_agent_label(agent_label) else {
            tracing::debug!(
                pane = pane_id.raw(),
                agent = %agent_label,
                "skipping resume nudge for an agent herdr cannot detect on screen"
            );
            return;
        };
        let now = Instant::now();
        self.pending_resume_nudges.insert(
            terminal_id.clone(),
            ResumeNudge {
                pane_id,
                agent,
                expires_at: now + RESUME_NUDGE_READY_TIMEOUT,
                idle_since: None,
            },
        );
    }

    pub(crate) fn next_resume_nudge_deadline(&self) -> Option<Instant> {
        self.pending_resume_nudges
            .values()
            .map(|nudge| match nudge.idle_since {
                Some(idle_since) => (idle_since + RESUME_NUDGE_IDLE_HOLD).min(nudge.expires_at),
                None => nudge.expires_at,
            })
            .min()
            .map(|deadline| deadline.min(Instant::now() + RESUME_NUDGE_POLL_INTERVAL))
    }

    /// Submit the queued "continue" to every resumed agent that has come back
    /// up idle and ready. Blocked agents, agents that resumed straight into
    /// work, and panes holding a human draft are dropped without a nudge.
    pub(crate) fn tick_resume_nudges(&mut self, now: Instant) -> bool {
        if self.pending_resume_nudges.is_empty() {
            return false;
        }

        let mut fire: Vec<(crate::terminal::TerminalId, ResumeNudge)> = Vec::new();
        let mut drop_ids: Vec<crate::terminal::TerminalId> = Vec::new();
        let mut idle_marks: Vec<(crate::terminal::TerminalId, Option<Instant>)> = Vec::new();

        for (terminal_id, nudge) in &self.pending_resume_nudges {
            match self.resume_nudge_readiness(terminal_id, nudge) {
                ResumeNudgeStep::Drop(reason) => {
                    tracing::debug!(
                        pane = nudge.pane_id.raw(),
                        terminal = %terminal_id,
                        agent = ?nudge.agent,
                        reason,
                        "dropping resume nudge"
                    );
                    drop_ids.push(terminal_id.clone());
                }
                ResumeNudgeStep::Wait => {
                    if nudge.idle_since.is_some() {
                        idle_marks.push((terminal_id.clone(), None));
                    }
                    if now >= nudge.expires_at {
                        tracing::debug!(
                            pane = nudge.pane_id.raw(),
                            terminal = %terminal_id,
                            agent = ?nudge.agent,
                            "resume nudge timed out before the agent was ready"
                        );
                        drop_ids.push(terminal_id.clone());
                    }
                }
                ResumeNudgeStep::Idle => match nudge.idle_since {
                    Some(idle_since)
                        if now.saturating_duration_since(idle_since) >= RESUME_NUDGE_IDLE_HOLD =>
                    {
                        fire.push((terminal_id.clone(), nudge.clone()));
                    }
                    Some(_) => {}
                    None => idle_marks.push((terminal_id.clone(), Some(now))),
                },
            }
        }

        for (terminal_id, idle_since) in idle_marks {
            if let Some(nudge) = self.pending_resume_nudges.get_mut(&terminal_id) {
                nudge.idle_since = idle_since;
            }
        }
        for terminal_id in drop_ids {
            self.pending_resume_nudges.remove(&terminal_id);
        }

        let mut changed = false;
        for (terminal_id, nudge) in fire {
            self.pending_resume_nudges.remove(&terminal_id);
            changed |= self.send_resume_nudge(&terminal_id, &nudge, now);
        }
        changed
    }

    fn resume_nudge_readiness(
        &self,
        terminal_id: &crate::terminal::TerminalId,
        nudge: &ResumeNudge,
    ) -> ResumeNudgeStep {
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return ResumeNudgeStep::Drop("terminal is gone");
        };
        let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
            return ResumeNudgeStep::Drop("pane has no runtime");
        };
        resume_nudge_step(&ResumeNudgeFacts {
            parked_for_resume: terminal.pending_agent_resume_plan.is_some(),
            human_draft: self
                .state
                .pending_human_drafts
                .get(&nudge.pane_id)
                .is_some_and(|draft| !draft.is_empty()),
            launch_pending: terminal.managed_agent_launch_pending(),
            agent_matches: terminal.effective_known_agent() == Some(nudge.agent),
            hosts_agent: super::agents::runtime_hosts_agent(runtime, nudge.agent),
            state: terminal.raw_agent_state(),
        })
    }

    fn send_resume_nudge(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        nudge: &ResumeNudge,
        now: Instant,
    ) -> bool {
        let message = self.state.resume_nudge_message.clone();
        let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
            return false;
        };
        let (text, enter) = crate::app::api_helpers::encode_api_submission_parts(runtime, &message);
        if let Err(err) = runtime.try_send_bytes(Bytes::from(text)) {
            tracing::warn!(
                pane = nudge.pane_id.raw(),
                terminal = %terminal_id,
                agent = ?nudge.agent,
                err = %err,
                "failed to send resume nudge to a resumed agent"
            );
            return false;
        }
        if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
            terminal.clear_auto_settle_user_reply();
        }
        runtime.send_bytes_after(Bytes::from(enter), RESUME_NUDGE_SUBMIT_DELAY);
        self.retire_blocked_hook_authority_for_automated_input(nudge.pane_id, now);
        tracing::info!(
            pane = nudge.pane_id.raw(),
            terminal = %terminal_id,
            agent = ?nudge.agent,
            "nudged a resumed agent to continue"
        );
        true
    }
}

/// Everything the nudge decision needs, read off the pane in one pass.
struct ResumeNudgeFacts {
    parked_for_resume: bool,
    human_draft: bool,
    launch_pending: bool,
    agent_matches: bool,
    hosts_agent: bool,
    state: crate::detect::AgentState,
}

/// Decide what to do with an armed nudge.
///
/// A resumed agent is only worth prompting while it is idle at its own prompt.
/// Blocked means a human owes it an answer, working means it already picked the
/// thread back up, and a draft in the composer means anything we submit would
/// carry the human's half-typed text with it.
fn resume_nudge_step(facts: &ResumeNudgeFacts) -> ResumeNudgeStep {
    if facts.parked_for_resume {
        return ResumeNudgeStep::Drop("pane was parked for another resume");
    }
    if facts.human_draft {
        return ResumeNudgeStep::Drop("pane holds a draft the human typed");
    }
    if facts.launch_pending || !facts.agent_matches || !facts.hosts_agent {
        return ResumeNudgeStep::Wait;
    }
    match facts.state {
        crate::detect::AgentState::Blocked => {
            ResumeNudgeStep::Drop("agent resumed blocked on a question")
        }
        crate::detect::AgentState::Working => {
            ResumeNudgeStep::Drop("agent resumed straight back into work")
        }
        crate::detect::AgentState::Idle => ResumeNudgeStep::Idle,
        crate::detect::AgentState::Unknown => ResumeNudgeStep::Wait,
    }
}

enum ResumeNudgeStep {
    /// The agent is up, idle, and safe to prompt.
    Idle,
    /// Still booting; check again on the next tick.
    Wait,
    /// Nothing to nudge; forget this pane.
    Drop(&'static str),
}

fn derived_pending_agent_resume_pane_infos(
    tab: &crate::workspace::Tab,
    terminal_area: Rect,
    pane_borders: bool,
    pane_gaps: bool,
    pane_outer_borders: bool,
) -> Vec<crate::layout::PaneInfo> {
    crate::ui::apply_pane_chrome(
        tab.layout.panes(terminal_area),
        pane_borders,
        pane_gaps,
        pane_outer_borders,
    )
    .into_iter()
    .map(|mut info| {
        let pane_inner = crate::ui::pane_inner_rect(info.rect, info.borders);
        info.inner_rect = stable_terminal_inner_rect(pane_inner);
        info
    })
    .collect()
}

fn stable_terminal_inner_rect(pane_inner: Rect) -> Rect {
    if pane_inner.width <= 4 {
        return pane_inner;
    }

    Rect::new(
        pane_inner.x,
        pane_inner.y,
        pane_inner.width.saturating_sub(1),
        pane_inner.height,
    )
}

fn pending_resume_spawn_size(rows: u16, cols: u16) -> (u16, u16) {
    (rows.max(MIN_RESUME_ROWS), cols.max(MIN_RESUME_COLS))
}

fn pending_resume_retry_delay(attempt: u8) -> Duration {
    let shift = u32::from(attempt.saturating_sub(1).min(5));
    Duration::from_secs((1_u64 << shift).min(MAX_PENDING_RESUME_BACKOFF.as_secs()))
}

fn shell_command_from_argv(argv: &[String]) -> Option<String> {
    let mut parts = argv.iter();
    let first = shell_quote(parts.next()?);
    let mut command = first;
    for part in parts {
        command.push(' ');
        command.push_str(&shell_quote(part));
    }
    Some(command)
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'%' | b'+' | b'='
            )
    }) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_live_pane_with_the_same_session_key_blocks_a_duplicate_resume() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("duplicate-session");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("same-session").unwrap(),
        });
        let plan = crate::agent_resume::plan(
            "herdr:claude",
            "claude",
            &crate::agent_resume::AgentSessionRef::id("same-session").unwrap(),
        )
        .unwrap();
        assert!(persisted_session_matches_resume_key(
            terminal,
            &plan.dedupe_key
        ));
        assert!(!persisted_session_matches_resume_key(
            terminal,
            "different-session"
        ));
    }

    #[cfg(unix)]
    fn long_running_test_argv() -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()]
    }

    #[cfg(unix)]
    fn marker_resume_test_argv() -> Vec<String> {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s' 'restored agent: shell quoted | marker'; sleep 5".into(),
        ]
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_agent_resume_waits_for_host_theme_before_launch() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        let pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.view.pane_infos = pane_infos;
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist");
        terminal.pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: marker_resume_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        assert!(!app.start_pending_agent_resumes(false));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());

        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };

        assert!(app.start_pending_agent_resumes(false));
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal should survive launch");
        assert!(terminal.pending_agent_resume_plan.is_none());
        assert!(!terminal.respawn_shell_on_exit);

        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("pending resume should leave a shell runtime");
        let marker = "restored agent: shell quoted | marker";
        for _ in 0..20 {
            if runtime
                .snapshot_history()
                .is_some_and(|text| text.contains(marker))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            runtime
                .snapshot_history()
                .expect("runtime should expose terminal history")
                .contains(marker),
            "deferred restore should inject the resume argv into the restored shell"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_agent_resume_can_launch_after_theme_wait_expires() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        app.sync_pending_agent_resume_deadline(std::time::Instant::now());
        assert!(!app.start_pending_agent_resumes(false));
        assert!(app.start_pending_agent_resumes(true));
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_launches_hidden_panes_with_current_terminal_area() {
        let mut app = test_app();
        let active_workspace = crate::workspace::Workspace::test_new("active");
        let active_pane = active_workspace.tabs[0].root_pane;
        let active_terminal = active_workspace.terminal_id(active_pane).cloned().unwrap();
        let hidden_workspace = crate::workspace::Workspace::test_new("hidden");
        let hidden_pane = hidden_workspace.tabs[0].root_pane;
        let hidden_terminal = hidden_workspace.terminal_id(hidden_pane).cloned().unwrap();
        app.state.view.pane_infos = active_workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![active_workspace, hidden_workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        for terminal_id in [&active_terminal, &hidden_terminal] {
            app.state
                .terminals
                .get_mut(terminal_id)
                .expect("test terminal should exist")
                .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
                agent: "codex".into(),
                argv: long_running_test_argv(),
                dedupe_key: format!("herdr:codex\0codex\0Id\0{terminal_id}"),
            });
        }
        app.pending_agent_resume_deadline =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1));

        assert!(app.start_pending_agent_resumes(false));
        assert!(app.terminal_runtimes.get(&active_terminal).is_some());
        assert!(app.terminal_runtimes.get(&hidden_terminal).is_some());
        assert!(
            app.pending_agent_resume_deadline.is_none(),
            "launched pending resumes should clear the wakeup deadline"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_launches_inactive_tab_panes_with_current_terminal_area() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("tabs");
        let active_pane = workspace.tabs[0].root_pane;
        let inactive_tab = workspace.test_add_tab(Some("agents"));
        let inactive_pane = workspace.tabs[inactive_tab].root_pane;
        let inactive_terminal = workspace.tabs[inactive_tab]
            .terminal_id(inactive_pane)
            .cloned()
            .unwrap();
        app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        assert!(app
            .state
            .workspaces
            .first()
            .and_then(|ws| ws.tabs[0].terminal_id(active_pane))
            .is_some());
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&inactive_terminal)
            .expect("inactive tab terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0inactive-tab-session".into(),
        });

        assert!(app.start_pending_agent_resumes(false));
        assert!(app.terminal_runtimes.get(&inactive_terminal).is_some());
        assert!(
            app.state
                .terminals
                .get(&inactive_terminal)
                .expect("inactive tab terminal should still exist")
                .pending_agent_resume_plan
                .is_none(),
            "inactive tab restored panes should not wait for tab focus"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_launches_zoom_hidden_active_tab_panes() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("zoomed");
        let hidden_pane = workspace.tabs[0].root_pane;
        let visible_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].zoomed = true;
        let hidden_terminal = workspace.terminal_id(hidden_pane).cloned().unwrap();
        app.state.view.pane_infos = vec![crate::layout::PaneInfo {
            id: visible_pane,
            rect: ratatui::layout::Rect::new(0, 0, 100, 30),
            inner_rect: ratatui::layout::Rect::new(1, 1, 98, 28),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        }];
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&hidden_terminal)
            .expect("hidden zoom pane terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0zoom-hidden-session".into(),
        });

        assert!(app.start_pending_agent_resumes(false));
        assert!(app.terminal_runtimes.get(&hidden_terminal).is_some());
        assert!(
            app.state
                .terminals
                .get(&hidden_terminal)
                .expect("hidden zoom pane terminal should still exist")
                .pending_agent_resume_plan
                .is_none(),
            "zoom-hidden restored panes should not wait for pane focus"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_uses_current_terminal_area_for_background_panes() {
        let mut app = test_app();
        let previous_workspace = crate::workspace::Workspace::test_new("previous");
        let previous_pane = previous_workspace.tabs[0].root_pane;
        let previous_terminal = previous_workspace
            .terminal_id(previous_pane)
            .cloned()
            .unwrap();
        let current_workspace = crate::workspace::Workspace::test_new("current");
        app.state.view.pane_infos = previous_workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        app.state.workspaces = vec![previous_workspace, current_workspace];
        app.state.active = Some(1);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&previous_terminal)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        app.sync_pending_agent_resume_deadline(std::time::Instant::now());
        assert!(app.pending_agent_resume_deadline.is_some());
        assert!(app.start_pending_agent_resumes(false));
        assert!(app.terminal_runtimes.get(&previous_terminal).is_some());
        assert!(
            app.state
                .terminals
                .get(&previous_terminal)
                .expect("previous terminal should still exist")
                .pending_agent_resume_plan
                .is_none(),
            "background restored panes should not wait for focus once terminal area is known"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_agent_resume_launches_with_inner_rect_size() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("split");
        let pane_id = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.view.pane_infos = vec![crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 100, 30),
            inner_rect: ratatui::layout::Rect::new(1, 1, 98, 28),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        }];
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        assert!(app.start_pending_agent_resumes(false));
        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .expect("pending resume should launch")
                .current_size(),
            (28, 98)
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_agent_resume_clamps_tiny_inner_rect_before_spawn() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("tiny-resume");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.view.pane_infos = vec![crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 1, 1),
            inner_rect: ratatui::layout::Rect::new(0, 0, 0, 0),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        }];
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 1, 1);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0tiny-session".into(),
        });

        assert!(app.start_pending_agent_resumes(false));
        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .expect("pending resume should launch")
                .current_size(),
            (MIN_RESUME_ROWS, MIN_RESUME_COLS)
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn settled_pane_is_skipped_by_pending_resume_candidates() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("settled-restore");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .unwrap()
            .settled_at = Some(1_725_000_020);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "settled-restore-test".into(),
        });

        assert!(app.pending_agent_resume_candidates().is_empty());
        assert!(!app.start_pending_agent_resumes(true));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
    }

    #[test]
    fn shell_command_from_argv_quotes_resume_arguments() {
        let argv = vec![
            "claude".to_string(),
            "--resume".to_string(),
            "session with ' quote".to_string(),
        ];

        assert_eq!(
            shell_command_from_argv(&argv).as_deref(),
            Some("claude --resume 'session with '\\'' quote'")
        );
        assert_eq!(shell_command_from_argv(&[]), None);
    }

    #[test]
    fn pending_agent_resume_retries_back_off_and_stop_after_five_attempts() {
        let mut now = Instant::now();
        let mut retry = PendingAgentResumeRetry::new("resume-key".into(), now);

        for expected_delay in [1, 2, 4, 8] {
            let retry_at = now + Duration::from_secs(expected_delay);
            assert_eq!(
                retry.record_failure_at(now),
                PendingAgentResumeFailure::RetryAt(retry_at)
            );
            assert!(!retry.can_attempt_at(retry_at - Duration::from_millis(1)));
            assert!(retry.can_attempt_at(retry_at));
            now = retry_at;
        }

        assert_eq!(
            retry.record_failure_at(now),
            PendingAgentResumeFailure::Exhausted
        );
        assert!(!retry.can_attempt_at(now + Duration::from_secs(60)));
        assert_eq!(
            retry.record_failure_at(now + Duration::from_secs(60)),
            PendingAgentResumeFailure::AlreadyExhausted,
            "an exhausted terminal must not emit another terminal warning"
        );
        assert_eq!(pending_resume_retry_delay(6), Duration::from_secs(30));
        assert_eq!(pending_resume_retry_delay(u8::MAX), Duration::from_secs(30));
    }

    #[cfg(unix)]
    #[test]
    fn syncing_without_pending_resumes_clears_failed_retry_state() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("retry-cleared");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let now = Instant::now();

        app.record_pending_agent_resume_failure(
            pane_id,
            &terminal_id,
            "codex",
            "retry-cleared",
            "test failure".into(),
            now,
        );
        assert!(!app.pending_agent_resume_retries.is_empty());
        assert!(app.pending_agent_resume_deadline.is_some());

        app.sync_pending_agent_resume_deadline(now);

        assert!(app.pending_agent_resume_retries.is_empty());
        assert!(app.pending_agent_resume_deadline.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn pending_agent_resume_retry_budget_survives_temporary_zero_geometry() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("retry-geometry");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        let dedupe_key = "herdr:codex\0codex\0Id\0retry-geometry".to_string();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: dedupe_key.clone(),
        });
        let now = Instant::now();
        let retry_at = now + Duration::from_secs(8);
        app.pending_agent_resume_retries.insert(
            terminal_id.clone(),
            PendingAgentResumeRetry {
                dedupe_key,
                attempts: 4,
                retry_at: Some(retry_at),
            },
        );

        app.state.view.terminal_area = Rect::new(0, 0, 0, 0);
        app.sync_pending_agent_resume_deadline(now);
        assert!(app.pending_agent_resume_retries.contains_key(&terminal_id));
        assert!(app.pending_agent_resume_deadline.is_none());

        app.state.view.terminal_area = Rect::new(0, 0, 100, 30);
        app.sync_pending_agent_resume_deadline(now);
        assert_eq!(app.pending_agent_resume_deadline, Some(retry_at));
        assert_eq!(app.pending_agent_resume_retries[&terminal_id].attempts, 4);
    }

    #[cfg(unix)]
    #[test]
    fn direct_resume_entry_does_not_reset_an_exhausted_retry_budget() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("retry-exhausted");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        let dedupe_key = "herdr:codex\0codex\0Id\0retry-exhausted".to_string();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: dedupe_key.clone(),
        });
        app.pending_agent_resume_retries.insert(
            terminal_id.clone(),
            PendingAgentResumeRetry {
                dedupe_key,
                attempts: MAX_PENDING_RESUME_ATTEMPTS,
                retry_at: None,
            },
        );

        assert!(!app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true,));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        let retry = &app.pending_agent_resume_retries[&terminal_id];
        assert_eq!(retry.attempts, MAX_PENDING_RESUME_ATTEMPTS);
        assert!(retry.retry_at.is_none());
    }
}

#[cfg(test)]
mod resume_nudge_tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::terminal::{TerminalId, TerminalRuntime};
    use crate::workspace::Workspace;

    fn ready_facts(state: AgentState) -> ResumeNudgeFacts {
        ResumeNudgeFacts {
            parked_for_resume: false,
            human_draft: false,
            launch_pending: false,
            agent_matches: true,
            hosts_agent: true,
            state,
        }
    }

    fn step(facts: &ResumeNudgeFacts) -> &'static str {
        match resume_nudge_step(facts) {
            ResumeNudgeStep::Idle => "idle",
            ResumeNudgeStep::Wait => "wait",
            ResumeNudgeStep::Drop(_) => "drop",
        }
    }

    #[test]
    fn an_idle_resumed_agent_is_ready_for_its_nudge() {
        assert_eq!(step(&ready_facts(AgentState::Idle)), "idle");
    }

    #[test]
    fn a_blocked_agent_is_never_nudged() {
        assert_eq!(step(&ready_facts(AgentState::Blocked)), "drop");
    }

    #[test]
    fn an_agent_that_resumed_into_work_is_never_nudged() {
        assert_eq!(step(&ready_facts(AgentState::Working)), "drop");
    }

    #[test]
    fn a_human_draft_cancels_the_nudge() {
        let mut facts = ready_facts(AgentState::Idle);
        facts.human_draft = true;
        assert_eq!(step(&facts), "drop");
    }

    #[test]
    fn an_agent_that_is_still_booting_is_waited_out() {
        for facts in [
            ResumeNudgeFacts {
                launch_pending: true,
                ..ready_facts(AgentState::Idle)
            },
            ResumeNudgeFacts {
                agent_matches: false,
                ..ready_facts(AgentState::Idle)
            },
            ResumeNudgeFacts {
                hosts_agent: false,
                ..ready_facts(AgentState::Idle)
            },
            ready_facts(AgentState::Unknown),
        ] {
            assert_eq!(step(&facts), "wait");
        }
    }

    fn app_with_resumed_pane(
        state: AgentState,
    ) -> (App, TerminalId, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let config = crate::config::Config::default();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        let workspace = Workspace::test_new("resume-nudge");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(Agent::Claude), state);
        let (runtime, rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 1024, b"", 4);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        app.arm_resume_nudge(pane_id, &terminal_id, "claude");
        (app, terminal_id, rx)
    }

    fn drain(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> String {
        let mut out = String::new();
        while let Ok(bytes) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    #[tokio::test]
    async fn an_idle_resumed_agent_is_nudged_once_the_idle_hold_passes() {
        let (mut app, terminal_id, mut rx) = app_with_resumed_pane(AgentState::Idle);
        let armed_at = Instant::now();

        assert!(!app.tick_resume_nudges(armed_at));
        assert!(app.pending_resume_nudges.contains_key(&terminal_id));
        assert_eq!(drain(&mut rx), "");

        let due = armed_at + RESUME_NUDGE_IDLE_HOLD;
        assert!(app.tick_resume_nudges(due));
        assert!(!app.pending_resume_nudges.contains_key(&terminal_id));
        assert!(drain(&mut rx).contains("continue"));
    }

    #[tokio::test]
    async fn a_blocked_resumed_agent_is_dropped_without_a_nudge() {
        let (mut app, terminal_id, mut rx) = app_with_resumed_pane(AgentState::Blocked);
        let now = Instant::now();

        assert!(!app.tick_resume_nudges(now));
        assert!(!app.pending_resume_nudges.contains_key(&terminal_id));
        assert!(!app.tick_resume_nudges(now + RESUME_NUDGE_IDLE_HOLD));
        assert_eq!(drain(&mut rx), "");
    }

    #[tokio::test]
    async fn a_pane_that_never_becomes_ready_gives_up_at_the_timeout() {
        let (mut app, terminal_id, mut rx) = app_with_resumed_pane(AgentState::Unknown);
        let armed_at = Instant::now();

        assert!(!app.tick_resume_nudges(armed_at));
        assert!(app.pending_resume_nudges.contains_key(&terminal_id));

        assert!(!app.tick_resume_nudges(armed_at + RESUME_NUDGE_READY_TIMEOUT));
        assert!(!app.pending_resume_nudges.contains_key(&terminal_id));
        assert_eq!(drain(&mut rx), "");
    }

    #[tokio::test]
    async fn a_pane_that_stops_being_idle_has_to_hold_idle_again() {
        let (mut app, terminal_id, mut rx) = app_with_resumed_pane(AgentState::Idle);
        let armed_at = Instant::now();

        assert!(!app.tick_resume_nudges(armed_at));

        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(Agent::Claude), AgentState::Unknown);
        assert!(!app.tick_resume_nudges(armed_at + RESUME_NUDGE_IDLE_HOLD));
        assert_eq!(drain(&mut rx), "");

        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let restarted = armed_at + RESUME_NUDGE_IDLE_HOLD;
        assert!(!app.tick_resume_nudges(restarted));
        assert!(app.tick_resume_nudges(restarted + RESUME_NUDGE_IDLE_HOLD));
        assert!(drain(&mut rx).contains("continue"));
    }

    #[tokio::test]
    async fn the_nudge_is_not_armed_when_the_setting_is_off() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let config = crate::config::Config::default();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        let workspace = Workspace::test_new("resume-nudge-off");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        app.state.workspaces = vec![workspace];
        app.state.nudge_resumed_agents = false;

        app.arm_resume_nudge(pane_id, &terminal_id, "claude");
        assert!(app.pending_resume_nudges.is_empty());

        app.state.nudge_resumed_agents = true;
        app.state.resume_nudge_message = "   ".to_string();
        app.arm_resume_nudge(pane_id, &terminal_id, "claude");
        assert!(app.pending_resume_nudges.is_empty());

        app.state.resume_nudge_message = "continue".to_string();
        app.arm_resume_nudge(pane_id, &terminal_id, "definitely-not-an-agent");
        assert!(app.pending_resume_nudges.is_empty());

        app.arm_resume_nudge(pane_id, &terminal_id, "claude");
        assert!(app.pending_resume_nudges.contains_key(&terminal_id));
    }
}
