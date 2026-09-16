//! Virtual rendering helpers for headless client frame streaming.

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};

use crate::app::state::AppState;
use crate::protocol::render_ansi::{BlitEncoder, EncodedBlit};
use crate::protocol::{
    CursorState, FrameData, PaneSurfaceFrame, PaneSurfacePatch, RenderEncoding, ServerMessage,
    TerminalFrame,
};
use crate::terminal::TerminalRuntimeRegistry;

/// Per-client render baseline for the negotiated render encoding.
pub(crate) enum ClientRenderState {
    /// Semantic clients compare full frame data and skip identical frames.
    Semantic {
        last_surface: Option<Box<PaneSurfaceFrame>>,
        surface_revision: u64,
        surface_reuse: bool,
        surface_delta: bool,
        recompute_pending: bool,
        hyperlink_index: std::collections::HashMap<String, u32>,
        retained_hyperlink_bytes: usize,
    },
    /// Terminal-ANSI clients keep a terminal diff encoder and sequence number.
    TerminalAnsi {
        blit_encoder: BlitEncoder,
        seq: u64,
        repaint_pending: bool,
    },
}

impl ClientRenderState {
    pub(crate) fn new(render_encoding: RenderEncoding) -> Self {
        match render_encoding {
            RenderEncoding::SemanticFrame => Self::Semantic {
                last_surface: None,
                surface_revision: 0,
                surface_reuse: false,
                surface_delta: false,
                recompute_pending: false,
                hyperlink_index: std::collections::HashMap::new(),
                retained_hyperlink_bytes: 0,
            },
            RenderEncoding::TerminalAnsi => Self::TerminalAnsi {
                blit_encoder: BlitEncoder::new(),
                seq: 0,
                repaint_pending: false,
            },
        }
    }

    pub(crate) fn enable_surface_reuse(&mut self, enabled: bool) {
        if let Self::Semantic { surface_reuse, .. } = self {
            *surface_reuse = enabled;
        }
    }

    pub(crate) fn enable_surface_delta(&mut self, enabled: bool) {
        if let Self::Semantic { surface_delta, .. } = self {
            *surface_delta = enabled;
        }
    }

    pub(crate) fn request_recompute(&mut self) {
        if let Self::Semantic {
            surface_delta: true,
            recompute_pending,
            ..
        } = self
        {
            *recompute_pending = true;
        } else {
            self.request_repaint();
        }
    }

    pub(crate) fn requires_recompute(&self) -> bool {
        matches!(
            self,
            Self::Semantic {
                recompute_pending: true,
                ..
            }
        )
    }

    pub(crate) fn reset_baseline(&mut self) {
        match self {
            Self::Semantic {
                last_surface,
                hyperlink_index,
                retained_hyperlink_bytes,
                ..
            } => {
                *last_surface = None;
                hyperlink_index.clear();
                *retained_hyperlink_bytes = 0;
            }
            Self::TerminalAnsi {
                blit_encoder,
                repaint_pending,
                ..
            } => {
                *blit_encoder = BlitEncoder::new();
                *repaint_pending = false;
            }
        }
    }

    pub(crate) fn request_repaint(&mut self) {
        match self {
            Self::Semantic {
                last_surface,
                hyperlink_index,
                retained_hyperlink_bytes,
                ..
            } => {
                *last_surface = None;
                hyperlink_index.clear();
                *retained_hyperlink_bytes = 0;
            }
            Self::TerminalAnsi {
                repaint_pending, ..
            } => *repaint_pending = true,
        }
    }

    pub(crate) fn prepare_frame(&mut self, frame: FrameData) -> Option<PreparedRender> {
        match self {
            Self::Semantic { .. } => None,
            Self::TerminalAnsi {
                blit_encoder,
                seq,
                repaint_pending,
            } => {
                if !*repaint_pending && blit_encoder.is_current(&frame) {
                    crate::render_prof::event("prepare_frame.ansi.skip_current");
                    return None;
                }
                let mut encoded = blit_encoder.encode(&frame, *repaint_pending);
                crate::render_prof::event("prepare_frame.ansi.changed");
                crate::render_prof::counter("prepare_frame.ansi.bytes", encoded.bytes.len() as u64);
                if encoded.full {
                    crate::render_prof::event("prepare_frame.ansi.full");
                } else {
                    crate::render_prof::event("prepare_frame.ansi.partial");
                }
                insert_graphics_before_sync_end(&mut encoded.bytes, &frame.graphics);
                crate::render_prof::counter(
                    "prepare_frame.graphics.bytes",
                    frame.graphics.len() as u64,
                );
                Some(PreparedRender::TerminalAnsi {
                    message: ServerMessage::Terminal(TerminalFrame {
                        seq: *seq + 1,
                        width: frame.width,
                        height: frame.height,
                        full: encoded.full,
                        bytes: encoded.bytes.clone(),
                    }),
                    frame,
                    encoded: Some(encoded),
                })
            }
        }
    }

    pub(crate) fn last_pane_surface(&self) -> Option<&PaneSurfaceFrame> {
        match self {
            Self::Semantic { last_surface, .. } => last_surface.as_deref(),
            Self::TerminalAnsi { .. } => None,
        }
    }

    pub(crate) fn prepare_pane_surface_delta(
        &self,
        mut delta: crate::protocol::delta::ClientShellSurfaceDelta,
    ) -> Option<PreparedRender> {
        let Self::Semantic {
            last_surface,
            surface_revision,
            ..
        } = self
        else {
            return None;
        };
        let last = last_surface.as_deref()?;
        if last.boot_id != delta.boot_id || last.surface_revision != delta.base_surface_revision {
            return None;
        }
        delta.surface_revision = surface_revision.saturating_add(1);
        prepared_legacy_patch_or_v2_delta(last, delta)
    }

    fn surface_delta_changed_cells(
        delta: &crate::protocol::delta::ClientShellSurfaceDelta,
    ) -> usize {
        delta.spans.iter().map(|span| span.cells.len()).sum()
    }

    /// Near-full cell rewrites encode larger as a v2 delta than as a seeded
    /// `PaneSurface`. Spinner ticks are 1–2 cells and must not pay that encode.
    fn v2_delta_should_yield_to_full(changed_cells: usize, frame_cells: usize) -> bool {
        changed_cells > 64 && changed_cells > frame_cells / 4
    }

