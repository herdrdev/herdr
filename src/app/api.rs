use std::time::{Duration, Instant};

pub(crate) mod accounts;
mod agent_view;
pub(super) mod agents;
mod env;
mod gram;
#[cfg(unix)]
pub(crate) use gram::gram_push_notification;
mod integrations;
mod layouts;
mod machines;
mod panes;
pub(crate) mod plugins;
pub(super) mod remote_push;
pub(super) mod responses;
mod session;
mod session_transfer;
mod tabs;
pub(crate) mod transcript;
pub(crate) mod usage_fetch;
mod workspaces;
mod worktrees;

use super::{api_helpers::pane_agent_status, App, Mode, OverlayPaneState, ToastKind};
use crate::events::AppEvent;
use crate::push::sanitized_notification_text;

const API_NOTIFICATION_RATE_LIMIT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const WINDOWS_POWERSHELL_AGENT_EXIT_RESPAWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeExitAction {
    RespawnShell,
    ClosePane,
}

impl App {
    pub(crate) fn handle_internal_event_with_render_impact(&mut self, ev: AppEvent) -> bool {
        if !self.detector_event_runtime_matches(&ev) {
            if let Some((pane_id, runtime_epoch)) = ev.detector_runtime() {
                tracing::debug!(
                    pane = pane_id.raw(),
                    ?runtime_epoch,
                    "ignored detector event from a retired pane runtime"
                );
            }
            return false;
        }
        match ev {
            AppEvent::GitStatusRefreshed {
                results,
                cache_updates,
            } => self.handle_git_status_refreshed(results, cache_updates),
            AppEvent::TabBarCommandFinished {
                generation,
                segment_index,
                result,
            } => self.handle_tab_bar_command_finished(generation, segment_index, result),
            AppEvent::UsageRefreshed { account_id, usage } => {
                self.handle_usage_refreshed(account_id, usage);
                // Live usage is surfaced only through the `accounts.list` API,
                // never the TUI, so it never dirties a render.
                false
            }
            AppEvent::AgentSessionTransferPrepared {
                terminal_id,
                transfer_id,
                result,
            } => self.handle_agent_session_transfer_prepared(terminal_id, transfer_id, *result),
            AppEvent::AgentSessionTransferCutoverVerified {
                terminal_id,
                transfer_id,
                result,
            } => self.handle_agent_session_transfer_cutover_verified(
                terminal_id,
                transfer_id,
                result,
            ),
            AppEvent::AgentSessionTransferRuntimeVerified {
                terminal_id,
                transfer_id,
                kind,
                process_pid,
                result,
            } => self.handle_agent_session_transfer_runtime_verified(
                terminal_id,
                transfer_id,
                kind,
                process_pid,
                result,
            ),
            ev @ AppEvent::AgentProcessDetected { pane_id, agent, .. } => {
                self.handle_internal_event(ev);
                self.reconcile_codex_session_transfer_process(pane_id, agent);
                true
            }
            ev @ AppEvent::StateChanged {
                pane_id,
                process_exited: true,
                ..
            } => {
                let transfer_owns_exit = self
                    .find_pane(pane_id)
                    .and_then(|(_, pane)| self.state.terminals.get(&pane.attached_terminal_id))
                    .is_some_and(|terminal| terminal.session_transfer.is_some());
                if transfer_owns_exit {
                    // During a transfer, only PaneDied is process-exit authority.
                    // Even a current-runtime detector exit can race replacement
                    // teardown and cannot prove which managed resume command exited.
                    return false;
                }
                self.handle_internal_event(ev);
                true
            }
            ev @ AppEvent::AgentSessionReported { .. } => {
                let report = if let AppEvent::AgentSessionReported {
                    pane_id,
                    source,
                    agent_label,
                    session_ref,
                    session_cursor,
                    process_pid,
                    ..
                } = &ev
                {
                    Some((
                        *pane_id,
                        source.clone(),
                        agent_label.clone(),
                        session_ref.clone(),
                        session_cursor.clone(),
                        *process_pid,
                    ))
                } else {
                    None
                };
                let accepted_generation_before = report.as_ref().and_then(|(pane_id, ..)| {
                    let (_, pane) = self.find_pane(*pane_id)?;
                    self.state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .map(|terminal| terminal.accepted_session_report_generation())
                });
                self.handle_internal_event(ev);
                if let Some((
                    pane_id,
                    source,
                    agent_label,
                    session_ref,
                    session_cursor,
                    process_pid,
                )) = report
                {
                    let accepted = accepted_generation_before.is_some_and(|before| {
                        self.find_pane(pane_id)
                            .and_then(|(_, pane)| {
                                self.state.terminals.get(&pane.attached_terminal_id)
                            })
                            .is_some_and(|terminal| {
                                terminal.accepted_session_report_generation() != before
                            })
                    });
                    self.reconcile_agent_session_transfer_report(
                        pane_id,
                        &source,
                        &agent_label,
                        session_ref.as_ref(),
                        session_cursor.as_deref(),
                        process_pid,
                        accepted,
                    );
                }
                true
            }
            AppEvent::WorktreeReadFinished(result) => {
                let changes_workspace = matches!(
                    &result.request.method,
                    crate::api::schema::Method::WorktreeOpen(_)
                );
                self.handle_api_worktree_read_finished(*result);
                changes_workspace
            }
            ev @ AppEvent::TerminalBell { .. } => {
                self.handle_internal_event(ev);
                false
            }
            ev => {
                self.handle_internal_event(ev);
                true
            }
        }
    }

