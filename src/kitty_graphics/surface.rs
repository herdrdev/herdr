use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::output::GraphicsOutput;

use ratatui::layout::Rect;

use super::{
    clipped_placement, collect_visible_placements, encode_graphics_output_incremental,
    HostCellSize, HostGraphicsCache, HostPlacement, HostSourceKey, ImageSignature,
};
use crate::ghostty::{
    KittyImageDescriptor, KittyImageFormat, KittyImagePlacement, KittyPlacementRenderInfo,
};
use crate::layout::PaneId;
use crate::protocol::{
    SurfaceGraphicsAsset, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat, SurfaceGraphicsPlacement,
    SurfaceGraphicsScene, SurfaceGraphicsSource, SurfaceGraphicsTarget,
};

const MAX_SURFACE_GRAPHICS_PLACEMENTS: usize = 4_096;

#[derive(Clone, Debug, Default)]
pub(crate) struct DeliveryCache {
    assets: HashSet<SurfaceGraphicsAssetKey>,
    pending: bool,
}

pub(crate) type SourceFiles =
    HashMap<SurfaceGraphicsAssetKey, Arc<crate::pane_graphics_files::OwnedExport>>;

impl DeliveryCache {
    pub(crate) fn forget_asset(&mut self, key: &SurfaceGraphicsAssetKey) {
        self.assets.remove(key);
        self.pending = true;
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.pending
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Visibility {
    Main,
    Popup,
    Hidden,
}

#[derive(Debug, Default)]
pub(crate) struct Occlusion {
    regions: Vec<Rect>,
    popup_start: usize,
}

impl Occlusion {
    pub(crate) fn cover(&mut self, rect: Rect) {
        if !rect.is_empty() {
            self.regions.push(rect);
        }
    }

    pub(crate) fn start_popup(&mut self, rect: Rect) {
        self.cover(rect);
        self.popup_start = self.regions.len();
    }

    fn covers(
        &self,
        placement: &SurfaceGraphicsPlacement,
        origin: (u16, u16),
        cell: HostCellSize,
    ) -> bool {
        let regions = if matches!(
            placement.asset.source,
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Popup { .. },
                ..
            }
        ) {
            &self.regions[self.popup_start..]
        } else {
            &self.regions
        };
        if regions.is_empty() {
            return false;
        }
        let width = u64::from(cell.width_px);
        let height = u64::from(cell.height_px);
        let x =
            u64::from(origin.0.saturating_add(placement.x)) * width + u64::from(placement.x_offset);
        let y = u64::from(origin.1.saturating_add(placement.y)) * height
            + u64::from(placement.y_offset);
        let right = x + u64::from(placement.cols) * width;
        let bottom = y + u64::from(placement.rows) * height;
        regions.iter().any(|rect| {
            x < u64::from(rect.right()) * width
                && right > u64::from(rect.x) * width
                && y < u64::from(rect.bottom()) * height
                && bottom > u64::from(rect.y) * height
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ClientState {
    scope: String,
    scene: SurfaceGraphicsScene,
    display_scene: SurfaceGraphicsScene,
    native_image_ids: HashMap<SurfaceGraphicsAssetKey, u32>,
    assets: HashMap<SurfaceGraphicsAssetKey, Arc<[u8]>>,
    host: HostGraphicsCache,
    trusted_direct: HashMap<SurfaceGraphicsAssetKey, u32>,
    reset_pending: bool,
    stale_images: Vec<u32>,
    forced_delete_images: Vec<u32>,
}

impl ClientState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn image_id(&self, key: &SurfaceGraphicsAssetKey) -> u32 {
        if matches!(key.source, SurfaceGraphicsSource::Terminal { .. }) {
            return self
                .native_image_ids
                .get(key)
                .copied()
                .unwrap_or_else(|| native_host_image_id(&self.scope, key));
        }
        host_image_id(&self.scope, key)
    }

    pub(crate) fn scope(&self) -> &str {
        &self.scope
    }

    pub(crate) fn set_scope(&mut self, scope: &str) {
        if self.scope == scope {
            return;
        }
        self.scope = scope.to_owned();
        self.scene = SurfaceGraphicsScene::default();
        self.display_scene = SurfaceGraphicsScene::default();
        self.native_image_ids.clear();
        self.assets.clear();
        self.trusted_direct.clear();
        self.stale_images.clear();
        self.forced_delete_images.clear();
        self.reset_pending = true;
    }

    #[cfg(unix)]
    pub(crate) fn accepts_direct_asset(
        &self,
        key: &SurfaceGraphicsAssetKey,
        image_id: u32,
    ) -> bool {
        if matches!(key.source, SurfaceGraphicsSource::Terminal { .. }) {
            return !self.stale_images.contains(&image_id)
                && !self.forced_delete_images.contains(&image_id)
                && (image_id == native_host_image_id(&self.scope, key)
                    || image_id == (native_host_image_id(&self.scope, key) ^ NATIVE_SLOT_BIT))
                && !self
                    .display_scene
                    .placements
                    .iter()
                    .map(|p| &p.asset)
                    .chain(self.display_scene.retained_assets.iter())
                    .any(|visible| {
                        self.image_id(visible) == image_id
                            && self.host.images.get(&image_id)
                                == Some(&image_signature_from_asset(visible))
                    })
                && (self
                    .scene
                    .placements
                    .iter()
                    .any(|placement| &placement.asset == key)
                    || self.scene.retained_assets.contains(key));
        }
        image_id == host_image_id(&self.scope, key)
    }

    #[cfg(unix)]
    pub(crate) fn trust_direct_asset(
        &mut self,
        key: &SurfaceGraphicsAssetKey,
        image_id: u32,
    ) -> bool {
        if !self.accepts_direct_asset(key, image_id) {
            return false;
        }
        if matches!(key.source, SurfaceGraphicsSource::Terminal { .. }) {
            self.native_image_ids.insert(key.clone(), image_id);
        }
        self.host
            .images
            .insert(image_id, image_signature_from_asset(key));
        self.refresh_display_scene();
        if self
            .scene
            .placements
            .iter()
            .all(|placement| &placement.asset != key)
            && !self.scene.retained_assets.contains(key)
        {
            self.trusted_direct.insert(key.clone(), image_id);
        }
        true
    }

    #[cfg(unix)]
    pub(crate) fn retire_direct_image(&mut self, image_id: u32) {
        self.trusted_direct
            .retain(|_, trusted| *trusted != image_id);
        self.forced_delete_images.push(image_id);
    }

    pub(crate) fn take_pending_cleanup(&mut self) -> Vec<u8> {
        let mut bytes = if self.reset_pending {
            self.reset_pending = false;
            self.stale_images.clear();
            self.host.clear_bytes()
        } else {
            Vec::new()
        };
        self.forced_delete_images.sort_unstable();
        self.forced_delete_images.dedup();
        for image_id in self.forced_delete_images.drain(..) {
            self.host.images.remove(&image_id);
            self.host.placements.retain(|(id, _), _| *id != image_id);
            self.host.sources.retain(|_, id| *id != image_id);
            self.host
                .replayed_placements
                .retain(|(id, _)| *id != image_id);
            super::encode_delete_image(&mut bytes, image_id);
        }
        bytes
    }

    pub(crate) fn set_scene(&mut self, mut scene: SurfaceGraphicsScene) {
        let desired = scene
            .placements
            .iter()
            .map(|placement| placement.asset.clone())
            .chain(scene.retained_assets.iter().cloned())
            .collect::<HashSet<_>>();
        let unclaimed = self
            .trusted_direct
            .iter()
            .filter_map(|(key, image_id)| (!desired.contains(key)).then_some(*image_id))
            .collect::<Vec<_>>();
        self.stale_images.extend(unclaimed);
        self.trusted_direct.clear();
        let placed = scene
            .placements
            .iter()
            .map(|placement| placement.asset.clone())
            .collect::<HashSet<_>>();
        for asset in std::mem::take(&mut scene.assets) {
            if asset.data.len() as u64 == asset.key.data_len && placed.contains(&asset.key) {
                self.assets.insert(asset.key, Arc::from(asset.data));
            }
        }
        self.scene = scene;
        self.refresh_display_scene();
    }

    fn refresh_display_scene(&mut self) {
        let mut next = self.scene.clone();
        let previous_placements = self
            .display_scene
            .placements
            .iter()
            .map(|p| ((&p.asset.source, p.logical_placement_id, p.x, p.y), p))
            .collect::<HashMap<_, _>>();
        let previous_assets = self
            .display_scene
            .retained_assets
            .iter()
            .chain(self.display_scene.placements.iter().map(|p| &p.asset))
            .filter(|key| {
                self.host.images.get(&self.image_id(key)) == Some(&image_signature_from_asset(key))
            })
            .map(|key| (&key.source, key))
            .collect::<HashMap<_, _>>();
        for placement in &mut next.placements {
            let key = &placement.asset;
            if !matches!(key.source, SurfaceGraphicsSource::Terminal { .. })
                || self.assets.contains_key(key)
                || self.host.images.get(&self.image_id(key))
                    == Some(&image_signature_from_asset(key))
            {
                continue;
            }
            // A metadata-only replacement is not visible until its upload is ACKed.
            // Reuse only an identical placement, never stale resize/scroll geometry.
            if let Some(previous) = previous_placements
                .get(&(
                    &key.source,
                    placement.logical_placement_id,
                    placement.x,
                    placement.y,
                ))
                .copied()
                .filter(|previous| {
                    if self.host.images.get(&self.image_id(&previous.asset))
                        != Some(&image_signature_from_asset(&previous.asset))
                    {
                        return false;
                    }
                    let mut candidate = (*previous).clone();
                    candidate.asset = key.clone();
                    candidate == *placement
                })
            {
                placement.asset = previous.asset.clone();
            }
        }
        for key in &mut next.retained_assets {
            if matches!(key.source, SurfaceGraphicsSource::Terminal { .. })
                && !self.assets.contains_key(key)
                && self.host.images.get(&self.image_id(key))
                    != Some(&image_signature_from_asset(key))
            {
                if let Some(previous) = previous_assets.get(&key.source) {
                    *key = (*previous).clone();
                }
            }
        }
        let desired = next
            .placements
            .iter()
            .map(|p| &p.asset)
            .chain(next.retained_assets.iter())
            .collect::<HashSet<_>>();
        let stale = self
            .display_scene
            .placements
            .iter()
            .map(|p| &p.asset)
            .chain(self.display_scene.retained_assets.iter())
            .filter(|key| !desired.contains(key))
            .map(|key| self.image_id(key))
            .collect::<Vec<_>>();
        self.stale_images.extend(stale);
        let owned = self
            .scene
            .placements
            .iter()
            .map(|p| &p.asset)
            .chain(self.scene.retained_assets.iter())
            .chain(desired.iter().copied())
            .collect::<HashSet<_>>();
        self.assets.retain(|key, _| owned.contains(key));
        self.native_image_ids.retain(|key, _| owned.contains(key));
        self.display_scene = next;
    }

    #[cfg(test)]
    pub(crate) fn encode(
        &mut self,
        visibility: Visibility,
        main_origin: (u16, u16),
        popup_origin: Option<(u16, u16)>,
        cell_size: HostCellSize,
        occlusion: &Occlusion,
    ) -> Vec<u8> {
        self.encode_output(visibility, main_origin, popup_origin, cell_size, occlusion)
            .into_inline_bytes()
    }

    pub(crate) fn encode_output(
        &mut self,
        visibility: Visibility,
        main_origin: (u16, u16),
        popup_origin: Option<(u16, u16)>,
        cell_size: HostCellSize,
        occlusion: &Occlusion,
    ) -> GraphicsOutput {
        let mut bytes = self.take_pending_cleanup();
        self.stale_images.sort_unstable();
        self.stale_images.dedup();
        for image_id in self.stale_images.drain(..) {
            if self.host.images.remove(&image_id).is_some() {
                super::encode_delete_image(&mut bytes, image_id);
            }
            self.host.placements.retain(|(id, _), _| *id != image_id);
            self.host.sources.retain(|_, id| *id != image_id);
            self.host
                .replayed_placements
                .retain(|(id, _)| *id != image_id);
        }
        if !cell_size.is_known() || self.scope.is_empty() {
            bytes.extend(self.host.clear_bytes());
            return GraphicsOutput::from_bytes(bytes);
        }

        let placements = self
            .display_scene
            .placements
            .iter()
            .filter_map(|placement| {
                client_host_placement(
                    &self.scope,
                    placement,
                    visibility,
                    main_origin,
                    popup_origin,
                    cell_size,
                    occlusion,
                )
                .map(|mut host| {
                    host.host_image_id = Some(self.image_id(&placement.asset));
                    host.raw_data = self.assets.get(&placement.asset).map(Arc::clone);
                    host
                })
            })
            .collect::<Vec<_>>();
        let mut output = GraphicsOutput::from_bytes(bytes);
        self.host.request_placement_replay();
        loop {
            let (encoded, incomplete) =
                encode_graphics_output_incremental(&mut self.host, &placements);
            output.extend(encoded);
            if !incomplete {
                return output;
            }
        }
    }
}

pub(crate) const NATIVE_TRANSFER_BIT: u64 = 1 << 63;
pub(crate) const NATIVE_SLOT_BIT: u32 = 1 << 29;

pub(crate) fn native_host_image_id(scope: &str, key: &SurfaceGraphicsAssetKey) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    key.source.hash(&mut hasher);
    // Keep native IDs separate from the direct API namespace.
    0x4000_0000 | (hasher.finish() as u32 & 0x1fff_ffff)
}

pub(crate) fn host_image_id(scope: &str, key: &SurfaceGraphicsAssetKey) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    key.hash(&mut hasher);
    10_000 + ((hasher.finish() as u32) % 900_000)
}

#[cfg(all(test, unix))]
pub(crate) fn direct_upload_control(scope: &str, key: &SurfaceGraphicsAssetKey) -> (u32, String) {
    let image_id = host_image_id(scope, key);
    (
        image_id,
        format!(
            "a=t,f={},s={},v={},i={image_id},q=0",
            format_code(key.format),
            key.image_width,
            key.image_height
        ),
    )
}

pub(crate) fn collect_scene(
    app: &crate::app::App,
    surface: crate::ui::TabSurfaceView<'_>,
    popup_content_size: Option<(u16, u16)>,
    cell_size: HostCellSize,
    delivered: &DeliveryCache,
    _client_id: u64,
) -> (SurfaceGraphicsScene, DeliveryCache, SourceFiles) {
    if !cell_size.is_known() {
        return (
            SurfaceGraphicsScene::default(),
            DeliveryCache::default(),
            SourceFiles::default(),
        );
    }
    let workspace_index = surface.target.map(|target| target.workspace_index);
    let mut targets = HashMap::new();
    let mut public_panes = HashMap::new();
    if let Some(workspace_index) = workspace_index {
        for pane in surface.pane_infos {
            if let Some(public_id) = app.public_pane_id(workspace_index, pane.id) {
                public_panes.insert(public_id.clone(), pane.id);
                targets.insert(pane.id, SurfaceGraphicsTarget::Pane { pane_id: public_id });
            }
        }
    }
    let popup_target = app.state.popup_pane.as_ref().map(|popup| {
        (
            popup.pane_id,
            SurfaceGraphicsTarget::Popup {
                terminal_id: popup.terminal_id.to_string(),
            },
        )
    });
    if let Some((pane_id, target)) = popup_target.as_ref() {
        targets.insert(*pane_id, target.clone());
    }

    // Reconstruct the small source-signature index expected by the collector.
    // This avoids copying already-delivered payloads while Ghostty remains the
    // authoritative image store.
    let mut delivered_terminal_images = HashMap::new();
    for key in &delivered.assets {
        let signature = image_signature_from_asset(key);
        match &key.source {
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane { pane_id },
                image_id,
            } => {
                if let Some(pane_id) = public_panes.get(pane_id) {
                    delivered_terminal_images.insert(
                        HostSourceKey::Terminal {
                            pane_id: *pane_id,
                            image_id: *image_id,
                        },
                        signature,
                    );
                }
            }
            // Frozen for old-server compatibility; current servers no longer collect layers.
            SurfaceGraphicsSource::PaneLayer { .. } => {}
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Popup { .. },
                ..
            } => {}
        }
    }
    let mut host_placements = collect_visible_placements(
        &app.state,
        &app.terminal_runtimes,
        surface,
        cell_size,
        &delivered_terminal_images,
    );

