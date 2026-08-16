use super::{HeadlessServer, RenderImpact};
use crate::api;
use crate::protocol::{ServerMessage, MAX_GRAPHICS_FRAME_SIZE};
use crate::server::clients::{render_targets, ClientConnectionMode, DeferredRender};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetainedGraphicsOutcome {
    Sent,
    Deferred,
    Fallback,
}

pub(super) fn frame_pane_graphics(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.is_empty() {
        return bytes;
    }
    [b"\x1b7".as_slice(), &bytes, b"\x1b8"].concat()
}

impl HeadlessServer {
    pub(super) fn pane_graphics_runtime_active(&self) -> bool {
        !self.app.pane_graphics.slots.is_empty()
    }

    pub(super) fn handle_pane_graphics_stream_frame(
        &mut self,
        msg: api::ApiRequestMessage,
    ) -> RenderImpact {
        if self.shutting_down {
            let _ = self.handle_api_request_with_shutdown_check_inner(msg, false);
            return RenderImpact::None;
        }

        let internal_changed = self.drain_all_internal_events_with_forwarding();
        let direct = match &msg.request.method {
            api::schema::Method::PaneGraphicsStreamDirect(params) => Some(params.clone()),
            _ => None,
        };
        let direct_key = direct.as_ref().and_then(|params| {
            self.app.parse_pane_id(&params.pane_id).map(|(_, pane_id)| {
                (
                    pane_id,
                    params
                        .layer_id
                        .clone()
                        .unwrap_or_else(|| api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.into()),
                )
            })
        });
        let direct_client = self
            .direct_graphics_available()
            .then_some(self.foreground_client_id)
            .flatten();
        let gate_busy = self
            .app
            .pane_graphics
            .slots
            .values()
            .any(|slot| slot.direct_gate.is_some());
        self.app.direct_graphics_available = direct_client.is_some() && !gate_busy;
        let response = self
            .app
            .handle_api_request_after_internal_events_drained(msg.request);
        self.app.direct_graphics_available = self.direct_graphics_available();
        let succeeded = serde_json::from_str::<api::schema::SuccessResponse>(&response).is_ok();

        if succeeded {
            if let (Some(params), Some(client_id)) = (direct, direct_client) {
                let direct_frame = direct_key.as_ref().and_then(|key| {
                    self.app.pane_graphics.slots.get(key).and_then(|slot| {
                        let active_owner = slot.stream_owner.as_deref() == Some(&params.owner)
                            && slot.stream_is_active();
                        active_owner.then_some(())?;
                        slot.layer
                            .as_ref()
                            .and_then(crate::app::pane_graphics::Layer::direct_lease)
                            .map(|lease| {
                                (
                                    slot.host_image_id,
                                    lease.path().to_string_lossy().into_owned(),
                                    lease.len() as u64,
                                    lease.fingerprint(),
                                )
                            })
                    })
                });
                if let (Some(key), Some((image_id, path, expected_len, transfer_id))) =
                    (direct_key.clone(), direct_frame)
                {
                    let command = self.clients.get(&client_id).and_then(|client| {
                        crate::kitty_graphics::prepare_direct_file(
                            &self.app.state,
                            &self.app.pane_graphics,
                            self.app.state.view.tab_surface(),
                            client.cell_size,
                            !internal_changed,
                            &client.graphics_cache,
                            &key,
                        )
                    });
                    let Some(command) = command else {
                        if self.install_inline_fallback(&key) {
                            if msg.respond_to.send(response).is_err() {
                                self.retire_direct_gate(&key);
                            }
                            return if internal_changed {
                                RenderImpact::Full
                            } else {
                                RenderImpact::Graphics
                            };
                        }
                        self.retire_direct_gate(&key);
                        return if internal_changed {
                            RenderImpact::Full
                        } else {
                            RenderImpact::Graphics
                        };
                    };
                    let message = ServerMessage::GraphicsFile {
                        path,
                        expected_len,
                        image_id,
                        transfer_id,
                        leading: command.leading,
                        control: command.control,
                    };
                    let send =
                        Self::frame_server_message_with_max(&message, MAX_GRAPHICS_FRAME_SIZE)
                            .map_err(|_| std::sync::mpsc::TrySendError::Full(Vec::new()))
                            .and_then(|framed| {
                                let Some(writer) = self
                                    .clients
                                    .get(&client_id)
                                    .and_then(|client| client.writer.as_ref())
                                else {
                                    return Err(std::sync::mpsc::TrySendError::Disconnected(
                                        framed,
                                    ));
                                };
                                writer.render.send_ordered(framed)
                            });
                    match send {
                        Ok(()) => {
                            if let Some(slot) = self.app.pane_graphics.slots.get_mut(&key) {
                                slot.direct_gate = Some(crate::app::pane_graphics::DirectGate {
                                    transfer_id,
                                    client_id,
                                    deadline: std::time::Instant::now()
                                        + crate::app::pane_graphics::DIRECT_DELIVERY_TIMEOUT,
                                    written: false,
                                    success_response: response,
                                    respond_to: msg.respond_to,
                                });
                            }
                            return if internal_changed {
                                RenderImpact::Full
                            } else {
                                RenderImpact::None
                            };
                        }
                        Err(error) => {
                            self.handle_unwritten_direct_failure(
                                &key,
                                response,
                                msg.respond_to,
                                error,
                            );
                            return RenderImpact::Graphics;
                        }
                    }
                }
            }
        }

        let response_failed = msg.respond_to.send(response).is_err();
        if succeeded && response_failed {
            if let Some(key) = direct_key {
                self.retire_direct_gate(&key);
            }
        }
        if internal_changed {
            RenderImpact::Full
        } else if succeeded {
            RenderImpact::Graphics
        } else {
            RenderImpact::None
        }
    }