    fn handle_git_status_refreshed(
        &mut self,
        results: Vec<crate::workspace::WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, crate::workspace::GitStatusCacheEntry)>,
    ) -> bool {
        self.git_refresh_in_flight = false;
        for (key, entry) in cache_updates {
            self.git_status_cache.insert(key, entry);
        }
        if self.git_refresh_due_after_in_flight {
            self.mark_git_status_refresh_due(Instant::now());
            self.git_refresh_due_after_in_flight = false;
        } else {
            self.last_git_remote_status_refresh = Some(Instant::now());
        }
        let changed = self
            .state
            .apply_workspace_git_statuses(&self.terminal_runtimes, results);
        if changed {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changed
    }

    /// Apply a finished background live-usage fetch: clear the in-flight marker
    /// (so a later `accounts.list` can retry) and, on success, cache the result.
    /// A `None` result (failure/timeout) leaves any existing cache untouched.
    fn handle_usage_refreshed(
        &mut self,
        account_id: String,
        usage: Option<(crate::api::schema::AccountUsage, bool)>,
    ) {
        self.usage_refresh_inflight.remove(&account_id);
        if let Some((usage, active)) = usage {
            self.usage_cache.insert(
                account_id,
                usage_fetch::CachedUsage {
                    fetched_at: Instant::now(),
                    usage,
                    active,
                },
            );
        }
    }

    pub(crate) fn handle_internal_event(&mut self, ev: AppEvent) {
        let _ = self.handle_internal_event_with_pane_updates(ev);
    }

    pub(crate) fn pane_runtime_epoch_matches(
        &self,
        pane_id: crate::layout::PaneId,
        runtime_epoch: Option<u64>,
    ) -> bool {
        let Some(runtime_epoch) = runtime_epoch else {
            return true;
        };
        if let Some(current_epoch) = self.current_pane_runtime_epoch(pane_id) {
            return current_epoch == runtime_epoch;
        }
        self.expected_pane_exit_epochs.get(&pane_id) == Some(&runtime_epoch)
    }

    fn current_pane_runtime_epoch(&self, pane_id: crate::layout::PaneId) -> Option<u64> {
        if let Some(popup) = self
            .state
            .popup_pane
            .as_ref()
            .filter(|popup| popup.pane_id == pane_id)
        {
            return self
                .terminal_runtimes
                .get(&popup.terminal_id)
                .map(crate::terminal::TerminalRuntime::epoch);
        }
        let (_, pane) = self.find_pane(pane_id)?;
        self.terminal_runtimes
            .get(&pane.attached_terminal_id)
            .map(crate::terminal::TerminalRuntime::epoch)
    }

    pub(crate) fn detector_event_runtime_matches(&self, ev: &AppEvent) -> bool {
        let Some((pane_id, runtime_epoch)) = ev.detector_runtime() else {
            return true;
        };
        let Some(runtime_epoch) = runtime_epoch else {
            return true;
        };
        self.current_pane_runtime_epoch(pane_id) == Some(runtime_epoch)
    }

    pub(crate) fn consume_expected_pane_exit_epoch(
        &mut self,
        pane_id: crate::layout::PaneId,
        runtime_epoch: Option<u64>,
    ) {
        if runtime_epoch.is_some()
            && self.expected_pane_exit_epochs.get(&pane_id).copied() == runtime_epoch
        {
            self.expected_pane_exit_epochs.remove(&pane_id);
        }
    }

    pub(crate) fn handle_internal_event_with_pane_updates(
        &mut self,
        ev: AppEvent,
    ) -> Vec<crate::app::actions::PaneStateUpdate> {
        if !self.detector_event_runtime_matches(&ev) {
            if let Some((pane_id, runtime_epoch)) = ev.detector_runtime() {
                tracing::debug!(
                    pane = pane_id.raw(),
                    ?runtime_epoch,
                    "ignored detector event from a retired pane runtime"
                );
            }
            return Vec::new();
        }
        if let AppEvent::PaneDied {
            pane_id,
            runtime_epoch,
            ..
        } = &ev
        {
            let matches = self.pane_runtime_epoch_matches(*pane_id, *runtime_epoch);
            self.consume_expected_pane_exit_epoch(*pane_id, *runtime_epoch);
            if !matches {
                tracing::debug!(
                    pane = pane_id.raw(),
                    ?runtime_epoch,
                    "ignored PaneDied from a retired pane runtime"
                );
                return Vec::new();
            }
        }

        let mut worktree_restore_failed = false;
        let ev = match ev {
            AppEvent::WorktreeRuntimeRestoreFailed {
                pane_id,
                operation_id,
            } => {
                if !self.claim_worktree_runtime_restore_failure(pane_id, operation_id) {
                    return Vec::new();
                }
                worktree_restore_failed = true;
                AppEvent::PaneDied {
                    pane_id,
                    runtime_epoch: None,
                    exit_reason: crate::platform::ChildExitReason::Exited,
                }
            }
            ev => ev,
        };
        if matches!(
            &ev,
            AppEvent::TerminalBell { .. } | AppEvent::ClipboardWrite { .. }
        ) {
            return Vec::new();
        }

        if let AppEvent::GitStatusRefreshed {
            results,
            cache_updates,
        } = ev
        {
            self.handle_git_status_refreshed(results, cache_updates);
            return Vec::new();
        }

        if let AppEvent::RestoredWorktreeSpaceChecked {
            workspace_id,
            expected,
            valid,
        } = ev
        {
            self.pending_restored_worktree_spaces
                .retain(|(id, space)| id != &workspace_id || space != &expected);
            let changed_workspace = (!valid)
                .then(|| {
                    self.state.workspaces.iter().position(|workspace| {
                        workspace.id == workspace_id
                            && workspace.worktree_space.as_ref() == Some(&expected)
                    })
                })
                .flatten();
            self.state
                .handle_app_event(AppEvent::RestoredWorktreeSpaceChecked {
                    workspace_id,
                    expected,
                    valid,
                });
            if let Some(ws_idx) = changed_workspace {
                self.emit_workspace_updated(ws_idx);
            }
            return Vec::new();
        }

        if let AppEvent::TabBarCommandFinished {
            generation,
            segment_index,
            result,
        } = ev
        {
            let _ = self.handle_tab_bar_command_finished(generation, segment_index, result);
            return Vec::new();
        }

        if let AppEvent::UsageRefreshed { account_id, usage } = ev {
            self.handle_usage_refreshed(account_id, usage);
            return Vec::new();
        }

        if let AppEvent::PluginCommandFinished {
            log_id,
            finished_unix_ms,
            exit_code,
            stdout,
            stderr,
            error,
        } = ev
        {
            self.state.plugin_commands_in_flight =
                self.state.plugin_commands_in_flight.saturating_sub(1);
            if let Some(log) = self
                .state
                .plugin_command_logs
                .iter_mut()
                .find(|log| log.log_id == log_id)
            {
                log.finished_unix_ms = Some(finished_unix_ms);
                log.exit_code = exit_code;
                log.stdout = Some(stdout);
                log.stderr = Some(stderr);
                log.error = error;
                log.status = if log.error.is_none() && log.exit_code == Some(0) {
                    crate::api::schema::PluginCommandStatus::Succeeded
                } else {
                    crate::api::schema::PluginCommandStatus::Failed
                };
            }
            return Vec::new();
        }

        if let AppEvent::WorktreeReadFinished(result) = ev {
            self.handle_api_worktree_read_finished(*result);
            return Vec::new();
        }

        if let AppEvent::WorktreeAddFinished(result) = ev {
            self.handle_api_worktree_add_finished(*result);
            return Vec::new();
        }

        if let AppEvent::WorktreeRemoveFinished(result) = ev {
            return self.handle_api_worktree_remove_finished(*result);
        }

        let mut worktree_restore_updates = Vec::new();
        if let AppEvent::PaneDied { pane_id, .. } = &ev {
            if self
                .state
                .popup_pane
                .as_ref()
                .is_some_and(|popup| popup.pane_id == *pane_id)
            {
                self.close_popup_pane();
                return Vec::new();
            }
            if let Some((ws_idx, _)) = self.find_pane(*pane_id) {
                if let Some(runtime) = self.state.runtime_for_pane_in_workspace(
                    &self.terminal_runtimes,
                    ws_idx,
                    *pane_id,
                ) {
                    runtime.normalize_alternate_screen_on_exit();
                }
            }
            if worktree_restore_failed {
                worktree_restore_updates
                    .extend(self.publish_worktree_runtime_agent_release(*pane_id));
            } else {
                let expected_exit = self
                    .pending_worktree_remove_runtime_exits
                    .get_mut(pane_id)
                    .map(|remaining| {
                        *remaining -= 1;
                        *remaining == 0
                    });
                if let Some(remove_entry) = expected_exit {
                    let restore_failed = if remove_entry {
                        self.pending_worktree_remove_runtime_exits.remove(pane_id);
                        let restore_requested = self
                            .pending_worktree_remove_runtime_restores
                            .remove(pane_id)
                            .is_some();
                        if restore_requested {
                            worktree_restore_updates
                                .extend(self.publish_worktree_runtime_agent_release(*pane_id));
                        }
                        restore_requested && !self.respawn_shell_for_launch_pane(*pane_id, false)
                    } else {
                        false
                    };
                    if !restore_failed {
                        return worktree_restore_updates;
                    }
                }
                let previous_toast = self.state.toast.clone();
                if let Some(update) = self
                    .state
                    .publish_pane_process_exit_if_agent(*pane_id, false)
                {
                    self.sync_full_lifecycle_authority_detection_pauses();
                    self.refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
                    self.emit_pane_state_update(&update);
                    self.emit_apns_agent_notifications(std::slice::from_ref(&update), true);
                    self.emit_live_activity_updates();
                }
                // A replacement that exits before reporting the staged session is
                // a transfer failure. Arm source rollback before choosing the
                // respawn action below.
                self.session_transfer_process_exited(*pane_id);
                if self.runtime_exit_action(*pane_id) == RuntimeExitAction::RespawnShell {
                    let relaunched = if self.pane_has_pending_agent_resume(*pane_id) {
                        self.resume_pending_agent_for_pane(*pane_id)
                            || self.respawn_shell_for_launch_pane(*pane_id, true)
                    } else {
                        self.respawn_shell_for_launch_pane(*pane_id, true)
                    };
                    if relaunched {
                        self.overlay_panes.remove(pane_id);
                        self.render_dirty.request_generic();
                        self.render_notify.notify_one();
                        return worktree_restore_updates;
                    }
                }
            }
        }

        let checkpointed_pane_exit = matches!(
            &ev,
            AppEvent::PaneDied {
                pane_id,
                exit_reason,
                ..
            } if exit_reason.requires_session_checkpoint() && self.find_pane(*pane_id).is_some() && !self.overlay_panes.contains_key(pane_id)
        );
        if checkpointed_pane_exit {
            self.checkpoint_session_before_pane_exit();
        }
        let overlay_state = if let AppEvent::PaneDied { pane_id, .. } = &ev {
            self.overlay_panes.remove(pane_id).map(|overlay| {
                let was_overlay_active =
                    self.state
                        .is_active_pane(overlay.ws_idx, overlay.tab_idx, *pane_id);
                let tab_before_exit = self
                    .state
                    .workspaces
                    .get(overlay.ws_idx)
                    .and_then(|ws| ws.tabs.get(overlay.tab_idx));
                let was_overlay_focused_in_tab =
                    tab_before_exit.is_some_and(|tab| tab.layout.focused() == *pane_id);
                let tab_zoomed_before_exit = tab_before_exit.map(|tab| tab.zoomed);
                (
                    overlay,
                    was_overlay_active,
                    was_overlay_focused_in_tab,
                    tab_zoomed_before_exit,
                )
            })
        } else {
            None
        };

        if let AppEvent::PaneDied { pane_id, .. } = &ev {
            if let Some((ws_idx, _)) = self.find_pane(*pane_id) {
                if let Some(public_pane_id) = self.public_pane_id(ws_idx, *pane_id) {
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::PaneExited,
                        data: crate::api::schema::EventData::PaneExited {
                            pane_id: public_pane_id,
                            workspace_id: self.public_workspace_id(ws_idx),
                        },
                    });
                }
            }
        }
        let pane_exit_layout_target = if let AppEvent::PaneDied { pane_id, .. } = &ev {
            self.find_pane(*pane_id).and_then(|(ws_idx, _)| {
                self.layout_update_target_after_pane_removal(ws_idx, *pane_id)
            })
        } else {
            None
        };

        let released_agent = if let AppEvent::HookAgentReleased {
            pane_id,
            known_agent,
            ..
        } = &ev
        {
            known_agent.map(|agent| (*pane_id, agent))
        } else {
            None
        };

        let update_ready = if let AppEvent::UpdateReady {
            version,
            install_command,
        } = &ev
        {
            Some((version.clone(), install_command.clone()))
        } else {
            None
        };
        let manifest_update_agents =
            if let AppEvent::AgentDetectionManifestsUpdated { activated, .. } = &ev {
                Some(activated.clone())
            } else {
                None
            };
        let terminal_cwd_reported = matches!(ev, AppEvent::TerminalCwdReported { .. });
        let previous_toast = self.state.toast.clone();
        let mut pane_updates = self.state.handle_app_event(ev);
        if update_ready.is_some() {
            self.state.latest_release_notes = crate::release_notes::load_latest();
        }
        if checkpointed_pane_exit {
            self.finish_checkpointed_pane_exit();
        }
        if let Some(agents) = manifest_update_agents {
            self.reset_agent_detection_for_agents(&agents);
        }
        if let Some((pane_id, agent)) = released_agent {
            if pane_updates.iter().any(|update| update.pane_id == pane_id) {
                if let Some((ws_idx, _)) = self.find_pane(pane_id) {
                    if let Some(runtime) = self.state.runtime_for_pane_in_workspace(
                        &self.terminal_runtimes,
                        ws_idx,
                        pane_id,
                    ) {
                        runtime.begin_graceful_release(agent);
                    }
                }
            }
        }
        self.sync_full_lifecycle_authority_detection_pauses();
        if terminal_cwd_reported {
            self.request_git_identity_refresh(Instant::now());
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        for update in &pane_updates {
            self.refresh_new_herdr_toast_context_for_update(update, &previous_toast);
            self.emit_pane_state_update(update);
        }
        self.emit_apns_agent_notifications(&pane_updates, false);
        self.emit_live_activity_updates();
        self.sync_agent_metadata_deadline();
        if let Some((
            overlay,
            was_overlay_active,
            was_overlay_focused_in_tab,
            tab_zoomed_before_exit,
        )) = overlay_state
        {
            self.restore_overlay_after_exit(
                overlay,
                was_overlay_active,
                was_overlay_focused_in_tab,
                tab_zoomed_before_exit,
            );
        }
        if let Some((ws_idx, tab_idx)) = pane_exit_layout_target {
            self.emit_layout_updated_event(ws_idx, tab_idx);
        }

        self.sync_toast_deadline(previous_toast);
        self.shutdown_detached_terminal_runtimes();
        pane_updates.extend(worktree_restore_updates);
        pane_updates
    }

    fn reset_agent_detection_for_agents(&self, agents: &[crate::detect::Agent]) {
        if agents.is_empty() {
            return;
        }
        for (terminal_id, terminal) in &self.state.terminals {
            let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
                continue;
            };
            if !agents.contains(&agent) {
                continue;
            }
            if let Some(runtime) = self.terminal_runtimes.get(terminal_id) {
                runtime.reset_agent_detection();
            }
        }
    }

    fn reset_all_agent_detection_runtimes(&self) {
        for runtime in self.terminal_runtimes.values() {
            runtime.reset_agent_detection();
        }
    }

    pub(crate) fn refresh_new_herdr_toast_context_for_update(
        &mut self,
        update: &crate::app::actions::PaneStateUpdate,
        previous_toast: &Option<crate::app::state::ToastNotification>,
    ) {
        if !matches!(
            self.state.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        ) || self.state.toast == *previous_toast
        {
            return;
        }

        let Some(target) = self
            .state
            .toast
            .as_ref()
            .and_then(|toast| toast.target.as_ref())
        else {
            return;
        };
        if target.pane_id != update.pane_id {
            return;
        }
        let Some(ws) = self.state.workspaces.get(update.ws_idx) else {
            return;
        };
        if ws.id != target.workspace_id {
            return;
        }

        let workspace_label = ws.display_name_from(&self.state.terminals, &self.terminal_runtimes);
        let context = crate::app::actions::notification_context(
            ws,
            &workspace_label,
            update.ws_idx,
            update.pane_id,
        );
        if let Some(toast) = self.state.toast.as_mut() {
            toast.context = context;
        }
    }

    fn sync_full_lifecycle_authority_detection_pauses(&self) {
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                for pane in tab.panes.values() {
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    let Some(runtime) = self.terminal_runtimes.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    runtime.set_full_lifecycle_authority_active(
                        terminal.full_lifecycle_hook_authority_active(),
                    );
                    runtime.set_self_reported_agent_active(terminal.self_reported_agent_active());
                }
            }
        }
    }

    fn restore_overlay_after_exit(
        &mut self,
        overlay: OverlayPaneState,
        was_overlay_active: bool,
        was_overlay_focused_in_tab: bool,
        tab_zoomed_before_exit: Option<bool>,
    ) {
        for temp_file in &overlay.temp_files {
            let _ = std::fs::remove_file(temp_file);
        }

        let Some(ws) = self.state.workspaces.get_mut(overlay.ws_idx) else {
            return;
        };
        if overlay.tab_idx >= ws.tabs.len() {
            return;
        }

        if !was_overlay_focused_in_tab {
            if let Some(tab_zoomed_before_exit) = tab_zoomed_before_exit {
                ws.tabs[overlay.tab_idx].zoomed = tab_zoomed_before_exit;
            }
            return;
        }

        if was_overlay_active {
            ws.active_tab = overlay.tab_idx;
        }
        let tab = &mut ws.tabs[overlay.tab_idx];
        if tab.panes.contains_key(&overlay.previous_focus) {
            tab.layout.focus_pane(overlay.previous_focus);
        }
        tab.zoomed = overlay.previous_zoomed;

        if was_overlay_active && self.state.active == Some(overlay.ws_idx) {
            self.state.mode = Mode::Terminal;
        }
    }

    fn runtime_exit_action(&self, pane_id: crate::layout::PaneId) -> RuntimeExitAction {
        let Some((_, pane_state)) = self.find_pane(pane_id) else {
            return RuntimeExitAction::ClosePane;
        };
        let Some(terminal) = self.state.terminals.get(&pane_state.attached_terminal_id) else {
            return RuntimeExitAction::ClosePane;
        };

        if terminal.respawn_shell_on_exit || self.should_respawn_shell_after_agent_exit(terminal) {
            RuntimeExitAction::RespawnShell
        } else {
            RuntimeExitAction::ClosePane
        }
    }

    fn should_respawn_shell_after_agent_exit(
        &self,
        terminal: &crate::terminal::TerminalState,
    ) -> bool {
        #[cfg(not(windows))]
        {
            let _ = terminal;
            false
        }

        #[cfg(windows)]
        {
            if !terminal.agent_process_exited_within(
                Instant::now(),
                WINDOWS_POWERSHELL_AGENT_EXIT_RESPAWN_GRACE,
            ) {
                return false;
            }

            crate::pane::uses_windows_powershell_pane_shell(crate::pane::PaneShellConfig::new(
                &self.state.default_shell,
                self.state.shell_mode,
            ))
        }
    }

    fn respawn_shell_for_launch_pane(
        &mut self,
        pane_id: crate::layout::PaneId,
        focus_pane: bool,
    ) -> bool {
        let Some((ws_idx, pane_state)) = self.find_pane(pane_id) else {
            return false;
        };
        let terminal_id = pane_state.attached_terminal_id.clone();
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return false;
        };

        let cwd = terminal.cwd.clone();
        // Reuse any account config-home env armed for this pane so a bare-shell
        // fallback keeps the same account. Empty for an ordinary launch-pane
        // respawn, so that path stays byte-identical.
        let account_env = crate::config::AccountLaunchEnv::from_resolved_vars(
            terminal.pending_launch_env.clone(),
        );
        let (rows, cols) = self
            .terminal_runtimes
            .get(&terminal_id)
            .map(|runtime| runtime.current_size())
            .unwrap_or_else(|| {
                self.state
                    .new_pane_size(crate::ui::NewPanePlacement::Existing {
                        ws_idx,
                        pane: pane_id,
                    })
            });
        let Some(launch_env) = self.pane_launch_env(ws_idx, pane_id, account_env) else {
            return false;
        };
        let runtime = match crate::terminal::TerminalRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            &launch_env,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::warn!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    err = %err,
                    "failed to respawn shell after launch command exited"
                );
                return false;
            }
        };

        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.clear_agent_runtime_identity_after_respawn();
            terminal.pending_launch_env.clear();
        }
        if focus_pane {
            self.state.focus_pane_in_workspace(ws_idx, pane_id);
        }
        self.schedule_session_save();
        true
    }

    pub(crate) fn claim_worktree_runtime_restore_failure(
        &mut self,
        pane_id: crate::layout::PaneId,
        operation_id: u64,
    ) -> bool {
        if self.pending_worktree_remove_runtime_restores.get(&pane_id) != Some(&operation_id) {
            return false;
        }
        self.pending_worktree_remove_runtime_restores
            .remove(&pane_id);
        self.pending_worktree_remove_runtime_exits.remove(&pane_id);
        true
    }

    pub(crate) fn publish_worktree_runtime_agent_release(
        &mut self,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::app::actions::PaneStateUpdate> {
        self.state.pending_agent_notifications.remove(&pane_id);
        let previous_toast = self.state.toast.clone();
        let update = self
            .state
            .publish_pane_process_exit_if_agent(pane_id, true)?;
        self.sync_full_lifecycle_authority_detection_pauses();
        self.refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
        self.emit_pane_state_update(&update);
        Some(update)
    }

    fn queue_worktree_runtime_restore_failed(
        &self,
        pane_id: crate::layout::PaneId,
        operation_id: u64,
    ) {
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            let _ = event_tx
                .send(AppEvent::WorktreeRuntimeRestoreFailed {
                    pane_id,
                    operation_id,
                })
                .await;
        });
    }

    /// True when the pane's terminal holds a pending agent-resume plan — set by
    /// an `agent.restart` just before it killed the process, so the imminent
    /// `PaneDied` relaunches with `--resume` instead of a bare shell.
    fn pane_has_pending_agent_resume(&self, pane_id: crate::layout::PaneId) -> bool {
        self.find_pane(pane_id)
            .map(|(_, pane)| pane.attached_terminal_id.clone())
            .and_then(|terminal_id| self.state.terminals.get(&terminal_id))
            .is_some_and(|terminal| terminal.pending_agent_resume_plan.is_some())
    }

    /// Relaunch the pane's agent from its pending resume plan: the deferred
    /// launcher spawns a fresh shell in the same pane, types the harness's
    /// `--resume` command, and clears the plan. Returns true if a relaunch was
    /// started. The old runtime was already removed by `agent.restart`'s
    /// `shutdown_terminal_runtime`, so the launcher (which requires no live
    /// runtime) proceeds.
    pub(super) fn resume_pending_agent_for_pane(&mut self, pane_id: crate::layout::PaneId) -> bool {
        let Some((ws_idx, pane)) = self.find_pane(pane_id) else {
            return false;
        };
        let terminal_id = pane.attached_terminal_id.clone();
        let (rows, cols) = self
            .state
            .new_pane_size(crate::ui::NewPanePlacement::Existing {
                ws_idx,
                pane: pane_id,
            });
        // A restart is an explicit user action, so proceed even when the host
        // terminal theme has not been reported yet (`allow_empty_theme = true`) —
        // never strand a deliberate restart waiting for a theme that a headless
        // daemon may never receive. Residual failures degrade to a bare shell
        // (see the caller), so the pane still survives.
        self.start_pending_agent_resume_for_terminal(&terminal_id, rows, cols, true)
    }

    pub(crate) fn emit_pane_state_update(&mut self, update: &crate::app::actions::PaneStateUpdate) {
        let Some(pane_id) = self.public_pane_id(update.ws_idx, update.pane_id) else {
            return;
        };
        let workspace_id = self.public_workspace_id(update.ws_idx);

        if update.agent_name_changed {
            self.emit_pane_updated(update.ws_idx, update.pane_id);
        }

        if update.previous_agent_label != update.agent_label || update.agent_released {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentDetected,
                data: crate::api::schema::EventData::PaneAgentDetected {
                    pane_id: pane_id.clone(),
                    workspace_id: workspace_id.clone(),
                    agent: update.agent_label.clone(),
                    released: update.agent_released,
                    final_status: update.agent_release_status,
                },
            });
        }

        let previous_agent_status = pane_agent_status(update.previous_state, update.previous_seen);
        let agent_status = self
            .state
            .workspaces
            .get(update.ws_idx)
            .and_then(|ws| ws.pane_state(update.pane_id))
            .map(|pane| pane_agent_status(update.state, pane.seen))
            .unwrap_or_else(|| pane_agent_status(update.state, update.seen));

        if previous_agent_status != agent_status
            || update.previous_presentation != update.presentation
            || update.previous_input_pending != update.input_pending
            || update.previous_input_prompt_kind != update.input_prompt_kind
        {
            let presentation = update.presentation.clone();
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentStatusChanged,
                data: crate::api::schema::EventData::PaneAgentStatusChanged {
                    pane_id: pane_id.clone(),
                    workspace_id: workspace_id.clone(),
                    agent_status,
                    input_pending: update.input_pending,
                    input_prompt_kind: update.input_prompt_kind,
                    agent: update.agent_label.clone(),
                    title: presentation.title,
                    display_agent: presentation.display_agent,
                    state_labels: presentation.state_labels,
                    turn: update.turn,
                    turn_epoch: update.turn_epoch,
                },
            });
        }

        if let Some(completed) = &update.completed_turn {
            if let Some(pane) = self.pane_info(update.ws_idx, update.pane_id) {
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::PaneTurnCompleted,
                    data: crate::api::schema::EventData::PaneTurnCompleted {
                        pane,
                        turn: completed.turn,
                        turn_epoch: completed.turn_epoch,
                        outcome: completed.outcome,
                        message: completed.message.clone(),
                        message_truncated: completed.message_truncated,
                        agent_session_path: completed.agent_session_path.clone(),
                        completed_unix_ms: completed.completed_unix_ms,
                    },
                });
            }
        }
    }

    /// Deliver agent-state transitions to registered remote devices via APNs.
    ///
    /// A sibling of `emit_terminal_or_system_agent_notifications`, but with its
    /// own guard (`crate::push::may_deliver`), independent of `local_terminal_
    /// notifications` and `ToastDelivery`: a remote device wants push even when
    /// local toasts are Off/Herdr and even on the focused active tab (so
    /// active-tab suppression is intentionally not applied here). Delivery is
    /// detached and best-effort — it never blocks the app loop or holds a lock
    /// across the curl. `from_pane_death` maps every produced alert to the
    /// pane-died kind; otherwise the alert kind follows the toast kind.
    fn emit_apns_agent_notifications(
        &self,
        pane_updates: &[crate::app::actions::PaneStateUpdate],
        from_pane_death: bool,
    ) {
        if self.no_session || !crate::push::may_deliver(&self.state.push_config) {
            return;
        }

        let mut notifications = Vec::new();
        for update in pane_updates {
            // Pass `false`: remote push is not suppressed on the active tab. A
            // pane death is an event, not a state transition, so `push_kind_for`
            // yields a Died push even when the toast predicate returns None
            // (e.g. an already-idle pane whose process exits).
            let toast_kind =
                crate::app::actions::notification_toast_for_pane_state_update(false, update);
            let Some(push_kind) = push_kind_for(from_pane_death, toast_kind) else {
                continue;
            };
            let Some(ws) = self.state.workspaces.get(update.ws_idx) else {
                continue;
            };
            let Some(pane) = ws
                .tabs
                .iter()
                .find_map(|tab| tab.panes.get(&update.pane_id))
            else {
                continue;
            };
            let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id) else {
                continue;
            };
            // Only agent panes notify — an unrecognised terminal has no effective label.
            let Some(agent_label) = terminal.effective_agent_label() else {
                continue;
            };
            // Prefer the human agent name in the alert title ("jarvis needs attention");
            // fall back to the detected tool label ("claude") when the agent is unnamed.
            let agent_display = push_title_agent(terminal.agent_name.as_deref(), agent_label);
            let Some(public_pane_id) = self.public_pane_id(update.ws_idx, update.pane_id) else {
                continue;
            };
            let event_text = push_event_text(push_kind);
            let workspace_label =
                ws.display_name_from(&self.state.terminals, &self.terminal_runtimes);
            // Bound the alert text so the JSON payload stays well under Apple's 4096-byte
            // limit — workspace/tab names are user-controlled and unbounded, and an oversized
            // payload is a 413 that would be silently dropped. Reuse the local toast path's
            // sanitize+truncate (title 80, body 240); a title that sanitizes away is skipped.
            let Some(title) =
                sanitized_notification_text(&format!("{agent_display} {event_text}"), 80)
            else {
                continue;
            };
            let body = sanitized_notification_text(
                &crate::app::actions::notification_context(
                    ws,
                    &workspace_label,
                    update.ws_idx,
                    update.pane_id,
                ),
                240,
            )
            .unwrap_or_default();
            notifications.push(crate::push::PushNotification {
                title,
                body,
                pane_id: public_pane_id,
                workspace_id: self.public_workspace_id(update.ws_idx),
                kind: push_kind,
                #[cfg(unix)]
                guest_scope: Some(crate::guest::push::GuestScope::Agent {
                    terminal_id: pane.attached_terminal_id.to_string(),
                    name: terminal.agent_name.clone(),
                    kind: agent_label.to_string(),
                }),
                ask_source: push_ask_source(push_kind, agent_label, terminal, agent_display),
                ask: None,
            });
        }

        crate::push::dispatch(self.state.push_config.clone(), notifications);
    }

    /// Push the session's aggregate agent status to every registered Live Activity, so the
    /// lock-screen / Dynamic Island widget refreshes while the app is closed. Fired from the
    /// same sites as `emit_apns_agent_notifications` (i.e. whenever agent status changes)
    /// and after remote federation agents change (`sync_remote_agent_notifications`).
    /// Best-effort + detached; a no-op when push is off or no Live Activity is registered.
    fn emit_live_activity_updates(&self) {
        use std::sync::atomic::Ordering;

        if self.no_session || !crate::push::may_deliver(&self.state.push_config) {
            return;
        }

        // The aggregate is computed from in-memory state (no disk on the hot loop). The
        // activity-store read + the send happen off-loop in the spawned thread below; the
        // dedup + monotonic-timestamp bookkeeping here is all lock-free on this one thread.
        let mut agents = self.collect_agent_infos();
        agents.extend(self.reachable_remote_agents());
        let content_state = live_activity_content_state(&agents);

        // Dedup: skip when the aggregate is unchanged since the last dispatch. Status-change
        // events fire often without altering the widget's content (e.g. a git refresh), and
        // Live Activity push budgets are small, so unchanged states must not hit APNs.
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            content_state.to_string().hash(&mut hasher);
            hasher.finish()
        };
        if LAST_LIVE_ACTIVITY_HASH.load(Ordering::Relaxed) == hash {
            return;
        }

        // Monotonic, source-ordered timestamp (seconds), assigned HERE on the single app loop
        // rather than per-thread, so reordered sender threads can't let a stale snapshot
        // overwrite a newer one — APNs keeps the highest aps.timestamp.
        let now_secs = unix_millis_now() / 1000;
        let timestamp = {
            let previous = LAST_LIVE_ACTIVITY_TS.load(Ordering::Relaxed);
            let next = now_secs.max(previous + 1);
            LAST_LIVE_ACTIVITY_TS.store(next, Ordering::Relaxed);
            next
        };
        LAST_LIVE_ACTIVITY_HASH.store(hash, Ordering::Relaxed);

        crate::push::dispatch_live_activity(
            self.state.push_config.clone(),
            content_state,
            timestamp,
        );
    }
    pub(crate) fn sync_toast_deadline(
        &mut self,
        previous_toast: Option<crate::app::state::ToastNotification>,
    ) {
        if self.state.toast != previous_toast {
            self.toast_deadline = self.state.toast.as_ref().map(|toast| {
                let duration = match toast.kind {
                    ToastKind::NeedsAttention => Duration::from_secs(8),
                    ToastKind::Finished => Duration::from_secs(5),
                    ToastKind::UpdateInstalled => Duration::from_secs(3),
                };
                Instant::now() + duration
            });
        }
    }

    pub(crate) fn refresh_agent_notification_delivery_contexts(
        &mut self,
        deliveries: &mut [crate::app::state::AgentNotificationDelivery],
    ) {
        for delivery in deliveries {
            let Some(ws_idx) = self
                .state
                .workspaces
                .iter()
                .position(|ws| ws.id == delivery.workspace_id)
            else {
                continue;
            };
            let ws = &self.state.workspaces[ws_idx];
            let workspace_label =
                ws.display_name_from(&self.state.terminals, &self.terminal_runtimes);
            let context = crate::app::actions::notification_context(
                ws,
                &workspace_label,
                ws_idx,
                delivery.pane_id,
            );
            if let Some(toast) = delivery.toast.as_mut() {
                toast.context = context.clone();
            }
            if let Some(toast) = delivery.client_notification.as_mut() {
                toast.context = context.clone();
            }
            if let Some(toast) = self.state.toast.as_mut() {
                if toast.target.as_ref().is_some_and(|target| {
                    target.workspace_id == delivery.workspace_id
                        && target.pane_id == delivery.pane_id
                }) {
                    toast.context = context;
                }
            }
        }
    }

    pub(super) fn emit_event(&mut self, event: crate::api::schema::EventEnvelope) {
        self.run_plugin_event_hooks(&event);
        self.event_hub.push(event);
    }

    pub(crate) fn emit_pane_updated(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        if let Some(pane) = self.pane_info(ws_idx, pane_id) {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneUpdated,
                data: crate::api::schema::EventData::PaneUpdated { pane },
            });
        }
    }

    pub(crate) fn emit_workspace_token_updated(&mut self, ws_idx: usize) {
        // Token updates bypass plugin hooks so a hook cannot refresh its own
        // token and recursively trigger workspace.updated.
        self.event_hub.push(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::WorkspaceMetadataUpdated,
            data: crate::api::schema::EventData::WorkspaceMetadataUpdated {
                workspace: self.workspace_info(ws_idx),
            },
        });
    }

    pub(crate) fn sync_focus_events(&mut self) {
        self.sync_focus_events_with_outer_event(None);
    }

    pub(crate) fn accept_current_focus_without_events(&mut self) {
        self.last_focus = self.state.active.and_then(|idx| {
            self.state
                .workspaces
                .get(idx)
                .and_then(|workspace| workspace.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
    }

    pub(crate) fn accept_current_focus_with_api_events(&mut self) {
        let current_focus = self.state.active.and_then(|idx| {
            self.state
                .workspaces
                .get(idx)
                .and_then(|workspace| workspace.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
        if current_focus == self.last_focus {
            return;
        }
        self.last_focus = current_focus;
        if let Some((ws_idx, pane_id)) = current_focus {
            self.emit_focus_api_events(ws_idx, pane_id);
        }
    }

    pub(crate) fn emit_focus_api_events(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::WorkspaceFocused,
            data: crate::api::schema::EventData::WorkspaceFocused {
                workspace_id: self.public_workspace_id(ws_idx),
            },
        });
        if let Some(tab_id) = self.public_tab_id(ws_idx, self.state.workspaces[ws_idx].active_tab) {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::TabFocused,
                data: crate::api::schema::EventData::TabFocused {
                    tab_id,
                    workspace_id: self.public_workspace_id(ws_idx),
                },
            });
        }
        if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneFocused,
                data: crate::api::schema::EventData::PaneFocused {
                    pane_id: public_pane_id,
                    workspace_id: self.public_workspace_id(ws_idx),
                },
            });
        }
    }

    fn sync_focus_events_with_outer_event(
        &mut self,
        outer_event: Option<crate::ghostty::FocusEvent>,
    ) {
        let current_focus = self.state.active.and_then(|idx| {
            self.state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
        if current_focus == self.last_focus {
            if let (Some((ws_idx, pane_id)), Some(event)) = (current_focus, outer_event) {
                self.send_pane_focus_event(ws_idx, pane_id, event);
            }
            return;
        }

        if let Some((ws_idx, pane_id)) = self.last_focus {
            self.send_pane_focus_event(ws_idx, pane_id, crate::ghostty::FocusEvent::Lost);
        }
        if let Some((ws_idx, pane_id)) = current_focus {
            let event = outer_event.unwrap_or_else(|| {
                if self.state.outer_terminal_focus == Some(false) {
                    crate::ghostty::FocusEvent::Lost
                } else {
                    crate::ghostty::FocusEvent::Gained
                }
            });
            self.send_pane_focus_event(ws_idx, pane_id, event);
            self.emit_focus_api_events(ws_idx, pane_id);
        }

        self.last_focus = current_focus;
    }

    pub(crate) fn send_pane_focus_event(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        event: crate::ghostty::FocusEvent,
    ) {
        let Some(runtime) = self.state.workspaces.get(ws_idx).and_then(|_| {
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        }) else {
            return;
        };
        runtime.try_send_focus_event(event);
    }

    #[cfg(test)]
    pub(crate) fn handle_api_request(&mut self, request: crate::api::schema::Request) -> String {
        self.drain_all_internal_events();
        self.handle_api_request_after_internal_events_drained(request)
    }

    pub(crate) fn handle_api_request_after_internal_events_drained(
        &mut self,
        request: crate::api::schema::Request,
    ) -> String {
        self.sync_pending_terminal_titles();
        use crate::api::schema::{
            ErrorBody, ErrorResponse, Method, ResponseResult, SuccessResponse,
        };

        let response = match request.method {
            Method::ServerStop(_) => {
                self.state.should_quit = true;
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::ServerSshAgentRegister(_) => {
                return responses::encode_error(
                    request.id,
                    "connection_local_only",
                    "SSH agent registration requires a persistent local JSON API connection",
                );
            }
            Method::ServerLiveHandoff(_) => {
                let response = ErrorResponse {
                    id: request.id,
                    error: ErrorBody {
                        code: "unsupported_in_app_mode".into(),
                        message: "live handoff is only supported by the headless server".into(),
                    },
                };
                return serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
            }
            Method::ServerApplyStagedUpdate(_) => {
                let response = ErrorResponse {
                    id: request.id,
                    error: ErrorBody {
                        code: "unsupported_in_app_mode".into(),
                        message:
                            "applying a staged update is only supported by the headless server"
                                .into(),
                    },
                };
                return serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
            }
            Method::ServerReloadConfig(_) => {
                let report = self.reload_config();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::ConfigReload {
                        status: report.status,
                        diagnostics: report.diagnostics,
                    },
                }
            }
            Method::ServerStagedUpdate(_) => {
                // Report the running version/protocol plus the staged (built, not-yet-running)
                // build, if the fleet build step recorded one. A staged build with a different
                // version/sha means an update is ready to activate (server.apply_staged_update).
                //
                // A staged build of the COMMIT ALREADY RUNNING is not an update, and must not be
                // reported as one. The two shas are written by different producers at different
                // lengths - `staged.sha` is the short form the fleet build step records, while
                // `running_sha` is `build_info::commit()`, the full 40 characters - so a client
                // comparing them with `!=` sees a permanent phantom update. Observed live:
                // staged `5a244caa` against running `5a244caa60b0c3a5742315c59d20ed81c05bc23e`.
                // Deciding it HERE fixes every client, including ones already shipped, instead of
                // relying on each to normalise lengths it never agreed on.
                let running_sha = crate::build_info::commit();
                let staged = crate::persist::staged_build::load()
                    .filter(|staged| !same_commit(&staged.sha, running_sha))
                    .map(|staged| crate::api::schema::StagedBuildInfo {
                        version: staged.version,
                        sha: staged.sha,
                        built_at: staged.built_at,
                    });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::StagedUpdate {
                        running_version: crate::build_info::version(),
                        running_protocol: crate::protocol::PROTOCOL_VERSION,
                        running_sha: running_sha.map(str::to_string),
                        staged,
                    },
                }
            }
            Method::ServerAgentManifests(_) => {
                self.state.refresh_agent_manifest_summaries();
                let update_status = crate::detect::manifest_update::load_status();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::AgentManifestStatus {
                        last_check_unix: update_status.last_check_unix,
                        last_result: update_status.last_result.clone(),
                        manifests: self
                            .state
                            .agent_manifest_summaries
                            .clone()
                            .into_iter()
                            .map(|summary| agent_manifest_info(summary, &update_status))
                            .collect(),
                    },
                }
            }
            Method::ServerReloadAgentManifests(_) => {
                let summaries = crate::detect::manifest::reload_manifests();
                self.state.agent_manifest_summaries = summaries.clone();
                let update_status = crate::detect::manifest_update::load_status();
                self.reset_all_agent_detection_runtimes();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::AgentManifestReload {
                        manifests: summaries
                            .into_iter()
                            .map(|summary| agent_manifest_info(summary, &update_status))
                            .collect(),
                    },
                }
            }
            Method::NotificationShow(params) => {
                return self.handle_notification_show(request.id, params);
            }
            Method::ReleaseNotesDismiss(params) => {
                let Some(notes) = self.state.latest_release_notes.as_ref() else {
                    return responses::encode_error(
                        request.id,
                        "stale_release_notes",
                        "the release notes are no longer current",
                    );
                };
                if notes.version != params.version {
                    return responses::encode_error(
                        request.id,
                        "stale_release_notes",
                        "the release notes are no longer current",
                    );
                }
                let preview = notes.preview;
                self.mark_release_notes_seen(preview);
                return responses::encode_success(request.id, ResponseResult::Ok {});
            }
            Method::ProductAnnouncementDismiss(params) => {
                let matches_current =
                    self.state
                        .product_announcement
                        .as_ref()
                        .is_some_and(|announcement| {
                            announcement.version == params.version && announcement.id == params.id
                        });
                if !matches_current {
                    return responses::encode_error(
                        request.id,
                        "stale_announcement",
                        "the product announcement is no longer current",
                    );
                }
                self.dismiss_product_announcement();
                return responses::encode_success(request.id, ResponseResult::Ok {});
            }
            Method::CommandInvoke(params) => {
                return self.handle_command_invoke(request.id, params);
            }
            Method::NotificationsRegisterDevice(params) => {
                return self.handle_notifications_register_device(request.id, params);
            }
            Method::NotificationsUnregisterDevice(params) => {
                return self.handle_notifications_unregister_device(request.id, params);
            }
            Method::NotificationsRegisterActivity(params) => {
                return self.handle_notifications_register_activity(request.id, params);
            }
            Method::NotificationsUnregisterActivity(params) => {
                return self.handle_notifications_unregister_activity(request.id, params);
            }
            Method::NotificationsStatus(_) => return self.handle_notifications_status(request.id),
            Method::GramSend(params) => return self.handle_gram_send(request.id, params),
            Method::GramPost(params) => return self.handle_gram_post(request.id, params),
            Method::GramList(params) => return self.handle_gram_list(request.id, params),
            Method::GramGrab(params) => return self.handle_gram_grab(request.id, params),
            Method::GramMarkRead(params) => {
                return self.handle_gram_mark_read(request.id, params);
            }
            Method::GramDelete(params) => return self.handle_gram_delete(request.id, params),
            Method::GramUploadChunk(params) => {
                return self.handle_gram_upload_chunk(request.id, params);
            }
            Method::GramUploadDiscard(params) => {
                return self.handle_gram_upload_discard(request.id, params);
            }
            Method::GramGetFile(params) => return self.handle_gram_get_file(request.id, params),
            Method::GramGetFileChunk(params) => {
                return self.handle_gram_get_file_chunk(request.id, params);
            }
            Method::GramRelayStatus(_) => return self.handle_gram_relay_status(request.id),
            #[cfg(unix)]
            Method::GuestAgentProbe(params) => {
                return self.handle_guest_agent_probe(request.id, params);
            }
            Method::GramRelay(params) => {
                #[cfg(unix)]
                {
                    return self.handle_gram_relay(request.id, params);
                }
                #[cfg(not(unix))]
                {
                    let _ = params;
                    return responses::encode_error(
                        request.id,
                        "unsupported_platform",
                        "Gram reverse SSH requires Unix sockets",
                    );
                }
            }
            Method::ClientWindowTitleSet(_) | Method::ClientWindowTitleClear(_) => {
                return responses::encode_success(
                    request.id,
                    ResponseResult::ClientWindowTitle {
                        changed: false,
                        reason: crate::api::schema::ClientWindowTitleReason::NoForegroundClient,
                    },
                );
            }
            Method::SessionSnapshot(_) => return self.handle_session_snapshot(request.id),
            Method::WorkspaceList(params) => return self.handle_workspace_list(request.id, params),
            Method::WorkspaceGet(target) => return self.handle_workspace_get(request.id, target),
            Method::WorkspaceCreate(params) => {
                return self.handle_workspace_create(request.id, params);
            }
            Method::WorkspaceFocus(target) => {
                return self.handle_workspace_focus(request.id, target);
            }
            Method::WorkspaceRename(params) => {
                return self.handle_workspace_rename(request.id, params);
            }
            Method::WorkspaceMove(params) => {
                return self.handle_workspace_move(request.id, params);
            }
            Method::WorkspaceMoveBlock(params) => {
                return self.handle_workspace_move_block(request.id, params);
            }
            Method::WorkspaceReportMetadata(params) => {
                return self.handle_workspace_report_metadata(request.id, params);
            }
            Method::WorkspaceClose(target) => {
                return self.handle_workspace_close(request.id, target);
            }
            Method::WorktreeList(_) | Method::WorktreeOpen(_) => {
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "worktree discovery is handled asynchronously by the app runtime",
                );
            }
            Method::WorktreeCreate(params) => {
                let _ = params;
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "worktree.create is handled asynchronously by the app runtime",
                );
            }
            Method::WorktreeRemove(params) => {
                let _ = params;
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "worktree.remove is handled asynchronously by the app runtime",
                );
            }
            Method::TabList(params) => return self.handle_tab_list(request.id, params),
            Method::TabGet(target) => return self.handle_tab_get(request.id, target),
            Method::TabCreate(params) => return self.handle_tab_create(request.id, params),
            Method::TabFocus(target) => return self.handle_tab_focus(request.id, target),
            Method::TabRename(params) => return self.handle_tab_rename(request.id, params),
            Method::TabMove(params) => return self.handle_tab_move(request.id, params),
            Method::TabClose(target) => return self.handle_tab_close(request.id, target),
            Method::MachineStatus(_) => return self.handle_machine_status(request.id),
            Method::AgentList(params) => return self.handle_agent_list(request.id, params),
            Method::AgentGet(target) => return self.handle_agent_get(request.id, target),
            Method::AgentFocus(target) => return self.handle_agent_focus(request.id, target),
            Method::AgentRename(params) => return self.handle_agent_rename(request.id, params),
            Method::AgentArchive(params) => return self.handle_agent_archive(request.id, params),
            Method::AgentUnarchive(params) => {
                return self.handle_agent_unarchive(request.id, params)
            }
            Method::AgentForget(params) => return self.handle_agent_forget(request.id, params),
            Method::AgentViewSet(params) => return self.handle_agent_view_set(request.id, params),
            Method::AgentViewClear(params) => {
                return self.handle_agent_view_clear(request.id, params);
            }
            Method::AgentStart(params) => return self.handle_agent_start(request.id, params),
            Method::AgentRestart(params) => return self.handle_agent_restart(request.id, params),
            Method::AgentTransferSession(params) => {
                return self.handle_agent_transfer_session(request.id, params)
            }
            Method::AccountsList(_) => return self.handle_accounts_list(request.id),
            Method::AccountsCreate(params) => {
                return self.handle_accounts_create(request.id, params)
            }
            Method::AccountsRemove(params) => {
                return self.handle_accounts_remove(request.id, params)
            }
            Method::AgentKinds(_) => return self.handle_agent_kinds(request.id),
            Method::FsListDir(params) => return self.handle_fs_list_dir(request.id, params),
            Method::AgentPrompt(_) => {
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "agent.prompt is handled asynchronously by the app runtime",
                );
            }
            Method::AgentTranscript(_) => {
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "agent.transcript is handled asynchronously by the app runtime",
                );
            }
            Method::AgentWait(_) => {
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "agent.wait is handled by the api server",
                );
            }
            Method::AgentAnswer(_) => {
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "agent.answer is handled by the api server",
                );
            }
            Method::AgentRead(params) => return self.handle_agent_read(request.id, params),
            Method::AgentExplain(target) => return self.handle_agent_explain(request.id, target),
            Method::AgentSendKeys(params) => {
                return self.handle_agent_send_keys(request.id, params);
            }
            Method::PaneSplit(params) => return self.handle_pane_split(request.id, params),
            Method::PaneSwap(params) => return self.handle_pane_swap(request.id, params),
            Method::PaneMove(params) => return self.handle_pane_move(request.id, params),
            Method::PaneZoom(params) => return self.handle_pane_zoom(request.id, params),
            Method::PaneLayout(params) => return self.handle_pane_layout(request.id, params),
            Method::PaneProcessInfo(params) => {
                return self.handle_pane_process_info(request.id, params);
            }
            Method::LayoutExport(params) => return self.handle_layout_export(request.id, params),
            Method::LayoutApply(params) => return self.handle_layout_apply(request.id, params),
            Method::LayoutSetSplitRatio(params) => {
                return self.handle_layout_set_split_ratio(request.id, params);
            }
            Method::PaneNeighbor(params) => return self.handle_pane_neighbor(request.id, params),
            Method::PaneEdges(params) => return self.handle_pane_edges(request.id, params),
            Method::PaneFocusDirection(params) => {
                return self.handle_pane_focus_direction(request.id, params);
            }
            Method::PaneResize(params) => return self.handle_pane_resize(request.id, params),
            Method::PaneScroll(params) => return self.handle_pane_scroll(request.id, params),
            Method::PaneClear(target) => return self.handle_pane_clear(request.id, target),
            Method::PaneEditScrollback(target) => {
                return self.handle_pane_edit_scrollback(request.id, target);
            }
            Method::PaneSelectionRead(params) => {
                return self.handle_pane_selection_read(request.id, params);
            }
            Method::PaneCopyMotion(params) => {
                return self.handle_pane_copy_motion(request.id, params);
            }
            Method::PaneCopySearch(params) => {
                return self.handle_pane_copy_search(request.id, params);
            }
            Method::PaneSetPtySize(params) => {
                return self.handle_pane_set_pty_size(request.id, params);
            }
            Method::PaneList(params) => return self.handle_pane_list(request.id, params),
            Method::PaneCurrent(params) => return self.handle_pane_current(request.id, params),
            Method::PaneGet(target) => return self.handle_pane_get(request.id, target),
            Method::PaneTurns(params) => return self.handle_pane_turns(request.id, params),
            Method::PaneFocus(target) => return self.handle_pane_focus(request.id, target),
            Method::PaneInputSet(params) => return self.handle_pane_input_set(request.id, params),
            Method::PaneLinkResolve(params) => {
                return self.handle_pane_link_resolve(request.id, params);
            }
            Method::PaneLinkActivate(params) => {
                return self.handle_pane_link_activate(request.id, params);
            }
            Method::PaneRename(params) => return self.handle_pane_rename(request.id, params),
            Method::PaneRead(params) => return self.handle_pane_read(request.id, params),
            Method::PaneHistory(params) => return self.handle_pane_history(request.id, params),
            Method::PaneStream(_) => {
                return responses::encode_error(
                    request.id,
                    "stream_transport_required",
                    "pane.stream requires the streaming socket transport",
                );
            }
            Method::PaneStreamOpen(params) => {
                return self.handle_pane_stream_open(request.id, params);
            }
            Method::PaneStreamClose(params) => {
                return self.handle_pane_stream_close(request.id, params);
            }
            #[cfg(unix)]
            Method::PanePtyLeaseRelease(params) => {
                return self.handle_pane_pty_lease_release(request.id, params);
            }
            Method::PaneInputStream(_) => {
                return responses::encode_error(
                    request.id,
                    "stream_transport_required",
                    "pane.input.stream requires the streaming socket transport",
                );
            }
            Method::PaneInputStreamOpen(params) => {
                return self.handle_pane_input_stream_open(request.id, params);
            }
            Method::GramUploadStream(_) => {
                return responses::encode_error(
                    request.id,
                    "stream_transport_required",
                    "gram.upload.stream requires the streaming socket transport",
                );
            }
            Method::GramUploadStreamOpen(params) => {
                return self.handle_gram_upload_stream_open(request.id, params);
            }
            Method::PaneReportAgent(params) => {
                return self.handle_pane_report_agent(request.id, params);
            }
            Method::PaneReportAgentSession(params) => {
                return self.handle_pane_report_agent_session(request.id, params);
            }
            Method::PaneReportMetadata(params) => {
                return self.handle_pane_report_metadata(request.id, params);
            }
            Method::PaneClearAgentAuthority(params) => {
                return self.handle_pane_clear_agent_authority(request.id, params);
            }
            Method::PaneReleaseAgent(params) => {
                return self.handle_pane_release_agent(request.id, params);
            }
            Method::PaneSendText(params) => return self.handle_pane_send_text(request.id, params),
            Method::PaneSendInput(params) => {
                return self.handle_pane_send_input(request.id, params);
            }
            Method::PaneClose(target) => return self.handle_pane_close(request.id, target),
            Method::PopupClose(_) => {
                return if self.close_popup_pane() {
                    responses::encode_success(request.id, ResponseResult::Ok {})
                } else {
                    responses::encode_error(request.id, "popup_not_open", "no popup is open")
                };
            }
            Method::PaneSendKeys(params) => return self.handle_pane_send_keys(request.id, params),
            Method::IntegrationList(_) => {
                return self.handle_integration_list(request.id);
            }
            Method::IntegrationInstall(params) => {
                return self.handle_integration_install(request.id, params);
            }
            Method::IntegrationUninstall(params) => {
                return self.handle_integration_uninstall(request.id, params);
            }
            Method::PluginLink(params) => {
                return self.handle_plugin_link(request.id, params);
            }
            Method::PluginList(params) => {
                return self.handle_plugin_list(request.id, params);
            }
            Method::PluginUnlink(params) => {
                return self.handle_plugin_unlink(request.id, params);
            }
            Method::PluginEnable(params) => {
                return self.handle_plugin_enable(request.id, params);
            }
            Method::PluginDisable(params) => {
                return self.handle_plugin_disable(request.id, params);
            }
            Method::PluginActionList(params) => {
                return self.handle_plugin_action_list(request.id, params);
            }
            Method::PluginActionInvoke(params) => {
                return self.handle_plugin_action_invoke(request.id, params);
            }
            Method::PluginLogList(params) => {
                return self.handle_plugin_log_list(request.id, params);
            }
            Method::PluginPaneOpen(params) => {
                return self.handle_plugin_pane_open(request.id, params);
            }
            Method::PluginPaneFocus(params) => {
                return self.handle_plugin_pane_focus(request.id, params);
            }
            Method::PluginPaneClose(params) => {
                return self.handle_plugin_pane_close(request.id, params);
            }
            _ => {
                return responses::encode_error(
                    request.id,
                    "not_implemented",
                    "method not implemented yet",
                );
            }
        };

        serde_json::to_string(&response).unwrap()
    }

    /// `agent.kinds` — every known agent kind and whether its interactive
    /// harness binary is installed on the daemon's `$PATH`. Read-only; used by a
    /// client to offer only the harnesses that can actually launch.
    fn handle_agent_kinds(&self, id: String) -> String {
        use crate::api::schema::{AgentKindInfo, ResponseResult};

        let kinds = crate::detect::Agent::ALL
            .iter()
            .map(|&agent| AgentKindInfo {
                kind: crate::detect::agent_label(agent).to_string(),
                installed: executable_on_path(crate::detect::interactive_agent_executable(agent)),
            })
            .collect();
        responses::encode_success(id, ResponseResult::AgentKinds { kinds })
    }

    /// `fs.list_dir` — list a single directory's entries for an app folder
    /// picker. Read-only and non-recursive. `path` defaults to `$HOME` (or `/`
    /// when unset) and expands a leading `~`. A path that does not exist or is
    /// not a directory is a `not_a_directory` error.
    fn handle_fs_list_dir(
        &self,
        id: String,
        params: crate::api::schema::FsListDirParams,
    ) -> String {
        use crate::api::schema::{DirEntryInfo, ResponseResult};

        let resolved = resolve_list_dir_path(params.path.as_deref());

        let read_dir = match std::fs::read_dir(&resolved) {
            Ok(read_dir) => read_dir,
            Err(_) => {
                return responses::encode_error(
                    id,
                    "not_a_directory",
                    format!("{} is not a directory", resolved.display()),
                );
            }
        };

        let mut entries: Vec<DirEntryInfo> = Vec::new();
        for entry in read_dir.flatten() {
            // Skip an entry whose metadata can't be read (e.g. a broken symlink)
            // rather than failing the whole listing. `metadata` follows symlinks,
            // so a symlink to a directory reports `is_dir = true`.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            entries.push(DirEntryInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: metadata.is_dir(),
            });
        }

        // Directories first, then case-insensitive by name.
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        // Report the resolved absolute path, canonicalized best-effort.
        let path = std::fs::canonicalize(&resolved)
            .unwrap_or(resolved)
            .to_string_lossy()
            .into_owned();

        responses::encode_success(id, ResponseResult::DirList { path, entries })
    }

    fn handle_notification_show(
        &mut self,
        id: String,
        params: crate::api::schema::NotificationShowParams,
    ) -> String {
        use crate::api::schema::{NotificationShowReason, ResponseResult};

        let Some(title) = sanitized_notification_text(&params.title, 80) else {
            return responses::encode_error(id, "invalid_params", "notification title is empty");
        };
        let body = params
            .body
            .as_deref()
            .and_then(|body| sanitized_notification_text(body, 240));

        let reason = match self.state.toast_config.delivery {
            crate::config::ToastDelivery::Off => NotificationShowReason::Disabled,
            crate::config::ToastDelivery::Herdr => {
                if self.state.toast.is_some() {
                    NotificationShowReason::Busy
                } else if self.api_notification_rate_limited(Instant::now()) {
                    NotificationShowReason::RateLimited
                } else {
                    let previous_toast = self.state.toast.clone();
                    self.mark_api_notification_shown(Instant::now());
                    self.state.toast = Some(crate::app::state::ToastNotification {
                        kind: ToastKind::UpdateInstalled,
                        title,
                        context: body.unwrap_or_default(),
                        position: params.position,
                        target: None,
                    });
                    self.sync_toast_deadline(previous_toast);
                    NotificationShowReason::Shown
                }
            }
            crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System => {
                NotificationShowReason::NoForegroundClient
            }
        };

        responses::encode_success(
            id,
            ResponseResult::NotificationShow {
                shown: matches!(reason, NotificationShowReason::Shown),
                reason,
            },
        )
    }

    fn handle_notifications_register_device(
        &mut self,
        id: String,
        params: crate::api::schema::NotificationsRegisterDeviceParams,
    ) -> String {
        use crate::api::schema::ResponseResult;

        let device = match registered_device(params) {
            Ok(device) => device,
            Err(message) => return responses::encode_error(id, "invalid_params", message),
        };
        // No-session/monolithic mode has no shared device registry to persist
        // to; acknowledge without touching disk, mirroring the plugin handlers.
        if self.no_session {
            return responses::encode_success(id, ResponseResult::Ok {});
        }
        match crate::persist::devices::upsert(device) {
            Ok(_) => responses::encode_success(id, ResponseResult::Ok {}),
            Err(err) => responses::encode_error(id, "device_registry_save_failed", err.to_string()),
        }
    }

    fn handle_notifications_unregister_device(
        &mut self,
        id: String,
        params: crate::api::schema::NotificationsUnregisterDeviceParams,
    ) -> String {
        use crate::api::schema::ResponseResult;

        let token = params.device_token.trim();
        // Best-effort, like unregister_activity: an unknown token is fine.
        if self.no_session || token.is_empty() {
            return responses::encode_success(id, ResponseResult::Ok {});
        }
        match crate::persist::devices::remove_token(token) {
            Ok(_) => responses::encode_success(id, ResponseResult::Ok {}),
            Err(err) => responses::encode_error(id, "device_registry_save_failed", err.to_string()),
        }
    }

    fn handle_notifications_register_activity(
        &mut self,
        id: String,
        params: crate::api::schema::NotificationsRegisterActivityParams,
    ) -> String {
        use crate::api::schema::ResponseResult;

        let token = params.activity_push_token.trim();
        if !is_valid_apns_activity_token(token) {
            return responses::encode_error(
                id,
                "invalid_params",
                "activity_push_token must be 32-512 hexadecimal characters",
            );
        }
        let relay_capability = match normalize_relay_capability(params.relay_capability) {
            Ok(capability) => capability,
            Err(message) => return responses::encode_error(id, "invalid_params", message),
        };

        // No-session/monolithic mode has no shared registry to persist to; ack without disk,
        // mirroring handle_notifications_register_device.
        if self.no_session {
            return responses::encode_success(id, ResponseResult::Ok {});
        }

        let activity = crate::persist::activities::RegisteredActivity {
            activity_push_token: token.to_string(),
            registered_unix_ms: unix_millis_now(),
            relay_capability,
        };
        match crate::persist::activities::upsert(activity) {
            Ok(_) => {
                // Reset the dedup hash so the next status change is delivered to the
                // newly-registered activity even if the aggregate is unchanged since the
                // last send.
                LAST_LIVE_ACTIVITY_HASH.store(0, std::sync::atomic::Ordering::Relaxed);
                responses::encode_success(id, ResponseResult::Ok {})
            }
            Err(err) => {
                responses::encode_error(id, "activity_registry_save_failed", err.to_string())
            }
        }
    }

    fn handle_notifications_unregister_activity(
        &mut self,
        id: String,
        params: crate::api::schema::NotificationsRegisterActivityParams,
    ) -> String {
        use crate::api::schema::ResponseResult;

        let token = params.activity_push_token.trim();
        // Best-effort: nothing to prune in no-session mode or for an empty token.
        if self.no_session || token.is_empty() {
            return responses::encode_success(id, ResponseResult::Ok {});
        }
        match crate::persist::activities::remove_token(token) {
            Ok(_) => responses::encode_success(id, ResponseResult::Ok {}),
            Err(err) => {
                responses::encode_error(id, "activity_registry_save_failed", err.to_string())
            }
        }
    }

    /// Report remote push readiness for the app's settings screen. Counts only:
    /// never tokens, capabilities, or key material.
    fn handle_notifications_status(&self, id: String) -> String {
        let cfg = &self.state.push_config;
        // No-session mode keeps no device registry, so there is nothing to count.
        let (devices, relay_devices) = if self.no_session {
            (0, 0)
        } else {
            let devices = crate::persist::devices::load();
            let relay_devices = devices
                .iter()
                .filter(|device| device.relay_capability.is_some())
                .count();
            (devices.len(), relay_devices)
        };
        responses::encode_success(
            id,
            crate::api::schema::ResponseResult::NotificationsStatus {
                state: crate::push::status_state(self.no_session, cfg, relay_devices),
                mode: cfg.mode,
                relay_url: cfg.relay_url.clone(),
                devices: devices as u64,
                relay_devices: relay_devices as u64,
            },
        )
    }

    pub(crate) fn api_notification_rate_limited(&self, now: Instant) -> bool {
        self.last_api_notification_at
            .is_some_and(|last| now.duration_since(last) < API_NOTIFICATION_RATE_LIMIT)
    }

    pub(crate) fn mark_api_notification_shown(&mut self, now: Instant) {
        self.last_api_notification_at = Some(now);
    }
}

