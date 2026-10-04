use std::{sync::Arc, time::Instant};

#[cfg(test)]
use std::time::Duration;

use crossterm::terminal;

use super::{
    background_update_check_enabled, App, AUTO_UPDATE_CHECK_INTERVAL, MIN_RENDER_INTERVAL,
    RESIZE_POLL_INTERVAL, SELECTION_AUTOSCROLL_INTERVAL,
};

fn provider_usage_disabled_for_test_execution() -> bool {
    !cfg!(test) && std::env::var_os("NEXTEST").is_some()
}
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

fn retain_custom_command_after_wait(
    pid: u32,
    result: std::io::Result<Option<std::process::ExitStatus>>,
) -> bool {
    match result {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
        Err(err) => {
            tracing::warn!(pid, err = %err, "failed to reap detached custom command");
            false
        }
    }
}

impl App {
    fn local_pomodoro_presentation_for_input(&mut self) -> crate::ui::pomodoro::InputPresentation {
        let presentation = self.local_pomodoro_presentation.map_or_else(
            || {
                crate::ui::pomodoro::input_presentation_at(
                    &self.state,
                    self.state.screen_rect(),
                    Instant::now(),
                )
            },
            |gate| crate::ui::pomodoro::input_gate_at(&self.state, gate, Instant::now()),
        );
        self.local_pomodoro_presentation = Some(presentation);
        presentation
    }

    pub(crate) fn reap_finished_custom_commands(&mut self) {
        self.detached_custom_command_children
            .retain_mut(|child| retain_custom_command_after_wait(child.id(), child.try_wait()));
    }

    pub(crate) fn reap_finished_detached_processes(&mut self) {
        self.detached_process_children
            .retain_mut(|child| retain_detached_process_after_wait(child.id(), child.try_wait()));
    }

    pub(crate) fn shutdown_terminal_runtime(&mut self, terminal_id: crate::terminal::TerminalId) {
        let target = super::TerminalInputTarget {
            terminal_id: terminal_id.clone(),
        };
        self.release_input_target_headless(&target);
        // A proxy pane's shutdown releases its remote focus lease exactly
        // once, on whichever close path removed the pane.
        self.detach_remote_proxy_for_terminal(&terminal_id);
        if let Some(runtime) = self.terminal_runtimes.remove(&terminal_id) {
            runtime.shutdown();
        }
    }

    pub(crate) fn shutdown_detached_terminal_runtimes(&mut self) {
        let terminal_ids = std::mem::take(&mut self.state.terminal_runtime_shutdowns);
        for terminal_id in terminal_ids {
            self.shutdown_terminal_runtime(terminal_id);
        }
    }

    pub(crate) fn drain_api_requests(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.api_rx.try_recv() {
            changed |= self.handle_api_request_message(msg);
            self.shutdown_detached_terminal_runtimes();
        }
        changed
    }

    pub(super) fn handle_api_request_message(
        &mut self,
        msg: crate::api::ApiRequestMessage,
    ) -> bool {
        let previous_mode = self.state.effective_interaction_mode();
        let stream_open = match &msg.request.method {
            crate::api::schema::Method::PaneGraphicsStreamOpen(params) => Some(params.clone()),
            _ => None,
        };
        let stream_active = msg.stream_active.clone();
        let mut changed = self.expire_due_metadata(Instant::now());
        changed |= crate::api::request_changes_ui(&msg.request);
        let skip_default_workspace = matches!(
            &msg.request.method,
            crate::api::schema::Method::ServerStop(_)
                | crate::api::schema::Method::ServerLiveHandoff(_)
        );
        if matches!(
            &msg.request.method,
            crate::api::schema::Method::WorktreeCreate(_)
                | crate::api::schema::Method::WorktreeRemove(_)
        ) {
            self.drain_all_internal_events();
            let deferred_changed =
                self.handle_deferred_worktree_api_request(msg.request, msg.respond_to);
            if !skip_default_workspace {
                changed |= self.ensure_default_workspace();
            }
            self.sync_prefix_input_source(previous_mode);
            return changed | deferred_changed;
        }
        if matches!(
            &msg.request.method,
            crate::api::schema::Method::AgentSearch(params) if params.include_session
        ) {
            self.drain_all_internal_events();
            self.start_deferred_agent_search(msg.request, msg.respond_to, stream_active);
            if !skip_default_workspace {
                changed |= self.ensure_default_workspace();
            }
            self.sync_prefix_input_source(previous_mode);
            return changed;
        }
        if self.should_defer_group_api_request(&msg.request) {
            self.drain_all_internal_events();
            self.start_deferred_group_api_request(msg.request, msg.respond_to);
            if !skip_default_workspace {
                changed |= self.ensure_default_workspace();
            }
            self.sync_prefix_input_source(previous_mode);
            return changed;
        }
        let response = self.handle_api_request(msg.request);
        if let (Some(params), Some(active)) = (stream_open.as_ref(), stream_active) {
            self.attach_pane_graphics_stream_active(params, active, &response);
        }
        if !skip_default_workspace {
            changed |= self.ensure_default_workspace();
        }
        let _ = msg.respond_to.send(response);
        self.sync_prefix_input_source(previous_mode);
        changed
    }

    pub(super) async fn handle_raw_input_batch(
        &mut self,
        first: crate::raw_input::RawInputEvent,
    ) -> bool {
        self.begin_contract_false_positive_input_burst();
        let mut pomodoro_presentation = self.local_pomodoro_presentation_for_input();
        let mut changed = self
            .handle_raw_input_event_with_pomodoro_presentation(first, pomodoro_presentation)
            .await;
        pomodoro_presentation =
            crate::ui::pomodoro::input_gate_at(&self.state, pomodoro_presentation, Instant::now());
        self.local_pomodoro_presentation = Some(pomodoro_presentation);

        while let Some(rx) = self.input_rx.as_mut() {
            match rx.try_recv() {
                Ok(event) => {
                    changed |= self
                        .handle_raw_input_event_with_pomodoro_presentation(
                            event,
                            pomodoro_presentation,
                        )
                        .await;
                    pomodoro_presentation = crate::ui::pomodoro::input_gate_at(
                        &self.state,
                        pomodoro_presentation,
                        Instant::now(),
                    );
                    self.local_pomodoro_presentation = Some(pomodoro_presentation);
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.input_rx = None;
                    break;
                }
            }
        }

        changed
    }

    async fn execute_repeat_plan(
        &mut self,
        lease_key: super::input::InputLeaseKey,
        key: crate::input::TerminalKey,
        plan: super::input::RepeatPlan,
        owner: super::state::InputOwner,
    ) -> bool {
        match plan {
            super::input::RepeatPlan::Forwarded(target) => {
                if !self.forward_terminal_key_to_target(&target, key).await {
                    self.input_leases.remove(&lease_key);
                }
                true
            }
            super::input::RepeatPlan::Reprocess {
                context,
                repetitions,
                tracked,
            } => {
                let key = key
                    .with_kind(crossterm::event::KeyEventKind::Repeat)
                    .with_repeat_count(1);
                let mut forwarded_target = None;
                for _ in 0..repetitions {
                    if let Some(target) = &forwarded_target {
                        if !self
                            .forward_terminal_key_to_target(target, key.clone())
                            .await
                        {
                            self.input_leases.remove(&lease_key);
                            break;
                        }
                        continue;
                    }
                    let current_context = self.terminal_input_context();
                    if !self.input_leases.reprocess_allowed(
                        lease_key,
                        &context,
                        current_context.as_ref(),
                        tracked,
                    ) {
                        break;
                    }
                    if let Some(target) = self.handle_key_for_input_owner(key.clone(), owner).await
                    {
                        if tracked {
                            self.input_leases.insert_forwarded(
                                lease_key,
                                target.clone(),
                                key.clone(),
                            );
                            forwarded_target = Some(target);
                        }
                    }
                }
                true
            }
            super::input::RepeatPlan::Ignore => false,
        }
    }