    pub(crate) fn prepare_pane_surface_v2(
        &mut self,
        surface: PaneSurfaceFrame,
    ) -> Option<PreparedRender> {
        let Self::Semantic {
            last_surface,
            surface_revision,
            hyperlink_index,
            retained_hyperlink_bytes,
            ..
        } = self
        else {
            return None;
        };

        let is_compatible = last_surface.as_deref().is_some_and(|last| {
            last.boot_id == surface.boot_id
                && last.frame.width == surface.frame.width
                && last.frame.height == surface.frame.height
                && last.panes.len() == surface.panes.len()
                && last.panes.iter().zip(&surface.panes).all(|(left, right)| {
                    left.pane_id == right.pane_id
                        && left.rect == right.rect
                        && left.inner_rect == right.inner_rect
                })
        });
        if !is_compatible {
            return Some(seed_full_pane_surface(surface_revision, surface));
        }

        let last = last_surface.as_deref().expect("validated baseline");
        let popup_identity_changed = match (&last.popup, &surface.popup) {
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
            (Some(previous), Some(next)) => previous.terminal_id != next.terminal_id,
        };
        if popup_identity_changed {
            return Some(seed_full_pane_surface(surface_revision, surface));
        }

        if last.projection_revision == surface.projection_revision
            && last.frame == surface.frame
            && last.panes == surface.panes
            && last.splits == surface.splits
            && last.popup == surface.popup
            && last.graphics.placements == surface.graphics.placements
            && last.graphics.retained_assets == surface.graphics.retained_assets
            && surface.graphics.assets.is_empty()
        {
            return None;
        }

        if last.projection_revision != surface.projection_revision
            && last.frame == surface.frame
            && last.panes == surface.panes
            && last.splits == surface.splits
            && last.popup == surface.popup
            && last.graphics.placements == surface.graphics.placements
            && last.graphics.retained_assets == surface.graphics.retained_assets
            && surface.graphics.assets.is_empty()
        {
            if let Some(last) = last_surface.as_mut() {
                last.projection_revision = surface.projection_revision;
            }
            return None;
        }

        let mut appended_hyperlinks = Vec::new();
        let mut appended_bytes = 0usize;
        let mut link_map = std::collections::HashMap::new();
        for (new_idx, uri) in surface.frame.hyperlinks.iter().enumerate() {
            if let Some(&old_idx) = hyperlink_index.get(uri) {
                link_map.insert(new_idx as u32, old_idx);
            } else if let Some(appended_pos) = appended_hyperlinks.iter().position(|u| u == uri) {
                link_map.insert(
                    new_idx as u32,
                    (last.frame.hyperlinks.len() + appended_pos) as u32,
                );
            } else {
                let uri_bytes = uri.len().saturating_add(std::mem::size_of::<String>());
                if retained_hyperlink_bytes
                    .saturating_add(appended_bytes)
                    .saturating_add(uri_bytes)
                    > crate::protocol::MAX_FRAME_SIZE
                {
                    return Some(seed_full_pane_surface(surface_revision, surface));
                }
                let next_idx = (last.frame.hyperlinks.len() + appended_hyperlinks.len()) as u32;
                appended_hyperlinks.push(uri.clone());
                appended_bytes = appended_bytes.saturating_add(uri_bytes);
                link_map.insert(new_idx as u32, next_idx);
            }
        }

        let cells_equal = |last_cell: &crate::protocol::CellData,
                           next_cell: &crate::protocol::CellData| {
            if last_cell.symbol != next_cell.symbol
                || last_cell.fg != next_cell.fg
                || last_cell.bg != next_cell.bg
                || last_cell.modifier != next_cell.modifier
                || last_cell.skip != next_cell.skip
            {
                return false;
            }
            last_cell.hyperlink
                == match next_cell.hyperlink {
                    None => None,
                    Some(idx) => link_map.get(&idx).copied(),
                }
        };
        let remap_cell = |cell: &crate::protocol::CellData| {
            let mut cell = cell.clone();
            if let Some(idx) = cell.hyperlink {
                cell.hyperlink = link_map.get(&idx).copied();
            }
            cell
        };

        let width = usize::from(surface.frame.width);
        let height = usize::from(surface.frame.height);
        let frame_cells = last.frame.cells.len();
        let mut changed_cells = 0usize;
        let mut spans = Vec::new();
        for y in 0..height {
            let row_start = y * width;
            let mut x = 0;
            while x < width {
                if cells_equal(
                    &last.frame.cells[row_start + x],
                    &surface.frame.cells[row_start + x],
                ) {
                    x += 1;
                    continue;
                }
                let start = x;
                x += 1;
                while x < width
                    && !cells_equal(
                        &last.frame.cells[row_start + x],
                        &surface.frame.cells[row_start + x],
                    )
                {
                    x += 1;
                }
                let span_len = x - start;
                spans.push(crate::protocol::PaneSurfacePatchRow {
                    x: start as u16,
                    y: y as u16,
                    cells: (start..x)
                        .map(|col| remap_cell(&surface.frame.cells[row_start + col]))
                        .collect(),
                });
                changed_cells = changed_cells.saturating_add(span_len);
                if Self::v2_delta_should_yield_to_full(changed_cells, frame_cells) {
                    return Some(seed_full_pane_surface(surface_revision, surface));
                }
            }
        }

        let panes = last
            .panes
            .iter()
            .zip(&surface.panes)
            .filter_map(|(base, next)| {
                crate::protocol::delta::PaneSurfacePaneDelta::diff(base, next)
            })
            .collect::<Vec<_>>();
        let splits = (last.splits != surface.splits).then(|| surface.splits.clone());
        let cursor = crate::protocol::delta::SurfaceFieldUpdate::diff(
            &last.frame.cursor,
            &surface.frame.cursor,
        );
        let graphics = graphics_delta(&last.graphics, &surface.graphics);
        let popup = popup_delta(last.popup.as_deref(), surface.popup.as_deref(), &link_map);

        if spans.is_empty()
            && panes.is_empty()
            && splits.is_none()
            && matches!(
                cursor,
                crate::protocol::delta::SurfaceFieldUpdate::Unchanged
            )
            && graphics.is_none()
            && popup.is_none()
            && last.projection_revision == surface.projection_revision
        {
            return None;
        }

        let next_rev = surface_revision.saturating_add(1);
        let delta = crate::protocol::delta::ClientShellSurfaceDelta {
            boot_id: surface.boot_id.clone(),
            projection_revision: surface.projection_revision,
            base_surface_revision: last.surface_revision,
            surface_revision: next_rev,
            spans,
            row_moves: Vec::new(),
            panes,
            splits,
            cursor,
            appended_hyperlinks,
            graphics,
            popup,
        };
        match prepared_legacy_patch_or_v2_delta(last, delta) {
            Some(prepared) => Some(prepared),
            None => Some(seed_full_pane_surface(surface_revision, surface)),
        }
    }

