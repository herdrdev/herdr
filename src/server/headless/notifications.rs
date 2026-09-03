use super::*;

impl HeadlessServer {
    fn pane_effective_state(&self, pane_id: crate::layout::PaneId) -> crate::detect::AgentState {
        self.app
            .state
            .workspaces
            .iter()
            .find_map(|ws| {
                ws.tabs.iter().find_map(|tab| {
                    let pane = tab.panes.get(&pane_id)?;
                    self.app
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .map(|terminal| terminal.state)
                })
            })
            .unwrap_or(crate::detect::AgentState::Unknown)
    }

    fn pane_effective_agent_label(&self, pane_id: crate::layout::PaneId) -> Option<String> {
        self.app.state.workspaces.iter().find_map(|ws| {
            ws.tabs.iter().find_map(|tab| {
                let pane = tab.panes.get(&pane_id)?;
                self.app
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .and_then(|terminal| terminal.effective_agent_label())
                    .map(str::to_string)
            })
        })
    }

    fn forward_semantic_agent_notification(
        &mut self,
        update: &crate::app::actions::PaneStateUpdate,
    ) -> bool {
        if update.suppress_completion {
            return false;
        }
        self.forward_semantic_agent_transition(
            update.ws_idx,
            update.pane_id,
            update.previous_state,
            update.state,
            update.previous_agent_label.as_deref(),
            update.agent_label.as_deref(),
            update.known_agent.or(update.previous_known_agent),
        )
    }

    pub(super) fn forward_semantic_agent_transition(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        previous_state: crate::detect::AgentState,
        state: crate::detect::AgentState,
        previous_agent_label: Option<&str>,
        agent_label: Option<&str>,
        known_agent: Option<crate::detect::Agent>,
    ) -> bool {
        let Some(kind) = crate::app::actions::notification_toast_for_state_change_with_agent_labels(
            false,
            previous_state,
            state,
            previous_agent_label,
            agent_label,
        ) else {
            return false;
        };
        let Some(workspace) = self.app.state.workspaces.get(ws_idx) else {
            return false;
        };
        let Some(tab_idx) = workspace.find_tab_index_for_pane(pane_id) else {
            return false;
        };
        let Some(tab_number) = workspace.public_tab_number(tab_idx) else {
            return false;
        };
        let Some(public_pane_id) = self.app.public_pane_id(ws_idx, pane_id) else {
            return false;
        };
        let Some(agent_label) = agent_label.or(previous_agent_label) else {
            return false;
        };
        let (semantic_kind, event_text, sound) = match kind {
            crate::app::state::ToastKind::NeedsAttention => (
                protocol::SemanticNotificationKind::NeedsAttention,
                "needs attention",
                Some(protocol::SemanticNotificationSound::Request),
            ),
            crate::app::state::ToastKind::Finished => (
                protocol::SemanticNotificationKind::Finished,
                "finished",
                Some(protocol::SemanticNotificationSound::Done),
            ),
            crate::app::state::ToastKind::UpdateInstalled => (
                protocol::SemanticNotificationKind::UpdateInstalled,
                "updated",
                None,
            ),
        };
        let workspace_id = workspace.id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(&workspace_id, tab_number);
        let workspace_label =
            workspace.display_name_from(&self.app.state.terminals, &self.app.terminal_runtimes);
        let context =
            crate::app::actions::notification_context(workspace, &workspace_label, ws_idx, pane_id);
        let agent = known_agent
            .map(crate::detect::agent_label)
            .map(str::to_owned);
        self.send_to_client_shells(ServerMessage::SemanticNotification(
            protocol::SemanticNotification {
                kind: semantic_kind,
                title: format!("{agent_label} {event_text}"),
                body: non_empty_body(&context),
                sound,
                agent,
                workspace_id: Some(workspace_id),
                tab_id: Some(tab_id),
                pane_id: Some(public_pane_id),
                position: None,
            },
        ))
    }

    fn forward_pane_state_update_notifications_to_clients(
        &mut self,
        update: &crate::app::actions::PaneStateUpdate,
    ) {
        if self.app.state.toast_config.delay_seconds != 0 {
            return;
        }

        let is_active_tab = self
            .app
            .state
            .pane_is_in_active_tab(update.ws_idx, update.pane_id);
        let suppress_active_tab_notifications =
            self.active_tab_suppresses_notifications(is_active_tab);

        if !update.suppress_completion && self.app.state.sound.allows(update.known_agent) {
            if let Some(sound) =
                crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                    suppress_active_tab_notifications,
                    update.previous_state,
                    update.state,
                    update.previous_agent_label.as_deref(),
                    update.agent_label.as_deref(),
                )
            {
                self.send_notify_to_foreground_client(
                    protocol::NotifyKind::Sound,
                    sound_notify_message(sound),
                    None,
                );
            }
        }

