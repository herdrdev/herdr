use super::*;

impl HeadlessServer {
    /// Apply a client-shell surface lease and return the resulting projection floor.
    ///
    /// Every `active: true` request advances the floor, even if the server already considered
    /// the connection active. That makes a new client activation epoch distinguishable from a
    /// delayed same-boot PaneSurface that was prepared for an earlier epoch.
    pub(super) fn set_client_shell_surface_active(
        &mut self,
        client_id: u64,
        active: bool,
    ) -> Option<(bool, u64)> {
        let focus_before = self.shell_focus_targets();
        let focused_tabs_before = self.focused_shell_tabs();
        let (changed, projection_revision) = {
            let client = self.clients.get_mut(&client_id)?;
            if !client.is_shell_client() {
                return None;
            }
            let changed = client.shell_surface_active != active;
            if active {
                client.shell_projection_revision =
                    client.shell_projection_revision.saturating_add(1);
                // Force the next control snapshot to carry this new floor instead of reusing a
                // same-boot cached snapshot from the prior surface epoch.
                client.shell_snapshot = None;
            }
            if !changed && !active {
                return Some((false, client.shell_projection_revision));
            }
            client.shell_surface_active = active;
            client.request_repaint();
            client.shell_graphics_delivery = Default::default();
            // The client drops target effects while its old source frame is frozen. Reset the
            // dedupe state whenever a viewer is (re)activated so the post-commit replay can
            // produce mouse/keyboard modes and graphics even when runtime demand is unchanged.
            if active {
                client.host_mouse_capture_active = None;
                client.host_sgr_pixels_active = None;
                client.host_keyboard_report_all_active = None;
            }
            client.clear_deferred_render();
            if !active {
                if let Some(writer) = &client.writer {
                    writer.discard_pending_render();
                }
            }
            (changed, client.shell_projection_revision)
        };
        let held_inputs = (!active && changed).then(|| {
            self.clients
                .get_mut(&client_id)
                .expect("checked client")
                .drain_shell_held_inputs()
        });

        if let Some(held_inputs) = held_inputs {
            self.release_client_shell_inputs(client_id, held_inputs);
            self.retire_direct_graphics_for_client(client_id);
        }
        if changed {
            self.finish_shell_location_reconciliation(focus_before, &focused_tabs_before);
        }

        if active {
            self.promote_client_to_foreground(client_id);
            // Do not emit/cache a title during a frozen target activation. A committed client
            // explicitly requests the bounded replay after its coherent frame is visible.
            self.sent_window_title = None;
            self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(true);
            let focused_viewer_already_owns_tab = self
                .shell_tab_id_for_client(client_id)
                .is_some_and(|tab_id| {
                    self.clients.iter().any(|(&other_id, client)| {
                        other_id != client_id
                            && client.is_active_shell_client()
                            && client.outer_terminal_focus == Some(true)
                            && self.shell_tab_id_for_client(other_id).as_deref()
                                == Some(tab_id.as_str())
                    })
                });
            if !focused_viewer_already_owns_tab {
                self.claim_shell_tab_geometry(client_id, true);
            }
        } else {
            self.tab_geometry_controllers
                .retain(|_, controller_id| *controller_id != client_id);
            if self.foreground_client_id == Some(client_id) {
                self.promote_latest_remaining_client();
                self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(true);
            } else {
                self.sync_foreground_client_state();
            }
            self.reapply_controlled_shell_tab_geometry(true);
        }
        Some((changed || active, projection_revision))
    }
}
