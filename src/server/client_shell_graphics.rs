use crate::kitty_graphics::surface::{DeliveryCache, SourceFiles};
use crate::protocol::{ClientShellPopupSurface, SurfaceGraphicsScene};

pub(crate) fn collect_retained(
    app: &crate::app::App,
    surface: &crate::protocol::PaneSurfaceFrame,
    target: crate::ui::TabSurfaceTarget,
    cell_size: crate::kitty_graphics::HostCellSize,
    delivered: &DeliveryCache,
    client_id: u64,
) -> Option<(SurfaceGraphicsScene, DeliveryCache, SourceFiles)> {
    let rect = |rect: crate::protocol::SurfaceRect| {
        ratatui::layout::Rect::new(rect.x, rect.y, rect.width, rect.height)
    };
    let pane_infos = surface
        .panes
        .iter()
        .map(|pane| {
            let (workspace_index, id) = app.parse_pane_id(&pane.pane_id)?;
            if workspace_index != target.workspace_index {
                return None;
            }
            Some(crate::layout::PaneInfo {
                id,
                rect: rect(pane.rect),
                inner_rect: rect(pane.inner_rect),
                scrollbar_rect: pane.scrollbar_rect.map(rect),
                borders: ratatui::widgets::Borders::NONE,
                is_focused: pane.focused,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    // Image clipping uses the retained content rectangles, not borders or split handles.
    Some(collect(
        app,
        &pane_infos,
        &[],
        None,
        Some(target),
        cell_size,
        delivered,
        client_id,
    ))
}

pub(crate) fn collect(
    app: &crate::app::App,
    pane_infos: &[crate::layout::PaneInfo],
    split_borders: &[crate::layout::SplitBorder],
    popup: Option<&ClientShellPopupSurface>,
    target: Option<crate::ui::TabSurfaceTarget>,
    cell_size: crate::kitty_graphics::HostCellSize,
    delivered: &DeliveryCache,
    client_id: u64,
) -> (SurfaceGraphicsScene, DeliveryCache, SourceFiles) {
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