    pub(crate) fn prepare_pane_surface(
        &mut self,
        mut surface: PaneSurfaceFrame,
    ) -> Option<PreparedRender> {
        let Self::Semantic {
            last_surface,
            surface_revision,
            surface_reuse,
            surface_delta,
            recompute_pending,
            ..
        } = self
        else {
            return None;
        };
        if !*recompute_pending
            && surface.graphics.assets.is_empty()
            && last_surface.as_deref().is_some_and(|last| {
                last.projection_revision == surface.projection_revision
                    && last.frame == surface.frame
                    && last.panes == surface.panes
                    && last.splits == surface.splits
                    && last.popup == surface.popup
                    && last.graphics.placements == surface.graphics.placements
                    && last.graphics.retained_assets == surface.graphics.retained_assets
            })
        {
            return None;
        }
        surface.surface_revision = surface_revision.saturating_add(1);
        let assets = std::mem::take(&mut surface.graphics.assets);
        let committed_surface = surface.clone();
        surface.graphics.assets = assets;
        let mut message = ServerMessage::PaneSurface(surface);
        let delta = (*surface_delta)
            .then_some(last_surface.as_deref())
            .flatten()
            .and_then(|last| {
                crate::protocol::surface_delta::message(last, &mut message)
                    .map_err(|error| tracing::warn!(%error, "failed to encode surface delta"))
                    .ok()
                    .flatten()
            });
        let reused = if let ServerMessage::PaneSurface(surface) = &mut message {
            (delta.is_none() && *surface_reuse)
                .then_some(last_surface.as_deref())
                .flatten()
                .filter(|last| {
                    last.boot_id == surface.boot_id
                        && last.frame == surface.frame
                        // Popup cells are not part of the reusable grid; keep their compact codec.
                        && surface.popup.is_none()
                        && surface.graphics.assets.is_empty()
                })
                .and_then(|last| {
                    crate::protocol::surface_reuse::message(last.surface_revision, surface)
                        .map_err(|error| tracing::warn!(%error, "failed to encode surface reuse"))
                        .ok()
                        .flatten()
                })
        } else {
            None
        };
        Some(PreparedRender::Semantic {
            message: delta.or(reused).unwrap_or(message),
            committed_surface: Box::new(committed_surface),
        })
    }

    pub(crate) fn prepare_pane_surface_patch(
        &self,
        mut patch: PaneSurfacePatch,
    ) -> Option<PreparedRender> {
        let Self::Semantic {
            last_surface,
            surface_revision,
            ..
        } = self
        else {
            return None;
        };
        if self.requires_recompute() {
            return None;
        }
        let last = last_surface.as_deref()?;
        if last.boot_id != patch.boot_id
            || last.projection_revision != patch.projection_revision
            || last.surface_revision != patch.base_surface_revision
        {
            return None;
        }
        let next_revision = surface_revision.saturating_add(1);
        patch.surface_revision = next_revision;
        Some(PreparedRender::SemanticPatch {
            message: ServerMessage::PaneSurfacePatch(patch),
        })
    }

    pub(crate) fn commit_sent_frame(&mut self, prepared: PreparedRender) {
        match (self, prepared) {
            (
                Self::Semantic {
                    last_surface,
                    surface_revision,
                    recompute_pending,
                    hyperlink_index,
                    retained_hyperlink_bytes,
                    ..
                },
                PreparedRender::Semantic {
                    committed_surface, ..
                },
            ) => {
                *surface_revision = committed_surface.surface_revision;
                let (index, bytes) = hyperlink_index_from_surface(&committed_surface);
                *hyperlink_index = index;
                *retained_hyperlink_bytes = bytes;
                *last_surface = Some(committed_surface);
                *recompute_pending = false;
            }
            (
                Self::Semantic {
                    last_surface,
                    surface_revision,
                    hyperlink_index,
                    retained_hyperlink_bytes,
                    ..
                },
                PreparedRender::SurfaceDelta { delta, .. },
            ) => {
                let Some(surface) = last_surface.as_deref_mut() else {
                    tracing::warn!(
                        "missing surface baseline during surface delta commit; resetting"
                    );
                    *last_surface = None;
                    return;
                };
                let mut graphics = std::mem::take(&mut surface.graphics);
                if let Err(err) = delta.apply_to(surface, &mut graphics) {
                    tracing::warn!(error = %err, "failed to apply surface delta during commit; resetting baseline");
                    *last_surface = None;
                    return;
                }
                surface.graphics = graphics;
                *surface_revision = delta.surface_revision;
                let (index, bytes) = hyperlink_index_from_surface(surface);
                *hyperlink_index = index;
                *retained_hyperlink_bytes = bytes;
            }
            (
                Self::Semantic {
                    last_surface,
                    surface_revision,
                    ..
                },
                PreparedRender::SemanticPatch {
                    message: ServerMessage::PaneSurfacePatch(patch),
                },
            ) => {
                let surface = last_surface
                    .as_deref_mut()
                    .expect("prepared patch baseline");
                apply_pane_surface_patch(surface, &patch);
                *surface_revision = patch.surface_revision;
            }
            (
                Self::TerminalAnsi {
                    blit_encoder,
                    seq,
                    repaint_pending,
                },
                PreparedRender::TerminalAnsi {
                    frame,
                    encoded: Some(encoded),
                    ..
                },
            ) => {
                blit_encoder.commit(frame, encoded);
                *seq += 1;
                *repaint_pending = false;
            }
            _ => {}
        }
    }
}