    if let (Some(popup), Some((width, height)), Some((_, target))) = (
        app.state.popup_pane.as_ref(),
        popup_content_size,
        popup_target.as_ref(),
    ) {
        if let Some(runtime) = app.terminal_runtimes.get(&popup.terminal_id) {
            let mut requested = HashSet::new();
            for placement in runtime.kitty_image_placements_with_data_filter(|descriptor| {
                let key = asset_key_from_descriptor(
                    SurfaceGraphicsSource::Terminal {
                        target: target.clone(),
                        image_id: descriptor.image_id,
                    },
                    descriptor,
                );
                !descriptor.source_file && !delivered.assets.contains(&key) && requested.insert(key)
            }) {
                host_placements.push(HostPlacement {
                    raw_data: None,
                    pane_id: popup.pane_id,
                    host_image_id: None,
                    area: Rect::new(0, 0, width, height),
                    cell_size,
                    source_key: HostSourceKey::Terminal {
                        pane_id: popup.pane_id,
                        image_id: placement.image_id,
                    },
                    placement,
                    scrollback_offset: runtime
                        .scroll_metrics()
                        .map(|metrics| metrics.offset_from_bottom as u32)
                        .unwrap_or(0),
                });
            }
        }
    }

    let mut placements = Vec::new();
    let mut asset_data = HashMap::<SurfaceGraphicsAssetKey, Vec<u8>>::new();
    let mut sources = SourceFiles::default();
    for mut placement in host_placements {
        if placements.len() == MAX_SURFACE_GRAPHICS_PLACEMENTS {
            break;
        }
        let Some(target) = targets.get(&placement.pane_id).cloned() else {
            continue;
        };
        let source = match &placement.source_key {
            HostSourceKey::Terminal { image_id, .. } => SurfaceGraphicsSource::Terminal {
                target,
                image_id: *image_id,
            },
            HostSourceKey::ClientSurface { .. } => continue,
        };
        let Some((clipped, _)) = clipped_placement(&placement) else {
            continue;
        };
        let asset = asset_key(source, &placement.placement);
        if !delivered.assets.contains(&asset) {
            if let Some(file) = placement.placement.source_file.take() {
                sources.entry(asset.clone()).or_insert(file);
                asset_data.entry(asset.clone()).or_default();
            }
        }
        if !placement.placement.data.is_empty() {
            asset_data
                .entry(asset.clone())
                .or_insert_with(|| std::mem::take(&mut placement.placement.data));
        }
        placements.push(SurfaceGraphicsPlacement {
            asset,
            logical_placement_id: placement.placement.placement_id,
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
            z: placement.placement.z,
            scrollback_offset: placement.scrollback_offset,
        });
    }

