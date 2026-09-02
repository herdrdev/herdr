use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use ratatui::layout::Rect;

use super::{
    clipped_placement, collect_visible_placements, encode_graphics_update_incremental,
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

impl DeliveryCache {
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
pub(crate) struct ClientState {
    scope: String,
    scene: SurfaceGraphicsScene,
    assets: HashMap<SurfaceGraphicsAssetKey, Vec<u8>>,
    host: HostGraphicsCache,
    trusted_direct: HashMap<SurfaceGraphicsAssetKey, u32>,
    reset_pending: bool,
    stale_images: Vec<u32>,
    forced_delete_images: Vec<u32>,
}

impl ClientState {
    pub(crate) fn set_scope(&mut self, scope: &str) {
        if self.scope == scope {
            return;
        }
        self.scope = scope.to_owned();
        self.scene = SurfaceGraphicsScene::default();
        self.assets.clear();
        self.trusted_direct.clear();
        self.stale_images.clear();
        self.forced_delete_images.clear();
        self.reset_pending = true;
    }

    #[cfg(unix)]
    pub(crate) fn trust_direct_asset(
        &mut self,
        key: &SurfaceGraphicsAssetKey,
        image_id: u32,
    ) -> bool {
        if image_id != host_image_id(&self.scope, key) {
            return false;
        }
        self.host
            .images
            .insert(image_id, image_signature_from_asset(key));
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
        let previous = self
            .scene
            .placements
            .iter()
            .map(|placement| placement.asset.clone())
            .chain(self.scene.retained_assets.iter().cloned())
            .collect::<HashSet<_>>();
        self.stale_images.extend(
            previous
                .difference(&desired)
                .map(|key| host_image_id(&self.scope, key)),
        );
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
        self.assets.retain(|key, _| placed.contains(key));
        for asset in std::mem::take(&mut scene.assets) {
            if asset.data.len() as u64 == asset.key.data_len && placed.contains(&asset.key) {
                self.assets.insert(asset.key, asset.data);
            }
        }
        self.scene = scene;
    }

    pub(crate) fn encode(
        &mut self,
        visibility: Visibility,
        main_origin: (u16, u16),
        popup_origin: Option<(u16, u16)>,
        cell_size: HostCellSize,
    ) -> Vec<u8> {
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
            return bytes;
        }

        let placements = self
            .scene
            .placements
            .iter()
            .filter_map(|placement| {
                client_host_placement(
                    &self.scope,
                    placement,
                    self.assets.get(&placement.asset).map(Vec::as_slice),
                    visibility,
                    main_origin,
                    popup_origin,
                    cell_size,
                )
            })
            .collect::<Vec<_>>();
        self.host.request_placement_replay();
        loop {
            let encoded = encode_graphics_update_incremental(
                &mut self.host,
                &placements,
                &HashSet::new(),
                None,
                false,
            );
            bytes.extend(encoded.bytes);
            if !encoded.incomplete {
                return bytes;
            }
        }
    }
}

pub(crate) fn pane_layer_asset_key(
    app: &crate::app::App,
    key: &crate::app::pane_graphics::Key,
    layer: &crate::app::pane_graphics::Layer,
) -> Option<SurfaceGraphicsAssetKey> {
    let workspace_index = app
        .state
        .workspaces
        .iter()
        .position(|workspace| workspace.pane_state(key.0).is_some())?;
    Some(SurfaceGraphicsAssetKey {
        source: SurfaceGraphicsSource::PaneLayer {
            pane_id: app.public_pane_id(workspace_index, key.0)?,
            layer_id: key.1.clone(),
        },
        image_width: layer.image_width,
        image_height: layer.image_height,
        format: match layer.format {
            crate::api::schema::PaneGraphicsFormat::Rgb => SurfaceGraphicsFormat::Rgb,
            crate::api::schema::PaneGraphicsFormat::Rgba
            | crate::api::schema::PaneGraphicsFormat::Bgra => SurfaceGraphicsFormat::Rgba,
            crate::api::schema::PaneGraphicsFormat::Png => SurfaceGraphicsFormat::Png,
        },
        data_len: layer.data_len() as u64,
        data_fingerprint: layer.data_fingerprint,
    })
}

pub(crate) fn host_image_id(scope: &str, key: &SurfaceGraphicsAssetKey) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    key.hash(&mut hasher);
    10_000 + ((hasher.finish() as u32) % 900_000)
}

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
    client_id: u64,
) -> (SurfaceGraphicsScene, DeliveryCache) {
    if !cell_size.is_known() {
        return (SurfaceGraphicsScene::default(), DeliveryCache::default());
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

    // Reconstruct only the small image-signature index expected by the existing
    // collector. This prevents copying already-delivered image payloads on each
    // pane-scaled render while keeping Ghostty as the authoritative image store.
    let mut uploaded_images = HashMap::new();
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
            SurfaceGraphicsSource::PaneLayer { pane_id, layer_id } => {
                if let Some(pane_id) = public_panes.get(pane_id) {
                    if let Some(slot) = app.pane_graphics.slots.get(&(*pane_id, layer_id.clone())) {
                        uploaded_images.insert(slot.host_image_id, signature);
                    }
                }
            }
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Popup { .. },
                ..
            } => {}
        }
    }
    let mut host_placements = collect_visible_placements(
        &app.state,
        &app.pane_graphics,
        &app.terminal_runtimes,
        surface,
        cell_size,
        &uploaded_images,
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
                !delivered.assets.contains(&key) && requested.insert(key)
            }) {
                host_placements.push(HostPlacement {
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
            HostSourceKey::PaneLayer { layer_id, .. } => {
                let SurfaceGraphicsTarget::Pane { pane_id } = target else {
                    continue;
                };
                SurfaceGraphicsSource::PaneLayer {
                    pane_id,
                    layer_id: layer_id.clone(),
                }
            }
            HostSourceKey::ClientSurface { .. } => continue,
        };
        let Some((clipped, _)) = clipped_placement(&placement) else {
            continue;
        };
        let asset = asset_key(source, &placement.placement);
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
        let encoded_size = super::image_transfer_estimated_size(data.len());
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
    let mut retained_assets = app
        .pane_graphics
        .slots
        .iter()
        .filter_map(|(key, slot)| {
            let layer = slot.layer.as_ref()?;
            let retained_for_client = layer.resident_client() == Some(client_id)
                || slot
                    .direct_gate
                    .as_ref()
                    .is_some_and(|gate| gate.client_id == client_id);
            retained_for_client.then(|| pane_layer_asset_key(app, key, layer))?
        })
        .collect::<Vec<_>>();
    retained_assets.sort_by_key(|key| format!("{:?}", key.source));
    (
        SurfaceGraphicsScene {
            assets,
            placements,
            retained_assets,
        },
        next,
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
    data: Option<&[u8]>,
    visibility: Visibility,
    main_origin: (u16, u16),
    popup_origin: Option<(u16, u16)>,
    cell_size: HostCellSize,
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
            data: data.unwrap_or_default().to_vec(),
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
        let _ = state.encode(Visibility::Main, (4, 2), None, cell);

        let hidden = state.encode(Visibility::Hidden, (4, 2), None, cell);
        assert!(String::from_utf8_lossy(&hidden).contains("a=d,d=i"));

        let restored = state.encode(Visibility::Main, (4, 2), None, cell);
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
        );
        assert!(String::from_utf8_lossy(&bytes).contains("\u{1b}[12;33H"));
    }

    #[cfg(unix)]
    #[test]
    fn trusted_direct_asset_is_placed_without_inline_reupload() {
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
        );
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            15,
            vec![1, 2, 3, 4],
        );
        let image_id = host_image_id("endpoint-a:boot-1", &image.key);
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
        let image_id = host_image_id("endpoint-a:boot-1", &image.key);
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
        let image_id = host_image_id("endpoint-a:boot-1", &image.key);
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
    fn unclaimed_direct_asset_is_deleted_by_the_next_authoritative_scene() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let _ = state.take_pending_cleanup();
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            16,
            vec![1, 2, 3, 4],
        );
        let image_id = host_image_id("endpoint-a:boot-1", &image.key);
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
        );
        assert!(String::from_utf8_lossy(&bytes).contains("a=t,t=d"));
    }
}
