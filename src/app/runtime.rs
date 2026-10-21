use std::time::Instant;

#[cfg(test)]
use std::time::Duration;

use super::{
    background_update_check_enabled, App, AUTO_UPDATE_CHECK_INTERVAL, MIN_RENDER_INTERVAL,
};
fn retain_detached_process_after_wait(
    pid: u32,
    result: std::io::Result<Option<std::process::ExitStatus>>,
) -> bool {
    match result {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
        Err(err) => {
            tracing::warn!(pid, err = %err, "failed to reap detached process");
            false
        }
    }
}

impl App {
    pub(crate) fn reap_finished_detached_processes(&mut self) {
        self.detached_process_children
            .retain_mut(|child| retain_detached_process_after_wait(child.id(), child.try_wait()));
    }

    pub(crate) fn shutdown_terminal_runtime(&mut self, terminal_id: crate::terminal::TerminalId) {
        if let Some(runtime) = self.terminal_runtimes.remove(&terminal_id) {
            if let Some(pane_id) = self.state.workspaces.iter().find_map(|workspace| {
                workspace.tabs.iter().find_map(|tab| {
                    tab.panes.iter().find_map(|(pane_id, pane)| {
                        (pane.attached_terminal_id == terminal_id).then_some(*pane_id)
                    })
                })
            }) {
                self.expected_pane_exit_epochs
                    .insert(pane_id, runtime.epoch());
            }
            runtime.shutdown();
        }
    }

    pub(crate) fn shutdown_detached_terminal_runtimes(&mut self) {
        let terminal_ids = std::mem::take(&mut self.state.terminal_runtime_shutdowns);
        for terminal_id in terminal_ids {
            self.shutdown_terminal_runtime(terminal_id);
        }
    }

    pub(crate) fn sync_agent_metadata_deadline(&mut self) {
        self.agent_metadata_deadline = self.state.next_agent_metadata_expiry();
    }

    pub(crate) fn expire_due_metadata(&mut self, now: Instant) -> bool {
        let Some(deadline) = self
            .agent_metadata_deadline
            .filter(|deadline| now >= *deadline)
        else {
            return false;
        };
        self.expire_metadata_at(deadline, now);
        true
    }

    pub(crate) fn expire_metadata_at(&mut self, deadline: Instant, now: Instant) {
        let previous_toast = self.state.toast.clone();
        for update in self.state.expire_agent_metadata_at(deadline, now) {
            self.refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
            self.emit_pane_state_update(&update);
        }
        let (panes, workspaces) = self.state.expire_metadata_tokens(now);
        for (ws_idx, pane_id) in panes {
            self.emit_pane_updated(ws_idx, pane_id);
        }
        for ws_idx in workspaces {
            self.emit_workspace_token_updated(ws_idx);
        }
        self.sync_agent_metadata_deadline();
    }

    /// Width-lease TTL sweep + debounced-shrink application (#137). Mirrors
    /// [`App::expire_due_metadata`]: invoked from the same tick sites with an
    /// injected `now`, never on the per-render path. Evicts expired leases and,
    /// for terminals whose effective width changed (a lease expired, or a
    /// scheduled shrink came due), reconciles the PTY winsize. Returns true when
    /// a resize was applied or a lease was evicted (both warrant a render — an
    /// evicted last lease lets the TUI reclaim the layout width via the gate).
    pub(crate) fn expire_pty_leases_and_apply_shrinks(&mut self, now: Instant) -> bool {
        if self.state.pty_width_leases.is_empty() && self.state.pty_pending_shrinks.is_empty() {
            return false;
        }

        // 1. Evict expired leases, collecting terminals whose lease set changed.
        let mut dirty: std::collections::HashSet<crate::terminal::TerminalId> =
            std::collections::HashSet::new();
        let mut evicted_any = false;
        self.state.pty_width_leases.retain(|terminal_id, viewers| {
            let before = viewers.len();
            viewers.retain(|_, lease| lease.expires_at > now);
            if viewers.len() != before {
                evicted_any = true;
                dirty.insert(terminal_id.clone());
            }
            !viewers.is_empty()
        });

        // 2. Terminals whose scheduled shrink's debounce has elapsed also need a
        //    reconcile to apply it.
        for (terminal_id, pending) in &self.state.pty_pending_shrinks {
            if now >= pending.deadline {
                dirty.insert(terminal_id.clone());
            }
        }

        // 3. Reconcile each dirty terminal via its current pane location.
        let mut applied = false;
        if !dirty.is_empty() {
            for (ws_idx, pane_id, terminal_id) in self.state.pane_locations_for_terminals(&dirty) {
                // The sweep only ever applies debounced shrinks (or grows that
                // race in), never an immediate shrink.
                applied |= self.reconcile_pty_lease_size(ws_idx, pane_id, &terminal_id, now, false);
            }
        }

        // 4. Drop pending shrinks for terminals that no longer have any lease
        //    (all leases expired, or the pane vanished) — nothing to shrink to.
        if !self.state.pty_pending_shrinks.is_empty() {
            let leased: std::collections::HashSet<crate::terminal::TerminalId> =
                self.state.pty_width_leases.keys().cloned().collect();
            self.state
                .pty_pending_shrinks
                .retain(|terminal_id, _| leased.contains(terminal_id));
        }

        applied || evicted_any
    }