/// Is `staged_sha` an abbreviation of - or equal to - the running commit?
///
/// The two are written by different producers and are NOT the same length: the
/// fleet build step records a short sha into `staged-build.json`, the binary
/// embeds the full 40. So this is a prefix match, not equality.
///
/// Deliberately ONE-DIRECTIONAL: `staged` may abbreviate `running`, never the
/// reverse. A staged value LONGER than the running sha that merely starts with it
/// is a different (or malformed) identifier, and suppressing it would hide a real
/// update. Empty or absent shas never match either, so an unidentifiable staged
/// build is reported rather than swallowed.
///
/// Compares BYTES, not `str` slices: `staged` is attacker-adjacent JSON off disk
/// and slicing it at a `min(len)` offset panics when that offset lands inside a
/// multi-byte scalar (`"aé"` against `"ab"` did). Git shas are ASCII hex, so a
/// non-ASCII value simply fails to match.
fn same_commit(staged_sha: &str, running_sha: Option<&str>) -> bool {
    let Some(running) = running_sha else {
        return false;
    };
    let (staged, running) = (staged_sha.as_bytes(), running.as_bytes());
    if staged.is_empty() || running.is_empty() || staged.len() > running.len() {
        return false;
    }
    running[..staged.len()].eq_ignore_ascii_case(staged)
}

