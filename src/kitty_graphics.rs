use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use ratatui::layout::Rect;

use crate::app::state::AppState;
use crate::app::Mode;
use crate::ghostty::{
    KittyImageDescriptor, KittyImageFormat, KittyImagePlacement, KittyPlacementRenderInfo,
};
use crate::layout::{PaneId, PaneInfo};
use crate::terminal::TerminalRuntimeRegistry;

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
    pub(crate) fn try_from_terminal(area: Rect) -> Option<Self> {
        let Ok(size) = crossterm::terminal::window_size() else {
            return None;
        };
        if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
            return None;
        }
        Some(
            Self {
                width_px: (size.width as u32 / size.columns as u32).max(1),
                height_px: (size.height as u32 / size.rows as u32).max(1),
            }
            .for_area(area),
        )
    }

    pub(crate) fn is_known(self) -> bool {
        self.width_px > 0 && self.height_px > 0
    }

    pub(crate) fn fallback_for_area(area: Rect) -> Self {
        Self {
            width_px: 8,
            height_px: 16,
        }
        .for_area(area)
    }

    fn for_area(self, area: Rect) -> Self {
        if area.width == 0 || area.height == 0 {
            return Self::default();
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostViewKey {
    workspace_index: usize,
    tab_index: usize,
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
    Terminal { pane_id: PaneId, image_id: u32 },
    PaneLayer { pane_id: PaneId, layer_id: String },
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
    view: Option<HostViewKey>,
    replay_placements: bool,
    replayed_placements: HashSet<(u32, u32)>,
}

static KITTY_GRAPHICS_ENABLED: AtomicBool = AtomicBool::new(false);
static LOCAL_HOST_GRAPHICS: OnceLock<Mutex<HostGraphicsCache>> = OnceLock::new();

pub(crate) fn set_enabled(enabled: bool) {
    KITTY_GRAPHICS_ENABLED.store(enabled, Ordering::Release);
}

pub(crate) fn is_enabled() -> bool {
    KITTY_GRAPHICS_ENABLED.load(Ordering::Acquire)
}

pub(crate) fn paint_local_pane_graphics(
    app: &AppState,
    graphics: &crate::app::pane_graphics::Runtime,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cell_size: HostCellSize,
) -> io::Result<()> {
    let cache = LOCAL_HOST_GRAPHICS.get_or_init(|| Mutex::new(HostGraphicsCache::default()));
    let Ok(mut cache) = cache.lock() else {
        return Ok(());
    };
    if graphics.slots.is_empty() && !cache.has_pane_sources() {
        let encoded = encode_local_pane_graphics(
            app,
            graphics,
            terminal_runtimes,
            app.view.tab_surface(),
            cell_size,
            None,
            &mut cache,
        );
        drop(cache);
        if encoded.bytes.is_empty() {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"\x1b7")?;
        stdout.write_all(&encoded.bytes)?;
        stdout.write_all(b"\x1b8")?;
        return stdout.flush();
    }

    let mut stdout = io::stdout().lock();
    loop {
        let encoded = encode_local_pane_graphics(
            app,
            graphics,
            terminal_runtimes,
            app.view.tab_surface(),
            cell_size,
            None,
            &mut cache,
        );
        if !encoded.bytes.is_empty() {
            stdout.write_all(b"\x1b7")?;
            stdout.write_all(&encoded.bytes)?;
            stdout.write_all(b"\x1b8")?;
        }
        if !encoded.incomplete {
            break;
        }
    }
    stdout.flush()
}

pub(crate) struct EncodedGraphics {
    pub(crate) bytes: Vec<u8>,
    pub(crate) incomplete: bool,
}

pub(crate) fn encode_local_pane_graphics(
    app: &AppState,
    graphics: &crate::app::pane_graphics::Runtime,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
    transaction_budget: Option<usize>,
    cache: &mut HostGraphicsCache,
) -> EncodedGraphics {
    let visible = app.mode == Mode::Terminal && cell_size.is_known();
    if graphics.slots.is_empty() {
        if !visible {
            return EncodedGraphics {
                bytes: cache.clear_bytes(),
                incomplete: false,
            };
        }
        let mut bytes = if transaction_budget.is_none() && cache.has_pane_sources() {
            cache.clear_pane_sources()
        } else {
            Vec::new()
        };
        let placements = collect_visible_placements(
            app,
            graphics,
            terminal_runtimes,
            surface,
            cell_size,
            &cache.images,
            &cache.oversized,
        );
        let view_changed = cache.update_view(active_view_key(app));
        let mut encoded =
            encode_terminal_graphics_update(cache, &placements, view_changed, transaction_budget);
        if !bytes.is_empty() {
            bytes.extend(encoded.bytes);
            encoded.bytes = bytes;
        }
        return encoded;
    }

    let live_pane_sources = graphics
        .slots
        .iter()
        .filter(|(_, slot)| slot.layer.is_some())
        .map(|((pane_id, layer_id), _)| HostSourceKey::PaneLayer {
            pane_id: *pane_id,
            layer_id: layer_id.clone(),
        })
        .collect::<HashSet<_>>();
    let placements = if visible {
        collect_visible_placements(
            app,
            graphics,
            terminal_runtimes,
            surface,
            cell_size,
            &cache.images,
            &cache.oversized,
        )
    } else {
        Vec::new()
    };
    cache.update_view(visible.then(|| active_view_key(app)).flatten());
    // The host text blit overwrites Kitty placements, so every rendered frame must
    // display cached images again even when their data and geometry are unchanged.
    cache.request_placement_replay();
    encode_graphics_update_incremental(
        cache,
        &placements,
        &live_pane_sources,
        transaction_budget,
        false,
    )
}

pub(crate) fn has_visible_pane_graphics(
    app: &AppState,
    graphics: &crate::app::pane_graphics::Runtime,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
) -> bool {
    if app.mode != Mode::Terminal || !cell_size.is_known() {
        return false;
    }

    let Some(ws_idx) = app.active else {
        return false;
    };
    if app
        .workspaces
        .get(ws_idx)
        .and_then(crate::workspace::Workspace::active_tab)
        .is_none()
    {
        return false;
    }

    for info in surface.pane_infos {
        let empty_uploaded = HashMap::new();
        if graphics.slots.iter().any(|((pane_id, layer_id), slot)| {
            *pane_id == info.id
                && slot.layer.as_ref().is_some_and(|layer| {
                    clipped_placement(&pane_graphics_host_placement(
                        info,
                        layer_id,
                        slot.host_image_id,
                        cell_size,
                        layer,
                        &empty_uploaded,
                        false,
                    ))
                    .is_some()
                })
        }) {
            return true;
        }

        if let Some(runtime) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        {
            let scrollback_offset = runtime
                .scroll_metrics()
                .map(|m| m.offset_from_bottom as u32)
                .unwrap_or(0);
            for placement in runtime.kitty_image_placements_with_data_filter(|_| false) {
                let host_placement = HostPlacement {
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
                };
                if clipped_placement(&host_placement).is_some() {
                    return true;
                }
            }
        }
    }
    false
}

fn encode_terminal_graphics_update(
    cache: &mut HostGraphicsCache,
    placements: &[HostPlacement],
    view_changed: bool,
    transaction_budget: Option<usize>,
) -> EncodedGraphics {
    if transaction_budget.is_some() {
        cache.request_placement_replay();
        return encode_graphics_update_incremental(
            cache,
            placements,
            &HashSet::new(),
            transaction_budget,
            true,
        );
    }

    cache.reset_incremental_state();
    let mut bytes = Vec::new();
    encode_terminal_graphics_update_legacy(&mut bytes, placements, view_changed, cache);
    EncodedGraphics {
        bytes,
        incomplete: false,
    }
}

fn encode_terminal_graphics_update_legacy(
    bytes: &mut Vec<u8>,
    placements: &[HostPlacement],
    view_changed: bool,
    cache: &mut HostGraphicsCache,
) {
    let current_sources = placements
        .iter()
        .filter(|placement| matches!(placement.source_key, HostSourceKey::Terminal { .. }))
        .map(|placement| placement.source_key.clone())
        .collect::<HashSet<_>>();
    cache
        .sources
        .retain(|source, _| current_sources.contains(source));

    let mut current_placements = HashSet::new();
    for placement in placements {
        let Some((clipped, format_code)) = clipped_placement(placement) else {
            continue;
        };
        let host_id = host_image_id(placement.pane_id, &placement.placement);
        let placement_id = host_placement_id(&placement.source_key, &placement.placement);
        let image_signature = image_signature(placement, format_code);
        let placement_signature =
            placement_signature(clipped, placement.placement.z, placement.scrollback_offset);
        let placement_key = (host_id, placement_id);
        current_placements.insert(placement_key);

        match cache.images.get(&host_id).copied() {
            Some(existing) if existing == image_signature => {}
            Some(_) => {
                encode_delete_image(bytes, host_id);
                cache.placements.retain(|(image_id, id), _| {
                    if *image_id == host_id {
                        current_placements.remove(&(*image_id, *id));
                        false
                    } else {
                        true
                    }
                });
                if !encode_upload_image(bytes, placement, format_code, host_id) {
                    continue;
                }
                cache.images.insert(host_id, image_signature);
            }
            None => {
                if !encode_upload_image(bytes, placement, format_code, host_id) {
                    continue;
                }
                cache.images.insert(host_id, image_signature);
            }
        }

        release_superseded_terminal_image_legacy(
            bytes,
            cache,
            &mut current_placements,
            placement.source_key.clone(),
            host_id,
        );

        match cache.placements.get_mut(&placement_key) {
            Some(existing) if !view_changed && *existing == placement_signature => {}
            Some(existing) => {
                encode_display_placement(
                    bytes,
                    clipped,
                    host_id,
                    placement_id,
                    placement.placement.z,
                );
                *existing = placement_signature;
            }
            None => {
                encode_display_placement(
                    bytes,
                    clipped,
                    host_id,
                    placement_id,
                    placement.placement.z,
                );
                cache.placements.insert(placement_key, placement_signature);
            }
        }
    }

    let stale = cache
        .placements
        .keys()
        .filter(|key| !current_placements.contains(key))
        .copied()
        .collect::<Vec<_>>();
    for (host_id, placement_id) in stale {
        encode_delete_placement(bytes, host_id, placement_id);
        cache.placements.remove(&(host_id, placement_id));
    }
}

fn release_superseded_terminal_image_legacy(
    bytes: &mut Vec<u8>,
    cache: &mut HostGraphicsCache,
    current_placements: &mut HashSet<(u32, u32)>,
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
    cache.placements.retain(|(image_id, placement_id), _| {
        if *image_id == previous {
            current_placements.remove(&(*image_id, *placement_id));
            false
        } else {
            true
        }
    });
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

fn image_transaction_fits(placement: &HostPlacement, budget: Option<usize>) -> bool {
    let Some(budget) = budget else {
        return true;
    };
    let data = placement.placement.data_len;
    let encoded = data.div_ceil(3).saturating_mul(4);
    let command_overhead = data.div_ceil(KITTY_CHUNK_BYTES).saturating_mul(16) + 1024;
    encoded.saturating_add(command_overhead) <= budget
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

#[cfg(test)]
fn encode_graphics_update(
    bytes: &mut Vec<u8>,
    placements: &[HostPlacement],
    replay: bool,
    images: &mut HashMap<u32, ImageSignature>,
    host_placements: &mut HashMap<(u32, u32), PlacementSignature>,
    sources: &mut HashMap<HostSourceKey, u32>,
) {
    let mut cache = HostGraphicsCache {
        images: std::mem::take(images),
        placements: std::mem::take(host_placements),
        sources: std::mem::take(sources),
        ..HostGraphicsCache::default()
    };
    let mut live = cache
        .sources
        .keys()
        .filter(|source| matches!(source, HostSourceKey::PaneLayer { .. }))
        .cloned()
        .collect::<HashSet<_>>();
    live.extend(
        placements
            .iter()
            .filter(|placement| matches!(placement.source_key, HostSourceKey::PaneLayer { .. }))
            .map(|placement| placement.source_key.clone()),
    );
    if live.is_empty() {
        encode_terminal_graphics_update_legacy(bytes, placements, replay, &mut cache);
    } else {
        if replay {
            cache.request_placement_replay();
        }
        bytes.extend(drain_graphics_updates(&mut cache, placements, &live));
    }
    *images = cache.images;
    *host_placements = cache.placements;
    *sources = cache.sources;
}

pub(crate) fn clear_all_host_graphics() -> io::Result<()> {
    let cache = LOCAL_HOST_GRAPHICS.get_or_init(|| Mutex::new(HostGraphicsCache::default()));
    let mut bytes = Vec::new();
    if let Ok(mut cache) = cache.lock() {
        bytes = cache.clear_bytes();
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.flush()
}

impl HostGraphicsCache {
    fn clear_pane_sources(&mut self) -> Vec<u8> {
        let pane_sources = self
            .sources
            .keys()
            .filter(|source| matches!(source, HostSourceKey::PaneLayer { .. }))
            .cloned()
            .collect::<Vec<_>>();
        let mut removed_images = HashSet::new();
        for source in pane_sources {
            if let Some(image_id) = self.sources.remove(&source) {
                removed_images.insert(image_id);
            }
            self.oversized.remove(&source);
        }

        let mut bytes = Vec::new();
        for image_id in removed_images {
            if self.sources.values().any(|id| *id == image_id) {
                continue;
            }
            encode_delete_image(&mut bytes, image_id);
            self.images.remove(&image_id);
            self.placements.retain(|(id, _), _| *id != image_id);
            self.replayed_placements.retain(|(id, _)| *id != image_id);
        }
        self.reset_incremental_progress();
        bytes
    }

    fn has_pane_sources(&self) -> bool {
        self.sources
            .keys()
            .any(|source| matches!(source, HostSourceKey::PaneLayer { .. }))
    }

    fn reset_incremental_progress(&mut self) {
        self.continuation = None;
        self.replay_placements = false;
        self.replayed_placements.clear();
    }

    fn reset_incremental_state(&mut self) {
        self.oversized.clear();
        self.reset_incremental_progress();
    }

    fn quarantine_oversized(&mut self, source: HostSourceKey, signature: ImageSignature) {
        if !self.oversized.contains_key(&source) && self.oversized.len() >= MAX_OVERSIZED_SOURCES {
            if let Some(evicted) = self.oversized.keys().next().cloned() {
                self.oversized.remove(&evicted);
            }
        }
        self.oversized.insert(source, signature);
    }

    pub(crate) fn trust_pane_layer(
        &mut self,
        key: &crate::app::pane_graphics::Key,
        host_id: u32,
        layer: &crate::app::pane_graphics::Layer,
    ) {
        let source = HostSourceKey::PaneLayer {
            pane_id: key.0,
            layer_id: key.1.clone(),
        };
        self.oversized.remove(&source);
        self.sources.insert(source, host_id);
        self.images
            .insert(host_id, pane_layer_image_signature(layer));
    }

    pub(crate) fn forget_pane_layer(&mut self, key: &crate::app::pane_graphics::Key, host_id: u32) {
        let source = HostSourceKey::PaneLayer {
            pane_id: key.0,
            layer_id: key.1.clone(),
        };
        self.sources.remove(&source);
        self.oversized.remove(&source);
        self.images.remove(&host_id);
        self.placements.retain(|(id, _), _| *id != host_id);
        self.replayed_placements.retain(|(id, _)| *id != host_id);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.images.is_empty() && self.placements.is_empty()
    }

    pub(crate) fn request_placement_replay(&mut self) {
        if !self.replay_placements {
            self.replay_placements = true;
            self.replayed_placements.clear();
        }
    }

    #[cfg(test)]
    fn hide_except_live_pane_layers(&mut self, live: &HashSet<HostSourceKey>) -> Vec<u8> {
        drain_graphics_updates(self, &[], live)
    }

    #[cfg(test)]
    pub(crate) fn test_image_count(&self) -> usize {
        self.images.len()
    }

    #[cfg(test)]
    pub(crate) fn test_mark_non_empty(&mut self) {
        self.images.insert(
            HOST_IMAGE_ID_BASE,
            ImageSignature {
                image_width: 1,
                image_height: 1,
                format_code: 32,
                data_len: 4,
                data_fingerprint: 1,
            },
        );
    }

    pub(crate) fn clear_bytes(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for id in self.images.keys().copied().collect::<Vec<_>>() {
            encode_delete_image(&mut bytes, id);
        }
        self.images.clear();
        self.placements.clear();
        self.sources.clear();
        self.reset_incremental_state();
        self.view = None;
        bytes
    }

    pub(crate) fn clear_next(&mut self) -> EncodedGraphics {
        self.continuation = None;
        let mut bytes = Vec::new();
        if let Some(id) = self.images.keys().copied().min() {
            encode_delete_image(&mut bytes, id);
            self.images.remove(&id);
            self.placements.retain(|(image, _), _| *image != id);
            self.sources.retain(|_, image| *image != id);
            self.replayed_placements.retain(|(image, _)| *image != id);
        } else if let Some(key) = self.placements.keys().copied().min() {
            encode_delete_placement(&mut bytes, key.0, key.1);
            self.placements.remove(&key);
            self.replayed_placements.remove(&key);
        } else {
            self.sources.clear();
            self.oversized.clear();
            self.view = None;
            self.replay_placements = false;
            self.replayed_placements.clear();
        }
        EncodedGraphics {
            bytes,
            incomplete: !self.is_empty(),
        }
    }

    fn update_view(&mut self, view_key: Option<HostViewKey>) -> bool {
        if self.view == view_key {
            return false;
        }
        self.view = view_key;
        self.continuation = None;
        true
    }
}

fn active_view_key(app: &AppState) -> Option<HostViewKey> {
    let ws_idx = app.active?;
    let ws = app.workspaces.get(ws_idx)?;
    Some(HostViewKey {
        workspace_index: ws_idx,
        tab_index: ws.active_tab_index(),
    })
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
    let ws_idx = match app.active {
        Some(idx) => idx,
        None => {
            tracing::debug!("collect_visible_placements: no active workspace");
            return Vec::new();
        }
    };
    if app
        .workspaces
        .get(ws_idx)
        .and_then(crate::workspace::Workspace::active_tab)
        .is_none()
    {
        tracing::debug!(ws_idx, "collect_visible_placements: no active tab");
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
    }
    placement.image_id.hash(&mut hasher);
    placement.placement_id.hash(&mut hasher);
    1 + ((hasher.finish() as u32) % 900_000)
}

pub(crate) struct DirectFileCommand {
    pub(crate) leading: Vec<u8>,
    pub(crate) control: String,
}

pub(crate) fn prepare_direct_file(
    app: &AppState,
    graphics: &crate::app::pane_graphics::Runtime,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
    allow_placement: bool,
    cache: &HostGraphicsCache,
    key: &crate::app::pane_graphics::Key,
) -> Option<DirectFileCommand> {
    let slot = graphics.slots.get(key)?;
    let layer = slot.layer.as_ref()?;
    layer.direct_lease()?;

    let info = allow_placement
        .then(|| surface.pane_infos.iter().find(|info| info.id == key.0))
        .flatten()
        .filter(|_| app.mode == Mode::Terminal && cell_size.is_known() && app.active.is_some());
    if let Some(command) = info
        .map(|info| {
            pane_graphics_host_placement(
                info,
                &key.1,
                slot.host_image_id,
                cell_size,
                layer,
                &cache.images,
                false,
            )
        })
        .and_then(|placement| direct_file_command(&placement, slot.host_image_id))
        .map(|(command, _, _, _)| command)
    {
        return Some(command);
    }

    let inline_fallback_available = layer.data_len()
        <= crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES
        && graphics.can_store_inline(key, layer.data_len());
    (!inline_fallback_available).then(|| direct_file_upload_command(layer, slot.host_image_id))
}

fn direct_file_upload_command(
    layer: &crate::app::pane_graphics::Layer,
    host_image_id: u32,
) -> DirectFileCommand {
    DirectFileCommand {
        leading: Vec::new(),
        control: format!(
            "a=t,f=32,s={},v={},i={host_image_id},q=0",
            layer.image_width, layer.image_height
        ),
    }
}

fn direct_file_command(
    placement: &HostPlacement,
    host_image_id: u32,
) -> Option<(DirectFileCommand, ClippedPlacement, u32, u32)> {
    let (clipped, format_code) = clipped_placement(placement)?;
    let placement_id = host_placement_id(&placement.source_key, &placement.placement);
    let mut control = format!(
        "a=T,f={format_code},s={},v={},i={host_image_id},p={placement_id},c={},r={},z={},C=1,q=0",
        placement.placement.image_width,
        placement.placement.image_height,
        clipped.cols,
        clipped.rows,
        placement.placement.z,
    );
    append_placement_controls(&mut control, clipped);
    Some((
        DirectFileCommand {
            leading: format!("\x1b[{};{}H", clipped.y + 1, clipped.x + 1).into_bytes(),
            control,
        },
        clipped,
        format_code,
        placement_id,
    ))
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

    #[test]
    fn fallback_cell_size_is_usable_only_for_nonempty_areas() {
        assert_eq!(
            HostCellSize::fallback_for_area(Rect::new(0, 0, 80, 24)),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            }
        );
        assert!(!HostCellSize::fallback_for_area(Rect::default()).is_known());
    }

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
        encode_graphics_update(
            &mut bytes,
            placements,
            replay,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );
        bytes
    }

    #[test]
    fn terminal_graphics_without_pane_layers_preserves_legacy_transcript() {
        fn record(transcript: &mut Vec<u8>, bytes: &[u8]) {
            transcript.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            transcript.extend_from_slice(bytes);
        }

        fn fnv1a(bytes: &[u8]) -> u64 {
            bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            })
        }

        let mut cache = HostGraphicsCache::default();
        let mut transcript = Vec::new();

        record(
            &mut transcript,
            &update(&mut cache, &[test_placement(0, 0)], false),
        );
        record(
            &mut transcript,
            &update(&mut cache, &[test_placement(0, 0)], false),
        );
        record(
            &mut transcript,
            &update(&mut cache, &[test_placement(-1, 2)], false),
        );
        record(
            &mut transcript,
            &update(&mut cache, &[test_placement(-1, 2)], true),
        );

        let mut changed = test_placement(-1, 2);
        changed.placement.data_fingerprint = 43;
        record(&mut transcript, &update(&mut cache, &[changed], false));
        record(&mut transcript, &update(&mut cache, &[], false));

        assert_eq!(transcript.len(), 10_084);
        assert_eq!(fnv1a(&transcript), 0xc5bd_83e4_b039_870e);
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
    fn direct_file_uses_one_clipped_transmit_and_display_at_the_final_position() {
        let mut placement = pane_layer_placement(-1, 2);
        placement.area = Rect::new(10, 4, 8, 6);
        let (command, _, _, _) = direct_file_command(&placement, (1 << 31) | 9).unwrap();
        let control = command.control;

        assert_eq!(command.leading, b"\x1b[7;11H");
        assert!(control.starts_with("a=T,f=32,s=30,v=30,i=2147483657,p="));
        assert!(control.contains(",c=2,r=3,z=0,C=1,q=0,x=10,w=20,h=30"));
        assert!(direct_file_command(&pane_layer_placement(30, 0), 9).is_none());
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
    fn empty_image_data_does_not_mark_image_uploaded() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let mut placement = test_placement(0, 0);
        placement.placement.data.clear();

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        assert!(bytes.is_empty());
        assert!(images.is_empty());
        assert!(placements.is_empty());
    }

    #[test]
    fn same_image_signature_reuses_host_upload_across_source_image_ids() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);

        bytes.clear();
        let mut same_image_new_source_id = test_placement(0, 0);
        same_image_new_source_id.placement.image_id = 8;
        same_image_new_source_id.placement.placement_id = 4;
        same_image_new_source_id.placement.data.clear();
        encode_graphics_update(
            &mut bytes,
            &[same_image_new_source_id],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let reused = String::from_utf8_lossy(&bytes);
        assert!(!reused.contains("a=t"));
        assert!(reused.contains("a=p"));
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn pane_layer_replacement_is_atomic_without_delete_to_blank() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let mut first = pane_layer_placement(0, 0);
        first.host_image_id = Some((1 << 31) | 7);
        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        bytes.clear();
        let mut changed = pane_layer_placement(0, 0);
        changed.host_image_id = Some((1 << 31) | 7);
        changed.placement.data_fingerprint += 1;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains("a=T,t=d"));
        assert!(update.contains(",p=") && update.contains(",C=1,q=2"));
        assert!(!update.contains("a=d"));
    }

    #[test]
    fn replaced_image_content_deletes_superseded_host_image() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        let superseded_host_id = *images.keys().next().expect("uploaded host image");

        // Same source image id, new pixel content: the fresh content maps to
        // a fresh host image id, so the replaced one must be deleted.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains("a=t"), "changed content re-uploads");
        assert!(
            update.contains(&format!("a=d,d=I,i={superseded_host_id}")),
            "superseded host image is deleted"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn shared_host_image_survives_while_another_source_references_it() {
        fn twin_placement() -> HostPlacement {
            let mut twin = test_placement(5, 5);
            twin.placement.image_id = 8;
            twin.placement.placement_id = 4;
            twin.source_key = HostSourceKey::Terminal {
                pane_id: twin.pane_id,
                image_id: twin.placement.image_id,
            };
            twin
        }

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0), twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1, "same content dedups to one host image");

        // One source moves to new content while the other still shows the
        // old image: the shared host image must survive.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed, twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(!update.contains("a=d,d=I"), "shared host image survives");
        assert_eq!(images.len(), 2);
    }

    #[test]
    fn stale_source_entry_does_not_block_superseded_image_delete() {
        fn twin_placement() -> HostPlacement {
            let mut twin = test_placement(5, 5);
            twin.placement.image_id = 8;
            twin.placement.placement_id = 4;
            twin.source_key = HostSourceKey::Terminal {
                pane_id: twin.pane_id,
                image_id: twin.placement.image_id,
            };
            twin
        }

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0), twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 2);
        let shared_host_id = *images.keys().next().expect("uploaded host image");

        // The twin source is gone and the survivor changed content: the
        // vanished source's stale entry must not keep the old host image
        // alive.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(
            update.contains(&format!("a=d,d=I,i={shared_host_id}")),
            "old host image is deleted once its last live source moves on"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn stale_placement_deletes_placement_not_image_immediately() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(placements.len(), 1);

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains("a=d,d=i"));
        assert!(!delete.contains("d=I"));
        assert!(placements.is_empty());
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn trusted_direct_image_uses_reserved_id_for_placement_without_upload() {
        let key = (PaneId::from_raw(1), "primary".to_owned());
        let layer = crate::app::pane_graphics::Layer::inline(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            30,
            30,
            vec![255; 30 * 30 * 4],
            Default::default(),
            0,
        );
        let reserved_id = (1 << 31) | 77;
        let mut cache = HostGraphicsCache::default();
        cache.trust_pane_layer(&key, reserved_id, &layer);
        let mut placement = pane_layer_placement(0, 0);
        placement.host_image_id = Some(reserved_id);
        placement.placement.data.clear();
        placement.placement.data_len = layer.data_len();
        placement.placement.data_fingerprint = layer.data_fingerprint;
        let mut bytes = Vec::new();

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains(&format!("a=p,i={reserved_id}")));
        assert!(!update.contains("a=t"));

        let live = HashSet::from([HostSourceKey::PaneLayer {
            pane_id: key.0,
            layer_id: key.1.clone(),
        }]);
        let hidden = String::from_utf8(cache.hide_except_live_pane_layers(&live)).unwrap();
        assert!(hidden.contains("a=d,d=i"));
        assert!(!hidden.contains("a=d,d=I"));
        assert!(cache.images.contains_key(&reserved_id));
        assert!(cache.placements.is_empty());

        let mut returning = pane_layer_placement(0, 0);
        returning.host_image_id = Some(reserved_id);
        returning.placement.data.clear();
        returning.placement.data_len = layer.data_len();
        returning.placement.data_fingerprint = layer.data_fingerprint;
        let mut replay = Vec::new();
        encode_graphics_update(
            &mut replay,
            &[returning],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );
        let replay = String::from_utf8(replay).unwrap();
        assert!(replay.contains(&format!("a=p,i={reserved_id}")));
        assert!(!replay.contains("a=t"));

        cache.forget_pane_layer(&key, reserved_id);
        let mut fallback = pane_layer_placement(0, 0);
        fallback.host_image_id = Some(reserved_id);
        let mut retransmit = Vec::new();
        encode_graphics_update(
            &mut retransmit,
            &[fallback],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );
        assert!(String::from_utf8_lossy(&retransmit).contains("a=t"));
    }

    #[test]
    fn hidden_layer_and_full_redraw_replay_placement_without_pixels() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = pane_layer_placement(0, 0);
        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        for (visible, replay) in [(false, false), (true, false), (true, true)] {
            bytes.clear();
            let current = visible.then(|| pane_layer_placement(0, 0));
            encode_graphics_update(
                &mut bytes,
                current.as_slice(),
                replay,
                &mut images,
                &mut placements,
                &mut sources,
            );
            let update = String::from_utf8_lossy(&bytes);
            assert!(!update.contains("a=t"));
            assert!(!update.contains("a=d,d=I"));
            assert_eq!(update.contains("a=p"), visible);
        }
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn removed_pane_layer_deletes_unreferenced_host_image() {
        let mut cache = HostGraphicsCache::default();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(0, 0)],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );
        let host_id = *cache.images.keys().next().expect("uploaded pane layer");

        bytes = drain_graphics_updates(&mut cache, &[], &HashSet::new());

        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains(&format!("a=d,d=I,i={host_id}")));
        assert!(cache.images.is_empty());
        assert!(cache.placements.is_empty());
        assert!(cache.sources.is_empty());
    }

    #[test]
    fn hidden_pane_layer_retains_image_and_removes_only_placement() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(0, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let host_id = *images.keys().next().expect("uploaded pane layer");

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(100, 100)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains("a=d,d=i"));
        assert!(!update.contains(&format!("a=d,d=I,i={host_id}")));
        assert_eq!(images.len(), 1);
        assert!(placements.is_empty());
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn clipped_terminal_source_retains_identity_for_later_content_replacement() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let original_host_id = *images.keys().next().expect("uploaded terminal image");

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[test_placement(100, 100)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);

        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains(&format!("a=d,d=I,i={original_host_id}")));
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn removed_pane_layer_preserves_image_shared_with_terminal_source() {
        let mut cache = HostGraphicsCache::default();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(0, 0), test_placement(4, 0)],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );
        assert_eq!(cache.images.len(), 1);

        bytes = drain_graphics_updates(&mut cache, &[], &HashSet::new());
        encode_graphics_update(
            &mut bytes,
            &[test_placement(4, 0)],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(!update.contains("a=d,d=I"));
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.placements.len(), 1);
        assert_eq!(cache.sources.len(), 1);
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
    fn terminal_only_headless_budget_does_not_let_large_image_starve_small_image() {
        let mut large = test_placement(0, 0);
        large.placement.data_len = 24 * 1024 * 1024;
        let mut small = test_placement(4, 0);
        small.placement.image_id = 8;
        small.source_key = HostSourceKey::Terminal {
            pane_id: small.pane_id,
            image_id: 8,
        };
        let small_source = small.source_key.clone();
        let mut cache = HostGraphicsCache::default();

        let large_only = encode_terminal_graphics_update(
            &mut cache,
            std::slice::from_ref(&large),
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(large_only.bytes.is_empty());
        assert_eq!(cache.oversized.len(), 1);
        assert!(cache.images.is_empty());

        let hidden = encode_terminal_graphics_update(
            &mut cache,
            &[],
            true,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(hidden.bytes.is_empty());
        assert_eq!(cache.oversized.len(), 1);

        let with_small = encode_terminal_graphics_update(
            &mut cache,
            &[large, small],
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(String::from_utf8_lossy(&with_small.bytes).contains("a=t"));
        assert!(with_small.bytes.len() <= HEADLESS_GRAPHICS_TRANSACTION_BUDGET);
        assert!(!with_small.incomplete);
        assert_eq!(cache.oversized.len(), 1);
        assert_eq!(cache.images.len(), 1);
        assert!(cache.sources.contains_key(&small_source));
    }

    /// A Unicode-placeholder image reaches `encode_terminal_graphics_update` as one
    /// placement per viewport row, because `kitty_virtual_image_placements` scans the
    /// viewport row by row. Build one image that covers `rows` rows that way.
    fn image_covering_rows(rows: usize) -> Vec<HostPlacement> {
        let cell_height = 10u32;
        let image_height = (rows as u32) * cell_height;
        let image_width = 30u32;
        let data_len = (image_width * image_height * 4) as usize;
        (0..rows)
            .map(|row| {
                let mut placement = test_placement(0, row as i32);
                placement.area = Rect::new(0, 0, 120, 60);
                placement.placement.placement_id = 100 + row as u32;
                placement.placement.image_width = image_width;
                placement.placement.image_height = image_height;
                placement.placement.data_len = data_len;
                placement.placement.data = vec![255; data_len];
                placement.placement.render.grid_rows = 1;
                placement.placement.render.source_y = (row as u32) * cell_height;
                placement.placement.render.source_height = cell_height;
                placement.placement.render.source_width = image_width;
                placement
            })
            .collect()
    }

    #[test]
    fn budgeted_image_rows_upload_in_one_transaction() {
        const IMAGE_ROWS: usize = 23;
        let placements = image_covering_rows(IMAGE_ROWS);
        let mut cache = HostGraphicsCache::default();

        let upload = encode_terminal_graphics_update(
            &mut cache,
            &placements,
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(!upload.incomplete);
        let upload = String::from_utf8(upload.bytes).unwrap();
        assert_eq!(upload.matches("a=t").count(), 1);
        assert_eq!(upload.matches("a=p").count(), IMAGE_ROWS);
        assert_eq!(cache.images.len(), 1, "one image backs every row");
        assert_eq!(cache.placements.len(), IMAGE_ROWS, "one placement per row");
    }

    #[test]
    fn budgeted_image_rows_disappear_in_one_transaction() {
        const IMAGE_ROWS: usize = 23;
        let placements = image_covering_rows(IMAGE_ROWS);
        let mut cache = HostGraphicsCache::default();
        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                &placements,
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            if !encoded.incomplete {
                break;
            }
        }

        let removed = encode_terminal_graphics_update(
            &mut cache,
            &[],
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(!removed.incomplete);
        let removed = String::from_utf8(removed.bytes).unwrap();
        assert_eq!(removed.matches("a=d,d=i").count(), IMAGE_ROWS);
        assert!(cache.placements.is_empty());
    }

    #[test]
    fn budgeted_image_rows_delete_old_rows_together_before_replacement() {
        const IMAGE_ROWS: usize = 23;
        let old = image_covering_rows(IMAGE_ROWS);
        let mut cache = HostGraphicsCache::default();
        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                &old,
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            if !encoded.incomplete {
                break;
            }
        }

        let mut replacement = image_covering_rows(IMAGE_ROWS);
        for placement in &mut replacement {
            placement.placement.data_fingerprint += 1;
        }
        let cleanup = encode_terminal_graphics_update(
            &mut cache,
            &replacement,
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(cleanup.incomplete);
        let cleanup = String::from_utf8(cleanup.bytes).unwrap();
        assert_eq!(cleanup.matches("a=d,d=i").count(), IMAGE_ROWS);
        assert!(!cleanup.contains("a=t"));

        let replaced = encode_terminal_graphics_update(
            &mut cache,
            &replacement,
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(!replaced.incomplete);
        let replaced = String::from_utf8(replaced.bytes).unwrap();
        assert_eq!(replaced.matches("a=t").count(), 1);
        assert_eq!(replaced.matches("a=d,d=I").count(), 1);
        assert_eq!(replaced.matches("a=p").count(), IMAGE_ROWS);
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.placements.len(), IMAGE_ROWS);
    }

    #[test]
    fn budgeted_image_rows_redisplay_in_one_transaction() {
        const IMAGE_ROWS: usize = 23;
        let placements = image_covering_rows(IMAGE_ROWS);
        let mut cache = HostGraphicsCache::default();

        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                &placements,
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            if !encoded.incomplete {
                break;
            }
        }

        let replay = encode_terminal_graphics_update(
            &mut cache,
            &placements,
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(!replay.incomplete);
        let replay = String::from_utf8(replay.bytes).unwrap();
        assert!(!replay.contains("a=t"));
        assert_eq!(replay.matches("a=p").count(), IMAGE_ROWS);
    }

    #[test]
    fn budgeted_pixel_uploads_do_not_coalesce() {
        let first = test_placement(0, 0);
        let mut second = test_placement(4, 0);
        second.placement.image_id = 8;
        second.placement.placement_id = 4;
        second.placement.data_fingerprint = 43;
        second.source_key = HostSourceKey::Terminal {
            pane_id: second.pane_id,
            image_id: 8,
        };
        let placements = [first, second];
        let mut cache = HostGraphicsCache::default();

        let first_pass = encode_terminal_graphics_update(
            &mut cache,
            &placements,
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(
            first_pass.incomplete,
            "the second upload must wait for its own transaction"
        );
        let first_pass = String::from_utf8(first_pass.bytes).unwrap();
        assert_eq!(first_pass.matches("a=t").count(), 1);

        let second_pass = encode_terminal_graphics_update(
            &mut cache,
            &placements,
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(!second_pass.incomplete);
        let second_pass = String::from_utf8(second_pass.bytes).unwrap();
        assert_eq!(second_pass.matches("a=t").count(), 1);
        assert_eq!(cache.images.len(), 2);

        let replay = encode_terminal_graphics_update(
            &mut cache,
            &placements,
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(!replay.incomplete);
        let replay = String::from_utf8(replay.bytes).unwrap();
        assert!(!replay.contains("a=t"));
        assert_eq!(replay.matches("a=p").count(), 2);
    }

    #[test]
    fn budgeted_redisplay_coalescing_respects_budget() {
        let placements = image_covering_rows(2);
        let mut cache = HostGraphicsCache::default();
        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                &placements,
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            if !encoded.incomplete {
                break;
            }
        }

        // A budget that admits one re-display command but not two.
        let tight = encode_terminal_graphics_update(&mut cache, &placements, false, Some(100));
        assert!(tight.incomplete);
        let tight = String::from_utf8(tight.bytes).unwrap();
        assert!(!tight.contains("a=t"));
        assert_eq!(tight.matches("a=p").count(), 1);

        let rest = encode_terminal_graphics_update(&mut cache, &placements, false, Some(100));
        assert!(!rest.incomplete);
        let rest = String::from_utf8(rest.bytes).unwrap();
        assert!(!rest.contains("a=t"));
        assert_eq!(rest.matches("a=p").count(), 1);
    }

    #[test]
    fn budgeted_upload_keeps_other_redisplays_out() {
        let fresh = test_placement(0, 0);
        let mut cached = test_placement(4, 0);
        cached.placement.image_id = 8;
        cached.placement.placement_id = 4;
        cached.placement.data_fingerprint = 43;
        cached.source_key = HostSourceKey::Terminal {
            pane_id: cached.pane_id,
            image_id: 8,
        };
        let mut cache = HostGraphicsCache::default();
        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                std::slice::from_ref(&cached),
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            if !encoded.incomplete {
                break;
            }
        }

        // A fresh image followed by the cached image at a new position.
        cached.placement.render.viewport_col = 8;
        let placements = [fresh, cached];
        let mut passes = 0;
        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                &placements,
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            passes += 1;
            assert!(passes <= 4, "did not converge");
            let bytes = String::from_utf8(encoded.bytes).unwrap();
            if bytes.contains("a=t") {
                assert_eq!(
                    bytes.matches("a=p").count(),
                    1,
                    "an upload carries only its own placement"
                );
            }
            if !encoded.incomplete {
                break;
            }
        }
        assert_eq!(cache.images.len(), 2);
    }

    #[test]
    fn budgeted_superseded_image_delete_does_not_coalesce() {
        fn pair() -> [HostPlacement; 2] {
            let first = test_placement(0, 0);
            let mut second = test_placement(4, 0);
            second.placement.image_id = 8;
            second.placement.placement_id = 4;
            second.placement.data_fingerprint = 43;
            second.source_key = HostSourceKey::Terminal {
                pane_id: second.pane_id,
                image_id: 8,
            };
            [first, second]
        }
        let mut cache = HostGraphicsCache::default();
        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                &pair(),
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            if !encoded.incomplete {
                break;
            }
        }
        assert_eq!(cache.images.len(), 2);

        // The first source now shows the second image's content, so its old
        // image gets released, while the second placement moves.
        let [mut first, mut second] = pair();
        first.placement.data_fingerprint = 43;
        second.placement.render.viewport_col = 8;
        let placements = [first, second];
        let mut passes = 0;
        let mut saw_release = false;
        loop {
            let encoded = encode_terminal_graphics_update(
                &mut cache,
                &placements,
                false,
                Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            );
            passes += 1;
            assert!(passes <= 6, "did not converge");
            let bytes = String::from_utf8(encoded.bytes).unwrap();
            if bytes.contains("a=d,d=I") {
                saw_release = true;
                assert!(
                    bytes.matches("a=p").count() <= 1,
                    "a superseded-image delete carries at most its own placement"
                );
            }
            if !encoded.incomplete {
                break;
            }
        }
        assert!(saw_release, "the old image was released");
        assert_eq!(cache.images.len(), 1);
    }

    #[test]
    fn budgeted_pane_cleanup_precedes_terminal_image_upload() {
        let mut cache = HostGraphicsCache::default();
        let pane_source = HostSourceKey::PaneLayer {
            pane_id: PaneId::from_raw(1),
            layer_id: "primary".into(),
        };
        cache.sources.insert(pane_source, 99);
        cache.images.insert(
            99,
            ImageSignature {
                image_width: 30,
                image_height: 30,
                format_code: 32,
                data_len: 30 * 30 * 4,
                data_fingerprint: 9,
            },
        );
        let terminal = test_placement(0, 0);

        let cleanup = encode_terminal_graphics_update(
            &mut cache,
            std::slice::from_ref(&terminal),
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(cleanup.incomplete);
        let cleanup = String::from_utf8(cleanup.bytes).unwrap();
        assert!(cleanup.contains("a=d,d=I,i=99"));
        assert!(!cleanup.contains("a=t"));
        assert!(cache.images.is_empty());

        let upload = encode_terminal_graphics_update(
            &mut cache,
            &[terminal],
            false,
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
        );
        assert!(String::from_utf8_lossy(&upload.bytes).contains("a=t"));
    }

    #[test]
    fn terminal_only_high_level_path_preserves_budget_quarantine() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = crate::workspace::Workspace::test_new("graphics-budget-dispatch");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 24));

        let source = HostSourceKey::Terminal {
            pane_id,
            image_id: 7,
        };
        let mut cache = HostGraphicsCache::default();
        cache.quarantine_oversized(
            source.clone(),
            ImageSignature {
                image_width: 3456,
                image_height: 2234,
                format_code: 32,
                data_len: 3456 * 2234 * 4,
                data_fingerprint: 42,
            },
        );

        let encoded = encode_local_pane_graphics(
            &app,
            &crate::app::pane_graphics::Runtime::default(),
            &TerminalRuntimeRegistry::new(),
            app.view.tab_surface(),
            HostCellSize {
                width_px: 10,
                height_px: 20,
            },
            Some(HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            &mut cache,
        );

        assert!(encoded.bytes.is_empty());
        assert!(cache.oversized.contains_key(&source));
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
    fn terminal_quarantine_survives_pane_layer_cleanup_and_stays_bounded() {
        let terminal_source = HostSourceKey::Terminal {
            pane_id: PaneId::from_raw(1),
            image_id: 7,
        };
        let signature = ImageSignature {
            image_width: 3456,
            image_height: 2234,
            format_code: 32,
            data_len: 3456 * 2234 * 4,
            data_fingerprint: 42,
        };
        let pane_source = HostSourceKey::PaneLayer {
            pane_id: PaneId::from_raw(1),
            layer_id: "primary".into(),
        };
        let mut cache = HostGraphicsCache::default();
        cache.quarantine_oversized(terminal_source.clone(), signature);
        cache.sources.insert(pane_source, 99);
        cache.images.insert(99, signature);

        let cleared = String::from_utf8(cache.clear_pane_sources()).unwrap();
        assert!(cleared.contains("a=d,d=I,i=99"));
        assert!(cache.oversized.contains_key(&terminal_source));

        for image_id in 0..=MAX_OVERSIZED_SOURCES as u32 {
            cache.quarantine_oversized(
                HostSourceKey::Terminal {
                    pane_id: PaneId::from_raw(2),
                    image_id,
                },
                ImageSignature {
                    data_fingerprint: u64::from(image_id),
                    ..signature
                },
            );
        }
        assert_eq!(cache.oversized.len(), MAX_OVERSIZED_SOURCES);
        cache.clear_bytes();
        assert!(cache.oversized.is_empty());
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