// Planning validates all rows and pane IDs before any send. The server does not yield
// between planning and commit, so applying the accepted patch cannot fail partway through.
pub(super) fn apply_pane_surface_patch(surface: &mut PaneSurfaceFrame, patch: &PaneSurfacePatch) {
    debug_assert_eq!(surface.boot_id, patch.boot_id);
    debug_assert_eq!(surface.projection_revision, patch.projection_revision);
    debug_assert_eq!(surface.surface_revision, patch.base_surface_revision);
    for row in &patch.rows {
        let start = usize::from(row.y) * usize::from(surface.frame.width) + usize::from(row.x);
        surface.frame.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
    }
    for updated in &patch.panes {
        let pane = surface
            .panes
            .iter_mut()
            .find(|pane| pane.pane_id == updated.pane_id)
            .expect("planned patch pane");
        pane.clone_from(updated);
    }
    surface.frame.cursor.clone_from(&patch.cursor);
    surface.surface_revision = patch.surface_revision;
}

fn hyperlink_index_from_surface(
    surface: &PaneSurfaceFrame,
) -> (std::collections::HashMap<String, u32>, usize) {
    let mut index = std::collections::HashMap::new();
    let mut bytes = 0usize;
    for (i, uri) in surface.frame.hyperlinks.iter().enumerate() {
        index.insert(uri.clone(), i as u32);
        bytes = bytes.saturating_add(uri.len().saturating_add(std::mem::size_of::<String>()));
    }
    (index, bytes)
}

fn encoded_size(message: &ServerMessage) -> Option<usize> {
    let payload = bincode::serde::encode_to_vec(message, bincode::config::standard()).ok()?;
    Some(4usize.saturating_add(payload.len()))
}

fn prepared_legacy_patch_or_v2_delta(
    last: &PaneSurfaceFrame,
    delta: crate::protocol::delta::ClientShellSurfaceDelta,
) -> Option<PreparedRender> {
    let changed = ClientRenderState::surface_delta_changed_cells(&delta);
    if ClientRenderState::v2_delta_should_yield_to_full(changed, last.frame.cells.len()) {
        return None;
    }
    match delta.into_legacy_patch(last) {
        Ok(patch) => Some(PreparedRender::SemanticPatch {
            message: ServerMessage::PaneSurfacePatch(patch),
        }),
        Err(delta) => {
            let encoded = crate::protocol::delta::encode_surface_delta(&delta).ok()?;
            let message = ServerMessage::EndpointControl {
                kind: crate::protocol::delta::SURFACE_CODEC_DELTA_V2.into(),
                data: encoded,
            };
            if changed > 64 {
                let full = ServerMessage::PaneSurface(last.clone());
                if encoded_size(&message).unwrap_or(usize::MAX) >= encoded_size(&full).unwrap_or(0)
                {
                    return None;
                }
            }
            Some(PreparedRender::SurfaceDelta {
                message,
                delta: Box::new(delta),
            })
        }
    }
}

fn seed_full_pane_surface(surface_revision: &u64, mut surface: PaneSurfaceFrame) -> PreparedRender {
    surface.surface_revision = surface_revision.saturating_add(1);
    let mut committed_surface = surface.clone();
    committed_surface.graphics.assets.clear();
    PreparedRender::Semantic {
        message: ServerMessage::PaneSurface(surface),
        committed_surface: Box::new(committed_surface),
    }
}