fn unix_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Validate a `notifications.register_device` call (owner or guest) into the
/// record to store. Errors are `invalid_params` messages.
pub(crate) fn registered_device(
    params: crate::api::schema::NotificationsRegisterDeviceParams,
) -> Result<crate::persist::devices::RegisteredDevice, &'static str> {
    let device_token = params.device_token.trim();
    if !is_valid_apns_device_token(device_token) {
        return Err("device_token must be 32-200 hexadecimal characters");
    }
    let platform = params.platform.trim();
    if platform.is_empty() {
        return Err("platform is empty");
    }
    let relay_capability = normalize_relay_capability(params.relay_capability)?;
    Ok(crate::persist::devices::RegisteredDevice {
        device_token: device_token.to_string(),
        platform: platform.to_string(),
        notify_needs_input: params.notify_needs_input,
        notify_dies: params.notify_dies,
        notify_finishes: params.notify_finishes,
        notify_gram: params.notify_gram,
        // The client owns the mute set and re-sends it whole on each change;
        // store a normalised copy (trimmed, blanks dropped, sorted,
        // de-duplicated, count-capped) so the record is deterministic and
        // truly bounded.
        muted_panes: sanitize_muted_panes(params.muted_panes),
        registered_unix_ms: unix_millis_now(),
        relay_capability,
    })
}

