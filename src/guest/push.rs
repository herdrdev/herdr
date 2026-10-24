//! Push notifications for guests. A guest registers its phone through the
//! guest connection; the registration lives in `devices.json` beside the guest
//! store, apart from the owner's devices, and goes when the guest is revoked.
//! Every owner alert about a local agent or a new Gram carries a
//! [`GuestScope`]; after the owner's devices, the same batch goes to the
//! devices of each active guest it concerns: the granted agent's status
//! transitions, and its Grams when the guest has `share_gram`.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::PushConfig;
use crate::persist::devices::RegisteredDevice;
use crate::push::{AlertPlan, PushNotification, Route};

use super::store::{self, GuestRecord};

const DEVICES_FILE: &str = "devices.json";
/// A guest's registrations beyond this many drop the oldest.
const MAX_DEVICES_PER_GUEST: usize = 8;

/// What an alert is about, for finding the guests it concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuestScope {
    /// A status transition of this local agent.
    Agent {
        terminal_id: String,
        name: Option<String>,
        kind: String,
    },
    /// A new Gram to the owner, with its `from` label and recorded sender.
    Gram {
        from: String,
        sender: Option<crate::persist::gram::GramSender>,
        gram_id: String,
    },
}

/// One guest's registered device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GuestDevice {
    pub guest_id: String,
    #[serde(flatten)]
    pub device: RegisteredDevice,
}

/// Register or replace a device for this guest. The same phone may retain
/// independent registrations and notification preferences for other grants.
pub(crate) fn register(dir: &Path, guest_id: &str, device: RegisteredDevice) -> io::Result<()> {
    store::update_side(dir, DEVICES_FILE, |devices: &mut Vec<GuestDevice>| {
        devices.retain(|existing| {
            existing.guest_id != guest_id || existing.device.device_token != device.device_token
        });
        let own = devices
            .iter()
            .filter(|existing| existing.guest_id == guest_id)
            .count();
        if own >= MAX_DEVICES_PER_GUEST {
            let oldest = devices
                .iter()
                .enumerate()
                .filter(|(_, existing)| existing.guest_id == guest_id)
                .min_by_key(|(_, existing)| existing.device.registered_unix_ms)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                devices.remove(index);
            }
        }
        devices.push(GuestDevice {
            guest_id: guest_id.to_string(),
            device,
        });
        ((), true)
    })
}

/// Remove one of this guest's devices. Returns whether it was registered.
pub(crate) fn unregister(dir: &Path, guest_id: &str, device_token: &str) -> io::Result<bool> {
    store::update_side(dir, DEVICES_FILE, |devices: &mut Vec<GuestDevice>| {
        let before = devices.len();
        devices.retain(|existing| {
            existing.guest_id != guest_id || existing.device.device_token != device_token
        });
        let removed = devices.len() != before;
        (removed, removed)
    })
}

/// Remove every device of a revoked guest.
pub(crate) fn remove_guest(dir: &Path, guest_id: &str) -> io::Result<()> {
    store::update_side(dir, DEVICES_FILE, |devices: &mut Vec<GuestDevice>| {
        let before = devices.len();
        devices.retain(|existing| existing.guest_id != guest_id);
        ((), devices.len() != before)
    })
}

pub(crate) fn devices(dir: &Path) -> io::Result<Vec<GuestDevice>> {
    store::load_side(dir, DEVICES_FILE)
}

fn remove_tokens(dir: &Path, tokens: &HashSet<String>) -> io::Result<()> {
    store::update_side(dir, DEVICES_FILE, |devices: &mut Vec<GuestDevice>| {
        let before = devices.len();
        devices.retain(|existing| !tokens.contains(&existing.device.device_token));
        ((), devices.len() != before)
    })
}