fn graphics_delta(
    last: &crate::protocol::SurfaceGraphicsScene,
    next: &crate::protocol::SurfaceGraphicsScene,
) -> Option<crate::protocol::delta::SurfaceGraphicsDelta> {
    let last_known = last
        .assets
        .iter()
        .map(|asset| asset.key.clone())
        .chain(last.retained_assets.iter().cloned())
        .collect::<std::collections::HashSet<_>>();
    let next_live = next
        .assets
        .iter()
        .map(|asset| asset.key.clone())
        .chain(next.retained_assets.iter().cloned())
        .chain(
            next.placements
                .iter()
                .map(|placement| placement.asset.clone()),
        )
        .collect::<std::collections::HashSet<_>>();

    let added_assets = next
        .assets
        .iter()
        .filter(|asset| !last_known.contains(&asset.key))
        .cloned()
        .collect::<Vec<_>>();
    let removed_assets = last_known
        .into_iter()
        .filter(|key| !next_live.contains(key))
        .collect::<Vec<_>>();
    let added_placements = next
        .placements
        .iter()
        .filter(|placement| {
            last.placements.iter().all(|existing| {
                existing.asset != placement.asset
                    || existing.logical_placement_id != placement.logical_placement_id
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let removed_placements = last
        .placements
        .iter()
        .filter(|placement| {
            next.placements.iter().all(|existing| {
                existing.asset != placement.asset
                    || existing.logical_placement_id != placement.logical_placement_id
            })
        })
        .map(
            |placement| crate::protocol::delta::SurfaceGraphicsPlacementKey {
                asset: placement.asset.clone(),
                logical_placement_id: placement.logical_placement_id,
            },
        )
        .collect::<Vec<_>>();
    if added_assets.is_empty()
        && removed_assets.is_empty()
        && added_placements.is_empty()
        && removed_placements.is_empty()
        && last.retained_assets == next.retained_assets
    {
        return None;
    }
    Some(crate::protocol::delta::SurfaceGraphicsDelta {
        added_assets,
        removed_assets,
        added_placements,
        removed_placements,
        retained_assets: next.retained_assets.clone(),
    })
}

fn popup_delta(
    last: Option<&crate::protocol::ClientShellPopupSurface>,
    next: Option<&crate::protocol::ClientShellPopupSurface>,
    _link_map: &std::collections::HashMap<u32, u32>,
) -> Option<crate::protocol::delta::ClientShellPopupDelta> {
    match (last, next) {
        (None, None) => None,
        (Some(_), None) => Some(crate::protocol::delta::ClientShellPopupDelta {
            seed: None,
            clear: true,
            spans: Vec::new(),
            cursor: crate::protocol::delta::SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
        }),
        (None, Some(next)) => Some(crate::protocol::delta::ClientShellPopupDelta {
            seed: Some(Box::new(next.clone())),
            clear: false,
            spans: Vec::new(),
            cursor: crate::protocol::delta::SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
        }),
        (Some(last), Some(next)) if last.terminal_id == next.terminal_id => {
            if last.frame == next.frame
                && last.title == next.title
                && last.width == next.width
                && last.height == next.height
            {
                return None;
            }
            if last.frame.width != next.frame.width
                || last.frame.height != next.frame.height
                || last.title != next.title
                || last.width != next.width
                || last.height != next.height
            {
                return Some(crate::protocol::delta::ClientShellPopupDelta {
                    seed: Some(Box::new(next.clone())),
                    clear: false,
                    spans: Vec::new(),
                    cursor: crate::protocol::delta::SurfaceFieldUpdate::Unchanged,
                    appended_hyperlinks: Vec::new(),
                });
            }
            let width = usize::from(next.frame.width);
            let height = usize::from(next.frame.height);
            let mut spans = Vec::new();
            for y in 0..height {
                let row_start = y * width;
                let mut x = 0;
                while x < width {
                    if last.frame.cells[row_start + x] == next.frame.cells[row_start + x] {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    x += 1;
                    while x < width
                        && last.frame.cells[row_start + x] != next.frame.cells[row_start + x]
                    {
                        x += 1;
                    }
                    spans.push(crate::protocol::PaneSurfacePatchRow {
                        x: start as u16,
                        y: y as u16,
                        cells: next.frame.cells[row_start + start..row_start + x].to_vec(),
                    });
                }
            }
            Some(crate::protocol::delta::ClientShellPopupDelta {
                seed: None,
                clear: false,
                spans,
                cursor: crate::protocol::delta::SurfaceFieldUpdate::diff(
                    &last.frame.cursor,
                    &next.frame.cursor,
                ),
                appended_hyperlinks: Vec::new(),
            })
        }
        (Some(_), Some(next)) => Some(crate::protocol::delta::ClientShellPopupDelta {
            seed: Some(Box::new(next.clone())),
            clear: false,
            spans: Vec::new(),
            cursor: crate::protocol::delta::SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
        }),
    }
}

fn insert_graphics_before_sync_end(encoded: &mut Vec<u8>, graphics: &[u8]) {
    if graphics.is_empty() {
        return;
    }

    if let Some(sync_end) = crate::protocol::render_ansi::final_sync_output_end(encoded) {
        encoded.splice(sync_end..sync_end, graphics.iter().copied());
    } else {
        encoded.extend_from_slice(graphics);
    }
}

/// A prepared client render message plus any baseline state needed after send.
pub(crate) enum PreparedRender {
    Semantic {
        message: ServerMessage,
        committed_surface: Box<PaneSurfaceFrame>,
    },
    SemanticPatch {
        message: ServerMessage,
    },
    SurfaceDelta {
        message: ServerMessage,
        delta: Box<crate::protocol::delta::ClientShellSurfaceDelta>,
    },
    TerminalAnsi {
        message: ServerMessage,
        frame: FrameData,
        encoded: Option<EncodedBlit>,
    },
}

impl PreparedRender {
    pub(crate) fn message(&self) -> &ServerMessage {
        match self {
            Self::Semantic { message, .. }
            | Self::SemanticPatch { message }
            | Self::SurfaceDelta { message, .. }
            | Self::TerminalAnsi { message, .. } => message,
        }
    }

    pub(crate) fn strip_pane_surface_assets(&mut self) -> bool {
        let Self::Semantic {
            message: ServerMessage::PaneSurface(surface),
            ..
        } = self
        else {
            return false;
        };
        if surface.graphics.assets.is_empty() {
            return false;
        }
        surface.graphics.assets.clear();
        true
    }
}

struct CursorTrackingBackend {
    inner: TestBackend,
    rendered_cursor: Option<Position>,
}

impl CursorTrackingBackend {
    fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            rendered_cursor: None,
        }
    }

    fn buffer(&self) -> &ratatui::buffer::Buffer {
        self.inner.buffer()
    }

    fn rendered_cursor(&self) -> Option<CursorState> {
        self.rendered_cursor.map(|pos| CursorState {
            x: pos.x,
            y: pos.y,
            visible: true,
            shape: 0,
        })
    }
}

impl Backend for CursorTrackingBackend {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()?;
        self.rendered_cursor = None;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.rendered_cursor = Some(position);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

pub(crate) type RenderedTabSurface = (
    ratatui::buffer::Buffer,
    Option<CursorState>,
    Vec<((u16, u16), String, String)>,
    crate::ui::TabSurfaceLayout,
);

/// Renders only the active tab's pane surface at an origin-relative client viewport.
pub(crate) fn render_tab_surface_virtual(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    target: Option<crate::ui::TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> RenderedTabSurface {
    let layout = crate::ui::compute_tab_surface_for(
        app_state,
        terminal_runtimes,
        target,
        area,
        resize_panes,
        cell_size,
    );
    let surface = crate::ui::TabSurfaceView {
        target: layout.target,
        pane_infos: &layout.pane_infos,
        split_borders: &layout.split_borders,
    };
    let cursor = crate::ui::tab_surface_cursor(app_state, terminal_runtimes, surface);
    let hyperlinks = crate::ui::tab_surface_hyperlinks(app_state, terminal_runtimes, surface);

    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");
    terminal
        .draw(|frame| {
            crate::ui::render_tab_surface(app_state, terminal_runtimes, surface, frame);
        })
        .expect("render to TestBackend should never fail");

    (
        terminal.backend().buffer().clone(),
        cursor,
        hyperlinks,
        layout,
    )
}

/// Renders one server-owned terminal directly for `terminal attach` clients.
pub(crate) fn render_terminal_virtual(
    runtime: &crate::terminal::TerminalRuntime,
    area: Rect,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let suppress_cursor = runtime.synchronized_output_active();
    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    terminal
        .draw(|frame| {
            runtime.render(frame, area, true);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.backend().buffer().clone();
    let cursor = (!suppress_cursor)
        .then(|| runtime.cursor_state(area, true))
        .flatten()
        .map(|cursor| CursorState {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
            shape: cursor.shape,
        })
        .or_else(|| {
            (!suppress_cursor)
                .then(|| terminal.backend().rendered_cursor())
                .flatten()
        });

    (buffer, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        ClientShellPopupSurface, PaneSurfacePane, PaneSurfacePatchRow, SurfaceRect,
    };

    fn popup_surface(content: &str) -> PaneSurfaceFrame {
        let pane = ratatui::buffer::Buffer::with_lines(["pane"]);
        let popup = ratatui::buffer::Buffer::with_lines([content]);
        PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(&pane, None, &[]),
            panes: Vec::new(),
            splits: Vec::new(),
            popup: Some(Box::new(ClientShellPopupSurface {
                terminal_id: "popup-terminal".into(),
                title: "popup".into(),
                width: None,
                height: None,
                frame: FrameData::from_ratatui_buffer_with_hyperlinks(&popup, None, &[]),
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            })),
            graphics: crate::protocol::SurfaceGraphicsScene::default(),
        }
    }

    fn pane_surface(content: &str) -> PaneSurfaceFrame {
        let pane = ratatui::buffer::Buffer::with_lines([content]);
        let frame = FrameData::from_ratatui_buffer_with_hyperlinks(&pane, None, &[]);
        let width = frame.width;
        let height = frame.height;
        PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame,
            panes: vec![PaneSurfacePane {
                pane_id: "pane_1".into(),
                content_revision: 1,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                scrollbar_rect: None,
                rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                inner_rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: Vec::new(),
            popup: None,
            graphics: crate::protocol::SurfaceGraphicsScene::default(),
        }
    }

    #[test]
    fn surface_delta_recompute_preserves_wire_baseline_but_epoch_reset_drops_it() {
        for enabled in [false, true] {
            let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
            state.enable_surface_delta(enabled);
            let mut surface = popup_surface("popup");
            surface.popup = None;
            surface.frame = FrameData::from_ratatui_buffer(
                &ratatui::buffer::Buffer::empty(Rect::new(0, 0, 120, 40)),
                None,
            );
            let initial = state.prepare_pane_surface(surface.clone()).unwrap();
            state.commit_sent_frame(initial);
            state.request_recompute();
            assert_eq!(state.last_pane_surface().is_some(), enabled);
            assert_eq!(state.requires_recompute(), enabled);
            // A freshness request still emits a new revision when every cell is equal.
            let fresh = state.prepare_pane_surface(surface.clone()).unwrap();
            assert_eq!(
                matches!(fresh.message(), ServerMessage::EndpointControl { kind, .. }
                if kind == crate::protocol::surface_delta::MESSAGE_KIND),
                enabled
            );
            assert_eq!(
                state.requires_recompute(),
                enabled,
                "prepare must not commit"
            );
            state.commit_sent_frame(fresh);
            assert!(!state.requires_recompute());
            assert_eq!(state.last_pane_surface().unwrap().surface_revision, 2);
            state.request_repaint();
            assert!(state.last_pane_surface().is_none());
            let recovery = state.prepare_pane_surface(surface).unwrap();
            assert!(
                matches!(recovery.message(), ServerMessage::PaneSurface(frame) if frame.surface_revision == 3)
            );
        }
    }

    #[test]
    fn surface_reuse_preserves_projection_and_patch_baselines_without_resending_cells() {
        for enabled in [false, true] {
            let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
            state.enable_surface_reuse(enabled);
            let mut decoder = crate::protocol::surface_reuse::Decoder::default();
            let mut surface = popup_surface("popup");
            surface.popup = None;
            let buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 240, 100));
            surface.frame = FrameData::from_ratatui_buffer(&buffer, None);
            let initial = state.prepare_pane_surface(surface.clone()).unwrap();
            decoder.decode(initial.message().clone()).unwrap();
            state.commit_sent_frame(initial);

            surface.projection_revision += 1;
            let update = state.prepare_pane_surface(surface.clone()).unwrap();
            let mut bytes = Vec::new();
            crate::protocol::write_message(&mut bytes, update.message()).unwrap();
            if enabled {
                assert!(
                    matches!(update.message(), ServerMessage::EndpointControl { kind, .. }
                    if kind == crate::protocol::surface_reuse::MESSAGE_KIND)
                );
                assert!(
                    bytes.len() < 2000,
                    "metadata update was {} bytes",
                    bytes.len()
                );
            } else {
                assert!(matches!(update.message(), ServerMessage::PaneSurface(_)));
                assert!(bytes.len() > 100_000);
            }
            let ServerMessage::PaneSurface(decoded) =
                decoder.decode(update.message().clone()).unwrap()
            else {
                panic!("decoded full surface");
            };
            assert_eq!(decoded.frame, surface.frame);
            assert_eq!(decoded.projection_revision, surface.projection_revision);
            assert_eq!(decoded.surface_revision, 2);
            state.commit_sent_frame(update);

            let mut changed_cell = surface.frame.cells[0].clone();
            changed_cell.symbol = "x".into();
            let patch = state
                .prepare_pane_surface_patch(PaneSurfacePatch {
                    boot_id: surface.boot_id.clone(),
                    projection_revision: surface.projection_revision,
                    base_surface_revision: 2,
                    surface_revision: 0,
                    rows: vec![crate::protocol::PaneSurfacePatchRow {
                        x: 0,
                        y: 0,
                        cells: vec![changed_cell.clone()],
                    }],
                    panes: Vec::new(),
                    cursor: None,
                })
                .unwrap();
            decoder.decode(patch.message().clone()).unwrap();
            state.commit_sent_frame(patch);
            surface.frame.cells[0] = changed_cell;
            surface.projection_revision += 1;
            let update = state.prepare_pane_surface(surface.clone()).unwrap();
            let ServerMessage::PaneSurface(decoded) =
                decoder.decode(update.message().clone()).unwrap()
            else {
                panic!("decoded surface after patch");
            };
            assert_eq!(decoded.frame, surface.frame);
            assert_eq!(decoded.surface_revision, 4);
            state.commit_sent_frame(update);

            // A changed border or terminal cell must still reach the client.
            surface.frame.cells[0].symbol = "y".into();
            let changed = state.prepare_pane_surface(surface.clone()).unwrap();
            assert!(matches!(changed.message(), ServerMessage::PaneSurface(_)));
            let ServerMessage::PaneSurface(decoded) =
                decoder.decode(changed.message().clone()).unwrap()
            else {
                panic!("changed full surface");
            };
            assert_eq!(decoded.frame, surface.frame);
            state.commit_sent_frame(changed);

            state.request_repaint();
            assert!(matches!(
                state.prepare_pane_surface(surface).unwrap().message(),
                ServerMessage::PaneSurface(_)
            ));
        }
    }

    #[test]
    fn surface_reuse_keeps_popup_cells_on_the_binary_codec() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        state.enable_surface_reuse(true);
        let mut surface = popup_surface("popup");
        let buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 400, 100));
        surface.popup.as_mut().unwrap().frame = FrameData::from_ratatui_buffer(&buffer, None);
        let initial = state.prepare_pane_surface(surface.clone()).unwrap();
        state.commit_sent_frame(initial);
        surface.projection_revision += 1;
        let update = state.prepare_pane_surface(surface).unwrap();
        assert!(matches!(update.message(), ServerMessage::PaneSurface(_)));
        let mut bytes = Vec::new();
        crate::protocol::write_message(&mut bytes, update.message()).unwrap();
        assert!(bytes.len() < crate::protocol::MAX_FRAME_SIZE);
    }

    #[test]
    fn surface_reuse_falls_back_when_json_metadata_exceeds_the_frame_limit() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        state.enable_surface_reuse(true);
        let mut surface = popup_surface("popup");
        surface.popup = None;
        surface.frame.hyperlinks = vec!["\"".repeat(crate::protocol::MAX_FRAME_SIZE / 2)];
        let initial = state.prepare_pane_surface(surface.clone()).unwrap();
        state.commit_sent_frame(initial);
        surface.projection_revision += 1;
        let update = state.prepare_pane_surface(surface).unwrap();
        assert!(matches!(update.message(), ServerMessage::PaneSurface(_)));
        let mut bytes = Vec::new();
        crate::protocol::write_message(&mut bytes, update.message()).unwrap();
        assert!(bytes.len() < crate::protocol::MAX_FRAME_SIZE);
    }

    #[test]
    fn popup_only_surface_changes_are_not_deduplicated() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let prepared = state
            .prepare_pane_surface(popup_surface("first"))
            .expect("initial surface");
        state.commit_sent_frame(prepared);

        assert!(state
            .prepare_pane_surface(popup_surface("second"))
            .is_some());
    }

    #[test]
    fn forced_full_surface_keeps_the_connection_revision_monotonic() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let prepared = state
            .prepare_pane_surface(popup_surface("first"))
            .expect("initial surface");
        state.commit_sent_frame(prepared);
        state.request_repaint();

        let prepared = state
            .prepare_pane_surface(popup_surface("replacement"))
            .expect("forced replacement surface");
        assert!(matches!(
            prepared.message(),
            ServerMessage::PaneSurface(surface) if surface.surface_revision == 2
        ));
        state.commit_sent_frame(prepared);
        assert_eq!(state.last_pane_surface().unwrap().surface_revision, 2);
    }

    #[test]
    fn v2_full_render_preparation_emits_sparse_delta_when_baseline_compatible() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let initial = popup_surface("first");
        let prepared = state
            .prepare_pane_surface(initial)
            .expect("initial surface");
        state.commit_sent_frame(prepared);

        let candidate = popup_surface("second");
        let prepared_v2 = state
            .prepare_pane_surface_v2(candidate)
            .expect("prepared v2 delta");
        assert!(matches!(
            prepared_v2.message(),
            ServerMessage::EndpointControl { kind, .. } if kind == crate::protocol::delta::SURFACE_CODEC_DELTA_V2
        ));
        state.commit_sent_frame(prepared_v2);
        let last = state.last_pane_surface().expect("last surface");
        assert_eq!(last.surface_revision, 2);

        let identical_candidate = popup_surface("second");
        assert!(state.prepare_pane_surface_v2(identical_candidate).is_none());

        let mut rebind_candidate = popup_surface("second");
        rebind_candidate.projection_revision = 2;
        assert!(
            state.prepare_pane_surface_v2(rebind_candidate).is_none(),
            "projection-only rebind stays local"
        );
        assert_eq!(state.last_pane_surface().unwrap().projection_revision, 2);
        assert_eq!(state.last_pane_surface().unwrap().surface_revision, 2);

        let mut incompatible = popup_surface("second");
        incompatible.frame.width = 100;
        let prepared_seed = state
            .prepare_pane_surface_v2(incompatible)
            .expect("prepared seed");
        assert!(matches!(
            prepared_seed.message(),
            ServerMessage::PaneSurface(surface) if surface.frame.width == 100
        ));
    }

    #[test]
    fn v2_popup_identity_change_emits_full_pane_surface() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let mut initial = popup_surface("first");
        initial.popup = None;
        let prepared = state
            .prepare_pane_surface(initial)
            .expect("initial surface");
        state.commit_sent_frame(prepared);

        let prepared_open = state
            .prepare_pane_surface_v2(popup_surface("popup-open"))
            .expect("popup identity seed");
        assert!(matches!(
            prepared_open.message(),
            ServerMessage::PaneSurface(surface) if surface.popup.is_some()
        ));
    }

    #[test]
    fn v2_cell_only_surface_emits_frozen_pane_surface_patch() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let prepared = state.prepare_pane_surface(pane_surface("/")).expect("seed");
        state.commit_sent_frame(prepared);

        let prepared_tick = state
            .prepare_pane_surface_v2(pane_surface("-"))
            .expect("cell tick");
        assert!(matches!(
            prepared_tick.message(),
            ServerMessage::PaneSurfacePatch(patch)
                if patch.rows.len() == 1 && patch.panes.is_empty()
        ));
        state.commit_sent_frame(prepared_tick);
        assert_eq!(state.last_pane_surface().unwrap().surface_revision, 2);
    }

    #[test]
    fn v2_cell_only_delta_emits_frozen_pane_surface_patch() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let prepared = state.prepare_pane_surface(pane_surface("/")).expect("seed");
        state.commit_sent_frame(prepared);
        let last = state.last_pane_surface().expect("baseline").clone();
        let delta = crate::protocol::delta::ClientShellSurfaceDelta {
            boot_id: last.boot_id.clone(),
            projection_revision: last.projection_revision,
            base_surface_revision: last.surface_revision,
            surface_revision: 0,
            spans: vec![PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![crate::protocol::CellData {
                    symbol: "-".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                }],
            }],
            row_moves: Vec::new(),
            panes: Vec::new(),
            splits: None,
            cursor: crate::protocol::delta::SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
            graphics: None,
            popup: None,
        };
        let prepared = state
            .prepare_pane_surface_delta(delta)
            .expect("legacy patch");
        assert!(matches!(
            prepared.message(),
            ServerMessage::PaneSurfacePatch(patch) if !patch.rows.is_empty()
        ));
    }

    fn filled_pane_surface(ch: char, width: u16, height: u16) -> PaneSurfaceFrame {
        let line = ch.to_string().repeat(usize::from(width));
        let lines: Vec<String> = (0..height).map(|_| line.clone()).collect();
        let pane = ratatui::buffer::Buffer::with_lines(lines);
        let frame = FrameData::from_ratatui_buffer_with_hyperlinks(&pane, None, &[]);
        let width = frame.width;
        let height = frame.height;
        PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame,
            panes: vec![PaneSurfacePane {
                pane_id: "pane_1".into(),
                content_revision: 1,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                scrollbar_rect: None,
                rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                inner_rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: Vec::new(),
            popup: None,
            graphics: crate::protocol::SurfaceGraphicsScene::default(),
        }
    }

    #[test]
    fn v2_near_full_grid_diff_seeds_full_pane_surface() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let prepared = state
            .prepare_pane_surface(filled_pane_surface('A', 40, 20))
            .expect("seed");
        state.commit_sent_frame(prepared);

        let prepared_full = state
            .prepare_pane_surface_v2(filled_pane_surface('B', 40, 20))
            .expect("near-full rewrite");
        assert!(
            matches!(
                prepared_full.message(),
                ServerMessage::PaneSurface(surface)
                    if surface.frame.width == 40 && surface.frame.height == 20
            ),
            "near-full v2 delta must yield to a seeded PaneSurface, got {:?}",
            prepared_full.message()
        );
    }

    #[test]
    fn v2_near_full_retained_delta_defers_to_full() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let seed = filled_pane_surface('A', 40, 20);
        let prepared = state.prepare_pane_surface(seed.clone()).expect("seed");
        state.commit_sent_frame(prepared);
        let last = state.last_pane_surface().expect("baseline").clone();
        let cell = crate::protocol::CellData {
            symbol: "B".into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        };
        let spans = (0..20u16)
            .map(|y| PaneSurfacePatchRow {
                x: 0,
                y,
                cells: vec![cell.clone(); 40],
            })
            .collect();
        let delta = crate::protocol::delta::ClientShellSurfaceDelta {
            boot_id: last.boot_id.clone(),
            projection_revision: last.projection_revision,
            base_surface_revision: last.surface_revision,
            surface_revision: 0,
            spans,
            row_moves: Vec::new(),
            panes: Vec::new(),
            splits: None,
            cursor: crate::protocol::delta::SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
            graphics: None,
            popup: None,
        };
        assert!(
            state.prepare_pane_surface_delta(delta).is_none(),
            "retained near-full delta must defer a full seed"
        );
    }

    fn image_key() -> crate::protocol::SurfaceGraphicsAssetKey {
        crate::protocol::SurfaceGraphicsAssetKey {
            source: crate::protocol::SurfaceGraphicsSource::Terminal {
                target: crate::protocol::SurfaceGraphicsTarget::Pane {
                    pane_id: "pane_1".into(),
                },
                image_id: 419,
            },
            image_width: 64,
            image_height: 32,
            format: crate::protocol::SurfaceGraphicsFormat::Rgba,
            data_len: 4,
            data_fingerprint: 419,
        }
    }

    fn image_asset() -> crate::protocol::SurfaceGraphicsAsset {
        crate::protocol::SurfaceGraphicsAsset {
            key: image_key(),
            data: vec![255, 0, 0, 255],
        }
    }

    fn image_placement() -> crate::protocol::SurfaceGraphicsPlacement {
        crate::protocol::SurfaceGraphicsPlacement {
            asset: image_key(),
            logical_placement_id: 1,
            x: 0,
            y: 0,
            cols: 8,
            rows: 3,
            source_x: 0,
            source_y: 0,
            source_width: 64,
            source_height: 32,
            x_offset: 0,
            y_offset: 0,
            z: 0,
            scrollback_offset: 0,
        }
    }

    #[test]
    fn v2_retained_graphics_do_not_emit_asset_removals() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let mut initial = popup_surface("first");
        initial.popup = None;
        let prepared = state.prepare_pane_surface(initial).expect("seed");
        state.commit_sent_frame(prepared);

        let mut with_image = popup_surface("first");
        with_image.popup = None;
        with_image.graphics.assets = vec![image_asset()];
        with_image.graphics.placements = vec![image_placement()];
        let prepared_image = state
            .prepare_pane_surface_v2(with_image)
            .expect("image delta");
        let PreparedRender::SurfaceDelta { delta, .. } = &prepared_image else {
            panic!("expected surface delta for first image");
        };
        let gfx = delta.graphics.as_ref().expect("added graphics");
        assert_eq!(gfx.added_assets.len(), 1);
        assert!(gfx.removed_assets.is_empty());
        state.commit_sent_frame(prepared_image);

        let mut retained = popup_surface("first");
        retained.popup = None;
        retained.graphics.assets.clear();
        retained.graphics.placements = vec![image_placement()];
        retained.graphics.retained_assets = vec![image_key()];
        match state.prepare_pane_surface_v2(retained.clone()) {
            None => {}
            Some(prepared_retained) => {
                let PreparedRender::SurfaceDelta { delta, .. } = &prepared_retained else {
                    panic!("retained image must stay a delta, not a seed");
                };
                let gfx = delta.graphics.as_ref();
                if let Some(gfx) = gfx {
                    assert!(
                        gfx.removed_assets.is_empty(),
                        "retained image must not be removed: {gfx:?}"
                    );
                    assert!(gfx.removed_placements.is_empty());
                }
                state.commit_sent_frame(prepared_retained);
            }
        }

        let mut still_retained = popup_surface("first");
        still_retained.popup = None;
        still_retained.graphics.placements = vec![image_placement()];
        still_retained.graphics.retained_assets = vec![image_key()];
        assert!(
            state.prepare_pane_surface_v2(still_retained).is_none(),
            "steady retained graphics must not retransmit"
        );
    }
}
