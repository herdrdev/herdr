use base64::Engine;

use crate::api::schema::{
    PaneGraphicsClearParams, PaneGraphicsSetParams, PaneGraphicsStreamParams, ResponseResult,
    PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES, PANE_GRAPHICS_MAX_LAYERS_PER_PANE,
    PANE_GRAPHICS_PRIMARY_LAYER_ID, PANE_GRAPHICS_SET_MAX_BYTES, PANE_GRAPHICS_STREAM_MAX_BYTES,
};
use crate::app::pane_graphics::{Key as PaneGraphicsKey, Layer, Slot};
use crate::app::App;
use crate::layout::PaneId;

use super::responses::{encode_error, encode_success};

impl App {
    /// Whether graphics for a pane can be placed on the surface rendered by this client.
    ///
    /// This deliberately follows the pane layout rather than pane existence: inactive
    /// workspaces/tabs and panes hidden by zoom are not placeable. Short-lived UI modes do not
    /// suspend the producer because the pane becomes visible again without a layout event.
    fn pane_graphics_visible(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        if self.state.active != Some(ws_idx) {
            return false;
        }
        let Some(tab) = self.state.workspaces[ws_idx].active_tab() else {
            return false;
        };
        if tab.zoomed {
            tab.layout.focused() == pane_id
        } else {
            tab.layout.pane_ids().contains(&pane_id)
        }
    }

    pub(super) fn handle_pane_graphics_info(
        &mut self,
        id: String,
        target: crate::api::schema::PaneTarget,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let pane_visible = self.pane_graphics_visible(ws_idx, pane_id);
        if !self.state.host_cell_size.is_known() {
            return encode_error(id, "cell_size_unavailable", "host cell size is unavailable");
        }
        let file_frame_directory = self
            .direct_graphics_available
            .then(|| self.pane_graphics_files.source_directory().ok())
            .flatten();
        let direct = file_frame_directory.is_some();
        encode_success(
            id,
            ResponseResult::PaneGraphicsInfo {
                cell_width_px: self.state.host_cell_size.width_px,
                cell_height_px: self.state.host_cell_size.height_px,
                pane_visible,
                file_frame_directory: file_frame_directory
                    .map(|directory| directory.to_string_lossy().into_owned()),
                file_frame_formats: if direct {
                    vec!["rgba".into(), "bgra".into()]
                } else {
                    Vec::new()
                },
                file_frame_max_bytes: direct.then_some(PANE_GRAPHICS_STREAM_MAX_BYTES),
                file_frame_direct_max_bytes: direct.then_some(PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES),
                // Damage metadata never changes the complete canonical frame contract.
                file_frame_damage: true,
                max_layers_per_pane: PANE_GRAPHICS_MAX_LAYERS_PER_PANE,
                pixel_mouse: self.pixel_mouse_available,
                file_frame_transport: direct.then(|| "direct-kitty".into()),
            },
        )
    }

    pub(crate) fn attach_pane_graphics_stream_active(
        &mut self,
        params: &PaneGraphicsStreamParams,
        active: std::sync::Arc<std::sync::atomic::AtomicBool>,
        response: &str,
    ) {
        if serde_json::from_str::<crate::api::schema::SuccessResponse>(response).is_err() {
            return;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return;
        };
        let Ok(key) = graphics_key(pane_id, params.layer_id.as_deref()) else {
            return;
        };
        self.pane_graphics
            .attach_stream_active(&key, &params.owner, active);
    }

