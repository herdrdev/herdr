use base64::Engine;

use crate::api::schema::{
    PaneGraphicsClearParams, PaneGraphicsSetParams, PaneGraphicsStreamParams, ResponseResult,
    PANE_GRAPHICS_SET_MAX_BYTES, PANE_GRAPHICS_STREAM_MAX_BYTES,
};
use crate::app::state::PaneGraphicsLayer;
use crate::app::App;
use crate::layout::PaneId;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_pane_graphics_info(
        &mut self,
        id: String,
        target: crate::api::schema::PaneTarget,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        if self.parse_pane_id(&target.pane_id).is_none() {
            return pane_not_found(id, &target.pane_id);
        }
        if !self.state.host_cell_size.is_known() {
            return encode_error(id, "cell_size_unavailable", "host cell size is unavailable");
        }
        encode_success(
            id,
            ResponseResult::PaneGraphicsInfo {
                cell_width_px: self.state.host_cell_size.width_px,
                cell_height_px: self.state.host_cell_size.height_px,
            },
        )
    }

    pub(super) fn handle_pane_graphics_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self.state.pane_graphics_streams.contains_key(&pane_id) {
            return encode_error(
                id,
                "stream_conflict",
                "pane already has an active graphics stream",
            );
        }
        self.set_pane_graphics_layer(id, pane_id, params)
    }

    fn set_pane_graphics_layer(
        &mut self,
        id: String,
        pane_id: PaneId,
        params: PaneGraphicsSetParams,
    ) -> String {
        if params.image_width == 0 || params.image_height == 0 {
            return encode_error(
                id,
                "invalid_image",
                "image_width and image_height must be greater than zero",
            );
        }
        let max_bytes = if params.data.is_some() {
            PANE_GRAPHICS_STREAM_MAX_BYTES
        } else {
            PANE_GRAPHICS_SET_MAX_BYTES
        };
        let data = match params.data {
            Some(data) => data,
            None => match base64::engine::general_purpose::STANDARD.decode(params.data_base64) {
                Ok(data) => data,
                Err(_) => {
                    return encode_error(id, "invalid_image", "data_base64 is not valid base64");
                }
            },
        };
        if data.is_empty() {
            return encode_error(id, "invalid_image", "image data must not be empty");
        }
        if data.len() > max_bytes {
            return encode_error(id, "image_too_large", "image data is too large");
        }
        match pane_graphics_expected_data_len(
            params.format,
            params.image_width,
            params.image_height,
        ) {
            Ok(Some(expected_len)) if data.len() != expected_len => {
                return encode_error(
                    id,
                    "invalid_image",
                    "image data length does not match format and dimensions",
                );
            }
            Ok(_) => {}
            Err(()) => {
                return encode_error(id, "invalid_image", "image dimensions are too large");
            }
        }

        let layer = PaneGraphicsLayer::new(
            params.format,
            params.image_width,
            params.image_height,
            data,
            params.placement,
        );
        self.state.pane_graphics_layers.insert(pane_id, layer);
        self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_clear(
        &mut self,
        id: String,
        params: PaneGraphicsClearParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self.state.pane_graphics_streams.contains_key(&pane_id) {
            return encode_error(
                id,
                "stream_conflict",
                "pane already has an active graphics stream",
            );
        }
        if self.state.pane_graphics_layers.remove(&pane_id).is_some() {
            self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        match self.state.pane_graphics_streams.get(&pane_id) {
            Some(owner) if owner == &params.owner => {}
            Some(_) => {
                return encode_error(
                    id,
                    "stream_conflict",
                    "pane graphics stream owner does not match active stream",
                );
            }
            None => return encode_error(id, "stream_closed", "pane graphics stream is not active"),
        }
        self.set_pane_graphics_layer(id, pane_id, params)
    }

    pub(super) fn handle_pane_graphics_stream_open(
        &mut self,
        id: String,
        params: PaneGraphicsStreamParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        if params.owner.is_empty() {
            return encode_error(
                id,
                "invalid_stream",
                "pane graphics stream owner is required",
            );
        }
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self.state.pane_graphics_streams.contains_key(&pane_id) {
            return encode_error(
                id,
                "stream_conflict",
                "pane already has an active graphics stream",
            );
        }
        self.state
            .pane_graphics_streams
            .insert(pane_id, params.owner);
        self.state.pane_graphics_layers.remove(&pane_id);
        self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_close(
        &mut self,
        id: String,
        params: PaneGraphicsStreamParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self
            .state
            .pane_graphics_streams
            .get(&pane_id)
            .is_some_and(|owner| owner == &params.owner)
        {
            self.state.pane_graphics_streams.remove(&pane_id);
            self.state.pane_graphics_layers.remove(&pane_id);
            self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn require_pane_graphics_enabled(app: &App, id: &str) -> Result<(), String> {
    if app.state.kitty_graphics_enabled {
        return Ok(());
    }
    Err(encode_error(
        id.to_owned(),
        "feature_disabled",
        "pane graphics require experimental.kitty_graphics",
    ))
}

fn pane_graphics_expected_data_len(
    format: crate::api::schema::PaneGraphicsFormat,
    image_width: u32,
    image_height: u32,
) -> Result<Option<usize>, ()> {
    let bytes_per_pixel = match format {
        crate::api::schema::PaneGraphicsFormat::Png => return Ok(None),
        crate::api::schema::PaneGraphicsFormat::Rgb => 3_u64,
        crate::api::schema::PaneGraphicsFormat::Rgba => 4_u64,
    };
    let pixels = u64::from(image_width)
        .checked_mul(u64::from(image_height))
        .ok_or(())?;
    pixels
        .checked_mul(bytes_per_pixel)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .map(Some)
        .ok_or(())
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ErrorResponse, SuccessResponse};
    use crate::config::Config;
    use crate::workspace::Workspace;

    fn app_with_test_workspace() -> (App, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("pane-graphics")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        (app, public_pane_id)
    }

    #[test]
    fn api_pane_graphics_info_is_explicit_and_side_effect_free() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let request = |pane_id: String| crate::api::schema::Request {
            id: "graphics-info".into(),
            method: crate::api::schema::Method::PaneGraphicsInfo(crate::api::schema::PaneTarget {
                pane_id,
            }),
        };

        let response = app.handle_api_request(request(public_pane_id.clone()));
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "feature_disabled");

        app.state.kitty_graphics_enabled = true;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 11,
            height_px: 22,
        };
        let response = app.handle_api_request(request(public_pane_id));
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::PaneGraphicsInfo {
                cell_width_px: 11,
                cell_height_px: 22,
            }
        ));
        assert!(app.state.pane_graphics_layers.is_empty());
        assert!(app.state.pane_graphics_streams.is_empty());
    }

    #[test]
    fn api_pane_graphics_set_and_clear_updates_runtime_layers() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        app.state.kitty_graphics_enabled = true;
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3, 4]);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-set".into(),
            method: crate::api::schema::Method::PaneGraphicsSet(PaneGraphicsSetParams {
                pane_id: public_pane_id.clone(),
                owner: String::new(),
                format: crate::api::schema::PaneGraphicsFormat::Rgba,
                image_width: 1,
                image_height: 1,
                data: None,
                data_base64: data,
                placement: crate::api::schema::PaneGraphicsPlacementParams {
                    grid_cols: 10,
                    grid_rows: 4,
                    ..Default::default()
                },
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));

        let layer = app
            .state
            .pane_graphics_layers
            .get(&pane_id)
            .expect("graphics layer stored");
        assert_eq!(layer.image_width, 1);
        assert_eq!(layer.image_height, 1);
        assert_eq!(layer.render.grid_cols, 10);
        assert_eq!(layer.render.grid_rows, 4);
        assert!(!app.state.session_dirty);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-clear".into(),
            method: crate::api::schema::Method::PaneGraphicsClear(PaneGraphicsClearParams {
                pane_id: public_pane_id,
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));
    }

    #[test]
    fn api_pane_graphics_rejects_when_kitty_graphics_disabled() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3, 4]);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-set".into(),
            method: crate::api::schema::Method::PaneGraphicsSet(PaneGraphicsSetParams {
                pane_id: public_pane_id.clone(),
                owner: String::new(),
                format: crate::api::schema::PaneGraphicsFormat::Rgba,
                image_width: 1,
                image_height: 1,
                data: None,
                data_base64: data,
                placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "feature_disabled");
        assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-clear".into(),
            method: crate::api::schema::Method::PaneGraphicsClear(PaneGraphicsClearParams {
                pane_id: public_pane_id,
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "feature_disabled");
        assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));
    }

    #[test]
    fn api_pane_graphics_stream_claim_uses_resolved_pane_identity() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        app.state.kitty_graphics_enabled = true;
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        let alias = format!("{public_pane_id}:alias");
        app.state
            .public_pane_id_aliases
            .insert(alias.clone(), pane_id);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-open".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamOpen(
                crate::api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id,
                    owner: "stream-1".into(),
                },
            ),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert_eq!(
            app.state.pane_graphics_streams.get(&pane_id),
            Some(&"stream-1".to_string())
        );

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-open-alias".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamOpen(
                crate::api::schema::PaneGraphicsStreamParams {
                    pane_id: alias.clone(),
                    owner: "stream-2".into(),
                },
            ),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "stream_conflict");
        assert_eq!(
            app.state.pane_graphics_streams.get(&pane_id),
            Some(&"stream-1".to_string())
        );

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-close-wrong-owner".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamClose(
                crate::api::schema::PaneGraphicsStreamParams {
                    pane_id: alias.clone(),
                    owner: "stream-2".into(),
                },
            ),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert_eq!(
            app.state.pane_graphics_streams.get(&pane_id),
            Some(&"stream-1".to_string())
        );

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-close-alias".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamClose(
                crate::api::schema::PaneGraphicsStreamParams {
                    pane_id: alias,
                    owner: "stream-1".into(),
                },
            ),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(!app.state.pane_graphics_streams.contains_key(&pane_id));
    }

    #[test]
    fn api_pane_graphics_stream_owns_layer_while_active() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        app.state.kitty_graphics_enabled = true;
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        let public_data = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3, 4]);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-open".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamOpen(
                crate::api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id.clone(),
                    owner: "stream-1".into(),
                },
            ),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-set-public".into(),
            method: crate::api::schema::Method::PaneGraphicsSet(PaneGraphicsSetParams {
                pane_id: public_pane_id.clone(),
                owner: String::new(),
                format: crate::api::schema::PaneGraphicsFormat::Rgba,
                image_width: 1,
                image_height: 1,
                data: None,
                data_base64: public_data.clone(),
                placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "stream_conflict");
        assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-set-stream".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id: public_pane_id.clone(),
                owner: "stream-1".into(),
                format: crate::api::schema::PaneGraphicsFormat::Rgba,
                image_width: 1,
                image_height: 1,
                data: None,
                data_base64: public_data,
                placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(app.state.pane_graphics_layers.contains_key(&pane_id));

        let wrong_owner_data = base64::engine::general_purpose::STANDARD.encode([5_u8, 6, 7, 8]);
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-set-stream-wrong-owner".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id: public_pane_id.clone(),
                owner: "stream-2".into(),
                format: crate::api::schema::PaneGraphicsFormat::Rgba,
                image_width: 1,
                image_height: 1,
                data: None,
                data_base64: wrong_owner_data,
                placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "stream_conflict");
        assert_eq!(
            app.state.pane_graphics_layers.get(&pane_id).unwrap().data,
            vec![1_u8, 2, 3, 4]
        );

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-clear-public".into(),
            method: crate::api::schema::Method::PaneGraphicsClear(PaneGraphicsClearParams {
                pane_id: public_pane_id.clone(),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "stream_conflict");
        assert!(app.state.pane_graphics_layers.contains_key(&pane_id));

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-close".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamClose(
                crate::api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id,
                    owner: "stream-1".into(),
                },
            ),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));
    }

    #[test]
    fn api_pane_graphics_rejects_overflowing_raw_dimensions() {
        for format in [
            crate::api::schema::PaneGraphicsFormat::Rgb,
            crate::api::schema::PaneGraphicsFormat::Rgba,
        ] {
            let (mut app, public_pane_id) = app_with_test_workspace();
            app.state.kitty_graphics_enabled = true;
            let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();

            let response = app.handle_api_request(crate::api::schema::Request {
                id: "graphics-overflow".into(),
                method: crate::api::schema::Method::PaneGraphicsSet(PaneGraphicsSetParams {
                    pane_id: public_pane_id,
                    owner: String::new(),
                    format,
                    image_width: u32::MAX,
                    image_height: u32::MAX,
                    data: None,
                    data_base64: base64::engine::general_purpose::STANDARD.encode([1_u8]),
                    placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
                }),
            });

            let error: ErrorResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(error.error.code, "invalid_image");
            assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));
        }
    }

    #[test]
    fn api_pane_graphics_validates_exact_raw_lengths() {
        for (format, exact_len) in [
            (crate::api::schema::PaneGraphicsFormat::Rgb, 6_usize),
            (crate::api::schema::PaneGraphicsFormat::Rgba, 8_usize),
        ] {
            for data_len in [exact_len, exact_len - 1] {
                let (mut app, public_pane_id) = app_with_test_workspace();
                app.state.kitty_graphics_enabled = true;
                let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
                let response = app.handle_api_request(crate::api::schema::Request {
                    id: "graphics-raw-length".into(),
                    method: crate::api::schema::Method::PaneGraphicsSet(PaneGraphicsSetParams {
                        pane_id: public_pane_id,
                        owner: String::new(),
                        format,
                        image_width: 2,
                        image_height: 1,
                        data: None,
                        data_base64: base64::engine::general_purpose::STANDARD
                            .encode(vec![1_u8; data_len]),
                        placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
                    }),
                });

                if data_len == exact_len {
                    let success: SuccessResponse = serde_json::from_str(&response).unwrap();
                    assert!(matches!(success.result, ResponseResult::Ok {}));
                    assert!(app.state.pane_graphics_layers.contains_key(&pane_id));
                } else {
                    let error: ErrorResponse = serde_json::from_str(&response).unwrap();
                    assert_eq!(error.error.code, "invalid_image");
                    assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));
                }
            }
        }
    }

    #[test]
    fn api_pane_graphics_uses_transport_specific_limits() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        app.state.kitty_graphics_enabled = true;
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-public-too-large".into(),
            method: crate::api::schema::Method::PaneGraphicsSet(PaneGraphicsSetParams {
                pane_id: public_pane_id,
                owner: String::new(),
                format: crate::api::schema::PaneGraphicsFormat::Png,
                image_width: 1,
                image_height: 1,
                data: None,
                data_base64: base64::engine::general_purpose::STANDARD.encode(vec![
                    1_u8;
                    PANE_GRAPHICS_SET_MAX_BYTES
                        + 1
                ]),
                placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "image_too_large");
        assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));
    }

    #[test]
    fn api_pane_graphics_stream_methods_require_kitty_graphics() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        let open = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-open".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamOpen(
                crate::api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id.clone(),
                    owner: "stream-1".into(),
                },
            ),
        });
        let error: ErrorResponse = serde_json::from_str(&open).unwrap();
        assert_eq!(error.error.code, "feature_disabled");

        let set = app.handle_api_request(crate::api::schema::Request {
            id: "graphics-stream-set".into(),
            method: crate::api::schema::Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id: public_pane_id,
                owner: "stream-1".into(),
                format: crate::api::schema::PaneGraphicsFormat::Png,
                image_width: 1,
                image_height: 1,
                data: Some(vec![1_u8]),
                data_base64: String::new(),
                placement: crate::api::schema::PaneGraphicsPlacementParams::default(),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&set).unwrap();
        assert_eq!(error.error.code, "feature_disabled");
        assert!(!app.state.pane_graphics_streams.contains_key(&pane_id));
        assert!(!app.state.pane_graphics_layers.contains_key(&pane_id));
    }
}
