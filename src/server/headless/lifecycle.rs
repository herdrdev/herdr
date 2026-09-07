use super::*;

const LIVE_HANDOFF_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(6);

pub(super) fn wait_for_live_handoff_response_write(
    response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
) {
    let Some(response_write_complete) = response_write_complete else {
        return;
    };

    match response_write_complete.recv_timeout(LIVE_HANDOFF_RESPONSE_WRITE_TIMEOUT) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            warn!("timed out waiting for live handoff response write; old server exiting");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            warn!("live handoff response writer disconnected; old server exiting");
        }
    }
}

impl HeadlessServer {
    #[cfg(any(unix, windows))]
    pub(super) fn perform_live_handoff(
        &mut self,
        params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        use crate::server::handoff;
        #[cfg(windows)]
        if !crate::pty::backend::windows_handoff_available()
            || self
                .app
                .terminal_runtimes
                .values()
                .any(|runtime| !runtime.windows_handoff_supported())
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "live handoff requires the bundled ConPTY runtime",
            ));
        }
        let socket_path = handoff::handoff_socket_path();
        let token = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let listener = handoff::bind_listener(&socket_path)?;
        let mut pane_by_terminal = HashMap::new();
        for ws in &self.app.state.workspaces {
            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    pane_by_terminal.insert(pane.attached_terminal_id.clone(), pane_id.raw());
                }
            }
        }
        #[cfg(unix)]
        if pane_by_terminal.len() > handoff::MAX_FDS_PER_HANDOFF {
            let _ = std::fs::remove_file(&socket_path);
            return Err(io::Error::new(io::ErrorKind::InvalidInput,
                format!("live handoff supports at most {} panes in one update; close panes or restart herdr normally", handoff::MAX_FDS_PER_HANDOFF)));
        }

        self.handoff_in_progress = true;
        let mut paused_terminal_ids = Vec::new();
        let mut import_child = None;
        #[cfg(unix)]
        let mut public_sockets_released = false;
        let transaction = (|| {
            #[cfg(windows)]
            {
                self.api_server
                    .as_ref()
                    .ok_or_else(|| io::Error::other("API listener unavailable"))?
                    .pause_listener_for_handoff()?;
                self.client_listener_control.pause()?;
            }
            self.disconnect_all_clients_for_handoff();
            #[cfg(unix)]
            let _ = reject_pending_client_connections(&self.client_listener);
            for terminal_id in pane_by_terminal.keys() {
                if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                    runtime.pause_handoff_reader(Duration::from_secs(2))?;
                    paused_terminal_ids.push(terminal_id.clone());
                }
            }
            let snapshot = crate::persist::capture(
                &self.app.state.workspaces,
                &self.app.state.terminals,
                &self.app.terminal_runtimes,
                self.app.state.active,
                self.app.state.selected,
            );
            let mut entries = Vec::new();
            for (terminal_id, runtime) in self.app.terminal_runtimes.iter() {
                let Some(pane_id) = pane_by_terminal.get(terminal_id).copied() else {
                    continue;
                };
                let mut state = runtime.handoff_runtime_state(pane_id);
                if self
                    .app
                    .state
                    .terminals
                    .get(terminal_id)
                    .is_none_or(|terminal| terminal.persisted_agent_session.is_none())
                {
                    state.initial_history_ansi = runtime.handoff_history_ansi();
                }
                entries.push((runtime, state));
            }
            let manifest = handoff::manifest_for(
                snapshot,
                entries.iter().map(|(_, state)| state.clone()).collect(),
                params.expected_protocol,
                params.expected_version,
                self.api_window_title.clone(),
            );
            let _child = import_child.insert(handoff::spawn_handoff_import(
                params.import_exe.as_deref().map(Path::new),
                &socket_path,
                &token,
            )?);
            #[cfg(windows)]
            let mut stream = {
                let child = _child;
                crate::platform::ensure_same_process_session(child.id())?;
                let panes = entries
                    .iter()
                    .map(|(runtime, _)| runtime.duplicate_windows_handoff(child))
                    .collect::<io::Result<Vec<_>>>()?;
                let api = self
                    .api_server
                    .as_ref()
                    .ok_or_else(|| io::Error::other("API listener unavailable"))?
                    .duplicate_listener_for_handoff(child)?;
                let client = self.client_listener_control.duplicate_for_handoff(child)?;
                handoff::accept_windows_handoff(
                    listener,
                    child,
                    &token,
                    &manifest,
                    panes,
                    [api, client],
                )?
            };
            #[cfg(unix)]
            let mut stream = {
                use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
                let fds = entries
                    .iter()
                    .map(|(runtime, _)| {
                        runtime
                            .duplicate_handoff_fd()
                            .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                let mut stream =
                    handoff::accept_and_validate_on(listener, &socket_path, &token, &manifest)?;
                handoff::send_fds_and_wait_restored(
                    &mut stream,
                    &fds.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>(),
                )?;
                if let Some(api) = &self.api_server {
                    let _ = api.remove_socket_file_if_owned();
                } else {
                    let _ = std::fs::remove_file(crate::api::socket_path());
                }
                if let Some(identity) = &self.client_socket_identity {
                    let _ = remove_socket_file_if_owned(&self.client_socket_path, identity);
                }
                public_sockets_released = true;
                handoff::wait_ready(&mut stream)?;
                stream
            };
            handoff::report_committed(&mut stream)?;
            Ok::<_, io::Error>(stream)
        })();
        let _ = std::fs::remove_file(&socket_path);
        let mut stream = match transaction {
            Ok(stream) => stream,
            Err(error) => {
                // No source authority resumes while the target can still run.
                if let Some(child) = import_child.as_mut() {
                    handoff::cleanup_failed_import_child(child).map_err(|cleanup| {
                        io::Error::other(format!("{error}; replacement cleanup failed: {cleanup}"))
                    })?;
                }
                #[cfg(unix)]
                let restore = if public_sockets_released {
                    self.wait_then_restore_public_sockets_after_failed_handoff()
                } else {
                    Ok(())
                };
                #[cfg(windows)]
                let restore = {
                    let api = self
                        .api_server
                        .as_ref()
                        .map(|api| api.resume_listener_after_handoff())
                        .transpose();
                    let client = self.client_listener_control.resume();
                    api.and(client)
                };
                for terminal_id in &paused_terminal_ids {
                    if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                        runtime.set_handoff_reader_paused(false);
                    }
                }
                self.handoff_in_progress = false;
                return Err(match restore {
                    Ok(()) => error,
                    Err(restore) => io::Error::other(format!(
                        "{error}; source listener restore failed: {restore}"
                    )),
                });
            }
        };
        // COMMIT was fully delivered. OWNED and source release errors cannot roll back.
        #[cfg(windows)]
        {
            if let Some(api) = &mut self.api_server {
                if let Err(err) = api.release_listener_after_handoff() {
                    warn!(%err, "failed to release old API acceptance");
                }
            }
            self.client_socket_identity = None;
            if let Err(err) = self.client_listener_control.release_after_commit() {
                warn!(%err, "failed to release old client acceptance");
            }
        }
        for (terminal_id, runtime) in self.app.terminal_runtimes.drain_for_handoff() {
            if pane_by_terminal.contains_key(&terminal_id) {
                runtime.preserve_for_handoff();
            }
        }
        handoff::wait_owned_ack(&mut stream);
        Ok(())
    }

    pub(super) fn finish_live_handoff_shutdown(&mut self) {
        self.shutting_down = true;
        self.app.state.should_quit = true;
        self.app.policy.persist_session = false;
        info!("live handoff completed; old server exiting");
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn perform_live_handoff(
        &mut self,
        _params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        Err(io::Error::other("live handoff is only supported on Unix"))
    }

    #[cfg(unix)]
    fn restore_public_sockets_after_failed_handoff(&mut self) -> io::Result<()> {
        let api_tx = self
            .api_tx
            .clone()
            .ok_or_else(|| io::Error::other("cannot restore api socket without api sender"))?;
        let api_server = api::start_server_with_stop_control(
            api_tx,
            self.app.event_hub.clone(),
            self.should_quit.clone(),
        )?;

        let client_path = client_socket_path();
        prepare_socket_path(&client_path)?;
        let listener = bind_local_listener(&client_path)?;
        restrict_socket_permissions(&client_path)?;
        let client_socket_identity = socket_file_identity(&client_path)?;
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

        self.api_server = Some(api_server);
        self.client_listener = listener;
        self.client_socket_path = client_path;
        self.client_socket_identity = Some(client_socket_identity);
        Ok(())
    }

    #[cfg(unix)]
    fn wait_then_restore_public_sockets_after_failed_handoff(&mut self) -> io::Result<()> {
        let timeout = crate::server::handoff::COMMIT_TIMEOUT + Duration::from_secs(2);
        wait_for_old_public_sockets_to_close(timeout)?;
        self.restore_public_sockets_after_failed_handoff()
    }

    #[cfg(unix)]
    pub(super) fn nudge_handoff_panes_on_first_client_attach(&mut self) {
        if !self.pending_handoff_repaint_nudge {
            return;
        }
        self.pending_handoff_repaint_nudge = false;
        self.app
            .terminal_runtimes
            .nudge_child_redraw_after_handoff();
    }

    #[cfg(not(unix))]
    pub(super) fn nudge_handoff_panes_on_first_client_attach(&mut self) {}
    /// Initiates graceful shutdown.
    pub(super) fn initiate_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        info!("server shutdown initiated");
        self.shutting_down = true;

        // Clear client-local host graphics, then send ServerShutdown to all connected clients.
        let shutdown_msg = ServerMessage::ServerShutdown {
            reason: Some("server is shutting down".to_owned()),
        };
        self.send_to_all_clients(shutdown_msg);

        // Give client writer threads a moment to flush the shutdown message.
        // A short sleep ensures the message is written to the socket before
        // we close the connections.
        std::thread::sleep(Duration::from_millis(50));

        // Signal the main loop to exit.
        self.should_quit.store(true, Ordering::Release);
        self.app.state.should_quit = true;
    }

    /// Completes the shutdown sequence: send ServerShutdown to clients,
    /// close client connections, remove socket files, and clean up.
    pub(super) async fn complete_shutdown(&mut self) -> io::Result<()> {
        info!("completing server shutdown");
        self.reject_late_client_connections().await;

        // Send ServerShutdown to all remaining clients.
        if !self.clients.is_empty() {
            let shutdown_msg = ServerMessage::ServerShutdown {
                reason: Some("server is shutting down".to_owned()),
            };
            self.send_to_all_clients(shutdown_msg);

            // Give writer threads a moment to flush before closing.
            std::thread::sleep(Duration::from_millis(50));
        }

        // Reject only the requests already queued when shutdown reached cleanup.
        self.reject_queued_api_requests_for_shutdown();

        // Close all client connections.
        let staged_files = self
            .clients
            .drain()
            .flat_map(|(_, client)| client.staged_clipboard_files)
            .collect::<Vec<_>>();
        crate::server::clipboard_image::remove_files(staged_files);

        // Remove socket files.
        self.cleanup_sockets()?;

        Ok(())
    }

    /// Removes socket files created by the server.
    pub(super) fn cleanup_sockets(&self) -> io::Result<()> {
        if let Some(identity) = &self.client_socket_identity {
            if let Err(err) = remove_socket_file_if_owned(&self.client_socket_path, identity) {
                if err.kind() != io::ErrorKind::NotFound {
                    warn!(
                        path = %self.client_socket_path.display(),
                        err = %err,
                        "failed to remove client socket on shutdown"
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(super) fn wait_for_old_public_sockets_to_close(timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let api_socket = api::socket_path();
    let client_socket = client_socket_path();
    while Instant::now() < deadline {
        let api_open = api_socket.exists() && crate::ipc::connect_local_stream(&api_socket).is_ok();
        let client_open =
            client_socket.exists() && crate::ipc::connect_local_stream(&client_socket).is_ok();
        if !api_open && !client_open {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "old server sockets did not close before handoff import bind",
    ))
}