/// An APNs device token is lower/upper hex; bound the length so a garbage
/// registration cannot bloat the store.
fn is_valid_apns_device_token(token: &str) -> bool {
    (32..=200).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Trim a registration's relay capability; blank means none. A non-blank value
/// must be a sealed `hpr1.` capability.
fn normalize_relay_capability(capability: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(capability) = capability else {
        return Ok(None);
    };
    let capability = capability.trim();
    if capability.is_empty() {
        return Ok(None);
    }
    if !crate::push::is_valid_relay_capability(capability) {
        return Err(
            "relay_capability must be an hpr1. base64url capability of at most 512 characters",
        );
    }
    Ok(Some(capability.to_string()))
}

/// Live Activity push tokens are hex like device tokens but LONGER, so allow a wider range.
fn is_valid_apns_activity_token(token: &str) -> bool {
    (32..=512).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Source-ordered, strictly-increasing timestamp (Unix seconds) of the last Live Activity
/// update dispatched. Assigned on the single app loop so reordered sender threads keep order.
static LAST_LIVE_ACTIVITY_TS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Hash of the last content-state dispatched, so an unchanged aggregate is not re-sent.
static LAST_LIVE_ACTIVITY_HASH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Resolve an `fs.list_dir` request path to an absolute filesystem path. `None`
/// defaults to `$HOME` (or `/` when unset). A leading `~` (`~` alone or `~/…`)
/// expands against `$HOME`; any other value is used verbatim. Handled locally so
/// this stays independent of `worktree`'s `pub(crate)` tilde helper.
fn resolve_list_dir_path(path: Option<&str>) -> std::path::PathBuf {
    let home = || {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/"))
    };
    match path {
        None | Some("~") => home(),
        Some(raw) => match raw.strip_prefix("~/") {
            Some(rest) => home().join(rest),
            None => std::path::PathBuf::from(raw),
        },
    }
}

/// Whether `exe` names an executable file in any `$PATH` directory. Unix-focused:
/// on unix it requires an executable bit; on other targets it only checks that a
/// file by that name exists. Compile-safe on every target.
fn executable_on_path(exe: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if is_executable_file(&dir.join(exe)) {
            return true;
        }
    }
    false
}

#[cfg(unix)]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        // `metadata` follows symlinks, so a symlinked binary resolves to its
        // target. Require a regular file with at least one executable bit set.
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Aggregate a session's agents into the Live Activity content-state the widget renders.
/// Keys MUST match the app's `AgentActivityAttributes.State` (camelCase). The status +
/// priority mirror the iOS `AgentGroup` / `LiveActivityController` mapping so the background
/// (push) and foreground (in-app) computations agree: needs-you (blocked/unknown) outranks
/// working, which outranks idle; a dead pane simply drops off this live list.
fn live_activity_content_state(agents: &[crate::api::schema::AgentInfo]) -> serde_json::Value {
    use crate::api::schema::AgentStatus;

    fn rank(status: AgentStatus) -> u8 {
        match status {
            AgentStatus::Blocked => 0,
            AgentStatus::Unknown => 2,
            AgentStatus::Working => 3,
            AgentStatus::Idle | AgentStatus::Done => 4,
        }
    }
    fn status_str(status: AgentStatus) -> &'static str {
        match status {
            AgentStatus::Blocked | AgentStatus::Unknown => "needsYou",
            AgentStatus::Working => "working",
            AgentStatus::Idle | AgentStatus::Done => "idle",
        }
    }

    let needs_you_count = agents
        .iter()
        .filter(|agent| agent.agent_status == AgentStatus::Blocked)
        .count();
    let working_count = agents
        .iter()
        .filter(|agent| agent.agent_status == AgentStatus::Working)
        .count();

    let lead = agents.iter().min_by_key(|agent| rank(agent.agent_status));
    let (headline, status) = match lead {
        Some(agent) => (
            // A remote agent's name is stored "<machine-id>/<name>" for addressing; the
            // headline is read on a lock screen, so it shows the agent's own name.
            agent
                .name
                .as_deref()
                .map(|name| {
                    agent
                        .machine_id
                        .as_deref()
                        .and_then(|id| name.strip_prefix(id))
                        .and_then(|rest| rest.strip_prefix('/'))
                        .unwrap_or(name)
                        .to_string()
                })
                .or_else(|| agent.terminal_title_stripped.clone())
                .or_else(|| agent.agent.clone())
                .unwrap_or_else(|| "agent".to_string()),
            status_str(agent.agent_status),
        ),
        None => ("No agents".to_string(), "idle"),
    };

    // Only the working state carries a start time (Unix SECONDS) — the lead agent's
    // last completed-turn boundary ≈ its current turn's start. The iOS widget drives a
    // live `Text(_, style:.timer)` off it, the one thing a Live Activity actually
    // animates, so the working state shows real motion even on a locked phone. Null
    // otherwise. Key is camelCase to match the client's `ContentState` Codable.
    let working_since = if status == "working" {
        lead.and_then(|agent| agent.last_completed_turn.as_ref())
            .map(|turn| turn.completed_unix_ms as f64 / 1000.0)
    } else {
        None
    };

    serde_json::json!({
        "headline": headline,
        "status": status,
        "needsYouCount": needs_you_count,
        "workingCount": working_count,
        "totalCount": agents.len(),
        "workingSince": working_since,
    })
}