    pub(super) fn handle_pane_graphics_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        self.reclaim_inactive_stream(&key);
        if self
            .pane_graphics
            .slots
            .get(&key)
            .is_some_and(|slot| slot.stream_owner.is_some())
        {
            return encode_error(
                id,
                "stream_conflict",
                "pane graphics layer has an active stream",
            );
        }
        self.set_layer(id, key, params)
    }

    pub(super) fn handle_pane_graphics_clear(
        &mut self,
        id: String,
        params: PaneGraphicsClearParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        self.reclaim_inactive_stream(&key);
        if self
            .pane_graphics
            .slots
            .get(&key)
            .is_some_and(|slot| slot.stream_owner.is_some())
        {
            return encode_error(
                id,
                "stream_conflict",
                "pane graphics layer has an active stream",
            );
        }
        if self.pane_graphics.slots.remove(&key).is_some() {
            self.pane_graphics.mark_changed();
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        match self.pane_graphics.slots.get(&key) {
            Some(slot)
                if slot.stream_owner.as_ref() == Some(&params.owner) && slot.stream_is_active() =>
            {
                self.set_layer(id, key, params)
            }
            Some(slot) if slot.stream_owner.as_ref() == Some(&params.owner) => {
                encode_error(id, "stream_closed", "pane graphics stream is not active")
            }
            Some(_) => encode_error(
                id,
                "stream_conflict",
                "pane graphics stream owner does not match active stream",
            ),
            None => encode_error(id, "stream_closed", "pane graphics stream is not active"),
        }
    }

    pub(super) fn handle_pane_graphics_stream_open(
        &mut self,
        id: String,
        params: PaneGraphicsStreamParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        if params.owner.is_empty() {
            return encode_error(
                id,
                "invalid_stream",
                "pane graphics stream owner is required",
            );
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        let stale = self
            .pane_graphics
            .slots
            .get(&key)
            .is_some_and(|slot| slot.stream_owner.is_some() && !slot.stream_is_active());
        if stale {
            self.pane_graphics.slots.remove(&key);
        }
        if self
            .pane_graphics
            .slots
            .get(&key)
            .and_then(|slot| slot.stream_owner.as_ref())
            .is_some()
        {
            return encode_error(
                id,
                "stream_conflict",
                "pane graphics layer has an active stream",
            );
        }
        let exists = self.pane_graphics.slots.contains_key(&key);
        if !self.pane_graphics.can_add_slot(&key)
            || (!exists
                && self.pane_graphics.layer_count(pane_id) >= PANE_GRAPHICS_MAX_LAYERS_PER_PANE)
        {
            return encode_error(id, "layer_limit", "pane graphics layer limit reached");
        }
        let Some(host_image_id) = self.pane_graphics.reserve_image_id(&key) else {
            return encode_error(id, "layer_limit", "pane graphics image ids are exhausted");
        };
        self.pane_graphics.slots.insert(
            key,
            Slot {
                host_image_id,
                layer: None,
                stream_owner: Some(params.owner),
                stream_active: Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    true,
                ))),
                direct_gate: None,
            },
        );
        self.pane_graphics.mark_changed();
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_close(
        &mut self,
        id: String,
        params: PaneGraphicsStreamParams,
    ) -> String {
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if let Ok(key) = graphics_key(pane_id, params.layer_id.as_deref()) {
            if self
                .pane_graphics
                .slots
                .get(&key)
                .and_then(|slot| slot.stream_owner.as_ref())
                .is_some_and(|owner| owner == &params.owner)
            {
                self.pane_graphics.slots.remove(&key);
                self.pane_graphics.mark_changed();
            }
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_direct(
        &mut self,
        id: String,
        params: crate::api::schema::PaneGraphicsDirectParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        let owner_matches = self.pane_graphics.slots.get(&key).is_some_and(|slot| {
            slot.stream_owner.as_deref() == Some(params.owner.as_str()) && slot.stream_is_active()
        });
        if !owner_matches {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        }
        if params.image_width == 0 || params.image_height == 0 {
            return encode_error(
                id,
                "invalid_image",
                "image_width and image_height must be greater than zero",
            );
        }
        if !matches!(
            params.format,
            crate::api::schema::PaneGraphicsFormat::Rgba
                | crate::api::schema::PaneGraphicsFormat::Bgra
        ) {
            return encode_error(id, "invalid_image", "direct frames require rgba or bgra");
        }
        let primary = key.1 == PANE_GRAPHICS_PRIMARY_LAYER_ID;
        let direct = self.direct_graphics_available
            && primary
            && params.format == crate::api::schema::PaneGraphicsFormat::Rgba;
        let max_bytes = if direct {
            PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES
        } else {
            PANE_GRAPHICS_STREAM_MAX_BYTES
        };
        let expected_len =
            match expected_len(params.format, params.image_width, params.image_height) {
                Ok(Some(len)) if len <= max_bytes => len,
                _ => return encode_error(id, "invalid_image", "invalid direct RGBA dimensions"),
            };
        let lease = match self
            .pane_graphics_files
            .lease(std::path::Path::new(&params.path), expected_len)
        {
            Ok(lease) => lease,
            Err(err) => return encode_error(id, "invalid_frame_file", err.to_string()),
        };
        if !direct && !self.pane_graphics.can_store_inline(&key, expected_len) {
            return encode_error(
                id,
                "graphics_budget_exceeded",
                "pane graphics inline memory limit reached",
            );
        }
        let layer = if direct {
            Layer::direct(
                params.image_width,
                params.image_height,
                lease,
                params.placement,
                params.z_index,
            )
        } else {
            let mut data = match lease.copy_rgba() {
                Ok(data) => data,
                Err(err) => return encode_error(id, "invalid_frame_file", err.to_string()),
            };
            canonicalize_bgra(params.format, &mut data);
            Layer::inline(
                crate::api::schema::PaneGraphicsFormat::Rgba,
                params.image_width,
                params.image_height,
                data,
                params.placement,
                params.z_index,
            )
        };
        let Some(slot) = self.pane_graphics.slots.get_mut(&key) else {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        };
        if !slot.stream_is_active() {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        }
        slot.layer = Some(layer);
        slot.direct_gate = None;
        self.pane_graphics.mark_changed();
        encode_success(
            id,
            ResponseResult::PaneGraphicsFrameAck {
                sequence: params.sequence,
                revision: params.revision,
            },
        )
    }

    fn set_layer(
        &mut self,
        id: String,
        key: PaneGraphicsKey,
        params: PaneGraphicsSetParams,
    ) -> String {
        if !self.pane_graphics.can_add_slot(&key)
            || (!self.pane_graphics.slots.contains_key(&key)
                && self.pane_graphics.layer_count(key.0) >= PANE_GRAPHICS_MAX_LAYERS_PER_PANE)
        {
            return encode_error(id, "layer_limit", "pane graphics layer limit reached");
        }
        if params.image_width == 0 || params.image_height == 0 {
            return encode_error(
                id,
                "invalid_image",
                "image_width and image_height must be greater than zero",
            );
        }
        let expected = match expected_len(params.format, params.image_width, params.image_height) {
            Ok(expected) => expected,
            Err(()) => return encode_error(id, "invalid_image", "image dimensions are too large"),
        };
        let streamed = params.data.is_some();
        let mut data = match params.data {
            Some(data) => data,
            None => match base64::engine::general_purpose::STANDARD.decode(params.data_base64) {
                Ok(data) => data,
                Err(_) => {
                    return encode_error(id, "invalid_image", "data_base64 is not valid base64")
                }
            },
        };
        let limit = if streamed {
            PANE_GRAPHICS_STREAM_MAX_BYTES
        } else {
            PANE_GRAPHICS_SET_MAX_BYTES
        };
        if data.is_empty() {
            return encode_error(id, "invalid_image", "image data must not be empty");
        }
        if data.len() > limit {
            return encode_error(id, "image_too_large", "image data is too large");
        }
        if expected.is_some_and(|size| size != data.len()) {
            return encode_error(
                id,
                "invalid_image",
                "image data does not match the frame contract",
            );
        }
        let format = canonicalize_bgra(params.format, &mut data);
        if !self.pane_graphics.can_store_inline(&key, data.len()) {
            return encode_error(
                id,
                "graphics_budget_exceeded",
                "pane graphics inline memory limit reached",
            );
        }
        let Some(host_image_id) = self.pane_graphics.reserve_image_id(&key) else {
            return encode_error(id, "layer_limit", "pane graphics image ids are exhausted");
        };
        // Move the liveness handle out before replacing the slot so Slot::drop cannot
        // deactivate the stream that the replacement still owns.
        let (stream_owner, stream_active) = self
            .pane_graphics
            .slots
            .get_mut(&key)
            .map(|slot| (slot.stream_owner.take(), slot.stream_active.take()))
            .unwrap_or_default();
        self.pane_graphics.slots.insert(
            key,
            Slot {
                host_image_id,
                layer: Some(Layer::inline(
                    format,
                    params.image_width,
                    params.image_height,
                    data,
                    params.placement,
                    params.z_index,
                )),
                stream_owner,
                stream_active,
                direct_gate: None,
            },
        );
        self.pane_graphics.mark_changed();
        encode_success(id, ResponseResult::Ok {})
    }

    fn reclaim_inactive_stream(&mut self, key: &PaneGraphicsKey) {
        let stale = self
            .pane_graphics
            .slots
            .get(key)
            .is_some_and(|slot| slot.stream_owner.is_some() && !slot.stream_is_active());
        if stale {
            self.pane_graphics.slots.remove(key);
            self.pane_graphics.mark_changed();
        }
    }
}

fn canonicalize_bgra(
    format: crate::api::schema::PaneGraphicsFormat,
    data: &mut [u8],
) -> crate::api::schema::PaneGraphicsFormat {
    if format != crate::api::schema::PaneGraphicsFormat::Bgra {
        return format;
    }
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    crate::api::schema::PaneGraphicsFormat::Rgba
}

fn graphics_key(pane_id: PaneId, layer_id: Option<&str>) -> Result<PaneGraphicsKey, &'static str> {
    let layer_id = layer_id.unwrap_or(PANE_GRAPHICS_PRIMARY_LAYER_ID);
    if layer_id.is_empty() || layer_id.len() > 64 {
        return Err("layer_id must contain between 1 and 64 characters");
    }
    if !layer_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err("layer_id contains unsupported characters");
    }
    Ok((pane_id, layer_id.to_owned()))
}

