use super::*;

impl HeadlessServer {
    fn animation_patch_preserves_full_frame(
        client: &crate::server::clients::ClientConnection,
        app_state: &crate::app::state::AppState,
    ) -> bool {
        let presentation = client.presentation_policy(app_state);
        presentation.mode() == crate::app::Mode::Terminal
            && !presentation.owns_input()
            && presentation.tab_surface_visible()
            && !app_state.hover_tooltip_visible
            && !client.dock_presentation.hover_tooltip_visible
            && app_state.toast.is_none()
            && app_state.config_diagnostic.is_none()
            && app_state.copy_feedback.is_none()
            && app_state.popup_pane.is_none()
    }

    /// Updates the visible star field in each cached app frame without laying
    /// out or drawing the rest of the client UI.
    pub(super) fn render_sidebar_animation_and_stream(&mut self) -> bool {
        if self.clients.values().any(|client| {
            client.is_full_app_client()
                && client.writer.is_some()
                && client.animation_rect.width > 0
                && client.animation_rect.height > 0
                && !Self::animation_patch_preserves_full_frame(client, &self.app.state)
        }) {
            crate::render_prof::event("animation_render.fallback_overlay");
            return false;
        }

        if self.clients.values().any(|client| {
            client.is_full_app_client()
                && client.writer.is_some()
                && client.animation_rect.width > 0
                && client.animation_rect.height > 0
                && (client.render_pending
                    || client.render_state.last_frame().is_none_or(|frame| {
                        frame.width != client.terminal_size.0
                            || frame.height != client.terminal_size.1
                            || client.animation_rect.right() > frame.width
                            || client.animation_rect.bottom() > frame.height
                    }))
        }) {
            crate::render_prof::event("animation_render.fallback_missing_frame");
            return false;
        }

        let mut broken_clients = Vec::new();
        let mut needs_full_render = false;
        let app_state = &self.app.state;
        for (&client_id, client) in self
            .clients
            .iter_mut()
            .filter(|(_, client)| client.is_full_app_client() && client.writer.is_some())
        {
            let rect = client.animation_rect;
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            let hovered = client.dock_presentation.hovered_control
                == Some(crate::app::state::ControlId::SidebarAnimationPause);
            let Some(writer) = client.writer.as_ref() else {
                continue;
            };
            let area = Rect::new(0, 0, rect.width, rect.height);
            let patch_buffer =
                crate::server::render_stream::render_hyperspace_animation_buffer_reusing(
                    app_state,
                    area,
                    hovered,
                    &mut client.animation_terminal,
                );
            let patch = FrameData::from_ratatui_buffer_with_hyperlinks(&patch_buffer, None, &[]);
            let semantic_frame = client.render_state.take_semantic_frame();
            let semantic_baseline = semantic_frame.is_some();
            let Some(mut frame) =
                semantic_frame.or_else(|| client.render_state.last_frame().cloned())
            else {
                crate::render_prof::event("animation_render.fallback_missing_frame");
                return false;
            };
            frame.graphics.clear();
            let mut frame_changed = false;
            for row in 0..rect.height {
                for column in 0..rect.width {
                    let source = usize::from(row) * usize::from(rect.width) + usize::from(column);
                    let destination = usize::from(rect.y + row) * usize::from(frame.width)
                        + usize::from(rect.x + column);
                    let Some(cell) = patch.cells.get(source) else {
                        continue;
                    };
                    if let Some(target) = frame.cells.get_mut(destination) {
                        if target != cell {
                            target.clone_from(cell);
                            frame_changed = true;
                        }
                    }
                }
            }
            if !frame_changed {
                if semantic_baseline {
                    client.render_state.restore_semantic_frame(frame);
                }
                crate::render_prof::event("animation_render.skip_unchanged");
                continue;
            }

            let Some(prepared) = client.render_state.prepare_frame(frame) else {
                continue;
            };
            let serialized = match Self::frame_server_message(prepared.message()) {
                Ok(serialized) => serialized,
                Err(error) => {
                    warn!(client_id, %error, "failed to serialize sidebar animation frame");
                    client.defer_full_render();
                    crate::render_prof::event("animation_render.serialize_error");
                    needs_full_render = true;
                    continue;
                }
            };
            match writer.render.try_send(serialized) {
                Ok(render_sequence) => {
                    client.render_state.commit_sent_frame(prepared);
                    client.clear_deferred_render();
                    crate::render_prof::event("animation_render.sent");
                    let _ = render_sequence;
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                    crate::render_prof::event("animation_render.deferred");
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    broken_clients.push(client_id);
                }
            }
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
        crate::render_prof::event("animation_render.complete");
        !needs_full_render
    }
}
