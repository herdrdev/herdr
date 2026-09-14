//! Optional endpoint encoding that retains unchanged terminal cells across projections.

use super::{CellData, PaneSurfaceFrame, ServerMessage};
use serde::{Deserialize, Serialize};

pub(crate) const CAPABILITY: &str = "surface_reuse";
pub(crate) const MESSAGE_KIND: &str = "endpoint.surface-reuse.v1";

#[derive(Serialize, Deserialize)]
struct SurfaceReuse<S> {
    base_surface_revision: u64,
    surface: S,
}

pub(crate) fn message(
    base_surface_revision: u64,
    surface: &mut PaneSurfaceFrame,
) -> serde_json::Result<Option<ServerMessage>> {
    let cells = std::mem::take(&mut surface.frame.cells);
    let data = serde_json::to_string(&SurfaceReuse {
        base_surface_revision,
        surface: &*surface,
    });
    surface.frame.cells = cells;
    let message = ServerMessage::EndpointControl {
        kind: MESSAGE_KIND.into(),
        data: data?,
    };
    // JSON can expand non-cell data (for example escaped hyperlink URLs). A
    // failed compact encoding must fall back, not strand a newer snapshot.
    let size = bincode::serde::encode_into_std_write(
        &message,
        &mut std::io::sink(),
        bincode::config::standard(),
    );
    match size {
        Ok(size) => Ok((size <= super::MAX_FRAME_SIZE).then_some(message)),
        Err(error) => {
            tracing::warn!(%error, "failed to size surface reuse");
            Ok(None)
        }
    }
}

#[derive(Default)]
struct CellBaseline {
    boot_id: String,
    projection_revision: u64,
    surface_revision: u64,
    width: u16,
    height: u16,
    cells: Vec<CellData>,
}

/// Connection-local decoding happens before activation and presentation filtering, so
/// switching endpoints cannot discard a baseline needed by the next wire message.
#[derive(Default)]
pub(crate) struct Decoder {
    baseline: Option<CellBaseline>,
}

impl Decoder {
    pub(crate) fn decode(&mut self, message: ServerMessage) -> Result<ServerMessage, String> {
        let message = match message {
            ServerMessage::EndpointControl { kind, data } if kind == MESSAGE_KIND => {
                let reuse: SurfaceReuse<PaneSurfaceFrame> = serde_json::from_str(&data)
                    .map_err(|error| format!("invalid surface reuse: {error}"))?;
                let Some(base) = &mut self.baseline else {
                    return Err("surface reuse without a baseline".into());
                };
                let mut surface = reuse.surface;
                if base.boot_id != surface.boot_id
                    || base.surface_revision != reuse.base_surface_revision
                    || surface.surface_revision != base.surface_revision.saturating_add(1)
                    || base.width != surface.frame.width
                    || base.height != surface.frame.height
                    || !surface.frame.cells.is_empty()
                {
                    return Err("surface reuse does not match its baseline".into());
                }
                surface.frame.cells.clone_from(&base.cells);
                base.projection_revision = surface.projection_revision;
                base.surface_revision = surface.surface_revision;
                return Ok(ServerMessage::PaneSurface(surface));
            }
            message => message,
        };
        match &message {
            ServerMessage::PaneSurface(surface) => {
                let base = self.baseline.get_or_insert_with(CellBaseline::default);
                base.boot_id.clone_from(&surface.boot_id);
                base.projection_revision = surface.projection_revision;
                base.surface_revision = surface.surface_revision;
                base.width = surface.frame.width;
                base.height = surface.frame.height;
                base.cells.clone_from(&surface.frame.cells);
            }
            ServerMessage::PaneSurfacePatch(patch) => {
                if let Some(base) = &mut self.baseline {
                    if patch.boot_id != base.boot_id
                        || patch.projection_revision != base.projection_revision
                        || patch.base_surface_revision != base.surface_revision
                        || patch.surface_revision != base.surface_revision.saturating_add(1)
                    {
                        self.baseline = None;
                    } else {
                        for row in &patch.rows {
                            let start =
                                usize::from(row.y) * usize::from(base.width) + usize::from(row.x);
                            let end = start.saturating_add(row.cells.len());
                            if row.y >= base.height
                                || usize::from(row.x) + row.cells.len() > usize::from(base.width)
                                || end > base.cells.len()
                            {
                                return Err("surface patch exceeds the cell baseline".into());
                            }
                            base.cells[start..end].clone_from_slice(&row.cells);
                        }
                        base.surface_revision = patch.surface_revision;
                    }
                }
            }
            _ => {}
        }
        Ok(message)
    }
}