fn expected_len(
    format: crate::api::schema::PaneGraphicsFormat,
    width: u32,
    height: u32,
) -> Result<Option<usize>, ()> {
    let bytes = match format {
        crate::api::schema::PaneGraphicsFormat::Png => return Ok(None),
        crate::api::schema::PaneGraphicsFormat::Rgb => 3_u64,
        crate::api::schema::PaneGraphicsFormat::Rgba
        | crate::api::schema::PaneGraphicsFormat::Bgra => 4_u64,
    };
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(bytes))
        .and_then(|size| usize::try_from(size).ok())
        .map(Some)
        .ok_or(())
}

fn require_enabled(app: &App, id: &str) -> Result<(), String> {
    app.state
        .kitty_graphics_enabled
        .then_some(())
        .ok_or_else(|| {
            encode_error(
                id.to_owned(),
                "feature_disabled",
                "pane graphics require experimental.kitty_graphics",
            )
        })
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

    fn app() -> (App, String) {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("graphics")];
        app.state.ensure_test_terminals();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let public = app.public_pane_id(0, pane).unwrap();
        app.state.kitty_graphics_enabled = true;
        (app, public)
    }

    fn frame(
        pane_id: String,
        layer_id: Option<&str>,
        owner: &str,
        value: u8,
    ) -> PaneGraphicsSetParams {
        PaneGraphicsSetParams {
            pane_id,
            layer_id: layer_id.map(str::to_owned),
            z_index: 2,
            owner: owner.to_owned(),
            format: crate::api::schema::PaneGraphicsFormat::Rgba,
            image_width: 1,
            image_height: 1,
            data: Some(vec![value; 4]),
            data_base64: String::new(),
            placement: Default::default(),
        }
    }

    fn pane_visible(response: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(response).unwrap()["result"]["pane_visible"]
            .as_bool()
            .unwrap()
    }

    #[test]
    fn info_reports_visibility_for_terminal_surface_workspace_tab_and_zoom() {
        let (mut app, pane_id) = app();
        app.state.mode = crate::app::Mode::Terminal;
        app.state.active = Some(0);
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        let visible = app.handle_pane_graphics_info(
            "visible".into(),
            crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            },
        );
        assert!(pane_visible(&visible));

        let hidden_workspace = Workspace::test_new("hidden");
        let hidden_workspace_pane = hidden_workspace.tabs[0].root_pane;
        app.state.workspaces.push(hidden_workspace);
        let hidden_workspace_id = app.public_pane_id(1, hidden_workspace_pane).unwrap();
        let hidden = app.handle_pane_graphics_info(
            "hidden-workspace".into(),
            crate::api::schema::PaneTarget {
                pane_id: hidden_workspace_id,
            },
        );
        assert!(!pane_visible(&hidden));

        let inactive_tab = app.state.workspaces[0].test_add_tab(Some("inactive"));
        let inactive_pane = app.state.workspaces[0].tabs[inactive_tab].root_pane;
        let inactive_id = app.public_pane_id(0, inactive_pane).unwrap();
        let hidden_tab = app.handle_pane_graphics_info(
            "hidden-tab".into(),
            crate::api::schema::PaneTarget {
                pane_id: inactive_id,
            },
        );
        assert!(!pane_visible(&hidden_tab));

        app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].zoomed = true;
        let zoomed_away = app.handle_pane_graphics_info(
            "zoomed-away".into(),
            crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            },
        );
        assert!(!pane_visible(&zoomed_away));

        app.state.workspaces[0].tabs[0].zoomed = false;
        app.state.mode = crate::app::Mode::Navigate;
        let short_lived_mode = app.handle_pane_graphics_info(
            "navigate".into(),
            crate::api::schema::PaneTarget { pane_id },
        );
        assert!(pane_visible(&short_lived_mode));
    }

    #[test]
    fn monolithic_info_keeps_fast_transport_and_exact_pixels_disabled() {
        let (mut app, pane_id) = app();
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        assert!(!app.pane_graphics_files.is_initialized());
        let response = app
            .handle_pane_graphics_info("info".into(), crate::api::schema::PaneTarget { pane_id });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::PaneGraphicsInfo {
                file_frame_damage: true,
                max_layers_per_pane: 16,
                pixel_mouse: false,
                ..
            }
        ));
        assert!(app.pane_graphics.slots.is_empty());
        assert!(!app.pane_graphics_files.is_initialized());
    }

    #[cfg(unix)]
    #[test]
    fn info_advertises_rgba_direct_and_bgra_fallback_file_formats_on_demand() {
        let (mut app, pane_id) = app();
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        app.direct_graphics_available = true;
        let response = app
            .handle_pane_graphics_info("info".into(), crate::api::schema::PaneTarget { pane_id });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(app.pane_graphics_files.is_initialized());
        assert_eq!(
            value["result"]["file_frame_formats"],
            serde_json::json!(["rgba", "bgra"])
        );
        assert_eq!(
            value["result"]["file_frame_max_bytes"],
            PANE_GRAPHICS_STREAM_MAX_BYTES
        );
        assert_eq!(
            value["result"]["file_frame_direct_max_bytes"],
            PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES
        );
        assert_eq!(value["result"]["file_frame_damage"], true);
        assert_eq!(value["result"]["file_frame_transport"], "direct-kitty");
    }

    #[test]
    fn layers_are_isolated_and_stream_owned() {
        let (mut app, pane_id) = app();
        for (layer, z, value) in [(None, 0, 1), (Some("chrome"), 10, 2)] {
            let mut request = frame(pane_id.clone(), layer, "", value);
            request.z_index = z;
            let response = app.handle_pane_graphics_set(format!("set-{value}"), request);
            assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        }
        let (_, pane) = app.parse_pane_id(&pane_id).unwrap();
        assert_eq!(
            app.pane_graphics
                .slots
                .get(&(pane, PANE_GRAPHICS_PRIMARY_LAYER_ID.into()))
                .and_then(|slot| slot.layer.as_ref())
                .unwrap()
                .z_index,
            0
        );
        assert_eq!(
            app.pane_graphics
                .slots
                .get(&(pane, "chrome".into()))
                .and_then(|slot| slot.layer.as_ref())
                .unwrap()
                .z_index,
            10
        );
        let response = app.handle_pane_graphics_stream_open(
            "open".into(),
            PaneGraphicsStreamParams {
                pane_id: pane_id.clone(),
                layer_id: Some("chrome".into()),
                z_index: 10,
                owner: "stream".into(),
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        let response =
            app.handle_pane_graphics_set("conflict".into(), frame(pane_id, Some("chrome"), "", 3));
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "stream_conflict");
    }

    fn error_code(response: &str) -> String {
        serde_json::from_str::<ErrorResponse>(response)
            .unwrap()
            .error
            .code
    }

    #[test]
    fn validates_layer_and_frame_contracts() {
        let cases = [
            (Some("bad space"), 1, 1, vec![1; 4], "invalid_layer_id"),
            (None, 0, 1, vec![1; 4], "invalid_image"),
            (None, 1, 1, Vec::new(), "invalid_image"),
            (None, 1, 1, vec![1; 3], "invalid_image"),
        ];
        for (layer, width, height, data, expected) in cases {
            let (mut app, pane_id) = app();
            let mut params = frame(pane_id, layer, "", 1);
            params.image_width = width;
            params.image_height = height;
            params.data = Some(data);
            let response = app.handle_pane_graphics_set("invalid".into(), params);
            assert_eq!(error_code(&response), expected);
            assert!(app.pane_graphics.slots.is_empty());
        }
    }

    #[test]
    fn disabled_requests_have_no_graphics_side_effects() {
        let (mut app, pane_id) = app();
        app.state.kitty_graphics_enabled = false;
        let revision = app.pane_graphics.revision();

        let response = app.handle_pane_graphics_set("disabled".into(), frame(pane_id, None, "", 1));

        assert_eq!(error_code(&response), "feature_disabled");
        assert!(app.pane_graphics.slots.is_empty());
        assert_eq!(app.pane_graphics.revision(), revision);
    }

    #[test]
    fn stream_owner_controls_only_its_named_layer() {
        let (mut app, pane_id) = app();
        let params = |owner: &str| PaneGraphicsStreamParams {
            pane_id: pane_id.clone(),
            layer_id: Some("chrome".into()),
            z_index: 7,
            owner: owner.into(),
        };
        assert!(serde_json::from_str::<SuccessResponse>(
            &app.handle_pane_graphics_stream_open("open".into(), params("owner"))
        )
        .is_ok());
        assert_eq!(
            error_code(&app.handle_pane_graphics_stream_set(
                "wrong".into(),
                frame(pane_id.clone(), Some("chrome"), "other", 2),
            )),
            "stream_conflict"
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_stream_set(
                "frame".into(),
                frame(pane_id.clone(), Some("chrome"), "owner", 3),
            ))
            .is_ok()
        );
        app.handle_pane_graphics_stream_close("stale".into(), params("other"));
        assert_eq!(app.pane_graphics.slots.len(), 1);
        app.handle_pane_graphics_stream_close("close".into(), params("owner"));
        assert!(app.pane_graphics.slots.is_empty());
    }

    #[test]
    fn static_set_and_clear_reclaim_inactive_stream_layers() {
        let (mut app, pane_id) = app();
        let open = |layer: &str| PaneGraphicsStreamParams {
            pane_id: pane_id.clone(),
            layer_id: Some(layer.into()),
            z_index: 0,
            owner: format!("owner-{layer}"),
        };
        for layer in ["set", "clear"] {
            assert!(serde_json::from_str::<SuccessResponse>(
                &app.handle_pane_graphics_stream_open(layer.into(), open(layer))
            )
            .is_ok());
            let (_, pane) = app.parse_pane_id(&pane_id).unwrap();
            app.pane_graphics.slots[&(pane, layer.into())]
                .stream_active
                .as_ref()
                .unwrap()
                .store(false, std::sync::atomic::Ordering::Release);
        }

        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_set(
                "set-static".into(),
                frame(pane_id.clone(), Some("set"), "", 4),
            ))
            .is_ok()
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_clear(
                "clear-static".into(),
                PaneGraphicsClearParams {
                    pane_id,
                    layer_id: Some("clear".into()),
                },
            ))
            .is_ok()
        );
        assert_eq!(app.pane_graphics.slots.len(), 1);
        assert!(app
            .pane_graphics
            .slots
            .values()
            .all(|slot| slot.stream_owner.is_none()));
    }

    #[test]
    fn layer_limit_counts_empty_and_populated_stream_slots() {
        let (mut app, pane_id) = app();
        for index in 0..PANE_GRAPHICS_MAX_LAYERS_PER_PANE {
            let layer_id = format!("layer-{index}");
            let response = app.handle_pane_graphics_stream_open(
                layer_id.clone(),
                PaneGraphicsStreamParams {
                    pane_id: pane_id.clone(),
                    layer_id: Some(layer_id),
                    z_index: index as i32,
                    owner: format!("owner-{index}"),
                },
            );
            assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        }
        let response = app.handle_pane_graphics_set(
            "set-overflow".into(),
            frame(pane_id.clone(), Some("set-overflow"), "", 1),
        );
        assert_eq!(error_code(&response), "layer_limit");
        let response = app.handle_pane_graphics_stream_open(
            "overflow".into(),
            PaneGraphicsStreamParams {
                pane_id,
                layer_id: Some("overflow".into()),
                z_index: 0,
                owner: "overflow".into(),
            },
        );
        assert_eq!(error_code(&response), "layer_limit");
        assert_eq!(app.pane_graphics.slots.len(), 16);
    }

    #[test]
    fn set_and_clear_preserve_runtime_only_layer_details() {
        let (mut app, pane_id) = app();
        let (_, pane) = app.parse_pane_id(&pane_id).unwrap();
        let mut params = frame(pane_id.clone(), None, "", 9);
        params.placement.grid_cols = 10;
        params.placement.grid_rows = 4;
        assert!(serde_json::from_str::<SuccessResponse>(
            &app.handle_pane_graphics_set("set".into(), params)
        )
        .is_ok());
        let layer = app
            .pane_graphics
            .slots
            .get(&(pane, PANE_GRAPHICS_PRIMARY_LAYER_ID.into()))
            .and_then(|slot| slot.layer.as_ref())
            .unwrap();
        assert_eq!(layer.inline_data(), Some([9; 4].as_slice()));
        assert_eq!((layer.render.grid_cols, layer.render.grid_rows), (10, 4));
        assert!(!app.state.session_dirty);

        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_clear(
                "clear".into(),
                PaneGraphicsClearParams {
                    pane_id,
                    layer_id: None,
                },
            ))
            .is_ok()
        );
        assert!(app.pane_graphics.slots.is_empty());
    }

    #[test]
    fn raw_formats_require_exact_non_overflowing_lengths() {
        for (format, exact) in [
            (crate::api::schema::PaneGraphicsFormat::Rgb, 6),
            (crate::api::schema::PaneGraphicsFormat::Rgba, 8),
        ] {
            for (length, expected) in [(exact, "ok"), (exact - 1, "invalid_image")] {
                let (mut app, pane_id) = app();
                let mut params = frame(pane_id, None, "", 1);
                params.format = format;
                params.image_width = 2;
                params.data = Some(vec![1; length]);
                let response = app.handle_pane_graphics_set("length".into(), params);
                if expected == "ok" {
                    assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
                } else {
                    assert_eq!(error_code(&response), expected);
                }
            }
            let (mut app, pane_id) = app();
            let mut params = frame(pane_id, None, "", 1);
            params.format = format;
            params.image_width = u32::MAX;
            params.image_height = u32::MAX;
            assert_eq!(
                error_code(&app.handle_pane_graphics_set("overflow".into(), params)),
                "invalid_image"
            );
        }
    }

    #[test]
    fn public_frames_validate_base64_and_public_size_limit() {
        let (mut app, pane_id) = app();
        let mut params = frame(pane_id.clone(), None, "", 1);
        params.format = crate::api::schema::PaneGraphicsFormat::Png;
        params.data = None;
        params.data_base64 = "not base64".into();
        assert_eq!(
            error_code(&app.handle_pane_graphics_set("base64".into(), params)),
            "invalid_image"
        );

        let mut params = frame(pane_id, None, "", 1);
        params.format = crate::api::schema::PaneGraphicsFormat::Png;
        params.data = None;
        params.data_base64 =
            base64::engine::general_purpose::STANDARD
                .encode(vec![1; PANE_GRAPHICS_SET_MAX_BYTES + 1]);
        assert_eq!(
            error_code(&app.handle_pane_graphics_set("large".into(), params)),
            "image_too_large"
        );
    }

    #[test]
    fn aliases_resolve_to_the_same_layer_identity() {
        let (mut app, pane_id) = app();
        let (_, pane) = app.parse_pane_id(&pane_id).unwrap();
        let alias = format!("{pane_id}:alias");
        app.state.public_pane_id_aliases.insert(alias.clone(), pane);
        let params = |target: String, owner: &str| PaneGraphicsStreamParams {
            pane_id: target,
            layer_id: None,
            z_index: 0,
            owner: owner.into(),
        };
        assert!(serde_json::from_str::<SuccessResponse>(
            &app.handle_pane_graphics_stream_open("open".into(), params(pane_id.clone(), "owner"),)
        )
        .is_ok());
        assert_eq!(
            error_code(&app.handle_pane_graphics_stream_open(
                "conflict".into(),
                params(alias.clone(), "other"),
            )),
            "stream_conflict"
        );
        app.handle_pane_graphics_stream_close("stale".into(), params(alias.clone(), "other"));
        assert_eq!(app.pane_graphics.slots.len(), 1);
        app.handle_pane_graphics_stream_close("close".into(), params(alias, "owner"));
        assert!(app.pane_graphics.slots.is_empty());
    }

    #[cfg(unix)]
    fn direct_file(app: &App, name: &str, data: &[u8]) -> String {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = app
            .pane_graphics_files
            .source_directory()
            .unwrap()
            .join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(data).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn sparse_direct_file(app: &App, name: &str, len: usize) -> String {
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = app
            .pane_graphics_files
            .source_directory()
            .unwrap()
            .join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.set_len(len as u64).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn direct_params(
        pane_id: String,
        owner: &str,
        path: String,
    ) -> crate::api::schema::PaneGraphicsDirectParams {
        crate::api::schema::PaneGraphicsDirectParams {
            pane_id,
            layer_id: None,
            z_index: 0,
            owner: owner.into(),
            image_width: 1,
            image_height: 1,
            format: crate::api::schema::PaneGraphicsFormat::Rgba,
            path,
            sequence: 1,
            revision: 1,
            placement: Default::default(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn direct_file_is_leased_or_safely_copied_for_inline_fallback() {
        let (mut app, pane_id) = app();
        let stream = PaneGraphicsStreamParams {
            pane_id: pane_id.clone(),
            layer_id: None,
            z_index: 0,
            owner: "owner".into(),
        };
        app.handle_pane_graphics_stream_open("open".into(), stream.clone());
        for (width, height) in [(0, 1), (1, 0)] {
            let mut params = direct_params(pane_id.clone(), "owner", "unused".into());
            (params.image_width, params.image_height) = (width, height);
            assert_eq!(
                error_code(&app.handle_pane_graphics_stream_direct("invalid".into(), params)),
                "invalid_image"
            );
        }
        for format in [
            crate::api::schema::PaneGraphicsFormat::Rgb,
            crate::api::schema::PaneGraphicsFormat::Png,
        ] {
            let mut params = direct_params(pane_id.clone(), "owner", "unused".into());
            params.format = format;
            assert_eq!(
                error_code(&app.handle_pane_graphics_stream_direct("invalid".into(), params)),
                "invalid_image"
            );
        }
        app.direct_graphics_available = true;
        let path = direct_file(&app, "direct", &[1, 2, 3, 4]);
        let response = app.handle_pane_graphics_stream_direct(
            "direct".into(),
            crate::api::schema::PaneGraphicsDirectParams {
                sequence: 7,
                revision: 8,
                ..direct_params(pane_id.clone(), "owner", path)
            },
        );
        let ack: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            ack.result,
            ResponseResult::PaneGraphicsFrameAck {
                sequence: 7,
                revision: 8
            }
        ));
        assert!(app
            .pane_graphics
            .slots
            .values()
            .next()
            .unwrap()
            .layer
            .as_ref()
            .unwrap()
            .direct_lease()
            .is_some());

        app.handle_pane_graphics_stream_close("close".into(), stream.clone());
        app.handle_pane_graphics_stream_open("reopen".into(), stream);
        app.direct_graphics_available = false;
        let path = direct_file(&app, "fallback", &[5, 6, 7, 8]);
        app.handle_pane_graphics_stream_direct(
            "fallback".into(),
            crate::api::schema::PaneGraphicsDirectParams {
                sequence: 9,
                revision: 10,
                ..direct_params(pane_id, "owner", path)
            },
        );
        assert_eq!(
            app.pane_graphics
                .slots
                .values()
                .next()
                .unwrap()
                .layer
                .as_ref()
                .unwrap()
                .inline_data(),
            Some([5, 6, 7, 8].as_slice())
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_primary_rgba_accepts_fullscreen_retina_frame() {
        let (mut app, pane_id) = app();
        app.direct_graphics_available = true;
        app.handle_pane_graphics_stream_open(
            "open".into(),
            PaneGraphicsStreamParams {
                pane_id: pane_id.clone(),
                layer_id: None,
                z_index: 0,
                owner: "owner".into(),
            },
        );
        let (image_width, image_height) = (3456, 2234);
        let len = image_width * image_height * 4;
        let path = sparse_direct_file(&app, "retina-frame", len as usize);
        let response = app.handle_pane_graphics_stream_direct(
            "frame".into(),
            crate::api::schema::PaneGraphicsDirectParams {
                image_width,
                image_height,
                ..direct_params(pane_id, "owner", path)
            },
        );

        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert!(app
            .pane_graphics
            .slots
            .values()
            .next()
            .unwrap()
            .layer
            .as_ref()
            .unwrap()
            .direct_lease()
            .is_some());
    }

    #[cfg(unix)]
    #[test]
    fn bgra_and_secondary_file_frames_are_canonical_owned_rgba() {
        let (mut app, pane_id) = app();
        app.direct_graphics_available = true;
        for (layer, format, source) in [
            (
                "browser-chrome",
                crate::api::schema::PaneGraphicsFormat::Rgba,
                [1, 2, 3, 4],
            ),
            (
                "primary",
                crate::api::schema::PaneGraphicsFormat::Bgra,
                [3, 2, 1, 4],
            ),
        ] {
            app.handle_pane_graphics_stream_open(
                format!("open-{layer}"),
                PaneGraphicsStreamParams {
                    pane_id: pane_id.clone(),
                    layer_id: Some(layer.into()),
                    z_index: 0,
                    owner: layer.into(),
                },
            );
            let path = direct_file(&app, layer, &source);
            app.handle_pane_graphics_stream_direct(
                format!("frame-{layer}"),
                crate::api::schema::PaneGraphicsDirectParams {
                    layer_id: Some(layer.into()),
                    format,
                    ..direct_params(pane_id.clone(), layer, path)
                },
            );
            let key = (app.parse_pane_id(&pane_id).unwrap().1, layer.into());
            assert_eq!(
                app.pane_graphics.slots[&key]
                    .layer
                    .as_ref()
                    .unwrap()
                    .inline_data(),
                Some([1, 2, 3, 4].as_slice())
            );
        }
    }

    #[test]
    fn disabled_info_and_stream_requests_are_rejected() {
        let (mut app, pane_id) = app();
        app.state.kitty_graphics_enabled = false;
        assert_eq!(
            error_code(&app.handle_pane_graphics_info(
                "info".into(),
                crate::api::schema::PaneTarget {
                    pane_id: pane_id.clone(),
                },
            )),
            "feature_disabled"
        );
        assert_eq!(
            error_code(&app.handle_pane_graphics_stream_open(
                "open".into(),
                PaneGraphicsStreamParams {
                    pane_id,
                    layer_id: None,
                    z_index: 0,
                    owner: "owner".into(),
                },
            )),
            "feature_disabled"
        );
        assert!(app.pane_graphics.slots.is_empty());
    }
}