    let desired = placements
        .iter()
        .map(|placement| placement.asset.clone())
        .collect::<HashSet<_>>();
    let mut next = DeliveryCache {
        assets: delivered.assets.intersection(&desired).cloned().collect(),
        pending: false,
    };
    let mut assets = Vec::new();
    let mut available = asset_data.into_iter().collect::<Vec<_>>();
    available.sort_by_key(|(key, _)| format!("{:?}", key.source));
    let mut payload_bytes = 0usize;
    for (key, data) in available {
        if next.assets.contains(&key) {
            continue;
        }
        let encoded_size = super::image_transfer_estimated_size(
            usize::try_from(key.data_len).unwrap_or(usize::MAX),
        );
        if encoded_size > super::HEADLESS_GRAPHICS_TRANSACTION_BUDGET {
            continue;
        }
        if payload_bytes.saturating_add(encoded_size) > super::HEADLESS_GRAPHICS_TRANSACTION_BUDGET
        {
            next.pending = true;
            continue;
        }
        payload_bytes = payload_bytes.saturating_add(encoded_size);
        assets.push(SurfaceGraphicsAsset {
            key: key.clone(),
            data,
        });
        next.assets.insert(key);
    }
    assets.sort_by_key(|asset| format!("{:?}", asset.key.source));
    placements.sort_by_key(|placement| {
        (
            format!("{:?}", placement.asset.source),
            placement.logical_placement_id,
            placement.y,
            placement.x,
        )
    });
    let retained_assets = Vec::new();
    sources.retain(|key, _| next.assets.contains(key));
    (
        SurfaceGraphicsScene {
            assets,
            placements,
            retained_assets,
        },
        next,
        sources,
    )
}

