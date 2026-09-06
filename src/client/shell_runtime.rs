use super::*;

pub(super) fn dispatch_client_shell_actions(
    actions: Vec<shell::ClientShellAction>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    endpoints: &mut endpoint::EndpointRegistry,
    mut shell: Option<&mut shell::ClientShellState>,
    detached_process_children: &mut Vec<std::process::Child>,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
) -> Result<(Vec<crossterm::event::MouseEvent>, bool), ClientError> {
    let mut replay_mouse = Vec::new();
    let mut repaint = false;
    for action in actions {
        match action {
            shell::ClientShellAction::Endpoint {
                endpoint_id,
                boot_id,
                request,
            } => {
                if let Some(connection) = endpoints.connection(&endpoint_id).filter(|_| {
                    endpoints.active_id() == &endpoint_id && endpoints.active_surface_available()
                }) {
                    endpoint_commands.enqueue(endpoint_id, connection.generation, boot_id, request);
                } else if let Some(shell) = shell.as_deref_mut() {
                    repaint |= shell.cancel_endpoint_request(&request.id);
                }
            }
            shell::ClientShellAction::ClipboardWrite(bytes) => {
                crate::selection::write_osc52_bytes(&bytes);
            }
            shell::ClientShellAction::ActivateEndpoint {
                endpoint_id,
                target,
            } => {
                let _ = event_tx.try_send(ClientLoopEvent::ActivateEndpoint {
                    endpoint_id,
                    target,
                    force: false,
                });
            }
            shell::ClientShellAction::OpenSafeWebUrl(url) => {
                if crate::app::actions::safe_web_url(&url).is_some() {
                    match crate::platform::open_url(&url) {
                        Ok(Some(child)) => detached_process_children.push(child),
                        Ok(None) => {}
                        Err(err) => warn!(err = %err, url = %url, "failed to open pane URL"),
                    }
                }
            }
            shell::ClientShellAction::ReplayMouse(events) => replay_mouse.extend(events),
            shell::ClientShellAction::Keybind(action) => {
                debug!(
                    ?action,
                    "client shell action awaits its presentation family"
                );
            }
        }
    }
    // A source-off-first handoff leaves the registry's committed identity pointing at a
    // deliberately surface-inactive source. Do not drain its retained queue into a server that
    // must reject it; completion below resumes the committed owner's lane.
    if endpoints.active_surface_available() {
        let active_endpoint = endpoints.active_id().clone();
        let cancelled = endpoint_commands.send_next(&active_endpoint, endpoints);
        if let Some(shell) = shell {
            for request_id in cancelled {
                repaint |= shell.cancel_endpoint_request(&request_id);
            }
        }
    }
    Ok((replay_mouse, repaint))
}

pub(super) fn client_shell_resize_message(
    shell: &shell::ClientShellState,
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    pixel_mouse: bool,
) -> ClientMessage {
    ClientMessage::ClientShellResize {
        cell_width_px,
        cell_height_px,
        surface_size: shell.surface_size(cols, rows),
        pixel_mouse,
    }
}

pub(super) fn sync_client_shell_keyboard_report_all(
    state: &mut ClientState,
) -> Result<(), ClientError> {
    let Some(shell) = state.shell.as_ref() else {
        return Ok(());
    };
    let desired = state.pane_keyboard_report_all || shell.host_keyboard_report_all_requested();
    if desired == state.keyboard_report_all_active {
        return Ok(());
    }
    crate::terminal_modes::set_host_kitty_keyboard_report_all(&mut io::stdout(), desired)
        .map_err(ClientError::ConnectionFailed)?;
    state.keyboard_report_all_active = desired;
    Ok(())
}

