use super::*;

impl HeadlessServer {
    fn shell_focused_runtime(
        &self,
        client_id: u64,
    ) -> Option<(&crate::terminal::TerminalRuntime, crate::layout::PaneId)> {
        if self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id) {
            if let Some(popup) = &self.app.state.popup_pane {
                return self
                    .app
                    .terminal_runtimes
                    .get(&popup.terminal_id)
                    .map(|runtime| (runtime, popup.pane_id));
            }
        }
        let target = self.shell_target_for_client(client_id)?;
        let tab = self
            .app
            .state
            .workspaces
            .get(target.workspace_index)?
            .tabs
            .get(target.tab_index)?;
        let pane_id = tab.layout.focused();
        self.app
            .state
            .runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                target.workspace_index,
                pane_id,
            )
            .map(|runtime| (runtime, pane_id))
    }

    pub(super) fn stream_host_mouse_capture_mode(&mut self) {
        let requested = self
            .clients
            .iter()
            .filter_map(|(&client_id, client)| match &client.mode {
                ClientConnectionMode::ClientShell => {
                    let focused = self.shell_focused_runtime(client_id);
                    let child_requests_mouse =
                        focused.is_some_and(|(runtime, _)| runtime.mouse_reporting_enabled());
                    let sgr_pixels = client.pixel_mouse
                        && focused.is_some_and(|(runtime, pane_id)| {
                            self.app.pane_graphics.active_for_pane(pane_id)
                                && runtime.sgr_pixel_mouse_enabled()
                        });
                    Some((
                        client_id,
                        client.shell_mouse_capture || child_requests_mouse,
                        sgr_pixels,
                    ))
                }
                ClientConnectionMode::TerminalAttach { terminal_id } => {
                    let runtime = self.runtime_for_terminal_id_string(terminal_id);
                    let child_requests_mouse = runtime
                        .is_some_and(crate::terminal::TerminalRuntime::mouse_reporting_enabled);
                    let sgr_pixels = child_requests_mouse
                        && client.pixel_mouse
                        && runtime
                            .is_some_and(crate::terminal::TerminalRuntime::sgr_pixel_mouse_enabled);
                    Some((client_id, child_requests_mouse, sgr_pixels))
                }
                ClientConnectionMode::TerminalPending
                | ClientConnectionMode::TerminalObserve { .. } => None,
            })
            .collect::<Vec<_>>();

        let mut broken_clients = Vec::new();
        for (client_id, enabled, sgr_pixels) in requested {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_mouse_capture_active == Some(enabled)
                && client.host_sgr_pixels_active == Some(sgr_pixels)
            {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized = match Self::frame_server_message(&ServerMessage::MouseCapture {
                enabled,
                sgr_pixels,
            }) {
                Ok(framed) => framed,
                Err(err) => {
                    warn!(err = %err, "failed to serialize mouse capture mode for client");
                    continue;
                }
            };
            if writer.control.send(serialized).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during mouse capture update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_mouse_capture_active = Some(enabled);
            client.host_sgr_pixels_active = Some(sgr_pixels);
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    pub(super) fn stream_direct_terminal_keyboard_mode(&mut self) {
        let shell_modes = self
            .clients
            .iter()
            .filter(|(_, client)| client.is_shell_client())
            .map(|(&client_id, _)| {
                let report_all =
                    self.shell_focused_runtime(client_id)
                        .is_some_and(|(runtime, _)| {
                            let protocol = runtime.keyboard_protocol();
                            protocol.reports_all_keys()
                                || (protocol.reports_event_types()
                                    && runtime.modify_other_keys_level() > 0)
                        });
                (client_id, report_all)
            })
            .collect::<Vec<_>>();
        let mut broken_clients = Vec::new();
        for (client_id, report_all) in shell_modes {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_keyboard_report_all_active == Some(report_all) {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized = match Self::frame_server_message(
                &ServerMessage::ClientShellKeyboardReportAll {
                    enabled: report_all,
                },
            ) {
                Ok(serialized) => serialized,
                Err(err) => {
                    warn!(err = %err, "failed to serialize client shell keyboard report-all mode");
                    continue;
                }
            };
            if writer.control.send(serialized).is_err() {
                broken_clients.push(client_id);
                continue;
            }
            client.host_keyboard_report_all_active = Some(report_all);
        }

        let requested = self
            .clients
            .iter()
            .filter_map(|(&client_id, client)| {
                let ClientConnectionMode::TerminalAttach { terminal_id } = &client.mode else {
                    return None;
                };
                let (flags, modify_other_keys_level) = self
                    .runtime_for_terminal_id_string(terminal_id)
                    .map_or((0, 0), |runtime| {
                        let flags = match runtime.keyboard_protocol() {
                            crate::input::KeyboardProtocol::Legacy => 0,
                            crate::input::KeyboardProtocol::Kitty { flags } => flags,
                        };
                        (flags, runtime.modify_other_keys_level())
                    });
                Some((client_id, flags, modify_other_keys_level))
            })
            .collect::<Vec<_>>();

        for (client_id, flags, modify_other_keys_level) in requested {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_keyboard_protocol_active == Some((flags, modify_other_keys_level)) {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized =
                match Self::frame_server_message(&ServerMessage::DirectTerminalKeyboardProtocol {
                    flags,
                    modify_other_keys_level,
                }) {
                    Ok(framed) => framed,
                    Err(err) => {
                        warn!(err = %err, "failed to serialize direct terminal keyboard mode");
                        continue;
                    }
                };
            if writer.control.send(serialized).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during direct terminal keyboard update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_keyboard_protocol_active = Some((flags, modify_other_keys_level));
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    pub(super) fn has_pending_presentation_work(
        &self,
        needs_full_render: bool,
        needs_graphics_render: bool,
    ) -> bool {
        needs_full_render || needs_graphics_render || self.app.render_dirty.has_immediate_work()
    }

    pub(super) fn sync_immediate_pty_sources(&self) {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        let mut pane_ids = HashSet::new();
        if has_app_target {
            for (&client_id, client) in &self.clients {
                if !client.is_shell_client() || client.writer.is_none() {
                    continue;
                }
                let Some(target) = self.shell_target_for_client(client_id) else {
                    continue;
                };
                let Some(tab) = self
                    .app
                    .state
                    .workspaces
                    .get(target.workspace_index)
                    .and_then(|workspace| workspace.tabs.get(target.tab_index))
                else {
                    continue;
                };
                if tab.zoomed {
                    pane_ids.insert(tab.layout.focused());
                } else {
                    pane_ids.extend(tab.layout.pane_ids());
                }
                if self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id) {
                    if let Some(popup) = &self.app.state.popup_pane {
                        pane_ids.insert(popup.pane_id);
                    }
                }
            }
        }
        if !direct_terminal_targets.is_empty() {
            for workspace in &self.app.state.workspaces {
                for tab in &workspace.tabs {
                    pane_ids.extend(tab.panes.iter().filter_map(|(&pane_id, pane)| {
                        direct_terminal_targets
                            .contains(pane.attached_terminal_id.as_str())
                            .then_some(pane_id)
                    }));
                }
            }
            if let Some(popup) = &self.app.state.popup_pane {
                if direct_terminal_targets.contains(popup.terminal_id.as_str()) {
                    pane_ids.insert(popup.pane_id);
                }
            }
        }
        self.app.render_dirty.set_immediate_pty_sources(pane_ids);
    }

    fn pty_render_targets(&self) -> (bool, HashSet<&str>) {
        let mut has_app_target = false;
        let mut direct_terminal_targets = HashSet::new();
        for client in self
            .clients
            .values()
            .filter(|client| client.writer.is_some())
        {
            match &client.mode {
                ClientConnectionMode::ClientShell => has_app_target = true,
                ClientConnectionMode::TerminalAttach { terminal_id }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    direct_terminal_targets.insert(terminal_id.as_str());
                }
                ClientConnectionMode::TerminalPending => {}
            }
        }
        (has_app_target, direct_terminal_targets)
    }

    fn pty_source_visible_to_render_targets(
        &self,
        pane_id: crate::layout::PaneId,
        has_app_target: bool,
        direct_terminal_targets: &HashSet<&str>,
    ) -> bool {
        let terminal_id = self.terminal_id_for_pane(pane_id);
        (has_app_target && (terminal_id.is_none() || self.any_shell_surface_contains_pane(pane_id)))
            || terminal_id.is_none_or(|source| direct_terminal_targets.contains(source.as_str()))
    }

    pub(super) fn pty_sources_visible_to_any_render_target(
        &self,
        sources: &HashSet<crate::layout::PaneId>,
    ) -> bool {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        if !has_app_target && direct_terminal_targets.is_empty() {
            return false;
        }

        sources.iter().copied().any(|pane_id| {
            self.pty_source_visible_to_render_targets(
                pane_id,
                has_app_target,
                &direct_terminal_targets,
            )
        })
    }

    fn terminal_id_for_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalId> {
        if let Some(popup) = self
            .app
            .state
            .popup_pane
            .as_ref()
            .filter(|popup| popup.pane_id == pane_id)
        {
            return Some(&popup.terminal_id);
        }
        self.app
            .find_pane(pane_id)
            .map(|(_, pane)| &pane.attached_terminal_id)
    }

    fn any_shell_surface_contains_pane(&self, pane_id: crate::layout::PaneId) -> bool {
        self.clients.iter().any(|(&client_id, client)| {
            if !client.is_shell_client() || client.writer.is_none() {
                return false;
            }
            if self
                .app
                .state
                .popup_pane
                .as_ref()
                .is_some_and(|popup| popup.pane_id == pane_id)
            {
                return self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id);
            }
            let Some(target) = self.shell_target_for_client(client_id) else {
                return false;
            };
            let Some(tab) = self
                .app
                .state
                .workspaces
                .get(target.workspace_index)
                .and_then(|workspace| workspace.tabs.get(target.tab_index))
            else {
                return false;
            };
            tab.panes.contains_key(&pane_id) && (!tab.zoomed || tab.layout.focused() == pane_id)
        })
    }

    pub(super) fn render_and_stream(&mut self) {
        let full_started = crate::render_prof::timer();
        let render_targets = render_targets(&self.clients, self.foreground_client_id);

        if render_targets.is_empty() {
            let (cols, rows) = self.effective_size;
            let area = Rect::new(0, 0, cols, rows);
            let resize_panes = self.app.state.view.pane_infos.is_empty();
            if resize_panes {
                crate::ui::compute_view_with_runtime_registry(
                    &mut self.app.state,
                    &self.app.terminal_runtimes,
                    area,
                );
            } else {
                crate::ui::compute_view_without_resizing_panes(
                    &mut self.app.state,
                    &self.app.terminal_runtimes,
                    area,
                );
            }
            self.app.full_redraw_pending = false;
            crate::render_prof::duration_since("full_render.total", full_started);
            debug!(
                cols,
                rows, resize_panes, "updated geometry with no attached clients"
            );
            return;
        }

        let mut broken_clients: Vec<u64> = Vec::new();
        for (client_id, (cols, rows), cell_size, _is_foreground, mode) in render_targets {
            let area = Rect::new(0, 0, cols, rows);
            let shell_target = self.shell_target_for_client(client_id);
            let shell_tab_id = self.shell_tab_id_for_client(client_id);
            let shell_shows_popup = shell_tab_id.as_deref() == self.popup_owner_tab_id.as_deref();
            let mut shell_projection_revision = 0;
            if matches!(mode, ClientConnectionMode::ClientShell) {
                let location = self
                    .clients
                    .get(&client_id)
                    .and_then(|client| client.shell_location.clone());
                let Some(client) = self.clients.get_mut(&client_id) else {
                    continue;
                };
                let mut candidate = client_shell_snapshot(
                    &self.app,
                    &self.client_shell_boot_id,
                    client.shell_projection_revision,
                    None,
                    location.as_ref(),
                );
                candidate.config_diagnostic = if client.shell_uses_endpoint_keybindings {
                    self.server_config_diagnostic.clone()
                } else {
                    self.server_config_diagnostic_without_keybindings.clone()
                };
                candidate.revision = client.shell_projection_revision;
                if client.shell_snapshot.as_ref() != Some(&candidate) {
                    client.shell_projection_revision =
                        client.shell_projection_revision.saturating_add(1);
                    candidate.revision = client.shell_projection_revision;
                    let message = match crate::protocol::endpoint::snapshot_message(&candidate) {
                        Ok(message) => message,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to encode endpoint snapshot");
                            broken_clients.push(client_id);
                            continue;
                        }
                    };
                    let framed = match Self::frame_server_message(&message) {
                        Ok(framed) => framed,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to frame endpoint snapshot");
                            broken_clients.push(client_id);
                            continue;
                        }
                    };
                    let Some(writer) = client.writer.as_ref() else {
                        broken_clients.push(client_id);
                        continue;
                    };
                    if writer.control.send(framed).is_err() {
                        broken_clients.push(client_id);
                        continue;
                    }
                    client.shell_snapshot = Some(candidate);
                }
                shell_projection_revision = client.shell_projection_revision;
            }
            let shell_graphics_delivery = self
                .clients
                .get(&client_id)
                .map(|client| client.shell_graphics_delivery.clone())
                .unwrap_or_default();
            let mut surface_parts = None;
            let frame = match mode {
                ClientConnectionMode::ClientShell => {
                    let render_started = crate::render_prof::timer();
                    let render_cell_size = if cell_size.is_known() {
                        cell_size
                    } else {
                        crate::kitty_graphics::HostCellSize::default()
                    };
                    let crate::server::client_shell::RenderedPaneSurface {
                        frame,
                        panes,
                        splits,
                        popup,
                        graphics,
                        graphics_delivery: next_graphics_delivery,
                    } = render_client_shell_pane_surface(
                        &mut self.app,
                        shell_target,
                        area,
                        false,
                        shell_shows_popup,
                        render_cell_size,
                        &shell_graphics_delivery,
                        client_id,
                    );
                    crate::render_prof::duration_since(
                        "full_render.render_tab_surface_virtual",
                        render_started,
                    );
                    surface_parts = Some((panes, splits, popup, graphics, next_graphics_delivery));
                    frame
                }
                ClientConnectionMode::TerminalPending => continue,
                ClientConnectionMode::TerminalAttach { terminal_id }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) else {
                        self.send_to_client(
                            client_id,
                            ServerMessage::ServerShutdown {
                                reason: Some(format!(
                                    "terminal attach ended: terminal {terminal_id} not found"
                                )),
                            },
                        );
                        broken_clients.push(client_id);
                        continue;
                    };
                    let render_started = crate::render_prof::timer();
                    let (buffer, cursor) =
                        crate::server::render_stream::render_terminal_virtual(runtime, area);
                    crate::render_prof::duration_since(
                        "full_render.render_terminal_virtual",
                        render_started,
                    );
                    let hyperlinks_started = crate::render_prof::timer();
                    let hyperlinks = runtime.visible_hyperlinks(area);
                    crate::render_prof::duration_since(
                        "full_render.visible_hyperlinks",
                        hyperlinks_started,
                    );
                    let frame_started = crate::render_prof::timer();
                    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    );
                    crate::render_prof::duration_since("full_render.frame_build", frame_started);
                    frame
                }
            };

            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                crate::render_prof::event("full_render.writer_missing");
                continue;
            };
            let has_graphics = surface_parts
                .as_ref()
                .is_some_and(|(_, _, _, graphics, _)| {
                    !graphics.assets.is_empty()
                        || !graphics.placements.is_empty()
                        || !graphics.retained_assets.is_empty()
                });
            let mut next_shell_graphics_delivery = None;
            let prepared = if let Some((panes, splits, popup, graphics, delivery)) = surface_parts {
                next_shell_graphics_delivery = Some(delivery);
                client
                    .render_state
                    .prepare_pane_surface(protocol::PaneSurfaceFrame {
                        boot_id: self.client_shell_boot_id.clone(),
                        projection_revision: shell_projection_revision,
                        surface_revision: 0,
                        frame,
                        panes,
                        splits,
                        popup,
                        graphics,
                    })
            } else {
                client.render_state.prepare_frame(frame)
            };
            let Some(mut prepared) = prepared else {
                client.clear_deferred_render();
                crate::render_prof::event("full_render.skip_identical");
                continue;
            };
            let max = if has_graphics {
                MAX_GRAPHICS_FRAME_SIZE
            } else {
                crate::protocol::MAX_FRAME_SIZE
            };
            let mut shell_assets_deferred = false;
            let serialized = match Self::frame_server_message_with_max(prepared.message(), max) {
                Ok(frame) => frame,
                Err(protocol::FramingError::Oversized { claimed, max }) if has_graphics => {
                    warn!(
                        client_id,
                        claimed, max, "dropping graphics assets from oversized pane surface"
                    );
                    if !prepared.strip_pane_surface_assets() {
                        crate::render_prof::event("full_render.serialize_oversized");
                        continue;
                    }
                    next_shell_graphics_delivery = None;
                    shell_assets_deferred = true;
                    match Self::frame_server_message(prepared.message()) {
                        Ok(framed) => framed,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to serialize pane surface without assets");
                            broken_clients.push(client_id);
                            crate::render_prof::event("full_render.serialize_error");
                            continue;
                        }
                    }
                }
                Err(protocol::FramingError::Oversized { claimed, max }) => {
                    warn!(
                        client_id,
                        claimed, max, "skipping oversized frame for client"
                    );
                    crate::render_prof::event("full_render.serialize_oversized");
                    continue;
                }
                Err(err) => {
                    warn!(client_id, err = %err, "failed to serialize frame");
                    broken_clients.push(client_id);
                    crate::render_prof::event("full_render.serialize_error");
                    continue;
                }
            };
            let shell_graphics_pending = next_shell_graphics_delivery
                .as_ref()
                .is_some_and(crate::kitty_graphics::surface::DeliveryCache::has_pending);
            match writer.render.try_send(serialized) {
                Ok(()) => {
                    if let Some(delivery) = next_shell_graphics_delivery {
                        client.shell_graphics_delivery = delivery;
                    }
                    client.render_state.commit_sent_frame(prepared);
                    if shell_graphics_pending || shell_assets_deferred {
                        client.defer_full_render();
                    } else {
                        client.clear_deferred_render();
                    }
                    crate::render_prof::event("full_render.sent");
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    broken_clients.push(client_id);
                }
            }
        }

        if !broken_clients.is_empty() {
            for client_id in broken_clients {
                self.remove_client_and_resize_if_needed(client_id);
            }
        }

        let (cols, rows) = self.effective_size;
        // Full-frame recovery is tracked per connection. A slow client must not
        // keep responsive peers on the global full-render path while it waits
        // for its render slot to drain.
        self.app.full_redraw_pending = false;
        crate::render_prof::duration_since("full_render.total", full_started);
        debug!(cols, rows, foreground_client_id = ?self.foreground_client_id, "rendered virtual frame(s)");
    }
}