    #[cfg(test)]
    pub(super) async fn handle_raw_input_event(
        &mut self,
        event: crate::raw_input::RawInputEvent,
    ) -> bool {
        let ends_input_lifecycle = matches!(
            &event,
            crate::raw_input::RawInputEvent::Key(key)
                if key.kind == crossterm::event::KeyEventKind::Release
        ) || matches!(
            &event,
            crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Up(_),
                ..
            })
        );
        let mut pomodoro_presentation = self.local_pomodoro_presentation_for_input();
        if pomodoro_presentation.prompt.is_none() && self.state.pomodoro.prompt.is_some() {
            pomodoro_presentation.prompt = Some(self.state.screen_rect());
        }
        let changed = self
            .handle_raw_input_event_with_pomodoro_presentation(event, pomodoro_presentation)
            .await;
        self.local_pomodoro_presentation = if ends_input_lifecycle {
            None
        } else {
            Some(crate::ui::pomodoro::input_gate_at(
                &self.state,
                pomodoro_presentation,
                Instant::now(),
            ))
        };
        changed
    }

    async fn handle_raw_input_event_with_pomodoro_presentation(
        &mut self,
        event: crate::raw_input::RawInputEvent,
        pomodoro_presentation: crate::ui::pomodoro::InputPresentation,
    ) -> bool {
        if !matches!(&event, crate::raw_input::RawInputEvent::Key(_)) {
            self.invalidate_window_cycle_snapshot();
        }
        if !matches!(&event, crate::raw_input::RawInputEvent::Key(_))
            && self.intercept_pomodoro_send_off_raw_input_with_visibility(
                super::LOCAL_INPUT_SOURCE,
                &event,
                pomodoro_presentation.send_off.is_some(),
            )
        {
            return true;
        }
        let previous_mode = self.state.effective_interaction_mode();
        let owner = self
            .state
            .input_owner_with_pomodoro(pomodoro_presentation.prompt.is_some());
        let changed = match event {
            crate::raw_input::RawInputEvent::Key(key) => {
                let lease_key = super::input::InputLeaseKey::new(super::LOCAL_INPUT_SOURCE, &key);
                let key = self.input_leases.normalize_press(&lease_key, key);
                let normalized_event = crate::raw_input::RawInputEvent::Key(key.clone());
                if !self.input_leases.contains(&lease_key)
                    && self.intercept_pomodoro_send_off_raw_input_with_visibility(
                        super::LOCAL_INPUT_SOURCE,
                        &normalized_event,
                        pomodoro_presentation.send_off.is_some(),
                    )
                {
                    if key.kind != crossterm::event::KeyEventKind::Release {
                        self.input_leases.insert_consumed(
                            lease_key,
                            super::input::ConsumedInputLease::SuppressRepeats,
                        );
                    }
                    return true;
                }
                match key.kind {
                    crossterm::event::KeyEventKind::Press => {
                        let terminal_owner = owner.forwards_unhandled_input_to_pane()
                            || matches!(
                                owner,
                                super::state::InputOwner::Popup
                                    | super::state::InputOwner::Dock(
                                        super::state::DockInputOwner::Editor
                                    )
                            );
                        let initial_context = terminal_owner
                            .then(|| self.terminal_input_context())
                            .flatten();
                        let proxy_input_gate_closed =
                            terminal_owner && self.focused_remote_proxy_input_gate_closed();
                        let target = self.handle_key_for_input_owner(key.clone(), owner).await;
                        let resulting_context = terminal_owner
                            .then(|| self.terminal_input_context())
                            .flatten();
                        let plan = self.input_leases.complete_press_with_reprocess(
                            lease_key,
                            &key,
                            initial_context.as_ref(),
                            resulting_context.as_ref(),
                            target,
                            !proxy_input_gate_closed,
                        );
                        self.execute_repeat_plan(lease_key, key, plan, owner).await;
                        true
                    }
                    crossterm::event::KeyEventKind::Repeat => {
                        let current_context = (owner.forwards_unhandled_input_to_pane()
                            || matches!(
                                owner,
                                super::state::InputOwner::Popup
                                    | super::state::InputOwner::Dock(
                                        super::state::DockInputOwner::Editor
                                    )
                            ))
                        .then(|| self.terminal_input_context())
                        .flatten();
                        let plan = self.input_leases.plan_repeat(
                            lease_key,
                            &key,
                            current_context.as_ref(),
                        );
                        self.execute_repeat_plan(lease_key, key, plan, owner).await
                    }
                    crossterm::event::KeyEventKind::Release => {
                        if let Some(lease) = self.input_leases.remove_forwarded(&lease_key) {
                            let release = key.with_windows_composition_from(&lease.key);
                            let _ = self
                                .forward_terminal_key_to_target(&lease.target, release)
                                .await;
                        }
                        false
                    }
                }
            }
            crate::raw_input::RawInputEvent::Text(text) => {
                if pomodoro_presentation.prompt.is_some() {
                    return true;
                }
                self.handle_text_commit_for_input_owner(owner, text.into_string())
                    .await;
                true
            }
            crate::raw_input::RawInputEvent::Paste(text) => {
                if pomodoro_presentation.prompt.is_some() {
                    return true;
                }
                self.handle_paste_for_input_owner(owner, text).await;
                true
            }
            crate::raw_input::RawInputEvent::Mouse(mouse) => {
                let previous_hover = self.state.hovered_control;
                self.handle_mouse_for_input_owner(
                    super::LOCAL_INPUT_SOURCE,
                    mouse,
                    pomodoro_presentation,
                    owner,
                );
                !matches!(mouse.kind, crossterm::event::MouseEventKind::Moved)
                    || self
                        .state
                        .client_presentation_policy()
                        .mouse_motion_changes_view()
                    || self.state.hovered_control != previous_hover
            }
            crate::raw_input::RawInputEvent::OuterFocusGained => {
                #[cfg(not(windows))]
                self.query_host_terminal_appearance();
                if self.state.symphony_detail.is_none() && self.state.work_view.is_none() {
                    self.send_outer_focus_event(crate::ghostty::FocusEvent::Gained);
                }
                if self.state.redraw_on_focus_gained {
                    self.request_repaint();
                }
                self.state.outer_terminal_focus = Some(true);
                self.state.pomodoro.resume_held(std::time::Instant::now());
                self.state.mark_active_pane_seen();
                true
            }
            crate::raw_input::RawInputEvent::OuterFocusLost => {
                self.release_input_source(super::LOCAL_INPUT_SOURCE).await;
                if self.state.symphony_detail.is_none() && self.state.work_view.is_none() {
                    self.send_outer_focus_event(crate::ghostty::FocusEvent::Lost);
                }
                self.state.outer_terminal_focus = Some(false);
                false
            }
            crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                self.update_host_terminal_theme(kind, color)
            }
            crate::raw_input::RawInputEvent::HostPaletteColors { colors } => {
                self.update_host_terminal_palette_colors(&colors)
            }
            crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                self.query_host_terminal_theme();
                self.set_host_terminal_appearance(appearance, true)
            }
            // Cell size reports are consumed by the thin client, not the runtime.
            crate::raw_input::RawInputEvent::HostCellSizeReport { .. } => false,
            crate::raw_input::RawInputEvent::Unsupported => false,
        };
        self.sync_prefix_input_source(previous_mode);
        self.shutdown_detached_terminal_runtimes();
        changed
    }

    fn handle_resize_poll(&mut self) -> bool {
        let Ok(size) = terminal::size() else {
            return false;
        };
        if self.last_terminal_size != Some(size) {
            self.last_terminal_size = Some(size);
            return true;
        }
        false
    }

    /// Whether the idle animation is on screen and moving. A collapsed sidebar
    /// or a sidebar too short to hold the panel draws nothing, so it must not
    /// wake the loop either.
    fn sidebar_animation_visible(&self) -> bool {
        self.state.hyperspace.running()
            && !self.state.sidebar_collapsed
            && self.state.view.hyperspace_rect.height > 0
    }

    /// Advances the sidebar's idle animation. Returns whether the frame changed.
    pub(crate) fn tick_sidebar_animation(&mut self, now: Instant) -> bool {
        if !self.sidebar_animation_visible() {
            return false;
        }
        self.state.hyperspace.tick(now)
    }

    /// When the loop should wake for the animation's next frame. `None` while it
    /// is paused, disabled, or off screen, so a still sidebar costs nothing.
    fn sidebar_animation_deadline(&self) -> Option<Instant> {
        self.sidebar_animation_visible()
            .then(|| self.state.hyperspace.next_deadline())
            .flatten()
    }

    fn pomodoro_animation_due(&self, now: Instant) -> bool {
        crate::ui::pomodoro::animation_due_at(
            &self.state,
            self.state.screen_rect(),
            now,
            self.last_render_at,
        )
    }

    pub(crate) fn handle_scheduled_tasks(&mut self, now: Instant, geometry_dirty: bool) -> bool {
        let mut changed = self.take_due_agent_activity_refresh(now);
        changed |= self.handle_loop_receipt_fallback(now);
        changed |= self.tick_notepad(now);
        changed |= self.tick_scratch(now, false);
        changed |= self.schedule_goals_refresh(now);
        changed |= self.tick_pomodoro(now, self.state.outer_terminal_focus != Some(false));
        changed |= self.pomodoro_animation_due(now);
        changed |= self.tick_sidebar_animation(now);
        let mut resized = false;

        if now >= self.next_resize_poll {
            resized = self.handle_resize_poll();
            changed |= resized;
            self.next_resize_poll = now + RESIZE_POLL_INTERVAL;
        }
        changed |= self.schedule_status_metrics_after_resize_poll(now, resized);
        self.schedule_status_side_signals(now);

        if self
            .config_diagnostic_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.config_diagnostic_deadline = None;
            self.state.config_diagnostic = None;
            changed = true;
        }

        if self.toast_deadline.is_some_and(|deadline| now >= deadline) {
            self.toast_deadline = None;
            self.state.toast = None;
            changed = true;
        }

        if self
            .state
            .next_pending_agent_notification_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let previous_toast = self.state.toast.clone();
            let mut deliveries = self.state.drain_due_agent_notifications(now);
            if !deliveries.is_empty() {
                self.refresh_agent_notification_delivery_contexts(&mut deliveries);
                self.emit_delayed_client_local_agent_notifications(&deliveries);
                self.sync_toast_deadline(previous_toast);
                changed = true;
            }
        }

        if self
            .state
            .next_managed_agent_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let panes = self.state.reconcile_managed_agents_at(now);
            if !panes.is_empty() {
                for (ws_idx, pane_id) in panes {
                    self.emit_pane_updated(ws_idx, pane_id);
                }
                self.schedule_session_save();
                changed = true;
            }
        }

        if self
            .state
            .next_agent_watchdog_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            for update in self.state.mark_due_agent_status_stale_at(now) {
                self.emit_pane_state_update(&update);
                changed = true;
            }
            // The mark is what re-enables the process probe under a hook.
            self.sync_detection_authority_mirrors();
        }

        if self.state.done_hide_transition_due(now) {
            changed = true;
        }
        if self
            .state
            .next_done_reap_deadline(now)
            .is_some_and(|deadline| now >= deadline)
        {
            changed |= self.reap_due_done_panes(now);
        }

        if self
            .state
            .next_full_lifecycle_hook_authority_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let (updates, due) = self.state.expire_due_full_lifecycle_hook_authority_at(now);
            self.sync_detection_authority_mirrors();
            for update in &updates {
                self.emit_pane_state_update(update);
            }
            for (ws_idx, pane_id) in due {
                self.emit_pane_updated(ws_idx, pane_id);
                changed = true;
            }
        }

        if self
            .copy_feedback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.copy_feedback_deadline = None;
            self.state.copy_feedback = None;
            changed = true;
        }

        if self
            .selection_autoscroll_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.tick_selection_autoscroll(now);
            changed = true;
        }

        changed |= self.clear_due_selection_highlight(now);
        changed |= self.state.reveal_hover_tooltip_at(now);
        changed |= self.process_git_action_panes(now);

        changed |= self.refresh_pane_settlement_at(now);

        changed |= self.start_sidebar_refresh_if_requested(now);
        self.start_git_work_context_refresh_if_due(now);
        self.start_work_index_refresh_if_due(now);
        self.start_usage_scan_if_requested();
        let work_view_selection = self.state.work_view.as_ref().and_then(|view| {
            view.selected.clone().or_else(|| match view.projection {
                crate::app::state::WorkProjection::PullRequests => {
                    self.visible_pr_view_keys().into_iter().next()
                }
                crate::app::state::WorkProjection::Tickets => {
                    self.visible_ticket_view_keys().into_iter().next()
                }
                _ => None,
            })
        });
        let dock_pr_visible =
            !self.state.dock_collapsed && self.state.dock_tab == Some(crate::app::DockSurface::Pr);
        let dock_pr_selection = dock_pr_visible
            .then(|| crate::ui::dock::pr::focused_pr_key(&self.state))
            .flatten();
        // A collapsed dock still previews a ticket in the pane area, and that
        // host renders the same detail, so it counts as visible.
        let dock_linear_visible = if self.state.dock_collapsed {
            self.state
                .dock_object_preview
                .as_ref()
                .is_some_and(|object| object.surface == crate::app::DockSurface::Linear)
        } else {
            self.state.dock_tab == Some(crate::app::DockSurface::Linear)
        };
        let dock_linear_selection = dock_linear_visible
            .then(|| crate::ui::dock::linear::focused_ticket_key(&self.state))
            .flatten();
        let detail_visible = self.state.work_view.is_some()
            || dock_pr_visible
            || dock_linear_visible
            || !self.state.dock_collapsed
                && self.state.dock_tab == Some(crate::app::DockSurface::Home);
        self.start_work_item_detail_refresh_if_due(
            now,
            if dock_linear_visible
                || self.state.work_view.as_ref().is_some_and(|view| {
                    view.projection == crate::app::state::WorkProjection::Tickets
                })
            {
                crate::app::state::DockHomeSection::Tickets
            } else if self.state.work_view.is_some() || dock_pr_visible {
                crate::app::state::DockHomeSection::Prs
            } else {
                self.state.dock_home_section
            },
            work_view_selection
                .or(dock_pr_selection)
                .or(dock_linear_selection)
                .or_else(|| self.state.dock_home_active_selection()),
            detail_visible,
        );
        self.start_foreground_process_refresh_if_due(now);
        self.start_claude_subagent_refresh_if_due(now);
        self.observe_status_changes(now);
        self.start_git_status_refresh_if_due(now);
        changed |= self.finish_sidebar_refresh_if_idle();
        self.start_dock_diff_refresh_if_needed();
        self.start_dock_files_refresh_if_needed();
        self.start_home_ref_refresh_if_requested();
        self.start_home_github_refresh_if_requested();

        if self
            .next_auto_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.run_auto_update_check();
        }

        if self
            .next_agent_manifest_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.run_agent_manifest_update_check();
        }

        if self
            .session_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.start_background_session_save();
        }

        changed |= self.expire_due_metadata(now);
        changed |= self.handle_tab_bar_status_tasks(now);

        if geometry_dirty || resized {
            self.pending_agent_resume_deadline = None;
        } else {
            self.sync_pending_agent_resume_deadline(now);
            changed |= self.start_pending_agent_resumes_at(self.pending_agent_resume_due(now), now);
        }
        changed |= self.tick_resume_nudges(now);
        changed |= self.tick_auto_nudges(now);
        changed
    }

    pub(crate) fn handle_loop_receipt_fallback(&mut self, now: Instant) -> bool {
        let watcher_unavailable = self.loop_history_reader.is_some()
            && self._loop_receipt_watcher.is_none()
            && self.loop_receipt_fallback_deadline.is_some();
        let failure_detected = self.loop_receipt_watch_health.take_fallback_request();
        if !watcher_unavailable && !failure_detected {
            return false;
        }

        let mut changed = false;
        if !self.loop_receipt_watch_degraded {
            self.loop_receipt_watch_degraded = true;
            changed = true;
        }
        if self.loop_receipt_fallback_deadline.is_none() {
            self.loop_receipt_fallback_deadline = Some(now);
        }
        const WARNING: &str =
            "loop run history watcher unavailable; using low-frequency fallback refresh";
        if !self
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("loop run history watcher unavailable"))
        {
            self.state.config_diagnostic = Some(match self.state.config_diagnostic.take() {
                Some(message) => format!("{message}; {WARNING}"),
                None => WARNING.to_string(),
            });
            self.config_diagnostic_deadline = None;
            changed = true;
        }

        if self
            .loop_receipt_fallback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.loop_receipt_fallback_deadline = Some(now + super::LOOP_RECEIPT_FALLBACK_INTERVAL);
            changed |= self.refresh_loop_run_history();
        }
        changed
    }

    pub(crate) fn sync_agent_activity_refresh_deadline(&mut self, now: Instant) {
        self.agent_activity_refresh_deadline = self.state.next_agent_activity_age_change(now);
    }

    pub(crate) fn take_due_agent_activity_refresh(&mut self, now: Instant) -> bool {
        if self
            .agent_activity_refresh_deadline
            .is_none_or(|deadline| now < deadline)
        {
            return false;
        }
        self.agent_activity_refresh_deadline = None;
        true
    }

    fn schedule_status_metrics_after_resize_poll(&mut self, now: Instant, resized: bool) -> bool {
        if resized {
            // The previous frame no longer proves that the status row is
            // renderable. The next render restores visibility when appropriate.
            self.status_metrics_visible = false;
        }
        self.schedule_status_metrics(now)
    }

    /// Drop a snapshot that outlived its staleness window so the status row
    /// shows unavailable metrics instead of silently aging values.
    pub(crate) fn discard_stale_status_metrics(&mut self, now: Instant) -> bool {
        let stale = self
            .state
            .status_metrics
            .as_ref()
            .is_some_and(|snapshot| snapshot.is_stale_at(now));
        if stale {
            self.state.status_metrics = None;
        }
        stale
    }

    /// Refreshes account quota and reachability on their own slow cadences.
    ///
    /// Both read the world, so both run on background threads and reach the UI
    /// as events. Neither blocks a frame, and a failure leaves the previous
    /// snapshot standing rather than blanking the row.
    pub(crate) fn schedule_status_side_signals(&mut self, now: Instant) {
        // The same switch that silences native metric sampling silences these:
        // a test process must not spawn `kimi-usage` or open a socket to the
        // internet just because a frame was rendered.
        if !self.status_metric_refresh_enabled {
            return;
        }
        if self.state.status_bar_enabled
            || self.state.notepad.enabled
            || self
                .state
                .notepad
                .is_tab_visible(crate::notepad::NotepadTabTarget::Usage)
            || self.state.usage_view.is_some()
            || self.state.home.is_some()
        {
            self.schedule_provider_usage(now);
        }
        if self.state.status_bar_enabled {
            self.schedule_connectivity_probe(now);
        }
    }

    pub(crate) fn schedule_provider_usage(&mut self, now: Instant) {
        if !self.status_metric_refresh_enabled
            || self.provider_usage_in_flight
            || !crate::provider_usage::snapshot_is_due(self.provider_usage_refreshed_at, now)
        {
            return;
        }
        if cfg!(test) {
            // Tests observe scheduling state without reading provider files or
            // spawning the external Kimi helper.
            self.provider_usage_in_flight = true;
            self.provider_usage_refreshed_at = Some(now);
            return;
        }
        // Integration tests execute the normal Herdr binary, so cfg!(test) is
        // false there. Nextest exports this marker to each test and its child
        // processes; keep those binaries from launching real provider tools.
        if provider_usage_disabled_for_test_execution() {
            return;
        }
        let tx = self.event_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("herdr-provider-usage".into())
            .spawn(move || {
                let snapshot = crate::provider_usage::collect(
                    crate::provider_usage::now_unix(),
                    Instant::now(),
                );
                let _ = tx.blocking_send(crate::events::AppEvent::ProviderUsageRefreshed {
                    snapshot: Box::new(snapshot),
                });
            })
            .is_ok();
        self.provider_usage_in_flight = spawned;
        if spawned {
            self.provider_usage_refreshed_at = Some(now);
        }
    }

    pub(crate) fn start_usage_scan_if_requested(&mut self) {
        if !self.state.request_usage_scan {
            return;
        }
        self.start_usage_scan();
    }

    pub(crate) fn start_usage_scan(&mut self) {
        self.state.request_usage_scan = false;
        self.usage_scan_generation = self.usage_scan_generation.wrapping_add(1);
        let generation = self.usage_scan_generation;
        self.usage_scan_in_flight = Some(generation);
        if let Some(view) = self.state.usage_view.as_mut() {
            view.scanning = true;
        }
        if cfg!(test) {
            return;
        }
        let tx = self.event_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("herdr-usage-scan".into())
            .spawn(move || {
                let result = crate::provider_usage::scan_historical_usage()
                    .map(Box::new)
                    .map_err(|error| error.to_string());
                let _ = tx.blocking_send(crate::events::AppEvent::UsageScanFinished {
                    generation,
                    result,
                });
            })
            .is_ok();
        if !spawned {
            self.usage_scan_in_flight = None;
            if let Some(view) = self.state.usage_view.as_mut() {
                view.scanning = false;
            }
        }
    }

    pub(crate) fn handle_usage_scan_finished(
        &mut self,
        generation: u64,
        result: Result<Box<crate::provider_usage::UsageSnapshot>, String>,
    ) -> bool {
        if self.usage_scan_in_flight != Some(generation) {
            return false;
        }
        self.usage_scan_in_flight = None;
        self.state.status_now_unix = crate::provider_usage::now_unix();
        let mut changed = false;
        if let Ok(snapshot) = result {
            changed = self.state.usage_snapshot.as_ref() != Some(snapshot.as_ref());
            self.state.usage_snapshot = Some((*snapshot).clone());
            if let Some(view) = self.state.usage_view.as_mut() {
                view.snapshot = Some(*snapshot);
                view.scanning = false;
                changed = true;
            }
        } else if let Some(view) = self.state.usage_view.as_mut() {
            view.scanning = false;
            changed = true;
        }
        changed
    }

    fn schedule_connectivity_probe(&mut self, now: Instant) {
        if self.connectivity_probe_in_flight
            || self.connectivity_probed_at.is_some_and(|last| {
                now.checked_duration_since(last)
                    .is_some_and(|elapsed| elapsed < crate::connectivity::PROBE_INTERVAL)
            })
        {
            return;
        }
        if cfg!(test) {
            // Tests record the probe cadence without opening a network socket.
            self.connectivity_probe_in_flight = true;
            self.connectivity_probed_at = Some(now);
            return;
        }
        let tx = self.event_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("herdr-connectivity".into())
            .spawn(move || {
                let reachable = crate::connectivity::probe();
                let _ = tx.blocking_send(crate::events::AppEvent::ConnectivityProbed { reachable });
            })
            .is_ok();
        self.connectivity_probe_in_flight = spawned;
        if spawned {
            self.connectivity_probed_at = Some(now);
        }
    }

    pub(crate) fn schedule_status_metrics(&mut self, now: Instant) -> bool {
        let stale = self.discard_stale_status_metrics(now);
        if !self.status_metric_refresh_enabled
            || !self.status_metrics_visible
            || !self.state.status_bar_enabled
            || !self.status_metric_refresh.begin(now)
        {
            return stale;
        }

        let tx = self.event_tx.clone();
        let sampler = Arc::clone(&self.status_metric_sampler);
        let spawn_result = std::thread::Builder::new()
            .name("herdr-status-metrics".into())
            .spawn(move || {
                let snapshot = std::panic::catch_unwind(|| {
                    // The sampler only carries the previous CPU delta,
                    // so a poisoned lock stays usable; recovering keeps a single
                    // collector panic from disabling metrics for the session.
                    let mut sampler = sampler
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    Box::new(crate::platform::status_metrics::StatusMetricsSnapshot {
                        metrics: crate::platform::sample_status_metrics(&mut sampler),
                        sampled_at: Instant::now(),
                    })
                })
                .inspect_err(|_| {
                    tracing::warn!("status metric collection panicked; retrying next interval");
                })
                .ok();
                let _ =
                    tx.blocking_send(crate::events::AppEvent::StatusMetricsRefreshed { snapshot });
            });
        if spawn_result.is_err() {
            self.status_metric_refresh.finish();
        }
        stale
    }

    /// Clears temporary copied-token highlights, such as after double-click copy.
    pub(crate) fn clear_due_selection_highlight(&mut self, now: Instant) -> bool {
        if self
            .selection_highlight_clear_deadline
            .is_none_or(|deadline| now < deadline)
        {
            return false;
        }

        self.selection_highlight_clear_deadline = None;
        if self
            .state
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.is_in_progress())
        {
            self.state.clear_selection();
            return true;
        }
        false
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

    pub(crate) fn tick_selection_autoscroll(&mut self, now: Instant) {
        let Some(autoscroll) = self.state.selection_autoscroll.clone() else {
            // Self-heal: state cleared but deadline leaked
            self.selection_autoscroll_deadline = None;
            return;
        };

        // Selection must still be in progress for autoscroll to continue
        let Some(pane_id) = self.state.selection.as_ref().map(|s| s.pane_id) else {
            self.stop_selection_autoscroll();
            return;
        };
        if !self
            .state
            .selection
            .as_ref()
            .is_some_and(|s| s.is_dragging())
        {
            self.stop_selection_autoscroll();
            return;
        }

        // Rect-change detection: if inner_rect changed since drag, stop
        let current_rect = self
            .state
            .pane_info_by_id(pane_id)
            .map(|info| info.inner_rect);
        if current_rect != Some(autoscroll.inner_rect) {
            self.stop_selection_autoscroll();
            return;
        }

        // Scrollback boundary detection via ScrollMetrics — fail-closed if unavailable
        let Some(metrics) = self
            .state
            .pane_scroll_metrics(&self.terminal_runtimes, pane_id)
        else {
            self.stop_selection_autoscroll();
            return;
        };
        match autoscroll.direction {
            crate::app::state::SelectionAutoscrollDirection::Up => {
                let at_top = metrics.offset_from_bottom >= metrics.max_offset_from_bottom;
                if at_top {
                    self.stop_selection_autoscroll();
                    return;
                }
                self.state
                    .scroll_pane_up(&self.terminal_runtimes, pane_id, 1);
            }
            crate::app::state::SelectionAutoscrollDirection::Down => {
                let at_bottom = metrics.offset_from_bottom == 0;
                if at_bottom {
                    self.stop_selection_autoscroll();
                    return;
                }
                self.state
                    .scroll_pane_down(&self.terminal_runtimes, pane_id, 1);
            }
        }

        // Extend selection cursor to last known mouse position
        self.state.update_selection_cursor(
            &self.terminal_runtimes,
            pane_id,
            autoscroll.last_mouse_screen_col,
            autoscroll.last_mouse_screen_row,
        );

        // Reschedule
        self.selection_autoscroll_deadline = Some(now + SELECTION_AUTOSCROLL_INTERVAL);
    }

    pub(crate) fn stop_selection_autoscroll(&mut self) {
        self.state.stop_selection_autoscroll_state();
        self.selection_autoscroll_deadline = None;
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
        if !background_update_check_enabled(self.no_session, self.update_version_check_enabled) {
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
        std::thread::spawn(move || crate::update::auto_update(update_tx));
    }

    pub(crate) fn run_agent_manifest_update_check(&mut self) {
        if !background_update_check_enabled(self.no_session, self.update_manifest_check_enabled) {
            self.next_agent_manifest_update_check = None;
            return;
        }

        self.next_agent_manifest_update_check = Some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        let manifest_update_tx = self.event_tx.clone();
        std::thread::spawn(move || crate::detect::manifest_update::auto_update(manifest_update_tx));
    }

    pub(crate) fn next_loop_deadline(&self, now: Instant, needs_render: bool) -> Option<Instant> {
        let deadline = self.next_loop_deadline_with_resize_poll(now, needs_render, true, true);
        [
            deadline,
            self.state
                .scratch
                .poll_at
                .map(|at| at + std::time::Duration::from_secs(2)),
            self.state
                .scratch
                .editor
                .as_ref()
                .and_then(|e| e.dirty_at.map(|at| at + crate::scratch::SAVE_DELAY)),
            crate::ui::pomodoro::animation_deadline_at(&self.state, self.state.screen_rect(), now),
            self.planning_lock_render_deadline(now),
            self.state.agent_finder_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn next_headless_loop_deadline_with_client_refresh(
        &self,
        now: Instant,
        needs_render: bool,
        include_client_refresh: bool,
    ) -> Option<Instant> {
        [
            self.next_loop_deadline_with_resize_poll(
                now,
                needs_render,
                false,
                include_client_refresh,
            ),
            self.planning_lock_render_deadline(now),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn planning_lock_render_deadline(&self, now: Instant) -> Option<Instant> {
        let unix_now = crate::app::settled::unix_seconds(std::time::SystemTime::now());
        self.state
            .planning_lock
            .snapshot(unix_now)?
            .unlock_until_unix_s?;
        Some(now + std::time::Duration::from_secs(1))
    }

    fn next_loop_deadline_with_resize_poll(
        &self,
        now: Instant,
        needs_render: bool,
        include_resize_poll: bool,
        include_client_refresh: bool,
    ) -> Option<Instant> {
        let render_deadline = if needs_render {
            self.last_render_at
                .map(|last_render_at| last_render_at + MIN_RENDER_INTERVAL)
                .filter(|deadline| *deadline > now)
        } else {
            None
        };

        [
            include_resize_poll.then_some(self.next_resize_poll),
            self.config_diagnostic_deadline,
            self.toast_deadline,
            self.state.next_pending_agent_notification_deadline(),
            self.state.next_managed_agent_deadline(),
            self.state.next_agent_watchdog_deadline(),
            self.state.next_full_lifecycle_hook_authority_deadline(),
            include_client_refresh
                .then(|| self.state.next_done_hide_deadline(now))
                .flatten(),
            self.state.next_done_reap_deadline(now),
            self.state.next_done_settle_deadline(now),
            self.state.next_snooze_deadline_at(
                now,
                super::settled::unix_seconds(std::time::SystemTime::now()),
            ),
            self.copy_feedback_deadline,
            // Presentation only: an unattached headless server draws no
            // sidebar, so client-specific animation deadlines are handled by
            // the headless server instead of this shared App scheduler.
            include_client_refresh
                .then(|| self.sidebar_animation_deadline())
                .flatten(),
            self.status_metric_refresh.deadline().filter(|_| {
                include_client_refresh
                    && self.status_metric_refresh_enabled
                    && self.status_metrics_visible
                    && self.state.status_bar_enabled
            }),
            self.status_metrics_visible
                .then(|| {
                    self.state.status_metrics.as_ref().map(|snapshot| {
                        snapshot.sampled_at
                            + crate::platform::status_metrics::STATUS_METRIC_STALE_AFTER
                    })
                })
                .flatten(),
            include_client_refresh
                .then(|| self.git_refresh_deadline())
                .flatten(),
            include_client_refresh
                .then(|| self.git_work_context_refresh_deadline())
                .flatten(),
            self.work_index_refresh_deadline(),
            self.work_item_detail_refresh_deadline(),
            include_client_refresh
                .then(|| self.foreground_process_refresh_deadline())
                .flatten(),
            self.claude_subagent_refresh_deadline(),
            self.next_auto_update_check,
            self.next_agent_manifest_update_check,
            self.agent_metadata_deadline,
            include_client_refresh
                .then_some(self.agent_activity_refresh_deadline)
                .flatten(),
            self.pending_agent_resume_deadline,
            self.next_resume_nudge_deadline(),
            self.next_auto_nudge_deadline(now),
            self.session_save_deadline,
            self.loop_receipt_fallback_deadline,
            self.selection_autoscroll_deadline,
            self.selection_highlight_clear_deadline,
            self.git_action_deadline(),
            include_client_refresh
                .then(|| self.state.hover_tooltip_deadline())
                .flatten(),
            self.next_tab_bar_status_deadline(),
            render_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn drain_internal_events(&mut self) -> bool {
        self.drain_internal_events_up_to(super::APP_EVENT_DRAIN_LIMIT)
            .1
    }

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

    fn drain_internal_events_up_to(&mut self, limit: usize) -> (bool, bool) {
        let mut had_event = false;
        let mut changed = false;
        for _ in 0..limit {
            let Ok(ev) = self.event_rx.try_recv() else {
                break;
            };
            had_event = true;
            changed |= self.handle_internal_event_with_prefix_sync(ev);
        }
        (had_event, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state;
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

    #[tokio::test]
    async fn monolithic_focus_return_raises_a_held_pomodoro_prompt() {
        let mut app = test_app_with_pane().0;
        let started_at = Instant::now();
        app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            started_at,
        );

        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusLost)
                .await
        );
        assert!(app.handle_scheduled_tasks(started_at + Duration::from_secs(60), false));
        assert!(app.state.pomodoro.held());
        assert!(app.state.pomodoro.prompt.is_none());

        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
                .await
        );
        let prompt = app
            .state
            .pomodoro
            .prompt
            .as_ref()
            .expect("focus return raises the held prompt");
        assert_eq!(prompt.ended, crate::pomodoro::PomodoroPhase::Work);
        assert!(!app.state.pomodoro.held());
    }

    #[tokio::test]
    async fn monolithic_raw_text_paste_and_uncaptured_click_close_send_off_without_pane_input() {
        for event in [
            crate::raw_input::RawInputEvent::Text(crate::input::TextCommit::new("committed")),
            crate::raw_input::RawInputEvent::Paste("pasted".into()),
        ] {
            let (mut app, mut pane_input) = test_app_with_send_off_and_mouse_reporting(false);
            assert!(app.handle_raw_input_event(event).await);
            assert!(app.state.pomodoro.send_off.is_none());
            assert!(
                pane_input.try_recv().is_err(),
                "the closing raw input must not reach the pane"
            );
        }

        let mouse = |kind| {
            crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 40,
                row: 12,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        };
        for mouse_capture in [false, true] {
            let (mut app, mut pane_input) =
                test_app_with_send_off_and_mouse_reporting(mouse_capture);
            for event in [
                mouse(crossterm::event::MouseEventKind::Moved),
                mouse(crossterm::event::MouseEventKind::ScrollDown),
                mouse(crossterm::event::MouseEventKind::Down(
                    crossterm::event::MouseButton::Left,
                )),
                mouse(crossterm::event::MouseEventKind::Drag(
                    crossterm::event::MouseButton::Left,
                )),
                mouse(crossterm::event::MouseEventKind::Up(
                    crossterm::event::MouseButton::Left,
                )),
            ] {
                assert!(app.handle_raw_input_event(event).await);
            }
            assert!(app.state.pomodoro.send_off.is_none());
            assert!(
                pane_input.try_recv().is_err(),
                "send-off mouse events reached the pane with capture={mouse_capture}"
            );
        }

        let (mut app, mut pane_input) = test_app_with_send_off_and_mouse_reporting(false);
        app.state.pomodoro.send_off = None;
        let click = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 40,
            row: 12,
            modifiers: crossterm::event::KeyModifiers::empty(),
        };
        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(click))
                .await
        );
        assert!(
            pane_input.try_recv().is_ok(),
            "without the send-off, the same click reaches the mouse-reporting pane"
        );
    }

    #[tokio::test]
    async fn p1_monolithic_raw_input_batch_uses_its_starting_pomodoro_gate() {
        let (mut app, mut pane_input) = test_app_with_send_off_and_mouse_reporting(false);
        let (input_tx, input_rx) = tokio::sync::mpsc::channel(4);
        app.input_rx = Some(input_rx);
        let key = |ch| {
            crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ))
        };
        input_tx.send(key('y')).await.expect("batch follow-up key");

        assert!(app.handle_raw_input_batch(key('x')).await);
        assert!(app.state.pomodoro.send_off.is_none());
        assert!(
            pane_input.try_recv().is_err(),
            "an event after dismissal escaped the batch's starting gate"
        );
    }

    #[tokio::test]
    async fn p1_monolithic_focus_return_batch_refreshes_the_pomodoro_gate() {
        let (mut app, mut pane_input) = test_app_with_send_off_and_mouse_reporting(false);
        let started_at = Instant::now();
        app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            started_at,
        );

        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusLost)
                .await
        );
        assert!(app.handle_scheduled_tasks(started_at + Duration::from_secs(60), false));
        assert!(app.state.pomodoro.held());

        let (input_tx, input_rx) = tokio::sync::mpsc::channel(4);
        app.input_rx = Some(input_rx);
        input_tx
            .send(crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('x'),
                    crossterm::event::KeyModifiers::empty(),
                ),
            ))
            .await
            .expect("batch follow-up key");

        assert!(
            app.handle_raw_input_batch(crate::raw_input::RawInputEvent::OuterFocusGained)
                .await
        );
        assert!(app.state.pomodoro.prompt.is_some());
        assert!(
            pane_input.try_recv().is_err(),
            "a key after focus return must not reach the pane"
        );
    }

    #[tokio::test]
    async fn p1_monolithic_focus_return_gate_survives_until_a_frame_is_committed() {
        let (mut app, mut pane_input) = test_app_with_send_off_and_mouse_reporting(false);
        let started_at = Instant::now();
        app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            started_at,
        );

        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusLost)
                .await
        );
        assert!(app.handle_scheduled_tasks(started_at + Duration::from_secs(60), false));
        assert!(app.state.pomodoro.held());
        assert!(
            app.handle_raw_input_batch(crate::raw_input::RawInputEvent::OuterFocusGained)
                .await
        );
        assert!(app.state.pomodoro.prompt.is_some());

        let key = crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(app.handle_raw_input_batch(key).await);
        assert!(
            pane_input.try_recv().is_err(),
            "the prompt raised between frames must gate the next batch"
        );
    }

    #[tokio::test]
    async fn p1_monolithic_focus_refresh_cannot_remove_a_visible_send_off_gate() {
        let (mut app, mut pane_input) = test_app_with_send_off_and_mouse_reporting(false);
        let area = app.state.screen_rect();
        app.local_pomodoro_presentation = Some(crate::ui::pomodoro::input_presentation_at(
            &app.state,
            area,
            Instant::now(),
        ));
        app.state.pomodoro.send_off = None;
        let (input_tx, input_rx) = tokio::sync::mpsc::channel(2);
        app.input_rx = Some(input_rx);
        input_tx
            .send(crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('x'),
                    crossterm::event::KeyModifiers::empty(),
                ),
            ))
            .await
            .expect("batch follow-up key");

        assert!(
            app.handle_raw_input_batch(crate::raw_input::RawInputEvent::OuterFocusGained)
                .await
        );
        assert!(
            pane_input.try_recv().is_err(),
            "shared state cannot clear a send-off still visible in the committed frame"
        );
    }

    #[tokio::test]
    async fn f2_monolithic_send_off_release_clears_the_dismissal_key_lease() {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let (runtime, mut pane_input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id, runtime);
        app.state.set_server_mode(crate::app::Mode::Terminal);
        app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });

        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        );
        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Key(key.clone()))
                .await
        );
        assert!(app.state.pomodoro.send_off.is_none());
        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::Key(
                key.clone()
                    .with_kind(crossterm::event::KeyEventKind::Repeat),
            ))
            .await
        );
        app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::Key(
                key.clone()
                    .with_kind(crossterm::event::KeyEventKind::Release),
            ))
            .await
        );
        assert!(app.input_leases.is_empty());
        assert!(app.state.pomodoro.send_off.is_some());
        assert!(
            pane_input.try_recv().is_err(),
            "the dismissal key lifecycle must not reach the pane"
        );
        app.state.pomodoro.send_off = None;
        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Key(key))
                .await
        );
        assert_eq!(
            pane_input.recv().await.expect("second press reaches pane"),
            bytes::Bytes::from_static(b"x")
        );
    }

    #[tokio::test]
    async fn f3_monolithic_send_off_only_swallows_mouse_up_after_its_down() {
        let (mut app, mut pane_input) = test_app_with_send_off_and_mouse_reporting(false);
        app.state.pomodoro.send_off = None;
        let mouse = |kind| {
            crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 40,
                row: 12,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        };

        assert!(
            app.handle_raw_input_event(mouse(crossterm::event::MouseEventKind::Down(
                crossterm::event::MouseButton::Left,
            )))
            .await
        );
        pane_input.recv().await.expect("mouse down reaches pane");

        app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        assert!(
            app.handle_raw_input_event(mouse(crossterm::event::MouseEventKind::Up(
                crossterm::event::MouseButton::Left,
            )))
            .await
        );
        pane_input.recv().await.expect("mouse up reaches pane");
        assert!(app.state.pomodoro.send_off.is_some());

        assert!(
            app.handle_raw_input_event(mouse(crossterm::event::MouseEventKind::Down(
                crossterm::event::MouseButton::Left,
            )))
            .await
        );
        assert!(
            app.handle_raw_input_event(mouse(crossterm::event::MouseEventKind::Up(
                crossterm::event::MouseButton::Left,
            )))
            .await
        );
        assert!(pane_input.try_recv().is_err());
    }

    fn test_app_with_send_off_and_mouse_reporting(
        mouse_capture: bool,
    ) -> (super::super::App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let (runtime, pane_input) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[?1003h\x1b[?1006h",
                16,
            );
        assert!(
            runtime.mouse_reporting_enabled(),
            "reproduction requires a mouse-reporting pane"
        );
        app.terminal_runtimes.insert(terminal_id, runtime);
        app.state.set_server_mode(crate::app::Mode::Terminal);
        app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        app.state.mouse_capture = mouse_capture;
        (app, pane_input)
    }

    fn test_app_with_pane() -> (super::super::App, crate::layout::PaneId) {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
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

    #[tokio::test]
    async fn uncaptured_mouse_input_refreshes_activity_on_both_ingress_paths() {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let (runtime, mut pane_input) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[?1000h\x1b[?1006h",
                4,
            );
        app.terminal_runtimes.insert(terminal_id, runtime);
        app.state.set_server_mode(crate::app::Mode::Terminal);
        app.state.mouse_capture = false;
        let mouse = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };

        for ingress in ["monolithic", "headless"] {
            let stale_at = Instant::now() - Duration::from_secs(60);
            app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane_id)
                .expect("test pane")
                .activity
                .set_last_at(stale_at);
            let event = crate::raw_input::RawInputEvent::Mouse(mouse);
            if ingress == "monolithic" {
                assert!(app.handle_raw_input_event(event).await);
            } else {
                app.route_client_events(vec![event], false);
            }

            assert!(
                app.state.workspaces[0].tabs[0].panes[&pane_id]
                    .activity
                    .last_at()
                    > stale_at,
                "{ingress} ingress must refresh activity"
            );
            assert!(pane_input.try_recv().is_ok(), "{ingress} mouse forwarding");
        }
    }

    #[tokio::test]
    async fn runtime_scheduler_ticks_stalled_agent_auto_nudge() {
        let activity_at = Instant::now();
        let now = activity_at + Duration::from_secs(20 * 60);
        let mut config = crate::config::Config::default();
        config.session.auto_nudge_stalled_agents = true;
        let mut app = super::super::App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("runtime-auto-nudge");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        workspace.tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(activity_at);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.supervisor_stale = true;
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 4,
            );
        app.terminal_runtimes.insert(terminal_id, runtime);

        assert_eq!(app.next_auto_nudge_deadline(now), Some(now));
        assert!(app
            .next_headless_loop_deadline_with_client_refresh(now, false, false)
            .is_some_and(|deadline| deadline <= now));
        app.handle_scheduled_tasks(now, false);

        let mut sent = String::new();
        while let Ok(bytes) = rx.try_recv() {
            sent.push_str(&String::from_utf8_lossy(&bytes));
        }
        assert!(
            sent.contains("Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue."),
            "expected auto-nudge, got {sent:?}"
        );
    }

    #[test]
    fn disabled_status_bar_suppresses_metric_sampling_and_deadline() {
        let mut config = crate::config::Config::default();
        config.ui.status_bar.enabled = false;
        config.work_index.enabled = false;
        let mut app = super::super::App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let now = Instant::now();

        assert!(!app.schedule_status_metrics(now));
        assert!(!app.status_metric_refresh.in_flight());
        assert!(app.status_metric_refresh.deadline().is_some());
        assert_eq!(
            app.next_headless_loop_deadline_with_client_refresh(now, false, true),
            None
        );
    }

    #[test]
    fn pomodoro_animation_deadline_uses_the_visible_client_geometry() {
        fn app_without_other_deadlines() -> super::super::App {
            let mut config = crate::config::Config::default();
            config.ui.status_bar.enabled = false;
            config.work_index.enabled = false;
            super::super::App::new(
                &config,
                true,
                None,
                tokio::sync::mpsc::unbounded_channel().1,
                crate::api::EventHub::default(),
            )
        }

        let now = Instant::now();
        let mut app = app_without_other_deadlines();
        app.next_resize_poll = now + Duration::from_secs(5);
        app.state.pomodoro.enabled = true;
        app.state.pomodoro.prompt = Some(crate::pomodoro::PomodoroPrompt {
            ended: crate::pomodoro::PomodoroPhase::Work,
            next: crate::pomodoro::PomodoroPhase::ShortBreak,
            raised_at: now,
            input: String::new(),
            error: None,
        });
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 18, 30);

        assert_eq!(
            app.next_headless_loop_deadline_with_client_refresh(now, false, true),
            None
        );
        assert_eq!(
            app.next_loop_deadline_with_resize_poll(now, false, false, true),
            None
        );
        assert_eq!(
            app.next_loop_deadline(now, false),
            Some(now + Duration::from_secs(5))
        );

        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        assert_eq!(
            app.next_loop_deadline(now, false),
            Some(now + crate::ui::pomodoro::ANIMATION_FRAME_INTERVAL)
        );
        assert!(!app.tick_pomodoro(now + crate::ui::pomodoro::ANIMATION_FRAME_INTERVAL, true));
        assert!(app.pomodoro_animation_due(now + crate::ui::pomodoro::ANIMATION_FRAME_INTERVAL));

        let mut send_off = app_without_other_deadlines();
        send_off.state.pomodoro.enabled = true;
        send_off.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: now,
        });
        send_off.next_resize_poll = now + Duration::from_secs(5);
        send_off.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 18, 30);
        assert_eq!(
            send_off.next_loop_deadline(now, false),
            Some(now + Duration::from_secs(5))
        );
        send_off.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        assert_eq!(
            send_off.next_loop_deadline(now, false),
            Some(now + crate::ui::pomodoro::ANIMATION_FRAME_INTERVAL)
        );
        assert!(send_off.tick_pomodoro(now + crate::pomodoro::SEND_OFF_DURATION, true));
        assert!(send_off.state.pomodoro.send_off.is_none());
    }

    #[test]
    fn resize_invalidates_renderability_before_metric_sampling() {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let now = Instant::now();
        app.status_metrics_visible = true;
        app.status_metric_refresh =
            crate::platform::status_metrics::StatusMetricRefresh::immediate(now);

        assert!(!app.schedule_status_metrics_after_resize_poll(now, true));
        assert!(!app.status_metrics_visible);
        assert!(!app.status_metric_refresh.in_flight());
    }

    #[test]
    fn stale_status_metrics_are_dropped_without_sampling() {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let now = Instant::now();
        let sampled_at = now
            .checked_sub(crate::platform::status_metrics::STATUS_METRIC_STALE_AFTER)
            .expect("stale instant")
            - Duration::from_millis(1);
        app.state.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: crate::platform::status_metrics::StatusMetrics::default(),
            sampled_at,
        });

        assert!(app.discard_stale_status_metrics(now));
        assert!(app.state.status_metrics.is_none());
        assert!(!app.discard_stale_status_metrics(now));
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_metrics_unavailable() {
        // Without a runtime, pane_scroll_metrics returns None.
        // Fail-closed: stop autoscroll instead of rescheduling forever.
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        // Drag to a different cell so it becomes Dragging
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 5,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // Should stop because no runtime metrics available
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_done() {
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        // Create a selection that is already finished (not in progress)
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        // Drag to a different cell so it becomes visible, then finish
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        sel.finish(); // now it's Done, not in progress
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_cleared() {
        let (mut app, _pane_id) = test_app_with_pane();
        let now = Instant::now();
        app.state.selection = None;
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_anchored() {
        // Anchored (click, no drag) should not keep the timer running.
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        app.state.selection = Some(crate::selection::Selection::anchor(pane_id, 0, 0, None));
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    /// Creates an app with a real TerminalRuntime (no PTY) so scroll_metrics
    /// returns meaningful data. Uses test_with_scrollback_bytes.
    fn test_app_with_runtime(
        cols: u16,
        rows: u16,
        bytes: &[u8],
    ) -> (super::super::App, crate::layout::PaneId) {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(cols, rows, 0, bytes);
        ws.tabs[0].runtimes.insert(pane_id, runtime);
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.view.pane_infos.push(crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, cols, rows),
            inner_rect: ratatui::layout::Rect::new(0, 0, cols, rows),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        });
        (app, pane_id)
    }

    #[tokio::test]
    async fn tick_selection_autoscroll_stops_at_scrollback_top() {
        // Create a runtime with no scrollback content — we're already at
        // the top (offset_from_bottom == max_offset_from_bottom).
        let (mut app, pane_id) = test_app_with_runtime(80, 24, &[]);
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 5, 5, None);
        sel.drag(0, 0, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Up,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 0,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // At scrollback top, can't scroll further up — should stop
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn tick_selection_autoscroll_stops_at_scrollback_bottom() {
        // Create a runtime with no scrollback content — we're already at
        // the bottom (offset_from_bottom == 0).
        let (mut app, pane_id) = test_app_with_runtime(80, 24, &[]);
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 5,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // At scrollback bottom, can't scroll further down — should stop
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn passive_mouse_motion_does_not_request_monolithic_render() {
        let (mut app, _) = test_app_with_pane();
        app.state.set_server_mode(crate::app::Mode::Terminal);
        let motion = || {
            crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Moved,
                column: 10,
                row: 5,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        };

        assert!(!app.handle_raw_input_event(motion()).await);
        app.state.sidebar_group_menu_open = true;
        assert!(app.handle_raw_input_event(motion()).await);
        app.state.sidebar_group_menu_open = false;
        app.state.set_server_mode(crate::app::Mode::GlobalMenu);
        assert!(app.handle_raw_input_event(motion()).await);
    }

    #[tokio::test]
    async fn raw_input_batch_does_not_start_pending_agent_resume_before_render() {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane should have a terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        assert!(
            app.handle_raw_input_batch(crate::raw_input::RawInputEvent::HostDefaultColor {
                kind: crate::terminal_theme::DefaultColorKind::Foreground,
                color: crate::terminal_theme::RgbColor {
                    r: 220,
                    g: 220,
                    b: 220,
                },
            })
            .await
        );
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_none(),
            "raw input can mutate active geometry; pending resumes must wait for render to refresh pane_infos"
        );
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
    }

    #[tokio::test]
    async fn scheduled_tasks_do_not_start_pending_agent_resume_when_geometry_dirty() {
        let (mut app, pane_id) = test_app_with_pane();
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
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane should have a terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });
        app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        assert!(!app.handle_scheduled_tasks(Instant::now(), true));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(app.pending_agent_resume_deadline.is_none());
    }
}