    /// Reconcile a terminal's real PTY winsize toward the width arbiter's
    /// effective size (#137). Grows (or an unchanged size) always apply
    /// immediately. Shrinks apply immediately when `immediate` is set — used for
    /// an explicit synchronous `pane.set_pty_size` call (a set, or a release with
    /// other viewers still attached), which the caller expects to take effect at
    /// once — and are otherwise deferred behind a per-terminal debounce deadline
    /// that the sweep applies once stable. The debounced path is the one that
    /// matters for the fix: when a viewer's lease is dropped by the tick (its
    /// `pane.stream` closing or its TTL expiring) the pane must not thrash the
    /// PTY with an immediate SIGWINCH down to the next-widest. `now` is injected
    /// so the debounce is deterministic in tests. Returns true only when a resize
    /// actually changed the winsize.
    pub(crate) fn reconcile_pty_lease_size(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        terminal_id: &crate::terminal::TerminalId,
        now: Instant,
        immediate: bool,
    ) -> bool {
        let effective = self.state.effective_pty_size(terminal_id);

        // Read the current winsize under a scoped immutable borrow so the lease /
        // pending-shrink maps can be mutated afterwards without a borrow conflict.
        let Some((cur_rows, cur_cols)) = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .map(|runtime| runtime.current_size())
        else {
            // Pane/runtime is gone: nothing to resize. Drop any stale pending
            // shrink so it cannot leak.
            self.state.pty_pending_shrinks.remove(terminal_id);
            return false;
        };

        let Some((rows, cols, cell_width_px, cell_height_px)) = effective else {
            // No live lease: do NOT force a size — the TUI reclaims the winsize
            // via the render gate. Clear any pending shrink.
            self.state.pty_pending_shrinks.remove(terminal_id);
            return false;
        };

        // Grow (or unchanged in both dimensions), or an explicit synchronous
        // shrink, applies immediately.
        if immediate || (rows >= cur_rows && cols >= cur_cols) {
            self.state.pty_pending_shrinks.remove(terminal_id);
            self.resize_pane_runtime(ws_idx, pane_id, rows, cols, cell_width_px, cell_height_px);
            return (rows, cols) != (cur_rows, cur_cols);
        }

        // Debounced shrink (a wider viewer left): honour (or arm) the debounce.
        let target = (rows, cols, cell_width_px, cell_height_px);
        match self.state.pty_pending_shrinks.get(terminal_id).copied() {
            Some(pending) if pending.target == target && now >= pending.deadline => {
                self.state.pty_pending_shrinks.remove(terminal_id);
                self.resize_pane_runtime(
                    ws_idx,
                    pane_id,
                    rows,
                    cols,
                    cell_width_px,
                    cell_height_px,
                );
                true
            }
            // Still waiting out the debounce for the same target: keep the
            // existing deadline (do not restart the clock).
            Some(pending) if pending.target == target => false,
            // No pending shrink, or the target changed (a wider viewer left and
            // the next-widest is different): (re)arm the debounce.
            _ => {
                self.state.pty_pending_shrinks.insert(
                    terminal_id.clone(),
                    crate::app::state::PtyPendingShrink {
                        target,
                        deadline: now + crate::app::state::PTY_SHRINK_DEBOUNCE,
                    },
                );
                false
            }
        }
    }

