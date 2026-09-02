use super::{HeadlessServer, RenderImpact};
use crate::api;
use crate::protocol::{ServerMessage, MAX_GRAPHICS_FRAME_SIZE};
use crate::server::clients::ClientConnectionMode;

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
                                    lease.path().to_string_lossy().into_owned(),
                                    lease.len() as u64,
                                    lease.fingerprint(),
                                )
                            })
                    })
                });
                if let (Some(key), Some((path, expected_len, transfer_id))) =
                    (direct_key.clone(), direct_frame)
                {
                    let prepared = self.clients.get(&client_id).and_then(|client| {
                        matches!(client.mode, ClientConnectionMode::ClientShell).then_some(())?;
                        let layer = self
                            .app
                            .pane_graphics
                            .slots
                            .get(&key)
                            .and_then(|slot| slot.layer.as_ref())?;
                        let asset = crate::kitty_graphics::surface::pane_layer_asset_key(
                            &self.app, &key, layer,
                        )?;
                        let (image_id, control) =
                            crate::kitty_graphics::surface::direct_upload_control(
                                &self.client_shell_boot_id,
                                &asset,
                            );
                        Some((
                            crate::kitty_graphics::DirectFileCommand {
                                leading: Vec::new(),
                                control,
                            },
                            Some(asset),
                            image_id,
                        ))
                    });
                    let Some((command, surface_asset, image_id)) = prepared else {
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
                        surface_asset,
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
                                    image_id,
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
            slot.stream_is_active()
                .then_some(slot.direct_gate.as_mut())
                .flatten()
                .filter(|gate| {
                    gate.client_id == client_id
                        && gate.transfer_id == transfer_id
                        && gate.image_id == image_id
                })
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
                        && gate.image_id == image_id
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
            self.retire_direct_gate_with_client_notice(&key);
            return false;
        }
        if success {
            let gate = {
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
                slot.direct_gate.take().expect("matched gate")
            };
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
                            image_id: gate.image_id,
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

    fn retire_direct_gate_with_client_notice(&mut self, key: &crate::app::pane_graphics::Key) {
        if let Some((client_id, transfer_id, image_id)) = self
            .app
            .pane_graphics
            .slots
            .get(key)
            .and_then(|slot| slot.direct_gate.as_ref())
            .map(|gate| (gate.client_id, gate.transfer_id, gate.image_id))
        {
            self.send_to_client(
                client_id,
                ServerMessage::GraphicsTransmissionRetired {
                    transfer_id,
                    image_id,
                },
            );
        }
        self.retire_direct_gate(key);
    }

    pub(super) fn retain_live_pane_graphics(&mut self) -> bool {
        let dead_direct = self
            .app
            .pane_graphics
            .slots
            .iter()
            .filter(|((pane_id, _), slot)| {
                slot.direct_gate.is_some()
                    && !self
                        .app
                        .state
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.pane_state(*pane_id).is_some())
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let changed = !dead_direct.is_empty();
        for key in dead_direct {
            self.retire_direct_gate_with_client_notice(&key);
        }
        let retained = self.app.pane_graphics.retain_live_panes(&self.app.state);
        changed || retained
    }

    pub(super) fn retire_all_direct_graphics(&mut self) {
        let retired = self
            .app
            .pane_graphics
            .slots
            .iter()
            .filter(|(_, slot)| {
                slot.layer
                    .as_ref()
                    .is_some_and(crate::app::pane_graphics::Layer::terminal_only)
            })
            .map(|(key, slot)| {
                let notification = slot
                    .direct_gate
                    .as_ref()
                    .map(|gate| (gate.client_id, gate.transfer_id, gate.image_id));
                (key.clone(), notification)
            })
            .collect::<Vec<_>>();
        for (_, notification) in &retired {
            if let Some((client_id, transfer_id, image_id)) = notification {
                self.send_to_client(
                    *client_id,
                    ServerMessage::GraphicsTransmissionRetired {
                        transfer_id: *transfer_id,
                        image_id: *image_id,
                    },
                );
            }
        }
        for (key, _) in retired {
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
}