pub(super) fn clear_endpoint_host_effects(
    state: &mut ClientState,
    host_mouse_capture_active: &std::sync::atomic::AtomicBool,
    host_sgr_pixels_active: &std::sync::atomic::AtomicBool,
) {
    state.endpoint_mouse_capture_requested = false;
    state.endpoint_sgr_pixels_requested = false;
    let enabled = if state.shell.is_some() {
        state.shell_mouse_capture_preference
    } else {
        state.direct_mouse_capture_preference
    };
    let sgr_pixels = super::effective_sgr_pixel_mouse(enabled, false, state.pixel_geometry_exact);
    if enabled != state.mouse_capture_active
        || sgr_pixels != host_sgr_pixels_active.load(std::sync::atomic::Ordering::Acquire)
    {
        let _ = super::set_mouse_capture(enabled, sgr_pixels);
    }
    state.mouse_capture_active = enabled;
    host_mouse_capture_active.store(enabled, std::sync::atomic::Ordering::Release);
    host_sgr_pixels_active.store(sgr_pixels, std::sync::atomic::Ordering::Release);

    state.pane_keyboard_report_all = false;
    let _ = sync_client_shell_keyboard_report_all(state);
    let _ = crate::terminal_effects::write_window_title(&mut std::io::stdout(), None);
}

pub(super) fn apply_client_shell_input_source_changes(
    state: &mut ClientState,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
) {
    let changes = state
        .shell
        .as_mut()
        .map(shell::ClientShellState::take_input_source_changes)
        .unwrap_or_default();
    for active in changes {
        if active {
            prefix_input_source.switch_to_ascii();
        } else {
            prefix_input_source.restore();
        }
    }
}

fn install_pending_activation(
    state: &mut ClientState,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    next_surface_serial: &mut u64,
    activation: endpoint::PendingEndpointActivation,
) {
    let retired = activation
        .source_command_lane()
        .map(|source| endpoint_commands.retire_lane(source))
        .unwrap_or_default();
    if let Some(shell) = state.shell.as_mut() {
        for request_id in retired {
            shell.cancel_endpoint_request(&request_id);
        }
    }
    *next_surface_serial = next_surface_serial.saturating_add(1);
    state.freeze_presentation();
    *pending = Some(activation);
}

pub(super) fn begin_endpoint_activation(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    next_surface_serial: &mut u64,
    endpoint_id: endpoint::ClientEndpointId,
    target: Option<shell::ClientEndpointFocusTarget>,
    force: bool,
    now: std::time::Instant,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
) -> Result<(), ClientError> {
    if let Some(activation) = pending.as_mut() {
        if activation.can_retarget(&endpoint_id) {
            let retarget_error = activation.retarget(target, endpoints).err();
            if let Some(error) = retarget_error {
                rollback_endpoint_activation(state, endpoints, pending, error, false);
            }
        } else {
            // Once rollback starts, even a request for the original target is a new intent. It
            // replaces the retained successor instead of mutating the transaction being retired.
            let outcome = activation.supersede(endpoint_id, target, endpoints);
            if let endpoint::ActivationRollback::Unavailable(message) = outcome {
                *pending = None;
                present_handoff_unavailable(state, message);
            }
        }
        return Ok(());
    }
    let already_active = !force
        && endpoints.active_id() == &endpoint_id
        && endpoints
            .connection(&endpoint_id)
            .is_some_and(|connection| connection.surface_active);
    if already_active {
        if let (Some(shell), Some(target)) = (state.shell.as_mut(), target) {
            let actions = shell.focus_endpoint_target(target);
            let (_, repaint) = dispatch_client_shell_actions(
                actions,
                endpoint_commands,
                endpoints,
                Some(shell),
                &mut state.detached_process_children,
                event_tx,
            )?;
            if repaint {
                if let Some(frame) = shell.compose(state.reported_size.0, state.reported_size.1) {
                    state.present_frame(frame);
                }
            }
        }
        return Ok(());
    }
    let Some(shell) = state.shell.as_ref() else {
        return Ok(());
    };
    let resize = client_shell_resize_message(
        shell,
        state.reported_size.0,
        state.reported_size.1,
        state.reported_cell_size.0,
        state.reported_cell_size.1,
        state.pixel_geometry_exact,
    );
    match endpoint::PendingEndpointActivation::begin(
        shell,
        endpoints,
        endpoint_id.clone(),
        target,
        resize,
        *next_surface_serial,
        now,
    ) {
        Ok(activation) => install_pending_activation(
            state,
            endpoint_commands,
            pending,
            next_surface_serial,
            activation,
        ),
        Err(endpoint::ActivationBeginError::Preflight(error)) => {
            if let Some(shell) = state.shell.as_mut() {
                shell.receive_endpoint_unavailable(format!(
                    "{}: {error}",
                    shell.endpoint_label(&endpoint_id)
                ));
            }
        }
        Err(endpoint::ActivationBeginError::Partial { activation, error }) => {
            // A send error is not evidence that its peer did not observe the write. Freeze and
            // retain the lifecycle object before rollback so no source or target output can be
            // projected until one ownership path has been proved again.
            install_pending_activation(
                state,
                endpoint_commands,
                pending,
                next_surface_serial,
                *activation,
            );
            rollback_endpoint_activation(
                state,
                endpoints,
                pending,
                format!(
                    "{}: {error}",
                    state
                        .shell
                        .as_ref()
                        .map(|shell| shell.endpoint_label(&endpoint_id).to_owned())
                        .unwrap_or_else(|| format!("{endpoint_id:?}"))
                ),
                false,
            );
        }
    }
    Ok(())
}