#[cfg(test)]
mod live_activity_content_state_tests {
    use super::live_activity_content_state;
    use crate::api::schema::AgentInfo;

    fn agent(status: &str, name: Option<&str>) -> AgentInfo {
        let mut value = serde_json::json!({
            "terminal_id": "t",
            "agent_status": status,
            "workspace_id": "ws",
            "tab_id": "tab",
            "pane_id": "pane",
            "focused": false,
            "revision": 1,
        });
        if let Some(name) = name {
            value["name"] = serde_json::Value::String(name.to_string());
        }
        serde_json::from_value(value).expect("agent info deserializes")
    }

    #[test]
    fn empty_session_reports_no_agents_idle() {
        let cs = live_activity_content_state(&[]);
        assert_eq!(cs["headline"], "No agents");
        assert_eq!(cs["status"], "idle");
        assert_eq!(cs["totalCount"], 0);
        assert_eq!(cs["needsYouCount"], 0);
        assert_eq!(cs["workingCount"], 0);
    }

    #[test]
    fn blocked_leads_and_counts_are_reported() {
        // Blocked (needs you) outranks working and idle regardless of list order.
        let agents = [
            agent("working", Some("builder")),
            agent("blocked", Some("reviewer")),
            agent("idle", Some("idler")),
        ];
        let cs = live_activity_content_state(&agents);
        assert_eq!(cs["headline"], "reviewer");
        assert_eq!(cs["status"], "needsYou");
        assert_eq!(cs["needsYouCount"], 1);
        assert_eq!(cs["workingCount"], 1);
        assert_eq!(cs["totalCount"], 3);
    }

    #[test]
    fn working_leads_when_nothing_needs_you() {
        let agents = [agent("idle", Some("a")), agent("working", Some("b"))];
        let cs = live_activity_content_state(&agents);
        assert_eq!(cs["headline"], "b");
        assert_eq!(cs["status"], "working");
    }

    #[test]
    fn working_since_is_turn_boundary_in_seconds_only_while_working() {
        fn working_with_turn(ms: u64) -> AgentInfo {
            let value = serde_json::json!({
                "terminal_id": "t", "agent_status": "working", "workspace_id": "ws",
                "tab_id": "tab", "pane_id": "pane", "focused": false, "revision": 1,
                "name": "b",
                "last_completed_turn": { "turn": 1, "turn_epoch": 1, "completed_unix_ms": ms },
            });
            serde_json::from_value(value).expect("agent info deserializes")
        }
        // Working lead with a completed-turn boundary → workingSince in SECONDS.
        let cs = live_activity_content_state(&[working_with_turn(1_700_000_000_000)]);
        assert_eq!(cs["status"], "working");
        assert_eq!(cs["workingSince"], 1_700_000_000.0);

        // Working but no completed turn yet → null (nothing to count from).
        let cs = live_activity_content_state(&[agent("working", Some("b"))]);
        assert!(cs["workingSince"].is_null());

        // Non-working lead → null regardless of any turn boundary.
        let cs = live_activity_content_state(&[agent("idle", Some("a"))]);
        assert!(cs["workingSince"].is_null());
    }
}

/// Map a pane update to the push kind to deliver, if any.
///
/// A process exit (`from_pane_death`) is an event, not a state transition, so it
/// always yields `Died` — even when the toast predicate returned `None` because
/// the pane was already Idle when its process exited. Otherwise the push kind
/// follows the toast kind; `UpdateInstalled` and `None` are not agent
/// transitions and produce no push.
fn push_kind_for(from_pane_death: bool, kind: Option<ToastKind>) -> Option<crate::push::PushKind> {
    if from_pane_death {
        return Some(crate::push::PushKind::Died);
    }
    match kind {
        Some(ToastKind::NeedsAttention) => Some(crate::push::PushKind::NeedsInput),
        Some(ToastKind::Finished) => Some(crate::push::PushKind::Finished),
        Some(ToastKind::UpdateInstalled) | None => None,
    }
}

/// The name to put in a push-notification title: the human agent name when the
/// user has set one, otherwise the detected tool label. So "jarvis needs
/// attention" for a named agent, falling back to "claude needs attention" when
/// it is unnamed. A blank or whitespace-only name is treated as unset.
fn push_title_agent<'a>(agent_name: Option<&'a str>, agent_label: &'a str) -> &'a str {
    agent_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(agent_label)
}

/// Where the push thread looks for a question this alert could carry
/// (herdr#309): only a NeedsInput alert about an omp agent of this daemon whose
/// session file herdr knows.
fn push_ask_source(
    kind: crate::push::PushKind,
    agent_label: &str,
    terminal: &crate::terminal::TerminalState,
    agent_display: &str,
) -> Option<crate::push::AskSource> {
    if kind != crate::push::PushKind::NeedsInput || agent_label != "omp" {
        return None;
    }
    let session = super::creation::terminal_agent_session_info(terminal)?;
    (session.kind == crate::agent_resume::AgentSessionRefKind::Path && !session.value.is_empty())
        .then(|| crate::push::AskSource {
            session_path: session.value.into(),
            agent: agent_display.to_owned(),
        })
}

/// The verb phrase after the agent in a push title ("jarvis needs attention").
fn push_event_text(kind: crate::push::PushKind) -> &'static str {
    match kind {
        crate::push::PushKind::NeedsInput => "needs attention",
        crate::push::PushKind::Finished => "finished",
        crate::push::PushKind::Died => "exited",
        // Gram alerts are built in `emit_apns_gram_message`, never from an agent
        // transition, so this arm only keeps the match exhaustive.
        crate::push::PushKind::Gram => "sent a message",
    }
}

/// The largest muted-pane set we persist per device. A defensive bound so a
/// malformed client cannot bloat `devices.json` — a real owner mutes a handful.
const MAX_MUTED_PANES: usize = 512;

/// Normalise a client-supplied muted-pane set for storage: TRIM each id (so a
/// padded id still matches `push::device_muted`'s exact-equality skip rather than
/// silently never matching), drop blanks, sort + de-duplicate, and cap the count.
fn sanitize_muted_panes(panes: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = panes
        .into_iter()
        .map(|pane| pane.trim().to_string())
        .filter(|pane| !pane.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_MUTED_PANES);
    out
}

fn agent_manifest_info(
    summary: crate::detect::manifest::AgentManifestSummary,
    update_status: &crate::detect::manifest_update::ManifestUpdateStatus,
) -> crate::api::schema::AgentManifestInfo {
    let remote = update_status.agent_status(summary.agent);
    crate::api::schema::AgentManifestInfo {
        agent: crate::detect::agent_label(summary.agent).to_string(),
        source: summary.active_source.label(),
        source_kind: summary.active_source.kind().to_string(),
        submission_verification_supported: summary.submission_verification_supported,
        active_version: summary.active_version,
        cached_remote_version: summary.cached_remote_version,
        local_override_shadowing_remote: summary.local_override_shadowing_remote,
        remote_update_result: remote.as_ref().map(|status| status.last_result.clone()),
        remote_update_error: remote.as_ref().and_then(|status| status.last_error.clone()),
        remote_last_checked_unix: remote.and_then(|status| status.last_checked_unix),
        warning: summary.warning,
    }
}

