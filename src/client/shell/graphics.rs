use super::*;

#[cfg(unix)]
pub(crate) struct ClientGraphicsCheckpoint(crate::kitty_graphics::surface::ClientState);

impl ClientShellState {
    #[cfg(all(test, unix))]
    pub(crate) fn graphics_scope(&self) -> &str {
        self.graphics.scope()
    }

    #[cfg(unix)]
    pub(crate) fn accepts_direct_graphics_asset(
        &self,
        key: &crate::protocol::SurfaceGraphicsAssetKey,
        image_id: u32,
    ) -> bool {
        self.graphics.accepts_direct_asset(key, image_id)
    }

    #[cfg(unix)]
    pub(crate) fn direct_graphics_checkpoint(&self) -> ClientGraphicsCheckpoint {
        ClientGraphicsCheckpoint(self.graphics.clone())
    }

    #[cfg(unix)]
    pub(crate) fn restore_direct_graphics_checkpoint(
        &mut self,
        checkpoint: ClientGraphicsCheckpoint,
    ) {
        self.graphics = checkpoint.0;
    }

    #[cfg(unix)]
    pub(crate) fn trust_direct_graphics_asset(
        &mut self,
        key: &crate::protocol::SurfaceGraphicsAssetKey,
        image_id: u32,
    ) -> bool {
        self.graphics.trust_direct_asset(key, image_id)
    }

    #[cfg(unix)]
    pub(crate) fn retire_direct_graphics_image(&mut self, image_id: u32) {
        self.graphics.retire_direct_image(image_id);
    }

    pub(crate) fn take_pending_graphics_cleanup(&mut self) -> Vec<u8> {
        self.graphics.take_pending_cleanup()
    }

    pub(crate) fn set_graphics_cell_size(&mut self, width_px: u32, height_px: u32) {
        self.graphics_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: width_px.max(1),
            height_px: height_px.max(1),
        };
    }

    pub(super) fn compose_graphics(
        &mut self,
        layout: ClientShellLayout,
        occlusion: &crate::kitty_graphics::surface::Occlusion,
    ) -> crate::kitty_graphics::GraphicsOutput {
        let visibility = if self.endpoint_error.is_some() {
            crate::kitty_graphics::surface::Visibility::Hidden
        } else if self.hits.popup.is_some() {
            crate::kitty_graphics::surface::Visibility::Popup
        } else {
            crate::kitty_graphics::surface::Visibility::Main
        };
        let popup_origin = self
            .hits
            .popup
            .as_ref()
            .map(|popup| (popup.inner_rect.x, popup.inner_rect.y));
        self.graphics.encode_output(
            visibility,
            (layout.pane_surface.x, layout.pane_surface.y),
            popup_origin,
            self.graphics_cell_size,
            occlusion,
        )
    }
}