    fn install_inline_fallback(&mut self, key: &crate::app::pane_graphics::Key) -> bool {
        let len = self
            .app
            .pane_graphics
            .slots
            .get(key)
            .and_then(|slot| slot.layer.as_ref())
            .and_then(crate::app::pane_graphics::Layer::direct_lease)
            .map(crate::pane_graphics_files::Lease::len);
        let Some(len) = len else {
            return false;
        };
        if len > crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES
            || !self.app.pane_graphics.can_store_inline(key, len)
        {
            return false;
        }
        let data = self
            .app
            .pane_graphics
            .slots
            .get(key)
            .and_then(|slot| slot.layer.as_ref())
            .and_then(crate::app::pane_graphics::Layer::direct_lease)
            .and_then(|lease| lease.copy_rgba().ok());
        let Some(data) = data else {
            return false;
        };
        let slot = self
            .app
            .pane_graphics
            .slots
            .get_mut(key)
            .expect("matched slot");
        if !slot.stream_is_active() {
            return false;
        }
        let layer = slot.layer.take().expect("direct layer");
        slot.layer = Some(crate::app::pane_graphics::Layer::inline(
            layer.format,
            layer.image_width,
            layer.image_height,
            data,
            layer.render,
            layer.z_index,
        ));
        self.app.pane_graphics.mark_changed();
        true
    }

    pub(super) fn handle_unwritten_direct_failure(
        &mut self,
        key: &crate::app::pane_graphics::Key,
        response: String,
        respond_to: std::sync::mpsc::Sender<String>,
        error: std::sync::mpsc::TrySendError<Vec<u8>>,
    ) -> bool {
        if matches!(error, std::sync::mpsc::TrySendError::Full(_))
            && self.install_inline_fallback(key)
            && respond_to.send(response).is_ok()
        {
            return true;
        }
        self.retire_direct_gate(key);
        false
    }

    pub(super) fn retire_direct_gate(&mut self, key: &crate::app::pane_graphics::Key) {
        if self.app.pane_graphics.slots.remove(key).is_some() {
            self.app.pane_graphics.mark_changed();
        }
    }

    pub(super) fn start_direct_graphics_response(
        &mut self,
        client_id: u64,
        transfer_id: u64,
        image_id: u32,
    ) -> bool {
        if let Some(gate) = self.app.pane_graphics.slots.values_mut().find_map(|slot| {
            (slot.host_image_id == image_id && slot.stream_is_active())
                .then_some(slot.direct_gate.as_mut())
                .flatten()
                .filter(|gate| gate.client_id == client_id && gate.transfer_id == transfer_id)
        }) {
            gate.written = true;
            gate.deadline =
                std::time::Instant::now() + crate::app::pane_graphics::DIRECT_RESPONSE_TIMEOUT;
        }
        false
    }

