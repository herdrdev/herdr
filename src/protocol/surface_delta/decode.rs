//! Allocation-bounded decoder for the surface-delta codec.
//!
//! Bincode's byte limit is necessary for strings and pixel payloads, but it is
//! not a logical item limit: nested `Vec`s can otherwise reserve far more
//! memory than their encoded representation occupies. Every collection owned
//! by this codec is therefore decoded only after its count and grid budget have
//! been checked.

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use bincode::{
    de::{read::Reader as _, Decode, Decoder},
    error::DecodeError,
};

use super::{GridUpdate, SurfaceDelta};
use crate::protocol::{
    CellData, ClientShellPopupSurface, CursorState, FrameData, PaneSurfaceFrame, PaneSurfacePane,
    PaneSurfacePatchRow, PaneSurfaceSplit, PaneSurfaceSplitDirection, SurfaceGraphicsAsset,
    SurfaceGraphicsAssetKey, SurfaceGraphicsPlacement, SurfaceGraphicsScene, SurfaceRect,
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE,
};

// These are intentionally practical protocol limits rather than bincode byte
// limits. A sender that exceeds one uses the unchanged full-frame codec.
const MAX_GRID_DIMENSION: u16 = 4096;
const MAX_GRID_CELLS: usize = 1_000_000;
const MAX_PANES: usize = 4096;
const MAX_SPLITS: usize = 4096;
const MAX_SPLIT_PATH: usize = 4096;
const MAX_HYPERLINKS: usize = 65_536;
const MAX_ASSETS: usize = 4096;
const MAX_PLACEMENTS: usize = 65_536;
const MAX_RETAINED_ASSETS: usize = 65_536;

#[derive(Clone, Copy)]
struct DecodeContext {
    expected: Option<(u16, u16)>,
    data_len: usize,
}

struct Decoded(SurfaceDelta<PaneSurfaceFrame>);

pub(super) fn decode(
    data: &str,
    expected: Option<(u16, u16)>,
) -> Result<SurfaceDelta<PaneSurfaceFrame>, String> {
    // EndpointControl's data string itself is subject to the outer graphics
    // frame cap. Checking before base64 decoding also bounds that allocation.
    if data.len() > MAX_GRAPHICS_FRAME_SIZE {
        return Err("surface delta exceeds the frame limit".into());
    }
    let bytes = STANDARD_NO_PAD
        .decode(data)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_GRAPHICS_FRAME_SIZE {
        return Err("decoded surface delta exceeds the frame limit".into());
    }
    decode_bytes_with_data_len(&bytes, expected, data.len())
}

#[cfg(test)]
fn decode_bytes(
    bytes: &[u8],
    expected: Option<(u16, u16)>,
) -> Result<SurfaceDelta<PaneSurfaceFrame>, String> {
    let data_len = base64::encoded_len(bytes.len(), false).unwrap_or(usize::MAX);
    decode_bytes_with_data_len(bytes, expected, data_len)
}

fn decode_bytes_with_data_len(
    bytes: &[u8],
    expected: Option<(u16, u16)>,
    data_len: usize,
) -> Result<SurfaceDelta<PaneSurfaceFrame>, String> {
    let context = DecodeContext { expected, data_len };
    let (Decoded(delta), consumed) = bincode::decode_from_slice_with_context(
        bytes,
        bincode::config::standard().with_limit::<MAX_GRAPHICS_FRAME_SIZE>(),
        context,
    )
    .map_err(|error| error.to_string())?;
    if consumed != bytes.len() {
        return Err("trailing surface delta bytes".into());
    }
    Ok(delta)
}

impl Decode<DecodeContext> for Decoded {
    fn decode<D: Decoder<Context = DecodeContext>>(decoder: &mut D) -> Result<Self, DecodeError> {
        let base_projection_revision = u64::decode(decoder)?;
        let base_surface_revision = u64::decode(decoder)?;
        let (surface, has_graphics) = decode_surface(decoder)?;

        // The outer transport permits the graphics cap so that it can carry
        // EndpointControl. Once metadata tells us this is an ordinary surface,
        // reject it at the ordinary frame cap before decoding either cell grid.
        if !has_graphics && decoder.context().data_len > MAX_FRAME_SIZE {
            return Err(DecodeError::Other(
                "ordinary surface delta exceeds the frame limit",
            ));
        }

        let metadata_dimensions = (surface.frame.width, surface.frame.height);
        let (width, height) = match decoder.context().expected {
            Some(expected) if expected != metadata_dimensions => {
                return Err(DecodeError::Other(
                    "surface dimensions do not match the baseline",
                ));
            }
            Some(expected) => expected,
            None => metadata_dimensions,
        };
        checked_grid_size(width, height)?;
        let rows = decode_rows(decoder, width, height)?;
        let popup_cells = decode_popup_update(decoder, surface.popup.as_deref())?;

        Ok(Self(SurfaceDelta {
            base_projection_revision,
            base_surface_revision,
            surface,
            rows,
            popup_cells,
        }))
    }
}

