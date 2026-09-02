use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::api::schema::{PaneGraphicsFormat, PaneGraphicsPlacementParams};
use crate::layout::PaneId;

pub(crate) type Key = (PaneId, String);

pub(crate) const DIRECT_DELIVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const DIRECT_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
pub(crate) const DIRECT_OUTER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(9);

#[derive(Debug)]
pub(crate) enum Backing {
    Inline(Vec<u8>),
    Direct(crate::pane_graphics_files::Lease),
    Resident { len: usize, client_id: u64 },
}

#[derive(Debug)]
pub(crate) struct Layer {
    pub(crate) format: PaneGraphicsFormat,
    pub(crate) image_width: u32,
    pub(crate) image_height: u32,
    pub(crate) backing: Backing,
    pub(crate) data_fingerprint: u64,
    pub(crate) render: PaneGraphicsPlacementParams,
    pub(crate) z_index: i32,
}

#[derive(Debug)]
pub(crate) struct DirectGate {
    pub(crate) transfer_id: u64,
    pub(crate) image_id: u32,
    pub(crate) client_id: u64,
    pub(crate) deadline: std::time::Instant,
    pub(crate) written: bool,
    pub(crate) success_response: String,
    pub(crate) respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub(crate) struct Slot {
    pub(crate) host_image_id: u32,
    pub(crate) layer: Option<Layer>,
    pub(crate) stream_owner: Option<String>,
    pub(crate) stream_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) direct_gate: Option<DirectGate>,
}

impl Slot {
    #[cfg(test)]
    pub(crate) fn test(host_image_id: u32, layer: Option<Layer>) -> Self {
        Self {
            host_image_id,
            layer,
            stream_owner: None,
            stream_active: None,
            direct_gate: None,
        }
    }

    pub(crate) fn stream_is_active(&self) -> bool {
        self.stream_active
            .as_ref()
            .is_some_and(|active| active.load(std::sync::atomic::Ordering::Acquire))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(active) = &self.stream_active {
            active.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

#[derive(Debug)]
pub(crate) struct Runtime {
    pub(crate) slots: HashMap<Key, Slot>,
    revision: u64,
    next_host_image_id: u32,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            revision: 0,
            next_host_image_id: 1 << 31,
        }
    }
}

impl Runtime {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn mark_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.mark_changed();
    }

    pub(crate) fn reserve_image_id(&mut self, key: &Key) -> Option<u32> {
        if let Some(id) = self.slots.get(key).map(|slot| slot.host_image_id) {
            return Some(id);
        }
        for _ in 0..=self.slots.len() {
            let id = self.next_host_image_id | (1 << 31);
            self.next_host_image_id = id.wrapping_add(1) | (1 << 31);
            if self.slots.values().all(|slot| slot.host_image_id != id) {
                return Some(id);
            }
        }
        None
    }

    pub(crate) fn layer_count(&self, pane_id: PaneId) -> usize {
        self.slots.keys().filter(|(id, _)| *id == pane_id).count()
    }

    pub(crate) fn can_add_slot(&self, key: &Key) -> bool {
        self.slots.contains_key(key)
            || self.slots.len() < crate::api::schema::panes::PANE_GRAPHICS_MAX_LAYERS_TOTAL
    }

    pub(crate) fn can_store_inline(&self, key: &Key, len: usize) -> bool {
        let replaced = self
            .slots
            .get(key)
            .and_then(|slot| slot.layer.as_ref())
            .and_then(Layer::inline_data)
            .map_or(0, <[u8]>::len);
        self.inline_bytes()
            .saturating_sub(replaced)
            .saturating_add(len)
            <= crate::api::schema::panes::PANE_GRAPHICS_MAX_INLINE_BYTES_TOTAL
    }

    fn inline_bytes(&self) -> usize {
        self.slots
            .values()
            .filter_map(|slot| slot.layer.as_ref())
            .filter_map(Layer::inline_data)
            .map(<[u8]>::len)
            .sum()
    }

    pub(crate) fn active_for_pane(&self, pane_id: PaneId) -> bool {
        self.slots
            .iter()
            .any(|((id, _), slot)| *id == pane_id && slot.layer.is_some())
    }

    pub(crate) fn attach_stream_active(
        &mut self,
        key: &Key,
        owner: &str,
        active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        if let Some(slot) = self
            .slots
            .get_mut(key)
            .filter(|slot| slot.stream_owner.as_deref() == Some(owner))
        {
            slot.stream_active = Some(active);
        }
    }