    pub(super) fn complete_direct_graphics(
        &mut self,
        client_id: u64,
        transfer_id: u64,
        image_id: u32,
        success: bool,
    ) -> bool {
        if self.shutting_down {
            self.retire_all_direct_graphics();
            return false;
        }
        let key = self.app.pane_graphics.slots.iter().find_map(|(key, slot)| {
            slot.direct_gate
                .as_ref()
                .is_some_and(|gate| {
                    slot.stream_is_active()
                        && gate.client_id == client_id
                        && gate.transfer_id == transfer_id
                        && slot.host_image_id == image_id
                        && (!success || gate.written)
                })
                .then(|| key.clone())
        });
        let Some(key) = key else {
            return false;
        };
        let pane_is_live = self
            .app
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.pane_state(key.0).is_some());
        if !pane_is_live {
            self.retire_direct_gate(&key);
            return false;
        }
        if success {
            let (gate, host_image_id) = {
                let slot = self
                    .app
                    .pane_graphics
                    .slots
                    .get_mut(&key)
                    .expect("matched slot");
                if !slot.stream_is_active() {
                    return false;
                }
                if let Some(layer) = slot.layer.as_mut() {
                    layer.mark_resident(client_id);
                }
                (
                    slot.direct_gate.take().expect("matched gate"),
                    slot.host_image_id,
                )
            };
            if let (Some(client), Some(layer)) = (
                self.clients.get_mut(&client_id),
                self.app
                    .pane_graphics
                    .slots
                    .get(&key)
                    .and_then(|slot| slot.layer.as_ref()),
            ) {
                client
                    .graphics_cache
                    .trust_pane_layer(&key, host_image_id, layer);
            }
            if gate.respond_to.send(gate.success_response).is_err() {
                self.retire_direct_gate(&key);
                return true;
            }
            return true;
        }