pub(super) fn complete_endpoint_activation(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
) -> Result<Option<ClientLoopEvent>, ClientError> {
    let sync_endpoint = pending
        .as_ref()
        .and_then(endpoint::PendingEndpointActivation::presentation_sync_endpoint)
        .cloned();
    if let Some(endpoint_id) = sync_endpoint.as_ref() {
        state.replay_host_theme(endpoints, endpoint_id);
    }
    let completion = {
        let Some(activation) = pending.as_mut() else {
            return Ok(None);
        };
        let Some(shell) = state.shell.as_mut() else {
            return Ok(None);
        };
        match activation.complete(shell, endpoints) {
            Ok(completion) => completion,
            Err(error) => {
                shell.receive_endpoint_unavailable(error);
                return Ok(None);
            }
        }
    };

    if matches!(
        completion,
        endpoint::ActivationCompletion::AwaitingPresentationSync { .. }
    ) {
        #[cfg(unix)]
        if let endpoint::ActivationCompletion::AwaitingPresentationSync { previous, endpoint } =
            &completion
        {
            if previous != endpoint {
                state.retire_endpoint_graphics(previous);
            }
        }
        // The coherent target frame can replace the frozen source now, but the registry keeps
        // pane input disabled until a second projection epoch has replayed host modes/effects.
        state.unfreeze_presentation();
        let (cleanup, frame) = {
            let shell = state.shell.as_mut().expect("checked client shell");
            (
                shell.take_pending_graphics_cleanup(),
                shell.compose(state.reported_size.0, state.reported_size.1),
            )
        };
        state.present_graphics(&cleanup);
        if let Some(frame) = frame {
            state.present_frame(frame);
        }
        return Ok(None);
    }
    if completion == endpoint::ActivationCompletion::AwaitingPresentationEffects {
        return Ok(None);
    }

    let _ = pending.take();
    endpoints.unfreeze_input();
    let successor = match completion {
        endpoint::ActivationCompletion::RestoredSource {
            error,
            successor: next,
            ..
        } => {
            if next.is_none() {
                if let Some(shell) = state.shell.as_mut() {
                    shell.receive_endpoint_unavailable(error);
                }
            }
            next
        }
        endpoint::ActivationCompletion::Activated => None,
        endpoint::ActivationCompletion::AwaitingPresentationSync { .. }
        | endpoint::ActivationCompletion::AwaitingPresentationEffects => unreachable!(),
    };
    state.unfreeze_presentation();
    if successor.is_none() {
        let active_endpoint = endpoints.active_id().clone();
        let cancelled = endpoint_commands.send_next(&active_endpoint, endpoints);
        if let Some(shell) = state.shell.as_mut() {
            for request_id in cancelled {
                shell.cancel_endpoint_request(&request_id);
            }
        }
    }
    let (cleanup, frame) = {
        let shell = state.shell.as_mut().expect("checked client shell");
        (
            shell.take_pending_graphics_cleanup(),
            shell.compose(state.reported_size.0, state.reported_size.1),
        )
    };
    state.present_graphics(&cleanup);
    if let Some(frame) = frame {
        state.present_frame(frame);
    }
    if let Some(intent) = successor {
        return Ok(Some(ClientLoopEvent::ActivateEndpoint {
            endpoint_id: intent.endpoint_id,
            target: intent.target,
            force: true,
        }));
    }
    Ok(None)
}

