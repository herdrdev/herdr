use std::{
    collections::HashSet,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::layout::PaneId;

use super::{
    state::{PaneSettlementChange, PaneSnoozeChange},
    App, AppState,
};

pub(crate) const MAX_SNOOZE_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettleHint {
    Countdown(std::time::Duration),
    Focused(std::time::Duration),
    Pinned,
    SettledCountdown(std::time::Duration),
    SettledLocked,
}

/// Shared eligibility and remaining-time calculation for the settled pass and sidebar.
pub(crate) fn settle_countdown(
    enabled: bool,
    seen: bool,
    active: bool,
    pinned: bool,
    quiet: bool,
    settled_or_snoozed: bool,
    quiet_for: Option<Duration>,
    threshold: Duration,
) -> Option<SettleHint> {
    if !enabled || !seen || settled_or_snoozed || !quiet {
        return None;
    }
    let quiet_for = quiet_for.unwrap_or_default();
    // Pinned wins over focus: leaving a pinned pane still never settles it.
    if pinned {
        return Some(SettleHint::Pinned);
    }
    if active {
        return Some(SettleHint::Focused(threshold));
    }
    Some(SettleHint::Countdown(threshold.saturating_sub(quiet_for)))
}

/// UNIX deadlines survive restarts; converting at the scheduler boundary avoids
/// relying on a monotonic Instant from the previous process.
pub(crate) fn snooze_instant(deadline: u64, now_unix: u64, now: Instant) -> Instant {
    now.checked_add(Duration::from_secs(
        deadline.saturating_sub(now_unix).min(MAX_SNOOZE_SECONDS),
    ))
    .unwrap_or(now)
}

struct PaneSettlementCandidate {
    agent_ref: crate::api::schema::AgentRef,
    ws_idx: usize,
    pane_id: PaneId,
}

pub(crate) fn unix_seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

pub(crate) fn settled_read_only_remaining(
    settled_at: u64,
    now_unix: u64,
    after: Duration,
) -> Duration {
    after.saturating_sub(Duration::from_secs(now_unix.saturating_sub(settled_at)))
}

fn state_is(value: Option<&str>, expected: &str) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn work_context_settlement_key(
    context: &crate::work_context::PaneWorkContext,
    snapshot: Option<&crate::work_index::Snapshot>,
) -> Option<String> {
    let snapshot = snapshot?;
    let primary_pr = context.primary_pr();
    let pr_done = primary_pr.and_then(|primary| {
        snapshot.items.iter().find_map(|item| {
            let state = item.pr_state.as_deref()?;
            item.pr_url
                .as_deref()
                .is_some_and(|url| url.eq_ignore_ascii_case(primary))
                .then_some(())?;
            // A closed, unmerged pull request usually means work continues.
            // Only merging makes the pull request a settlement candidate.
            state_is(Some(state), "merged")
                .then(|| format!("pr:{}:merged", primary.to_ascii_lowercase()))
        })
    });
    if pr_done.is_some() {
        return pr_done;
    }
    context.ticket_ids.iter().find_map(|identifier| {
        snapshot.items.iter().find_map(|item| {
            let item_state_done = item
                .ticket_ids
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(identifier))
                && state_is(item.ticket_state.as_deref(), "done");
            let detail_done = item.ticket_details.iter().any(|ticket| {
                ticket.identifier.eq_ignore_ascii_case(identifier)
                    && state_is(ticket.state.as_deref(), "done")
            });
            (item_state_done || detail_done)
                .then(|| format!("ticket:{}:done", identifier.to_ascii_lowercase()))
        })
    })
}

fn primary_pr_settled_label(
    terminal: &crate::terminal::TerminalState,
    snapshot: Option<&crate::work_index::Snapshot>,
) -> Option<String> {
    let primary_pr = terminal.effective_work_context().primary_pr()?;
    let item = snapshot?.items.iter().find(|item| {
        item.pr_url
            .as_deref()
            .is_some_and(|url| url.eq_ignore_ascii_case(primary_pr))
    })?;
    let number = item.pr_number?;
    let title = item.pr_title.as_deref()?.trim();
    (!title.is_empty()).then(|| format!("#{number} {title}"))
}

/// Whether the pane can be brought back after its agent is stopped.
pub(crate) fn pane_has_resume_plan(terminal: &crate::terminal::TerminalState) -> bool {
    terminal
        .persisted_agent_session
        .as_ref()
        .is_some_and(|session| {
            crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
                .is_some()
        })
}

fn derived_pane_label(state: &AppState, ws_idx: usize, pane_id: PaneId) -> Option<String> {
    let workspace = state.workspaces.get(ws_idx)?;
    let tab_idx = workspace
        .tabs
        .iter()
        .position(|tab| tab.panes.contains_key(&pane_id))?;
    let projection = workspace.tab_display_projection(&state.terminals, tab_idx)?;
    match &projection {
        crate::workspace::TabDisplayProjection::Manual(label) => Some(label.clone()),
        crate::workspace::TabDisplayProjection::Derived { ticket, title, .. }
            if ticket.is_some() || title.is_some() =>
        {
            crate::workspace::session_title(Some(&projection), None)
        }
        crate::workspace::TabDisplayProjection::Derived { .. }
        | crate::workspace::TabDisplayProjection::Fallback(_) => None,
    }
}

impl AppState {
    pub(crate) fn next_snooze_deadline_at(&self, now: Instant, now_unix: u64) -> Option<Instant> {
        let local = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .filter_map(crate::pane::PaneState::snoozed_until)
            .map(|deadline| snooze_instant(deadline, now_unix, now));
        let remote = self
            .remote_agent_panel_entries
            .iter()
            .filter_map(|entry| entry.snoozed_until)
            .filter(|deadline| *deadline > now_unix)
            .map(|deadline| snooze_instant(deadline, now_unix, now));
        local.chain(remote).min()
    }

    pub(crate) fn snooze_pane_at(&mut self, ws_idx: usize, pane_id: PaneId, deadline: u64) -> bool {
        let Some(workspace) = self.workspaces.get_mut(ws_idx) else {
            return false;
        };
        let Some(pane) = workspace.pane_state_mut(pane_id) else {
            return false;
        };
        if pane.settled_at.is_some() || pane.snoozed_until() == Some(deadline) {
            return false;
        }
        if deadline == 0 {
            return false;
        }
        pane.set_snoozed_until(Some(deadline));
        let workspace_id = workspace.id.clone();
        self.pending_pane_snooze_changes.push(PaneSnoozeChange {
            workspace_id,
            pane_id,
            deadline: Some(deadline),
            reason: None,
        });
        self.mark_session_dirty();
        self.mark_sidebar_projection_changed();
        true
    }

    pub(crate) fn unsnooze_pane_at(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        reason: crate::api::schema::PaneUnsnoozeReason,
        now: Instant,
    ) -> bool {
        let Some(workspace) = self.workspaces.get_mut(ws_idx) else {
            return false;
        };
        let Some(pane) = workspace.pane_state_mut(pane_id) else {
            return false;
        };
        if pane.take_snoozed_until().is_none() {
            return false;
        }
        if reason == crate::api::schema::PaneUnsnoozeReason::Expired {
            pane.seen = false;
            pane.done_since = Some(now);
            // A long-quiet Done pane must not auto-settle in the same tick that
            // returns it to the unread queue.
            pane.activity.note(now);
        }
        let workspace_id = workspace.id.clone();
        self.pending_pane_snooze_changes.push(PaneSnoozeChange {
            workspace_id,
            pane_id,
            deadline: None,
            reason: Some(reason),
        });
        self.mark_session_dirty();
        self.mark_sidebar_projection_changed();
        true
    }

