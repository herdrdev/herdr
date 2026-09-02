use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use ratatui::layout::Rect;

use crate::app::state::AppState;
use crate::ghostty::{
    KittyImageDescriptor, KittyImageFormat, KittyImagePlacement, KittyPlacementRenderInfo,
};
use crate::layout::{PaneId, PaneInfo};
use crate::terminal::TerminalRuntimeRegistry;

pub(crate) mod surface;

const KITTY_CHUNK_BYTES: usize = 3072;
const MAX_OVERSIZED_SOURCES: usize = 256;
pub(crate) const HEADLESS_GRAPHICS_TRANSACTION_BUDGET: usize =
    crate::protocol::MAX_GRAPHICS_FRAME_SIZE - crate::protocol::MAX_FRAME_SIZE;
const HOST_IMAGE_ID_BASE: u32 = 10_000;
#[cfg(test)]
const PANE_GRAPHICS_IMAGE_ID_BIT: u32 = 1 << 31;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HostCellSize {
    pub width_px: u32,
    pub height_px: u32,
}

impl HostCellSize {
    pub(crate) fn is_known(self) -> bool {
        self.width_px > 0 && self.height_px > 0
    }
}

#[derive(Debug)]
struct HostPlacement {
    pane_id: PaneId,
    host_image_id: Option<u32>,
    area: Rect,
    cell_size: HostCellSize,
    source_key: HostSourceKey,
    placement: KittyImagePlacement,
    scrollback_offset: u32,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum HostSourceKey {
    Terminal {
        pane_id: PaneId,
        image_id: u32,
    },
    PaneLayer {
        pane_id: PaneId,
        layer_id: String,
    },
    ClientSurface {
        scope: String,
        source: crate::protocol::SurfaceGraphicsSource,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct ImageSignature {
    image_width: u32,
    image_height: u32,
    format_code: u32,
    data_len: usize,
    data_fingerprint: u64,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct PlacementSignature {
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
    z: i32,
    scrollback_offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClippedPlacement {
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct HostGraphicsCache {
    images: HashMap<u32, ImageSignature>,
    placements: HashMap<(u32, u32), PlacementSignature>,
    /// Host image currently backing each (pane, source image id) pair.
    sources: HashMap<HostSourceKey, u32>,
    oversized: HashMap<HostSourceKey, ImageSignature>,
    continuation: Option<(HostSourceKey, u32, usize)>,
    replay_placements: bool,
    replayed_placements: HashSet<(u32, u32)>,
}

static KITTY_GRAPHICS_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_enabled(enabled: bool) {
    KITTY_GRAPHICS_ENABLED.store(enabled, Ordering::Release);
}

pub(crate) fn is_enabled() -> bool {
    KITTY_GRAPHICS_ENABLED.load(Ordering::Acquire)
}

pub(crate) struct EncodedGraphics {
    pub(crate) bytes: Vec<u8>,
    pub(crate) incomplete: bool,
}

/// Whether appending `additional` bytes to the `current_len` bytes already
/// assembled keeps the transaction inside the caller's budget. Without a
/// budget the incremental path intentionally stays one transaction per call.
fn coalesced_transaction_fits(
    current_len: usize,
    additional: usize,
    transaction_budget: Option<usize>,
) -> bool {
    let Some(budget) = transaction_budget else {
        return false;
    };
    current_len.saturating_add(additional) <= budget
}

fn image_transaction_fits(placement: &HostPlacement, budget: Option<usize>) -> bool {
    let Some(budget) = budget else {
        return true;
    };
    image_transfer_estimated_size(placement.placement.data_len) <= budget
}

pub(crate) fn image_transfer_estimated_size(data_len: usize) -> usize {
    let encoded = data_len.div_ceil(3).saturating_mul(4);
    let command_overhead = data_len.div_ceil(KITTY_CHUNK_BYTES).saturating_mul(16) + 1024;
    encoded.saturating_add(command_overhead)
}

fn placement_identity(placement: &HostPlacement) -> (HostSourceKey, u32) {
    (
        placement.source_key.clone(),
        host_placement_id(&placement.source_key, &placement.placement),
    )
}

fn source_order(source: &HostSourceKey) -> (u32, String) {
    match source {
        HostSourceKey::Terminal { pane_id, .. } => (pane_id.raw(), String::new()),
        HostSourceKey::PaneLayer { pane_id, layer_id } => (pane_id.raw(), layer_id.clone()),
        HostSourceKey::ClientSurface { scope, source } => {
            let mut hasher = DefaultHasher::new();
            scope.hash(&mut hasher);
            source.hash(&mut hasher);
            (hasher.finish() as u32, format!("{source:?}"))
        }
    }
}

fn encode_placement_update(
    cache: &mut HostGraphicsCache,
    placement: &HostPlacement,
) -> Option<Vec<u8>> {
    let (clipped, format_code) = clipped_placement(placement)?;
    let host_id = placement
        .host_image_id
        .unwrap_or_else(|| host_image_id(placement.pane_id, &placement.placement));
    let placement_id = host_placement_id(&placement.source_key, &placement.placement);
    let key = (host_id, placement_id);
    let image_signature = image_signature(placement, format_code);
    let placement_signature =
        placement_signature(clipped, placement.placement.z, placement.scrollback_offset);
    let image_current = cache.images.get(&host_id) == Some(&image_signature);
    let placement_current = cache.placements.get(&key) == Some(&placement_signature)
        && (!cache.replay_placements || cache.replayed_placements.contains(&key));
    if image_current
        && placement_current
        && cache.sources.get(&placement.source_key) == Some(&host_id)
    {
        return None;
    }

    let mut bytes = Vec::new();
    let mut displayed = false;
    if !image_current {
        if cache.images.contains_key(&host_id)
            && matches!(placement.source_key, HostSourceKey::PaneLayer { .. })
        {
            if !encode_transmit_and_display(
                &mut bytes,
                placement,
                clipped,
                format_code,
                host_id,
                placement_id,
            ) {
                return None;
            }
            displayed = true;
        } else {
            if cache.images.contains_key(&host_id) {
                encode_delete_image(&mut bytes, host_id);
                cache.placements.retain(|(id, _), _| *id != host_id);
                cache.replayed_placements.retain(|(id, _)| *id != host_id);
            }
            if !encode_upload_image(&mut bytes, placement, format_code, host_id) {
                return None;
            }
        }
        cache.images.insert(host_id, image_signature);
    }

    release_superseded_source_image(&mut bytes, cache, placement.source_key.clone(), host_id);
    if !displayed && !placement_current {
        encode_display_placement(
            &mut bytes,
            clipped,
            host_id,
            placement_id,
            placement.placement.z,
        );
    }
    cache.placements.insert(key, placement_signature);
    if cache.replay_placements {
        cache.replayed_placements.insert(key);
    }
    Some(bytes)
}

fn release_superseded_source_image(
    bytes: &mut Vec<u8>,
    cache: &mut HostGraphicsCache,
    source: HostSourceKey,
    host_id: u32,
) {
    let Some(previous) = cache.sources.insert(source, host_id) else {
        return;
    };
    if previous == host_id || cache.sources.values().any(|id| *id == previous) {
        return;
    }
    encode_delete_image(bytes, previous);
    cache.images.remove(&previous);
    cache.placements.retain(|(id, _), _| *id != previous);
    cache.replayed_placements.retain(|(id, _)| *id != previous);
}

fn encode_graphics_update_incremental(
    cache: &mut HostGraphicsCache,
    placements: &[HostPlacement],
    live_pane_sources: &HashSet<HostSourceKey>,
    transaction_budget: Option<usize>,
    coalesce_placements: bool,
) -> EncodedGraphics {
    let desired_sources = placements
        .iter()
        .map(|placement| placement.source_key.clone())
        .collect::<HashSet<_>>();
    let desired_placements = placements
        .iter()
        .filter_map(|placement| {
            clipped_placement(placement).map(|_| {
                let host_id = placement
                    .host_image_id
                    .unwrap_or_else(|| host_image_id(placement.pane_id, &placement.placement));
                (
                    host_id,
                    host_placement_id(&placement.source_key, &placement.placement),
                )
            })
        })
        .collect::<HashSet<_>>();
    let start = cache
        .continuation
        .as_ref()
        .and_then(|(source, id, _)| {
            placements
                .iter()
                .position(|placement| placement_identity(placement) == (source.clone(), *id))
        })
        .map(|index| index + 1)
        .or_else(|| cache.continuation.as_ref().map(|cursor| cursor.2))
        .map_or(0, |index| index % placements.len().max(1));
    let mut bytes = Vec::new();
    let mut emitted = false;

    let mut dead_sources = cache
        .sources
        .keys()
        .filter(|source| {
            matches!(source, HostSourceKey::PaneLayer { .. })
                && !live_pane_sources.contains(*source)
        })
        .cloned()
        .collect::<Vec<_>>();
    dead_sources.sort_by_key(source_order);
    for source in dead_sources {
        let host_id = cache.sources[&source];
        let last_reference = !cache
            .sources
            .iter()
            .any(|(other, id)| *other != source && *id == host_id);
        if emitted && last_reference {
            return EncodedGraphics {
                bytes,
                incomplete: true,
            };
        }
        cache.sources.remove(&source);
        if last_reference {
            encode_delete_image(&mut bytes, host_id);
            cache.images.remove(&host_id);
            cache.placements.retain(|(id, _), _| *id != host_id);
            cache.replayed_placements.retain(|(id, _)| *id != host_id);
            emitted = true;
        }
    }
    cache.sources.retain(|source, _| {
        matches!(source, HostSourceKey::PaneLayer { .. }) || desired_sources.contains(source)
    });
    cache.oversized.retain(|source, _| {
        matches!(source, HostSourceKey::Terminal { .. })
            || live_pane_sources.contains(source)
            || desired_sources.contains(source)
    });

    let mut stale = cache
        .placements
        .keys()
        .filter(|key| !desired_placements.contains(key))
        .copied()
        .collect::<Vec<_>>();
    stale.sort_unstable();
    let mut stale_image = None;
    for key @ (host_id, placement_id) in stale {
        let mut transaction = Vec::new();
        encode_delete_placement(&mut transaction, host_id, placement_id);
        let same_image = stale_image == Some(host_id);
        if emitted
            && !(coalesce_placements
                && same_image
                && coalesced_transaction_fits(bytes.len(), transaction.len(), transaction_budget))
        {
            return EncodedGraphics {
                bytes,
                incomplete: true,
            };
        }
        bytes.extend(transaction);
        cache.placements.remove(&key);
        cache.replayed_placements.remove(&key);
        emitted = true;
        stale_image = Some(host_id);
    }

    // Keep unrelated images isolated, but treat every row of one logical image
    // as part of its upload or replacement transaction. Sending only the first
    // row exposes the blank placeholder cells until later frames catch up.
    let coalesce_pass = coalesce_placements && !emitted;
    let mut coalesce_target = None;
    for offset in 0..placements.len() {
        let index = (start + offset) % placements.len();
        let placement = &placements[index];
        let signature = image_signature(placement, kitty_format_code(placement.placement.format));
        if transaction_budget.is_some()
            && cache.oversized.get(&placement.source_key) == Some(&signature)
        {
            continue;
        }
        cache.oversized.remove(&placement.source_key);
        let host_id = placement
            .host_image_id
            .unwrap_or_else(|| host_image_id(placement.pane_id, &placement.placement));
        let image_cached = cache.images.get(&host_id) == Some(&signature);
        // With the image uploaded and the source already bound to it, the
        // transaction is a re-display only: no upload and no superseded-image
        // delete from `release_superseded_source_image`.
        let pure_redisplay =
            image_cached && cache.sources.get(&placement.source_key) == Some(&host_id);
        if !image_cached && !image_transaction_fits(placement, transaction_budget) {
            cache.quarantine_oversized(placement.source_key.clone(), signature);
            continue;
        }
        let mut candidate = cache.clone();
        let Some(transaction) = encode_placement_update(&mut candidate, placement) else {
            continue;
        };
        if transaction.is_empty() {
            *cache = candidate;
            continue;
        }
        let same_logical_image = coalesce_target.as_ref().is_none_or(|(source, target_id)| {
            source == &placement.source_key && *target_id == host_id
        });
        if emitted
            && !(coalesce_pass
                && pure_redisplay
                && same_logical_image
                && coalesced_transaction_fits(bytes.len(), transaction.len(), transaction_budget))
        {
            return EncodedGraphics {
                bytes,
                incomplete: true,
            };
        }
        *cache = candidate;
        let (source, id) = placement_identity(placement);
        cache.continuation = Some((source, id, (index + 1) % placements.len()));
        bytes.extend(transaction);
        emitted = true;
        if coalesce_pass && !pure_redisplay {
            coalesce_target = Some((placement.source_key.clone(), host_id));
        }
    }

    cache.replay_placements = false;
    cache.replayed_placements.clear();
    EncodedGraphics {
        bytes,
        incomplete: false,
    }
}

#[cfg(test)]
fn drain_graphics_updates(
    cache: &mut HostGraphicsCache,
    placements: &[HostPlacement],
    live: &HashSet<HostSourceKey>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let encoded = encode_graphics_update_incremental(cache, placements, live, None, false);
        bytes.extend(encoded.bytes);
        if !encoded.incomplete {
            return bytes;
        }
    }
}

impl HostGraphicsCache {
    fn reset_incremental_progress(&mut self) {
        self.continuation = None;
        self.replay_placements = false;
        self.replayed_placements.clear();
    }

    fn quarantine_oversized(&mut self, source: HostSourceKey, signature: ImageSignature) {
        if !self.oversized.contains_key(&source) && self.oversized.len() >= MAX_OVERSIZED_SOURCES {
            if let Some(evicted) = self.oversized.keys().next().cloned() {
                self.oversized.remove(&evicted);
            }
        }
        self.oversized.insert(source, signature);
    }

    pub(crate) fn request_placement_replay(&mut self) {
        if !self.replay_placements {
            self.replay_placements = true;
            self.replayed_placements.clear();
        }
    }

    pub(crate) fn clear_bytes(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for id in self.images.keys().copied().collect::<Vec<_>>() {
            encode_delete_image(&mut bytes, id);
        }
        self.images.clear();
        self.placements.clear();
        self.sources.clear();
        self.oversized.clear();
        self.reset_incremental_progress();
        bytes
    }
}

fn collect_visible_placements(
    app: &AppState,
    graphics: &crate::app::pane_graphics::Runtime,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
    uploaded_images: &HashMap<u32, ImageSignature>,
    oversized_images: &HashMap<HostSourceKey, ImageSignature>,
) -> Vec<HostPlacement> {
    let Some(target) = surface.target else {
        tracing::debug!("collect_visible_placements: no tab surface target");
        return Vec::new();
    };
    let ws_idx = target.workspace_index;
    if app
        .workspaces
        .get(ws_idx)
        .and_then(|workspace| workspace.tabs.get(target.tab_index))
        .is_none()
    {
        tracing::debug!(
            ws_idx,
            tab_idx = target.tab_index,
            "collect_visible_placements: no target tab"
        );
        return Vec::new();
    }

    tracing::debug!(
        ws_idx,
        terminal_runtimes_len = terminal_runtimes.len(),
        pane_infos_len = surface.pane_infos.len(),
        "collect_visible_placements: starting iteration"
    );
    let mut placements = Vec::new();
    for info in surface.pane_infos {
        let mut pane_layers = graphics
            .slots
            .iter()
            .filter_map(|((pane_id, layer_id), slot)| {
                (*pane_id == info.id)
                    .then(|| {
                        slot.layer
                            .as_ref()
                            .map(|layer| (layer_id, slot.host_image_id, layer))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        pane_layers.sort_by_key(|(layer_id, _, layer)| (layer.z_index, layer_id.as_str()));
        for (layer_id, host_image_id, layer) in pane_layers {
            placements.push(pane_graphics_host_placement(
                info,
                layer_id,
                host_image_id,
                cell_size,
                layer,
                uploaded_images,
                true,
            ));
        }

        let runtime = match app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            Some(rt) => rt,
            None => {
                tracing::debug!(pane_id = ?info.id, "collect_visible_placements: runtime not found");
                continue;
            }
        };
        let mut requested_images = HashSet::new();
        for placement in runtime.kitty_image_placements_with_data_filter(|descriptor| {
            terminal_image_needs_data(
                info.id,
                descriptor,
                uploaded_images,
                oversized_images,
                &mut requested_images,
            )
        }) {
            let scrollback_offset = runtime
                .scroll_metrics()
                .map(|m| m.offset_from_bottom as u32)
                .unwrap_or(0);
            placements.push(HostPlacement {
                pane_id: info.id,
                host_image_id: None,
                area: info.inner_rect,
                cell_size,
                source_key: HostSourceKey::Terminal {
                    pane_id: info.id,
                    image_id: placement.image_id,
                },
                placement,
                scrollback_offset,
            });
        }
    }
    tracing::debug!(
        placements_len = placements.len(),
        "collect_visible_placements: done"
    );
    placements
}

fn terminal_image_needs_data(
    pane_id: PaneId,
    descriptor: KittyImageDescriptor,
    uploaded_images: &HashMap<u32, ImageSignature>,
    oversized_images: &HashMap<HostSourceKey, ImageSignature>,
    requested_images: &mut HashSet<(HostSourceKey, ImageSignature)>,
) -> bool {
    let format_code = kitty_format_code(descriptor.format);
    let signature = image_signature_from_descriptor(descriptor, format_code);
    let host_id = host_image_id_for_signature(pane_id, signature);
    let source = HostSourceKey::Terminal {
        pane_id,
        image_id: descriptor.image_id,
    };
    uploaded_images.get(&host_id).copied() != Some(signature)
        && oversized_images.get(&source).copied() != Some(signature)
        && requested_images.insert((source, signature))
}

fn pane_graphics_host_placement(
    info: &PaneInfo,
    layer_id: &str,
    host_id: u32,
    cell_size: HostCellSize,
    layer: &crate::app::pane_graphics::Layer,
    uploaded_images: &HashMap<u32, ImageSignature>,
    include_data: bool,
) -> HostPlacement {
    let format = pane_graphics_kitty_format(layer.format);
    let signature = pane_layer_image_signature(layer);
    let data = if !include_data || uploaded_images.get(&host_id).copied() == Some(signature) {
        Vec::new()
    } else {
        layer.inline_data().map(<[u8]>::to_vec).unwrap_or_default()
    };
    let render = layer.render;
    let grid_cols = if render.grid_cols == 0 {
        u32::from(info.inner_rect.width)
    } else {
        render.grid_cols
    };
    let grid_rows = if render.grid_rows == 0 {
        u32::from(info.inner_rect.height)
    } else {
        render.grid_rows
    };

    HostPlacement {
        pane_id: info.id,
        host_image_id: Some(host_id),
        area: info.inner_rect,
        cell_size,
        source_key: HostSourceKey::PaneLayer {
            pane_id: info.id,
            layer_id: layer_id.to_owned(),
        },
        scrollback_offset: 0,
        placement: KittyImagePlacement {
            image_id: 1,
            placement_id: 1,
            z: layer.z_index,
            x_offset: 0,
            y_offset: 0,
            image_width: layer.image_width,
            image_height: layer.image_height,
            format,
            data_len: layer.data_len(),
            data_fingerprint: layer.data_fingerprint,
            data,
            render: KittyPlacementRenderInfo {
                pixel_width: layer.image_width,
                pixel_height: layer.image_height,
                grid_cols,
                grid_rows,
                viewport_col: render.viewport_col,
                viewport_row: render.viewport_row,
                source_x: 0,
                source_y: 0,
                source_width: 0,
                source_height: 0,
            },
        },
    }
}

fn pane_graphics_kitty_format(format: crate::api::schema::PaneGraphicsFormat) -> KittyImageFormat {
    match format {
        crate::api::schema::PaneGraphicsFormat::Png => KittyImageFormat::Png,
        crate::api::schema::PaneGraphicsFormat::Rgb => KittyImageFormat::Rgb,
        crate::api::schema::PaneGraphicsFormat::Rgba
        | crate::api::schema::PaneGraphicsFormat::Bgra => KittyImageFormat::Rgba,
    }
}

fn host_image_id(pane_id: PaneId, placement: &KittyImagePlacement) -> u32 {
    let format_code = kitty_format_code(placement.format);
    host_image_id_for_signature(
        pane_id,
        ImageSignature {
            image_width: placement.image_width,
            image_height: placement.image_height,
            format_code,
            data_len: placement.data_len,
            data_fingerprint: placement.data_fingerprint,
        },
    )
}

fn host_image_id_for_signature(pane_id: PaneId, signature: ImageSignature) -> u32 {
    let mut hasher = DefaultHasher::new();
    pane_id.raw().hash(&mut hasher);
    signature.hash(&mut hasher);
    HOST_IMAGE_ID_BASE + ((hasher.finish() as u32) % 900_000)
}

fn host_placement_id(source_key: &HostSourceKey, placement: &KittyImagePlacement) -> u32 {
    let mut hasher = DefaultHasher::new();
    match source_key {
        HostSourceKey::Terminal { pane_id, .. } => pane_id.raw().hash(&mut hasher),
        HostSourceKey::PaneLayer { pane_id, layer_id } => {
            "pane.graphics".hash(&mut hasher);
            pane_id.raw().hash(&mut hasher);
            layer_id.hash(&mut hasher);
        }
        HostSourceKey::ClientSurface { scope, source } => {
            "client.surface".hash(&mut hasher);
            scope.hash(&mut hasher);
            source.hash(&mut hasher);
        }
    }
    placement.image_id.hash(&mut hasher);
    placement.placement_id.hash(&mut hasher);
    1 + ((hasher.finish() as u32) % 900_000)
}

pub(crate) struct DirectFileCommand {
    pub(crate) leading: Vec<u8>,
    pub(crate) control: String,
}

#[cfg(unix)]
pub(crate) fn encode_kitty_regular_file(
    out: &mut Vec<u8>,
    leading: &[u8],
    control: &str,
    path: &str,
) {
    let payload = base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
    out.extend_from_slice(b"\x1b7");
    out.extend_from_slice(leading);
    let _ = write!(out, "\x1b_G{control},t=f;{payload}\x1b\\");
    out.extend_from_slice(b"\x1b8");
}

fn encode_delete_image(out: &mut Vec<u8>, id: u32) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\");
}

fn encode_delete_placement(out: &mut Vec<u8>, host_id: u32, host_placement_id: u32) {
    let _ = write!(
        out,
        "\x1b_Ga=d,d=i,i={host_id},p={host_placement_id},q=2;\x1b\\"
    );
}

fn encode_upload_image(
    out: &mut Vec<u8>,
    placement: &HostPlacement,
    format_code: u32,
    host_id: u32,
) -> bool {
    if placement.placement.data.is_empty() {
        return false;
    }

    let control = format!(
        "a=t,t=d,f={format_code},s={},v={},i={host_id},q=2",
        placement.placement.image_width, placement.placement.image_height,
    );
    encode_kitty_data(out, &control, &placement.placement.data);
    true
}

fn encode_transmit_and_display(
    out: &mut Vec<u8>,
    placement: &HostPlacement,
    clipped: ClippedPlacement,
    format_code: u32,
    host_id: u32,
    host_placement_id: u32,
) -> bool {
    if placement.placement.data.is_empty() {
        return false;
    }
    let _ = write!(out, "\x1b[{};{}H", clipped.y + 1, clipped.x + 1);
    let mut control = format!(
        "a=T,t=d,f={format_code},s={},v={},i={host_id},p={host_placement_id},c={},r={},z={},C=1,q=2",
        placement.placement.image_width,
        placement.placement.image_height,
        clipped.cols,
        clipped.rows,
        placement.placement.z,
    );
    append_placement_controls(&mut control, clipped);
    encode_kitty_data(out, &control, &placement.placement.data);
    true
}

fn encode_display_placement(
    out: &mut Vec<u8>,
    clipped: ClippedPlacement,
    host_id: u32,
    host_placement_id: u32,
    z: i32,
) {
    let _ = write!(out, "\x1b[{};{}H", clipped.y + 1, clipped.x + 1);
    let mut control = format!(
        "a=p,i={host_id},p={host_placement_id},c={},r={},z={z},C=1,q=2",
        clipped.cols, clipped.rows,
    );
    append_placement_controls(&mut control, clipped);
    let _ = write!(out, "\x1b_G{control};\x1b\\");
}

fn append_placement_controls(control: &mut String, clipped: ClippedPlacement) {
    if clipped.source_x > 0 {
        let _ = write!(control, ",x={}", clipped.source_x);
    }
    if clipped.source_y > 0 {
        let _ = write!(control, ",y={}", clipped.source_y);
    }
    if clipped.source_width > 0 {
        let _ = write!(control, ",w={}", clipped.source_width);
    }
    if clipped.source_height > 0 {
        let _ = write!(control, ",h={}", clipped.source_height);
    }
    if clipped.x_offset > 0 {
        let _ = write!(control, ",X={}", clipped.x_offset);
    }
    if clipped.y_offset > 0 {
        let _ = write!(control, ",Y={}", clipped.y_offset);
    }
}

fn clipped_placement(placement: &HostPlacement) -> Option<(ClippedPlacement, u32)> {
    if placement.area.width == 0 || placement.area.height == 0 {
        tracing::debug!(
            area_w = placement.area.width,
            area_h = placement.area.height,
            "clipped_placement: area zero"
        );
        return None;
    }
    let render = placement.placement.render;
    if render.grid_cols == 0 || render.grid_rows == 0 {
        tracing::debug!(
            grid_cols = render.grid_cols,
            grid_rows = render.grid_rows,
            "clipped_placement: grid zero"
        );
        return None;
    }
    let format_code = kitty_format_code(placement.placement.format);

    let left_clip_cells = if render.viewport_col < 0 {
        render.viewport_col.saturating_neg() as u32
    } else {
        0
    };
    let top_clip_cells = if render.viewport_row < 0 {
        render.viewport_row.saturating_neg() as u32
    } else {
        0
    };
    let viewport_col = render.viewport_col.max(0) as u32;
    let viewport_row = render.viewport_row.max(0) as u32;
    tracing::debug!(
        viewport_col = viewport_col,
        viewport_row = viewport_row,
        area_w = placement.area.width,
        area_h = placement.area.height,
        scrollback_offset = placement.scrollback_offset,
        raw_viewport_row = render.viewport_row,
        cond1 = viewport_col >= placement.area.width as u32,
        cond2 = viewport_row >= placement.area.height as u32,
        "clipped_placement: viewport check"
    );
    if viewport_col >= placement.area.width as u32 || viewport_row >= placement.area.height as u32 {
        return None;
    }

    let visible_cols = render
        .grid_cols
        .saturating_sub(left_clip_cells)
        .min(placement.area.width as u32 - viewport_col);
    let visible_rows = render
        .grid_rows
        .saturating_sub(top_clip_cells)
        .min(placement.area.height as u32 - viewport_row);
    tracing::debug!(
        visible_cols = visible_cols,
        visible_rows = visible_rows,
        left_clip_cells = left_clip_cells,
        top_clip_cells = top_clip_cells,
        "clipped_placement: visible dims check"
    );
    if visible_cols == 0 || visible_rows == 0 {
        return None;
    }

    let source_width = if render.source_width == 0 {
        placement.placement.image_width
    } else {
        render.source_width
    };
    let source_height = if render.source_height == 0 {
        placement.placement.image_height
    } else {
        render.source_height
    };
    let pixel_width = render
        .pixel_width
        .max(
            render
                .grid_cols
                .saturating_mul(placement.cell_size.width_px),
        )
        .max(1);
    let pixel_height = render
        .pixel_height
        .max(
            render
                .grid_rows
                .saturating_mul(placement.cell_size.height_px),
        )
        .max(1);

    let crop_left_px = left_clip_cells.saturating_mul(placement.cell_size.width_px);
    let crop_top_px = top_clip_cells.saturating_mul(placement.cell_size.height_px);
    let visible_width_px = visible_cols.saturating_mul(placement.cell_size.width_px);
    let visible_height_px = visible_rows.saturating_mul(placement.cell_size.height_px);

    let source_x = render.source_x + scale_pixels(crop_left_px, source_width, pixel_width);
    let source_y = render.source_y + scale_pixels(crop_top_px, source_height, pixel_height);
    let source_width = scale_pixels(visible_width_px, source_width, pixel_width)
        .max(1)
        .min(placement.placement.image_width.saturating_sub(source_x));
    let source_height = scale_pixels(visible_height_px, source_height, pixel_height)
        .max(1)
        .min(placement.placement.image_height.saturating_sub(source_y));

    if source_width == 0 || source_height == 0 {
        tracing::debug!(
            source_width = source_width,
            source_height = source_height,
            image_width = placement.placement.image_width,
            image_height = placement.placement.image_height,
            "clipped_placement: source dims zero"
        );
        return None;
    }

    tracing::debug!("clipped_placement: success");
    Some((
        ClippedPlacement {
            x: placement.area.x + viewport_col as u16,
            y: placement.area.y + viewport_row as u16,
            cols: visible_cols,
            rows: visible_rows,
            source_x,
            source_y,
            source_width,
            source_height,
            x_offset: if left_clip_cells == 0 {
                placement.placement.x_offset
            } else {
                0
            },
            y_offset: if top_clip_cells == 0 {
                placement.placement.y_offset
            } else {
                0
            },
        },
        format_code,
    ))
}

fn scale_pixels(value: u32, source: u32, dest: u32) -> u32 {
    ((value as u64).saturating_mul(source as u64) / dest.max(1) as u64).min(u32::MAX as u64) as u32
}

fn pane_layer_image_signature(layer: &crate::app::pane_graphics::Layer) -> ImageSignature {
    ImageSignature {
        image_width: layer.image_width,
        image_height: layer.image_height,
        format_code: kitty_format_code(pane_graphics_kitty_format(layer.format)),
        data_len: layer.data_len(),
        data_fingerprint: layer.data_fingerprint,
    }
}

fn image_signature(placement: &HostPlacement, format_code: u32) -> ImageSignature {
    ImageSignature {
        image_width: placement.placement.image_width,
        image_height: placement.placement.image_height,
        format_code,
        data_len: placement.placement.data_len,
        data_fingerprint: placement.placement.data_fingerprint,
    }
}

fn image_signature_from_descriptor(
    descriptor: KittyImageDescriptor,
    format_code: u32,
) -> ImageSignature {
    ImageSignature {
        image_width: descriptor.image_width,
        image_height: descriptor.image_height,
        format_code,
        data_len: descriptor.data_len,
        data_fingerprint: descriptor.data_fingerprint,
    }
}

fn placement_signature(
    clipped: ClippedPlacement,
    z: i32,
    scrollback_offset: u32,
) -> PlacementSignature {
    PlacementSignature {
        x: clipped.x,
        y: clipped.y,
        cols: clipped.cols,
        rows: clipped.rows,
        source_x: clipped.source_x,
        source_y: clipped.source_y,
        source_width: clipped.source_width,
        source_height: clipped.source_height,
        x_offset: clipped.x_offset,
        y_offset: clipped.y_offset,
        z,
        scrollback_offset,
    }
}

fn kitty_format_code(format: KittyImageFormat) -> u32 {
    match format {
        KittyImageFormat::Rgb => 24,
        KittyImageFormat::Rgba => 32,
        KittyImageFormat::Png => 100,
    }
}

fn encode_kitty_data(out: &mut Vec<u8>, control: &str, data: &[u8]) {
    let mut chunks = data.chunks(KITTY_CHUNK_BYTES).peekable();
    let Some(first) = chunks.next() else {
        return;
    };
    let more = if chunks.peek().is_some() { 1 } else { 0 };
    let encoded = base64::engine::general_purpose::STANDARD.encode(first);
    let _ = write!(out, "\x1b_G{control},m={more};{encoded}\x1b\\");

    while let Some(chunk) = chunks.next() {
        let more = if chunks.peek().is_some() { 1 } else { 0 };
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        let _ = write!(out, "\x1b_Gm={more};{encoded}\x1b\\");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_placement(viewport_col: i32, viewport_row: i32) -> HostPlacement {
        HostPlacement {
            pane_id: PaneId::from_raw(1),
            host_image_id: None,
            area: Rect::new(0, 0, 20, 10),
            cell_size: HostCellSize {
                width_px: 10,
                height_px: 10,
            },
            source_key: HostSourceKey::Terminal {
                pane_id: PaneId::from_raw(1),
                image_id: 7,
            },
            scrollback_offset: 0,
            placement: KittyImagePlacement {
                image_id: 7,
                placement_id: 3,
                z: 0,
                x_offset: 0,
                y_offset: 0,
                image_width: 30,
                image_height: 30,
                format: KittyImageFormat::Rgba,
                data_len: 30 * 30 * 4,
                data_fingerprint: 42,
                data: vec![255; 30 * 30 * 4],
                render: KittyPlacementRenderInfo {
                    pixel_width: 0,
                    pixel_height: 0,
                    grid_cols: 3,
                    grid_rows: 3,
                    viewport_col,
                    viewport_row,
                    source_x: 0,
                    source_y: 0,
                    source_width: 0,
                    source_height: 0,
                },
            },
        }
    }

    fn pane_layer_placement(viewport_col: i32, viewport_row: i32) -> HostPlacement {
        let mut placement = test_placement(viewport_col, viewport_row);
        placement.source_key = HostSourceKey::PaneLayer {
            pane_id: placement.pane_id,
            layer_id: "primary".into(),
        };
        placement
    }

    fn update(
        cache: &mut HostGraphicsCache,
        placements: &[HostPlacement],
        replay: bool,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        if replay {
            cache.request_placement_replay();
        }
        let live = cache
            .sources
            .keys()
            .filter(|source| matches!(source, HostSourceKey::PaneLayer { .. }))
            .cloned()
            .collect::<HashSet<_>>();
        bytes.extend(drain_graphics_updates(cache, placements, &live));
        bytes
    }

    #[test]
    fn terminal_placement_id_preserves_legacy_identity() {
        let placement = test_placement(0, 0);
        let mut legacy = DefaultHasher::new();
        placement.pane_id.raw().hash(&mut legacy);
        placement.placement.image_id.hash(&mut legacy);
        placement.placement.placement_id.hash(&mut legacy);
        let expected = 1 + ((legacy.finish() as u32) % 900_000);

        assert_eq!(
            host_placement_id(&placement.source_key, &placement.placement),
            expected
        );
        assert_ne!(
            host_placement_id(
                &HostSourceKey::PaneLayer {
                    pane_id: placement.pane_id,
                    layer_id: "primary".into(),
                },
                &placement.placement,
            ),
            expected
        );
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_command_is_rgba_quiet_zero_and_path_encoded() {
        let mut bytes = Vec::new();
        encode_kitty_regular_file(
            &mut bytes,
            b"\x1b[2;3H",
            "a=T,f=32,s=3,v=2,i=42,p=7,c=3,r=2,z=0,C=1,q=0",
            "/private/frame",
        );
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("\x1b7\x1b[2;3H\x1b_Ga=T,f=32"));
        assert!(text.contains(",C=1,q=0,t=f;L3ByaXZhdGUvZnJhbWU="));
        assert!(text.ends_with("\x1b\\\x1b8"));
    }

    #[test]
    fn pane_graphics_image_ids_are_disjoint_from_terminal_image_ids() {
        let placement = test_placement(0, 0);
        let signature = image_signature(&placement, kitty_format_code(placement.placement.format));
        let terminal_id = host_image_id_for_signature(placement.pane_id, signature);
        let mut graphics = crate::app::pane_graphics::Runtime::default();
        let primary = (placement.pane_id, "primary".into());
        let pane_graphics_id = graphics.reserve_image_id(&primary).unwrap();
        graphics.slots.insert(
            primary.clone(),
            crate::app::pane_graphics::Slot::test(pane_graphics_id, None),
        );

        assert_eq!(terminal_id & PANE_GRAPHICS_IMAGE_ID_BIT, 0);
        assert_ne!(pane_graphics_id & PANE_GRAPHICS_IMAGE_ID_BIT, 0);
        assert_eq!(
            pane_graphics_id,
            graphics.reserve_image_id(&primary).unwrap()
        );
        assert_ne!(
            pane_graphics_id,
            graphics
                .reserve_image_id(&(placement.pane_id, "toolbar".into()))
                .unwrap()
        );
    }

    #[test]
    fn clipped_placement_handles_positive_viewport_without_wrapping() {
        let placement = test_placement(2, 2);
        let (clipped, _) = clipped_placement(&placement).expect("visible placement");

        assert_eq!(clipped.x, 2);
        assert_eq!(clipped.y, 2);
        assert_eq!(clipped.cols, 3);
        assert_eq!(clipped.rows, 3);
        assert_eq!(clipped.source_x, 0);
        assert_eq!(clipped.source_y, 0);
    }

    #[test]
    fn clipped_placement_crops_negative_viewport_offsets() {
        let placement = test_placement(-1, -1);
        let (clipped, _) = clipped_placement(&placement).expect("partially visible placement");

        assert_eq!(clipped.x, 0);
        assert_eq!(clipped.y, 0);
        assert_eq!(clipped.cols, 2);
        assert_eq!(clipped.rows, 2);
        assert_eq!(clipped.source_x, 10);
        assert_eq!(clipped.source_y, 10);
    }

    #[test]
    fn pane_graphics_layer_defaults_to_full_pane_grid() {
        let info = PaneInfo {
            id: PaneId::from_raw(9),
            rect: Rect::new(0, 0, 12, 5),
            inner_rect: Rect::new(2, 1, 8, 3),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        };
        let layer = crate::app::pane_graphics::Layer::inline(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            80,
            30,
            vec![255; 80 * 30 * 4],
            crate::api::schema::PaneGraphicsPlacementParams::default(),
            0,
        );

        let placement = pane_graphics_host_placement(
            &info,
            "primary",
            PANE_GRAPHICS_IMAGE_ID_BIT | 1,
            HostCellSize {
                width_px: 10,
                height_px: 10,
            },
            &layer,
            &HashMap::new(),
            true,
        );
        let (clipped, format_code) = clipped_placement(&placement).expect("visible layer");

        assert_eq!(format_code, 32);
        assert_eq!(clipped.x, 2);
        assert_eq!(clipped.y, 1);
        assert_eq!(clipped.cols, 8);
        assert_eq!(clipped.rows, 3);
        assert_eq!(placement.placement.data.len(), 80 * 30 * 4);
    }

    #[test]
    fn graphics_update_uploads_once_then_repositions_only() {
        let mut cache = HostGraphicsCache::default();
        let first = update(&mut cache, &[test_placement(0, 0)], false);
        assert!(String::from_utf8_lossy(&first).contains("a=t"));
        assert!(String::from_utf8_lossy(&first).contains("a=p"));
        assert!(update(&mut cache, &[test_placement(0, 0)], false).is_empty());

        let mut changed = test_placement(0, 0);
        changed.placement.z = 1;
        for placement in [changed, test_placement(0, 1)] {
            let bytes = update(&mut cache, &[placement], false);
            assert!(!String::from_utf8_lossy(&bytes).contains("a=t"));
            assert!(String::from_utf8_lossy(&bytes).contains("a=p"));
        }
    }

    #[test]
    fn view_change_redisplays_unchanged_visible_placement() {
        let mut cache = HostGraphicsCache::default();
        update(&mut cache, &[test_placement(0, 0)], false);
        assert_eq!(cache.placements.len(), 1);
        let bytes = update(&mut cache, &[test_placement(0, 0)], true);
        assert!(!String::from_utf8_lossy(&bytes).contains("a=t"));
        assert!(String::from_utf8_lossy(&bytes).contains("a=p"));
        assert_eq!(cache.placements.len(), 1);
    }

    #[test]
    fn surface_reset_deletes_then_reuploads_and_redisplays_placement() {
        let mut cache = HostGraphicsCache::default();
        update(&mut cache, &[test_placement(0, 0)], false);
        assert_eq!((cache.images.len(), cache.placements.len()), (1, 1));
        let mut bytes = cache.clear_bytes();
        bytes.extend(update(&mut cache, &[test_placement(0, 0)], false));
        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(redisplay.contains("a=d,d=I"));
        assert!(redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
        assert_eq!((cache.images.len(), cache.placements.len()), (1, 1));
    }

    #[test]
    fn scrollback_offset_change_redisplays_placement() {
        let mut cache = HostGraphicsCache::default();
        update(&mut cache, &[test_placement(0, 0)], false);
        let mut scrolled = test_placement(0, 0);
        scrolled.scrollback_offset = 3;
        let bytes = update(&mut cache, &[scrolled], false);
        assert!(!String::from_utf8_lossy(&bytes).contains("a=t"));
        assert!(String::from_utf8_lossy(&bytes).contains("a=p"));
    }

    #[test]
    fn changing_first_source_does_not_starve_second_source() {
        let layers = |first| {
            [(1, "a", first), (2, "b", 80)].map(|(id, name, fingerprint)| {
                let mut placement = pane_layer_placement(0, 0);
                placement.host_image_id = Some(PANE_GRAPHICS_IMAGE_ID_BIT | id);
                placement.source_key = HostSourceKey::PaneLayer {
                    pane_id: placement.pane_id,
                    layer_id: name.into(),
                };
                placement.placement.data_fingerprint = fingerprint;
                placement
            })
        };
        let initial = layers(42);
        let live = initial.iter().map(|p| p.source_key.clone()).collect();
        let mut cache = HostGraphicsCache::default();
        assert!(
            encode_graphics_update_incremental(&mut cache, &initial, &live, None, false).incomplete
        );
        assert!(
            encode_graphics_update_incremental(&mut cache, &layers(43), &live, None, false)
                .incomplete
        );
        assert_eq!(cache.images.len(), 2, "second source uploaded next");

        let terminal = |id| {
            let mut placement = test_placement(0, 0);
            placement.placement.image_id = id;
            placement.placement.data_fingerprint = u64::from(id);
            placement.source_key = HostSourceKey::Terminal {
                pane_id: placement.pane_id,
                image_id: id,
            };
            placement
        };
        let second = terminal(99).source_key;
        let mut cache = HostGraphicsCache::default();
        for id in 1..=3 {
            assert!(
                encode_graphics_update_incremental(
                    &mut cache,
                    &[terminal(id), terminal(99)],
                    &HashSet::new(),
                    None,
                    false,
                )
                .incomplete
            );
        }
        assert!(cache.sources.contains_key(&second));
    }

    #[test]
    fn large_terminal_image_is_local_but_quarantined_headless() {
        let placements = || {
            let mut large = test_placement(0, 0);
            large.placement.data_len = 24 * 1024 * 1024;
            let mut later = test_placement(4, 0);
            later.placement.image_id = 8;
            later.source_key = HostSourceKey::Terminal {
                pane_id: later.pane_id,
                image_id: 8,
            };
            [large, later]
        };
        for (budget, expected) in [
            (None, (true, 1, 0)),
            (Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET), (false, 1, 1)),
        ] {
            let mut cache = HostGraphicsCache::default();
            let encoded = encode_graphics_update_incremental(
                &mut cache,
                &placements(),
                &HashSet::new(),
                budget,
                false,
            );
            assert!(String::from_utf8_lossy(&encoded.bytes).contains("a=t"));
            assert_eq!(
                (
                    encoded.incomplete,
                    cache.images.len(),
                    cache.oversized.len()
                ),
                expected
            );
        }
    }

    #[test]
    fn terminal_image_data_requests_deduplicate_and_reconsider_changed_signatures() {
        let pane_id = PaneId::from_raw(1);
        let descriptor = KittyImageDescriptor {
            image_id: 7,
            placement_id: 1,
            image_width: 3456,
            image_height: 2234,
            format: KittyImageFormat::Rgba,
            data_len: 3456 * 2234 * 4,
            data_fingerprint: 42,
        };
        let mut requested = HashSet::new();
        assert!(terminal_image_needs_data(
            pane_id,
            descriptor,
            &HashMap::new(),
            &HashMap::new(),
            &mut requested,
        ));
        let mut second_placement = descriptor;
        second_placement.placement_id = 2;
        assert!(!terminal_image_needs_data(
            pane_id,
            second_placement,
            &HashMap::new(),
            &HashMap::new(),
            &mut requested,
        ));

        let signature = image_signature_from_descriptor(descriptor, 32);
        let source = HostSourceKey::Terminal {
            pane_id,
            image_id: descriptor.image_id,
        };
        let oversized = HashMap::from([(source, signature)]);
        let mut requested = HashSet::new();
        assert!(!terminal_image_needs_data(
            pane_id,
            descriptor,
            &HashMap::new(),
            &oversized,
            &mut requested,
        ));
        let mut changed = descriptor;
        changed.data_fingerprint += 1;
        assert!(terminal_image_needs_data(
            pane_id,
            changed,
            &HashMap::new(),
            &oversized,
            &mut requested,
        ));
    }

    #[test]
    fn maximum_pane_graphics_stream_payload_fits_client_graphics_frame() {
        let mut placement = pane_layer_placement(0, 0);
        placement.placement.format = KittyImageFormat::Png;
        placement.placement.image_width = 1;
        placement.placement.image_height = 1;
        placement.placement.data = vec![1_u8; crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES];
        placement.placement.data_len = placement.placement.data.len();
        let (clipped, format_code) = clipped_placement(&placement).expect("visible placement");
        let host_id = host_image_id(placement.pane_id, &placement.placement);
        let mut encoded = Vec::new();

        assert!(encode_upload_image(
            &mut encoded,
            &placement,
            format_code,
            host_id,
        ));
        encode_display_placement(&mut encoded, clipped, host_id, 1, 0);

        let mut framed = Vec::new();
        crate::protocol::write_message(
            &mut framed,
            &crate::protocol::ServerMessage::Graphics { bytes: encoded },
        )
        .unwrap();
        assert!(framed.len() <= crate::protocol::MAX_GRAPHICS_FRAME_SIZE + 4);
    }
}