fn image_signature_from_asset(key: &SurfaceGraphicsAssetKey) -> ImageSignature {
    ImageSignature {
        image_width: key.image_width,
        image_height: key.image_height,
        format_code: format_code(key.format),
        data_len: usize::try_from(key.data_len).unwrap_or(usize::MAX),
        data_fingerprint: key.data_fingerprint,
    }
}

fn asset_key_from_descriptor(
    source: SurfaceGraphicsSource,
    descriptor: KittyImageDescriptor,
) -> SurfaceGraphicsAssetKey {
    SurfaceGraphicsAssetKey {
        source,
        image_width: descriptor.image_width,
        image_height: descriptor.image_height,
        format: match descriptor.format {
            KittyImageFormat::Rgb => SurfaceGraphicsFormat::Rgb,
            KittyImageFormat::Rgba => SurfaceGraphicsFormat::Rgba,
            KittyImageFormat::Png => SurfaceGraphicsFormat::Png,
        },
        data_len: descriptor.data_len as u64,
        data_fingerprint: descriptor.data_fingerprint,
    }
}

fn asset_key(
    source: SurfaceGraphicsSource,
    placement: &KittyImagePlacement,
) -> SurfaceGraphicsAssetKey {
    SurfaceGraphicsAssetKey {
        source,
        image_width: placement.image_width,
        image_height: placement.image_height,
        format: match placement.format {
            KittyImageFormat::Rgb => SurfaceGraphicsFormat::Rgb,
            KittyImageFormat::Rgba => SurfaceGraphicsFormat::Rgba,
            KittyImageFormat::Png => SurfaceGraphicsFormat::Png,
        },
        data_len: placement.data_len as u64,
        data_fingerprint: placement.data_fingerprint,
    }
}

fn client_host_placement(
    scope: &str,
    placement: &SurfaceGraphicsPlacement,
    visibility: Visibility,
    main_origin: (u16, u16),
    popup_origin: Option<(u16, u16)>,
    cell_size: HostCellSize,
    occlusion: &Occlusion,
) -> Option<HostPlacement> {
    let origin = match (&placement.asset.source, visibility) {
        (
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Popup { .. },
                ..
            },
            Visibility::Popup,
        ) => popup_origin?,
        (
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane { .. },
                ..
            },
            Visibility::Main | Visibility::Popup,
        )
        | (SurfaceGraphicsSource::PaneLayer { .. }, Visibility::Main | Visibility::Popup) => {
            main_origin
        }
        _ => return None,
    };
    if occlusion.covers(placement, origin, cell_size) {
        return None;
    }
    let source_key = HostSourceKey::ClientSurface {
        scope: scope.to_owned(),
        source: placement.asset.source.clone(),
    };
    let signature = ImageSignature {
        image_width: placement.asset.image_width,
        image_height: placement.asset.image_height,
        format_code: format_code(placement.asset.format),
        data_len: usize::try_from(placement.asset.data_len).unwrap_or(usize::MAX),
        data_fingerprint: placement.asset.data_fingerprint,
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    placement.asset.source.hash(&mut hasher);
    signature.hash(&mut hasher);
    let raw = hasher.finish();
    let pane_id = PaneId::from_raw((raw as u32).max(1));
    let host_image_id = host_image_id(scope, &placement.asset);
    let cols = placement.cols.min(u32::from(u16::MAX)) as u16;
    let rows = placement.rows.min(u32::from(u16::MAX)) as u16;
    Some(HostPlacement {
        raw_data: None,
        pane_id,
        host_image_id: Some(host_image_id),
        area: Rect::new(
            origin.0.saturating_add(placement.x),
            origin.1.saturating_add(placement.y),
            cols,
            rows,
        ),
        cell_size,
        source_key,
        placement: KittyImagePlacement {
            image_id: 1,
            placement_id: placement.logical_placement_id,
            z: placement.z,
            x_offset: placement.x_offset,
            y_offset: placement.y_offset,
            image_width: placement.asset.image_width,
            image_height: placement.asset.image_height,
            format: match placement.asset.format {
                SurfaceGraphicsFormat::Rgb => KittyImageFormat::Rgb,
                SurfaceGraphicsFormat::Rgba => KittyImageFormat::Rgba,
                SurfaceGraphicsFormat::Png => KittyImageFormat::Png,
            },
            data_len: usize::try_from(placement.asset.data_len).unwrap_or(usize::MAX),
            data_fingerprint: placement.asset.data_fingerprint,
            source_file: None,
            data: Vec::new(),
            render: KittyPlacementRenderInfo {
                pixel_width: placement.cols.saturating_mul(cell_size.width_px),
                pixel_height: placement.rows.saturating_mul(cell_size.height_px),
                grid_cols: placement.cols,
                grid_rows: placement.rows,
                viewport_col: 0,
                viewport_row: 0,
                source_x: placement.source_x,
                source_y: placement.source_y,
                source_width: placement.source_width,
                source_height: placement.source_height,
            },
        },
        scrollback_offset: placement.scrollback_offset,
    })
}

