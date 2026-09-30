use std::time::{Duration, Instant};

mod agent_view;
mod agents;
mod day;
mod env;
mod fleet;
mod groups;
mod integrations;
mod layouts;
mod loops;
mod pane_graphics;
mod panes;
pub(crate) mod plugins;
mod responses;
mod session;
mod symphony;
mod tabs;
mod workspaces;
mod worktrees;

pub(crate) use panes::PaneSendError;

#[cfg(test)]
use super::Mode;
use super::{api_helpers::pane_agent_status_with_stale, App, OverlayPaneState, ToastKind};
use crate::events::AppEvent;

const API_NOTIFICATION_RATE_LIMIT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const WINDOWS_POWERSHELL_AGENT_EXIT_RESPAWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeExitAction {
    RespawnShell,
    ClosePane,
}

impl App {
    pub(crate) fn refresh_remote_agent_panel_entries(&mut self) {
        self.state.remote_agent_panel_entries =
            crate::ui::remote_agent_panel_entries(&self.state.fleet_snapshot, self.state.nerd_font);
        self.state.aloop_projection =
            crate::aloop::project(&self.state.fleet_snapshot).map(std::sync::Arc::new);
        if self
            .state
            .sidebar_selected_remote_agent
            .as_ref()
            .is_some_and(|selected| {
                !self
                    .state
                    .remote_agent_panel_entries
                    .iter()
                    .any(|entry| &entry.agent_ref == selected)
            })
        {
            self.state.sidebar_selected_remote_agent = None;
        }
    }

    fn install_fleet_snapshot(&mut self, snapshot: crate::fleet::Snapshot) -> bool {
        self.install_fleet_snapshot_against(snapshot, None)
    }

    #[cfg(test)]
    pub(crate) fn install_fleet_snapshot_for_test(
        &mut self,
        snapshot: crate::fleet::Snapshot,
    ) -> bool {
        self.install_fleet_snapshot(snapshot)
    }

