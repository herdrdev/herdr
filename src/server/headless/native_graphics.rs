//! Decoded native exports. Deliberately independent of API slots/leases.
use super::HeadlessServer;
use crate::kitty_graphics::surface::{DeliveryCache, SourceFiles, NATIVE_SLOT_BIT};
use crate::pane_graphics_files::{FileStore, OwnedExport};
use crate::protocol::{
    ServerMessage, SurfaceGraphicsAsset, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat,
    SurfaceGraphicsScene, SurfaceGraphicsSource,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kitty_graphics::surface::NATIVE_TRANSFER_BIT as NATIVE_BIT;
const MAX_FILE: usize = 16 * 1024 * 1024;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct Pending {
    export: Arc<OwnedExport>,
    transfer_id: u64,
    image_id: u32,
    source: SurfaceGraphicsSource,
    asset: SurfaceGraphicsAssetKey,
    slot: bool,
    deadline: Instant,
    written: bool,
    refresh_needed: bool,
    scene: SurfaceGraphicsScene,
    delivery: DeliveryCache,
}

struct AcknowledgedSlot {
    asset: SurfaceGraphicsAssetKey,
    slot: bool,
}

impl Pending {
    pub(super) fn defer_inline_delivery(&mut self, delivery: &DeliveryCache) {
        self.delivery = delivery.clone();
        self.refresh_needed = true;
    }
}

pub(super) struct NativeGraphics {
    store: FileStore,
    pending: HashMap<u64, Pending>,
    disabled: HashSet<u64>,
    next_transfer: u64,
    source_retries: HashMap<u64, Instant>,
    // The key and bank currently addressed for each logical source. Native state
    // advances on ACK; inline state and source pruning advance on queued scenes.
    acknowledged_slots: HashMap<u64, HashMap<SurfaceGraphicsSource, AcknowledgedSlot>>,
}
impl Default for NativeGraphics {
    fn default() -> Self {
        Self {
            store: FileStore::default(),
            pending: HashMap::new(),
            disabled: HashSet::new(),
            next_transfer: 1,
            source_retries: HashMap::new(),
            acknowledged_slots: HashMap::new(),
        }
    }
}
impl NativeGraphics {
    #[cfg(all(test, unix))]
    pub(super) fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
    pub(super) fn is_pending(&self, client: u64) -> bool {
        self.pending.contains_key(&client)
    }
    pub(super) fn can_hold(&self, client: u64, scene: &SurfaceGraphicsScene) -> bool {
        let Some(pending) = self.pending.get(&client) else {
            return true;
        };
        let same_image = |a: &crate::protocol::SurfaceGraphicsAssetKey,
                          b: &crate::protocol::SurfaceGraphicsAssetKey| {
            a.source == b.source
                && a.image_width == b.image_width
                && a.image_height == b.image_height
                && a.format == b.format
                && a.data_len == b.data_len
        };
        scene.placements.len() == pending.scene.placements.len()
            && scene
                .placements
                .iter()
                .zip(&pending.scene.placements)
                .all(|(current, held)| {
                    if !same_image(&current.asset, &held.asset) {
                        return false;
                    }
                    let mut geometry = current.clone();
                    geometry.asset = held.asset.clone();
                    geometry == *held
                })
            && scene.retained_assets.len() == pending.scene.retained_assets.len()
            && scene
                .retained_assets
                .iter()
                .zip(&pending.scene.retained_assets)
                .all(|(current, held)| same_image(current, held))
    }

    pub(super) fn hold(&self, client: u64) -> Option<(SurfaceGraphicsScene, DeliveryCache)> {
        self.pending
            .get(&client)
            .map(|p| (p.scene.clone(), p.delivery.clone()))
    }
    /// Records bank state only after the scene metadata has been queued.
    ///
    /// `inline_assets` contains only payloads actually queued on the wire; a
    /// selected native upload has already been removed and changes bank only
    /// after its ACK. A new inline key addresses bank 0, while replaying the
    /// exact native key keeps the client's existing key-to-bank mapping.
    /// Pruning is committed here as well: a scene rejected by the writer must
    /// not mutate bank history for pixels the client is still displaying.
    pub(super) fn commit_scene(
        &mut self,
        client: u64,
        scene: &SurfaceGraphicsScene,
        inline_assets: &[SurfaceGraphicsAssetKey],
    ) {
        let present_sources: HashSet<_> = scene
            .placements
            .iter()
            .map(|placement| &placement.asset.source)
            .chain(scene.retained_assets.iter().map(|asset| &asset.source))
            .cloned()
            .collect();
        let slots = self.acknowledged_slots.entry(client).or_default();
        slots.retain(|source, _| present_sources.contains(source));
        for asset in inline_assets
            .iter()
            .filter(|asset| matches!(asset.source, SurfaceGraphicsSource::Terminal { .. }))
        {
            let same_resident_key = slots
                .get(&asset.source)
                .is_some_and(|resident| resident.asset == *asset);
            if !same_resident_key {
                slots.insert(
                    asset.source.clone(),
                    AcknowledgedSlot {
                        asset: asset.clone(),
                        slot: false,
                    },
                );
            }
        }
        if slots.is_empty() {
            self.acknowledged_slots.remove(&client);
        }
    }
    #[cfg(all(test, unix))]
    pub(super) fn prepare(
        &mut self,
        client: u64,
        scope: &str,
        scene: &mut SurfaceGraphicsScene,
        delivery: &DeliveryCache,
    ) -> Option<(Pending, ServerMessage)> {
        self.prepare_with_sources(client, scope, scene, delivery, &mut SourceFiles::default())
    }

    pub(super) fn prepare_with_sources(
        &mut self,
        client: u64,
        scope: &str,
        scene: &mut SurfaceGraphicsScene,
        delivery: &DeliveryCache,
        sources: &mut SourceFiles,
    ) -> Option<(Pending, ServerMessage)> {
        if self.disabled.contains(&client)
            || self.pending.contains_key(&client)
            || self.pending.len() >= 8
        {
            return None;
        }
        let index = scene
            .assets
            .iter()
            .position(|asset| eligible(asset, sources))?;
        let asset = &scene.assets[index];
        if self.pending.values().map(|p| p.export.len()).sum::<usize>()
            + asset.key.data_len as usize
            > MAX_BYTES
            || self.next_transfer >= NATIVE_BIT
        {
            return None;
        }
        let export = if let Some(source) = sources.get(&asset.key) {
            Arc::clone(source)
        } else {
            Arc::new(self.store.export(&asset.data).ok()?)
        };
        let transfer_id = NATIVE_BIT | self.next_transfer;
        self.next_transfer += 1;
        let asset_key = asset.key.clone();
        let source = asset_key.source.clone();
        // Start in bank 1: an older client may still be displaying an inline image
        // under the unchanged logical base ID. Thereafter always stage into the
        // bank opposite the one this client currently addresses.
        let slot = !self
            .acknowledged_slots
            .get(&client)
            .and_then(|slots| slots.get(&source))
            .map(|resident| resident.slot)
            .unwrap_or(false);
        let base = crate::kitty_graphics::surface::native_host_image_id(scope, &asset.key);
        let image_id = base ^ if slot { NATIVE_SLOT_BIT } else { 0 };
        let message = ServerMessage::GraphicsFile {
            path: export.path().to_string_lossy().into_owned(),
            expected_len: export.len() as u64,
            image_id,
            transfer_id,
            leading: Vec::new(),
            control: format!(
                "a=t,f=32,s={},v={},i={image_id},q=0",
                asset.key.image_width, asset.key.image_height
            ),
            surface_asset: Some(asset_key.clone()),
        };
        sources.remove(&asset_key);
        scene.assets.remove(index);
        let mut held = scene.clone();
        held.assets.clear();
        Some((
            Pending {
                export,
                transfer_id,
                image_id,
                source,
                asset: asset_key,
                slot,
                deadline: Instant::now() + DELIVERY_TIMEOUT,
                written: false,
                refresh_needed: delivery.has_pending(),
                scene: held,
                delivery: delivery.clone(),
            },
            message,
        ))
    }
    pub(super) fn commit(&mut self, client: u64, pending: Pending) {
        self.pending.insert(client, pending);
    }
    fn matches(&self, client: u64, transfer: u64, image: u32) -> bool {
        self.pending
            .get(&client)
            .is_some_and(|p| p.transfer_id == transfer && p.image_id == image)
    }
}
fn materialize_sources(
    scene: &mut SurfaceGraphicsScene,
    delivery: &mut DeliveryCache,
    sources: &mut SourceFiles,
) -> bool {
    let mut failed = false;
    scene.assets.retain_mut(|asset| {
        let Some(source) = sources.remove(&asset.key) else {
            return true;
        };
        match source.copy_rgba() {
            Ok(data) => {
                asset.data = data;
                true
            }
            Err(error) => {
                tracing::debug!(%error, "native source fallback could not be read");
                failed = true;
                delivery.forget_asset(&asset.key);
                false
            }
        }
    });
    sources.clear();
    failed
}

fn eligible(asset: &SurfaceGraphicsAsset, sources: &SourceFiles) -> bool {
    matches!(asset.key.source, SurfaceGraphicsSource::Terminal { .. })
        && asset.key.format == SurfaceGraphicsFormat::Rgba
        && asset.key.data_len > 0
        && asset.key.data_len <= MAX_FILE as u64
        && sources.get(&asset.key).map_or_else(
            || asset.key.data_len == asset.data.len() as u64,
            |source| asset.key.data_len == source.len() as u64,
        )
        && u64::from(asset.key.image_width)
            .checked_mul(u64::from(asset.key.image_height))
            .and_then(|pixels| pixels.checked_mul(4))
            == Some(asset.key.data_len)
}
impl HeadlessServer {
    pub(super) fn defer_changed_native_geometry(
        &mut self,
        client: u64,
        scene: &SurfaceGraphicsScene,
    ) -> bool {
        if self.native_graphics.can_hold(client, scene) {
            return false;
        }
        self.retire_native_graphics_for_client(client);
        self.app.render_dirty.request_generic();
        if let Some(client) = self.clients.get_mut(&client) {
            client.defer_full_render();
        }
        true
    }

    fn materialize_native_sources(
        &mut self,
        client: u64,
        scene: &mut SurfaceGraphicsScene,
        delivery: &mut DeliveryCache,
        sources: &mut SourceFiles,
    ) {
        if materialize_sources(scene, delivery, sources) {
            self.native_graphics
                .source_retries
                .entry(client)
                .or_insert_with(|| Instant::now() + std::time::Duration::from_secs(1));
        } else {
            self.native_graphics.source_retries.remove(&client);
        }
    }

    pub(super) fn prepare_native_scene(
        &mut self,
        client: u64,
        scene: &mut SurfaceGraphicsScene,
        delivery: &mut DeliveryCache,
        sources: &mut SourceFiles,
    ) -> Option<(Pending, ServerMessage)> {
        if let Some(pending) = self.native_graphics.pending.get_mut(&client) {
            pending.refresh_needed |= !scene.assets.is_empty()
                || scene.placements != pending.scene.placements
                || scene.retained_assets != pending.scene.retained_assets;
        }
        if let Some((held, held_delivery)) = self.native_graphics.hold(client) {
            *scene = held;
            *delivery = held_delivery;
            sources.clear();
            return None;
        }
        if !self.client_supports_direct_graphics(client) {
            self.materialize_native_sources(client, scene, delivery, sources);
            return None;
        }
        let mut prepared = self.native_graphics.prepare_with_sources(
            client,
            &self.client_shell_boot_id,
            scene,
            delivery,
            sources,
        );
        self.materialize_native_sources(client, scene, delivery, sources);
        if let Some((pending, _)) = &mut prepared {
            pending.delivery = delivery.clone();
            pending.refresh_needed |= delivery.has_pending();
        }
        prepared
    }

    pub(super) fn native_started(&mut self, client: u64, transfer: u64, image: u32) -> bool {
        if transfer & NATIVE_BIT == 0 {
            return false;
        }
        if !self.native_graphics.disabled.contains(&client)
            && self.native_graphics.matches(client, transfer, image)
        {
            let p = self
                .native_graphics
                .pending
                .get_mut(&client)
                .expect("matched");
            p.written = true;
            p.deadline = Instant::now() + RESPONSE_TIMEOUT;
        }
        true
    }
    pub(super) fn native_result(
        &mut self,
        client: u64,
        transfer: u64,
        image: u32,
        success: bool,
    ) -> Option<bool> {
        if transfer & NATIVE_BIT == 0 {
            return None;
        }
        if !self.native_graphics.matches(client, transfer, image) {
            return Some(false);
        }
        if self.native_graphics.disabled.contains(&client) {
            return Some(self.expire_native_graphics(Instant::now()));
        }
        if success {
            if !self.native_graphics.pending[&client].written {
                return Some(false);
            }
            let pending = self
                .native_graphics
                .pending
                .remove(&client)
                .expect("matched");
            self.native_graphics
                .acknowledged_slots
                .entry(client)
                .or_default()
                .insert(
                    pending.source,
                    AcknowledgedSlot {
                        asset: pending.asset,
                        slot: pending.slot,
                    },
                );
            let refresh = pending.refresh_needed;
            if refresh {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.defer_full_render();
                }
            }
            return Some(refresh);
        }
        self.native_graphics.disabled.insert(client);
        self.native_graphics
            .pending
            .get_mut(&client)
            .expect("matched")
            .deadline = Instant::now();
        Some(self.expire_native_graphics(Instant::now()))
    }
    pub(super) fn expire_native_graphics(&mut self, now: Instant) -> bool {
        let expired: Vec<_> = self
            .native_graphics
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(id, p)| (*id, p.transfer_id, p.image_id))
            .collect();
        let mut changed = false;
        self.native_graphics.source_retries.retain(|id, deadline| {
            if *deadline > now {
                return true;
            }
            if let Some(client) = self.clients.get_mut(id) {
                client.defer_full_render();
                changed = true;
            }
            false
        });
        for (id, transfer_id, image_id) in expired {
            self.native_graphics.disabled.insert(id);
            let message = ServerMessage::GraphicsTransmissionRetired {
                transfer_id,
                image_id,
            };
            // Control messages precede queued renders. The client permanently rejects
            // retired native transfers, including a file still in the render queue.
            let sent = self.send_to_client(id, message);
            if !sent {
                continue;
            } // Keep the export alive until retirement is queued or disconnect cleans it.
            self.native_graphics.pending.remove(&id);
            self.native_graphics.disabled.insert(id);
            if let Some(c) = self.clients.get_mut(&id) {
                c.shell_graphics_delivery = DeliveryCache::default();
                c.defer_full_render();
            }
            changed = true;
        }
        changed
    }
    pub(super) fn retire_native_graphics_for_client(&mut self, id: u64) {
        if let Some(p) = self.native_graphics.pending.get_mut(&id) {
            p.deadline = Instant::now();
            self.native_graphics.disabled.insert(id);
            if self.expire_native_graphics(Instant::now()) {
                self.app.render_dirty.request_generic();
            }
        }
    }
    pub(super) fn disconnect_native_graphics(&mut self, id: u64) {
        self.native_graphics.pending.remove(&id);
        self.native_graphics.disabled.remove(&id);
        self.native_graphics.source_retries.remove(&id);
        self.native_graphics.acknowledged_slots.remove(&id);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::protocol::{SurfaceGraphicsAssetKey, SurfaceGraphicsTarget};

    fn asset() -> SurfaceGraphicsAsset {
        SurfaceGraphicsAsset {
            key: SurfaceGraphicsAssetKey {
                source: SurfaceGraphicsSource::Terminal {
                    target: SurfaceGraphicsTarget::Pane {
                        pane_id: "native-test".into(),
                    },
                    image_id: 1,
                },
                image_width: 1,
                image_height: 1,
                format: SurfaceGraphicsFormat::Rgba,
                data_len: 4,
                data_fingerprint: 123,
            },
            data: vec![1, 2, 3, 255],
        }
    }

    #[test]
    fn eligibility_is_decoded_rgba_terminal_only() {
        let mut a = asset();
        assert!(eligible(&a, &SourceFiles::default()));
        a.key.format = SurfaceGraphicsFormat::Png;
        assert!(!eligible(&a, &SourceFiles::default()));
        a.key.format = SurfaceGraphicsFormat::Rgba;
        a.key.data_len = 5;
        assert!(!eligible(&a, &SourceFiles::default()));
        a.key.data_len = 4;
        a.key.source = SurfaceGraphicsSource::PaneLayer {
            pane_id: "p".into(),
            layer_id: "l".into(),
        };
        assert!(!eligible(&a, &SourceFiles::default()));
        let mut a = asset();
        a.data.resize(MAX_FILE + 1, 0);
        a.key.data_len = a.data.len() as u64;
        assert!(!eligible(&a, &SourceFiles::default()));
    }

    #[test]
    fn pending_guard_and_client_identity_are_independent() {
        let mut state = NativeGraphics::default();
        let mut scene = SurfaceGraphicsScene {
            assets: vec![asset()],
            ..Default::default()
        };
        let (pending, _) = state
            .prepare(7, "scope", &mut scene, &DeliveryCache::default())
            .unwrap();
        let path = pending.export.path().to_owned();
        let transfer = pending.transfer_id;
        let image = pending.image_id;
        assert_ne!(transfer & NATIVE_BIT, 0);
        assert!(scene.assets.is_empty());
        assert!(!state.busy()); // prepare is not a delivery commit
        state.commit(7, pending);
        assert!(state.matches(7, transfer, image));
        assert!(!state.matches(8, transfer, image));
        assert!(!state.matches(7, transfer + 1, image));
        assert!(!state.matches(7, transfer, image + 1));
        assert!(path.exists());
        assert!(state.hold(7).unwrap().0.assets.is_empty());
        state.pending.remove(&7);
        assert!(!path.exists());
        state.disabled.insert(7);
        scene.assets.push(asset());
        assert!(state
            .prepare(7, "scope", &mut scene, &DeliveryCache::default())
            .is_none());
        assert_eq!(scene.assets.len(), 1); // native fallback keeps authoritative inline bytes
    }

    #[test]
    fn one_pending_per_client_and_global_count_limit() {
        let mut state = NativeGraphics::default();
        for id in 0..8 {
            let mut scene = SurfaceGraphicsScene {
                assets: vec![asset()],
                ..Default::default()
            };
            let (pending, _) = state
                .prepare(id, "scope", &mut scene, &DeliveryCache::default())
                .unwrap();
            state.commit(id, pending);
            scene.assets.push(asset());
            assert!(state
                .prepare(id, "scope", &mut scene, &DeliveryCache::default())
                .is_none());
        }
        let mut scene = SurfaceGraphicsScene {
            assets: vec![asset()],
            ..Default::default()
        };
        assert!(state
            .prepare(9, "scope", &mut scene, &DeliveryCache::default())
            .is_none());
        assert_eq!(scene.assets.len(), 1);
    }
}