pub(super) fn present_handoff_unavailable(state: &mut ClientState, message: String) {
    // An unavailable committed endpoint has no presentation lease. Keep all pane input and late
    // source output blocked, while allowing this client-owned chrome frame through the freeze.
    state.freeze_presentation();
    let frame = state.shell.as_mut().and_then(|shell| {
        shell.receive_endpoint_unavailable(message);
        shell.compose(state.reported_size.0, state.reported_size.1)
    });
    if let Some(frame) = frame {
        state.present_frozen_chrome(frame);
    }
}

pub(super) fn rollback_endpoint_activation(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    error: String,
    source_release_rejected: bool,
) {
    let Some(activation) = pending.as_mut() else {
        return;
    };
    match activation.rollback(endpoints, error.clone(), source_release_rejected) {
        endpoint::ActivationRollback::Pending => state.freeze_presentation(),
        endpoint::ActivationRollback::Unavailable(message) => {
            *pending = None;
            // No endpoint has been proven safe to present. Keep pane input frozen, but render
            // the client-owned unavailable chrome rather than silently swallowing the error.
            present_handoff_unavailable(state, message);
        }
    }
}

pub(super) fn handle_endpoint_disconnect(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    supervisors: &mut endpoint::EndpointSupervisors,
    pending_activation: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    now: std::time::Instant,
    notice: &str,
) -> bool {
    supervisors.disconnected(endpoint_id, generation, now);
    #[cfg(unix)]
    state.retire_endpoint_graphics(endpoint_id);
    if pending_activation
        .as_ref()
        .is_some_and(|pending| pending.involves_endpoint(endpoint_id))
    {
        let outcome = pending_activation
            .as_mut()
            .expect("checked pending activation")
            .endpoint_disconnected(
                endpoints,
                endpoint_id,
                format!("endpoint connection was lost while activating {notice}"),
            );
        match outcome {
            endpoint::ActivationRollback::Pending => {}
            endpoint::ActivationRollback::Unavailable(error) => {
                *pending_activation = None;
                present_handoff_unavailable(state, error);
            }
        }
    }
    let endpoint_was_active = endpoints.active_id() == endpoint_id;
    let cancelled = endpoint_commands.disconnect(endpoint_id);
    let unavailable = state.shell.as_mut().and_then(|shell| {
        for request_id in cancelled {
            shell.cancel_endpoint_request(&request_id);
        }
        shell.mark_endpoint_disconnected(endpoint_id);
        endpoint_was_active.then(|| format!("{} {notice}", shell.endpoint_label(endpoint_id)))
    });
    if let Some(message) = unavailable {
        present_handoff_unavailable(state, message);
    } else if let Some(frame) = state
        .shell
        .as_mut()
        .and_then(|shell| shell.compose(state.reported_size.0, state.reported_size.1))
    {
        state.present_frame(frame);
    }
    endpoint_was_active
}