        if !should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
            return;
        }
        let Some(kind) = crate::app::actions::notification_toast_for_pane_state_update(
            suppress_active_tab_notifications,
            update,
        ) else {
            return;
        };
        let Some(ws) = self.app.state.workspaces.get(update.ws_idx) else {
            return;
        };
        let Some(agent_label) = update.agent_label.as_deref() else {
            return;
        };
        let event_text = match kind {
            crate::app::state::ToastKind::NeedsAttention => "needs attention",
            crate::app::state::ToastKind::Finished => "finished",
            crate::app::state::ToastKind::UpdateInstalled => "updated",
        };
        let workspace_label =
            ws.display_name_from(&self.app.state.terminals, &self.app.terminal_runtimes);
        let context = crate::app::actions::notification_context(
            ws,
            &workspace_label,
            update.ws_idx,
            update.pane_id,
        );
        self.send_notify_to_foreground_client(
            toast_notify_kind(self.app.state.toast_config.delivery)
                .expect("toast forwarding requires a client notification kind"),
            format!("{agent_label} {event_text}"),
            non_empty_body(&context),
        );
    }

    pub(super) fn forward_agent_notification_delivery(
        &mut self,
        delivery: &crate::app::state::AgentNotificationDelivery,
    ) {
        if let Some(sound) = delivery.sound {
            self.send_notify_to_foreground_client(
                protocol::NotifyKind::Sound,
                sound_notify_message(sound),
                None,
            );
        }

        if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
            if let Some(toast) = &delivery.client_notification {
                self.send_notify_to_foreground_client(
                    toast_notify_kind(self.app.state.toast_config.delivery)
                        .expect("toast forwarding requires a client notification kind"),
                    &toast.title,
                    non_empty_body(&toast.context),
                );
            }
        }
    }

    pub(super) fn send_notify_to_foreground_client(
        &mut self,
        kind: protocol::NotifyKind,
        message: impl Into<String>,
        body: Option<String>,
    ) -> bool {
        self.send_to_foreground_client(ServerMessage::Notify {
            kind,
            message: message.into(),
            body,
        })
    }

    pub(super) fn send_flat_toast_to_foreground_client(
        &mut self,
        kind: protocol::NotifyKind,
        message: impl AsRef<str>,
    ) -> bool {
        let (title, body) = crate::terminal_notify::split_message(message.as_ref());
        self.send_notify_to_foreground_client(kind, title, body.map(str::to_string))
    }

    pub(super) fn handle_notification_show_api(
        &mut self,
        id: String,
        params: api::schema::NotificationShowParams,
    ) -> String {
        use api::schema::NotificationShowReason;

        let Some(title) = sanitize_notification_text(&params.title, 80) else {
            return serde_json::to_string(&api::schema::ErrorResponse {
                id,
                error: api::schema::ErrorBody {
                    code: "invalid_params".into(),
                    message: "notification title is empty".into(),
                },
            })
            .unwrap_or_else(|_| "{}".to_string());
        };

        let body = params
            .body
            .as_deref()
            .and_then(|body| sanitize_notification_text(body, 240));
        let has_client_shell = self.clients.values().any(ClientConnection::is_shell_client);
        if !has_client_shell {
            let reason = if self.app.state.toast_config.delivery == config::ToastDelivery::Off {
                NotificationShowReason::Disabled
            } else {
                NotificationShowReason::NoForegroundClient
            };
            return notification_show_result(id, false, reason);
        }
        if self.app.api_notification_rate_limited(Instant::now()) {
            return notification_show_result(id, false, NotificationShowReason::RateLimited);
        }
        let sound = match params.sound {
            api::schema::NotificationShowSound::None => None,
            api::schema::NotificationShowSound::Done => {
                Some(protocol::SemanticNotificationSound::Done)
            }
            api::schema::NotificationShowSound::Request => {
                Some(protocol::SemanticNotificationSound::Request)
            }
        };
        let shown = self.send_to_client_shells(ServerMessage::SemanticNotification(
            protocol::SemanticNotification {
                kind: protocol::SemanticNotificationKind::Custom,
                title,
                body,
                sound,
                agent: None,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                position: params.position,
            },
        ));
        if shown {
            self.app.mark_api_notification_shown(Instant::now());
        }
        notification_show_result(
            id,
            shown,
            if shown {
                NotificationShowReason::Shown
            } else {
                NotificationShowReason::NoForegroundClient
            },
        )
    }

    /// Handles a single internal event with forwarding logic for clipboard,
    /// sound, and toast notifications to connected clients.
    ///
    /// ALL internal events MUST be routed through this method to ensure
    /// clipboard/notify forwarding is never bypassed. Do not call
    /// `self.app.handle_internal_event()` directly for any internal event
    /// in the headless server — use this method instead.
    ///
    /// Returns true if the event changed visual state (requiring a re-render).
    pub(super) fn handle_internal_event_with_forwarding(&mut self, mut ev: AppEvent) -> bool {
        let mut focused_worktree_response = if let AppEvent::WorktreeAddFinished(result) = &mut ev {
            result
                .api_request
                .as_mut()
                .filter(|request| request.focus)
                .map(|request| {
                    let (proxy_tx, proxy_rx) = std::sync::mpsc::channel();
                    let original = std::mem::replace(&mut request.respond_to, proxy_tx);
                    (original, proxy_rx)
                })
        } else {
            None
        };
        match &ev {
            AppEvent::TerminalBell { pane_id, count } => {
                if !self.send_to_foreground_client(ServerMessage::TerminalBell { count: *count }) {
                    debug!(
                        pane = pane_id.raw(),
                        count, "dropped terminal bell without a foreground client"
                    );
                }
                false
            }
            AppEvent::ClipboardWrite { content } => {
                // Clipboard writes are client-local side effects. Forward them only to
                // the foreground client instead of broadcasting to every attached client.
                let data = base64::engine::general_purpose::STANDARD.encode(content.as_slice());
                self.send_to_foreground_client(ServerMessage::Clipboard { data });
                false
            }
            AppEvent::StateChanged { pane_id, agent, .. } => {
                // Capture toast before handling.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = *agent;

                // Find the previous effective state of this pane before the event
                // is processed. Notifications must follow effective state changes,
                // not raw fallback reports that may be masked by hook authority.
                let prev_state = self.pane_effective_state(pane_id_val);
                let prev_agent_label = self.pane_effective_agent_label(pane_id_val);

                // Handle the state change (updates pane state, sets toast on AppState).
                // Headless mode disables local sound playback separately from the
                // sound policy so reloads can keep server-side notification policy live.
                self.sync_foreground_client_state();
                let pane_updates = self.app.handle_internal_event_with_pane_updates(ev);
                let suppress_completion = pane_updates
                    .iter()
                    .any(|update| update.pane_id == pane_id_val && update.suppress_completion);
                for update in pane_updates
                    .iter()
                    .filter(|update| update.pane_id == pane_id_val)
                {
                    self.forward_semantic_agent_notification(update);
                }

                // Forward sound notification to clients when server-side sound policy allows it.
                let is_active_tab = self
                    .app
                    .state
                    .active
                    .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| {
                        ws.find_tab_index_for_pane(pane_id_val)
                            .is_some_and(|tab_idx| ws.active_tab_index() == tab_idx)
                    });

                let suppress_active_tab_notifications =
                    self.active_tab_suppresses_notifications(is_active_tab);

                let next_state = self.pane_effective_state(pane_id_val);
                let next_agent_label = self.pane_effective_agent_label(pane_id_val);

                if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && self.app.state.sound.allows(agent_val)
                {
                    if let Some(sound) =
                        crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                            next_agent_label.as_deref(),
                        )
                    {
                        self.send_notify_to_foreground_client(
                            protocol::NotifyKind::Sound,
                            sound_notify_message(sound),
                            None,
                        );
                    }
                }

                let toast_msg = if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                {
                    if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                        self.app
                            .state
                            .toast
                            .as_ref()
                            .map(|toast| format!("{}: {}", toast.title, toast.context))
                    } else {
                        toast_message_from_state_change(
                            &self.app.state,
                            &self.app.terminal_runtimes,
                            pane_id_val,
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                        )
                    }
                } else {
                    None
                };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::HookStateReported {
                pane_id,
                agent_label,
                ..
            } => {
                // Hook reports can be stale or no-op after sequence rejection.
                // Forward only effective state changes observed after handling.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = crate::detect::parse_agent_label(agent_label);

                // Capture the previous effective state for this pane. Hook reports
                // are already folded into pane.state; raw hook transitions must not
                // produce a second notification path.
                let prev_state = self.pane_effective_state(pane_id_val);
                let prev_agent_label = self.pane_effective_agent_label(pane_id_val);

                self.sync_foreground_client_state();
                let pane_updates = self.app.handle_internal_event_with_pane_updates(ev);
                let suppress_completion = pane_updates
                    .iter()
                    .any(|update| update.pane_id == pane_id_val && update.suppress_completion);
                for update in pane_updates
                    .iter()
                    .filter(|update| update.pane_id == pane_id_val)
                {
                    self.forward_semantic_agent_notification(update);
                }

                // Forward sound notification based on the effective transition when
                // server-side sound policy allows it.
                let is_active_tab = self
                    .app
                    .state
                    .active
                    .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| {
                        ws.find_tab_index_for_pane(pane_id_val)
                            .is_some_and(|tab_idx| ws.active_tab_index() == tab_idx)
                    });

                let suppress_active_tab_notifications =
                    self.active_tab_suppresses_notifications(is_active_tab);

                let next_state = self.pane_effective_state(pane_id_val);
                let next_agent_label = self.pane_effective_agent_label(pane_id_val);

                if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && self.app.state.sound.allows(agent_val)
                {
                    if let Some(sound) =
                        crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                            next_agent_label.as_deref(),
                        )
                    {
                        self.send_notify_to_foreground_client(
                            protocol::NotifyKind::Sound,
                            sound_notify_message(sound),
                            None,
                        );
                    }
                }

                let toast_msg = if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                {
                    if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                        self.app
                            .state
                            .toast
                            .as_ref()
                            .map(|toast| format!("{}: {}", toast.title, toast.context))
                    } else {
                        toast_message_from_state_change(
                            &self.app.state,
                            &self.app.terminal_runtimes,
                            pane_id_val,
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                        )
                    }
                } else {
                    None
                };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::UpdateReady {
                version,
                install_command,
            } => {
                let toast_before = self.app.state.toast.clone();
                let version = version.clone();
                let install_command = install_command.clone();

                self.app.handle_internal_event(ev);
                self.send_to_client_shells(ServerMessage::SemanticNotification(
                    protocol::SemanticNotification {
                        kind: protocol::SemanticNotificationKind::UpdateInstalled,
                        title: format!("Herdr v{version} available"),
                        body: Some(crate::update::update_install_instruction(&install_command)),
                        sound: None,
                        agent: None,
                        workspace_id: None,
                        tab_id: None,
                        pane_id: None,
                        position: None,
                    },
                ));

                let toast_msg =
                    if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
                        if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                            self.app
                                .state
                                .toast
                                .as_ref()
                                .map(|toast| format!("{}: {}", toast.title, toast.context))
                        } else {
                            Some(format!(
                                "v{version} available: {}",
                                crate::update::update_install_instruction(&install_command)
                            ))
                        }
                    } else {
                        None
                    };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::WorktreeAddFinished(result) => {
                let deferred_request_id = result
                    .api_request
                    .as_ref()
                    .map(|request| request.id.as_str());
                let shell_navigation_pending = deferred_request_id.is_some_and(|request_id| {
                    self.clients.values().any(|client| {
                        client.shell_deferred_navigation_request_id.as_deref() == Some(request_id)
                    })
                });
                let changed = self.app.handle_internal_event_with_render_impact(ev);
                let api_focus_succeeded = super::client_views::forward_proxied_api_response(
                    focused_worktree_response.take(),
                );
                self.reconcile_client_shell_locations();
                if shell_navigation_pending {
                    self.app.accept_current_focus_without_events();
                } else if api_focus_succeeded {
                    self.focus_all_shell_clients_on_default_target();
                }
                changed
            }
            AppEvent::WorktreeRemoveFinished(result) => {
                let focus_before = self.shell_focus_targets();
                let focused_tabs_before = self.focused_shell_tabs();
                let shutdown_terminals = result
                    .api_request
                    .as_ref()
                    .map(|request| {
                        request
                            .shutdown_panes
                            .iter()
                            .filter_map(|pane_id| {
                                self.app.find_pane(*pane_id).map(|(_, pane)| {
                                    (*pane_id, pane.attached_terminal_id.to_string())
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let pane_updates = self.app.handle_internal_event_with_pane_updates(ev);
                for update in &pane_updates {
                    self.forward_semantic_agent_notification(update);
                    self.forward_pane_state_update_notifications_to_clients(update);
                }
                self.reconcile_client_shell_locations();
                self.finish_shell_location_reconciliation(focus_before, &focused_tabs_before);
                self.reapply_controlled_shell_tab_geometry(false);
                for (pane_id, terminal_id) in shutdown_terminals {
                    if self.app.find_pane(pane_id).is_none() {
                        self.shutdown_terminal_stream_clients(
                            &terminal_id,
                            format!("terminal {terminal_id} exited"),
                        );
                    }
                }
                true
            }
            AppEvent::PaneDied { pane_id }
            | AppEvent::WorktreeRuntimeRestoreFailed { pane_id, .. } => {
                let focus_before = self.shell_focus_targets();
                let focused_tabs_before = self.focused_shell_tabs();
                let pane_id_val = *pane_id;
                let terminal_id = self.app.state.workspaces.iter().find_map(|ws| {
                    ws.tabs.iter().find_map(|tab| {
                        tab.panes
                            .get(pane_id)
                            .map(|pane| pane.attached_terminal_id.to_string())
                    })
                });
                if matches!(&ev, AppEvent::PaneDied { .. })
                    && !self
                        .app
                        .pending_worktree_remove_runtime_exits
                        .contains_key(&pane_id_val)
                {
                    if let Some(update) = self
                        .app
                        .state
                        .publish_pane_process_exit_if_agent(pane_id_val, false)
                    {
                        self.app.emit_pane_state_update(&update);
                        self.forward_semantic_agent_notification(&update);
                        self.forward_pane_state_update_notifications_to_clients(&update);
                    }
                }

                let pane_updates = self.app.handle_internal_event_with_pane_updates(ev);
                for update in &pane_updates {
                    self.forward_semantic_agent_notification(update);
                    self.forward_pane_state_update_notifications_to_clients(update);
                }
                self.reconcile_client_shell_locations();
                self.finish_shell_location_reconciliation(focus_before, &focused_tabs_before);
                self.reapply_controlled_shell_tab_geometry(false);

                if self.app.find_pane(pane_id_val).is_none() {
                    if let Some(terminal_id) = terminal_id {
                        self.shutdown_terminal_stream_clients(
                            &terminal_id,
                            format!("terminal {terminal_id} exited"),
                        );
                    }
                }

                true
            }
            _ => self.app.handle_internal_event_with_render_impact(ev),
        }
    }

    /// Drains internal events, forwarding clipboard, sound, and toast
    /// notifications to connected clients instead of processing them locally.
    ///
    /// The server has no host terminal or audio subsystem, so we:
    /// - Forward `ClipboardWrite` as `ServerMessage::Clipboard` to the
    ///   foreground client only.
    /// - Detect when a sound would be played and forward as
    ///   `ServerMessage::Notify { kind: Sound }` to the foreground client.
    /// - Detect when a toast is set on AppState and forward as
    ///   `ServerMessage::Notify` to the foreground client for terminal/system delivery.
    pub(super) fn drain_internal_events_with_forwarding(&mut self) -> bool {
        self.drain_internal_events_with_forwarding_up_to(crate::app::APP_EVENT_DRAIN_LIMIT)
            .1
    }

    pub(super) fn drain_all_internal_events_with_forwarding(&mut self) -> bool {
        let mut changed = false;
        loop {
            let (had_event, batch_changed) =
                self.drain_internal_events_with_forwarding_up_to(crate::app::APP_EVENT_DRAIN_LIMIT);
            changed |= batch_changed;
            if !had_event || self.should_quit.load(Ordering::Acquire) {
                break;
            }
        }
        changed
    }

    pub(super) fn drain_internal_events_with_forwarding_up_to(
        &mut self,
        limit: usize,
    ) -> (bool, bool) {
        let mut had_event = false;
        let mut changed = false;
        for _ in 0..limit {
            let Ok(ev) = self.app.event_rx.try_recv() else {
                break;
            };
            had_event = true;
            changed |= self.handle_internal_event_with_forwarding(ev);
        }
        (had_event, changed)
    }
}