/// Whether `scope` concerns this guest: its granted agent (same terminal, name
/// and kind), or a Gram from it while the guest has `share_gram`.
fn reaches(scope: &GuestScope, guest: &GuestRecord) -> bool {
    if guest.revoked {
        return false;
    }
    match scope {
        GuestScope::Agent {
            terminal_id,
            name,
            kind,
        } => super::grant_names(&guest.grant, terminal_id, name.as_deref(), kind),
        GuestScope::Gram { from, sender, .. } => {
            guest.share_gram && super::grant_sent(&guest.grant, from, sender.as_ref())
        }
    }
}

/// The guest's alert: the owner's title, and for a status change the owner's
/// machine label instead of the workspace context. `herdr_guest` routes a tap
/// into the guest screens.
fn payload(
    notification: &PushNotification,
    scope: &GuestScope,
    guest: &GuestRecord,
    host_id: &str,
) -> String {
    let (kind, body, gram_id) = match scope {
        GuestScope::Agent { .. } => ("status", guest.machine_label.as_str(), None),
        GuestScope::Gram { gram_id, .. } => ("gram", notification.body.as_str(), Some(gram_id)),
    };
    let mut route = serde_json::json!({
        "host_id": host_id,
        "guest_id": guest.guest_id,
        "kind": kind,
    });
    if let Some(gram_id) = gram_id {
        route["gram_id"] = serde_json::json!(gram_id);
    }
    serde_json::json!({
        "aps": {
            "alert": {"title": notification.title, "body": body},
            "sound": "default",
        },
        "herdr_guest": route,
    })
    .to_string()
}

/// Every guest alert one batch sends: one payload per (alert, guest) with a
/// device that opted into its kind, and that payload's sends per route.
pub(crate) fn plan<'a>(
    cfg: &PushConfig,
    notifications: &[PushNotification],
    guests: &[GuestRecord],
    devices: &'a [GuestDevice],
    host_id: &str,
) -> AlertPlan<'a> {
    let mut plan = AlertPlan {
        payloads: Vec::new(),
        direct: Vec::new(),
        relayed: Vec::new(),
    };
    for notification in notifications {
        let Some(scope) = &notification.guest_scope else {
            continue;
        };
        for guest in guests.iter().filter(|guest| reaches(scope, guest)) {
            let mut sends = devices
                .iter()
                .filter(|registered| registered.guest_id == guest.guest_id)
                .map(|registered| &registered.device)
                .filter(|device| crate::push::device_accepts(device, notification))
                .map(|device| {
                    let route = crate::push::route(cfg, device.relay_capability.as_deref());
                    (route, device)
                })
                .filter(|(route, _)| *route != Route::Skip)
                .peekable();
            if sends.peek().is_none() {
                continue;
            }
            let index = plan.payloads.len();
            plan.payloads
                .push(payload(notification, scope, guest, host_id));
            for (route, device) in sends {
                match route {
                    Route::Direct => plan.direct.push((index, device)),
                    Route::Relay => plan.relayed.push((index, device)),
                    Route::Skip => {}
                }
            }
        }
    }
    plan
}

/// Deliver `notifications` to the guests they concern. Runs on the push
/// sender thread after the owner's devices; best-effort like them.
pub(crate) fn deliver(cfg: &PushConfig, notifications: &[PushNotification]) {
    if notifications
        .iter()
        .all(|notification| notification.guest_scope.is_none())
    {
        return;
    }
    let dir = store::guest_dir();
    // No guest ever registered: leave the guest directory alone.
    if !dir.join(DEVICES_FILE).exists() {
        return;
    }
    let loaded = devices(&dir).and_then(|devices| {
        if devices.is_empty() {
            return Ok(None);
        }
        let guests = store::load_store(&dir)?.guests;
        let (host, _) = store::load_host(&dir)?;
        Ok(Some((devices, guests, host.host_id)))
    });
    let (devices, guests, host_id) = match loaded {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(err = %err, "guest push skipped: guest store unavailable");
            return;
        }
    };
    let plan = plan(cfg, notifications, &guests, &devices, &host_id);
    let gone = crate::push::send_plan(cfg, &plan);
    if !gone.is_empty() {
        if let Err(err) = remove_tokens(&dir, &gone) {
            tracing::warn!(err = %err, "failed to prune guest device tokens");
        }
    }
}