pub(super) fn handle_endpoint_attention(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    supervisors: &mut endpoint::EndpointSupervisors,
    pending_activation: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    now: std::time::Instant,
    message: String,
) -> bool {
    endpoints.disconnect(endpoint_id);
    supervisors.record_status(
        endpoint_id,
        generation,
        endpoint::ClientEndpointStatus::Attention,
        now,
    );
    #[cfg(unix)]
    state.retire_endpoint_graphics(endpoint_id);
    if pending_activation
        .as_ref()
        .is_some_and(|pending| pending.involves_endpoint(endpoint_id))
    {
        let outcome = pending_activation
            .as_mut()
            .expect("checked pending activation")
            .endpoint_disconnected(
                endpoints,
                endpoint_id,
                "endpoint reported attention while activating".into(),
            );
        if let endpoint::ActivationRollback::Unavailable(error) = outcome {
            *pending_activation = None;
            present_handoff_unavailable(state, error);
        }
    }
    let endpoint_was_active = endpoints.active_id() == endpoint_id;
    let cancelled = endpoint_commands.disconnect(endpoint_id);
    let unavailable = state.shell.as_mut().and_then(|shell| {
        for request_id in cancelled {
            shell.cancel_endpoint_request(&request_id);
        }
        shell.set_endpoint_status(endpoint_id, endpoint::ClientEndpointStatus::Attention);
        endpoint_was_active.then(|| format!("{}: {message}", shell.endpoint_label(endpoint_id)))
    });
    if let Some(message) = unavailable {
        present_handoff_unavailable(state, message);
    } else if let Some(frame) = state
        .shell
        .as_mut()
        .and_then(|shell| shell.compose(state.reported_size.0, state.reported_size.1))
    {
        state.present_frame(frame);
    }
    endpoint_was_active
}

pub(super) fn install_client_shell_snapshot(
    state: &mut ClientState,
    endpoint_id: &endpoint::ClientEndpointId,
    snapshot: Box<crate::protocol::ClientShellSnapshot>,
    projection_pending: bool,
    endpoints: &mut endpoint::EndpointRegistry,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
) -> Result<(), ClientError> {
    let Some(connection) = endpoints.connection(endpoint_id) else {
        return Ok(());
    };
    let generation = connection.generation;
    let project_snapshot =
        !projection_pending && endpoints.active_id() == endpoint_id && connection.surface_active;
    let (composed, resize, graphics_cleanup) = if let Some(shell) = &mut state.shell {
        let waits_for_selected_surface = projection_pending
            || (endpoints.active_id() == endpoint_id
                && !project_snapshot
                && shell.has_presented_surface());
        let previous_size = shell.surface_size(state.reported_size.0, state.reported_size.1);
        if !waits_for_selected_surface {
            shell.set_endpoint_status(endpoint_id, endpoint::ClientEndpointStatus::Online);
        }
        if project_snapshot {
            shell.set_endpoint_snapshot_for_generation(endpoint_id, generation, snapshot);
        } else {
            shell.cache_endpoint_snapshot_inactive_for_generation(
                endpoint_id,
                generation,
                snapshot,
            );
        }
        let graphics_cleanup = shell.take_pending_graphics_cleanup();
        let next_size = shell.surface_size(state.reported_size.0, state.reported_size.1);
        (
            shell.compose(state.reported_size.0, state.reported_size.1),
            (previous_size != next_size).then(|| {
                client_shell_resize_message(
                    shell,
                    state.reported_size.0,
                    state.reported_size.1,
                    state.reported_cell_size.0,
                    state.reported_cell_size.1,
                    state.pixel_geometry_exact,
                )
            }),
            graphics_cleanup,
        )
    } else {
        (None, None, Vec::new())
    };
    apply_client_shell_input_source_changes(state, prefix_input_source);
    state.present_graphics(&graphics_cleanup);
    if let Some(resize) = resize {
        endpoints.send_to(endpoint_id, &resize);
    }
    if let Some(frame) = composed {
        if projection_pending {
            state.present_frame(frame);
        } else {
            state.present_frozen_chrome(frame);
        }
    }
    Ok(())
}