        if let Some(client) = self.clients.get_mut(&client_id) {
            client.direct_graphics = false;
        }
        self.app.direct_graphics_available = false;
        if !self.install_inline_fallback(&key) {
            self.retire_direct_gate(&key);
            self.retire_all_direct_graphics();
            return true;
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.graphics_cache.forget_pane_layer(&key, image_id);
        }
        let gate = self
            .app
            .pane_graphics
            .slots
            .get_mut(&key)
            .filter(|slot| slot.stream_is_active())
            .and_then(|slot| slot.direct_gate.take());
        let Some(gate) = gate else {
            self.retire_direct_gate(&key);
            return true;
        };
        if gate.respond_to.send(gate.success_response).is_err() {
            self.retire_direct_gate(&key);
            return true;
        }
        self.retire_all_direct_graphics();
        true
    }

    pub(super) fn expire_direct_graphics(&mut self, now: std::time::Instant) -> bool {
        let expired = self
            .app
            .pane_graphics
            .slots
            .iter()
            .filter_map(|(key, slot)| {
                slot.direct_gate
                    .as_ref()
                    .filter(|gate| gate.deadline <= now)
                    .map(|gate| (key.clone(), gate.client_id))
            })
            .collect::<Vec<_>>();
        for (key, client_id) in &expired {
            if let Some(slot) = self.app.pane_graphics.slots.get(key) {
                if let Some(gate) = &slot.direct_gate {
                    self.send_to_client(
                        *client_id,
                        ServerMessage::GraphicsTransmissionRetired {
                            transfer_id: gate.transfer_id,
                            image_id: slot.host_image_id,
                        },
                    );
                }
            }
            if let Some(client) = self.clients.get_mut(client_id) {
                client.direct_graphics = false;
            }
            self.retire_direct_gate(key);
        }
        if !expired.is_empty() {
            self.app.direct_graphics_available = false;
            self.retire_all_direct_graphics();
        }
        !expired.is_empty()
    }

    pub(super) fn retire_all_direct_graphics(&mut self) {
        let keys = self
            .app
            .pane_graphics
            .slots
            .iter()
            .filter(|(_, slot)| {
                slot.layer
                    .as_ref()
                    .is_some_and(crate::app::pane_graphics::Layer::terminal_only)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.retire_direct_gate(&key);
        }
    }

    pub(super) fn retire_direct_graphics_for_client(&mut self, client_id: u64) {
        let keys = self
            .app
            .pane_graphics
            .slots
            .iter()
            .filter_map(|(key, slot)| {
                let layer = slot.layer.as_ref()?;
                let owned = layer.resident_client() == Some(client_id)
                    || (layer.direct_lease().is_some()
                        && slot
                            .direct_gate
                            .as_ref()
                            .is_some_and(|gate| gate.client_id == client_id));
                (layer.terminal_only() && owned).then(|| key.clone())
            })
            .collect::<Vec<_>>();
        for key in keys {
            self.retire_direct_gate(&key);
        }
    }

    pub(super) fn render_retained_graphics_update_and_stream(&mut self) -> RetainedGraphicsOutcome {
        crate::render_prof::event("retained_graphics.attempt");
        if self.app.full_redraw_pending {
            crate::render_prof::event("retained_graphics_fallback.full_redraw_pending");
            return RetainedGraphicsOutcome::Fallback;
        }

        let render_targets = render_targets(&self.clients, self.foreground_client_id);
        let mut app_view_size = None;
        for (_, terminal_size, _, _, mode) in &render_targets {
            if !matches!(mode, ClientConnectionMode::App) {
                continue;
            }
            if app_view_size.is_some_and(|size| size != *terminal_size) {
                crate::render_prof::event("retained_graphics_fallback.mixed_app_geometry");
                return RetainedGraphicsOutcome::Fallback;
            }
            app_view_size = Some(*terminal_size);
        }
        let mut deferred = false;
        let mut prepared = Vec::new();

        for (client_id, (cols, rows), cell_size, _is_foreground, mode) in render_targets {
            if !matches!(mode, ClientConnectionMode::App) {
                continue;
            }
            let Some(client) = self.clients.get_mut(&client_id) else {
                crate::render_prof::event("retained_graphics_fallback.client_missing");
                return RetainedGraphicsOutcome::Fallback;
            };
            if client.deferred_render() != DeferredRender::None {
                deferred = true;
                continue;
            }
            if client.graphics_surface_reset_pending || !cell_size.is_known() {
                crate::render_prof::event("retained_graphics_fallback.client_state");
                return RetainedGraphicsOutcome::Fallback;
            }
            let Some(last_frame) = client.render_state.last_frame() else {
                crate::render_prof::event("retained_graphics_fallback.no_last_frame");
                return RetainedGraphicsOutcome::Fallback;
            };
            if last_frame.width != cols || last_frame.height != rows {
                crate::render_prof::event("retained_graphics_fallback.frame_size_mismatch");
                return RetainedGraphicsOutcome::Fallback;
            }
            if client.writer.is_none() {
                crate::render_prof::event("retained_graphics_fallback.writer_missing");
                return RetainedGraphicsOutcome::Fallback;
            }

            let mut next_graphics_cache = client.graphics_cache.clone();
            let encode_started = crate::render_prof::timer();
            let encoded = crate::kitty_graphics::encode_local_pane_graphics(
                &self.app.state,
                &self.app.pane_graphics,
                &self.app.terminal_runtimes,
                self.app.state.view.tab_surface(),
                cell_size,
                Some(crate::kitty_graphics::HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
                &mut next_graphics_cache,
            );
            crate::render_prof::duration_since("retained_graphics.graphics_encode", encode_started);
            prepared.push((client_id, encoded, next_graphics_cache));
        }

        let mut broken_clients = Vec::new();
        for (client_id, encoded, next_graphics_cache) in prepared {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let serialized = if encoded.bytes.is_empty() {
                None
            } else {
                match Self::frame_server_message_with_max(
                    &ServerMessage::Graphics {
                        bytes: frame_pane_graphics(encoded.bytes),
                    },
                    MAX_GRAPHICS_FRAME_SIZE,
                ) {
                    Ok(serialized) => Some(serialized),
                    Err(_) => {
                        crate::render_prof::event("retained_graphics_fallback.oversized");
                        return RetainedGraphicsOutcome::Fallback;
                    }
                }
            };
            let result = match (serialized, client.writer.as_ref()) {
                (None, _) => Ok(()),
                (Some(bytes), Some(writer)) => writer.render.try_send(bytes),
                (Some(bytes), None) => Err(std::sync::mpsc::TrySendError::Disconnected(bytes)),
            };
            match result {
                Ok(()) => {
                    client.graphics_cache = next_graphics_cache;
                    if encoded.incomplete {
                        client.defer_full_render();
                        deferred = true;
                    } else {
                        client.clear_deferred_render();
                    }
                    crate::render_prof::event("retained_graphics.sent");
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                    deferred = true;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    broken_clients.push(client_id)
                }
            }
        }
        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }

        if deferred {
            RetainedGraphicsOutcome::Deferred
        } else {
            RetainedGraphicsOutcome::Sent
        }
    }
}