    fn install_fleet_snapshot_against(
        &mut self,
        mut snapshot: crate::fleet::Snapshot,
        admission_base_override: Option<&crate::fleet::AuthorityAcceptanceLedger>,
    ) -> bool {
        if snapshot.config_generation != self.fleet_poller_config.generation() {
            return false;
        }
        snapshot.preserve_live_agent_inventory_from(&self.state.fleet_snapshot, |host| {
            self.fleet_poller_config.agent_stream_is_live(host)
        });
        snapshot.sanitize_group_catalog_memberships();
        let raw_snapshot = snapshot.clone();
        snapshot.retain_unreachable_inventory_from(&self.state.fleet_snapshot);
        snapshot.retain_unavailable_group_catalogs_from(&self.state.fleet_snapshot);
        let admission_base = admission_base_override
            .or(self.queued_authority_acceptance_ledger.as_ref())
            .or(self.pending_authority_acceptance_ledger.as_ref())
            .unwrap_or(&self.authority_acceptance_ledger);
        let candidate_ledger =
            if let Some(error) = self.authority_acceptance_ledger_error.as_deref() {
                snapshot.reject_group_catalogs_without_durable_history(error);
                admission_base.clone()
            } else {
                snapshot.admit_group_catalogs(admission_base)
            };
        self.authority_mutation_router.observe_snapshot(&snapshot);
        if self.authority_acceptance_ledger_write_in_flight {
            self.queued_authority_acceptance_ledger = Some(candidate_ledger);
            self.queued_fleet_snapshot = Some(raw_snapshot);
            return false;
        }
        if candidate_ledger != self.authority_acceptance_ledger {
            if let Some(path) = self.authority_acceptance_ledger_path.as_deref() {
                match self.authority_acceptance_ledger_writer.enqueue(
                    path.to_path_buf(),
                    candidate_ledger.clone(),
                    snapshot.clone(),
                ) {
                    Ok(()) => {
                        self.authority_acceptance_ledger_write_in_flight = true;
                        self.pending_authority_acceptance_ledger = Some(candidate_ledger);
                        return false;
                    }
                    Err(error) => {
                        tracing::warn!(%error, path = %path.display(), "cannot queue authority acceptance ledger write");
                        snapshot.quarantine_unpersisted_advances(
                            &self.authority_acceptance_ledger,
                            &format!("durable ledger persistence failed: {error}"),
                        );
                        self.authority_mutation_router.observe_snapshot(&snapshot);
                    }
                }
            } else {
                self.authority_acceptance_ledger = candidate_ledger;
            }
        } else if let Some(path) = self.authority_acceptance_ledger_path.as_deref() {
            match self
                .authority_acceptance_ledger_writer
                .enqueue_reconciliation(
                    path.to_path_buf(),
                    candidate_ledger.clone(),
                    snapshot.clone(),
                ) {
                Ok(()) => {
                    self.authority_acceptance_ledger_write_in_flight = true;
                    self.pending_authority_acceptance_ledger = Some(candidate_ledger);
                    return false;
                }
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "cannot queue durable authority acceptance ledger reconciliation");
                    snapshot.reject_group_catalogs_without_durable_history(&format!(
                        "durable authority acceptance ledger reconciliation failed: {error}"
                    ));
                }
            }
        }
        self.commit_fleet_snapshot(snapshot)
    }

    fn finish_authority_acceptance_ledger_write(
        &mut self,
        ledger: crate::fleet::AuthorityAcceptanceLedger,
        mut snapshot: crate::fleet::Snapshot,
        result: Result<(), String>,
    ) -> bool {
        self.authority_acceptance_ledger_write_in_flight = false;
        self.pending_authority_acceptance_ledger = None;
        let mut changed = false;
        match result {
            Ok(()) => {
                snapshot.admit_group_catalogs(&ledger);
                self.authority_acceptance_ledger = ledger;
                if snapshot.config_generation == self.fleet_poller_config.generation() {
                    changed = self.commit_fleet_snapshot(snapshot);
                }
            }
            Err(error) => {
                if let Some(path) = self.authority_acceptance_ledger_path.as_deref() {
                    tracing::warn!(%error, path = %path.display(), "cannot persist authority acceptance ledger");
                } else {
                    tracing::warn!(%error, "cannot persist authority acceptance ledger");
                }
                if snapshot.config_generation == self.fleet_poller_config.generation() {
                    snapshot.quarantine_unpersisted_advances(
                        &self.authority_acceptance_ledger,
                        &format!("durable ledger persistence failed: {error}"),
                    );
                    self.authority_mutation_router.observe_snapshot(&snapshot);
                    changed = self.commit_fleet_snapshot(snapshot);
                }
            }
        }
        let queued_ledger = self.queued_authority_acceptance_ledger.take();
        if let Some(queued) = self.queued_fleet_snapshot.take() {
            if queued.config_generation == self.fleet_poller_config.generation() {
                changed |= self.install_fleet_snapshot_against(queued, queued_ledger.as_ref());
            } else if let Some(queued_ledger) = queued_ledger {
                self.persist_accepted_ledger_without_presentation(queued_ledger, queued);
            }
        }
        changed
    }

    fn finish_authority_acceptance_ledger_reconciliation(
        &mut self,
        ledger: crate::fleet::AuthorityAcceptanceLedger,
        mut snapshot: crate::fleet::Snapshot,
        result: Result<(), String>,
    ) -> bool {
        self.authority_acceptance_ledger_write_in_flight = false;
        self.pending_authority_acceptance_ledger = None;
        let mut changed = false;
        match result {
            Ok(()) => {
                snapshot.admit_group_catalogs(&ledger);
                self.authority_acceptance_ledger = ledger;
                if snapshot.config_generation == self.fleet_poller_config.generation() {
                    changed = self.commit_fleet_snapshot(snapshot);
                }
            }
            Err(error) => {
                if let Some(path) = self.authority_acceptance_ledger_path.as_deref() {
                    tracing::warn!(%error, path = %path.display(), "cannot reconcile durable authority acceptance ledger");
                } else {
                    tracing::warn!(%error, "cannot reconcile durable authority acceptance ledger");
                }
                if snapshot.config_generation == self.fleet_poller_config.generation() {
                    snapshot.reject_group_catalogs_without_durable_history(&format!(
                        "durable authority acceptance ledger reconciliation failed: {error}"
                    ));
                    self.authority_mutation_router.observe_snapshot(&snapshot);
                    changed = self.commit_fleet_snapshot(snapshot);
                }
            }
        }
        let queued_ledger = self.queued_authority_acceptance_ledger.take();
        if let Some(queued) = self.queued_fleet_snapshot.take() {
            if queued.config_generation == self.fleet_poller_config.generation() {
                changed |= self.install_fleet_snapshot_against(queued, queued_ledger.as_ref());
            } else if let Some(queued_ledger) = queued_ledger {
                self.persist_accepted_ledger_without_presentation(queued_ledger, queued);
            }
        }
        changed
    }

    fn persist_accepted_ledger_without_presentation(
        &mut self,
        ledger: crate::fleet::AuthorityAcceptanceLedger,
        snapshot: crate::fleet::Snapshot,
    ) {
        if ledger == self.authority_acceptance_ledger {
            return;
        }
        let Some(path) = self.authority_acceptance_ledger_path.as_deref() else {
            self.authority_acceptance_ledger = ledger;
            return;
        };
        match self.authority_acceptance_ledger_writer.enqueue(
            path.to_path_buf(),
            ledger.clone(),
            snapshot,
        ) {
            Ok(()) => {
                self.authority_acceptance_ledger_write_in_flight = true;
                self.pending_authority_acceptance_ledger = Some(ledger);
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot queue authority acceptance ledger write");
            }
        }
    }

    fn commit_fleet_snapshot(&mut self, snapshot: crate::fleet::Snapshot) -> bool {
        self.authority_mutation_router.observe_snapshot(&snapshot);
        self.remote_focus_transport
            .observe_fleet_snapshot(&snapshot);
        let catalogs_changed = self.state.fleet_snapshot.group_catalogs != snapshot.group_catalogs;
        let changed = self.state.fleet_snapshot != snapshot;
        self.state.fleet_snapshot = snapshot;
        if catalogs_changed {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::AuthorityCatalogsUpdated,
                data: crate::api::schema::EventData::AuthorityCatalogsUpdated {
                    catalogs: self.authority_catalog_infos(),
                },
            });
        }
        self.state.reconcile_dock_hosts_selection();
        self.refresh_remote_agent_panel_entries();
        changed
    }

    fn install_fleet_agent_inventory(
        &mut self,
        host: crate::config::FleetHostConfig,
        config_generation: u64,
        agents: Vec<crate::api::schema::AgentInfo>,
    ) -> bool {
        if config_generation != self.fleet_poller_config.generation() {
            return false;
        }
        let mut snapshot = self.state.fleet_snapshot.clone();
        if !snapshot.apply_agent_inventory_update(&host, agents, config_generation) {
            return false;
        }
        self.commit_fleet_snapshot(snapshot)
    }

    pub(crate) fn pane_runtime_is_suspended(&self, pane_id: crate::layout::PaneId) -> bool {
        self.find_pane(pane_id)
            .and_then(|(_, pane)| self.terminal_runtimes.get(&pane.attached_terminal_id))
            .is_some_and(crate::terminal::TerminalRuntime::is_suspended)
    }

    pub(crate) fn dispatch_api_request(
        &mut self,
        id: &'static str,
        method: crate::api::schema::Method,
    ) -> String {
        self.handle_api_request(crate::api::schema::Request {
            id: id.to_string(),
            method,
        })
    }

    pub(crate) fn dispatch_deferred_api_request(
        &mut self,
        id: &'static str,
        method: crate::api::schema::Method,
    ) -> Option<String> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        if !self.handle_deferred_worktree_api_request(
            crate::api::schema::Request {
                id: id.to_string(),
                method,
            },
            respond_to,
        ) {
            return None;
        }

        response_rx.try_recv().ok()
    }

    fn finish_remote_api_request(
        &mut self,
        agent_ref: crate::api::schema::AgentRef,
        response: String,
    ) -> bool {
        if let Ok(error) = serde_json::from_str::<crate::api::schema::ErrorResponse>(&response) {
            self.show_remote_pane_lifecycle_error(
                &agent_ref,
                format!(
                    "owner {}: {}: {}",
                    agent_ref.host, error.error.code, error.error.message
                ),
            );
            return true;
        }
        if serde_json::from_str::<serde_json::Value>(&response)
            .is_ok_and(|value| value.get("result").is_some())
        {
            return false;
        }
        self.show_remote_pane_lifecycle_error(
            &agent_ref,
            format!("owner {} returned an invalid response", agent_ref.host),
        );
        true
    }

    pub(crate) fn handle_internal_event_with_render_impact(&mut self, ev: AppEvent) -> bool {
        match ev {
            AppEvent::FleetRefreshed { snapshot } => {
                let changed = self.install_fleet_snapshot(snapshot);
                self.refresh_board_remote_lines();
                changed
            }
            AppEvent::BoardRemoteLinesFetched {
                note_path,
                fleet_generation,
                request_id,
                complete,
                lines,
            } => self.apply_board_remote_lines(
                &note_path,
                fleet_generation,
                request_id,
                complete,
                lines,
            ),
            AppEvent::FleetAgentInventoryChanged {
                host,
                config_generation,
                agents,
            } => self.install_fleet_agent_inventory(host, config_generation, agents),
            AppEvent::RemoteApiRequestFinished {
                agent_ref,
                response,
            } => self.finish_remote_api_request(agent_ref, response),
            AppEvent::AuthorityAcceptanceLedgerPersisted {
                ledger,
                snapshot,
                result,
            } => self.finish_authority_acceptance_ledger_write(ledger, *snapshot, result),
            AppEvent::AuthorityAcceptanceLedgerReconciled {
                ledger,
                snapshot,
                result,
            } => self.finish_authority_acceptance_ledger_reconciliation(ledger, *snapshot, result),
            AppEvent::SymphonyWorkflowsRefreshed { snapshot } => {
                self.refresh_symphony_snapshot(snapshot)
            }
            AppEvent::ScratchpadChanged => self.reload_scratchpad(),
            AppEvent::NotepadChanged => self.reload_notepad(),
            AppEvent::GoalsRefreshed {
                generation,
                refresh,
            } => self.finish_goals_refresh(generation, refresh),
            AppEvent::LoopRunHistoryChanged => self.refresh_loop_run_history(),
            AppEvent::StatusMetricsRefreshed { snapshot } => {
                let should_repaint = self
                    .status_metric_refresh
                    .finish_and_should_repaint(snapshot.as_ref().map(|value| value.sampled_at));
                if let Some(snapshot) = snapshot {
                    self.state.status_disk_visible =
                        crate::platform::status_metrics::disk_segment_visible(
                            snapshot.metrics.disk_percent,
                            self.state.status_disk_visible,
                        );
                    self.state.status_metrics = Some(*snapshot);
                }
                self.state.status_now_unix = crate::provider_usage::now_unix();
                should_repaint && self.status_metrics_visible
            }
            AppEvent::ProviderUsageRefreshed { snapshot } => {
                self.provider_usage_in_flight = false;
                self.state.status_now_unix = crate::provider_usage::now_unix();
                self.state.provider_usage = *snapshot;
                // Refresh is already bounded to once per minute. Repaint even when
                // the payload is unchanged so attach-local countdowns advance without
                // projecting per-client Usage-tab state into shared AppState.
                true
            }
            AppEvent::RemoteFocusTransition {
                operation_id,
                transition,
            } => {
                self.apply_remote_focus_transition(&operation_id, *transition);
                false
            }
            AppEvent::RemoteFocusFrame {
                operation_id,
                frame,
            } => {
                self.apply_remote_focus_frame(&operation_id, &frame);
                false
            }
            AppEvent::ConnectivityProbed { reachable } => {
                self.connectivity_probe_in_flight = false;
                self.state.connectivity.observe(reachable) && self.state.status_bar_enabled
            }
            AppEvent::HomeCatalogRefreshed { catalog } => {
                self.state.home_catalog.replace(catalog.clone());
                if let Some(home) = self.state.home.as_mut() {
                    home.replace_provider_catalog(catalog);
                    true
                } else {
                    false
                }
            }
            AppEvent::HomeRefsRefreshed { repo_root, result } => {
                self.handle_home_refs_refreshed(repo_root, result)
            }
            AppEvent::HomeGithubReposRefreshed { owner, result } => {
                self.handle_home_github_repos_refreshed(owner, result)
            }
            AppEvent::ToolProbesFinished { probes } => self.handle_tool_probes_finished(probes),
            AppEvent::HomeCheckoutFinished { plan, result } => {
                self.handle_home_checkout_finished(*plan, result)
            }
            AppEvent::HomeRemoteSpawnFinished { machine, result } => {
                self.handle_home_remote_spawn_finished(&machine, result)
            }
            AppEvent::GitStatusRefreshed {
                generation,
                results,
                cache_updates,
                file_fingerprints,
            } => self.handle_git_status_refreshed(
                generation,
                results,
                cache_updates,
                file_fingerprints,
            ),
            AppEvent::DockFilesRefreshed {
                generation,
                snapshot,
            } => self.handle_dock_files_refreshed(generation, snapshot),
            AppEvent::DiffRefreshed { generation, result } => {
                self.handle_diff_refreshed(generation, *result)
            }
            AppEvent::GitWorkContextRefreshed {
                generation,
                observations,
                cache_updates,
            } => self.handle_git_work_context_refreshed(generation, observations, cache_updates),
            AppEvent::WorkIndexRefreshed {
                generation,
                snapshot,
                session,
            } => self.handle_work_index_refreshed(generation, *snapshot, session),
            AppEvent::UsageScanFinished { generation, result } => {
                self.handle_usage_scan_finished(generation, result)
            }
            AppEvent::WorkItemDetailRefreshed {
                generation,
                details,
            } => self.handle_work_item_detail_refreshed(generation, details),
            AppEvent::ForegroundProcessesRefreshed {
                generation,
                observations,
            } => self.handle_foreground_processes_refreshed(generation, observations),
            AppEvent::ClaudeSubagentsRefreshed {
                generation,
                observations,
                stats,
            } => self.handle_claude_subagents_refreshed(generation, observations, stats),
            AppEvent::TabBarCommandFinished {
                generation,
                segment_index,
                result,
            } => self.handle_tab_bar_command_finished(generation, segment_index, result),
            ev => {
                self.handle_internal_event(ev);
                true
            }
        }
    }

    fn handle_git_status_refreshed(
        &mut self,
        generation: u64,
        results: Vec<crate::workspace::WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, crate::workspace::GitStatusCacheEntry)>,
        file_fingerprints: Vec<(std::path::PathBuf, u64)>,
    ) -> bool {
        let Some(refresh) = self.git_refresh_in_flight else {
            return false;
        };
        if generation != refresh.generation {
            return false;
        }
        let now = Instant::now();
        if now >= refresh.deadline {
            self.git_refresh_in_flight = None;
            self.git_refresh_due_after_in_flight = false;
            self.mark_git_status_refresh_due(now);
            return false;
        }

        self.git_refresh_in_flight = None;
        for (key, entry) in cache_updates {
            self.git_status_cache.insert(key, entry);
        }
        if self.git_refresh_due_after_in_flight {
            self.mark_git_status_refresh_due(now);
            self.git_refresh_due_after_in_flight = false;
        } else {
            self.last_git_remote_status_refresh = now;
        }
        let projected_focus = self
            .state
            .status_bar_enabled
            .then(|| self.focused_status_context_key());
        let mut changed = self
            .state
            .apply_workspace_git_statuses(&self.terminal_runtimes, results);
        for (root, fingerprint) in file_fingerprints {
            let stale = self
                .state
                .dock_file_cache
                .get(&root)
                .is_some_and(|snapshot| snapshot.fingerprint != fingerprint);
            if stale {
                self.state.dock_file_cache.remove(&root);
                changed = true;
            }
        }
        if let Some(projected_focus) = projected_focus {
            self.status_context_focus = projected_focus;
        }
        if changed {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changed | self.finish_sidebar_refresh_if_idle()
    }

    pub(crate) fn handle_internal_event_with_pane_updates(
        &mut self,
        ev: AppEvent,
    ) -> Vec<crate::app::actions::PaneStateUpdate> {
        if matches!(
            ev,
            AppEvent::StateChanged { .. }
                | AppEvent::AgentProcessDetected { .. }
                | AppEvent::PaneProcessStateChanged { .. }
                | AppEvent::HookStateReported { .. }
                | AppEvent::AgentSessionReported { .. }
                | AppEvent::HookMetadataReported { .. }
                | AppEvent::HookAuthorityCleared { .. }
                | AppEvent::HookAuthorityRetired { .. }
                | AppEvent::HookAgentReleased { .. }
                | AppEvent::TerminalCwdReported { .. }
        ) {
            let previous_toast = self.state.toast.clone();
            let (updates, _) = self.state.handle_app_event_with_hook_report_status(ev);
            for update in &updates {
                self.refresh_new_herdr_toast_context_for_update(update, &previous_toast);
                self.emit_pane_state_update(update);
            }
            self.flush_pane_snooze_events();
            return updates;
        }
        let _ = self.handle_internal_event(ev);
        Vec::new()
    }

    pub(crate) fn handle_internal_event(&mut self, ev: AppEvent) -> Option<bool> {
        if let AppEvent::PaneExitCheckpoint { pane_id } = &ev {
            self.pane_exit_checkpoint_requests.insert(*pane_id);
            return None;
        }
        let checkpoint_requested = match &ev {
            AppEvent::PaneDied { pane_id } => self.pane_exit_checkpoint_requests.remove(pane_id),
            _ => false,
        };
        let hook_report = match &ev {
            AppEvent::HookStateReported {
                pane_id,
                source,
                agent_label,
                ..
            } => Some((
                *pane_id,
                crate::detect::is_closing_block_source(source, agent_label),
            )),
            _ => None,
        };
        if let AppEvent::RemoteFocusTransition {
            operation_id,
            transition,
        } = ev
        {
            self.apply_remote_focus_transition(&operation_id, *transition);
            return None;
        }
        if let AppEvent::RemoteFocusFrame {
            operation_id,
            frame,
        } = ev
        {
            self.apply_remote_focus_frame(&operation_id, &frame);
            return Some(false);
        }
        if let AppEvent::SymphonyWorkflowsRefreshed { snapshot } = ev {
            return Some(self.refresh_symphony_snapshot(snapshot));
        }

        if let AppEvent::FleetRefreshed { snapshot } = ev {
            let changed = self.install_fleet_snapshot(snapshot);
            self.refresh_board_remote_lines();
            return Some(changed);
        }

        if let AppEvent::BoardRemoteLinesFetched {
            note_path,
            fleet_generation,
            request_id,
            complete,
            lines,
        } = ev
        {
            return Some(self.apply_board_remote_lines(
                &note_path,
                fleet_generation,
                request_id,
                complete,
                lines,
            ));
        }

        if let AppEvent::FleetAgentInventoryChanged {
            host,
            config_generation,
            agents,
        } = ev
        {
            return Some(self.install_fleet_agent_inventory(host, config_generation, agents));
        }

        if let AppEvent::RemoteApiRequestFinished {
            agent_ref,
            response,
        } = ev
        {
            return Some(self.finish_remote_api_request(agent_ref, response));
        }

        if let AppEvent::AuthorityAcceptanceLedgerPersisted {
            ledger,
            snapshot,
            result,
        } = ev
        {
            return Some(self.finish_authority_acceptance_ledger_write(ledger, *snapshot, result));
        }

        if let AppEvent::AuthorityAcceptanceLedgerReconciled {
            ledger,
            snapshot,
            result,
        } = ev
        {
            return Some(
                self.finish_authority_acceptance_ledger_reconciliation(ledger, *snapshot, result),
            );
        }

        if let AppEvent::ScratchpadChanged = ev {
            return Some(self.reload_scratchpad());
        }

        if let AppEvent::NotepadChanged = ev {
            return Some(self.reload_notepad());
        }

        if let AppEvent::GoalsRefreshed {
            generation,
            refresh,
        } = ev
        {
            return Some(self.finish_goals_refresh(generation, refresh.clone()));
        }

        if let AppEvent::LoopRunHistoryChanged = ev {
            return Some(self.refresh_loop_run_history());
        }

        if let AppEvent::StatusMetricsRefreshed { snapshot } = ev {
            self.status_metric_refresh
                .finish_and_should_repaint(snapshot.as_ref().map(|value| value.sampled_at));
            if let Some(snapshot) = snapshot {
                self.state.status_metrics = Some(*snapshot);
            }
            return None;
        }

        if let AppEvent::ProviderUsageRefreshed { snapshot } = ev {
            self.provider_usage_in_flight = false;
            self.state.provider_usage = *snapshot;
            return None;
        }

        if let AppEvent::ConnectivityProbed { reachable } = ev {
            self.connectivity_probe_in_flight = false;
            self.state.connectivity.observe(reachable);
            return None;
        }

        if let AppEvent::HomeCatalogRefreshed { catalog } = ev {
            self.state.home_catalog.replace(catalog.clone());
            if let Some(home) = self.state.home.as_mut() {
                home.replace_provider_catalog(catalog);
            }
            return None;
        }

        if let AppEvent::HomeRefsRefreshed { repo_root, result } = ev {
            self.handle_home_refs_refreshed(repo_root, result);
            return None;
        }

        if let AppEvent::HomeGithubReposRefreshed { owner, result } = ev {
            self.handle_home_github_repos_refreshed(owner, result);
            return None;
        }

        if let AppEvent::ToolProbesFinished { probes } = ev {
            self.handle_tool_probes_finished(probes);
            return None;
        }

        if let AppEvent::HomeCheckoutFinished { plan, result } = ev {
            self.handle_home_checkout_finished(*plan, result);
            return None;
        }

        if let AppEvent::ClipboardWrite { content } = ev {
            #[cfg(not(test))]
            crate::selection::write_osc52_bytes(&content);
            #[cfg(test)]
            let _ = content;
            self.show_clipboard_feedback();
            return None;
        }

        if let AppEvent::PrefixInputSource { active, .. } = ev {
            // Monolithic path applies the switch here. Server mode forwards it to the foreground
            // client instead (see HeadlessServer::handle_internal_event_with_forwarding); should an
            // App-internal drain consume the event before the forwarding drain, the flag keeps the
            // switch out of the headless server process.
            if !self.local_input_source_switch {
                return None;
            }
            if active {
                self.prefix_input_source.switch_to_ascii();
            } else {
                self.prefix_input_source.restore();
            }
            return None;
        }

        if let AppEvent::GitStatusRefreshed {
            generation,
            results,
            cache_updates,
            file_fingerprints,
        } = ev
        {
            self.handle_git_status_refreshed(generation, results, cache_updates, file_fingerprints);
            return None;
        }

        if let AppEvent::DockFilesRefreshed {
            generation,
            snapshot,
        } = ev
        {
            self.handle_dock_files_refreshed(generation, snapshot);
            return None;
        }

        if let AppEvent::GitWorkContextRefreshed {
            generation,
            observations,
            cache_updates,
        } = ev
        {
            self.handle_git_work_context_refreshed(generation, observations, cache_updates);
            return None;
        }

        if let AppEvent::DiffRefreshed { generation, result } = ev {
            self.handle_diff_refreshed(generation, *result);
            return None;
        }

        if let AppEvent::WorkIndexRefreshed {
            generation,
            snapshot,
            session,
        } = ev
        {
            self.handle_work_index_refreshed(generation, *snapshot, session);
            return None;
        }

        if let AppEvent::UsageScanFinished { generation, result } = ev {
            self.handle_usage_scan_finished(generation, result);
            return None;
        }

        if let AppEvent::WorkItemDetailRefreshed {
            generation,
            details,
        } = ev
        {
            self.handle_work_item_detail_refreshed(generation, details);
            return None;
        }

        if let AppEvent::ForegroundProcessesRefreshed {
            generation,
            observations,
        } = ev
        {
            self.handle_foreground_processes_refreshed(generation, observations);
            return None;
        }

        if let AppEvent::ClaudeSubagentsRefreshed {
            generation,
            observations,
            stats,
        } = ev
        {
            self.handle_claude_subagents_refreshed(generation, observations, stats);
            return None;
        }

        if let AppEvent::TabBarCommandFinished {
            generation,
            segment_index,
            result,
        } = ev
        {
            self.handle_tab_bar_command_finished(generation, segment_index, result);
            return None;
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
            return None;
        }

        if let AppEvent::WorktreeAddFinished(result) = ev {
            self.handle_worktree_add_finished(*result);
            return None;
        }

        if let AppEvent::WorktreeRemoveFinished(result) = ev {
            self.handle_worktree_remove_finished(*result);
            return None;
        }

        if let AppEvent::PaneDied { pane_id } = &ev {
            if self.pane_runtime_is_suspended(*pane_id) {
                return None;
            }
            if let Some(ws_idx) = self.orphan_pane_work_owner(*pane_id) {
                self.schedule_session_save();
                self.emit_pane_updated(ws_idx, *pane_id);
            }
            if self.handle_dock_editor_exit(*pane_id) {
                return None;
            }
            if self
                .state
                .popup_pane
                .as_ref()
                .is_some_and(|popup| popup.pane_id == *pane_id)
            {
                self.close_popup_pane();
                return None;
            }
            let previous_toast = self.state.toast.clone();
            if let Some(update) = self.state.publish_pane_process_exit_if_agent(*pane_id) {
                self.sync_detection_authority_mirrors();
                self.refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
                if update.hook_work_context_changed {
                    self.schedule_session_save();
                }
                self.emit_pane_state_update(&update);
                self.emit_terminal_or_system_agent_notifications(std::slice::from_ref(&update));
            }
            if self.runtime_exit_action(*pane_id) == RuntimeExitAction::RespawnShell
                && self.respawn_shell_for_launch_pane(*pane_id)
            {
                self.overlay_panes.remove(pane_id);
                self.render_dirty.request_generic();
                self.render_notify.notify_one();
                return None;
            }
        }

        let checkpointed_pane_exit = checkpoint_requested
            && matches!(
                &ev,
                AppEvent::PaneDied { pane_id }
                    if self.find_pane(*pane_id).is_some()
                        && !self.overlay_panes.contains_key(pane_id)
            );
        if checkpointed_pane_exit {
            self.checkpoint_session_before_pane_exit();
        }

        let overlay_state = if let AppEvent::PaneDied { pane_id } = &ev {
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

        if let AppEvent::PaneDied { pane_id } = &ev {
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
        let pane_exit_layout_target = if let AppEvent::PaneDied { pane_id } = &ev {
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
        let (pane_updates, hook_state_report_accepted) =
            self.state.handle_app_event_with_hook_report_status(ev);
        if checkpointed_pane_exit {
            self.finish_checkpointed_pane_exit();
        }
        if pane_updates
            .iter()
            .any(|update| update.hook_work_context_changed)
        {
            self.schedule_session_save();
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
        self.sync_detection_authority_mirrors();
        if hook_state_report_accepted == Some(true) {
            if let Some((pane_id, closing_report)) = hook_report {
                if let Some((ws_idx, _)) = self.find_pane(pane_id) {
                    if let Some(runtime) = self.state.runtime_for_pane_in_workspace(
                        &self.terminal_runtimes,
                        ws_idx,
                        pane_id,
                    ) {
                        runtime.rebaseline_hook_authority_output();
                        if closing_report {
                            // Closing-block reports describe turn-end/gate state,
                            // not the full lifecycle. Rescan unchanged terminal
                            // bytes so a quiet prompt becomes the baseline for a
                            // later visible turn start.
                            runtime.request_agent_screen_rescan();
                        }
                    }
                }
            }
        }
        if terminal_cwd_reported {
            if self.state.status_bar_enabled {
                self.project_status_context_from_cached();
            }
            self.request_git_work_context_refresh(Instant::now());
            self.request_git_identity_refresh(Instant::now());
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        for update in &pane_updates {
            self.refresh_new_herdr_toast_context_for_update(update, &previous_toast);
            self.emit_pane_state_update(update);
        }
        self.flush_pane_snooze_events();
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

        if self.local_terminal_notifications
            && matches!(
                self.state.toast_config.delivery,
                crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System
            )
        {
            if let Some((version, install_command)) = update_ready {
                let instruction = crate::update::update_install_instruction(&install_command);
                let _ = self.show_client_local_notification(
                    &format!("v{version} available"),
                    Some(&instruction),
                );
            } else if self.state.toast_config.delay_seconds == 0 {
                self.emit_terminal_or_system_agent_notifications(&pane_updates);
            }
        }

        self.sync_toast_deadline(previous_toast);
        self.shutdown_detached_terminal_runtimes();
        hook_state_report_accepted
    }

    fn refresh_symphony_snapshot(&mut self, snapshot: crate::symphony::Snapshot) -> bool {
        // Compare the whole snapshot rather than a hand-listed subset of its
        // fields. Enumerating them silently drops any field added later: the
        // first successful empty poll changes only `polled`, and that is exactly
        // the transition that makes the Symphony section appear.
        let changed = self.state.symphony_snapshot != snapshot;
        self.state.symphony_snapshot = snapshot.clone();
        if let Some(detail) = self.state.symphony_detail.as_mut() {
            detail.replace_snapshot(snapshot);
        }
        changed || self.state.symphony_detail.is_some()
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
            &self.state.terminals,
            &workspace_label,
            update.ws_idx,
            update.pane_id,
        );
        if let Some(toast) = self.state.toast.as_mut() {
            toast.context = context;
        }
    }

    /// Mirror the two terminal facts the detection task cannot read itself:
    /// whether a full-lifecycle hook owns the pane, and whether that hook has
    /// gone stale. The second one re-enables the process probe the first one
    /// suppresses, which is how a finished agent still gets resolved to idle.
    pub(crate) fn sync_detection_authority_mirrors(&self) {
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
                    runtime.set_full_lifecycle_authority_state(
                        terminal.full_lifecycle_hook_authority_active(),
                        terminal.hook_authority_output_retirement_eligible(),
                    );
                    runtime.set_supervisor_stale(terminal.supervisor_stale);
                }
            }
        }
    }

    pub(crate) fn retire_blocked_hook_authority_for_terminal(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        observed_at: Instant,
    ) {
        let Some(pane_id) = self.state.workspaces.iter().find_map(|workspace| {
            workspace.tabs.iter().find_map(|tab| {
                tab.panes.iter().find_map(|(pane_id, pane)| {
                    (pane.attached_terminal_id == *terminal_id).then_some(*pane_id)
                })
            })
        }) else {
            return;
        };
        self.retire_blocked_hook_authority_for_pane(pane_id, observed_at);
    }

    pub(crate) fn retire_blocked_hook_authority_for_pane(
        &mut self,
        pane_id: crate::layout::PaneId,
        observed_at: Instant,
    ) {
        self.retire_blocked_hook_authority_for_pane_with_gate_answer(pane_id, observed_at, true);
    }

    pub(crate) fn retire_blocked_hook_authority_for_automated_input(
        &mut self,
        pane_id: crate::layout::PaneId,
        observed_at: Instant,
    ) {
        self.retire_blocked_hook_authority_for_pane_with_gate_answer(pane_id, observed_at, false);
    }

    fn retire_blocked_hook_authority_for_pane_with_gate_answer(
        &mut self,
        pane_id: crate::layout::PaneId,
        observed_at: Instant,
        answers_human_gate: bool,
    ) {
        self.cancel_pending_stall_nudge_for_pane(pane_id);
        if answers_human_gate {
            self.state.note_human_pane_activity_at(pane_id, observed_at);
        } else {
            self.state
                .note_automated_pane_activity_at(pane_id, observed_at);
        }
        self.record_contract_false_positive_for_pane(pane_id, observed_at);
        self.handle_internal_event(AppEvent::HookAuthorityRetired {
            pane_id,
            observed_at,
            suppress_completion: true,
            answers_human_gate,
        });
    }

    pub(crate) fn begin_contract_false_positive_input_burst(&mut self) {
        self.contract_false_positive_burst_panes.clear();
    }

    fn record_contract_false_positive_for_pane(
        &mut self,
        pane_id: crate::layout::PaneId,
        observed_at: Instant,
    ) {
        let Some((ws_idx, pane)) = self.find_pane(pane_id) else {
            return;
        };
        let terminal_id = pane.attached_terminal_id.clone();
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return;
        };
        let active_subagents = terminal
            .effective_active_subagents()
            .filter(|count| *count > 0);
        let tier = crate::terminal::state::derive_completion_tier(
            terminal.raw_agent_state(),
            terminal.closing_contract(),
            terminal.closing_contract_met(),
            terminal.closing_idle(),
            !terminal.closing_gates().is_empty(),
            active_subagents,
            terminal.holds_shell,
            terminal.has_closing_report(),
        );
        if tier != Some(crate::terminal::state::CompletionTier::ContractSatisfied) {
            return;
        }
        let Some(contract_met_at) = terminal.closing_contract_met_at() else {
            return;
        };
        let Some(contract) = terminal.closing_contract().map(str::to_string) else {
            return;
        };
        let session_id = terminal.current_agent_session_id().map(str::to_string);
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        if !self.contract_false_positive_burst_panes.insert(pane_id) {
            return;
        }
        let record = crate::contract_false_positive::ContractFalsePositive::now(
            &public_pane_id,
            session_id.as_deref(),
            &contract,
            observed_at
                .saturating_duration_since(contract_met_at)
                .as_secs(),
        );
        if let Err(error) = crate::contract_false_positive::append(
            &record,
            self.contract_false_positive_log_path_override.as_deref(),
        ) {
            tracing::warn!(%error, pane_id = %public_pane_id, "failed to append contract false positive");
        }
    }

    pub(crate) fn show_clipboard_feedback(&mut self) {
        if !self.state.toast_config.clipboard.enabled {
            self.state.copy_feedback = None;
            self.copy_feedback_deadline = None;
            return;
        }
        self.state.copy_feedback = Some(crate::app::state::CopyFeedback {
            message: "copied to clipboard".to_string(),
        });
        self.copy_feedback_deadline = Some(Instant::now() + super::COPY_FEEDBACK_DURATION);
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
            self.focus_client_on_pane();
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

    fn respawn_shell_for_launch_pane(&mut self, pane_id: crate::layout::PaneId) -> bool {
        let Some((ws_idx, pane_state)) = self.find_pane(pane_id) else {
            return false;
        };
        let terminal_id = pane_state.attached_terminal_id.clone();
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return false;
        };

        let cwd = terminal.cwd.clone();
        let (rows, cols) = self
            .terminal_runtimes
            .get(&terminal_id)
            .map(|runtime| runtime.current_size())
            .unwrap_or_else(|| self.state.estimate_pane_size());
        let Some(launch_env) = self.pane_launch_env(ws_idx, pane_id, Vec::new()) else {
            return false;
        };
        let runtime = match crate::terminal::TerminalRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.pane_terminal_theme(),
            Some(self.state.pane_terminal_appearance()),
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
        }
        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.schedule_session_save();
        true
    }

    pub(crate) fn emit_pane_state_update(&mut self, update: &crate::app::actions::PaneStateUpdate) {
        if let Some(captured) = self.api_pane_state_updates.as_mut() {
            captured.push(update.clone());
        }
        let Some(pane_id) = self.public_pane_id(update.ws_idx, update.pane_id) else {
            return;
        };
        let workspace_id = self.public_workspace_id(update.ws_idx);

        if update.agent_name_changed
            || update.session_ref_changed
            || update.hook_work_context_changed
        {
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

        let previous_agent_status = pane_agent_status_with_stale(
            update.previous_state,
            update.previous_seen,
            update.previous_stale,
        );
        let agent_status = pane_agent_status_with_stale(update.state, update.seen, update.stale);

        if previous_agent_status != agent_status
            || update.previous_waiting_on_agents != update.waiting_on_agents
            || update.previous_wait != update.wait
            || update.previous_eta_s != update.eta_s
            || update.previous_reported_at != update.reported_at
            || update.previous_last_turn_at != update.last_turn_at
            || update.previous_presentation != update.presentation
        {
            let presentation = update.presentation.clone();
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentStatusChanged,
                data: crate::api::schema::EventData::PaneAgentStatusChanged {
                    pane_id,
                    workspace_id,
                    agent_status,
                    waiting_on_agents: update.waiting_on_agents,
                    wait: update.wait.clone(),
                    eta_s: update.eta_s,
                    reported_at: update.reported_at.clone(),
                    last_turn_at: update.last_turn_at.clone(),
                    agent: update.agent_label.clone(),
                    title: presentation.title,
                    display_agent: presentation.display_agent,
                    state_labels: presentation.state_labels,
                },
            });
        }
    }

    pub(crate) fn emit_terminal_or_system_agent_notifications(
        &self,
        pane_updates: &[crate::app::actions::PaneStateUpdate],
    ) {
        if !self.local_terminal_notifications
            || self.state.toast_config.delay_seconds != 0
            || !matches!(
                self.state.toast_config.delivery,
                crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System
            )
        {
            return;
        }

        for update in pane_updates {
            let is_active_tab = self
                .state
                .pane_is_in_active_tab(update.ws_idx, update.pane_id);
            let suppress_active_tab_notifications =
                crate::app::actions::active_tab_suppresses_notifications(
                    is_active_tab,
                    self.state.outer_terminal_focus,
                );
            let Some(kind) = crate::app::actions::notification_toast_for_pane_state_update(
                suppress_active_tab_notifications,
                update,
            ) else {
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
            let Some(agent_label) = self
                .state
                .terminals
                .get(&pane.attached_terminal_id)
                .and_then(|terminal| terminal.effective_agent_label())
            else {
                continue;
            };
            let event_text = match kind {
                ToastKind::NeedsAttention => "needs attention",
                ToastKind::Finished => "finished",
                ToastKind::UpdateInstalled => "updated",
                ToastKind::WorkLinked => "linked",
            };
            let workspace_label =
                ws.display_name_from(&self.state.terminals, &self.terminal_runtimes);
            let _ = self.show_client_local_notification(
                &format!("{} {}", agent_label, event_text),
                Some(&crate::app::actions::notification_context(
                    ws,
                    &self.state.terminals,
                    &workspace_label,
                    update.ws_idx,
                    update.pane_id,
                )),
            );
        }
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
                    ToastKind::WorkLinked => Duration::from_secs(4),
                };
                Instant::now() + duration
            });
        }
    }

    pub(crate) fn emit_delayed_client_local_agent_notifications(
        &self,
        deliveries: &[crate::app::state::AgentNotificationDelivery],
    ) {
        if !self.local_terminal_notifications
            || !matches!(
                self.state.toast_config.delivery,
                crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System
            )
        {
            return;
        }

        for delivery in deliveries {
            let Some(toast) = &delivery.client_notification else {
                continue;
            };
            let _ = self.show_client_local_notification(&toast.title, Some(&toast.context));
        }
    }

    fn show_client_local_notification(
        &self,
        title: &str,
        body: Option<&str>,
    ) -> std::io::Result<bool> {
        let result = match self.state.toast_config.delivery {
            crate::config::ToastDelivery::Terminal => crate::terminal_notify::show_notification(
                title,
                body,
                self.state.toast_config.terminal_backend,
            ),
            crate::config::ToastDelivery::System => {
                crate::platform::show_desktop_notification(title, body)
            }
            _ => Ok(false),
        };
        if matches!(
            (self.state.toast_config.delivery, &result),
            (crate::config::ToastDelivery::Terminal, Ok(false))
        ) && !self
            .missing_terminal_notification_backend_warned
            .replace(true)
        {
            tracing::warn!(
                "terminal notification backend could not be detected; set ui.toast.terminal_backend to osc9 or osc99"
            );
        }
        result
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
                &self.state.terminals,
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
        self.observe_group_membership_lifecycle_event(&event.data);
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

    pub(super) fn send_outer_focus_event(&mut self, event: crate::ghostty::FocusEvent) {
        self.sync_focus_events_with_outer_event(Some(event));
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

        self.sync_status_context_before_render();
        if self.state.status_bar_enabled {
            self.request_git_identity_refresh(std::time::Instant::now());
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
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::WorkspaceFocused,
                data: crate::api::schema::EventData::WorkspaceFocused {
                    workspace_id: self.public_workspace_id(ws_idx),
                },
            });
            if let Some(tab_id) =
                self.public_tab_id(ws_idx, self.state.workspaces[ws_idx].active_tab)
            {
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

        self.last_focus = current_focus;
    }

    fn send_pane_focus_event(
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

    fn theme_status_result(&self) -> crate::api::schema::ResponseResult {
        let appearance_name = |appearance: crate::terminal_theme::HostAppearance| match appearance {
            crate::terminal_theme::HostAppearance::Dark => "dark".to_string(),
            crate::terminal_theme::HostAppearance::Light => "light".to_string(),
        };
        crate::api::schema::ResponseResult::ThemeStatus {
            host_reported: self.state.host_terminal_appearance.map(appearance_name),
            appearance_override: self.state.theme_runtime.host_appearance,
            effective_appearance: appearance_name(self.state.pane_terminal_appearance()),
            theme_name: self.state.theme_name.clone(),
        }
    }

    pub(crate) fn handle_api_request(&mut self, request: crate::api::schema::Request) -> String {
        self.drain_all_internal_events();
        self.handle_api_request_after_internal_events_drained(request)
    }

    pub(crate) fn handle_api_request_after_internal_events_drained(
        &mut self,
        request: crate::api::schema::Request,
    ) -> String {
        self.begin_contract_false_positive_input_burst();
        // Session reports must adopt identity before title projection; otherwise an OSC
        // title emitted by the incoming session is indistinguishable from the old one.
        if !matches!(
            &request.method,
            crate::api::schema::Method::PaneReportAgent(_)
                | crate::api::schema::Method::PaneReportAgentSession(_)
        ) {
            let _ = self.sync_terminal_titles();
        }
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
            Method::ThemeStatus(_) => SuccessResponse {
                id: request.id,
                result: self.theme_status_result(),
            },
            Method::ThemeSet(params) => {
                self.set_host_appearance_override(params.host_appearance);
                SuccessResponse {
                    id: request.id,
                    result: self.theme_status_result(),
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
            Method::WorkspaceList(_) => return self.handle_workspace_list(request.id),
            Method::WorkspaceGet(target) => return self.handle_workspace_get(request.id, target),
            Method::LoopList(_) => return self.handle_loop_list(request.id),
            Method::LoopRunHistory(params) => {
                return self.handle_loop_run_history(request.id, params)
            }
            Method::LoopFindings(_) => return self.handle_loop_findings(request.id),
            Method::SymphonyList(_) => return self.handle_symphony_list(request.id),
            Method::FleetList(_) => return self.handle_fleet_list(request.id),
            Method::GroupHostSnapshot(_) => return self.handle_group_host_snapshot(request.id),
            Method::GroupCreate(params) => return self.handle_group_create(request.id, params),
            Method::GroupRename(params) => return self.handle_group_rename(request.id, params),
            Method::GroupDelete(params) => return self.handle_group_delete(request.id, params),
            Method::GroupAuthorityMutate(params) => {
                return self.handle_group_authority_mutate(request.id, params)
            }
            Method::WorkspaceCreate(params) => {
                return self.handle_workspace_create(request.id, params);
            }
            Method::WorkspaceFocus(target) => {
                return self.handle_workspace_focus(request.id, target)
            }
            Method::WorkspaceRename(params) => {
                return self.handle_workspace_rename(request.id, params);
            }
            Method::WorkspaceBind(params) => {
                return self.handle_workspace_bind(request.id, params);
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
                return self.handle_workspace_close(request.id, target)
            }
            Method::WorktreeList(params) => return self.handle_worktree_list(request.id, params),
            Method::WorktreeCreate(params) => {
                let _ = params;
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "worktree.create is handled asynchronously by the app runtime",
                );
            }
            Method::WorktreeOpen(params) => return self.handle_worktree_open(request.id, params),
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
            Method::TabPrio(params) => return self.handle_tab_prio(request.id, params),
            Method::TabPin(params) => return self.handle_tab_pin(request.id, params),
            Method::TabStar(params) => return self.handle_tab_star(request.id, params),
            Method::TabMove(params) => return self.handle_tab_move(request.id, params),
            Method::TabClose(target) => return self.handle_tab_close(request.id, target),
            Method::AgentList(_) => return self.handle_agent_list(request.id),
            Method::AgentGet(target) => return self.handle_agent_get(request.id, target),
            Method::AgentState(params) => return self.handle_agent_state(request.id, params),
            Method::AgentReport(params) => return self.handle_agent_report(request.id, params),
            Method::DayAdd(params) => return self.handle_day_add(request.id, params),
            Method::DayList(params) => return self.handle_day_list(request.id, params),
            Method::DayBind(params) => return self.handle_day_bind(request.id, params),
            Method::DayLink(params) => return self.handle_day_link(request.id, params),
            Method::DayNote(params) => return self.handle_day_note(request.id, params),
            Method::DayDone(params) => return self.handle_day_done(request.id, params),
            Method::DayDismiss(params) => return self.handle_day_dismiss(request.id, params),
            Method::AgentFocus(params) => {
                return self.handle_agent_focus_params(request.id, params)
            }
            Method::AgentFocusStatus(params) => {
                return self.handle_agent_focus_status(request.id, params)
            }
            Method::AgentRename(params) => return self.handle_agent_rename(request.id, params),
            Method::AgentViewSet(params) => return self.handle_agent_view_set(request.id, params),
            Method::AgentViewClear(params) => {
                return self.handle_agent_view_clear(request.id, params)
            }
            Method::AgentStart(params) => return self.handle_agent_start(request.id, params),
            Method::AgentPrompt(params) => return self.handle_agent_prompt(request.id, params),
            Method::AgentWait(_) => {
                return responses::encode_error(
                    request.id,
                    "invalid_request",
                    "agent.wait is handled by the api server",
                );
            }
            Method::AgentRead(params) => return self.handle_agent_read(request.id, params),
            Method::AgentExplain(target) => return self.handle_agent_explain(request.id, target),
            Method::AgentSendKeys(params) => {
                return self.handle_agent_send_keys(request.id, params)
            }
            Method::PaneSplit(params) => return self.handle_pane_split(request.id, params),
            Method::PaneSplitCompanion(params) => {
                return self.handle_pane_split_companion(request.id, params)
            }
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
            Method::PaneList(params) => return self.handle_pane_list(request.id, params),
            Method::PaneCurrent(params) => return self.handle_pane_current(request.id, params),
            Method::PaneGet(target) => return self.handle_pane_get(request.id, target),
            Method::PaneSettle(target) => {
                return self.handle_pane_settlement(request.id, target, true)
            }
            Method::PaneUnsettle(target) => {
                return self.handle_pane_settlement(request.id, target, false)
            }
            Method::PaneSnooze(params) => return self.handle_pane_snooze(request.id, params),
            Method::PaneUnsnooze(target) => return self.handle_pane_unsnooze(request.id, target),
            Method::PaneFocus(target) => return self.handle_pane_focus(request.id, target),
            Method::PaneInputSet(params) => return self.handle_pane_input_set(request.id, params),
            Method::PaneRename(params) => return self.handle_pane_rename(request.id, params),
            Method::PaneGroupSet(params) => return self.handle_pane_group_set(request.id, params),
            Method::PaneWorkContextSet(params) => {
                return self.handle_pane_work_context_set(request.id, params);
            }
            Method::PaneRead(params) => return self.handle_pane_read(request.id, params),
            Method::PaneGraphicsSet(params) => {
                return self.handle_pane_graphics_set(request.id, params);
            }
            Method::PaneGraphicsClear(params) => {
                return self.handle_pane_graphics_clear(request.id, params);
            }
            Method::PaneGraphicsInfo(params) => {
                return self.handle_pane_graphics_info(request.id, params);
            }
            Method::PaneGraphicsStream(_) => {
                return responses::encode_error(
                    request.id,
                    "stream_transport_required",
                    "pane.graphics.stream requires the streaming socket transport",
                );
            }
            Method::PaneGraphicsStreamSet(params) => {
                return self.handle_pane_graphics_stream_set(request.id, params);
            }
            Method::PaneGraphicsStreamDirect(params) => {
                return self.handle_pane_graphics_stream_direct(request.id, params);
            }
            Method::PaneGraphicsStreamOpen(params) => {
                return self.handle_pane_graphics_stream_open(request.id, params);
            }
            Method::PaneGraphicsStreamClose(params) => {
                return self.handle_pane_graphics_stream_close(request.id, params);
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
            Method::PaneSendTextIf(params) => {
                return self.handle_pane_send_text_if(request.id, params)
            }
            Method::PaneSendInput(params) => {
                return self.handle_pane_send_input(request.id, params)
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

    pub(crate) fn handle_api_request_after_internal_events_drained_with_pane_updates(
        &mut self,
        request: crate::api::schema::Request,
    ) -> (String, Vec<crate::app::actions::PaneStateUpdate>) {
        assert!(
            self.api_pane_state_updates.is_none(),
            "nested API pane-state capture"
        );
        self.api_pane_state_updates = Some(Vec::new());
        let response = self.handle_api_request_after_internal_events_drained(request);
        let pane_updates = self.api_pane_state_updates.take().unwrap_or_default();
        (response, pane_updates)
    }

    fn handle_notification_show(
        &mut self,
        id: String,
        params: crate::api::schema::NotificationShowParams,
    ) -> String {
        use crate::api::schema::{NotificationShowReason, ResponseResult};

        let requested_sound = params.sound;
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
                    self.emit_api_notification_sound(requested_sound);
                    NotificationShowReason::Shown
                }
            }
            crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System => {
                if self.api_notification_rate_limited(Instant::now()) {
                    NotificationShowReason::RateLimited
                } else {
                    match self.show_client_local_notification(&title, body.as_deref()) {
                        Ok(true) => {
                            self.mark_api_notification_shown(Instant::now());
                            self.emit_api_notification_sound(requested_sound);
                            NotificationShowReason::Shown
                        }
                        Ok(false) | Err(_) => NotificationShowReason::NoForegroundClient,
                    }
                }
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

    fn emit_api_notification_sound(&self, sound: crate::api::schema::NotificationShowSound) {
        if !self.state.local_sound_playback || !self.state.sound.allows(None) {
            return;
        }
        if let Some(sound) = sound.to_sound() {
            crate::sound::play(sound, &self.state.sound);
        }
    }

    pub(crate) fn api_notification_rate_limited(&self, now: Instant) -> bool {
        self.last_api_notification_at
            .is_some_and(|last| now.duration_since(last) < API_NOTIFICATION_RATE_LIMIT)
    }

    pub(crate) fn mark_api_notification_shown(&mut self, now: Instant) {
        self.last_api_notification_at = Some(now);
    }
}

fn sanitized_notification_text(value: &str, max_chars: usize) -> Option<String> {
    let mut sanitized = String::new();
    let mut previous_space = false;
    for ch in value.chars() {
        let replacement = if ch == '\n' || ch == '\r' || ch == '\t' {
            Some(' ')
        } else if ch.is_control() {
            None
        } else {
            Some(ch)
        };
        let Some(ch) = replacement else {
            continue;
        };
        if ch.is_whitespace() {
            if previous_space {
                continue;
            }
            previous_space = true;
            sanitized.push(' ');
        } else {
            previous_space = false;
            sanitized.push(ch);
        }
        if sanitized.chars().count() >= max_chars {
            break;
        }
    }
    let sanitized = sanitized.trim().to_string();
    (!sanitized.is_empty()).then_some(sanitized)
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
        active_version: summary.active_version,
        cached_remote_version: summary.cached_remote_version,
        local_override_shadowing_remote: summary.local_override_shadowing_remote,
        overlaid_rule_ids: summary.overlaid_rule_ids,
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

    fn fleet_host(name: &str, target: &str) -> crate::fleet::HostSnapshot {
        crate::fleet::HostSnapshot {
            name: name.into(),
            target: target.into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            entries: Vec::new(),
        }
    }

    fn fleet_snapshot(hosts: Vec<crate::fleet::HostSnapshot>) -> crate::fleet::Snapshot {
        crate::fleet::Snapshot {
            polled: true,
            configured_hosts: hosts.iter().map(|host| host.name.clone()).collect(),
            hosts,
            ..crate::fleet::Snapshot::default()
        }
    }

    fn app_with_remote_lifecycle_entry() -> (App, crate::api::schema::AgentRef) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (remote, entry) = crate::ui::sidebar::tests::remote_control_fixture(
            crate::fleet::HostState::Reachable,
            crate::api::schema::AgentStatus::Working,
            false,
            false,
            false,
        );
        app.state.fleet_snapshot = remote.fleet_snapshot;
        app.state.remote_agent_panel_entries = remote.remote_agent_panel_entries;
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        (app, entry.agent_ref.clone())
    }

    #[cfg(unix)]
    fn install_failing_pane_lifecycle_transport(app: &mut App) {
        app.authority_mutation_router = crate::fleet::AuthorityMutationRouter::with_ssh_program(
            "/usr/bin/false".into(),
            std::time::Duration::from_secs(1),
        );
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
    }

    #[cfg(unix)]
    #[test]
    fn n1_remote_completion_success_and_error_never_mutate_cached_lifecycle() {
        let (mut app, agent_ref) = app_with_remote_lifecycle_entry();
        install_failing_pane_lifecycle_transport(&mut app);
        let before = app.state.remote_agent_panel_entries[0].clone();

        assert!(app
            .remote_pane_snooze(
                agent_ref.clone(),
                crate::api::schema::PaneSnoozeParams {
                    pane_id: agent_ref.agent.clone(),
                    duration_s: Some(60),
                    snoozed_until: None,
                },
            )
            .is_ok());
        assert_eq!(
            app.state.remote_agent_panel_entries[0].settled,
            before.settled
        );
        assert_eq!(
            app.state.remote_agent_panel_entries[0].snoozed_until,
            before.snoozed_until
        );
        assert!(app.remote_pane_unsnooze(agent_ref.clone()).is_ok());
        assert_eq!(
            app.state.remote_agent_panel_entries[0].settled,
            before.settled
        );
        assert_eq!(
            app.state.remote_agent_panel_entries[0].snoozed_until,
            before.snoozed_until
        );

        assert!(!app.finish_remote_api_request(
            agent_ref.clone(),
            r#"{"id":"ok","result":{"type":"pane"}}"#.into(),
        ));
        assert!(app.finish_remote_api_request(
            agent_ref,
            r#"{"id":"error","error":{"code":"pane_snoozed","message":"snoozed"}}"#.into(),
        ));

        let after = &app.state.remote_agent_panel_entries[0];
        assert_eq!(after.settled, before.settled);
        assert_eq!(after.snoozed_until, before.snoozed_until);
    }

    #[cfg(unix)]
    #[test]
    fn n2_successful_remote_settle_discards_returned_pane_projection() {
        let (mut app, agent_ref) = app_with_remote_lifecycle_entry();
        install_failing_pane_lifecycle_transport(&mut app);
        let before = app.state.remote_agent_panel_entries[0].clone();

        assert!(app.remote_pane_settle(agent_ref.clone()).is_ok());
        assert_eq!(
            app.state.remote_agent_panel_entries[0].settled,
            before.settled
        );
        assert_eq!(
            app.state.remote_agent_panel_entries[0].snoozed_until,
            before.snoozed_until
        );

        assert!(!app.finish_remote_api_request(
            agent_ref,
            r#"{"id":"ok","result":{"type":"pane","settled_at":123,"snoozed_until":456}}"#.into(),
        ));

        assert!(!app.state.remote_agent_panel_entries[0].settled);
        assert_eq!(app.state.remote_agent_panel_entries[0].snoozed_until, None);
    }

    #[test]
    fn f2_invalid_remote_transport_response_names_owner_without_retry_state() {
        let (mut app, agent_ref) = app_with_remote_lifecycle_entry();

        assert!(app.finish_remote_api_request(agent_ref, "{}".into()));

        assert!(app.state.toast.as_ref().is_some_and(|toast| {
            toast.context.contains("owner remote") && toast.context.contains("invalid response")
        }));
    }

    #[test]
    fn f3_receiver_error_codes_keep_owner_context() {
        for code in [
            "pane_not_found",
            "pane_snoozed",
            "pane_settled",
            "pane_needs_attention",
            "invalid_duration",
            "invalid_deadline",
        ] {
            let (mut app, agent_ref) = app_with_remote_lifecycle_entry();
            let response = serde_json::json!({
                "id": "error",
                "error": {"code": code, "message": "rejected"}
            })
            .to_string();
            assert!(app.finish_remote_api_request(agent_ref, response));
            assert!(app.state.toast.as_ref().is_some_and(|toast| {
                toast.context.contains("owner remote") && toast.context.contains(code)
            }));
        }
    }

    fn catalog_with_duplicate_pane_memberships(
        host: &str,
        authority: &crate::groups::AuthorityId,
    ) -> crate::fleet::GroupCatalog {
        let membership = |incarnation: &str| crate::groups::OwnedPaneMembership {
            pane_id: "workspace:pane".into(),
            pane_incarnation: incarnation.into(),
            membership: crate::groups::PaneGroupMembership::default(),
        };
        crate::fleet::GroupCatalog {
            host: host.into(),
            target: host.into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision: 1,
                groups: Vec::new(),
                memberships: vec![membership("first"), membership("second")],
            }),
            error: None,
        }
    }

    fn wait_for_app_event(app: &mut App, description: &str) -> crate::events::AppEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match app.event_rx.try_recv() {
                Ok(event) => return event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("{description} event unavailable: {error}"),
            }
        }
    }

    #[test]
    fn unchanged_provider_usage_repaints_attach_local_countdowns() {
        let mut config = crate::config::Config::default();
        config.ui.status_bar.enabled = false;
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let snapshot = app.state.provider_usage.clone();

        assert!(
            app.handle_internal_event_with_render_impact(AppEvent::ProviderUsageRefreshed {
                snapshot: Box::new(snapshot),
            })
        );
    }

    #[test]
    fn stale_fleet_snapshot_after_reload_is_not_installed() {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "office".into(),
            target: "machine-a".into(),
            ..Default::default()
        }];
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let stale = fleet_snapshot(vec![fleet_host("office", "machine-a")]);
        assert!(app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::FleetRefreshed {
                snapshot: stale.clone(),
            }
        ));

        let mut reloaded = config;
        reloaded.remote.fleet.hosts[0].target = "machine-b".into();
        app.apply_live_config(&reloaded, &[], &[], false);
        let after_reload = app.state.fleet_snapshot.clone();

        assert!(!app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::FleetRefreshed { snapshot: stale }
        ));
        assert_eq!(app.state.fleet_snapshot, after_reload);
        assert_eq!(app.state.fleet_snapshot.config_generation, 1);
    }

    #[test]
    fn reload_keeps_unchanged_fleet_rows_until_the_next_poll() {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![
            crate::config::FleetHostConfig {
                name: "office".into(),
                target: "machine-a".into(),
                ..Default::default()
            },
            crate::config::FleetHostConfig {
                name: "home".into(),
                target: "machine-home".into(),
                ..Default::default()
            },
        ];
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let snapshot = fleet_snapshot(vec![
            fleet_host("office", "machine-a"),
            fleet_host("home", "machine-home"),
        ]);
        app.handle_internal_event_with_render_impact(crate::events::AppEvent::FleetRefreshed {
            snapshot,
        });

        let mut reloaded = config;
        reloaded.remote.fleet.hosts[0].target = "machine-b".into();
        app.apply_live_config(&reloaded, &[], &[], false);

        assert_eq!(
            app.state
                .fleet_snapshot
                .hosts
                .iter()
                .map(|host| (host.name.as_str(), host.target.as_str()))
                .collect::<Vec<_>>(),
            vec![("home", "machine-home")]
        );
        assert_eq!(
            app.state.fleet_snapshot.configured_hosts,
            vec!["office", "home"]
        );
    }

    #[test]
    fn reload_removing_a_fleet_connection_emits_one_catalog_removal_event() {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "office".into(),
            target: "machine-a".into(),
            ..Default::default()
        }];
        let hub = crate::api::EventHub::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            hub.clone(),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([11; 16]);
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority,
                revision: 1,
                groups: Vec::new(),
                memberships: Vec::new(),
            }),
            error: None,
        }];

        let removed = crate::config::Config::default();
        app.apply_live_config(&removed, &[], &[], false);
        app.apply_live_config(&removed, &[], &[], false);

        let removal_events = hub
            .events_after(0)
            .into_iter()
            .filter(|(_, event)| {
                matches!(
                    event.data,
                    crate::api::schema::EventData::AuthorityCatalogsUpdated { ref catalogs }
                        if catalogs.is_empty()
                )
            })
            .count();
        assert_eq!(removal_events, 1);
    }

    #[test]
    fn reload_removing_one_connection_alias_keeps_one_catalog_and_emits_once() {
        let host = |name: &str| crate::config::FleetHostConfig {
            name: name.into(),
            target: "machine-a".into(),
            session: Some("agents".into()),
            socket: Some("/tmp/herdr.sock".into()),
            ..Default::default()
        };
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![host("office"), host("duplicate")];
        let hub = crate::api::EventHub::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            hub.clone(),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([12; 16]);
        let catalog = |name: &str| crate::fleet::GroupCatalog {
            host: name.into(),
            target: "machine-a".into(),
            local: false,
            session: Some("agents".into()),
            socket: Some("/tmp/herdr.sock".into()),
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision: 1,
                groups: Vec::new(),
                memberships: Vec::new(),
            }),
            error: None,
        };
        app.state.fleet_snapshot.group_catalogs = vec![catalog("office"), catalog("duplicate")];

        let mut reloaded = config;
        reloaded.remote.fleet.hosts = vec![host("duplicate")];
        app.apply_live_config(&reloaded, &[], &[], false);
        app.apply_live_config(&reloaded, &[], &[], false);

        assert_eq!(app.state.fleet_snapshot.group_catalogs.len(), 1);
        assert_eq!(app.state.fleet_snapshot.group_catalogs[0].host, "duplicate");
        let one_catalog_events = hub
            .events_after(0)
            .into_iter()
            .filter(|(_, event)| {
                matches!(
                    event.data,
                    crate::api::schema::EventData::AuthorityCatalogsUpdated { ref catalogs }
                        if catalogs.len() == 1
                )
            })
            .count();
        assert_eq!(one_catalog_events, 1);
    }

    #[test]
    fn unreachable_host_retains_prior_rows_as_unknown() {
        let config = crate::config::Config::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut reachable = fleet_host("office", "machine-a");
        reachable.entries = vec![crate::fleet::FleetRow::test_agent_row(
            "office",
            "retained-task",
        )];
        assert!(app.install_fleet_snapshot(fleet_snapshot(vec![reachable])));

        let mut unreachable = fleet_host("office", "machine-a");
        unreachable.state = crate::fleet::HostState::Unreachable;
        unreachable.error = Some("offline".into());
        unreachable.remote_identity = Some("must-not-survive".into());
        assert!(app.install_fleet_snapshot(fleet_snapshot(vec![unreachable])));

        let host = &app.state.fleet_snapshot.hosts[0];
        assert_eq!(host.state, crate::fleet::HostState::Unreachable);
        assert_eq!(host.remote_identity, None);
        assert_eq!(host.entries.len(), 1);
        let retained = &app.state.remote_agent_panel_entries[0];
        assert_eq!(retained.agent_ref.to_string(), "office::retained-task");
        assert_eq!(retained.state, crate::detect::AgentState::Unknown);
        assert!(retained.stale);
    }

    #[test]
    fn admitted_authority_catalogs_emit_on_the_json_event_path() {
        let config = crate::config::Config::default();
        let hub = crate::api::EventHub::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            hub.clone(),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([9; 16]);
        let mut snapshot = fleet_snapshot(Vec::new());
        snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision: 0,
                groups: Vec::new(),
                memberships: Vec::new(),
            }),
            error: None,
        }];

        assert!(app.install_fleet_snapshot(snapshot));

        let events = hub.events_after(0);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].1.event,
            crate::api::schema::EventKind::AuthorityCatalogsUpdated
        );
        let crate::api::schema::EventData::AuthorityCatalogsUpdated { catalogs } =
            &events[0].1.data
        else {
            panic!("expected authority catalog event");
        };
        assert_eq!(catalogs[0].authority_id.as_ref(), Some(&authority));
        assert_eq!(
            catalogs[0].state,
            crate::api::schema::AuthorityCatalogStateInfo::Fresh
        );
    }

    #[test]
    fn conflicted_catalog_ingress_removes_duplicate_memberships_from_consumers() {
        let hub = crate::api::EventHub::default();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            hub.clone(),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([39; 16]);
        let mut snapshot = fleet_snapshot(Vec::new());
        snapshot.group_catalogs = vec![
            catalog_with_duplicate_pane_memberships("office", &authority),
            catalog_with_duplicate_pane_memberships("home", &authority),
        ];

        assert!(app.install_fleet_snapshot(snapshot));

        let infos = app.authority_catalog_infos();
        assert!(infos.iter().all(|catalog| {
            catalog.state == crate::api::schema::AuthorityCatalogStateInfo::IdentityConflict
                && catalog
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.memberships.is_empty())
        }));
        let update_catalogs = hub
            .events_after(0)
            .into_iter()
            .find_map(|(_, event)| match event.data {
                crate::api::schema::EventData::AuthorityCatalogsUpdated { catalogs } => {
                    Some(catalogs)
                }
                _ => None,
            })
            .expect("catalog update event");
        assert!(update_catalogs.iter().all(|catalog| catalog
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.memberships.is_empty())));
    }

    #[test]
    fn unreadable_history_catalog_ingress_removes_duplicate_memberships_from_consumers() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.authority_acceptance_ledger_error = Some("retained history is unreadable".into());
        let authority = crate::groups::AuthorityId::from_random_bytes([40; 16]);
        let mut snapshot = fleet_snapshot(Vec::new());
        snapshot.group_catalogs = vec![catalog_with_duplicate_pane_memberships(
            "office", &authority,
        )];

        assert!(app.install_fleet_snapshot(snapshot));

        let infos = app.authority_catalog_infos();
        assert_eq!(
            infos[0].state,
            crate::api::schema::AuthorityCatalogStateInfo::Unavailable
        );
        assert!(infos[0]
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.memberships.is_empty()));
    }

    #[test]
    fn durable_catalog_write_runs_off_the_app_event_loop() {
        let config = crate::config::Config::default();
        let hub = crate::api::EventHub::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            hub.clone(),
        );
        let root = std::env::temp_dir().join(format!(
            "herdr-group-cache-worker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = root.join("remote-group-catalogs.json");
        app.authority_acceptance_ledger_path = Some(path.clone());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        app.authority_acceptance_ledger_writer =
            crate::fleet::AuthorityAcceptanceLedgerWriter::with_save(
                app.event_tx.clone(),
                move |path, ledger| {
                    let _ = started_tx.send(());
                    release_rx
                        .lock()
                        .map_err(|_| std::io::Error::other("cache release gate poisoned"))?
                        .recv()
                        .map_err(|_| std::io::Error::other("cache release gate closed"))?;
                    crate::fleet::save_authority_acceptance_ledger(path, ledger)
                },
            );
        let authority = crate::groups::AuthorityId::from_random_bytes([13; 16]);
        let mut snapshot = fleet_snapshot(Vec::new());
        snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority,
                revision: 1,
                groups: Vec::new(),
                memberships: Vec::new(),
            }),
            error: None,
        }];

        assert!(!app.install_fleet_snapshot(snapshot));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("background cache write started");
        let status = app.dispatch_api_request(
            "status",
            crate::api::schema::Method::ThemeStatus(Default::default()),
        );
        assert!(serde_json::from_str::<crate::api::schema::SuccessResponse>(&status).is_ok());
        assert!(hub.events_after(0).is_empty());
        assert!(app.state.fleet_snapshot.group_catalogs.is_empty());

        release_tx.send(()).expect("release cache writer");
        let completion = wait_for_app_event(&mut app, "cache completion");
        assert!(app.handle_internal_event_with_render_impact(completion));
        assert_eq!(app.state.fleet_snapshot.group_catalogs.len(), 1);
        assert_eq!(hub.events_after(0).len(), 1);
        assert!(path.is_file());
        std::fs::remove_dir_all(root).expect("remove cache worker fixture");
    }

    #[test]
    fn coalesced_polls_keep_an_accepted_tombstone_separate_from_latest_presentation() {
        let config = crate::config::Config::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let root = std::env::temp_dir().join(format!(
            "herdr-ledger-coalescing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = root.join("authority-acceptance-ledger.json");
        app.authority_acceptance_ledger_path = Some(path.clone());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let write_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        app.authority_acceptance_ledger_writer =
            crate::fleet::AuthorityAcceptanceLedgerWriter::with_save(app.event_tx.clone(), {
                let write_count = std::sync::Arc::clone(&write_count);
                move |path, ledger| {
                    if write_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        let _ = started_tx.send(());
                        release_rx
                            .lock()
                            .map_err(|_| std::io::Error::other("ledger release gate poisoned"))?
                            .recv()
                            .map_err(|_| std::io::Error::other("ledger release gate closed"))?;
                    }
                    crate::fleet::save_authority_acceptance_ledger(path, ledger)
                }
            });
        let authority = crate::groups::AuthorityId::from_random_bytes([19; 16]);
        let catalog = |revision, state| crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision,
                groups: vec![crate::groups::GroupRecord {
                    id: crate::groups::GroupId {
                        owner: authority.clone(),
                        local: 1,
                    },
                    revision,
                    state,
                }],
                memberships: Vec::new(),
            }),
            error: None,
        };
        let poll = |catalog| {
            let mut snapshot = fleet_snapshot(Vec::new());
            snapshot.group_catalogs = vec![catalog];
            snapshot
        };

        assert!(!app.install_fleet_snapshot(poll(catalog(
            2,
            crate::groups::GroupState::Active {
                name: "Work".into(),
            },
        ))));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("revision 2 ledger write started");
        assert!(!app.install_fleet_snapshot(poll(catalog(3, crate::groups::GroupState::Deleted,))));
        assert!(!app.install_fleet_snapshot(poll(catalog(
            2,
            crate::groups::GroupState::Active {
                name: "Work".into(),
            },
        ))));

        release_tx.send(()).expect("release revision 2 write");
        let first = wait_for_app_event(&mut app, "revision 2 ledger completion");
        app.handle_internal_event_with_render_impact(first);
        let second = wait_for_app_event(&mut app, "revision 3 ledger completion");
        app.handle_internal_event_with_render_impact(second);

        let durable =
            crate::fleet::load_authority_acceptance_ledger(&path).expect("reload coalesced ledger");
        let accepted = durable.accepted(&authority).expect("accepted authority");
        assert_eq!(accepted.revision, 3);
        assert!(matches!(
            accepted.groups[0].state,
            crate::groups::GroupState::Deleted
        ));
        let shown = &app.state.fleet_snapshot.group_catalogs[0];
        assert_eq!(shown.state, crate::fleet::GroupCatalogState::Stale);
        assert_eq!(shown.snapshot.as_ref().expect("latest answer").revision, 2);
        std::fs::remove_dir_all(root).expect("remove coalescing fixture");
    }

    #[test]
    fn successful_ledger_write_survives_config_reload_without_publishing_stale_catalog() {
        let config = crate::config::Config::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let root = std::env::temp_dir().join(format!(
            "herdr-ledger-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = root.join("remote-group-catalogs.json");
        app.authority_acceptance_ledger_path = Some(path.clone());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        app.authority_acceptance_ledger_writer =
            crate::fleet::AuthorityAcceptanceLedgerWriter::with_save(
                app.event_tx.clone(),
                move |path, ledger| {
                    let _ = started_tx.send(());
                    release_rx
                        .lock()
                        .map_err(|_| std::io::Error::other("ledger release gate poisoned"))?
                        .recv()
                        .map_err(|_| std::io::Error::other("ledger release gate closed"))?;
                    crate::fleet::save_authority_acceptance_ledger(path, ledger)
                },
            );
        let authority = crate::groups::AuthorityId::from_random_bytes([18; 16]);
        let mut snapshot = fleet_snapshot(Vec::new());
        snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision: 1,
                groups: Vec::new(),
                memberships: Vec::new(),
            }),
            error: None,
        }];

        assert!(!app.install_fleet_snapshot(snapshot));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("background ledger write started");
        let mut reloaded = config;
        reloaded.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "replacement".into(),
            target: "machine-b".into(),
            ..Default::default()
        }];
        app.apply_live_config(&reloaded, &[], &[], false);
        release_tx.send(()).expect("release ledger writer");
        let completion = wait_for_app_event(&mut app, "ledger completion");
        assert!(!app.handle_internal_event_with_render_impact(completion));

        assert!(app
            .authority_acceptance_ledger
            .accepted(&authority)
            .is_some());
        assert!(app.state.fleet_snapshot.group_catalogs.is_empty());
        let reloaded_ledger =
            crate::fleet::load_authority_acceptance_ledger(&path).expect("reload written ledger");
        assert!(reloaded_ledger.accepted(&authority).is_some());
        std::fs::remove_dir_all(root).expect("remove ledger reload fixture");
    }

    #[test]
    fn queued_tombstone_survives_config_reload_after_prior_write_completes() {
        let config = crate::config::Config::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let root = std::env::temp_dir().join(format!(
            "herdr-ledger-queued-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = root.join("authority-acceptance-ledger.json");
        app.authority_acceptance_ledger_path = Some(path.clone());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let write_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        app.authority_acceptance_ledger_writer =
            crate::fleet::AuthorityAcceptanceLedgerWriter::with_save(app.event_tx.clone(), {
                let write_count = std::sync::Arc::clone(&write_count);
                move |path, ledger| {
                    if write_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        let _ = started_tx.send(());
                        release_rx
                            .lock()
                            .map_err(|_| std::io::Error::other("ledger release gate poisoned"))?
                            .recv()
                            .map_err(|_| std::io::Error::other("ledger release gate closed"))?;
                    }
                    crate::fleet::save_authority_acceptance_ledger(path, ledger)
                }
            });
        let authority = crate::groups::AuthorityId::from_random_bytes([21; 16]);
        let catalog = |revision, state| crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision,
                groups: vec![crate::groups::GroupRecord {
                    id: crate::groups::GroupId {
                        owner: authority.clone(),
                        local: 1,
                    },
                    revision,
                    state,
                }],
                memberships: Vec::new(),
            }),
            error: None,
        };
        let poll = |catalog| {
            let mut snapshot = fleet_snapshot(Vec::new());
            snapshot.group_catalogs = vec![catalog];
            snapshot
        };

        assert!(!app.install_fleet_snapshot(poll(catalog(
            2,
            crate::groups::GroupState::Active {
                name: "Work".into(),
            },
        ))));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("revision 2 ledger write started");
        assert!(!app.install_fleet_snapshot(poll(catalog(3, crate::groups::GroupState::Deleted,))));

        app.apply_live_config(&config, &[], &[], false);
        let presentation_after_reload = app.state.fleet_snapshot.clone();
        release_tx.send(()).expect("release revision 2 write");
        let first = wait_for_app_event(&mut app, "revision 2 ledger completion");
        assert!(!app.handle_internal_event_with_render_impact(first));
        let second = wait_for_app_event(&mut app, "queued revision 3 ledger completion");
        assert!(!app.handle_internal_event_with_render_impact(second));

        let durable = crate::fleet::load_authority_acceptance_ledger(&path)
            .expect("reload ledger after queued tombstone");
        let accepted = durable.accepted(&authority).expect("accepted authority");
        assert_eq!(accepted.revision, 3);
        assert!(matches!(
            accepted.groups[0].state,
            crate::groups::GroupState::Deleted
        ));
        assert_eq!(app.authority_acceptance_ledger, durable);
        assert_eq!(app.state.fleet_snapshot, presentation_after_reload);
        std::fs::remove_dir_all(root).expect("remove queued reload fixture");
    }

    #[test]
    fn replacement_reconciles_a_retiring_writers_later_tombstone() {
        let config = crate::config::Config::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let root = std::env::temp_dir().join(format!(
            "herdr-ledger-reverse-handoff-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = root.join("authority-acceptance-ledger.json");
        app.authority_acceptance_ledger_path = Some(path.clone());
        let authority_a = crate::groups::AuthorityId::from_random_bytes([31; 16]);
        let authority_b = crate::groups::AuthorityId::from_random_bytes([32; 16]);
        let catalog = |authority: &crate::groups::AuthorityId, revision, deleted| {
            crate::fleet::GroupCatalog {
                host: authority.to_string(),
                target: authority.to_string(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::GroupCatalogState::Fresh,
                observed_authority_id: Some(authority.clone()),
                snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                    authority_id: authority.clone(),
                    revision,
                    groups: vec![crate::groups::GroupRecord {
                        id: crate::groups::GroupId {
                            owner: authority.clone(),
                            local: 1,
                        },
                        revision,
                        state: if deleted {
                            crate::groups::GroupState::Deleted
                        } else {
                            crate::groups::GroupState::Active {
                                name: "Work".into(),
                            }
                        },
                    }],
                    memberships: Vec::new(),
                }),
                error: None,
            }
        };
        let snapshot = |catalogs| {
            let mut snapshot = fleet_snapshot(Vec::new());
            snapshot.group_catalogs = catalogs;
            snapshot
        };

        let active_a = catalog(&authority_a, 1, false);
        let mut base = crate::fleet::AuthorityAcceptanceLedger::default();
        base.advance(active_a.snapshot.as_ref().expect("active authority A"))
            .expect("accept active authority A");
        crate::fleet::save_authority_acceptance_ledger(&path, &base).expect("persist handoff base");
        app.authority_acceptance_ledger = base.clone();

        let mut retiring = base;
        retiring
            .advance(
                catalog(&authority_a, 2, true)
                    .snapshot
                    .as_ref()
                    .expect("authority A tombstone"),
            )
            .expect("retiring server accepts tombstone");
        let (retire_tx, retire_rx) = std::sync::mpsc::channel();
        let (retired_tx, retired_rx) = std::sync::mpsc::channel();
        let retiring_path = path.clone();
        let retiring_writer = std::thread::spawn(move || {
            retire_rx.recv().expect("release retiring writer");
            crate::fleet::save_authority_acceptance_ledger(&retiring_path, &retiring)
                .expect("retiring server persists tombstone after replacement");
            retired_tx.send(()).expect("report retiring write");
        });

        let active_b = catalog(&authority_b, 1, false);
        assert!(!app.install_fleet_snapshot(snapshot(vec![active_a.clone(), active_b.clone(),])));
        let replacement_write = wait_for_app_event(&mut app, "replacement ledger completion");
        retire_tx.send(()).expect("start retiring write");
        retired_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("retiring writer completion");
        retiring_writer.join().expect("join retiring writer");
        app.handle_internal_event_with_render_impact(replacement_write);

        let app_loop_thread = std::thread::current().id();
        let (reconcile_tx, reconcile_rx) = std::sync::mpsc::channel();
        let observed_authority = authority_a.clone();
        app.authority_acceptance_ledger_writer =
            crate::fleet::AuthorityAcceptanceLedgerWriter::with_save(
                app.event_tx.clone(),
                move |path, requested| {
                    let requested_tombstone =
                        requested
                            .accepted(&observed_authority)
                            .is_some_and(|snapshot| {
                                matches!(
                                    snapshot.groups[0].state,
                                    crate::groups::GroupState::Deleted
                                )
                            });
                    reconcile_tx
                        .send((std::thread::current().id(), requested_tombstone))
                        .map_err(|_| std::io::Error::other("reconciliation observation closed"))?;
                    crate::fleet::save_authority_acceptance_ledger(path, requested)
                },
            );

        assert!(!app.install_fleet_snapshot(snapshot(vec![active_a, active_b])));
        assert!(
            app.authority_acceptance_ledger_write_in_flight,
            "an equality poll must reconcile the ledger written by the retiring server"
        );
        let (reconciliation_thread, requested_tombstone) = reconcile_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("background reconciliation observation");
        assert_ne!(reconciliation_thread, app_loop_thread);
        assert!(
            !requested_tombstone,
            "the app loop loaded the late tombstone before queueing reconciliation"
        );
        let reconciliation = wait_for_app_event(&mut app, "handoff reconciliation completion");
        app.handle_internal_event_with_render_impact(reconciliation);

        let durable = crate::fleet::load_authority_acceptance_ledger(&path)
            .expect("reload reconciled handoff ledger");
        assert_eq!(app.authority_acceptance_ledger, durable);
        assert!(matches!(
            durable
                .accepted(&authority_a)
                .expect("authority A retained history")
                .groups[0]
                .state,
            crate::groups::GroupState::Deleted
        ));
        let shown_a = app
            .state
            .fleet_snapshot
            .group_catalogs
            .iter()
            .find(|catalog| catalog.authority_id() == Some(&authority_a))
            .expect("authority A catalog");
        assert_eq!(shown_a.state, crate::fleet::GroupCatalogState::Stale);
        std::fs::remove_dir_all(root).expect("remove reverse handoff fixture");
    }

    #[test]
    fn catalog_admission_fails_closed_when_the_durable_ledger_cannot_advance() {
        let config = crate::config::Config::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([10; 16]);
        let record = |revision, state| crate::groups::GroupRecord {
            id: crate::groups::GroupId {
                owner: authority.clone(),
                local: 1,
            },
            revision,
            state,
        };
        let catalog = |revision, state| crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision,
                groups: vec![record(revision, state)],
                memberships: Vec::new(),
            }),
            error: None,
        };
        let accepted_catalog = catalog(
            1,
            crate::groups::GroupState::Active {
                name: "Work".into(),
            },
        );
        app.authority_acceptance_ledger
            .advance(
                accepted_catalog
                    .snapshot
                    .as_ref()
                    .expect("accepted snapshot"),
            )
            .expect("seed accepted history");
        app.state.fleet_snapshot.group_catalogs = vec![accepted_catalog];

        let blocker = std::env::temp_dir().join(format!(
            "herdr-group-cache-blocker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&blocker, b"not a directory").expect("create cache blocker");
        app.authority_acceptance_ledger_path = Some(blocker.join("remote-group-catalogs.json"));
        let mut incoming = fleet_snapshot(Vec::new());
        incoming.group_catalogs = vec![catalog(2, crate::groups::GroupState::Deleted)];

        assert!(!app.install_fleet_snapshot(incoming));
        let completion = wait_for_app_event(&mut app, "failed cache write");
        assert!(app.handle_internal_event_with_render_impact(completion));

        let current = &app.state.fleet_snapshot.group_catalogs[0];
        assert_eq!(current.state, crate::fleet::GroupCatalogState::Stale);
        assert!(matches!(
            current
                .snapshot
                .as_ref()
                .expect("current authority answer")
                .groups[0]
                .state,
            crate::groups::GroupState::Deleted
        ));
        assert!(matches!(
            app.authority_acceptance_ledger
                .accepted(&authority)
                .expect("previous accepted history")
                .groups[0]
                .state,
            crate::groups::GroupState::Active { .. }
        ));
        assert!(current
            .error
            .as_deref()
            .is_some_and(|error| error.contains("durable ledger")));
        std::fs::remove_file(blocker).expect("remove cache blocker");
    }

    #[test]
    fn catalog_admission_fails_closed_when_retained_history_cannot_be_loaded() {
        let config = crate::config::Config::default();
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.authority_acceptance_ledger_error = Some("cannot parse retained history".into());
        let authority = crate::groups::AuthorityId::from_random_bytes([8; 16]);
        let mut incoming = fleet_snapshot(Vec::new());
        incoming.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: authority,
                revision: 2,
                groups: Vec::new(),
                memberships: Vec::new(),
            }),
            error: None,
        }];

        assert!(app.install_fleet_snapshot(incoming));

        let rejected = &app.state.fleet_snapshot.group_catalogs[0];
        assert_eq!(rejected.state, crate::fleet::GroupCatalogState::Unavailable);
        assert!(rejected.snapshot.is_some());
        assert!(rejected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("cannot parse retained history")));
    }

    fn codex_catalog(model: &str) -> crate::app::home_catalog::HomeProviderCatalog {
        crate::app::home_catalog::parse_codex_catalog(
            format!(
                r#"{{"models":[{{"slug":"{model}","visibility":"list","priority":1,"supported_reasoning_levels":[{{"effort":"high"}}]}}]}}"#
            )
            .as_bytes(),
        )
        .expect("valid Codex catalog")
    }

    #[test]
    fn first_empty_symphony_poll_repaints_so_the_section_can_appear() {
        // The default snapshot and a reachable-but-empty one differ only in
        // `polled`. If that is not treated as a change, the Symphony section
        // stays absent until some unrelated event forces a frame.
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        assert!(!app.state.symphony_snapshot.is_reachable());

        let empty = crate::symphony::Snapshot {
            workflows: Vec::new(),
            unavailable: None,
            polled: true,
        };
        assert!(app.handle_internal_event_with_render_impact(
            AppEvent::SymphonyWorkflowsRefreshed {
                snapshot: empty.clone(),
            }
        ));
        assert!(app.state.symphony_snapshot.is_reachable());

        // A second identical poll is not a change and must not force a frame.
        assert!(!app.handle_internal_event_with_render_impact(
            AppEvent::SymphonyWorkflowsRefreshed { snapshot: empty }
        ));
    }

    #[test]
    fn refreshed_home_catalog_updates_the_open_view_and_the_next_open() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.toggle_home();

        assert!(
            app.handle_internal_event_with_render_impact(AppEvent::HomeCatalogRefreshed {
                catalog: codex_catalog("refreshed-model"),
            })
        );
        let home = app.state.home.as_mut().expect("open Home");
        home.set_agent(Agent::Codex);
        assert!(home
            .model_options()
            .iter()
            .any(|model| model.id == "refreshed-model"));

        app.state.toggle_home();
        app.state.toggle_home();
        let home = app.state.home.as_mut().expect("reopened Home");
        home.set_agent(Agent::Codex);
        assert!(home
            .model_options()
            .iter()
            .any(|model| model.id == "refreshed-model"));
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
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.set_server_mode(Mode::Terminal);
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
    fn contract_false_positive_appends_once_per_input_burst() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("contract");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();

        let reported_at = Instant::now();
        let tokens = std::collections::HashMap::from([
            ("closing_idle".to_string(), Some("1".to_string())),
            (
                "closing_contract".to_string(),
                Some("tests pass".to_string()),
            ),
            ("closing_contract_met".to_string(), Some("1".to_string())),
        ]);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.apply_closing_contract_tokens(&tokens, reported_at);
        terminal.metadata_tokens.patch(tokens, None, reported_at);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("session-1").unwrap(),
        });

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "herdr-contract-burst-{}-{unique}",
            std::process::id()
        ));
        let path = dir.join("contract-false-positives.jsonl");
        app.contract_false_positive_log_path_override = Some(path.clone());
        let input_at = reported_at + Duration::from_secs(4);

        app.begin_contract_false_positive_input_burst();
        app.record_contract_false_positive_for_pane(pane_id, input_at);
        app.record_contract_false_positive_for_pane(pane_id, input_at);
        app.begin_contract_false_positive_input_burst();
        app.record_contract_false_positive_for_pane(pane_id, input_at);

        let records = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        for record in records {
            assert_eq!(record["pane_id"], app.public_pane_id(0, pane_id).unwrap());
            assert_eq!(record["session_id"], "session-1");
            assert_eq!(record["contract"], "tests pass");
            assert_eq!(record["age_s"], 4);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn loop_run_history_read_does_not_change_the_detail_surface() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.show_loop_run_history(
            "already-open".into(),
            crate::loop_runs::RunHistory::default(),
            std::time::SystemTime::UNIX_EPOCH,
        );
        let before = app.state.loop_run_history_detail.clone();

        app.dispatch_api_request(
            "read",
            crate::api::schema::Method::LoopRunHistory(crate::api::schema::LoopRunHistoryParams {
                loop_id: Some("requested".into()),
            }),
        );

        assert_eq!(app.state.loop_run_history_detail, before);
    }

    #[test]
    fn loop_receipt_change_event_refreshes_cache_and_publishes_update() {
        let path = std::env::temp_dir().join(format!(
            "herdr-loop-runs-app-refresh-{}.jsonl",
            crate::config::test_unique_suffix()
        ));
        std::fs::write(
            &path,
            b"{\"event\":\"start\",\"run_id\":\"refreshed\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\"}\n",
        )
        .expect("write temporary receipt fixture");
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let event_hub = crate::api::EventHub::default();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.loop_history_reader = Some(crate::loop_runs::ReceiptReader::new(path.clone()));
        let sequence = event_hub.current_sequence();

        assert!(app.handle_internal_event_with_render_impact(AppEvent::LoopRunHistoryChanged));
        assert_eq!(app.state.loop_run_history.runs.len(), 1);
        assert_eq!(event_hub.events_after(sequence).len(), 1);

        std::fs::remove_file(path).expect("remove temporary receipt fixture");
    }

    #[test]
    fn loop_history_refreshes_when_receipt_watcher_is_unavailable() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "herdr-loop-runs-app-fallback-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temporary receipt directory");
        let path = dir.join("run-receipts.jsonl");
        std::fs::write(
            &path,
            b"{\"event\":\"start\",\"run_id\":\"fallback\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\"}\n",
        )
        .expect("write temporary receipt fixture");
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.loop_history_reader = Some(crate::loop_runs::ReceiptReader::new(path));
        app._loop_receipt_watcher = None;
        app.loop_receipt_fallback_deadline = Some(std::time::Instant::now());

        assert!(app.handle_scheduled_tasks(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            false,
        ));
        assert_eq!(app.state.loop_run_history.runs.len(), 1);
        assert_eq!(app.state.loop_run_history.runs[0].run_id, "fallback");
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("low-frequency fallback refresh")));

        std::fs::remove_dir_all(dir).expect("remove temporary receipt directory");
    }

    #[tokio::test]
    async fn status_focus_transition_uses_cached_cwd_without_runtime_queries() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("status-focus");
        let root = workspace.tabs[0].root_pane;
        let nested = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(root);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.status_bar_enabled = true;
        let root_terminal = app.state.workspaces[0]
            .terminal_id(root)
            .expect("root terminal")
            .clone();
        let nested_terminal = app.state.workspaces[0]
            .terminal_id(nested)
            .expect("nested terminal")
            .clone();
        app.state.terminals.get_mut(&root_terminal).unwrap().cwd = "/repo".into();
        app.state.terminals.get_mut(&nested_terminal).unwrap().cwd = "/repo/nested".into();
        let (runtime, _input_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(nested_terminal.clone(), runtime);
        app.last_focus = Some((0, root));
        app.state.status_focused_cwd = Some("/repo".into());
        app.state.status_git_cwd = Some("/repo".into());
        app.state.status_git_branch = Some("root-branch".into());

        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();
        assert!(app.state.focus_pane_in_workspace(0, nested));
        app.sync_focus_events();

        assert_eq!(
            app.state.status_focused_cwd,
            Some(std::path::PathBuf::from("/repo/nested"))
        );
        assert_eq!(app.state.status_git_cwd, None);
        assert_eq!(app.state.status_git_branch, None);
        assert!(app.git_identity_refresh_requested);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/repo/nested")));
        assert_eq!(branch, None);

        app.state.terminals.remove(&root_terminal);
        app.state.status_focused_cwd = Some("/repo/nested".into());
        app.state.status_git_cwd = Some("/repo/nested".into());
        app.state.status_git_branch = Some("nested-branch".into());
        assert!(app.state.focus_pane_in_workspace(0, root));
        app.sync_focus_events();

        assert_eq!(app.state.status_focused_cwd, None);
        assert_eq!(app.state.status_git_cwd, None);
        assert_eq!(app.state.status_git_branch, None);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, None);
        assert_eq!(branch, None);
    }

    #[tokio::test]
    async fn focused_cwd_report_clears_stale_branch_while_refresh_is_in_flight() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("status-cwd-report");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.status_bar_enabled = true;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("focused terminal")
            .clone();
        let old_cwd = std::env::current_dir().expect("current directory");
        let new_cwd = std::env::temp_dir().join(format!(
            "herdr-status-cwd-report-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&new_cwd).expect("create reported cwd");
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = old_cwd.clone();
        app.state.status_focused_cwd = Some(old_cwd.clone());
        app.state.status_git_cwd = Some(old_cwd);
        app.state.status_git_branch = Some("stale-branch".into());
        app.state.status_focus_projection_initialized = true;
        app.test_begin_git_refresh(1);

        app.handle_internal_event(AppEvent::TerminalCwdReported {
            pane_id,
            cwd: new_cwd.clone(),
        });

        assert_eq!(app.state.status_focused_cwd, Some(new_cwd.clone()));
        assert_eq!(app.state.status_git_cwd, None);
        assert_eq!(app.state.status_git_branch, None);
        assert!(app.git_identity_refresh_requested);
        assert!(app.git_refresh_due_after_in_flight);
        let _ = std::fs::remove_dir_all(new_cwd);
    }

    #[tokio::test]
    async fn disabled_status_focus_transition_survives_reenable() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("disabled-status-focus");
        let root = workspace.tabs[0].root_pane;
        let nested = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(root);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let root_terminal = app.state.workspaces[0]
            .terminal_id(root)
            .expect("root terminal")
            .clone();
        let nested_terminal = app.state.workspaces[0]
            .terminal_id(nested)
            .expect("nested terminal")
            .clone();
        app.state.terminals.get_mut(&root_terminal).unwrap().cwd = "/repo".into();
        app.state.terminals.get_mut(&nested_terminal).unwrap().cwd = "/repo/nested".into();
        let (runtime, _input_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(nested_terminal.clone(), runtime);
        app.state.status_bar_enabled = true;
        app.state.sync_status_focused_cached_cwd();
        app.state.status_git_cwd = Some("/repo".into());
        app.state.status_git_branch = Some("root-branch".into());
        app.last_focus = Some((0, root));

        app.state.status_bar_enabled = false;
        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();
        assert!(app.state.focus_pane_in_workspace(0, nested));
        app.sync_focus_events();

        assert!(!app.git_identity_refresh_requested);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
        app.state.status_bar_enabled = true;
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/repo/nested")));
        assert_eq!(branch, None);
    }

    #[tokio::test]
    async fn manifest_activation_event_resets_matching_agent_detection_runtime() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
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
    fn theme_api_status_and_set_round_trip_server_owned_appearance() {
        let mut config = crate::config::Config::default();
        config.theme.auto_switch = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true);

        let status = app.handle_api_request(crate::api::schema::Request {
            id: "theme_status".into(),
            method: crate::api::schema::Method::ThemeStatus(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let status: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status["result"]["host_reported"], "light");
        assert_eq!(status["result"]["override"], "auto");
        assert_eq!(status["result"]["effective_appearance"], "light");
        assert_eq!(status["result"]["theme_name"], "catppuccin-latte");

        let set = app.handle_api_request(crate::api::schema::Request {
            id: "theme_set".into(),
            method: crate::api::schema::Method::ThemeSet(crate::api::schema::ThemeSetParams {
                host_appearance: crate::config::HostAppearanceOverride::Dark,
            }),
        });
        let set: serde_json::Value = serde_json::from_str(&set).unwrap();
        assert_eq!(set["result"]["host_reported"], "light");
        assert_eq!(set["result"]["override"], "dark");
        assert_eq!(set["result"]["effective_appearance"], "dark");
        assert_eq!(set["result"]["theme_name"], "catppuccin");
    }

    #[tokio::test]
    async fn server_reload_agent_manifests_resets_detection_runtimes() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
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
        let reset_notify = runtime.agent_detection_reset_notify_for_test();
        app.terminal_runtimes.insert(terminal_id, runtime);

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
            true,
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
        // The assertion names a rule from the bundled codex manifest, so the
        // cache has to be pinned to the bundled manifests; otherwise a manifest
        // downloaded into the developer's state directory answers instead.
        crate::detect::manifest::with_bundled_manifests("agent-explain-cache", || {
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &crate::config::Config::default(),
                true,
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
                b"press enter to confirm or esc to cancel",
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

            assert_eq!(response["result"]["type"], "agent_explain");
            assert_eq!(response["result"]["explain"]["screen_state"], "blocked");
            assert_eq!(response["result"]["explain"]["state"], "unknown");
            assert_eq!(response["result"]["explain"]["effective_state"], "unknown");
            assert_eq!(response["result"]["explain"]["arbitration"], "screen");
            assert_eq!(
                response["result"]["explain"]["matched_rule"]["id"],
                "live_strong_blocker"
            );
        });
    }

    #[tokio::test]
    async fn agent_explain_rejects_hook_only_full_lifecycle_authority() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
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
            true,
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
        assert!(response["result"]["process_info"]["tty"].is_null());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pane_process_info_keeps_recorded_tty_after_shell_exit() {
        use std::path::Path;
        use std::thread;
        use std::time::{Duration, Instant};

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("process-info-exit")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane_id,
            24,
            80,
            std::env::temp_dir(),
            &["/bin/sh".into(), "-c".into(), "sleep 0.2".into()],
            &crate::pane::PaneLaunchEnv::default(),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("spawn test pane");
        let shell_pid = runtime.child_pid().expect("test shell pid");
        let recorded_tty = runtime
            .tty_name()
            .map(Path::to_path_buf)
            .expect("recorded pane tty");
        let deadline = Instant::now() + Duration::from_secs(2);
        let shell_stdin = loop {
            if let Ok(path) = std::fs::read_link(format!("/proc/{shell_pid}/fd/0")) {
                break path;
            }
            assert!(Instant::now() < deadline, "test shell did not expose stdin");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            shell_stdin, recorded_tty,
            "recorded pane tty must match the live shell device"
        );
        app.terminal_runtimes.insert(terminal_id, runtime);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Path::new(&format!("/proc/{shell_pid}")).exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !Path::new(&format!("/proc/{shell_pid}")).exists(),
            "test shell did not exit"
        );

        let target = app.public_pane_id(0, pane_id).expect("public pane id");
        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "process_info_after_exit".into(),
                method: crate::api::schema::Method::PaneProcessInfo(
                    crate::api::schema::PaneProcessInfoParams {
                        pane_id: Some(target),
                    },
                ),
            });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["result"]["type"], "pane_process_info");
        assert_eq!(
            response["result"]["process_info"]["tty"],
            recorded_tty.display().to_string()
        );

        test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pane_process_info_reports_new_tty_after_runtime_respawn() {
        use std::path::Path;

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new(
            "process-info-respawn",
        )];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .respawn_shell_on_exit = true;
        let old_runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane_id,
            24,
            80,
            std::env::temp_dir(),
            &["/bin/sh".into(), "-c".into(), "sleep 30".into()],
            &crate::pane::PaneLaunchEnv::default(),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("spawn original pane");
        let old_tty = old_runtime
            .tty_name()
            .map(Path::to_path_buf)
            .expect("original pane tty");
        app.terminal_runtimes
            .insert(terminal_id.clone(), old_runtime);

        app.handle_internal_event(AppEvent::PaneDied { pane_id });

        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("respawned runtime");
        let shell_pid = runtime.child_pid().expect("respawned shell pid");
        let new_tty = runtime
            .tty_name()
            .map(Path::to_path_buf)
            .expect("respawned pane tty");
        assert_ne!(new_tty, old_tty, "respawn must allocate a fresh PTY");
        assert_eq!(
            std::fs::read_link(format!("/proc/{shell_pid}/fd/0")).expect("respawned shell stdin"),
            new_tty
        );

        let target = app.public_pane_id(0, pane_id).expect("public pane id");
        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "process_info_after_respawn".into(),
                method: crate::api::schema::Method::PaneProcessInfo(
                    crate::api::schema::PaneProcessInfoParams {
                        pane_id: Some(target),
                    },
                ),
            });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["result"]["type"], "pane_process_info");
        assert_eq!(
            response["result"]["process_info"]["tty"],
            new_tty.display().to_string()
        );

        test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn client_window_title_api_reports_no_foreground_client_in_app_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
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
            true,
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
        app.state.set_server_mode(Mode::Terminal);
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
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        app.handle_internal_event(AppEvent::StateChanged {
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
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
            true,
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
        app.state.set_server_mode(Mode::Terminal);
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
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        app.handle_internal_event(AppEvent::StateChanged {
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let notification_deadline = app
            .state
            .next_pending_agent_notification_deadline()
            .expect("pending notification deadline");
        assert!(app.handle_scheduled_tasks(notification_deadline, false));
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
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
            true,
            None,
            api_rx,
            event_hub.clone(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("pane-exit-layout");
        let dead_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let tab_id = app.public_tab_id(0, 0).unwrap();

        app.handle_internal_event(AppEvent::PaneDied { pane_id: dead_pane });

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
                true,
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
                pane_id,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                usage_limited: false,
                process_exited: true,
                observed_at: std::time::Instant::now(),
            });

            assert!(app.state.terminals[&terminal_id].agent_name.is_none());
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
            true,
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
            pane_id,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: true,
            observed_at,
        });

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.raw_agent_state(), AgentState::Idle);
        assert!(terminal.agent_name.is_none());
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
            true,
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
        app.state.set_server_mode(Mode::Terminal);
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
        app.state.set_server_mode(Mode::Prefix);

        app.handle_internal_event(AppEvent::PaneDied {
            pane_id: overlay_pane,
        });

        let tab = &app.state.workspaces[0].tabs[0];
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(tab.layout.focused(), previous_focus);
        assert!(!tab.zoomed);
        assert!(app.overlay_panes.is_empty());
        assert_eq!(app.state.server_mode(), Mode::Prefix);
        assert!(app.take_pending_client_pane_focus());
    }

    #[tokio::test]
    async fn pane_died_respawns_shell_and_clears_restored_agent_session() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = test_support::exiting_test_command().into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
        let workspace = crate::workspace::Workspace::test_new("restored");
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
        terminal.replace_prevalidated_manual_work_context(
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/o/r/pull/42".into()],
                role: Some(crate::work_context::PaneWorkRole::Ship),
                active_owner: true,
                ..Default::default()
            }
            .normalized_spawn_binding()
            .unwrap(),
        );

        app.handle_internal_event(AppEvent::PaneDied { pane_id });

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
        assert_eq!(
            terminal.effective_work_context().primary_pr(),
            Some("https://github.com/o/r/pull/42")
        );
        assert_eq!(
            terminal.effective_work_context().role,
            Some(crate::work_context::PaneWorkRole::Ship)
        );
        assert!(!terminal.effective_work_context().active_owner);

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
            true,
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
            pane_id,
            agent: Some(crate::detect::Agent::OpenCode),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
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
            true,
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
    fn terminal_delivery_does_not_refresh_existing_targeted_toast() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.local_terminal_notifications = false;

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
        app.state.set_server_mode(Mode::Terminal);
        app.state.toast_config.delivery = crate::config::ToastDelivery::Terminal;

        app.handle_internal_event(AppEvent::StateChanged {
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
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
            pane_id: root,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("__herdr_original__ · 1")
        );
    }
}