fn decode_surface<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
) -> Result<(PaneSurfaceFrame, bool), DecodeError> {
    let boot_id = String::decode(decoder)?;
    let projection_revision = u64::decode(decoder)?;
    let surface_revision = u64::decode(decoder)?;
    let frame = decode_frame(decoder)?;
    let panes = decode_bounded_vec::<D, PaneSurfacePane>(decoder, MAX_PANES, "too many panes")?;
    let splits = decode_splits(decoder)?;
    let popup = decode_popup(decoder)?;
    let graphics = decode_graphics_scene(decoder)?;
    let has_graphics = !graphics.assets.is_empty()
        || !graphics.placements.is_empty()
        || !graphics.retained_assets.is_empty();
    Ok((
        PaneSurfaceFrame {
            boot_id,
            projection_revision,
            surface_revision,
            frame,
            panes,
            splits,
            popup,
            graphics,
        },
        has_graphics,
    ))
}

fn decode_frame<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
) -> Result<FrameData, DecodeError> {
    // Delta metadata is encoded after the sender has taken both full cell
    // grids. Reading only the count prevents a forged Vec prefix from causing
    // any cell allocation.
    require_empty_sequence(decoder, "surface metadata contains main cells")?;
    let width = u16::decode(decoder)?;
    let height = u16::decode(decoder)?;
    checked_grid_size(width, height)?;
    let cursor = Option::<CursorState>::decode(decoder)?;
    let hyperlinks =
        decode_bounded_vec::<D, String>(decoder, MAX_HYPERLINKS, "too many hyperlinks")?;
    let graphics = decode_bytes_vec(
        decoder,
        MAX_GRAPHICS_FRAME_SIZE,
        "graphics payload too large",
    )?;
    Ok(FrameData {
        cells: Vec::new(),
        width,
        height,
        cursor,
        hyperlinks,
        graphics,
    })
}

fn decode_popup<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
) -> Result<Option<Box<ClientShellPopupSurface>>, DecodeError> {
    match u8::decode(decoder)? {
        0 => Ok(None),
        1 => {
            let terminal_id = String::decode(decoder)?;
            let title = String::decode(decoder)?;
            let width = Option::decode(decoder)?;
            let height = Option::decode(decoder)?;
            let frame = decode_frame(decoder)?;
            let mouse_reporting = bool::decode(decoder)?;
            let sgr_pixel_mouse = bool::decode(decoder)?;
            let pixel_width = u32::decode(decoder)?;
            let pixel_height = u32::decode(decoder)?;
            Ok(Some(Box::new(ClientShellPopupSurface {
                terminal_id,
                title,
                width,
                height,
                frame,
                mouse_reporting,
                sgr_pixel_mouse,
                pixel_width,
                pixel_height,
            })))
        }
        _ => Err(DecodeError::Other("invalid popup option")),
    }
}

fn decode_splits<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
) -> Result<Vec<PaneSurfaceSplit>, DecodeError> {
    let count = decode_count(decoder, MAX_SPLITS, "too many splits")?;
    let mut splits = Vec::with_capacity(count);
    for _ in 0..count {
        splits.push(PaneSurfaceSplit {
            direction: PaneSurfaceSplitDirection::decode(decoder)?,
            pos: u16::decode(decoder)?,
            area: SurfaceRect::decode(decoder)?,
            hit_rect: SurfaceRect::decode(decoder)?,
            path: decode_bounded_vec::<D, bool>(decoder, MAX_SPLIT_PATH, "split path too long")?,
        });
    }
    Ok(splits)
}

