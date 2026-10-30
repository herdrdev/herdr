//! Lays out composed pane rows in BiDi visual order and maps pane-content
//! columns from the drawn (visual) order back to logical columns.

use super::*;
use crate::pane_display::{RowMap, Scratch};

/// Lays out each row of the pane drawn in `inner`, in screen coordinates.
///
/// The `skip_row` row is left alone, so the mode bar stays intact.
/// The frame cursor follows its glyph when it sits on a reordered row.
pub(super) fn apply_pane_display(
    scratch: &mut Scratch,
    maps: &mut Vec<RowMap>,
    frame: &mut FrameData,
    inner: Rect,
    skip_row: Option<u16>,
) {
    let width = usize::from(frame.width);
    let right = inner.x.saturating_add(inner.width).min(frame.width);
    if inner.x >= right {
        return;
    }
    let bottom = inner.y.saturating_add(inner.height).min(frame.height);
    for y in inner.y..bottom {
        if skip_row == Some(y) {
            continue;
        }
        crate::render_prof::counter("pane_display.rows_scanned", 1);
        let start = usize::from(y) * width + usize::from(inner.x);
        let end = usize::from(y) * width + usize::from(right);
        let Some(segment) = frame.cells.get_mut(start..end) else {
            continue;
        };
        let Some(visual_to_logical) = crate::pane_display::layout_segment(segment, scratch) else {
            continue;
        };
        crate::render_prof::counter("pane_display.rows_reordered", 1);
        let map = RowMap::new(inner.x, y, visual_to_logical);
        if let Some(cursor) = frame
            .cursor
            .as_mut()
            .filter(|cursor| map.contains(cursor.x, cursor.y))
        {
            cursor.x = map.to_visual(cursor.x);
        }
        maps.push(map);
    }
}

/// Whether any graphics placement of the pane surface (`popup == false`) or of
/// the popup (`popup == true`) overlaps `inner`. `origin` is the screen origin
/// those placements are relative to. Image cells are never reordered.
pub(super) fn graphics_overlap(
    graphics: &crate::protocol::SurfaceGraphicsScene,
    popup: bool,
    origin: Rect,
    inner: Rect,
) -> bool {
    graphics
        .placements
        .iter()
        .filter(|placement| is_popup_placement(placement) == popup)
        .any(|placement| {
            let x = u32::from(origin.x) + u32::from(placement.x);
            let y = u32::from(origin.y) + u32::from(placement.y);
            let right = x.saturating_add(placement.cols);
            let bottom = y.saturating_add(placement.rows);
            x < u32::from(inner.x) + u32::from(inner.width)
                && right > u32::from(inner.x)
                && y < u32::from(inner.y) + u32::from(inner.height)
                && bottom > u32::from(inner.y)
        })
}

fn is_popup_placement(placement: &crate::protocol::SurfaceGraphicsPlacement) -> bool {
    matches!(
        placement.asset.source,
        crate::protocol::SurfaceGraphicsSource::Terminal {
            target: crate::protocol::SurfaceGraphicsTarget::Popup { .. },
            ..
        }
    )
}

/// Column ranges `[start, end)` of each pane inner rect on surface row `y`
/// that intersect `[x, x + len)`, skipping panes in BiDi explicit mode. Compose lays out each pane segment on its
/// own, so patch checks must look at the same segments.
fn pane_segments<'a>(
    panes: &'a [crate::protocol::PaneSurfacePane],
    y: u16,
    x: u16,
    len: usize,
    bidi_explicit: &'a impl Fn(&str) -> bool,
) -> impl Iterator<Item = (usize, usize)> + 'a {
    let span_end = usize::from(x).saturating_add(len);
    panes.iter().filter_map(move |pane| {
        if bidi_explicit(&pane.pane_id) {
            return None;
        }
        let inner = pane.inner_rect;
        let inside_row = y >= inner.y && y < inner.y.saturating_add(inner.height);
        let start = usize::from(inner.x);
        let end = start + usize::from(inner.width);
        (inside_row && start < span_end && end > usize::from(x)).then_some((start, end))
    })
}