fn format_code(format: SurfaceGraphicsFormat) -> u32 {
    match format {
        SurfaceGraphicsFormat::Rgb => 24,
        SurfaceGraphicsFormat::Rgba => 32,
        SurfaceGraphicsFormat::Png => 100,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(
        target: SurfaceGraphicsTarget,
        fingerprint: u64,
        data: Vec<u8>,
    ) -> SurfaceGraphicsAsset {
        SurfaceGraphicsAsset {
            key: SurfaceGraphicsAssetKey {
                source: SurfaceGraphicsSource::Terminal {
                    target,
                    image_id: 7,
                },
                image_width: 1,
                image_height: 1,
                format: SurfaceGraphicsFormat::Rgba,
                data_len: data.len() as u64,
                data_fingerprint: fingerprint,
            },
            data,
        }
    }

    fn scene(asset: SurfaceGraphicsAsset, x: u16, y: u16) -> SurfaceGraphicsScene {
        SurfaceGraphicsScene {
            placements: vec![SurfaceGraphicsPlacement {
                asset: asset.key.clone(),
                logical_placement_id: 3,
                x,
                y,
                cols: 1,
                rows: 1,
                source_x: 0,
                source_y: 0,
                source_width: 1,
                source_height: 1,
                x_offset: 0,
                y_offset: 0,
                z: 0,
                scrollback_offset: 0,
            }],
            assets: vec![asset],
            retained_assets: Vec::new(),
        }
    }

    #[test]
    fn legacy_pane_layer_scene_still_uploads_and_places() {
        let mut state = ClientState::default();
        state.set_scope("old-server-pane-layer");
        let mut image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![1, 2, 3, 255],
        );
        image.key.source = SurfaceGraphicsSource::PaneLayer {
            pane_id: "pane".into(),
            layer_id: "primary".into(),
        };
        state.set_scene(scene(image, 2, 1));

        let bytes = String::from_utf8(state.encode(
            Visibility::Main,
            (10, 5),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        ))
        .unwrap();

        assert!(bytes.contains("a=t,t=d"), "{bytes:?}");
        assert!(bytes.contains("a=p,"), "{bytes:?}");
        assert!(bytes.contains("\u{1b}[7;13H"), "{bytes:?}");
    }

    #[test]
    fn stable_ids_replace_native_pixels_and_delete_on_removal() {
        use crate::kitty_graphics::GraphicsOperation;
        let mut state = ClientState::default();
        state.set_scope("stable-native");
        let target = SurfaceGraphicsTarget::Pane {
            pane_id: "pane".into(),
        };
        let first = asset(target.clone(), 1, vec![1, 2, 3, 255]);
        let id = state.image_id(&first.key);
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        for revision in 1..=8 {
            let next = asset(target.clone(), revision, vec![revision as u8, 2, 3, 255]);
            assert_eq!(state.image_id(&next.key), id);
            state.set_scene(scene(next, 0, 0));
            let output =
                state.encode_output(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
            let uploads = output
                .operations
                .iter()
                .filter_map(|op| match op {
                    GraphicsOperation::Upload { control, data } => Some((control, data)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(uploads.len(), 1);
            assert!(uploads[0].0.contains(&format!("i={id},")));
            assert_eq!(uploads[0].1.as_ref(), &[revision as u8, 2, 3, 255]);
            let bytes = String::from_utf8(output.into_inline_bytes()).unwrap();
            let upload = bytes.find("a=t,").unwrap();
            let display = bytes.find("a=p,").unwrap();
            assert!(upload < display);
            if revision > 1 {
                assert!(bytes.find("a=d,d=I,").unwrap() < upload);
            }
            assert_eq!(state.host.images.len(), 1);
        }
        let mut other = first.key.clone();
        other.source = SurfaceGraphicsSource::Terminal {
            target: SurfaceGraphicsTarget::Pane {
                pane_id: "other-pane".into(),
            },
            image_id: 7,
        };
        assert_ne!(state.image_id(&other), id);
        other.source = SurfaceGraphicsSource::PaneLayer {
            pane_id: "pane".into(),
            layer_id: "layer".into(),
        };
        assert_eq!(state.image_id(&other), host_image_id(state.scope(), &other));
        let legacy_id = state.image_id(&other);
        other.data_fingerprint += 1;
        assert_ne!(state.image_id(&other), legacy_id);
        state.set_scene(SurfaceGraphicsScene::default());
        let bytes = state.encode(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        assert!(String::from_utf8(bytes)
            .unwrap()
            .contains(&format!("a=d,d=I,i={id}")));
        assert!(state.host.images.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn native_file_adoption_keeps_previous_frame_until_replacement_ack() {
        let mut state = ClientState::default();
        state.set_scope("native-files");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "p1".into(),
            },
            1,
            vec![1, 2, 3, 255],
        );
        let id = native_host_image_id(state.scope(), &image.key);
        assert!(!state.accepts_direct_asset(&image.key, id));
        let mut metadata = scene(image.clone(), 0, 0);
        metadata.assets.clear();
        state.set_scene(metadata);
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        state.encode(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        assert!(state.trust_direct_asset(&image.key, id));
        let first = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(first.contains("a=p,"));
        assert!(!first.contains("a=t,"));
        let mut next = image.clone();
        next.key.data_fingerprint += 1;
        let mut metadata = scene(next.clone(), 0, 0);
        metadata.assets.clear();
        state.set_scene(metadata);
        assert!(!state.trust_direct_asset(&image.key, id));
        let cleanup = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(!cleanup.contains("a=d,"));
        assert!(cleanup.contains(&format!("a=p,i={id},")));
        assert!(!state.accepts_direct_asset(&next.key, id));
        let replacement_id = id ^ NATIVE_SLOT_BIT;
        assert!(state.trust_direct_asset(&next.key, replacement_id));
        let placed = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(placed.contains(&format!("a=p,i={replacement_id},")));
        assert!(placed.contains(&format!("a=d,d=I,i={id},")));
        assert!(!placed.contains("a=t,"));
        assert_eq!(state.host.images.len(), 1);
        assert_eq!(state.native_image_ids.len(), 1);
        // The vacated bank is reusable, but the visible bank cannot be overwritten.
        assert!(state.accepts_direct_asset(&next.key, id));
        assert!(!state.accepts_direct_asset(&next.key, replacement_id));
        assert!(!state.accepts_direct_asset(&next.key, host_image_id(state.scope(), &next.key)));
    }

    #[cfg(unix)]
    #[test]
    fn native_replacements_use_two_ids_without_blank_pending_frames() {
        let mut state = ClientState::default();
        state.set_scope("double-buffer");
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let mut previous = None;
        let mut ids = HashSet::new();
        for revision in 0..32 {
            let image = asset(
                SurfaceGraphicsTarget::Pane {
                    pane_id: "pane".into(),
                },
                revision,
                vec![revision as u8, 2, 3, 255],
            );
            let base = native_host_image_id(state.scope(), &image.key);
            let id = if revision % 2 == 0 {
                base ^ NATIVE_SLOT_BIT
            } else {
                base
            };
            ids.insert(id);
            let mut metadata = scene(image.clone(), 0, 0);
            metadata.assets.clear();
            state.set_scene(metadata.clone());
            for _ in 0..3 {
                state.set_scene(metadata.clone());
                let pending = String::from_utf8(state.encode(
                    Visibility::Main,
                    (0, 0),
                    None,
                    cell,
                    &Occlusion::default(),
                ))
                .unwrap();
                assert!(!pending.contains("a=d,"), "{pending:?}");
                if let Some(previous) = previous {
                    assert!(pending.contains(&format!("a=p,i={previous},")));
                    assert!(!state.accepts_direct_asset(&image.key, previous));
                }
            }
            assert!(state.trust_direct_asset(&image.key, id));
            let swapped = String::from_utf8(state.encode(
                Visibility::Main,
                (0, 0),
                None,
                cell,
                &Occlusion::default(),
            ))
            .unwrap();
            assert!(swapped.contains(&format!("a=p,i={id},")));
            assert!(!swapped.contains(&format!("a=d,d=I,i={id},")));
            if let Some(previous) = previous {
                assert!(swapped.contains(&format!("a=d,d=I,i={previous},")));
            }
            assert_eq!(state.host.images.len(), 1);
            assert_eq!(state.native_image_ids.len(), 1);
            assert!(!state.accepts_direct_asset(&image.key, id));
            assert_eq!(id & 0x8000_0000, 0, "native IDs must not alias API IDs");
            previous = Some(id);
        }
        assert_eq!(ids.len(), 2);
        state.set_scene(SurfaceGraphicsScene::default());
        let cleanup = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(cleanup.contains(&format!("a=d,d=I,i={},", previous.unwrap())));
        assert!(state.host.images.is_empty());
        assert!(state.native_image_ids.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn native_pending_replacement_failure_and_geometry_do_not_preserve_stale_frames() {
        let mut state = ClientState::default();
        state.set_scope("replacement-fallback");
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![1, 2, 3, 255],
        );
        let base = native_host_image_id(state.scope(), &image.key);
        let active = base ^ NATIVE_SLOT_BIT;
        let mut metadata = scene(image.clone(), 0, 0);
        metadata.assets.clear();
        state.set_scene(metadata);
        state.encode(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        assert!(state.trust_direct_asset(&image.key, active));
        state.encode(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        let mut next = image.clone();
        next.key.data_fingerprint += 1;
        let mut metadata = scene(next.clone(), 0, 0);
        metadata.assets.clear();
        state.set_scene(metadata);
        state.retire_direct_image(base);
        let rejected = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(!rejected.contains(&format!("a=d,d=I,i={active},")));
        assert!(rejected.contains(&format!("a=p,i={active},")));
        state.set_scene(scene(next.clone(), 0, 0));
        let fallback = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(fallback.contains("a=t,"));
        assert!(fallback.contains(&format!("a=p,i={base},")));
        assert!(fallback.contains(&format!("a=d,d=I,i={active},")));
        assert!(!state.accepts_direct_asset(&next.key, base));
        next.key.data_fingerprint += 1;
        assert!(!state.accepts_direct_asset(&next.key, base));
        let mut moved = scene(next.clone(), 1, 0);
        moved.assets.clear();
        state.set_scene(moved);
        let geometry = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(geometry.contains(&format!("a=d,d=I,i={base},")));
        assert!(!geometry.contains("a=p,"));
        state.set_scene(SurfaceGraphicsScene::default());
        assert!(!state.trust_direct_asset(&next.key, active));
    }

    #[test]
    fn deferred_client_reuses_resident_arc_and_orders_cleanup_before_upload() {
        use super::super::output::GraphicsOperation;
        let mut state = ClientState::default();
        state.set_scope("raw");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![1, 2, 3, 4],
        );
        let key = image.key.clone();
        let mut graphics = scene(image, 0, 0);
        let mut second = graphics.placements[0].clone();
        second.logical_placement_id += 1;
        second.x = 2;
        graphics.placements.push(second);
        let mut replacement = graphics.clone();
        replacement.assets.clear();
        state.set_scene(graphics);
        let resident = Arc::clone(&state.assets[&key]);
        state.set_scene(replacement);
        assert!(Arc::ptr_eq(&resident, &state.assets[&key]));
        state.forced_delete_images.push(4242);
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let output =
            state.encode_output(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        assert!(
            matches!(&output.operations[0], GraphicsOperation::Bytes(bytes)
            if String::from_utf8_lossy(bytes).contains("a=d,d=I,i=4242"))
        );
        let uploads = output
            .operations
            .iter()
            .filter_map(|op| match op {
                GraphicsOperation::Upload { data, .. } => Some(data),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(uploads.len(), 1);
        assert!(Arc::ptr_eq(uploads[0], &resident));
        assert_eq!(
            String::from_utf8(output.clone().into_inline_bytes())
                .unwrap()
                .matches("a=p")
                .count(),
            2
        );
        let replay =
            state.encode_output(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        assert!(replay
            .operations
            .iter()
            .all(|op| matches!(op, GraphicsOperation::Bytes(_))));
    }

    #[test]
    fn occlusion_uses_final_pixel_bounds_and_strict_overlap() {
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![1, 2, 3, 4],
        );
        let mut placement = scene(image, 1, 2).placements.remove(0);
        let mut cover = Occlusion::default();
        cover.cover(Rect::new(12, 8, 1, 1));
        // The image at (11, 7) just touches the cover's top-left corner.
        assert!(!cover.covers(&placement, (10, 5), cell));
        placement.x_offset = 1;
        assert!(!cover.covers(&placement, (10, 5), cell));
        placement.y_offset = 1;
        assert!(cover.covers(&placement, (10, 5), cell));
        placement.x_offset = 0;
        assert!(!cover.covers(&placement, (10, 5), cell));
        placement.cols = 2;
        assert!(cover.covers(&placement, (10, 5), cell));
        assert!(!cover.covers(&placement, (0, 0), cell));
        cover = Occlusion::default();
        cover.cover(Rect::new(11, 7, 0, 1));
        assert!(!cover.covers(&placement, (10, 5), cell));
    }

    #[test]
    fn occlusion_keeps_shared_asset_placements_and_restores_without_upload() {
        for direct in [false, true] {
            let mut state = ClientState::default();
            state.set_scope("occlusion");
            let _ = state.take_pending_cleanup();
            let image = asset(
                SurfaceGraphicsTarget::Pane {
                    pane_id: "pane".into(),
                },
                1,
                vec![1, 2, 3, 4],
            );
            let id = state.image_id(&image.key);
            let mut graphics = scene(image.clone(), 0, 0);
            let mut second = graphics.placements[0].clone();
            second.logical_placement_id = 4;
            second.x = 5;
            graphics.placements.push(second);
            if direct {
                // Model an already uploaded asset, including on hosts without direct transport.
                graphics.assets.clear();
                state.host.images.insert(
                    id,
                    ImageSignature {
                        image_width: 1,
                        image_height: 1,
                        format_code: 32,
                        data_len: 4,
                        data_fingerprint: 1,
                    },
                );
            }
            state.set_scene(graphics);
            let cell = HostCellSize {
                width_px: 8,
                height_px: 16,
            };
            let _ = state.encode(Visibility::Main, (10, 5), None, cell, &Occlusion::default());
            assert_eq!(state.host.placements.len(), 2);
            let mut cover = Occlusion::default();
            cover.cover(Rect::new(10, 5, 1, 1));
            let hidden = state.encode(Visibility::Main, (10, 5), None, cell, &cover);
            let hidden = String::from_utf8_lossy(&hidden);
            assert!(hidden.contains("a=d,d=i"), "{hidden}");
            assert!(!hidden.contains("a=d,d=I"), "{hidden}");
            assert!(hidden.contains("\u{1b}[6;16H"), "{hidden}");
            assert_eq!(state.host.placements.len(), 1);
            cover.cover(Rect::new(15, 5, 1, 1));
            let _ = state.encode(Visibility::Main, (10, 5), None, cell, &cover);
            assert!(state.host.placements.is_empty());
            assert!(state.host.images.contains_key(&id));
            let restored =
                state.encode(Visibility::Main, (10, 5), None, cell, &Occlusion::default());
            let restored = String::from_utf8_lossy(&restored);
            assert!(restored.contains("a=p"), "{restored}");
            assert!(!restored.contains("a=t"), "{restored}");
            assert_eq!(state.host.placements.len(), 2);
        }
    }

    #[test]
    fn popup_occlusion_respects_draw_order_and_its_own_images() {
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![1, 2, 3, 4],
        );
        let main = scene(image, 0, 0).placements.remove(0);
        let image = asset(
            SurfaceGraphicsTarget::Popup {
                terminal_id: "popup".into(),
            },
            2,
            vec![4, 3, 2, 1],
        );
        let popup = scene(image, 0, 0).placements.remove(0);
        let mut cover = Occlusion::default();
        cover.cover(Rect::new(10, 5, 3, 3));
        cover.start_popup(Rect::new(9, 4, 5, 5));
        assert!(cover.covers(&main, (10, 5), cell));
        assert!(!cover.covers(&main, (0, 0), cell));
        assert!(!cover.covers(&popup, (10, 5), cell));
        cover.cover(Rect::new(10, 5, 1, 1));
        assert!(cover.covers(&popup, (10, 5), cell));
    }

    #[test]
    #[ignore = "manual image placement overlap scaling profile"]
    fn graphics_occlusion_render_scale_profile() {
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        for count in [1, 15] {
            let mut graphics = SurfaceGraphicsScene::default();
            for index in 0..count {
                let image = asset(
                    SurfaceGraphicsTarget::Pane {
                        pane_id: format!("pane-{index}"),
                    },
                    index as u64,
                    vec![1, 2, 3, 4],
                );
                let pane = scene(image, index * 4, 0);
                graphics.assets.extend(pane.assets);
                graphics.placements.extend(pane.placements);
            }
            for covered in [false, true] {
                let mut state = ClientState::default();
                state.set_scope("profile");
                state.set_scene(graphics.clone());
                let mut cover = Occlusion::default();
                if covered {
                    cover.cover(Rect::new(0, 10, 80, 3));
                    cover.cover(Rect::new(60, 0, 20, 5));
                }
                let _ = state.encode_output(Visibility::Main, (0, 0), None, cell, &cover);
                let mut samples = Vec::new();
                for _ in 0..101 {
                    let start = std::time::Instant::now();
                    for _ in 0..20 {
                        std::hint::black_box(state.encode_output(
                            Visibility::Main,
                            (0, 0),
                            None,
                            cell,
                            &cover,
                        ));
                    }
                    samples.push(start.elapsed().as_nanos() / 20);
                }
                samples.sort_unstable();
                eprintln!(
                    "graphics occlusion panes={count} disjoint_overlays={covered} median_ns={} p95_ns={}",
                    samples[50], samples[95]
                );
            }
            #[cfg(unix)]
            {
                let mut state = ClientState::default();
                state.set_scope("swap-profile");
                state.set_scene(graphics.clone());
                state.encode_output(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
                graphics.assets.clear();
                let mut samples = Vec::new();
                for revision in 1..=105 {
                    graphics.placements[0].asset.data_fingerprint = revision;
                    let key = graphics.placements[0].asset.clone();
                    let base = native_host_image_id(state.scope(), &key);
                    let id = if revision % 2 == 1 {
                        base ^ NATIVE_SLOT_BIT
                    } else {
                        base
                    };
                    let scene = graphics.clone();
                    let start = std::time::Instant::now();
                    state.set_scene(scene);
                    std::hint::black_box(state.encode_output(
                        Visibility::Main,
                        (0, 0),
                        None,
                        cell,
                        &Occlusion::default(),
                    ));
                    let checkpoint = state.clone();
                    assert!(state.trust_direct_asset(&key, id));
                    std::hint::black_box(state.encode_output(
                        Visibility::Main,
                        (0, 0),
                        None,
                        cell,
                        &Occlusion::default(),
                    ));
                    drop(checkpoint);
                    if revision > 5 {
                        samples.push(start.elapsed().as_nanos());
                    }
                }
                samples.sort_unstable();
                eprintln!(
                    "graphics replacement panes={count} median_ns={} p95_ns={}",
                    samples[50], samples[95]
                );
            }
        }
    }

    #[test]
    fn client_encodes_final_main_origin_upload_once_and_replays_placement() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            11,
            vec![1, 2, 3, 4],
        );
        state.set_scene(scene(image, 1, 2));

        let first = state.encode(
            Visibility::Main,
            (10, 5),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&first).contains("a=t,t=d"));
        assert!(String::from_utf8_lossy(&first).contains("\u{1b}[8;12H"));

        let second = state.encode(
            Visibility::Main,
            (10, 5),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        let second = String::from_utf8_lossy(&second);
        assert!(!second.contains("a=t,t=d"));
        assert!(second.contains("a=p"));
        assert!(second.contains("\u{1b}[8;12H"));
    }

    #[test]
    fn client_hides_and_restores_without_reuploading_pixels() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            12,
            vec![4, 3, 2, 1],
        );
        state.set_scene(scene(image, 0, 0));
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let _ = state.encode(Visibility::Main, (4, 2), None, cell, &Occlusion::default());

        let hidden = state.encode(
            Visibility::Hidden,
            (4, 2),
            None,
            cell,
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&hidden).contains("a=d,d=i"));

        let restored = state.encode(Visibility::Main, (4, 2), None, cell, &Occlusion::default());
        let restored = String::from_utf8_lossy(&restored);
        assert!(restored.contains("a=p"));
        assert!(!restored.contains("a=t,t=d"));
    }

    #[test]
    fn popup_candidates_use_client_resolved_popup_inner_origin() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Popup {
                terminal_id: "terminal-popup".into(),
            },
            13,
            vec![9, 8, 7, 6],
        );
        state.set_scene(scene(image, 2, 1));

        let bytes = state.encode(
            Visibility::Popup,
            (20, 4),
            Some((30, 10)),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&bytes).contains("\u{1b}[12;33H"));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_direct_asset_is_placed_without_inline_reupload() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let _ = state.encode(
            Visibility::Hidden,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        let mut image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            15,
            vec![1, 2, 3, 4],
        );
        image.key.source = SurfaceGraphicsSource::PaneLayer {
            pane_id: "w1:p1".into(),
            layer_id: "primary".into(),
        };
        let image_id = state.image_id(&image.key);
        assert!(state.trust_direct_asset(&image.key, image_id));
        let mut direct_scene = scene(image, 0, 0);
        direct_scene.assets.clear();
        state.set_scene(direct_scene);

        let bytes = state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        let bytes = String::from_utf8_lossy(&bytes);
        assert!(bytes.contains("a=p"));
        assert!(!bytes.contains("a=t,t=d"));
    }

    #[cfg(unix)]
    #[test]
    fn direct_asset_trusted_after_scene_arrival_is_immediately_placeable() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let _ = state.take_pending_cleanup();
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            18,
            vec![1, 2, 3, 4],
        );
        let image_id = state.image_id(&image.key);
        let mut direct_scene = scene(image.clone(), 0, 0);
        direct_scene.assets.clear();
        state.set_scene(direct_scene);
        assert!(state.trust_direct_asset(&image.key, image_id));

        let bytes = state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        let bytes = String::from_utf8_lossy(&bytes);
        assert!(bytes.contains("a=p"), "{bytes}");
        assert!(!bytes.contains("a=t,t=d"), "{bytes}");
    }

    #[cfg(unix)]
    #[test]
    fn retained_direct_asset_survives_hidden_scene_and_replays_without_upload() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let _ = state.take_pending_cleanup();
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            19,
            vec![1, 2, 3, 4],
        );
        let image_id = state.image_id(&image.key);
        let mut active = scene(image.clone(), 0, 0);
        active.assets.clear();
        state.set_scene(active.clone());
        assert!(state.trust_direct_asset(&image.key, image_id));
        let _ = state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );

        state.set_scene(SurfaceGraphicsScene {
            retained_assets: vec![image.key.clone()],
            ..SurfaceGraphicsScene::default()
        });
        let hidden = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(!hidden.contains(&format!("a=d,d=I,i={image_id}")));

        active.retained_assets.push(image.key.clone());
        state.set_scene(active);
        let restored = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(restored.contains("a=p"), "{restored}");
        assert!(!restored.contains("a=t,t=d"), "{restored}");

        state.set_scene(SurfaceGraphicsScene::default());
        let removed = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(removed.contains(&format!("a=d,d=I,i={image_id}")));
    }

    #[test]
    fn popup_visibility_keeps_uncovered_main_scene_placements() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let main = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            20,
            vec![1, 2, 3, 4],
        );
        let popup = asset(
            SurfaceGraphicsTarget::Popup {
                terminal_id: "popup-1".into(),
            },
            21,
            vec![4, 3, 2, 1],
        );
        let mut graphics = scene(main, 0, 0);
        let popup_scene = scene(popup, 0, 0);
        graphics.assets.extend(popup_scene.assets);
        graphics.placements.extend(popup_scene.placements);
        state.set_scene(graphics);

        let bytes = String::from_utf8(state.encode(
            Visibility::Popup,
            (2, 1),
            Some((20, 10)),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(bytes.contains("\u{1b}[2;3H"), "{bytes}");
        assert!(bytes.contains("\u{1b}[11;21H"), "{bytes}");
    }

    #[cfg(unix)]
    #[test]
    fn retired_pending_direct_asset_is_deleted_even_before_cache_adoption() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let _ = state.take_pending_cleanup();
        state.retire_direct_image(4242);
        let cleanup = String::from_utf8(state.take_pending_cleanup()).unwrap();
        assert!(cleanup.contains("a=d,d=I,i=4242"), "{cleanup}");
    }

    #[cfg(unix)]
    #[test]
    fn unclaimed_legacy_direct_asset_is_deleted_by_the_next_authoritative_scene() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let _ = state.take_pending_cleanup();
        let mut image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            16,
            vec![1, 2, 3, 4],
        );
        image.key.source = SurfaceGraphicsSource::PaneLayer {
            pane_id: "w1:p1".into(),
            layer_id: "primary".into(),
        };
        let image_id = state.image_id(&image.key);
        assert!(state.trust_direct_asset(&image.key, image_id));
        state.set_scene(SurfaceGraphicsScene::default());

        let bytes = state.encode(
            Visibility::Hidden,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        let bytes = String::from_utf8_lossy(&bytes);
        assert!(bytes.contains(&format!("a=d,d=I,i={image_id}")), "{bytes}");
    }

    #[test]
    fn boot_scope_cleanup_does_not_wait_for_a_coherent_surface() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            17,
            vec![1, 2, 3, 4],
        );
        state.set_scene(scene(image, 0, 0));
        let _ = state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );

        state.set_scope("endpoint-a:boot-2");
        let cleanup = String::from_utf8(state.take_pending_cleanup()).unwrap();
        assert!(cleanup.contains("a=d,d=I"), "{cleanup}");
        assert!(state.take_pending_cleanup().is_empty());
    }

    #[test]
    fn replacement_scene_without_repeated_asset_bytes_keeps_resident_data() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            14,
            vec![1, 1, 1, 1],
        );
        let first_scene = scene(image, 0, 0);
        let mut replacement = first_scene.clone();
        replacement.assets.clear();
        state.set_scene(first_scene);
        state.set_scene(replacement);

        let bytes = state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&bytes).contains("a=t,t=d"));
    }
}