    pub(crate) fn refresh_snoozes_at(&mut self, now: Instant, now_unix: u64) -> bool {
        let expired = self
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.panes.iter().filter_map(move |(id, pane)| {
                        pane.snoozed_until()
                            .filter(|deadline| *deadline <= now_unix)
                            .map(|_| (ws_idx, *id))
                    })
                })
            })
            .collect::<Vec<_>>();
        for (ws_idx, id) in &expired {
            self.unsnooze_pane_at(
                *ws_idx,
                *id,
                crate::api::schema::PaneUnsnoozeReason::Expired,
                now,
            );
        }
        !expired.is_empty()
    }

    pub(crate) fn refresh_remote_snoozes_at(&mut self, now_unix: u64) -> bool {
        if !self
            .remote_agent_panel_entries
            .iter()
            .filter_map(|entry| entry.snoozed_until)
            .any(|deadline| deadline <= now_unix)
        {
            return false;
        }
        self.remote_agent_panel_entries = crate::ui::remote_agent_panel_entries_at(
            &self.fleet_snapshot,
            now_unix,
            self.nerd_font,
        );
        self.remote_agent_device_groups =
            Some(crate::ui::sidebar::remote_agent_device_groups(self));
        true
    }
    fn settle_owned_candidates(
        &mut self,
        candidates: &[PaneSettlementCandidate],
        now_unix: u64,
    ) -> usize {
        let mut settled = 0;
        for candidate in candidates {
            if candidate.agent_ref.host != self.agent_host_name {
                // This client does not own remote panes. Selection may point at
                // one, but settlement must never mutate a colliding local id.
                continue;
            }
            settled +=
                usize::from(self.settle_pane_at(candidate.ws_idx, candidate.pane_id, now_unix));
        }
        settled
    }

    fn pane_state_mut(&mut self, pane_id: PaneId) -> Option<(usize, &mut crate::pane::PaneState)> {
        self.workspaces
            .iter_mut()
            .enumerate()
            .find_map(|(ws_idx, workspace)| {
                workspace
                    .tabs
                    .iter_mut()
                    .find_map(|tab| tab.panes.get_mut(&pane_id))
                    .map(|pane| (ws_idx, pane))
            })
    }

    pub(crate) fn pane_is_settled(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .is_some_and(|pane| pane.settled_at.is_some())
    }

    pub(crate) fn pane_is_snoozed(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .is_some_and(|pane| pane.snoozed_until().is_some())
    }

    pub(crate) fn pane_can_snooze(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        if self.pane_is_settled(ws_idx, pane_id) {
            return false;
        }
        self.workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| {
                self.terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| !pane.agent_projection(terminal).needs_human_attention())
            })
            .unwrap_or(false)
    }

    pub(crate) fn pane_is_settled_anywhere(&self, pane_id: PaneId) -> bool {
        self.workspaces.iter().any(|workspace| {
            workspace
                .pane_state(pane_id)
                .is_some_and(|pane| pane.settled_at.is_some())
        })
    }

    pub(crate) fn settle_pane_at(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        settled_at: u64,
    ) -> bool {
        let terminal_id = {
            let Some(pane) = self.workspaces.get_mut(ws_idx).and_then(|workspace| {
                workspace
                    .tabs
                    .iter_mut()
                    .find_map(|tab| tab.panes.get_mut(&pane_id))
            }) else {
                return false;
            };
            if pane.settled_at.is_some() || pane.snoozed_until().is_some() {
                return false;
            }
            pane.settled_at = Some(settled_at);
            pane.settled_locked = settled_read_only_remaining(
                settled_at,
                unix_seconds(SystemTime::now()),
                self.settled_read_only_after,
            )
            .is_zero();
            pane.attached_terminal_id.clone()
        };
        if let Some(terminal) = self.terminals.get_mut(&terminal_id) {
            terminal.clear_stale_for_settlement();
        }
        let workspace_id = self.workspaces[ws_idx].id.clone();
        self.pending_pane_settlement_changes
            .push(PaneSettlementChange {
                workspace_id,
                pane_id,
                settled_at: Some(settled_at),
                lock_only: false,
            });
        self.mark_session_dirty();
        self.mark_sidebar_projection_changed();
        true
    }

    pub(crate) fn note_human_pane_activity_at(&mut self, pane_id: PaneId, now: Instant) -> bool {
        let Some((ws_idx, pane)) = self.pane_state_mut(pane_id) else {
            return false;
        };
        pane.activity.note(now);
        pane.settle_resume_guard = false;
        let changed = pane.settled_at.take().is_some();
        if changed {
            pane.settled_locked = false;
        }
        self.mark_session_dirty();
        if changed {
            let workspace_id = self.workspaces[ws_idx].id.clone();
            self.pending_pane_settlement_changes
                .push(PaneSettlementChange {
                    workspace_id,
                    pane_id,
                    settled_at: None,
                    lock_only: false,
                });
            self.mark_session_dirty();
            self.mark_sidebar_projection_changed();
        }
        changed
    }

    pub(crate) fn note_automated_pane_activity_at(&mut self, pane_id: PaneId, now: Instant) {
        if let Some((_, pane)) = self.pane_state_mut(pane_id) {
            pane.activity.note(now);
            self.mark_session_dirty();
        }
    }

    pub(crate) fn observe_pane_detection_snapshot_at(
        &mut self,
        pane_id: PaneId,
        revision: u64,
        agent: Option<crate::detect::Agent>,
        snapshot: &str,
        now: Instant,
    ) -> bool {
        let Some((activity, terminal_id)) = self.pane_state_mut(pane_id).map(|(_, pane)| {
            let activity = pane
                .activity
                .observe_detection_snapshot(revision, agent, snapshot, now);
            (activity, pane.attached_terminal_id.clone())
        }) else {
            return false;
        };
        let is_working = activity
            && self.terminals.get(&terminal_id).is_some_and(|terminal| {
                terminal.raw_agent_state() == crate::detect::AgentState::Working
            });
        if activity {
            self.mark_session_dirty();
        }
        if is_working {
            let elapsed = Instant::now().saturating_duration_since(now);
            let observed_at = std::time::SystemTime::now()
                .checked_sub(elapsed)
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            self.agent_states.observe_working(pane_id, observed_at);
        }
        // Detection snapshots can change because settling stopped the agent and
        // its resume stub repainted the pane. Only input or an agent state
        // transition should unsettle it.
        false
    }

    pub(crate) fn refresh_settled_panes_at(
        &mut self,
        snapshot: Option<&crate::work_index::Snapshot>,
        now: Instant,
        now_unix: u64,
    ) -> usize {
        let mut candidates = Vec::new();
        let mut observed_work_keys = Vec::new();
        let mut arm_writes = Vec::new();
        let mut lock_writes = Vec::new();
        for (ws_idx, workspace) in self.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.terminals.get(&pane.attached_terminal_id) else {
                        continue;
                    };
                    let projection = pane.agent_projection(terminal);
                    let quiet = crate::app::pane_lifecycle::pane_is_quiet(pane, terminal);
                    let quiet_observation_changed = pane.activity.quiet_observation_changes(quiet);
                    if pane.settled_at.is_some() || pane.snoozed_until().is_some() {
                        if !pane.settled_locked {
                            if let Some(settled_at) = pane.settled_at.filter(|settled_at| {
                                settled_read_only_remaining(
                                    *settled_at,
                                    now_unix,
                                    self.settled_read_only_after,
                                )
                                .is_zero()
                            }) {
                                lock_writes.push((ws_idx, *pane_id, settled_at));
                            }
                        }
                        if quiet_observation_changed {
                            arm_writes.push((ws_idx, *pane_id, pane.finished_since, quiet));
                        }
                        continue;
                    }
                    if terminal.raw_agent_state() == crate::detect::AgentState::Working
                        || projection.state == crate::detect::AgentState::Working
                        || projection.needs_human_attention()
                        || projection.open_blockers
                        || terminal.declares_running_subagents()
                        || terminal.holds_shell
                    {
                        // Settling suspends the agent. Never let an external
                        // finished or inactivity signal stop visible work,
                        // bury a question, or stop work below the parent. Do
                        // not age the finished-work grace behind this guard.
                        if pane.finished_since.is_some() || quiet_observation_changed {
                            arm_writes.push((ws_idx, *pane_id, None, quiet));
                        }
                        continue;
                    }
                    let context = terminal.effective_work_context();
                    let work_key = work_context_settlement_key(context, snapshot);
                    let new_work_trigger = work_key
                        .as_ref()
                        .is_some_and(|key| pane.settled_work_key.as_ref() != Some(key));
                    let inactive_for = pane.activity.inactive_for(now);
                    let inactive = self.auto_settle_inactive && inactive_for >= self.settle_after;
                    let new_work_trigger = self.auto_settle_finished && new_work_trigger;
                    // A finished reading is evidence, not a verdict. A pull
                    // request reads merged while its agent works the follow-up,
                    // and a ticket can be moved to done by someone else. So the
                    // reading has to survive `settle_finished_after` with the
                    // pane quiet before it settles: settling stops the agent,
                    // which costs far more than waiting out a wrong reading.
                    let finished_since = pane.finished_since.unwrap_or(now);
                    let finished_ripe = now.saturating_duration_since(finished_since)
                        >= self.settle_finished_after
                        && inactive_for >= self.settle_finished_after;
                    // A quiet agent has no work running and no human decision
                    // pending. Use the pane activity clock rather than the
                    // transient unread-Done clock, which focus clears.
                    let quiet_ripe = matches!(
                        settle_countdown(
                            self.auto_settle_done,
                            pane.seen,
                            self.is_active_pane(ws_idx, tab_idx, *pane_id),
                            tab.pinned,
                            quiet,
                            false,
                            pane.activity.quiet_for(now),
                            self.settle_done_after,
                        ),
                        Some(SettleHint::Countdown(remaining)) if remaining.is_zero()
                    );
                    let holding = new_work_trigger && !finished_ripe;
                    let armed = holding.then_some(finished_since);
                    if pane.finished_since != armed || quiet_observation_changed {
                        arm_writes.push((ws_idx, *pane_id, armed, quiet));
                    }
                    // Recording the observed key consumes the one-shot trigger,
                    // so it must wait until the grace window resolves.
                    if !holding {
                        observed_work_keys.push((ws_idx, *pane_id, work_key.clone()));
                    }
                    if inactive || quiet_ripe || (new_work_trigger && finished_ripe) {
                        let Ok(agent_ref) = crate::api::schema::AgentRef::new(
                            self.agent_host_name.clone(),
                            pane_id.raw().to_string(),
                        ) else {
                            continue;
                        };
                        candidates.push(PaneSettlementCandidate {
                            agent_ref,
                            ws_idx,
                            pane_id: *pane_id,
                        });
                    }
                }
            }
        }

        for (ws_idx, pane_id, work_key) in &observed_work_keys {
            if let Some(pane) = self.workspaces[*ws_idx]
                .tabs
                .iter_mut()
                .find_map(|tab| tab.panes.get_mut(pane_id))
            {
                pane.settled_work_key.clone_from(work_key);
            }
        }
        let mut quiet_clock_changed = false;
        for (ws_idx, pane_id, armed, quiet) in &arm_writes {
            if let Some(pane) = self.workspaces[*ws_idx]
                .tabs
                .iter_mut()
                .find_map(|tab| tab.panes.get_mut(pane_id))
            {
                pane.finished_since = *armed;
                quiet_clock_changed |= pane.activity.observe_quiet(*quiet, now);
            }
        }
        if quiet_clock_changed {
            self.mark_session_dirty();
        }
        for (ws_idx, pane_id, settled_at) in &lock_writes {
            let workspace_id = self.workspaces[*ws_idx].id.clone();
            if let Some(pane) = self.workspaces[*ws_idx].pane_state_mut(*pane_id) {
                pane.settled_locked = true;
                self.pending_pane_settlement_changes
                    .push(PaneSettlementChange {
                        workspace_id,
                        pane_id: *pane_id,
                        settled_at: Some(*settled_at),
                        lock_only: true,
                    });
                self.mark_session_dirty();
                self.mark_sidebar_projection_changed();
            }
        }
        self.settle_owned_candidates(&candidates, now_unix) + lock_writes.len()
    }
}

impl App {
    pub(crate) fn flush_pane_snooze_events(&mut self) -> bool {
        let changes = std::mem::take(&mut self.state.pending_pane_snooze_changes);
        if changes.is_empty() {
            return false;
        }
        let mut seen = HashSet::new();
        let changes = changes
            .into_iter()
            .rev()
            .filter(|change| seen.insert((change.workspace_id.clone(), change.pane_id)))
            .collect::<Vec<_>>()
            .into_iter()
            .rev();
        for change in changes {
            let Some(ws_idx) = self
                .state
                .workspaces
                .iter()
                .position(|ws| ws.id == change.workspace_id)
            else {
                continue;
            };
            let Some(pane_id) = self.public_pane_id(ws_idx, change.pane_id) else {
                continue;
            };
            let workspace_id = self.public_workspace_id(ws_idx);
            let (event, data) = match change.deadline {
                Some(snoozed_until) => (
                    crate::api::schema::EventKind::PaneSnoozed,
                    crate::api::schema::EventData::PaneSnoozed {
                        pane_id,
                        workspace_id,
                        snoozed_until,
                    },
                ),
                None => (
                    crate::api::schema::EventKind::PaneUnsnoozed,
                    crate::api::schema::EventData::PaneUnsnoozed {
                        pane_id,
                        workspace_id,
                        reason: change
                            .reason
                            .unwrap_or(crate::api::schema::PaneUnsnoozeReason::Explicit),
                    },
                ),
            };
            self.emit_event(crate::api::schema::EventEnvelope { event, data });
            self.emit_pane_updated(ws_idx, change.pane_id);
        }
        self.schedule_session_save();
        true
    }