/// Shifts a one-based, pane-relative pixel x by `column_shift` cells, keeping
/// the pointer's offset inside its cell.
pub(super) fn shift_pixel_x(x: u32, hit: &PaneHit, column_shift: i32) -> u32 {
    if column_shift == 0 || hit.inner_rect.width == 0 {
        return x;
    }
    let cell_width = i64::from(hit.pixel_width / u32::from(hit.inner_rect.width));
    let shifted = i64::from(x) + i64::from(column_shift) * cell_width;
    shifted.clamp(1, i64::from(hit.pixel_width.max(1))) as u32
}

impl ClientShellState {
    /// Logical screen column of the pane glyph drawn at (`column`, `row`).
    ///
    /// Only pane-content handling (forwarded mouse reports, selection, link
    /// hover and activation) converts columns through this. Shell chrome,
    /// menus, and overlays hit-test the raw pointer position. Popup maps win
    /// while the pointer is inside the popup.
    pub(super) fn content_column(&self, column: u16, row: u16) -> u16 {
        let in_popup = self
            .hits
            .popup
            .as_ref()
            .is_some_and(|hit| super::contains(hit.rect, (column, row)));
        let maps = if in_popup {
            &self.popup_display_maps
        } else {
            &self.display_maps
        };
        maps.iter()
            .find(|map| map.contains(column, row))
            .map_or(column, |map| map.to_logical(column))
    }

    /// Whether the endpoint reports that the pane application switched BiDi
    /// to explicit mode (`CSI 8 l`). Such panes already draw visual order,
    /// so their rows are never reordered again.
    pub(super) fn pane_bidi_explicit(&self, pane_id: &str) -> bool {
        self.snapshot.as_deref().is_some_and(|snapshot| {
            snapshot
                .panes
                .iter()
                .any(|pane| pane.bidi_explicit && pane.pane_id == pane_id)
        })
    }

    /// Visual screen column where the tiled pane glyph at logical
    /// (`column`, `row`) is drawn; the inverse of [`Self::content_column`].
    pub(super) fn visual_column(&self, column: u16, row: u16) -> u16 {
        self.display_maps
            .iter()
            .find(|map| map.contains(column, row))
            .map_or(column, |map| map.to_visual(column))
    }

    /// Whether a surface patch could change a row BiDi layout reorders. Such patches recompose instead of taking the span fast path,
    /// because the forwarded span would land on logical columns.
    pub(super) fn patch_touches_display_rows(
        &self,
        patch: &crate::protocol::PaneSurfacePatch,
    ) -> bool {
        if !self.config.lays_out_bidi() {
            return false;
        }
        let Some(surface) = self.pane_surface.as_ref() else {
            return true;
        };
        let frame = &surface.frame;
        let panes = if patch.panes.is_empty() {
            &surface.panes
        } else {
            &patch.panes
        };
        let width = usize::from(frame.width);
        let bidi_explicit = |pane_id: &str| self.pane_bidi_explicit(pane_id);
        let old_affects = |y: u16, x: u16, len: usize| {
            let row = usize::from(y) * width;
            pane_segments(&surface.panes, y, x, len, &bidi_explicit).any(|(start, end)| {
                frame
                    .cells
                    .get(row + start..row + end.min(width))
                    .is_some_and(crate::pane_display::affects)
            })
        };
        let new_affects = |row: &crate::protocol::PaneSurfacePatchRow| {
            pane_segments(panes, row.y, row.x, row.cells.len(), &bidi_explicit).any(
                |(start, end)| {
                    let from = start.saturating_sub(usize::from(row.x));
                    let to = (end - usize::from(row.x).min(end)).min(row.cells.len());
                    row.cells
                        .get(from..to)
                        .is_some_and(crate::pane_display::affects)
                },
            )
        };
        patch
            .rows
            .iter()
            .any(|row| old_affects(row.y, row.x, row.cells.len()) || new_affects(row))
            || [frame.cursor.as_ref(), patch.cursor.as_ref()]
                .into_iter()
                .flatten()
                .any(|cursor| old_affects(cursor.y, cursor.x, 1))
    }
}