pub(super) fn finish_client_shell_input(
    state: &mut ClientState,
    outcome: shell::ClientShellInput,
    frame: Option<FrameData>,
    endpoints: &mut endpoint::EndpointRegistry,
    pending_activation: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
) -> Result<bool, ClientError> {
    apply_client_shell_input_source_changes(state, prefix_input_source);
    if outcome.detach {
        let _ = write_to_server(endpoints, &ClientMessage::Detach);
        return Ok(true);
    }
    if outcome.resize {
        let shell = state.shell.as_ref().expect("shell mode remains active");
        let resize = client_shell_resize_message(
            shell,
            state.reported_size.0,
            state.reported_size.1,
            state.reported_cell_size.0,
            state.reported_cell_size.1,
            state.pixel_geometry_exact,
        );
        if let Some(activation) = pending_activation.as_mut() {
            if let Err(error) = activation.update_resize(resize, endpoints) {
                rollback_endpoint_activation(state, endpoints, pending_activation, error, false);
            }
        } else {
            let _ = write_to_server(endpoints, &resize);
        }
    }
    #[cfg(not(windows))]
    if outcome.query_host_appearance {
        query_host_terminal_appearance();
    }
    if outcome.query_host_theme {
        query_host_terminal_theme();
    }
    sync_client_shell_keyboard_report_all(state)?;
    let (replay, dispatch_repaint) = dispatch_client_shell_actions(
        outcome.actions,
        endpoint_commands,
        endpoints,
        state.shell.as_mut(),
        &mut state.detached_process_children,
        event_tx,
    )?;
    let frame = if dispatch_repaint {
        state
            .shell
            .as_mut()
            .and_then(|shell| shell.compose(state.reported_size.0, state.reported_size.1))
    } else {
        frame
    };
    debug_assert!(
        replay.is_empty(),
        "mouse replay only follows endpoint results"
    );
    let active_endpoint_online = state
        .shell
        .as_ref()
        .is_none_or(|shell| shell.endpoint_is_online(endpoints.active_id()))
        && endpoints.active_surface_available();
    for request in outcome.requests {
        if let ClientMessage::ClientShellHostTheme { update } = &request {
            state.record_host_theme_update(update);
            if let Some(activation) = pending_activation.as_mut() {
                if let Err(error) = activation.update_host_theme(update.clone(), endpoints) {
                    rollback_endpoint_activation(
                        state,
                        endpoints,
                        pending_activation,
                        error,
                        false,
                    );
                }
                continue;
            }
        }
        // Host focus belongs to a pending target even when the source has gone offline or has
        // already had its surface revoked. Route it before the ordinary source-online gate.
        if let ClientMessage::ClientShellFocus { focused } = request {
            if let Some(activation) = pending_activation.as_mut() {
                if let Err(error) = activation.update_host_focus(focused, endpoints) {
                    rollback_endpoint_activation(
                        state,
                        endpoints,
                        pending_activation,
                        error,
                        false,
                    );
                }
                continue;
            }
            if active_endpoint_online {
                write_to_server(endpoints, &ClientMessage::ClientShellFocus { focused })
                    .map_err(ClientError::ConnectionLost)?;
            }
            continue;
        }
        if !active_endpoint_online {
            continue;
        }
        if pending_activation.is_some() {
            // Pane input and non-focus host effects do not cross the frozen handoff boundary.
            continue;
        }
        write_to_server(endpoints, &request).map_err(ClientError::ConnectionLost)?;
    }
    if let Some(frame) = frame {
        if pending_activation.is_some() {
            state.present_frame(frame);
        } else {
            state.present_frozen_chrome(frame);
        }
    }
    Ok(false)
}