fn decode_graphics_scene<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
) -> Result<SurfaceGraphicsScene, DecodeError> {
    let asset_count = decode_count(decoder, MAX_ASSETS, "too many graphics assets")?;
    let mut assets = Vec::with_capacity(asset_count);
    for _ in 0..asset_count {
        assets.push(SurfaceGraphicsAsset {
            key: SurfaceGraphicsAssetKey::decode(decoder)?,
            data: decode_bytes_vec(
                decoder,
                MAX_GRAPHICS_FRAME_SIZE,
                "graphics asset payload too large",
            )?,
        });
    }
    let placements = decode_bounded_vec::<D, SurfaceGraphicsPlacement>(
        decoder,
        MAX_PLACEMENTS,
        "too many graphics placements",
    )?;
    let retained_assets = decode_bounded_vec::<D, SurfaceGraphicsAssetKey>(
        decoder,
        MAX_RETAINED_ASSETS,
        "too many retained graphics assets",
    )?;
    Ok(SurfaceGraphicsScene {
        assets,
        placements,
        retained_assets,
    })
}

fn decode_popup_update<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
    popup: Option<&ClientShellPopupSurface>,
) -> Result<Option<GridUpdate>, DecodeError> {
    match u8::decode(decoder)? {
        0 => Ok(None),
        1 => {
            let popup = popup.ok_or(DecodeError::Other(
                "popup cell update has no popup metadata",
            ))?;
            let width = popup.frame.width;
            let height = popup.frame.height;
            let update = match u32::decode(decoder)? {
                0 => GridUpdate::Patch(decode_rows(decoder, width, height)?),
                1 => {
                    let expected = checked_grid_size(width, height)?;
                    let count = decode_sequence_len(decoder)?;
                    if count != expected {
                        return Err(DecodeError::Other(
                            "popup replacement does not match popup dimensions",
                        ));
                    }
                    let mut cells = Vec::with_capacity(count);
                    for _ in 0..count {
                        cells.push(CellData::decode(decoder)?);
                    }
                    GridUpdate::Replace(cells)
                }
                _ => return Err(DecodeError::Other("invalid popup grid update")),
            };
            Ok(Some(update))
        }
        _ => Err(DecodeError::Other("invalid popup update option")),
    }
}

fn decode_rows<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
    width: u16,
    height: u16,
) -> Result<Vec<PaneSurfacePatchRow>, DecodeError> {
    let cell_budget = checked_grid_size(width, height)?;
    let count = decode_count(
        decoder,
        super::MAX_SPANS.min(cell_budget),
        "too many surface delta spans",
    )?;
    let row_width = usize::from(width);
    let mut total_cells = 0usize;
    let mut previous_end = 0usize;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let x = u16::decode(decoder)?;
        let y = u16::decode(decoder)?;
        let cells_len = decode_sequence_len(decoder)?;
        let x_usize = usize::from(x);
        let y_usize = usize::from(y);
        if cells_len == 0 || y >= height || x_usize >= row_width || cells_len > row_width - x_usize
        {
            return Err(DecodeError::Other("surface delta span is outside its row"));
        }
        let start = y_usize
            .checked_mul(row_width)
            .and_then(|value| value.checked_add(x_usize))
            .ok_or(DecodeError::Other("surface delta span overflow"))?;
        if start < previous_end {
            return Err(DecodeError::Other(
                "surface delta spans overlap or are not sorted",
            ));
        }
        total_cells = total_cells
            .checked_add(cells_len)
            .ok_or(DecodeError::Other("surface delta cell budget overflow"))?;
        if total_cells > cell_budget {
            return Err(DecodeError::Other("surface delta cell budget exceeded"));
        }
        let mut cells = Vec::with_capacity(cells_len);
        for _ in 0..cells_len {
            cells.push(CellData::decode(decoder)?);
        }
        previous_end = start + cells_len;
        rows.push(PaneSurfacePatchRow { x, y, cells });
    }
    Ok(rows)
}

fn require_empty_sequence<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
    message: &'static str,
) -> Result<(), DecodeError> {
    if decode_sequence_len(decoder)? == 0 {
        Ok(())
    } else {
        Err(DecodeError::Other(message))
    }
}

fn decode_bounded_vec<D, T>(
    decoder: &mut D,
    max: usize,
    message: &'static str,
) -> Result<Vec<T>, DecodeError>
where
    D: Decoder<Context = DecodeContext>,
    T: Decode<DecodeContext>,
{
    let count = decode_count(decoder, max, message)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(T::decode(decoder)?);
    }
    Ok(values)
}