    pub(crate) fn retain_live_panes(&mut self, state: &crate::app::state::AppState) -> bool {
        if self.slots.is_empty() {
            return false;
        }
        let before = self.slots.len();
        self.slots.retain(|(pane_id, _), _| {
            state
                .workspaces
                .iter()
                .any(|workspace| workspace.pane_state(*pane_id).is_some())
        });
        if self.slots.len() == before {
            return false;
        }
        self.mark_changed();
        true
    }
}

impl Layer {
    pub(crate) fn inline(
        format: PaneGraphicsFormat,
        image_width: u32,
        image_height: u32,
        data: Vec<u8>,
        render: PaneGraphicsPlacementParams,
        z_index: i32,
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hasher);
        Self {
            format,
            image_width,
            image_height,
            backing: Backing::Inline(data),
            data_fingerprint: hasher.finish(),
            render,
            z_index,
        }
    }

    pub(crate) fn direct(
        image_width: u32,
        image_height: u32,
        lease: crate::pane_graphics_files::Lease,
        render: PaneGraphicsPlacementParams,
        z_index: i32,
    ) -> Self {
        let data_fingerprint = lease.fingerprint();
        Self {
            format: PaneGraphicsFormat::Rgba,
            image_width,
            image_height,
            backing: Backing::Direct(lease),
            data_fingerprint,
            render,
            z_index,
        }
    }

    pub(crate) fn data_len(&self) -> usize {
        match &self.backing {
            Backing::Inline(data) => data.len(),
            Backing::Direct(lease) => lease.len(),
            Backing::Resident { len, .. } => *len,
        }
    }

    pub(crate) fn inline_data(&self) -> Option<&[u8]> {
        match &self.backing {
            Backing::Inline(data) => Some(data),
            Backing::Direct(_) | Backing::Resident { .. } => None,
        }
    }

    pub(crate) fn direct_lease(&self) -> Option<&crate::pane_graphics_files::Lease> {
        match &self.backing {
            Backing::Direct(lease) => Some(lease),
            Backing::Inline(_) | Backing::Resident { .. } => None,
        }
    }

    pub(crate) fn mark_resident(&mut self, client_id: u64) -> bool {
        let Backing::Direct(lease) = &self.backing else {
            return false;
        };
        self.backing = Backing::Resident {
            len: lease.len(),
            client_id,
        };
        true
    }

    pub(crate) fn resident_client(&self) -> Option<u64> {
        match self.backing {
            Backing::Resident { client_id, .. } => Some(client_id),
            Backing::Inline(_) | Backing::Direct(_) => None,
        }
    }

    pub(crate) fn terminal_only(&self) -> bool {
        matches!(self.backing, Backing::Direct(_) | Backing::Resident { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_liveness_attaches_only_to_the_exact_owned_layer() {
        let mut runtime = Runtime::default();
        let first = (PaneId::from_raw(1), "first".into());
        let second = (PaneId::from_raw(2), "second".into());
        for (key, id) in [(&first, 1), (&second, 2)] {
            runtime.slots.insert(
                key.clone(),
                Slot {
                    host_image_id: (1 << 31) | id,
                    layer: None,
                    stream_owner: Some("shared-owner".into()),
                    stream_active: None,
                    direct_gate: None,
                },
            );
        }
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        runtime.attach_stream_active(&second, "shared-owner", active.clone());

        assert!(runtime.slots[&first].stream_active.is_none());
        assert!(std::sync::Arc::ptr_eq(
            runtime.slots[&second].stream_active.as_ref().unwrap(),
            &active
        ));
    }

    #[test]
    fn runtime_bounds_total_slots_and_inline_bytes() {
        let mut runtime = Runtime::default();
        for index in 0..crate::api::schema::panes::PANE_GRAPHICS_MAX_LAYERS_TOTAL {
            let key = (PaneId::from_raw(1), format!("layer-{index}"));
            runtime
                .slots
                .insert(key, Slot::test((1 << 31) | index as u32, None));
        }
        assert!(!runtime.can_add_slot(&(PaneId::from_raw(2), "extra".into())));

        let mut runtime = Runtime::default();
        let key = (PaneId::from_raw(1), "primary".into());
        runtime.slots.insert(
            key.clone(),
            Slot::test(
                1 << 31,
                Some(Layer::inline(
                    PaneGraphicsFormat::Rgba,
                    1,
                    1,
                    vec![0; 4],
                    PaneGraphicsPlacementParams::default(),
                    0,
                )),
            ),
        );
        assert!(runtime.can_store_inline(
            &key,
            crate::api::schema::panes::PANE_GRAPHICS_MAX_INLINE_BYTES_TOTAL,
        ));
        assert!(!runtime.can_store_inline(
            &(PaneId::from_raw(2), "primary".into()),
            crate::api::schema::panes::PANE_GRAPHICS_MAX_INLINE_BYTES_TOTAL,
        ));
    }
}