#[cfg(test)]
pub(super) mod test_support {
    pub(crate) fn exiting_test_command() -> &'static str {
        #[cfg(windows)]
        {
            "C:\\Windows\\System32\\whoami.exe"
        }
        #[cfg(not(windows))]
        {
            "/usr/bin/true"
        }
    }

    pub(crate) fn shutdown_test_runtimes(app: &mut crate::app::App) {
        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};

    #[test]
    fn push_title_agent_prefers_name_over_label() {
        // Named agent → the human name leads the title.
        assert_eq!(push_title_agent(Some("jarvis"), "claude"), "jarvis");
        // Unnamed → fall back to the detected tool label.
        assert_eq!(push_title_agent(None, "claude"), "claude");
        // A blank or whitespace-only name is not a name — fall back, and the
        // returned name is trimmed when it is used.
        assert_eq!(push_title_agent(Some(""), "codex"), "codex");
        assert_eq!(push_title_agent(Some("   "), "codex"), "codex");
        assert_eq!(push_title_agent(Some("  jarvis  "), "claude"), "jarvis");
    }

    #[test]
    fn sanitize_muted_panes_trims_dedups_drops_blanks_and_caps() {
        // A padded id is TRIMMED so it still matches device_muted's exact compare
        // (instead of being stored padded and silently never matching); blanks are
        // dropped and the set is de-duplicated.
        let out = sanitize_muted_panes(vec![
            " w1:p2 ".into(),
            "w1:p2".into(),
            "   ".into(),
            String::new(),
            "w1:p5".into(),
        ]);
        assert_eq!(out, vec!["w1:p2".to_string(), "w1:p5".to_string()]);

        // The count is capped, so a malformed client can't bloat devices.json.
        let many: Vec<String> = (0..(MAX_MUTED_PANES + 50))
            .map(|i| format!("w1:p{i}"))
            .collect();
        assert_eq!(sanitize_muted_panes(many).len(), MAX_MUTED_PANES);
    }

    #[test]
    fn push_kind_for_maps_events_and_transitions() {
        use crate::push::PushKind;

        // A process exit is an event: Died even when the toast predicate is None
        // (the idle-pane-exits case that would otherwise drop the notify_dies
        // push entirely).
        assert_eq!(push_kind_for(true, None), Some(PushKind::Died));
        assert_eq!(
            push_kind_for(true, Some(ToastKind::Finished)),
            Some(PushKind::Died)
        );

        // Otherwise the push kind follows the toast kind.
        assert_eq!(
            push_kind_for(false, Some(ToastKind::NeedsAttention)),
            Some(PushKind::NeedsInput)
        );
        assert_eq!(
            push_kind_for(false, Some(ToastKind::Finished)),
            Some(PushKind::Finished)
        );
        // UpdateInstalled and None are not agent transitions.
        assert_eq!(push_kind_for(false, Some(ToastKind::UpdateInstalled)), None);
        assert_eq!(push_kind_for(false, None), None);
    }

    #[test]
    fn apns_device_token_validation_bounds_and_hex() {
        let valid = "a".repeat(64);
        assert!(is_valid_apns_device_token(&valid));
        assert!(is_valid_apns_device_token(&"F0".repeat(16))); // upper hex, 32 chars
        assert!(!is_valid_apns_device_token("")); // empty
        assert!(!is_valid_apns_device_token(&"a".repeat(31))); // too short
        assert!(!is_valid_apns_device_token(&"a".repeat(201))); // too long
        assert!(!is_valid_apns_device_token(&"g".repeat(64))); // non-hex
        assert!(!is_valid_apns_device_token(&format!("{} ", "a".repeat(63)))); // whitespace
    }

    #[test]
    fn relay_capability_normalization() {
        assert_eq!(normalize_relay_capability(None), Ok(None));
        // Blank means "no capability", so a failed enrollment still registers.
        assert_eq!(normalize_relay_capability(Some("  ".into())), Ok(None));
        assert_eq!(
            normalize_relay_capability(Some(" hpr1.AbC-_ \n".into())),
            Ok(Some("hpr1.AbC-_".to_string()))
        );
        assert!(normalize_relay_capability(Some("hpr1.a+b".into())).is_err());
        assert!(normalize_relay_capability(Some("token".into())).is_err());
    }

    #[cfg(unix)]
    fn init_repo(path: &std::path::Path) {
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed for {}", path.display());
    }

    fn app_with_overlay(
        workspace: crate::workspace::Workspace,
        overlay_pane: crate::layout::PaneId,
        previous_focus: crate::layout::PaneId,
        previous_zoomed: bool,
    ) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.mode = Mode::Terminal;
        app.overlay_panes.insert(
            overlay_pane,
            OverlayPaneState {
                ws_idx: 0,
                tab_idx: 0,
                previous_focus,
                previous_zoomed,
                temp_files: Vec::new(),
            },
        );
        app
    }

    #[test]
    fn server_staged_update_reports_running_version_and_staged_without_leaking_path() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let tmp = std::env::temp_dir().join(format!(
            "herdr-staged-api-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let request = || crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::ServerStagedUpdate(
                crate::api::schema::EmptyParams {},
            ),
        };

        // No manifest staged → `staged` is null; the running version is reported.
        let v: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert_eq!(
            v["result"]["running_version"].as_str().unwrap(),
            crate::build_info::version()
        );
        // running_sha mirrors the binary's own commit: present in a git build, absent otherwise.
        match crate::build_info::commit() {
            Some(sha) => assert_eq!(v["result"]["running_sha"].as_str().unwrap(), sha),
            None => assert!(v["result"]["running_sha"].is_null()),
        }
        assert!(v["result"]["staged"].is_null());

        // Stage a manifest → it is reported, but the on-disk PATH is NOT exposed on the wire.
        std::fs::create_dir_all(crate::config::config_dir()).unwrap();
        std::fs::write(
            crate::config::config_dir().join("staged-build.json"),
            r#"{"version":"9.9.9","sha":"deadbee","built_at":"2026-08-23T15:00:00Z","path":"/root/.local/bin/herdr.staged"}"#,
        )
        .unwrap();
        let v2: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert_eq!(v2["result"]["staged"]["version"], "9.9.9");
        assert_eq!(v2["result"]["staged"]["sha"], "deadbee");
        assert_eq!(v2["result"]["staged"]["built_at"], "2026-08-23T15:00:00Z");
        assert!(
            v2["result"]["staged"]["path"].is_null(),
            "the staged binary path must not be exposed on the wire"
        );

        match prev_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn staged_build_of_the_running_commit_is_not_an_update() {
        // The two shas come from different producers at different lengths: the fleet
        // build step records a SHORT sha, the binary embeds the full 40. Observed live
        // on 2026-09-09: staged `5a244caa` vs running
        // `5a244caa60b0c3a5742315c59d20ed81c05bc23e`, which a client comparing with
        // `!=` shows as a permanent "update available" for a build already running.
        let Some(running) = crate::build_info::commit() else {
            // Not a git build: there is no running sha to abbreviate, and the
            // reporting path is exercised by the sibling test above.
            return;
        };

        let _guard = crate::config::test_config_env_lock().lock();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let tmp = std::env::temp_dir().join(format!(
            "herdr-staged-same-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
        std::fs::create_dir_all(crate::config::config_dir()).unwrap();

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let request = || crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::ServerStagedUpdate(
                crate::api::schema::EmptyParams {},
            ),
        };
        let stage = |sha: &str| {
            std::fs::write(
                crate::config::config_dir().join("staged-build.json"),
                format!(
                    r#"{{"version":"9.9.9","sha":"{sha}","built_at":"2026-09-09T06:11:26Z","path":"/x"}}"#
                ),
            )
            .unwrap();
        };

        // Abbreviated form of the RUNNING commit: nothing to update to. The embedded sha
        // is NOT always 40 characters - CI builds carry a 7-character short sha - so the
        // abbreviation is taken from whatever this binary actually has.
        let abbrev = &running[..running.len().min(8)];
        stage(abbrev);
        let v: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert!(
            v["result"]["staged"].is_null(),
            "a staged build of the running commit must not be reported as an update"
        );

        // Same, upper-cased: git shas are hex and case-insensitive.
        stage(&abbrev.to_ascii_uppercase());
        let v: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert!(
            v["result"]["staged"].is_null(),
            "sha match is case-insensitive"
        );

        // A DIFFERENT commit is still reported - the filter must not swallow real updates.
        stage("deadbee");
        let v: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert_eq!(v["result"]["staged"]["sha"], "deadbee");

        // A staged sha LONGER than the running one, sharing its prefix, is a different
        // (or malformed) identifier - suppressing it would hide a real update.
        stage(&format!("{running}0000"));
        let v: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert_eq!(
            v["result"]["staged"]["sha"],
            format!("{running}0000"),
            "a staged sha longer than the running commit is not an abbreviation of it"
        );

        // Non-ASCII cannot be a git sha. It must not match, and MUST NOT PANIC: slicing
        // `str` at a byte offset inside a multi-byte scalar aborts the request thread.
        stage("a\u{e9}");
        let v: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert_eq!(v["result"]["staged"]["version"], "9.9.9");

        // An empty sha is unidentifiable, so it is reported rather than hidden.
        stage("");
        let v: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request())).unwrap();
        assert_eq!(v["result"]["staged"]["version"], "9.9.9");

        match prev_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn fs_test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn fs_list_dir_lists_a_directory() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-fs-list-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("beta")).unwrap();
        std::fs::create_dir_all(dir.join("Alpha")).unwrap();
        std::fs::write(dir.join("zeta.txt"), b"x").unwrap();

        let mut app = fs_test_app();
        let request = crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::FsListDir(crate::api::schema::FsListDirParams {
                path: Some(dir.display().to_string()),
            }),
        };
        let value: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request)).unwrap();
        let entries = value["result"]["entries"].as_array().unwrap();

        // Directories first (case-insensitive: Alpha before beta), then the file.
        let names: Vec<&str> = entries
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Alpha", "beta", "zeta.txt"]);
        assert_eq!(entries[0]["is_dir"], true);
        assert_eq!(entries[1]["is_dir"], true);
        assert_eq!(entries[2]["is_dir"], false);
        // The reported path resolves back to the directory we listed.
        assert_eq!(
            std::fs::canonicalize(value["result"]["path"].as_str().unwrap()).unwrap(),
            std::fs::canonicalize(&dir).unwrap()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fs_list_dir_errors_on_a_non_directory() {
        let file = std::env::temp_dir().join(format!(
            "herdr-fs-notdir-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&file, b"not a dir").unwrap();

        let mut app = fs_test_app();
        let request = crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::FsListDir(crate::api::schema::FsListDirParams {
                path: Some(file.display().to_string()),
            }),
        };
        let value: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request)).unwrap();
        assert_eq!(value["error"]["code"], "not_a_directory");

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn agent_kinds_lists_all_known_kinds_with_installed_flags() {
        let mut app = fs_test_app();
        let request = crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::AgentKinds(crate::api::schema::EmptyParams {}),
        };
        let value: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request)).unwrap();
        let kinds = value["result"]["kinds"].as_array().unwrap();

        // One entry per known agent kind, each with a non-empty label and a bool
        // `installed` flag.
        assert_eq!(kinds.len(), crate::detect::Agent::ALL.len());
        for entry in kinds {
            assert!(!entry["kind"].as_str().unwrap().is_empty());
            assert!(entry["installed"].is_boolean());
        }
        // A representative known label is present.
        assert!(kinds
            .iter()
            .any(|entry| entry["kind"].as_str() == Some("claude")));
    }

    #[test]
    fn resolve_list_dir_path_defaults_and_expands_tilde() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/tester");

        // None -> $HOME.
        assert_eq!(
            resolve_list_dir_path(None),
            std::path::PathBuf::from("/home/tester")
        );
        // Bare "~" -> $HOME.
        assert_eq!(
            resolve_list_dir_path(Some("~")),
            std::path::PathBuf::from("/home/tester")
        );
        // "~/sub" -> $HOME/sub.
        assert_eq!(
            resolve_list_dir_path(Some("~/projects/x")),
            std::path::PathBuf::from("/home/tester/projects/x")
        );
        // An absolute path passes through verbatim.
        assert_eq!(
            resolve_list_dir_path(Some("/etc")),
            std::path::PathBuf::from("/etc")
        );

        match prev {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn executable_on_path_finds_present_and_rejects_missing() {
        // `sh` is guaranteed present and executable on any unix PATH.
        assert!(executable_on_path("sh"));
        // A name that cannot exist on PATH is not found.
        assert!(!executable_on_path("herdr-nonexistent-xyz"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn respawn_shell_caller_changes_turn_epoch() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("respawn")];
        app.state.ensure_test_terminals();
        app.state.default_shell = "/bin/sh".into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.terminal_id_for_pane(0, pane_id).unwrap();
        let previous_epoch = app.state.terminals[&terminal_id].turn_epoch;

        assert!(app.respawn_shell_for_launch_pane(pane_id, true));

        let respawn_epoch = app.state.terminals[&terminal_id].turn_epoch;
        assert_ne!(respawn_epoch, previous_epoch);
        assert_eq!(
            crate::terminal::state::turn_epoch_reset_path_for_test(respawn_epoch),
            crate::terminal::TurnCounterResetPath::PaneRespawn
        );
        if let Some(runtime) = app.terminal_runtimes.remove(&terminal_id) {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn manifest_activation_event_resets_matching_agent_detection_runtime() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("manifest-reset")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Codex);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let reset_notify = runtime.agent_detection_reset_notify_for_test();
        app.terminal_runtimes.insert(terminal_id, runtime);

        app.handle_internal_event(AppEvent::AgentDetectionManifestsUpdated {
            updated: Vec::new(),
            activated: vec![Agent::Codex],
            status: crate::detect::manifest_update::ManifestUpdateStatus::default(),
        });

        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            reset_notify.notified(),
        )
        .await
        .expect("matching agent detection runtime should be reset");
    }

    #[test]
    fn product_announcement_dismiss_requires_current_identity() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.product_announcement = Some(crate::app::state::ProductAnnouncementState {
            version: "0.8.2".into(),
            id: "client-shell".into(),
            title: "Client shell".into(),
            body: "announcement".into(),
            scroll: 0,
            preview: true,
        });

        let stale = app.handle_api_request(crate::api::schema::Request {
            id: "stale".into(),
            method: crate::api::schema::Method::ProductAnnouncementDismiss(
                crate::api::schema::ProductAnnouncementDismissParams {
                    version: "0.8.2".into(),
                    id: "old".into(),
                },
            ),
        });
        let stale: serde_json::Value = serde_json::from_str(&stale).unwrap();
        assert_eq!(stale["error"]["code"], "stale_announcement");
        assert!(app.state.product_announcement.is_some());

        let dismissed = app.handle_api_request(crate::api::schema::Request {
            id: "dismiss".into(),
            method: crate::api::schema::Method::ProductAnnouncementDismiss(
                crate::api::schema::ProductAnnouncementDismissParams {
                    version: "0.8.2".into(),
                    id: "client-shell".into(),
                },
            ),
        });
        let dismissed: serde_json::Value = serde_json::from_str(&dismissed).unwrap();
        assert_eq!(dismissed["result"]["type"], "ok");
        assert!(app.state.product_announcement.is_none());
    }

    #[test]
    fn release_notes_dismiss_requires_current_version() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.latest_release_notes = Some(crate::release_notes::ReleaseNotes {
            version: "99.99.99".into(),
            body: "release notes".into(),
            preview: true,
        });

        let stale = app.handle_api_request(crate::api::schema::Request {
            id: "stale".into(),
            method: crate::api::schema::Method::ReleaseNotesDismiss(
                crate::api::schema::ReleaseNotesDismissParams {
                    version: "0.8.2".into(),
                },
            ),
        });
        let stale: serde_json::Value = serde_json::from_str(&stale).unwrap();
        assert_eq!(stale["error"]["code"], "stale_release_notes");

        let dismissed = app.handle_api_request(crate::api::schema::Request {
            id: "dismiss".into(),
            method: crate::api::schema::Method::ReleaseNotesDismiss(
                crate::api::schema::ReleaseNotesDismissParams {
                    version: "99.99.99".into(),
                },
            ),
        });
        let dismissed: serde_json::Value = serde_json::from_str(&dismissed).unwrap();
        assert_eq!(dismissed["result"]["type"], "ok");
        assert!(app.state.latest_release_notes.is_some());
    }

    #[tokio::test]
    async fn server_reload_agent_manifests_resets_detection_runtimes() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("manifest-reload")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        #[cfg(unix)]
        let (runtime, _detection_events) = {
            let mut runtime = runtime;
            runtime.test_process_pty_bytes(b"shell prompt");
            let events = runtime.test_start_basic_detection();
            runtime.test_wait_for_detection_reads(1).await;
            (runtime, events)
        };
        #[cfg(not(unix))]
        let reset_notify = runtime.agent_detection_reset_notify_for_test();
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "reload_manifests".into(),
            method: crate::api::schema::Method::ServerReloadAgentManifests(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["result"]["type"], "agent_manifest_reload");
        assert!(!response["result"]["manifests"]
            .as_array()
            .unwrap()
            .is_empty());

        #[cfg(unix)]
        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_wait_for_detection_reads(2)
            .await;
        #[cfg(not(unix))]
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            reset_notify.notified(),
        )
        .await
        .expect("manual manifest reload should reset detection runtimes");
    }

    #[tokio::test]
    async fn server_agent_manifests_reports_status_without_resetting_runtimes() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("manifest-status")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let reset_notify = runtime.agent_detection_reset_notify_for_test();
        app.terminal_runtimes.insert(terminal_id, runtime);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "manifest_status".into(),
            method: crate::api::schema::Method::ServerAgentManifests(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["result"]["type"], "agent_manifest_status");
        assert!(!response["result"]["manifests"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                reset_notify.notified(),
            )
            .await
            .is_err(),
            "status request should not reset detection runtimes"
        );
    }

    #[tokio::test]
    async fn agent_explain_evaluates_with_server_manifest_cache() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let _env_guard = crate::config::test_config_env_lock().lock().unwrap();
        let old_config = std::env::var_os("XDG_CONFIG_HOME");
        let old_state = std::env::var_os("XDG_STATE_HOME");
        let manifest_home = std::env::temp_dir().join(format!(
            "herdr-agent-explain-manifests-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&manifest_home);
        std::env::set_var("XDG_CONFIG_HOME", manifest_home.join("config"));
        std::env::set_var("XDG_STATE_HOME", manifest_home.join("state"));
        crate::detect::manifest::reload_manifests();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("agent-explain")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Codex);
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(
            80,
            24,
            b"\xe2\x80\xba review changes\npress enter to confirm or esc to cancel",
        );
        app.terminal_runtimes.insert(terminal_id, runtime);
        let target = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "agent_explain".into(),
            method: crate::api::schema::Method::AgentExplain(crate::api::schema::AgentTarget {
                target,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        match old_config {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match old_state {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        crate::detect::manifest::reload_manifests();
        let _ = std::fs::remove_dir_all(&manifest_home);
        assert_eq!(response["result"]["type"], "agent_explain");
        assert_eq!(response["result"]["explain"]["state"], "blocked");
        assert_eq!(
            response["result"]["explain"]["matched_rule"]["id"],
            "live_strong_blocker"
        );
    }

    #[tokio::test]
    async fn agent_explain_rejects_hook_only_full_lifecycle_authority() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("agent-explain-omp")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority(
                "herdr:omp".to_string(),
                "omp".to_string(),
                AgentState::Working,
                None,
                Some(1),
            );
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        app.terminal_runtimes.insert(terminal_id, runtime);
        let target = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "agent_explain_omp".into(),
            method: crate::api::schema::Method::AgentExplain(crate::api::schema::AgentTarget {
                target,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["error"]["code"], "agent_not_found");
    }

    #[tokio::test]
    async fn pane_process_info_returns_response_for_existing_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("process-info")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id, runtime);
        let target = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "process_info".into(),
            method: crate::api::schema::Method::PaneProcessInfo(
                crate::api::schema::PaneProcessInfoParams {
                    pane_id: Some(target.clone()),
                },
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_process_info");
        assert_eq!(response["result"]["process_info"]["pane_id"], target);
    }

    #[test]
    fn client_window_title_api_reports_no_foreground_client_in_app_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let set = app.handle_api_request(crate::api::schema::Request {
            id: "title_set".into(),
            method: crate::api::schema::Method::ClientWindowTitleSet(
                crate::api::schema::ClientWindowTitleSetParams {
                    title: "plugin review".into(),
                },
            ),
        });
        let set: serde_json::Value = serde_json::from_str(&set).unwrap();
        assert_eq!(set["result"]["type"], "client_window_title");
        assert_eq!(set["result"]["changed"], false);
        assert_eq!(set["result"]["reason"], "no_foreground_client");

        let clear = app.handle_api_request(crate::api::schema::Request {
            id: "title_clear".into(),
            method: crate::api::schema::Method::ClientWindowTitleClear(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let clear: serde_json::Value = serde_json::from_str(&clear).unwrap();
        assert_eq!(clear["result"]["type"], "client_window_title");
        assert_eq!(clear["result"]["reason"], "no_foreground_client");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn herdr_toast_context_uses_live_root_runtime_cwd_label() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let mut workspace = crate::workspace::Workspace::test_new("stale");
        workspace.custom_name = None;
        let root = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(root).cloned().unwrap();
        let temp_root = std::env::temp_dir().join(format!(
            "herdr-toast-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let stale_cwd = temp_root.join("__herdr_original__");
        let live_cwd = temp_root.join("__herdr_projects__");
        std::fs::create_dir_all(&stale_cwd).unwrap();
        std::fs::create_dir_all(&live_cwd).unwrap();
        init_repo(&stale_cwd);
        init_repo(&live_cwd);

        workspace.identity_cwd = stale_cwd.clone();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = stale_cwd;
        app.state.active = None;
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast_config.delay_seconds = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            root,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        app.terminal_runtimes.insert(terminal_id, runtime);

        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("__herdr_projects__ · 1")
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delayed_herdr_toast_context_uses_live_root_runtime_cwd_label() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let mut workspace = crate::workspace::Workspace::test_new("stale");
        workspace.custom_name = None;
        let root = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(root).cloned().unwrap();
        let temp_root = std::env::temp_dir().join(format!(
            "herdr-delayed-toast-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let stale_cwd = temp_root.join("__herdr_original__");
        let live_cwd = temp_root.join("__herdr_projects__");
        std::fs::create_dir_all(&stale_cwd).unwrap();
        std::fs::create_dir_all(&live_cwd).unwrap();
        init_repo(&stale_cwd);
        init_repo(&live_cwd);

        workspace.identity_cwd = stale_cwd.clone();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = stale_cwd;
        app.state.active = None;
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast_config.delay_seconds = 1;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            root,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        app.terminal_runtimes.insert(terminal_id, runtime);

        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let notification_deadline = app
            .state
            .next_pending_agent_notification_deadline()
            .expect("pending notification deadline");
        let mut deliveries = app
            .state
            .drain_due_agent_notifications(notification_deadline);
        app.refresh_agent_notification_delivery_contexts(&mut deliveries);
        assert_eq!(
            deliveries
                .first()
                .and_then(|delivery| delivery.toast.as_ref())
                .map(|toast| toast.context.as_str()),
            Some("__herdr_projects__ · 1")
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn overlay_exit_preserves_focus_changed_before_exit() {
        let mut workspace = crate::workspace::Workspace::test_new("overlay");
        let previous_focus = workspace.tabs[0].root_pane;
        let overlay_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].zoomed = true;
        let new_tab = workspace.test_add_tab(Some("new"));
        workspace.switch_tab(new_tab);
        let mut app = app_with_overlay(workspace, overlay_pane, previous_focus, true);

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id: overlay_pane,
            runtime_epoch: None,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        let overlay_tab = &app.state.workspaces[0].tabs[0];
        assert_eq!(app.state.workspaces[0].active_tab, new_tab);
        assert_eq!(overlay_tab.layout.focused(), previous_focus);
        assert!(overlay_tab.zoomed);
        assert!(app.overlay_panes.is_empty());
    }

    #[test]
    fn pane_exit_emits_layout_updated_when_tab_survives() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("pane-exit-layout");
        let dead_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let tab_id = app.public_tab_id(0, 0).unwrap();

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id: dead_pane,
            runtime_epoch: None,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        let events = event_hub.events_after(0);
        let pane_exited = events
            .iter()
            .position(|(_, event)| event.event == crate::api::schema::EventKind::PaneExited)
            .expect("pane.exited should be emitted");
        let layout_updated = events
            .iter()
            .position(|(_, event)| event.event == crate::api::schema::EventKind::LayoutUpdated)
            .expect("layout.updated should be emitted");
        assert!(pane_exited < layout_updated);
        assert!(matches!(
            &events[layout_updated].1.data,
            crate::api::schema::EventData::LayoutUpdated { layout }
                if layout.tab_id == tab_id && layout.panes.len() == 1
        ));
    }

    #[test]
    fn idle_agent_exit_emits_release_event_without_a_state_change() {
        for agent_name in [None, Some("reviewer")] {
            let event_hub = crate::api::EventHub::default();
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                api_rx,
                event_hub.clone(),
            );
            let workspace = crate::workspace::Workspace::test_new("idle-agent-exit");
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
            app.state.workspaces = vec![workspace];
            app.state.ensure_test_terminals();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
            if let Some(agent_name) = agent_name {
                terminal.set_agent_name(agent_name.into());
            }

            app.handle_internal_event(AppEvent::StateChanged {
                runtime_epoch: None,
                pane_id,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                process_exited: true,
                observed_at: std::time::Instant::now(),
            });

            // The release event is this test's subject; the name outliving the
            // observation is pinned by
            // `a_process_exit_observation_alone_does_not_free_the_name`.
            assert_eq!(
                app.state.terminals[&terminal_id].agent_name.as_deref(),
                agent_name
            );
            assert!(event_hub.events_after(0).iter().any(|(_, event)| matches!(
                &event.data,
                crate::api::schema::EventData::PaneAgentDetected {
                    released: true,
                    final_status: Some(crate::api::schema::AgentStatus::Idle),
                    ..
                }
            )));
        }
    }

    #[test]
    fn process_exit_releases_a_newer_hook_owned_agent() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        let workspace = crate::workspace::Workspace::test_new("stale-agent-exit");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let observed_at = std::time::Instant::now();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal
            .set_hook_authority_at(
                "herdr:codex".into(),
                "codex".into(),
                AgentState::Working,
                None,
                None,
                Some(1),
                observed_at + std::time::Duration::from_secs(1),
            )
            .unwrap();
        terminal.set_agent_name("reviewer".into());

        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at,
        });

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.state, AgentState::Idle);
        // Releasing the registration does not free the name yet; a wrong
        // observation must not cost a live agent the handle its owner gave it.
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(event_hub.events_after(0).iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::PaneAgentDetected { released: true, .. }
        )));
    }

    #[test]
    fn overlay_exit_layout_updated_uses_restored_zoom_state() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("overlay-layout");
        let previous_focus = workspace.tabs[0].root_pane;
        let overlay_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(previous_focus);
        workspace.tabs[0].zoomed = true;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.mode = Mode::Terminal;
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.overlay_panes.insert(
            overlay_pane,
            OverlayPaneState {
                ws_idx: 0,
                tab_idx: 0,
                previous_focus,
                previous_zoomed: false,
                temp_files: Vec::new(),
            },
        );

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id: overlay_pane,
            runtime_epoch: None,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        let events = event_hub.events_after(0);
        let layout_updated = events
            .iter()
            .rposition(|(_, event)| event.event == crate::api::schema::EventKind::LayoutUpdated)
            .expect("layout.updated should be emitted");
        assert!(matches!(
            &events[layout_updated].1.data,
            crate::api::schema::EventData::LayoutUpdated { layout }
                if layout.tab_id == tab_id && layout.zoomed
        ));
    }

    #[test]
    fn overlay_exit_preserves_same_tab_focus_changed_before_exit() {
        let mut workspace = crate::workspace::Workspace::test_new("overlay");
        let previous_focus = workspace.tabs[0].root_pane;
        let overlay_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(previous_focus);
        workspace.tabs[0].zoomed = true;
        let mut app = app_with_overlay(workspace, overlay_pane, previous_focus, false);

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id: overlay_pane,
            runtime_epoch: None,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        let tab = &app.state.workspaces[0].tabs[0];
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(tab.layout.focused(), previous_focus);
        assert!(tab.zoomed);
        assert!(app.overlay_panes.is_empty());
    }

    #[test]
    fn overlay_exit_restores_previous_focus_when_overlay_still_focused() {
        let mut workspace = crate::workspace::Workspace::test_new("overlay");
        let previous_focus = workspace.tabs[0].root_pane;
        let overlay_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].zoomed = true;
        let mut app = app_with_overlay(workspace, overlay_pane, previous_focus, false);

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id: overlay_pane,
            runtime_epoch: None,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        let tab = &app.state.workspaces[0].tabs[0];
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(tab.layout.focused(), previous_focus);
        assert!(!tab.zoomed);
        assert!(app.overlay_panes.is_empty());
    }

    #[tokio::test]
    async fn pane_died_respawns_shell_and_clears_restored_agent_session() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("restored");
        app.state.default_shell = test_support::exiting_test_command().into();
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist");
        terminal.respawn_shell_on_exit = true;
        terminal.set_agent_name("codex".into());
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session")
                .expect("test session id should be valid"),
        });

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id,
            runtime_epoch: None,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        assert!(
            app.find_pane(pane_id).is_some(),
            "respawnable agent pane should stay attached after the agent process exits"
        );
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal should survive respawn");
        assert!(!terminal.respawn_shell_on_exit);
        assert!(terminal.persisted_agent_session.is_none());
        assert!(terminal.agent_name.is_none());

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn intentional_runtime_shutdown_claims_only_its_exact_exit_epoch() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("runtime-epoch");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let epoch = runtime.epoch();
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        app.shutdown_terminal_runtime(terminal_id);

        assert!(app.pane_runtime_epoch_matches(pane_id, Some(epoch)));
        assert!(!app.pane_runtime_epoch_matches(pane_id, Some(epoch.wrapping_add(1))));
        assert!(!app.pane_runtime_epoch_matches(
            crate::layout::PaneId::from_raw(pane_id.raw().wrapping_add(1000)),
            Some(epoch)
        ));
        app.consume_expected_pane_exit_epoch(pane_id, Some(epoch));
        assert!(!app.pane_runtime_epoch_matches(pane_id, Some(epoch)));
    }

    // Unix-only, and NOT because the assertions are unix-specific: this test leaves
    // `state.default_shell` UNSET, which is the whole point of the fixture — it is
    // reproducing a headless daemon with nothing configured. The resume plan names
    // `claude`, which is absent, so the PaneDied handler degrades to a bare shell,
    // and an unset default_shell resolves through `pane_shell_from` to
    // `powershell.exe` on Windows (the unix side falls back to $SHELL / /bin/sh).
    // The chain is:
    //   handle_internal_event(AppEvent::PaneDied)
    //     -> resume_pending_agent_for_pane / respawn_shell_for_launch_pane
    //     -> crate::terminal::TerminalRuntime::spawn
    //     -> crate::pane::PaneRuntime::spawn_command_builder
    //     -> crate::pty::backend::spawn_with_portable_pty  (cfg(windows) arm)
    //     -> native_pty_system().openpty() + slave.spawn_command(powershell.exe)
    // A ConPTY around an interactive PowerShell never exits on its own, and the
    // test blocks in that spawn for the whole run — 1339 s here, aborted by the job
    // timeout, exactly like the `agent.unarchive` family. Four of them saturate a
    // 4-core runner, which is what stalls the suite.
    //
    // It ran on Windows for the first time in this merge, because the fork's
    // windows_check.ps1 ran a filtered list and upstream's runs everything. Whether
    // the daemon's own restart path blocks the same way on Windows, or only this
    // in-process harness does, is UNVERIFIED — it needs a Windows host to answer,
    // and it is filed rather than assumed benign (#174).
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_restart_pane_survives_pane_died_with_empty_theme() {
        // Regression (failure atomicity): an `agent.restart` arms a resume plan
        // and kills the process. When the PaneDied fires with an EMPTY host
        // terminal theme (a headless daemon that never received an OSC 10/11
        // answer — the reachable strand JARVIS found), the resume must still
        // relaunch (allow_empty_theme) and, failing that, degrade to a bare
        // shell. Either way the PANE MUST SURVIVE — a restart never destroys it.
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("restart");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        // Host theme is unset by default — the exact strand condition.
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist");
        terminal.respawn_shell_on_exit = true;
        terminal.set_agent_name("claude".into());
        // Arm the restart's resume plan directly (as handle_agent_restart does).
        terminal.pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "claude".into(),
            argv: vec!["claude".into(), "--resume".into(), "sess-x".into()],
            dedupe_key: "claude:sess-x".into(),
        });

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id,
            runtime_epoch: None,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        assert!(
            app.find_pane(pane_id).is_some(),
            "an agent.restart must keep the pane alive after PaneDied, even with an empty theme"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_powershell_exit_after_agent_process_exit_respawns_shell() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("powershell");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.default_shell = "powershell.exe".into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;

        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id,
            agent: Some(crate::detect::Agent::OpenCode),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at: std::time::Instant::now(),
        });

        assert_eq!(
            app.runtime_exit_action(pane_id),
            RuntimeExitAction::RespawnShell
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_powershell_exit_without_recent_agent_process_exit_closes_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("powershell");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.default_shell = "powershell.exe".into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;

        assert_eq!(
            app.runtime_exit_action(pane_id),
            RuntimeExitAction::ClosePane
        );
    }

    #[test]
    fn stalled_worktree_runtime_exit_closes_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("worktree");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        #[cfg(windows)]
        {
            app.state.default_shell = "powershell.exe".into();
            app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Codex),
                AgentState::Idle,
                false,
                false,
                false,
                true,
                std::time::Instant::now(),
            );
            assert_eq!(
                app.runtime_exit_action(pane_id),
                RuntimeExitAction::RespawnShell
            );
        }
        app.pending_worktree_remove_runtime_exits.insert(pane_id, 1);
        app.pending_worktree_remove_runtime_restores
            .insert(pane_id, 8);

        app.handle_internal_event(AppEvent::WorktreeRuntimeRestoreFailed {
            pane_id,
            operation_id: 7,
        });
        assert_eq!(
            app.pending_worktree_remove_runtime_restores.get(&pane_id),
            Some(&8)
        );
        assert!(app.event_rx.try_recv().is_err());

        app.handle_internal_event(AppEvent::WorktreeRuntimeRestoreFailed {
            pane_id,
            operation_id: 8,
        });

        assert!(app.find_pane(pane_id).is_none());
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app.pending_worktree_remove_runtime_exits.is_empty());
        assert!(app.pending_worktree_remove_runtime_restores.is_empty());
    }

    #[test]
    fn terminal_delivery_does_not_refresh_existing_targeted_toast() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("stale");
        workspace.custom_name = None;
        workspace.identity_cwd = "/__herdr_original__".into();
        let root = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(root).cloned().unwrap();
        let workspace_id = workspace.id.clone();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = "/__herdr_projects__".into();
        app.state.active = None;
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Terminal;

        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: ToastKind::Finished,
            title: "codex finished".into(),
            context: "__herdr_original__ · 1".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id,
                pane_id: root,
            }),
        });

        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("__herdr_original__ · 1")
        );
    }

    #[test]
    fn input_only_change_emits_status_event_without_changing_lifecycle_sequence() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        let workspace = crate::workspace::Workspace::test_new("input");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.handle_internal_event(AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id,
            agent: Some(Agent::Kimi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: true,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-session")
            .expect("test session id should be valid");
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:kimi".to_string(),
                "kimi".to_string(),
                Some(session_ref.clone()),
                Some(1),
                Some("startup".to_string()),
            )
            .expect("session start should anchor full-lifecycle authority");
        terminal
            .set_hook_authority_with_session_ref(
                "herdr:kimi".to_string(),
                "kimi".to_string(),
                AgentState::Working,
                None,
                Some(session_ref),
                Some(2),
            )
            .expect("anchored hook report should be accepted");
        let baseline_sequence = app.state.terminals[&terminal_id].last_agent_state_change_seq;
        let baseline_event_sequence = event_hub.current_sequence();

        app.handle_internal_event(AppEvent::InputStateChanged {
            runtime_epoch: None,
            pane_id,
            kind: Some(crate::detect::InputPromptKind::Select),
        });

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.full_lifecycle_hook_authority_active());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.last_agent_state_change_seq, baseline_sequence);
        assert!(terminal.input_pending);
        assert_eq!(
            terminal.input_prompt_kind,
            Some(crate::detect::InputPromptKind::Select)
        );
        let pane = app.pane_info(0, pane_id).expect("pane info");
        assert!(pane.input_pending);
        assert_eq!(
            pane.input_prompt_kind,
            Some(crate::detect::InputPromptKind::Select)
        );
        let agent = app.agent_info(0, pane_id).expect("agent info");
        assert!(agent.input_pending);
        assert_eq!(
            agent.input_prompt_kind,
            Some(crate::detect::InputPromptKind::Select)
        );
        let events = event_hub.events_after(baseline_event_sequence);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].1.data,
            crate::api::schema::EventData::PaneAgentStatusChanged {
                agent_status: crate::api::schema::AgentStatus::Working,
                input_pending: true,
                input_prompt_kind: Some(crate::detect::InputPromptKind::Select),
                ..
            }
        ));

        app.handle_internal_event(AppEvent::InputStateChanged {
            runtime_epoch: None,
            pane_id,
            kind: None,
        });
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.full_lifecycle_hook_authority_active());
        assert_eq!(terminal.last_agent_state_change_seq, baseline_sequence);
        assert!(!terminal.input_pending);
        assert_eq!(terminal.input_prompt_kind, None);
    }
}