fn decode_bytes_vec<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
    max: usize,
    message: &'static str,
) -> Result<Vec<u8>, DecodeError> {
    let count = decode_count(decoder, max, message)?;
    decoder.claim_container_read::<u8>(count)?;
    let mut bytes = vec![0; count];
    decoder.reader().read(&mut bytes)?;
    Ok(bytes)
}

fn decode_count<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
    max: usize,
    message: &'static str,
) -> Result<usize, DecodeError> {
    let count = decode_sequence_len(decoder)?;
    if count > max {
        Err(DecodeError::Other(message))
    } else {
        Ok(count)
    }
}

fn decode_sequence_len<D: Decoder<Context = DecodeContext>>(
    decoder: &mut D,
) -> Result<usize, DecodeError> {
    let count = u64::decode(decoder)?;
    usize::try_from(count).map_err(|_| DecodeError::OutsideUsizeRange(count))
}

fn checked_grid_size(width: u16, height: u16) -> Result<usize, DecodeError> {
    if width > MAX_GRID_DIMENSION || height > MAX_GRID_DIMENSION {
        return Err(DecodeError::Other("surface dimensions exceed the limit"));
    }
    let cells = usize::from(width) * usize::from(height);
    if cells > MAX_GRID_CELLS {
        Err(DecodeError::Other("surface cell count exceeds the limit"))
    } else {
        Ok(cells)
    }
}

/// Mirrors all decoder-side metadata limits. The sparse sender must fall back
/// to the full-frame codec when this returns false.
pub(super) fn metadata_fits(surface: &PaneSurfaceFrame) -> bool {
    frame_metadata_fits(&surface.frame)
        && surface.panes.len() <= MAX_PANES
        && surface.splits.len() <= MAX_SPLITS
        && surface
            .splits
            .iter()
            .all(|split| split.path.len() <= MAX_SPLIT_PATH)
        && surface
            .popup
            .as_deref()
            .is_none_or(|popup| frame_metadata_fits(&popup.frame))
        && surface.graphics.assets.len() <= MAX_ASSETS
        && surface
            .graphics
            .assets
            .iter()
            .all(|asset| asset.data.len() <= MAX_GRAPHICS_FRAME_SIZE)
        && surface.graphics.placements.len() <= MAX_PLACEMENTS
        && surface.graphics.retained_assets.len() <= MAX_RETAINED_ASSETS
}

fn frame_metadata_fits(frame: &FrameData) -> bool {
    checked_grid_size(frame.width, frame.height).is_ok_and(|cells| frame.cells.len() == cells)
        && frame.hyperlinks.len() <= MAX_HYPERLINKS
        && frame.graphics.len() <= MAX_GRAPHICS_FRAME_SIZE
}

#[cfg(test)]
mod tests {
    use serde::{ser::SerializeSeq as _, Serialize, Serializer};

    use super::*;
    use crate::protocol::{
        ClientShellPopupSize, SurfaceGraphicsFormat, SurfaceGraphicsSource, SurfaceGraphicsTarget,
    };