    pub(crate) fn refresh_pane_settlement_at(&mut self, now: Instant) -> bool {
        self.refresh_pane_settlement_at_with_wall_clock(now, unix_seconds(SystemTime::now()))
    }

    pub(crate) fn refresh_pane_settlement_at_with_wall_clock(
        &mut self,
        now: Instant,
        now_unix: u64,
    ) -> bool {
        let snapshots = self
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .filter_map(|(pane_id, pane)| {
                let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
                let runtime = self.terminal_runtimes.get(&pane.attached_terminal_id)?;
                let revision = runtime.content_revision();
                let agent = terminal.effective_known_agent();
                if !pane.activity.needs_detection_snapshot(revision, agent) {
                    return None;
                }
                let content = runtime.detection_text();
                let snapshot =
                    crate::detect::manifest::non_chrome_activity_content(agent, &content);
                Some((*pane_id, revision, agent, snapshot))
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for (pane_id, revision, agent, snapshot) in snapshots {
            changed |= self
                .state
                .observe_pane_detection_snapshot_at(pane_id, revision, agent, &snapshot, now);
        }
        changed |= self.state.refresh_snoozes_at(now, now_unix);
        changed |= self.state.refresh_remote_snoozes_at(now_unix);
        changed |= self.flush_pane_snooze_events();
        changed |=
            self.state
                .refresh_settled_panes_at(self.work_index_snapshot.as_ref(), now, now_unix)
                > 0;
        self.flush_status_transitions();
        changed |= self.flush_pane_settlement_events();
        let active_locked = self.state.active.and_then(|ws_idx| {
            let workspace = self.state.workspaces.get(ws_idx)?;
            let pane_id = workspace.focused_pane_id()?;
            let pane = workspace.pane_state(pane_id)?;
            (pane.settled_locked
                && self.state.settled_view.is_none()
                && self
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .is_some_and(|terminal| terminal.pending_agent_resume_plan.is_some()))
            .then(|| crate::app::state::PaneFocusTarget {
                workspace_id: workspace.id.clone(),
                pane_id,
            })
        });
        if let Some(target) = active_locked {
            self.focus_settled_pane(target);
        }
        changed
    }

    pub(crate) fn flush_status_transitions(&mut self) {
        let transitions = std::mem::take(&mut self.state.pending_status_transitions);
        let Some(status_log) = self.status_log.as_ref() else {
            return;
        };
        for transition in transitions {
            let Some((ws_idx, pane)) = self.find_pane(transition.pane_id) else {
                continue;
            };
            let Some(pane_id) = self.public_pane_id(ws_idx, transition.pane_id) else {
                continue;
            };
            let (tail, tail_truncated) = self
                .terminal_runtimes
                .get(&pane.attached_terminal_id)
                .map(|runtime| {
                    let snapshot = runtime.recent_text_snapshot(crate::status_log::TAIL_MAX_LINES);
                    (snapshot.text, snapshot.truncated)
                })
                .unwrap_or_default();
            let mut record = crate::status_log::NewRecord {
                pane: &pane_id,
                session: Some(
                    crate::session::active_name()
                        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string()),
                ),
                agent: transition
                    .agent
                    .map(crate::detect::agent_label)
                    .map(str::to_string),
                host: crate::platform::hostname(),
                from_state: crate::status_log::state_label(transition.from_state),
                to_state: crate::status_log::state_label(transition.to_state),
                source: crate::status_log::Source::Detector,
                note: None,
                tail: &tail,
            }
            .into_record(time::OffsetDateTime::now_utc());
            record.tail_truncated |= tail_truncated;
            match status_log.append(&record) {
                Ok(crate::status_log::AppendOutcome::Written(_)) => {}
                Ok(crate::status_log::AppendOutcome::Capped) => {
                    tracing::warn!(pane = %pane_id, "agent status log day file is full; transition was dropped");
                }
                Err(error) => {
                    tracing::warn!(pane = %pane_id, %error, "could not append agent status transition");
                }
            }
        }
    }

    pub(crate) fn flush_pane_settlement_events(&mut self) -> bool {
        let changes = std::mem::take(&mut self.state.pending_pane_settlement_changes);
        if changes.is_empty() {
            return false;
        }
        for change in changes {
            let Some(ws_idx) = self
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == change.workspace_id)
            else {
                continue;
            };
            let Some(pane_id) = self.public_pane_id(ws_idx, change.pane_id) else {
                continue;
            };
            let Some(terminal_id) = self
                .find_pane(change.pane_id)
                .map(|(_, pane)| pane.attached_terminal_id.clone())
            else {
                continue;
            };

            match change.settled_at {
                Some(_)
                    if self.state.settle_stops_agent
                        && self
                            .state
                            .workspaces
                            .get(ws_idx)
                            .and_then(|workspace| workspace.pane_state(change.pane_id))
                            .is_some_and(|pane| pane.settled_locked) =>
                {
                    let derived_label = derived_pane_label(&self.state, ws_idx, change.pane_id);
                    let (resume_plan, settled_label) =
                        self.state
                            .terminals
                            .get(&terminal_id)
                            .map(|terminal| {
                                let resume_plan = terminal
                                    .persisted_agent_session
                                    .as_ref()
                                    .and_then(|session| {
                                        crate::agent_resume::plan(
                                            &session.source,
                                            &session.agent,
                                            &session.session_ref,
                                        )
                                    });
                                let settled_label = terminal.manual_label.is_none().then(|| {
                                    primary_pr_settled_label(
                                        terminal,
                                        self.work_index_snapshot.as_ref(),
                                    )
                                    .or(derived_label)
                                });
                                (resume_plan, settled_label.flatten())
                            })
                            .unwrap_or_default();

                    if let Some(resume_plan) = resume_plan {
                        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                            terminal.pending_agent_resume_plan = Some(resume_plan);
                            if let Some(label) = settled_label {
                                terminal.set_manual_label(label.clone());
                                terminal.settled_auto_label = Some(label);
                            }
                        }
                        if let Some(pane) = self
                            .state
                            .workspaces
                            .get_mut(ws_idx)
                            .and_then(|workspace| workspace.pane_state_mut(change.pane_id))
                        {
                            pane.settle_resume_guard = true;
                        }
                        if let Some(runtime) = self.terminal_runtimes.get_mut(&terminal_id) {
                            runtime.suspend_processes();
                        }
                    } else {
                        tracing::debug!(
                            pane = change.pane_id.raw(),
                            terminal = %terminal_id,
                            "leaving settled pane running without a supported resume plan"
                        );
                    }
                }
                Some(_) => {}
                None => {
                    let settled_auto_label = self
                        .state
                        .terminals
                        .get_mut(&terminal_id)
                        .and_then(|terminal| terminal.settled_auto_label.take());
                    if let Some(settled_auto_label) = settled_auto_label {
                        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                            if terminal.manual_label.as_ref() == Some(&settled_auto_label) {
                                terminal.clear_manual_label();
                            }
                        }
                    }

                    let suspended_size = self
                        .terminal_runtimes
                        .get(&terminal_id)
                        .filter(|runtime| runtime.is_suspended())
                        .map(crate::terminal::TerminalRuntime::current_size);
                    if let Some((rows, cols)) = suspended_size {
                        drop(self.terminal_runtimes.remove(&terminal_id));
                        self.start_pending_agent_resume_for_terminal(
                            &terminal_id,
                            rows,
                            cols,
                            true,
                        );
                    }
                }
            }
            let workspace_id = self.public_workspace_id(ws_idx);
            let (event, data) = match change.settled_at {
                Some(settled_at) => (
                    crate::api::schema::EventKind::PaneSettled,
                    crate::api::schema::EventData::PaneSettled {
                        pane_id,
                        workspace_id,
                        settled_at,
                    },
                ),
                None => (
                    crate::api::schema::EventKind::PaneUnsettled,
                    crate::api::schema::EventData::PaneUnsettled {
                        pane_id,
                        workspace_id,
                    },
                ),
            };
            if !change.lock_only {
                self.emit_event(crate::api::schema::EventEnvelope { event, data });
            }
            self.emit_pane_updated(ws_idx, change.pane_id);
        }
        self.schedule_session_save();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_read_only_countdown_uses_persisted_settled_at() {
        let grace = Duration::from_secs(15 * 60);
        assert_eq!(
            settled_read_only_remaining(100, 100 + 60, grace),
            Duration::from_secs(14 * 60)
        );
        assert_eq!(
            settled_read_only_remaining(100, 100 + 1, Duration::ZERO),
            Duration::ZERO
        );
        assert_eq!(
            settled_read_only_remaining(100, 100 + 900, grace),
            Duration::ZERO
        );
        assert_eq!(settled_read_only_remaining(100, 99, grace), grace);
    }

    #[test]
    fn settle_countdown_respects_all_eligibility_gates_and_rounding() {
        let duration = Duration::from_secs;
        let hint = |enabled, seen, active, pinned, quiet, stopped, elapsed| {
            settle_countdown(
                enabled,
                seen,
                active,
                pinned,
                quiet,
                stopped,
                elapsed,
                duration(300),
            )
        };
        assert_eq!(
            hint(true, true, false, false, true, false, Some(duration(59))),
            Some(SettleHint::Countdown(duration(241)))
        );
        assert_eq!(
            hint(true, true, false, false, true, false, Some(duration(300))),
            Some(SettleHint::Countdown(Duration::ZERO))
        );
        assert_eq!(
            hint(true, true, true, false, true, false, Some(duration(20))),
            Some(SettleHint::Focused(duration(300)))
        );
        assert_eq!(
            hint(true, true, false, true, true, false, Some(duration(20))),
            Some(SettleHint::Pinned)
        );
        assert_eq!(
            hint(true, true, true, true, true, false, Some(duration(20))),
            Some(SettleHint::Pinned)
        );
        for args in [
            (false, true, false, false, true, false),
            (true, false, false, false, true, false),
            (true, true, false, false, false, false),
            (true, true, false, false, true, true),
        ] {
            assert_eq!(
                hint(
                    args.0,
                    args.1,
                    args.2,
                    args.3,
                    args.4,
                    args.5,
                    Some(duration(20))
                ),
                None
            );
        }
        assert_eq!(
            hint(true, true, false, false, true, false, None),
            Some(SettleHint::Countdown(duration(300)))
        );
    }
    use crate::{
        detect::AgentState,
        terminal::{TerminalId, TerminalRuntime, TerminalState},
        workspace::Workspace,
    };

    #[test]
    fn snooze_deadline_converts_wall_clock_to_timer_and_survives_due_time() {
        let now = Instant::now();
        assert_eq!(
            snooze_instant(1_725_000_059, 1_725_000_000, now),
            now + Duration::from_secs(59)
        );
        assert_eq!(snooze_instant(1_725_000_000, 1_725_000_000, now), now);
        assert_eq!(snooze_instant(1_725_000_001, 1_725_000_100, now), now);
        assert_eq!(
            snooze_instant(u64::MAX, 1_725_000_000, now),
            now + Duration::from_secs(MAX_SNOOZE_SECONDS)
        );
    }

    #[test]
    fn remote_snooze_deadline_reprojects_without_a_fleet_poll() {
        let info: crate::api::schema::AgentInfo = serde_json::from_value(serde_json::json!({
            "terminal_id": "term_remote",
            "work_context": {"ticket_ids": [], "pr_urls": []},
            "name": "remote worker",
            "agent": "codex",
            "agent_status": "working",
            "reported_at": "1970-01-01T00:03:10Z",
            "gates": [],
            "workspace_id": "w1",
            "tab_id": "t1",
            "pane_id": "p1",
            "focused": false,
            "state_change_seq": 7,
            "revision": 1,
            "snoozed_until": 200
        }))
        .expect("valid remote agent");
        let snapshot = crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: "remote".into(),
                target: "remote".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: vec![crate::fleet::FleetRow::test_agent_info_row("remote", info)],
            }],
            ..crate::fleet::Snapshot::default()
        };
        let mut state = crate::app::state::AppState::test_new();
        state.fleet_snapshot = snapshot;
        state.remote_agent_panel_entries =
            crate::ui::remote_agent_panel_entries_at(&state.fleet_snapshot, 199, false);
        state.view_observed_unix_s = 199;
        state.collapsed_sidebar_groups.remove("repo:Fleet");
        let now = Instant::now();

        assert_eq!(
            state.next_snooze_deadline_at(now, 199),
            Some(now + Duration::from_secs(1))
        );
        assert!(crate::ui::sidebar_rows(&state).iter().any(|row| matches!(
            row,
            crate::ui::SidebarRow::SectionHeader {
                title: crate::ui::sidebar::SNOOZED_SECTION_TITLE,
                count: 1,
                ..
            }
        )));

        assert!(state.refresh_remote_snoozes_at(200));
        state.view_observed_unix_s = 200;
        assert_eq!(state.next_snooze_deadline_at(now, 200), None);
        assert!(crate::ui::sidebar_rows(&state)
            .iter()
            .any(|row| matches!(row, crate::ui::SidebarRow::RemoteAgent { .. })));
    }

    fn app_with_runtime(
        config: &crate::config::Config,
        context: crate::work_context::PaneWorkContext,
    ) -> (
        App,
        PaneId,
        TerminalId,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(config, true, None, api_rx, crate::api::EventHub::default());
        let workspace = Workspace::test_new("settlement-runtime");
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
            .restore_work_context(context)
            .expect("valid work context");
        let (runtime, rx) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            1024,
            b"retained settlement output\r\n",
            4,
        );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        (app, pane_id, terminal_id, rx)
    }

    fn set_persisted_session(app: &mut App, terminal_id: &TerminalId, source: &str) {
        let agent = source.rsplit_once(':').map_or(source, |(_, agent)| agent);
        app.state
            .terminals
            .get_mut(terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: source.into(),
                agent: agent.into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("settled-session")
                    .expect("valid session id"),
            });
    }

    fn state_with_context(context: crate::work_context::PaneWorkContext) -> (AppState, PaneId) {
        let mut state = AppState::test_new();
        let workspace = Workspace::test_new("settled");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .pane_state(pane_id)
            .expect("root pane")
            .attached_terminal_id
            .clone();
        let mut terminal = TerminalState::new(terminal_id.clone(), "/repo".into());
        terminal
            .restore_work_context(context)
            .expect("valid work context");
        state.terminals.insert(terminal_id, terminal);
        state.workspaces.push(workspace);
        (state, pane_id)
    }

    fn settled_state_with_agent_state(
        agent_state: AgentState,
        quiet_since: Instant,
    ) -> (AppState, PaneId) {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let _ = state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), agent_state);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(quiet_since);
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));
        (state, pane_id)
    }

    fn transition_agent_state(
        state: &mut AppState,
        pane_id: PaneId,
        agent_state: AgentState,
        now: Instant,
    ) {
        state
            .update_terminal_state_at(pane_id, now, |terminal| {
                Some(terminal.set_detected_state_with_screen_signals_at(
                    Some(crate::detect::Agent::Codex),
                    agent_state,
                    false,
                    false,
                    false,
                    false,
                    false,
                    now,
                ))
            })
            .expect("agent state transition");
    }

    fn assert_quiet_clock_restarted(state: &AppState, pane_id: PaneId, transition_at: Instant) {
        assert_eq!(
            state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(transition_at + Duration::from_secs(60)),
            Duration::from_secs(60)
        );
    }

    fn settle_ready_remote_collision(
        now: Instant,
    ) -> (AppState, PaneId, crate::work_index::Snapshot) {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        state.agent_host_name = "local".into();
        state.auto_settle_inactive = false;
        let quiet_since = now - state.settle_finished_after;
        let pane = state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.activity.set_last_at(quiet_since);
        pane.finished_since = Some(quiet_since);

        let remote_agent: crate::api::schema::AgentInfo =
            serde_json::from_value(serde_json::json!({
                "agent_ref": format!("remote::{}", pane_id.raw()),
                "terminal_id": "remote-term",
                "name": "remote reviewer",
                "agent": "codex",
                "agent_status": "idle",
                "workspace_id": "remote-workspace",
                "tab_id": "remote-tab",
                "pane_id": pane_id.raw().to_string(),
                "focused": false,
                "state_change_seq": 1,
                "revision": 1
            }))
            .expect("remote agent fixture");
        state.remote_agent_panel_entries = crate::ui::remote_agent_panel_entries(
            &crate::fleet::Snapshot {
                hosts: vec![crate::fleet::HostSnapshot {
                    name: "remote".into(),
                    target: "remote".into(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::HostState::Reachable,
                    version: None,
                    protocol: None,
                    error: None,
                    remote_identity: None,
                    sessions: None,
                    reachable: true,
                    last_seen_unix_ms: None,
                    entries: vec![crate::fleet::FleetRow::test_agent_info_row(
                        "remote",
                        remote_agent,
                    )],
                }],
                ..crate::fleet::Snapshot::default()
            },
            false,
        );
        let mut merged_item = item();
        merged_item.pr_url = Some(url.into());
        merged_item.pr_state = Some("merged".into());
        (state, pane_id, snapshot(merged_item))
    }

    fn item() -> crate::work_index::WorkItem {
        crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: Default::default(),
            audience: Default::default(),
            cached_pr_detail: None,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: Default::default(),
        }
    }

    fn snapshot(item: crate::work_index::WorkItem) -> crate::work_index::Snapshot {
        crate::work_index::Snapshot {
            items: vec![item],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::now(),
        }
    }

    #[test]
    fn primary_pr_merged_settles_and_input_unsettles() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let mut first_item = item();
        first_item.pr_url = Some(url.into());
        first_item.pr_state = Some("merged".into());

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(first_item.clone())), now, 1_725_000_000),
            0,
            "a finished pull request arms the grace window instead of settling"
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(first_item)),
                now + state.settle_finished_after,
                1_725_000_000
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
        assert!(state.note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(!state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot({
                    let mut item = item();
                    item.pr_url = Some(url.into());
                    item.pr_state = Some("merged".into());
                    item
                })),
                Instant::now(),
                1_725_000_001
            ),
            0,
            "resuming completed work must consume the trigger"
        );
    }

    #[test]
    fn working_agent_is_not_settled_by_finished_or_inactive_triggers() {
        let url = "https://github.com/owner/repo/pull/7";
        let now = Instant::now();
        let mut merged_item = item();
        merged_item.pr_url = Some(url.into());
        merged_item.pr_state = Some("merged".into());

        let (mut finished, finished_pane) =
            state_with_context(crate::work_context::PaneWorkContext {
                pr_urls: vec![url.into()],
                ..Default::default()
            });
        let finished_terminal = finished.workspaces[0].tabs[0].panes[&finished_pane]
            .attached_terminal_id
            .clone();
        finished
            .terminals
            .get_mut(&finished_terminal)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Working);
        let terminal = finished
            .terminals
            .get_mut(&finished_terminal)
            .expect("working terminal");
        terminal.supervisor_stale = true;
        terminal.stale_resolution = Some((AgentState::Idle, true));
        finished.auto_settle_inactive = false;
        finished.workspaces[0].tabs[0]
            .panes
            .get_mut(&finished_pane)
            .expect("root pane")
            .activity
            .set_last_at(now - finished.settle_finished_after);

        assert_eq!(
            finished.refresh_settled_panes_at(
                Some(&snapshot(merged_item.clone())),
                now,
                1_725_000_000,
            ),
            0
        );
        assert_eq!(
            finished.refresh_settled_panes_at(
                Some(&snapshot(merged_item)),
                now + finished.settle_finished_after,
                1_725_000_001,
            ),
            0
        );
        assert!(!finished.pane_is_settled(0, finished_pane));

        let (mut inactive, inactive_pane) = state_with_context(Default::default());
        let inactive_terminal = inactive.workspaces[0].tabs[0].panes[&inactive_pane]
            .attached_terminal_id
            .clone();
        inactive
            .terminals
            .get_mut(&inactive_terminal)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Working);
        inactive.auto_settle_finished = false;
        inactive.workspaces[0].tabs[0]
            .panes
            .get_mut(&inactive_pane)
            .expect("root pane")
            .activity
            .set_last_at(now - inactive.settle_after);

        assert_eq!(
            inactive.refresh_settled_panes_at(None, now, 1_725_000_002),
            0
        );
        assert!(!inactive.pane_is_settled(0, inactive_pane));
    }

    #[test]
    fn primary_pr_closed_without_merge_never_settles() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        state.auto_settle_inactive = false;
        let mut closed_item = item();
        closed_item.pr_url = Some(url.into());
        closed_item.pr_state = Some("closed".into());

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(closed_item.clone())),
                now,
                1_725_000_000
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(closed_item)),
                now + state.settle_finished_after * 3,
                1_725_000_001
            ),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn blocked_pane_with_merged_pr_waits_for_a_fresh_grace_window() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Blocked);
        state.settle_after = Duration::ZERO;
        let mut merged_item = item();
        merged_item.pr_url = Some(url.into());
        merged_item.pr_state = Some("merged".into());

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item.clone())),
                now,
                1_725_000_000
            ),
            0
        );
        let after_old_deadline = now + state.settle_finished_after * 3;
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item.clone())),
                after_old_deadline,
                1_725_000_001
            ),
            0
        );
        let pane = &state.workspaces[0].tabs[0].panes[&pane_id];
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(pane.finished_since.is_none());
        assert!(pane.settled_work_key.is_none());

        state.auto_settle_inactive = false;
        state
            .update_terminal_state_at(pane_id, after_old_deadline, |terminal| {
                Some(terminal.set_detected_state_with_screen_signals_at(
                    Some(crate::detect::Agent::Codex),
                    AgentState::Idle,
                    false,
                    false,
                    false,
                    false,
                    false,
                    after_old_deadline,
                ))
            })
            .expect("unblocking state transition");
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item.clone())),
                after_old_deadline,
                1_725_000_002
            ),
            0,
            "unblocking starts the normal grace window"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item)),
                after_old_deadline + state.settle_finished_after,
                1_725_000_003
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn finished_reading_that_disappears_during_the_grace_window_revokes_the_trigger() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let merged = || {
            let mut item = item();
            item.pr_url = Some(url.into());
            item.pr_state = Some("merged".into());
            item
        };
        let open = || {
            let mut item = item();
            item.pr_url = Some(url.into());
            item.pr_state = Some("open".into());
            item
        };

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(merged())), now, 1_725_000_000),
            0
        );
        // The reading is revoked mid-window: the pane must never settle for it.
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(open())),
                now + state.settle_finished_after / 2,
                1_725_000_001
            ),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));

        // A fresh finished reading arms a fresh window rather than inheriting
        // the revoked one.
        let restart = now + state.settle_finished_after;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(merged())), restart, 1_725_000_002),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged())),
                restart + state.settle_finished_after,
                1_725_000_003
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn linked_done_ticket_settles_and_agent_state_change_unsettles() {
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["SCA-42".into()],
            ..Default::default()
        });
        let mut item = item();
        item.ticket_ids = vec!["SCA-42".into()];
        item.ticket_state = Some("Done".into());
        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(item.clone())), now, 1_725_000_001),
            0,
            "a done ticket arms the grace window instead of settling"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(item)),
                now + state.settle_finished_after,
                1_725_000_001
            ),
            1
        );

        state
            .update_terminal_state(pane_id, |terminal| {
                Some(terminal.set_detected_state_with_screen_signals_at(
                    Some(crate::detect::Agent::Codex),
                    AgentState::Working,
                    false,
                    false,
                    false,
                    false,
                    false,
                    Instant::now(),
                ))
            })
            .expect("agent state transition");
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn settled_pane_stays_settled_when_idle_becomes_unknown_and_restarts_quiet_clock() {
        let now = Instant::now();
        let transition_at = now + Duration::from_secs(60);
        let (mut state, pane_id) = settled_state_with_agent_state(AgentState::Idle, now);

        transition_agent_state(&mut state, pane_id, AgentState::Unknown, transition_at);

        assert!(state.pane_is_settled(0, pane_id));
        assert_quiet_clock_restarted(&state, pane_id, transition_at);
    }

    #[test]
    fn settled_pane_stays_settled_when_unknown_becomes_idle_and_restarts_quiet_clock() {
        let now = Instant::now();
        let transition_at = now + Duration::from_secs(60);
        let (mut state, pane_id) = settled_state_with_agent_state(AgentState::Unknown, now);

        transition_agent_state(&mut state, pane_id, AgentState::Idle, transition_at);

        assert!(state.pane_is_settled(0, pane_id));
        assert_quiet_clock_restarted(&state, pane_id, transition_at);
    }

    #[test]
    fn settled_pane_unsettles_when_agent_enters_working() {
        let now = Instant::now();
        let transition_at = now + Duration::from_secs(60);
        let (mut state, pane_id) = settled_state_with_agent_state(AgentState::Idle, now);

        transition_agent_state(&mut state, pane_id, AgentState::Working, transition_at);

        assert!(!state.pane_is_settled(0, pane_id));
        assert_quiet_clock_restarted(&state, pane_id, transition_at);
    }

    #[test]
    fn revoked_finished_reading_requires_a_fresh_grace_window() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let mut finished_item = item();
        finished_item.pr_url = Some(url.into());
        finished_item.pr_state = Some("merged".into());
        let mut unfinished_item = finished_item.clone();
        unfinished_item.pr_state = Some("open".into());
        let now = Instant::now();
        let old_deadline = now + state.settle_finished_after;

        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(finished_item.clone())),
                now,
                1_725_000_001
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(unfinished_item.clone())),
                now + state.settle_finished_after / 2,
                1_725_000_002
            ),
            0
        );
        assert!(state.workspaces[0].tabs[0].panes[&pane_id]
            .finished_since
            .is_none());
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(unfinished_item)),
                old_deadline,
                1_725_000_003
            ),
            0,
            "revoked finished work must stay unsettled past the old deadline"
        );
        assert!(!state.pane_is_settled(0, pane_id));

        let rearmed_at = old_deadline + Duration::from_secs(1);
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(finished_item.clone())),
                rearmed_at,
                1_725_000_004
            ),
            0,
            "a fresh finished reading re-arms the grace window"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(finished_item)),
                rearmed_at + state.settle_finished_after,
                1_725_000_005
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    /// A Done pane the way the lifecycle sees one: an idle detected agent whose
    /// output nobody has read since it stopped.
    fn done_state(resumable: bool, done_since: Instant) -> (AppState, PaneId) {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Idle);
        if resumable {
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("done-session")
                    .expect("valid session id"),
            });
        }
        let pane = state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.seen = false;
        pane.done_since = Some(done_since);
        pane.activity.set_last_at(done_since);
        pane.activity.observe_quiet(true, done_since);
        state.settle_done_after = Duration::from_secs(30 * 60);
        // Isolate the Done trigger from the other two.
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        (state, pane_id)
    }

    fn stale_state(screen_state: Option<AgentState>, quiet_since: Instant) -> (AppState, PaneId) {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(1_000),
            quiet_since,
        );
        terminal
            .mark_agent_status_stale_at(
                quiet_since + crate::terminal::state::AGENT_BUSY_STALE_SILENCE,
                state.agent_stale_after,
            )
            .expect("working report should become stale");
        assert!(terminal.supervisor_stale);
        if screen_state.is_none() {
            terminal.set_raw_agent_state_for_test(AgentState::Idle);
        }
        state.note_human_pane_activity_at(pane_id, quiet_since);
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        state.settle_done_after = Duration::from_secs(30 * 60);
        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: screen_state.map(|state| (state, false)),
            observed_at: quiet_since + Duration::from_nanos(1),
        });
        let _ = state.refresh_settled_panes_at(None, quiet_since, 1_724_999_999);
        (state, pane_id)
    }

    #[test]
    fn done_resumable_pane_settles_once_the_done_window_passes() {
        let done_since = Instant::now();
        let now = done_since + Duration::from_secs(29 * 60);
        let (mut state, pane_id) = done_state(true, done_since);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("done pane")
            .seen = true;
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_030), 0);
        assert!(!state.pane_is_settled(0, pane_id));

        let done_since = Instant::now();
        let now = done_since + Duration::from_secs(31 * 60);
        let (mut state, pane_id) = done_state(true, done_since);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("done pane")
            .seen = true;
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_031), 1);
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn unread_done_pane_does_not_auto_settle() {
        let done_since = Instant::now();
        let (mut state, pane_id) = done_state(true, done_since);
        let deadline = done_since + state.settle_done_after;
        assert_eq!(
            state.refresh_settled_panes_at(None, deadline, 1_725_000_031),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn settling_clears_a_stale_supervisor_mark() {
        let now = Instant::now();
        let (mut state, pane_id) = stale_state(None, now);
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        assert!(state.terminals[&terminal_id].supervisor_stale);

        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));
        assert!(!state.terminals[&terminal_id].supervisor_stale);
        assert!(state.next_agent_watchdog_deadline().is_none());
        assert!(state
            .mark_due_agent_status_stale_at(now + Duration::from_secs(2 * 60 * 60))
            .is_empty());
        assert!(!state.terminals[&terminal_id].supervisor_stale);
    }

    #[test]
    fn informational_items_do_not_hold_quiet_settle() {
        let quiet_since = Instant::now();
        let now = quiet_since + Duration::from_secs(31 * 60);
        let (mut state, pane_id) = done_state(true, quiet_since);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("done pane")
            .seen = true;
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .closing_items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "What to test".into(),
            text: "Run the focused regression".into(),
            blocking: false,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_031), 1);
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn quiet_seen_pane_settles_after_focus_moves_elsewhere() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let now = Instant::now();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Idle);
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("seen-session")
                    .expect("valid session id"),
            });
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        state.settle_done_after = Duration::from_secs(30 * 60);
        state.note_human_pane_activity_at(pane_id, now);

        state.active = Some(0);
        state.focus_pane_in_workspace(0, pane_id);
        assert!(state.workspaces[0].tabs[0].panes[&pane_id].seen);
        assert_eq!(
            state.refresh_settled_panes_at(None, now, 1_725_000_035),
            0,
            "focus blocks settling while the quiet clock is observed"
        );
        let other = Workspace::test_new("other");
        let other_pane = other.tabs[0].root_pane;
        state.workspaces.push(other);
        state.ensure_test_terminals();
        state.focus_pane_in_workspace(1, other_pane);

        assert_eq!(
            state.refresh_settled_panes_at(None, now + state.settle_done_after, 1_725_000_036),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn stale_pane_settles_only_when_its_screen_resolves_idle() {
        let now = Instant::now();
        let (mut idle, idle_pane) = stale_state(Some(AgentState::Idle), now);
        let settle_at = idle.workspaces[0].tabs[0].panes[&idle_pane]
            .activity
            .last_at()
            + idle.settle_done_after;
        assert_eq!(
            idle.refresh_settled_panes_at(None, settle_at, 1_725_000_037),
            0
        );
        assert!(!idle.pane_is_settled(0, idle_pane));

        let (mut unresolved, unresolved_pane) = stale_state(None, now);
        let settle_at = unresolved.workspaces[0].tabs[0].panes[&unresolved_pane]
            .activity
            .last_at()
            + unresolved.settle_done_after;
        assert_eq!(
            unresolved.refresh_settled_panes_at(None, settle_at, 1_725_000_038),
            0,
            "a stale pane needs screen evidence before it can settle"
        );
        assert!(!unresolved.pane_is_settled(0, unresolved_pane));

        for screen_state in [AgentState::Working, AgentState::Blocked] {
            let (mut state, pane_id) = stale_state(Some(screen_state), now);
            let settle_at = state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .last_at()
                + state.settle_done_after;
            assert_eq!(
                state.refresh_settled_panes_at(None, settle_at, 1_725_000_039),
                0,
                "stale {screen_state:?} pane settled"
            );
            assert!(!state.pane_is_settled(0, pane_id));
        }
    }

    #[test]
    fn quiet_settle_deadline_covers_seen_and_stale_idle_panes() {
        let now = Instant::now();
        let expected = now + Duration::from_secs(30 * 60);
        let (mut seen, seen_pane) = done_state(true, now);
        let pane = seen.workspaces[0].tabs[0]
            .panes
            .get_mut(&seen_pane)
            .expect("seen pane");
        pane.seen = true;
        pane.activity.set_last_at(now);
        assert_eq!(seen.next_done_settle_deadline(now), Some(expected));

        let (stale, stale_pane) = stale_state(Some(AgentState::Idle), now);
        let stale_expected = stale.workspaces[0].tabs[0].panes[&stale_pane]
            .activity
            .last_at()
            + stale.settle_done_after;
        assert_eq!(stale.next_done_settle_deadline(now), Some(stale_expected));
    }

    #[test]
    fn stale_resolution_event_starts_the_quiet_window_at_the_evaluator() {
        let old_activity = Instant::now();
        let now = old_activity + Duration::from_secs(2 * 60 * 60);
        let (mut state, pane_id) = stale_state(Some(AgentState::Working), old_activity);

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: Some((AgentState::Idle, false)),
            observed_at: now,
        });

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_040), 0);
        assert_eq!(
            state.refresh_settled_panes_at(
                None,
                now + state.settle_done_after - Duration::from_nanos(1),
                1_725_001_839,
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(None, now + state.settle_done_after, 1_725_001_840,),
            0
        );
    }

    #[test]
    fn held_shell_end_event_starts_the_quiet_window_at_the_evaluator() {
        let old_activity = Instant::now();
        let now = old_activity + Duration::from_secs(2 * 60 * 60);
        let (mut state, pane_id) = stale_state(Some(AgentState::Idle), old_activity);
        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: true,
            stale_resolution: Some((AgentState::Idle, false)),
            observed_at: now,
        });
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_042), 0);

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: Some((AgentState::Idle, false)),
            observed_at: now,
        });

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_043), 0);
        assert_eq!(
            state.refresh_settled_panes_at(
                None,
                now + state.settle_done_after - Duration::from_nanos(1),
                1_725_001_841,
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(None, now + state.settle_done_after, 1_725_001_842,),
            0
        );
    }

    #[test]
    fn stale_blocked_resolution_event_unsettles_an_active_projection() {
        let now = Instant::now();
        let (mut state, pane_id) = stale_state(None, now);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .settled_at = Some(1_725_000_044);

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: Some((AgentState::Blocked, false)),
            observed_at: now + Duration::from_nanos(1),
        });

        assert!(
            !state.pane_is_settled(0, pane_id),
            "stale Blocked projection stayed settled"
        );
    }

    #[test]
    fn waiting_on_agents_requires_zero_agent_close_and_completion_to_settle() {
        let now = Instant::now();
        let (mut state, pane_id) = state_with_context(Default::default());
        state.active = None;
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        state.auto_settle_done = true;
        state.settle_done_after = Duration::ZERO;
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_closing_task_report(
                None,
                None,
                Some(crate::api::schema::ClosingParseStatus::Ok),
                Some(false),
                now,
            );
        state.handle_app_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "herdr:claude-closing-block".into(),
            agent_label: "claude".into(),
            state: AgentState::Idle,
            message: None,
            seq: Some(1),
            wait: None,
            eta_s: None,
            reported_at: None,
            session_ref: None,
            closing_block: Some(Box::new(crate::events::ClosingBlockReport {
                gates: Vec::new(),
                items: Vec::new(),
                decisions: Vec::new(),
                agents: Some(3),
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                dependencies_authoritative: true,
                session_id: None,
                last_turn_at: None,
                settle_ready: false,
            })),
        });
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now);

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_045), 0);
        assert!(!state.pane_is_settled(0, pane_id));

        state.handle_app_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "herdr:claude-closing-block".into(),
            agent_label: "claude".into(),
            state: AgentState::Idle,
            message: None,
            seq: Some(2),
            wait: None,
            eta_s: None,
            reported_at: None,
            session_ref: None,
            closing_block: Some(Box::new(crate::events::ClosingBlockReport {
                gates: Vec::new(),
                items: Vec::new(),
                decisions: Vec::new(),
                agents: Some(0),
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                dependencies_authoritative: true,
                session_id: None,
                last_turn_at: None,
                settle_ready: false,
            })),
        });
        let settled_at = now + Duration::from_secs(1);
        assert_eq!(
            state.refresh_settled_panes_at(None, settled_at, 1_725_000_046),
            0,
            "zero workers alone cannot prove task completion"
        );
        assert!(state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_closing_task_report(
                Some(crate::api::schema::ClosingCompletion::Complete),
                None,
                Some(crate::api::schema::ClosingParseStatus::Ok),
                Some(false),
                settled_at,
            ));
        assert_eq!(
            state.refresh_settled_panes_at(None, settled_at, 1_725_000_047),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn quiet_pane_without_a_resume_plan_settles_and_remains_reapable() {
        let done_since = Instant::now();
        let now = done_since + Duration::from_secs(5 * 60 * 60);
        let (mut state, pane_id) = done_state(false, done_since);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("done pane")
            .seen = true;
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_032), 1);
        assert!(state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.next_done_reap_deadline(now),
            Some(now),
            "settling an unresumable pane must preserve its reap clock"
        );
        assert_eq!(state.next_done_settle_deadline(now), None);
    }

    #[test]
    fn done_settle_skips_the_active_pane_and_pinned_tabs() {
        let done_since = Instant::now();
        let now = done_since + Duration::from_secs(31 * 60);
        let (mut state, pane_id) = done_state(true, done_since);
        state.active = Some(0);
        state.workspaces[0].tabs[0].panes.get_mut(&pane_id);
        assert!(
            state.is_active_pane(0, 0, pane_id),
            "the focused pane is the one the human is looking at"
        );
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_033), 0);
        assert!(!state.pane_is_settled(0, pane_id));

        let (mut state, pane_id) = done_state(true, done_since);
        state.active = None;
        state.workspaces[0].tabs[0].pinned = true;
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_034), 0);
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn quiet_settle_preserves_blocked_and_closing_gate_exclusions() {
        let quiet_since = Instant::now();
        let now = quiet_since + Duration::from_secs(31 * 60);
        let (mut eligible, eligible_pane) = done_state(true, quiet_since);
        eligible.workspaces[0].tabs[0]
            .panes
            .get_mut(&eligible_pane)
            .expect("eligible pane")
            .seen = true;
        assert_eq!(
            eligible.refresh_settled_panes_at(None, now, 1_725_000_038),
            1,
            "the control pane must be eligible before exclusions are applied"
        );

        let (mut blocked, blocked_pane) = done_state(true, quiet_since);
        let blocked_terminal_id = blocked.workspaces[0].tabs[0].panes[&blocked_pane]
            .attached_terminal_id
            .clone();
        blocked
            .terminals
            .get_mut(&blocked_terminal_id)
            .expect("blocked terminal")
            .set_raw_agent_state_for_test(AgentState::Blocked);
        assert_eq!(
            blocked.refresh_settled_panes_at(None, now, 1_725_000_039),
            0
        );

        let (mut gated, gated_pane) = done_state(true, quiet_since);
        let gated_terminal_id = gated.workspaces[0].tabs[0].panes[&gated_pane]
            .attached_terminal_id
            .clone();
        gated
            .terminals
            .get_mut(&gated_terminal_id)
            .expect("gated terminal")
            .closing_gates = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "Gate".into(),
            text: "Choose the release path".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        assert_eq!(gated.refresh_settled_panes_at(None, now, 1_725_000_040), 0);

        let (mut answered, answered_pane) = done_state(true, quiet_since);
        let answered_terminal_id = answered.workspaces[0].tabs[0].panes[&answered_pane]
            .attached_terminal_id
            .clone();
        answered
            .terminals
            .get_mut(&answered_terminal_id)
            .expect("answered terminal")
            .closing_items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Answer".into(),
            text: "Choose the release path".into(),
            blocking: true,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        assert_eq!(
            answered.refresh_settled_panes_at(None, now, 1_725_000_041),
            0
        );
    }

    #[test]
    fn done_settle_can_be_turned_off() {
        let done_since = Instant::now();
        let now = done_since + Duration::from_secs(5 * 60 * 60);
        let (mut state, pane_id) = done_state(true, done_since);
        state.auto_settle_done = false;
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_035), 0);
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(state.next_done_settle_deadline(now).is_none());
    }

    #[test]
    fn done_settle_deadline_is_the_pane_done_window() {
        let done_since = Instant::now();
        let now = done_since + Duration::from_secs(10 * 60);
        let (state, _) = done_state(true, done_since);
        assert_eq!(
            state.next_done_settle_deadline(now),
            Some(now + Duration::from_secs(20 * 60))
        );
    }

    #[test]
    fn detection_snapshot_change_preserves_settlement_and_updates_activity() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        assert!(!state.observe_pane_detection_snapshot_at(pane_id, 1, None, "before", now));
        state.settle_after = Duration::from_secs(3 * 24 * 60 * 60);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - state.settle_after);
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_002), 1);

        assert!(!state.observe_pane_detection_snapshot_at(pane_id, 2, None, "after", now));
        assert!(state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(now),
            Duration::ZERO
        );
    }

    #[test]
    fn working_detection_output_advances_notepad_activity_until_work_stops() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Working);

        let started = Instant::now();
        assert!(!state.observe_pane_detection_snapshot_at(pane_id, 1, None, "before", started));
        let observed = started + Duration::from_secs(60);
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            2,
            None,
            "working output",
            observed
        ));
        let latest_activity = state
            .agent_states
            .snapshot(pane_id, crate::api::schema::AgentStatus::Working)
            .last_acted_at
            .expect("working output observation");

        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Idle);
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            3,
            None,
            "stopped output",
            observed + Duration::from_secs(60),
        ));
        assert_eq!(
            state
                .agent_states
                .snapshot(pane_id, crate::api::schema::AgentStatus::Idle)
                .last_acted_at
                .as_deref(),
            Some(latest_activity.as_str()),
            "after Working stops, the age source remains the last output while Working"
        );
    }

    #[test]
    fn held_shell_blocks_overdue_inactivity_and_ripe_finished_work() {
        let url = "https://github.com/owner/repo/pull/17";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let mut merged_item = item();
        merged_item.pr_url = Some(url.into());
        merged_item.pr_state = Some("merged".into());
        let work = snapshot(merged_item);
        let now = Instant::now();
        state.auto_settle_done = false;
        state.auto_settle_inactive = false;
        state.settle_after = Duration::from_secs(60);
        state.settle_finished_after = Duration::from_secs(30);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - Duration::from_secs(120));
        let armed_at = now - state.settle_finished_after;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), armed_at, 1_725_000_010),
            0
        );

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: true,
            stale_resolution: None,
            observed_at: now,
        });
        state.auto_settle_inactive = true;
        assert!(
            state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(now)
                >= state.settle_after
        );
        assert_eq!(
            state.workspaces[0].tabs[0].panes[&pane_id].finished_since,
            Some(armed_at)
        );
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), now, 1_725_000_011),
            0,
            "a held shell must outrank both ripe settle triggers"
        );
        assert!(!state.pane_is_settled(0, pane_id));

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: None,
            observed_at: now,
        });
        state.auto_settle_inactive = false;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), now, 1_725_000_012),
            0,
            "clearing the shell must arm a fresh finished-work window"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&work),
                now + state.settle_finished_after,
                1_725_000_013,
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn remote_candidate_never_settles_a_colliding_local_pane() {
        let (mut state, pane_id) = state_with_context(Default::default());
        state.agent_host_name = "local".into();
        let candidate = PaneSettlementCandidate {
            agent_ref: crate::api::schema::AgentRef::new("remote", pane_id.raw().to_string())
                .expect("valid remote agent reference"),
            ws_idx: 0,
            pane_id,
        };

        assert_eq!(
            state.settle_owned_candidates(&[candidate], 1_725_000_002),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(state.pending_pane_settlement_changes.is_empty());
    }

    #[test]
    fn refresh_never_settles_remote_row_with_colliding_local_pane_id() {
        let now = Instant::now();
        let (mut state, pane_id, work) = settle_ready_remote_collision(now);
        assert_eq!(state.remote_agent_panel_entries[0].agent_ref.host, "remote");
        assert_eq!(
            state.remote_agent_panel_entries[0].agent_ref.agent,
            pane_id.raw().to_string()
        );
        assert_eq!(
            state.remote_agent_panel_entries[0].entry.identity,
            crate::ui::sidebar::AgentPanelIdentity::Remote(
                state.remote_agent_panel_entries[0].agent_ref.clone()
            ),
            "remote projection carries typed remote identity, not a local pane target"
        );

        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .finished_since = None;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), now, 1_725_000_003),
            0,
            "a fresh finished reading waits through the grace window"
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(state.pending_pane_settlement_changes.is_empty());

        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&work),
                now + state.settle_finished_after,
                1_725_000_004
            ),
            1,
            "the eligible local pane settles through the refresh path"
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn redraw_only_screen_update_keeps_settled_pane_and_inactivity_clock() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now);
        let first_screen = "Done.\n\n────────────────────\n❯\n────────────────────\nstatus 10:41";
        let second_screen = "Done.\n\n────────────────────\n❯\n────────────────────\nstatus 10:42";
        assert_ne!(first_screen, second_screen);
        let first = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            first_screen,
        );
        let second = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            second_screen,
        );
        assert_eq!(first, "Done.");
        assert_eq!(first, second);
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            1,
            Some(crate::detect::Agent::Claude),
            &first,
            now
        ));
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            2,
            Some(crate::detect::Agent::Claude),
            &second,
            now + Duration::from_secs(60)
        ));
        assert!(state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(now + Duration::from_secs(60)),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn detected_agent_classifier_change_rebaselines_without_activity() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            1,
            None,
            "────────────────────\n❯\n────────────────────\nstatus 10:41",
            now,
        ));
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            2,
            Some(crate::detect::Agent::Claude),
            "",
            now + Duration::from_secs(1),
        ));
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn delayed_claude_startup_chrome_keeps_settled_pane() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        let prompt_only = "────────────────────\n❯\n────────────────────\nstatus 10:41";
        let with_startup = concat!(
            "           Claude Code v2.1.263\n",
            " ▐▛███▛█   Fable 5.1 with medium effort\n",
            "▝▜██████▀  Claude Max\n",
            "  ▝▝ ▝▝    ~/repo\n\n\n",
            "                                ◐ medium · /effort\n",
            "────────────────────\n❯\n────────────────────\nstatus 10:42",
        );
        let first = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            prompt_only,
        );
        let second = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            with_startup,
        );
        assert_eq!(first, "");
        assert_eq!(first, second);
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            1,
            Some(crate::detect::Agent::Claude),
            &first,
            now,
        ));
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            2,
            Some(crate::detect::Agent::Claude),
            &second,
            now + Duration::from_secs(60),
        ));
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn user_input_activity_clears_settled_pane() {
        let (mut state, pane_id) = state_with_context(Default::default());
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(state.note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn automated_activity_preserves_settlement_and_resume_guard() {
        let (mut state, pane_id) = state_with_context(Default::default());
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));
        state.workspaces[0]
            .pane_state_mut(pane_id)
            .expect("test pane")
            .settle_resume_guard = true;

        state.note_automated_pane_activity_at(pane_id, Instant::now());

        assert!(state.pane_is_settled(0, pane_id));
        assert!(
            state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .settle_resume_guard
        );
    }

    #[test]
    fn meaningful_activity_marks_the_session_for_persistence() {
        let (mut state, pane_id) = state_with_context(Default::default());
        state.session_dirty = false;
        state.session_dirty_revision = 0;

        assert!(!state.note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(state.session_dirty);
        assert_eq!(state.session_dirty_revision, 1);
    }

    #[test]
    fn transition_to_working_or_blocked_clears_settled_pane() {
        for next_state in [AgentState::Working, AgentState::Blocked] {
            let (mut state, pane_id) = state_with_context(Default::default());
            assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

            state
                .update_terminal_state(pane_id, |terminal| {
                    Some(terminal.set_detected_state_with_screen_signals_at(
                        Some(crate::detect::Agent::Claude),
                        next_state,
                        next_state == AgentState::Blocked,
                        false,
                        next_state == AgentState::Working,
                        false,
                        false,
                        Instant::now(),
                    ))
                })
                .expect("active agent transition");

            assert!(!state.pane_is_settled(0, pane_id), "{next_state:?}");
        }
    }

    #[test]
    fn resume_startup_transition_does_not_unsettle_suspended_pane() {
        for next_state in [AgentState::Working, AgentState::Blocked] {
            let (mut state, pane_id) = state_with_context(Default::default());
            assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));
            state.workspaces[0]
                .pane_state_mut(pane_id)
                .unwrap()
                .settle_resume_guard = true;

            state
                .update_terminal_state(pane_id, |terminal| {
                    Some(terminal.set_detected_state_with_screen_signals_at(
                        Some(crate::detect::Agent::Claude),
                        next_state,
                        next_state == AgentState::Blocked,
                        false,
                        next_state == AgentState::Working,
                        false,
                        false,
                        Instant::now(),
                    ))
                })
                .expect("active agent transition");

            assert!(state.pane_is_settled(0, pane_id), "{next_state:?}");
        }
    }

    #[tokio::test]
    async fn settling_resumable_pane_suspends_and_survives_pane_died() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .respawn_shell_on_exit = true;

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_010));
        assert!(app.flush_pane_settlement_events());

        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_some());
        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("settled runtime remains available for rendering");
        assert!(runtime.is_suspended());
        assert!(runtime
            .snapshot_history()
            .is_some_and(|history| history.contains("retained settlement output")));

        app.handle_internal_event(crate::events::AppEvent::PaneDied { pane_id });

        assert!(app.find_pane(pane_id).is_some());
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(TerminalRuntime::is_suspended));
        assert!(app.state.terminals[&terminal_id].respawn_shell_on_exit);
    }

    #[tokio::test]
    async fn suspending_a_settled_agent_is_not_foreground_activity() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:claude");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_foreground_process(Some("claude".into()), true, Instant::now());
        let shell_pid = app
            .terminal_runtimes
            .get(&terminal_id)
            .and_then(TerminalRuntime::child_pid);

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_014));
        assert!(app.flush_pane_settlement_events());
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(TerminalRuntime::is_suspended));
        app.last_foreground_process_refresh_generation += 1;
        let generation = app.last_foreground_process_refresh_generation;
        let _ = app.handle_foreground_processes_refreshed(
            generation,
            vec![
                crate::app::foreground_process::ForegroundProcessObservation {
                    pane_id,
                    shell_pid,
                    process_name: None,
                    process_active: false,
                    agent_model: None,
                },
            ],
        );
        let _ = app.flush_pane_settlement_events();

        assert!(app.state.pane_is_settled(0, pane_id));
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(TerminalRuntime::is_suspended));
    }

    #[tokio::test]
    async fn settle_then_unsettle_in_one_batch_never_suspends_runtime() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:claude");

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_015));
        assert!(app
            .state
            .note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());

        assert!(!app.state.pane_is_settled(0, pane_id));
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(|runtime| !runtime.is_suspended()));
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
    }

    #[tokio::test]
    async fn automated_input_retirement_preserves_settled_guard() {
        let (mut app, pane_id, _terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_016));
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .expect("test pane")
            .settle_resume_guard = true;

        app.retire_blocked_hook_authority_for_automated_input(pane_id, Instant::now());

        assert!(app.state.pane_is_settled(0, pane_id));
        assert!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .settle_resume_guard
        );
    }

    #[tokio::test]
    async fn settling_unresumable_pane_keeps_runtime_live() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "custom:codex");

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_011));
        assert!(app.flush_pane_settlement_events());

        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(|runtime| !runtime.is_suspended()));
    }

    #[tokio::test]
    async fn snoozing_preserves_the_live_agent_runtime_and_session() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        let session = app.state.terminals[&terminal_id]
            .persisted_agent_session
            .clone();

        assert!(app.state.snooze_pane_at(0, pane_id, 1_725_000_060));
        assert!(app.flush_pane_snooze_events());

        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(|runtime| !runtime.is_suspended()));
        assert_eq!(
            app.state.terminals[&terminal_id].persisted_agent_session,
            session
        );
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unsettling_suspended_pane_starts_resume_at_saved_size() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_012));
        assert!(app.flush_pane_settlement_events());
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "settled-resume-test".into(),
        });

        assert!(app
            .state
            .note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());

        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("unsettling should install a replacement runtime");
        assert!(!runtime.is_suspended());
        assert_eq!(runtime.current_size(), (24, 80));
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_unsettle_resume_keeps_pending_plan() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_013));
        assert!(app.flush_pane_settlement_events());
        app.state.default_shell = "/__herdr_missing_settled_shell__".into();
        app.state.shell_mode = crate::config::ShellModeConfig::Login;

        assert!(app
            .state
            .note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());

        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_some());
    }

    #[tokio::test]
    async fn settlement_label_prefers_pr_and_clears_only_unchanged_label() {
        let pr_url = "https://github.com/owner/repo/pull/42";
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &crate::config::Config::default(),
            crate::work_context::PaneWorkContext {
                pr_urls: vec![pr_url.into()],
                work_title: Some("fallback title".into()),
                ..Default::default()
            },
        );
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        let mut pr = item();
        pr.pr_url = Some(pr_url.into());
        pr.pr_number = Some(42);
        pr.pr_title = Some("Fix login".into());
        app.work_index_snapshot = Some(snapshot(pr));

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_014));
        assert!(app.flush_pane_settlement_events());
        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("#42 Fix login")
        );
        assert_eq!(
            app.state.terminals[&terminal_id]
                .settled_auto_label
                .as_deref(),
            Some("#42 Fix login")
        );

        drop(app.terminal_runtimes.remove(&terminal_id));
        assert!(app
            .state
            .note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());
        assert!(app.state.terminals[&terminal_id].manual_label.is_none());

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_015));
        assert!(app.flush_pane_settlement_events());
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("Human label".into());
        assert!(app
            .state
            .note_human_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());
        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("Human label")
        );
        assert!(app.state.terminals[&terminal_id]
            .settled_auto_label
            .is_none());
    }

    #[tokio::test]
    async fn settlement_label_freezes_current_derived_title_without_pr() {
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &crate::config::Config::default(),
            crate::work_context::PaneWorkContext {
                work_title: Some("Current derived title".into()),
                ..Default::default()
            },
        );
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_016));
        assert!(app.flush_pane_settlement_events());

        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("Current derived title")
        );
    }

    #[tokio::test]
    async fn settlement_label_preserves_session_name_instead_of_agent_identity() {
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &crate::config::Config::default(),
            crate::work_context::PaneWorkContext {
                session_name: Some("Herdr merges pending overview".into()),
                ..Default::default()
            },
        );
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("settled-session")
                .expect("valid session id"),
        });
        terminal.set_detected_state(Some(crate::detect::Agent::Claude), AgentState::Idle);

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_016));
        assert!(app.flush_pane_settlement_events());

        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("Herdr merges pending overview")
        );
        assert_ne!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("claude")
        );
    }

    #[tokio::test]
    async fn settle_stops_agent_false_preserves_live_behavior() {
        let mut config = crate::config::Config::default();
        config.session.settle_stops_agent = false;
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &config,
            crate::work_context::PaneWorkContext {
                work_title: Some("Live title".into()),
                ..Default::default()
            },
        );
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_017));
        assert!(app.flush_pane_settlement_events());

        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(|runtime| !runtime.is_suspended()));
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        assert!(app.state.terminals[&terminal_id].manual_label.is_none());
        assert!(app.state.terminals[&terminal_id]
            .settled_auto_label
            .is_none());
    }

    #[tokio::test]
    async fn manual_settle_keeps_runtime_live_until_grace_expires_then_locks() {
        let config = crate::config::Config::default();
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&config, crate::work_context::PaneWorkContext::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");

        let settled_at = unix_seconds(SystemTime::now());
        assert!(app.state.settle_pane_at(0, pane_id, settled_at));
        assert!(app.flush_pane_settlement_events());
        assert!(!app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .is_suspended());
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        assert_eq!(
            app.state
                .refresh_settled_panes_at(None, Instant::now(), settled_at + 899),
            0
        );
        assert_eq!(
            app.state
                .refresh_settled_panes_at(None, Instant::now(), settled_at + 900),
            1
        );
        assert!(app.flush_pane_settlement_events());
        assert!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .settled_locked
        );
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .is_suspended());
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_some());
        app.focus_settled_pane(crate::app::state::PaneFocusTarget {
            workspace_id: app.state.workspaces[0].id.clone(),
            pane_id,
        });
        assert!(app
            .state
            .settled_view
            .as_ref()
            .is_some_and(|view| view.pane_id == pane_id));
    }

    #[tokio::test]
    async fn human_input_during_grace_unsettles_without_resume_or_nudge() {
        let config = crate::config::Config::default();
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&config, crate::work_context::PaneWorkContext::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        assert!(app
            .state
            .settle_pane_at(0, pane_id, unix_seconds(SystemTime::now())));
        app.flush_pane_settlement_events();

        app.resume_settled_pane_before_input(pane_id);
        app.flush_pane_settlement_events();
        assert!(!app.state.pane_is_settled(0, pane_id));
        assert!(!app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .is_suspended());
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
    }

    #[tokio::test]
    async fn zero_grace_keeps_immediate_stop_behavior() {
        let mut config = crate::config::Config::default();
        config.session.settled_read_only_after_minutes = 0;
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&config, crate::work_context::PaneWorkContext::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_000));
        app.flush_pane_settlement_events();
        assert!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .settled_locked
        );
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .is_suspended());
    }

    #[tokio::test]
    async fn stopped_agent_resume_repaint_keeps_pane_settled() {
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &crate::config::Config::default(),
            crate::work_context::PaneWorkContext::default(),
        );
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("root runtime")
            .test_process_pty_bytes(b"agent output before settle\r\n");
        let now = Instant::now();
        app.refresh_pane_settlement_at_with_wall_clock(now, 1_725_000_017);

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_017));
        assert!(app.flush_pane_settlement_events());
        assert!(app.state.pane_is_settled(0, pane_id));

        // Stopping the agent prints a resume stub, changing the pane snapshot.
        let before_repaint = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("root runtime")
            .detection_text();
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("root runtime")
            .test_process_pty_bytes(b"\r\nResume with herdr agent resume\r\n");
        let after_repaint = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("root runtime")
            .detection_text();
        assert_ne!(before_repaint, after_repaint);
        app.refresh_pane_settlement_at_with_wall_clock(now + Duration::from_secs(1), 1_725_000_018);
        assert!(app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn settlement_transitions_emit_settled_and_unsettled_events() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub.clone(),
        );
        let (state, pane_id) = state_with_context(Default::default());
        app.state = state;
        let now = Instant::now();
        app.state.settle_after = Duration::ZERO;

        assert_eq!(
            app.state.refresh_settled_panes_at(None, now, 1_725_000_003),
            1
        );
        assert!(app.flush_pane_settlement_events());
        assert!(app.state.note_human_pane_activity_at(pane_id, now));
        assert!(app.flush_pane_settlement_events());

        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| matches!(
            &event.data,
            crate::api::schema::EventData::PaneSettled {
                settled_at: 1_725_000_003,
                ..
            }
        )));
        assert!(events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::PaneUnsettled { .. }
        )));
    }

    #[tokio::test]
    async fn agent_status_transition_is_queued_and_flushed_to_log() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub,
        );
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let _ = state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Working);
        app.state = state;
        app.terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                80,
                24,
                b"recent session tail\n",
            ),
        );
        let log = crate::status_log::StatusLog::new(crate::status_log::test_tempdir());
        app.status_log = Some(log.clone());

        transition_agent_state(&mut app.state, pane_id, AgentState::Blocked, Instant::now());

        assert_eq!(
            app.state.pending_status_transitions,
            [crate::status_log::PendingTransition {
                pane_id,
                agent: Some(crate::detect::Agent::Codex),
                from_state: AgentState::Working,
                to_state: AgentState::Blocked,
            }]
        );
        app.flush_status_transitions();

        assert!(app.state.pending_status_transitions.is_empty());
        let records = log.read(&crate::status_log::ReadFilter::default());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].pane, app.public_pane_id(0, pane_id).unwrap());
        assert!(time::OffsetDateTime::parse(
            &records[0].ts,
            &time::format_description::well_known::Rfc3339,
        )
        .is_ok());
        assert!(records[0].host.is_some());
        assert!(records[0].session.is_some());
        assert_eq!(records[0].agent.as_deref(), Some("codex"));
        assert_eq!(records[0].from_state, "working");
        assert_eq!(records[0].to_state, "blocked");
        assert_eq!(records[0].source, crate::status_log::Source::Detector);
        assert!(records[0].tail.contains("recent session tail"));
        assert!(records[0].tail.len() <= crate::status_log::TAIL_MAX_BYTES);
        assert!(records[0].tail.lines().count() <= crate::status_log::TAIL_MAX_LINES);

        transition_agent_state(&mut app.state, pane_id, AgentState::Idle, Instant::now());
        assert_eq!(app.state.pending_status_transitions.len(), 1);
        app.status_log = None;
        app.flush_status_transitions();

        assert!(app.state.pending_status_transitions.is_empty());
        assert_eq!(log.read(&crate::status_log::ReadFilter::default()).len(), 1);
    }
}
