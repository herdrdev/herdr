use crate::kitty_graphics::surface::DeliveryCache;
use crate::protocol::{ClientShellPopupSurface, SurfaceGraphicsScene};

pub(crate) fn collect(
    app: &crate::app::App,
    pane_infos: &[crate::layout::PaneInfo],
    split_borders: &[crate::layout::SplitBorder],
    popup: Option<&ClientShellPopupSurface>,
    target: Option<crate::ui::TabSurfaceTarget>,
    cell_size: crate::kitty_graphics::HostCellSize,
    delivered: &DeliveryCache,
    client_id: u64,
) -> (SurfaceGraphicsScene, DeliveryCache) {
    let popup_content_size = popup.map(|popup| (popup.frame.width, popup.frame.height));
    crate::kitty_graphics::surface::collect_scene(
        app,
        crate::ui::TabSurfaceView {
            target,
            pane_infos,
            split_borders,
        },
        popup_content_size,
        cell_size,
        delivered,
        client_id,
    )
}