    /// Drive a pane's PTY winsize through the same runtime call the TUI uses.
    /// `PaneRuntime::resize` clamps to its minimums (rows >= 2, cols >= 4) and
    /// early-returns when unchanged, so no-op resizes dedup.
    fn resize_pane_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) {
        if let Some(runtime) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        {
            runtime.resize(rows, cols, cell_width_px, cell_height_px);
        }
    }

    pub(crate) fn can_render_now(&self, now: Instant) -> bool {
        match self.last_render_at {
            Some(last_render_at) => now.duration_since(last_render_at) >= MIN_RENDER_INTERVAL,
            None => true,
        }
    }

    pub(crate) fn can_present_now(&self, now: Instant) -> bool {
        match self.last_presentation_at {
            Some(last_presentation_at) => {
                now.duration_since(last_presentation_at) >= MIN_RENDER_INTERVAL
            }
            None => true,
        }
    }

    pub(crate) fn record_render_attempt(&mut self, now: Instant, presentation: bool) {
        self.last_render_at = Some(now);
        if presentation {
            self.last_presentation_at = Some(now);
        }
    }

    pub(crate) fn run_auto_update_check(&mut self) {
        if !background_update_check_enabled(
            self.policy.background_updates,
            self.update_version_check_enabled,
        ) {
            self.next_auto_update_check = None;
            return;
        }

        self.next_auto_update_check = self
            .state
            .update_available
            .is_none()
            .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        if self.state.update_available.is_some() {
            return;
        }

        let update_tx = self.event_tx.clone();
        if let Err(err) = crate::thread_spawn::spawn_named("herdr-update-check", move || {
            crate::update::auto_update(update_tx)
        }) {
            tracing::warn!(err = %err, "failed to spawn update check thread");
        }
    }

    pub(crate) fn run_agent_manifest_update_check(&mut self) {
        if !background_update_check_enabled(
            self.policy.background_updates,
            self.update_manifest_check_enabled,
        ) {
            self.next_agent_manifest_update_check = None;
            return;
        }

        self.next_agent_manifest_update_check = Some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        let manifest_update_tx = self.event_tx.clone();
        if let Err(err) = crate::thread_spawn::spawn_named("herdr-manifest-check", move || {
            crate::detect::manifest_update::auto_update(manifest_update_tx)
        }) {
            tracing::warn!(err = %err, "failed to spawn agent manifest check thread");
        }
    }

    pub(crate) fn next_headless_loop_deadline_with_git_refresh(
        &self,
        now: Instant,
        needs_render: bool,
        include_git_refresh: bool,
    ) -> Option<Instant> {
        let render_deadline = if needs_render {
            self.last_render_at
                .map(|last_render_at| last_render_at + MIN_RENDER_INTERVAL)
                .filter(|deadline| *deadline > now)
        } else {
            None
        };

        [
            self.config_diagnostic_deadline,
            self.toast_deadline,
            self.state.next_pending_agent_notification_deadline(),
            self.state.next_managed_agent_deadline(),
            include_git_refresh
                .then(|| self.git_refresh_deadline(now))
                .flatten(),
            self.restored_worktree_validation_retry_at,
            self.next_auto_update_check,
            self.next_agent_manifest_update_check,
            self.agent_metadata_deadline,
            self.pending_agent_resume_deadline,
            self.session_save_deadline,
            self.next_tab_bar_status_deadline(),
            render_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    #[cfg(test)]
    pub(crate) fn drain_internal_events(&mut self) -> bool {
        self.drain_internal_events_up_to(super::APP_EVENT_DRAIN_LIMIT)
            .1
    }

    #[cfg(test)]
    pub(crate) fn drain_all_internal_events(&mut self) -> bool {
        let mut changed = false;
        loop {
            let (had_event, batch_changed) =
                self.drain_internal_events_up_to(super::APP_EVENT_DRAIN_LIMIT);
            changed |= batch_changed;
            if !had_event {
                break;
            }
        }
        changed
    }

    #[cfg(test)]
    fn drain_internal_events_up_to(&mut self, limit: usize) -> (bool, bool) {
        let mut had_event = false;
        let mut changed = false;
        for _ in 0..limit {
            let Ok(ev) = self.event_rx.try_recv() else {
                break;
            };
            had_event = true;
            changed |= self.handle_internal_event_with_render_impact(ev);
        }
        (had_event, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn hidden_render_attempt_keeps_presentation_cadence_available() {
        let (mut app, _) = test_app_with_pane();
        let initial_presentation = Instant::now();
        app.record_render_attempt(initial_presentation, true);

        let hidden_attempt = initial_presentation + MIN_RENDER_INTERVAL;
        app.record_render_attempt(hidden_attempt, false);
        let foreground_echo = hidden_attempt + Duration::from_millis(1);

        assert!(!app.can_render_now(foreground_echo));
        assert!(app.can_present_now(foreground_echo));
    }

    #[test]
    fn interrupted_detached_process_wait_keeps_child_for_retry() {
        let interrupted = std::io::Error::new(std::io::ErrorKind::Interrupted, "test interrupt");

        assert!(retain_detached_process_after_wait(42, Err(interrupted)));
    }

    fn test_app_with_pane() -> (super::super::App, crate::layout::PaneId) {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.view.pane_infos.push(crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        });
        (app, pane_id)
    }
}