    fn cell() -> CellData {
        CellData {
            symbol: "x".into(),
            fg: 1,
            bg: 2,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn surface(width: u16, height: u16) -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 2,
            surface_revision: 3,
            frame: FrameData {
                cells: Vec::new(),
                width,
                height,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        }
    }

    fn encoded<T: Serialize>(value: &T) -> Vec<u8> {
        bincode::serde::encode_to_vec(value, bincode::config::standard())
            .expect("encode test value")
    }

    fn graphics_key() -> SurfaceGraphicsAssetKey {
        SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "pane".into(),
                },
                image_id: 1,
            },
            image_width: 1,
            image_height: 1,
            format: SurfaceGraphicsFormat::Rgba,
            data_len: 4,
            data_fingerprint: 1,
        }
    }

    #[test]
    fn native_decoder_matches_the_serde_wire_layout() {
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 2,
            surface: surface(4, 2),
            rows: vec![PaneSurfacePatchRow {
                x: 1,
                y: 1,
                cells: vec![cell()],
            }],
            popup_cells: None::<GridUpdate>,
        };
        let decoded = decode_bytes(&encoded(&delta), Some((4, 2))).expect("decode delta");
        assert!(decoded.surface.frame.cells.is_empty());
        assert_eq!(decoded.rows.len(), 1);
        assert_eq!(decoded.rows[0].cells, vec![cell()]);
    }

    #[test]
    fn ordinary_cap_uses_base64_length_and_the_sender_graphics_predicate() {
        let ordinary = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: surface(1, 1),
            rows: Vec::<PaneSurfacePatchRow>::new(),
            popup_cells: None::<GridUpdate>,
        };
        let bytes = encoded(&ordinary);
        assert!(decode_bytes_with_data_len(&bytes, Some((1, 1)), MAX_FRAME_SIZE + 1).is_err());

        let mut graphics = surface(1, 1);
        graphics.graphics.retained_assets.push(graphics_key());
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: graphics,
            rows: Vec::<PaneSurfacePatchRow>::new(),
            popup_cells: None::<GridUpdate>,
        };
        let bytes = encoded(&delta);
        assert!(decode_bytes_with_data_len(&bytes, Some((1, 1)), MAX_FRAME_SIZE + 1).is_ok());
    }

    #[test]
    fn rejects_nonempty_metadata_grids_before_cell_allocation() {
        let mut main = surface(1, 1);
        main.frame.cells.push(cell());
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: main,
            rows: Vec::<PaneSurfacePatchRow>::new(),
            popup_cells: None::<GridUpdate>,
        };
        assert!(decode_bytes(&encoded(&delta), Some((1, 1))).is_err());

        let mut popup_surface = surface(1, 1);
        popup_surface.popup = Some(Box::new(ClientShellPopupSurface {
            terminal_id: "popup".into(),
            title: "popup".into(),
            width: Some(ClientShellPopupSize::Cells(1)),
            height: Some(ClientShellPopupSize::Cells(1)),
            frame: FrameData {
                cells: vec![cell()],
                width: 1,
                height: 1,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            pixel_width: 1,
            pixel_height: 1,
        }));
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: popup_surface,
            rows: Vec::<PaneSurfacePatchRow>::new(),
            popup_cells: Some(GridUpdate::Replace(vec![cell()])),
        };
        assert!(decode_bytes(&encoded(&delta), Some((1, 1))).is_err());
    }

    struct CountOnly(usize);

    impl Serialize for CountOnly {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_seq(Some(self.0))?.end()
        }
    }

    #[test]
    fn rejects_excessive_span_count_without_allocating_the_claimed_rows() {
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: surface(2, 2),
            rows: CountOnly(super::super::MAX_SPANS + 1),
            popup_cells: None::<GridUpdate>,
        };
        assert!(decode_bytes(&encoded(&delta), Some((2, 2))).is_err());
    }

    #[test]
    fn accepts_max_spans_within_the_grid_cell_budget() {
        let rows = (0..super::super::MAX_SPANS)
            .map(|index| PaneSurfacePatchRow {
                x: (index % 64) as u16,
                y: (index / 64) as u16,
                cells: vec![cell()],
            })
            .collect::<Vec<_>>();
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: surface(64, 64),
            rows,
            popup_cells: None::<GridUpdate>,
        };
        let decoded = decode_bytes(&encoded(&delta), Some((64, 64))).expect("bounded delta");
        assert_eq!(decoded.rows.len(), super::super::MAX_SPANS);
    }

    #[test]
    fn row_bounds_use_the_actual_baseline_and_popup_replace_is_exact() {
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: surface(100, 100),
            rows: vec![PaneSurfacePatchRow {
                x: 2,
                y: 0,
                cells: vec![cell()],
            }],
            popup_cells: None::<GridUpdate>,
        };
        assert!(decode_bytes(&encoded(&delta), Some((2, 2))).is_err());

        let mut popup_surface = surface(1, 1);
        popup_surface.popup = Some(Box::new(ClientShellPopupSurface {
            terminal_id: "popup".into(),
            title: "popup".into(),
            width: None,
            height: None,
            frame: FrameData {
                cells: Vec::new(),
                width: 2,
                height: 1,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            pixel_width: 1,
            pixel_height: 1,
        }));
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: popup_surface,
            rows: Vec::<PaneSurfacePatchRow>::new(),
            popup_cells: Some(GridUpdate::Replace(vec![cell()])),
        };
        assert!(decode_bytes(&encoded(&delta), Some((1, 1))).is_err());
    }

    struct SurfaceWithPanes<'a> {
        source: &'a PaneSurfaceFrame,
        pane_count: usize,
    }

    impl Serialize for SurfaceWithPanes<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeTuple as _;
            let mut tuple = serializer.serialize_tuple(8)?;
            tuple.serialize_element(&self.source.boot_id)?;
            tuple.serialize_element(&self.source.projection_revision)?;
            tuple.serialize_element(&self.source.surface_revision)?;
            tuple.serialize_element(&self.source.frame)?;
            tuple.serialize_element(&CountOnly(self.pane_count))?;
            tuple.serialize_element(&self.source.splits)?;
            tuple.serialize_element(&self.source.popup)?;
            tuple.serialize_element(&self.source.graphics)?;
            tuple.end()
        }
    }

    #[test]
    fn rejects_excessive_metadata_count_without_allocating_the_claimed_items() {
        let source = surface(2, 2);
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: SurfaceWithPanes {
                source: &source,
                pane_count: MAX_PANES + 1,
            },
            rows: Vec::<PaneSurfacePatchRow>::new(),
            popup_cells: None::<GridUpdate>,
        };
        assert!(decode_bytes(&encoded(&delta), Some((2, 2))).is_err());
    }

    struct OversizedAsset<'a>(&'a SurfaceGraphicsAssetKey);

    impl Serialize for OversizedAsset<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeTuple as _;
            let mut tuple = serializer.serialize_tuple(2)?;
            tuple.serialize_element(self.0)?;
            tuple.serialize_element(&CountOnly(MAX_GRAPHICS_FRAME_SIZE + 1))?;
            tuple.end()
        }
    }

    struct SceneWithOversizedAsset<'a>(&'a SurfaceGraphicsAssetKey);

    impl Serialize for SceneWithOversizedAsset<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::{SerializeSeq as _, SerializeTuple as _};
            let mut tuple = serializer.serialize_tuple(3)?;
            struct Assets<'a>(&'a SurfaceGraphicsAssetKey);
            impl Serialize for Assets<'_> {
                fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                    let mut sequence = serializer.serialize_seq(Some(1))?;
                    sequence.serialize_element(&OversizedAsset(self.0))?;
                    sequence.end()
                }
            }
            tuple.serialize_element(&Assets(self.0))?;
            tuple.serialize_element(&Vec::<SurfaceGraphicsPlacement>::new())?;
            tuple.serialize_element(&Vec::<SurfaceGraphicsAssetKey>::new())?;
            tuple.end()
        }
    }

    struct SurfaceWithGraphics<'a> {
        source: &'a PaneSurfaceFrame,
        key: &'a SurfaceGraphicsAssetKey,
    }

    impl Serialize for SurfaceWithGraphics<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeTuple as _;
            let mut tuple = serializer.serialize_tuple(8)?;
            tuple.serialize_element(&self.source.boot_id)?;
            tuple.serialize_element(&self.source.projection_revision)?;
            tuple.serialize_element(&self.source.surface_revision)?;
            tuple.serialize_element(&self.source.frame)?;
            tuple.serialize_element(&self.source.panes)?;
            tuple.serialize_element(&self.source.splits)?;
            tuple.serialize_element(&self.source.popup)?;
            tuple.serialize_element(&SceneWithOversizedAsset(self.key))?;
            tuple.end()
        }
    }

    #[test]
    fn rejects_oversized_pixel_payload_prefix_before_allocating_it() {
        let source = surface(2, 2);
        let key = graphics_key();
        let delta = SurfaceDelta {
            base_projection_revision: 1,
            base_surface_revision: 1,
            surface: SurfaceWithGraphics {
                source: &source,
                key: &key,
            },
            rows: Vec::<PaneSurfacePatchRow>::new(),
            popup_cells: None::<GridUpdate>,
        };
        assert!(decode_bytes(&encoded(&delta), Some((2, 2))).is_err());
    }

    #[test]
    fn sender_eligibility_matches_grid_and_metadata_limits() {
        let mut exact = surface(2, 2);
        exact.frame.cells = vec![cell(); 4];
        assert!(metadata_fits(&exact));
        exact.frame.cells.pop();
        assert!(!metadata_fits(&exact));

        let mut candidate = surface(MAX_GRID_DIMENSION + 1, 1);
        assert!(!metadata_fits(&candidate));
        candidate.frame.width = 1;
        candidate.frame.cells = vec![cell()];
        candidate.frame.hyperlinks = vec![String::new(); MAX_HYPERLINKS + 1];
        assert!(!metadata_fits(&candidate));
    }
}
