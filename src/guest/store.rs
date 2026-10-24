//! Guest store (`guests.json`), host identity (`node_key`, `host.json`),
//! invites and admission. Everything lives in `<config>/guest/`: the directory
//! is 0700 and every file 0600, repaired on each load.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::api::schema::{GuestGrantInfo, GuestInfo, GuestInviteInfo};

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
pub(crate) const DEFAULT_INVITE_TTL_SECS: u64 = 24 * 60 * 60;
pub(crate) const MIN_INVITE_TTL_SECS: u64 = 60;
pub(crate) const MAX_INVITE_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// Unused invites are dropped from the store this long after they expire.
const EXPIRED_INVITE_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// `last_seen_ms` is rewritten at most this often, so each request does not
/// rewrite the store.
const LAST_SEEN_RESOLUTION_MS: u64 = 60 * 1000;
/// One `connected` audit entry per guest per this window: every guest request
/// is its own Noise session.
pub(crate) const CONNECTED_AUDIT_WINDOW_MS: u64 = 10 * 60 * 1000;
/// The link stays up this long after the latest revoke, so a revoked guest
/// hears `revoked` rather than `host_offline`.
pub(crate) const REVOKED_LINK_GRACE_MS: u64 = 24 * 60 * 60 * 1000;

pub(crate) fn guest_dir() -> PathBuf {
    crate::config::config_dir().join("guest")
}

pub(crate) fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .ok()
}

pub(crate) fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `SHA256:` plus lowercase hex of the first 8 bytes of SHA-256(device key),
/// grouped in 4s with `·`.
pub(crate) fn fingerprint(device_pub: &[u8; 32]) -> String {
    let hex = sha256_hex(device_pub);
    let groups: Vec<&str> = (0..4).map(|index| &hex[index * 4..index * 4 + 4]).collect();
    format!("SHA256:{}", groups.join("·"))
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$`.
pub(crate) fn valid_guest_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 32
        && first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub(crate) fn guest_label(name: &str) -> String {
    format!("{name} (via HerdrUp): ")
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// Create the directory 0700, or tighten an existing one.
pub(crate) fn ensure_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    repair_mode(dir, DIR_MODE)
}

fn repair_mode(path: &Path, mode: u32) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.permissions().mode() & 0o777 != mode => {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Read a private file, first tightening its mode to 0600.
fn read_private(path: &Path) -> io::Result<Option<Vec<u8>>> {
    repair_mode(path, FILE_MODE)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Atomically replace `path` with `bytes` through a 0600 temp file.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let written = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(&tmp)
        .and_then(|mut file| {
            file.set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
            file.write_all(bytes)?;
            file.sync_all()
        });
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Hold the store lock for one read-modify-write.
fn with_lock<T>(dir: &Path, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    ensure_dir(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(FILE_MODE)
        .open(dir.join(".guests.lock"))?;
    lock.lock()?;
    operation()
}

fn load_side_unlocked<T: Default + serde::de::DeserializeOwned>(
    dir: &Path,
    file: &str,
) -> io::Result<T> {
    match read_private(&dir.join(file))? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
        None => Ok(T::default()),
    }
}

/// Load a private JSON file kept beside `guests.json` (default when absent).
pub(crate) fn load_side<T: Default + serde::de::DeserializeOwned>(
    dir: &Path,
    file: &str,
) -> io::Result<T> {
    with_lock(dir, || load_side_unlocked(dir, file))
}

/// Read-modify-write a file kept beside `guests.json` under the store lock.
/// It is written only when `mutate` reports a change.
pub(crate) fn update_side<T, R>(
    dir: &Path,
    file: &str,
    mutate: impl FnOnce(&mut T) -> (R, bool),
) -> io::Result<R>
where
    T: Default + Serialize + serde::de::DeserializeOwned,
{
    with_lock(dir, || {
        let mut value = load_side_unlocked(dir, file)?;
        let (result, changed) = mutate(&mut value);
        if changed {
            write_private(&dir.join(file), &serde_json::to_vec_pretty(&value)?)?;
        }
        Ok(result)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HostIdentity {
    pub host_id: String,
    pub relay_secret: String,
}

/// Load or create the X25519 node key and `host.json`.
pub(crate) fn load_host(dir: &Path) -> io::Result<(HostIdentity, [u8; 32])> {
    with_lock(dir, || {
        let key_path = dir.join("node_key");
        let node_secret: [u8; 32] = match read_private(&key_path)? {
            Some(bytes) => bytes.try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "guest node_key is not 32 bytes; refusing to replace it",
                )
            })?,
            None => {
                let secret = random_bytes::<32>()?;
                write_private(&key_path, &secret)?;
                secret
            }
        };
        let host_path = dir.join("host.json");
        let host = match read_private(&host_path)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
            None => {
                let host = HostIdentity {
                    host_id: b64url(&random_bytes::<16>()?),
                    relay_secret: b64url(&random_bytes::<32>()?),
                };
                write_private(&host_path, &serde_json::to_vec_pretty(&host)?)?;
                host
            }
        };
        Ok((host, node_secret))
    })
}

/// X25519 public key for `secret`.
pub(crate) fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
    use snow::resolvers::{CryptoResolver as _, DefaultResolver};
    let mut dh = DefaultResolver
        .resolve_dh(&snow::params::DHChoice::Curve25519)
        .expect("snow always resolves Curve25519");
    dh.set(secret);
    let mut public = [0u8; 32];
    public.copy_from_slice(dh.pubkey());
    public
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GuestRecord {
    pub guest_id: String,
    pub name: String,
    /// b64url X25519 device public key. Never leaves this file.
    pub device_pub: String,
    pub fingerprint: String,
    #[serde(default)]
    pub device: String,
    pub grant: GuestGrantInfo,
    #[serde(default)]
    pub owner_name: String,
    #[serde(default)]
    pub machine_label: String,
    pub created_ms: u64,
    #[serde(default)]
    pub last_seen_ms: Option<u64>,
    /// Last `connected` audit entry. Tracked apart from `last_seen_ms`, which
    /// a polling client refreshes every minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected_ms: Option<u64>,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_ms: Option<u64>,
    /// The guest sees the agent's Gram from `created_ms` on.
    #[serde(default)]
    pub share_gram: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InviteRecord {
    pub invite_id: String,
    /// Hex SHA-256 of the decoded 32-byte secret. The secret itself is never stored.
    pub secret_sha256: String,
    pub name: String,
    pub grant: GuestGrantInfo,
    pub owner_name: String,
    pub machine_label: String,
    pub created_ms: u64,
    pub expires_ms: u64,
    #[serde(default)]
    pub used_by: Option<String>,
    #[serde(default)]
    pub share_gram: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct GuestStore {
    #[serde(default)]
    pub guests: Vec<GuestRecord>,
    #[serde(default)]
    pub invites: Vec<InviteRecord>,
}

impl GuestRecord {
    pub(crate) fn info(&self) -> GuestInfo {
        GuestInfo {
            guest_id: self.guest_id.clone(),
            name: self.name.clone(),
            fingerprint: self.fingerprint.clone(),
            device: self.device.clone(),
            grant: self.grant.clone(),
            created_ms: self.created_ms,
            last_seen_ms: self.last_seen_ms,
            revoked: self.revoked,
            share_gram: self.share_gram,
        }
    }
}

impl InviteRecord {
    pub(crate) fn info(&self) -> GuestInviteInfo {
        GuestInviteInfo {
            invite_id: self.invite_id.clone(),
            name: self.name.clone(),
            grant: self.grant.clone(),
            owner_name: self.owner_name.clone(),
            machine_label: self.machine_label.clone(),
            created_ms: self.created_ms,
            expires_ms: self.expires_ms,
            used_by: self.used_by.clone(),
            share_gram: self.share_gram,
        }
    }

    fn pending(&self, now: u64) -> bool {
        self.used_by.is_none() && now < self.expires_ms
    }
}

fn load_store_unlocked(dir: &Path) -> io::Result<GuestStore> {
    match read_private(&dir.join("guests.json"))? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
        None => Ok(GuestStore::default()),
    }
}

pub(crate) fn load_store(dir: &Path) -> io::Result<GuestStore> {
    with_lock(dir, || load_store_unlocked(dir))
}

/// Read-modify-write the store under the lock. The store is written only
/// when `mutate` reports a change.
pub(crate) fn update_store<T>(
    dir: &Path,
    mutate: impl FnOnce(&mut GuestStore) -> (T, bool),
) -> io::Result<T> {
    with_lock(dir, || {
        let mut store = load_store_unlocked(dir)?;
        let (value, changed) = mutate(&mut store);
        if changed {
            let now = now_ms();
            store.invites.retain(|invite| {
                invite.used_by.is_some()
                    || invite
                        .expires_ms
                        .saturating_add(EXPIRED_INVITE_RETENTION_MS)
                        > now
            });
            write_private(
                &dir.join("guests.json"),
                &serde_json::to_vec_pretty(&store)?,
            )?;
        }
        Ok(value)
    })
}

/// At least one non-revoked guest, one unexpired unused invite, or a revoke
/// within [`REVOKED_LINK_GRACE_MS`].
pub(crate) fn link_wanted_in(dir: &Path, now: u64) -> bool {
    load_store(dir).is_ok_and(|store| {
        store.guests.iter().any(|guest| {
            !guest.revoked
                || guest
                    .revoked_ms
                    .is_some_and(|at| now < at.saturating_add(REVOKED_LINK_GRACE_MS))
        }) || store.invites.iter().any(|invite| invite.pending(now))
    })
}

/// Record a resumed agent's new harness session on an active guest's grant.
/// Returns whether the stored grant changed.
pub(crate) fn update_grant_session(
    dir: &Path,
    guest_id: &str,
    session: &crate::api::schema::AgentSessionInfo,
) -> io::Result<bool> {
    update_store(dir, |store| {
        let Some(guest) = store
            .guests
            .iter_mut()
            .find(|guest| guest.guest_id == guest_id && !guest.revoked)
        else {
            return (false, false);
        };
        let changed = guest.grant.agent_session != *session;
        if changed {
            guest.grant.agent_session = session.clone();
        }
        (changed, changed)
    })
}

/// Whether an active guest shares the agent's Gram. False for a revoked or
/// unknown guest, or when the store cannot be read.
pub(crate) fn shares_gram(dir: &Path, guest_id: &str) -> bool {
    load_store(dir).is_ok_and(|store| {
        store
            .guests
            .iter()
            .any(|guest| guest.guest_id == guest_id && !guest.revoked && guest.share_gram)
    })
}

pub(crate) fn is_revoked(dir: &Path, guest_id: &str) -> bool {
    load_store(dir).map_or(true, |store| {
        store
            .guests
            .iter()
            .find(|guest| guest.guest_id == guest_id)
            .is_none_or(|guest| guest.revoked)
    })
}

pub(crate) struct NewInvite {
    pub record: InviteRecord,
    /// b64url secret; returned once, inside the links.
    pub secret: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_invite(
    dir: &Path,
    name: &str,
    grant: GuestGrantInfo,
    owner_name: &str,
    machine_label: &str,
    ttl_secs: u64,
    share_gram: bool,
    now: u64,
) -> io::Result<NewInvite> {
    let secret_bytes = random_bytes::<32>()?;
    let record = InviteRecord {
        invite_id: b64url(&random_bytes::<16>()?),
        secret_sha256: sha256_hex(&secret_bytes),
        name: name.to_string(),
        grant,
        owner_name: owner_name.to_string(),
        machine_label: machine_label.to_string(),
        created_ms: now,
        expires_ms: now.saturating_add(ttl_secs.saturating_mul(1000)),
        used_by: None,
        share_gram,
    };
    let stored = record.clone();
    update_store(dir, move |store| {
        store.invites.push(stored);
        ((), true)
    })?;
    Ok(NewInvite {
        record,
        secret: b64url(&secret_bytes),
    })
}

/// Turn Gram sharing on or off for an active guest. Returns the updated
/// record, or `None` when no active guest has this id.
pub(crate) fn update_share_gram(
    dir: &Path,
    guest_id: &str,
    share_gram: bool,
) -> io::Result<Option<GuestRecord>> {
    update_store(dir, |store| {
        let Some(guest) = store
            .guests
            .iter_mut()
            .find(|guest| guest.guest_id == guest_id && !guest.revoked)
        else {
            return (None, false);
        };
        let changed = guest.share_gram != share_gram;
        guest.share_gram = share_gram;
        (Some(guest.clone()), changed)
    })
}

/// The invite payload (JSON, then b64url) and its app and web links.
pub(crate) fn invite_links(
    relay_url: &str,
    host: &HostIdentity,
    host_pub: &[u8; 32],
    invite: &NewInvite,
) -> (String, String) {
    let relay = relay_url.trim_end_matches('/');
    let payload = serde_json::json!({
        "v": 1,
        "relay": relay,
        "host_id": host.host_id,
        "host_pub": b64url(host_pub),
        "invite_id": invite.record.invite_id,
        "secret": invite.secret,
        "machine_label": invite.record.machine_label,
        "owner_name": invite.record.owner_name,
        "agent_name": invite.record.grant.agent_name,
        "guest_name": invite.record.name,
        "expires_unix": invite.record.expires_ms / 1000,
    });
    let encoded = b64url(payload.to_string().as_bytes());
    (
        format!("herdrup://guest-invite#{encoded}"),
        format!("{relay}/i/{encoded}"),
    )
}

/// Outcome of a Noise message-1 payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdmitOutcome {
    /// `connected` is true when this admission should be audited as a new
    /// connection (first contact in [`CONNECTED_AUDIT_WINDOW_MS`]).
    Returning {
        guest: GuestRecord,
        connected: bool,
    },
    /// `replaced` lists this device's earlier guest ids, now revoked.
    Accepted {
        guest: GuestRecord,
        replaced: Vec<String>,
    },
    Refused(&'static str),
}

/// Admit a device. A returning guest sends `{"v":1,"guest_id":"..."}` to select
/// a grant belonging to its authenticated device key. Omitting `guest_id` works
/// only for a single active grant; ambiguous hellos get `guest_selection_required`.
/// A new guest instead sends `invite_id`, `secret` and `device`. The secret is
/// checked in constant time before the invite's state is revealed.
pub(crate) fn admit_in(
    dir: &Path,
    device_pub: &[u8; 32],
    hello: &serde_json::Value,
    now: u64,
) -> io::Result<AdmitOutcome> {
    if hello.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
        return Ok(AdmitOutcome::Refused("invite_invalid"));
    }
    let device = b64url(device_pub);
    let Some(invite_id) = hello.get("invite_id") else {
        let guest_id = match hello.get("guest_id") {
            None => None,
            Some(serde_json::Value::String(id)) if !id.is_empty() => Some(id.as_str()),
            Some(_) => return Ok(AdmitOutcome::Refused("unknown")),
        };
        return admit_returning(dir, &device, guest_id, now);
    };
    let (Some(invite_id), Some(secret)) = (
        invite_id.as_str(),
        hello.get("secret").and_then(serde_json::Value::as_str),
    ) else {
        return Ok(AdmitOutcome::Refused("invite_invalid"));
    };
    let secret_hash = b64url_decode(secret)
        .filter(|bytes| bytes.len() == 32)
        .map(|bytes| sha256_hex(&bytes));
    let device_name: String = hello
        .get("device")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let fingerprint = fingerprint(device_pub);
    update_store(dir, |store| {
        let Some(index) = store
            .invites
            .iter()
            .position(|invite| invite.invite_id == invite_id)
        else {
            return (AdmitOutcome::Refused("invite_invalid"), false);
        };
        let invite = &store.invites[index];
        let secret_ok = secret_hash.as_deref().is_some_and(|hash| {
            crate::api::federation::constant_time_eq(
                hash.as_bytes(),
                invite.secret_sha256.as_bytes(),
            )
        });
        if !secret_ok {
            return (AdmitOutcome::Refused("invite_invalid"), false);
        }
        if invite.used_by.is_some() {
            return (AdmitOutcome::Refused("invite_used"), false);
        }
        if now >= invite.expires_ms {
            return (AdmitOutcome::Refused("invite_expired"), false);
        }
        let guest_id = match random_bytes::<16>() {
            Ok(bytes) => b64url(&bytes),
            Err(_) => return (AdmitOutcome::Refused("invite_invalid"), false),
        };
        // Re-inviting the same terminal replaces that grant, not the device's
        // independently authorized access to other agents on this host.
        let mut replaced = Vec::new();
        for existing in store.guests.iter_mut().filter(|guest| {
            guest.device_pub == device
                && !guest.revoked
                && guest.grant.terminal_id == invite.grant.terminal_id
        }) {
            existing.revoked = true;
            existing.revoked_ms = Some(now);
            replaced.push(existing.guest_id.clone());
        }
        let guest = GuestRecord {
            guest_id: guest_id.clone(),
            name: invite.name.clone(),
            device_pub: device.clone(),
            fingerprint: fingerprint.clone(),
            device: device_name.clone(),
            grant: invite.grant.clone(),
            owner_name: invite.owner_name.clone(),
            machine_label: invite.machine_label.clone(),
            created_ms: now,
            last_seen_ms: Some(now),
            last_connected_ms: None,
            revoked: false,
            revoked_ms: None,
            share_gram: invite.share_gram,
        };
        store.invites[index].used_by = Some(guest_id);
        store.guests.push(guest.clone());
        (AdmitOutcome::Accepted { guest, replaced }, true)
    })
}

fn admit_returning(
    dir: &Path,
    device: &str,
    guest_id: Option<&str>,
    now: u64,
) -> io::Result<AdmitOutcome> {
    update_store(dir, |store| {
        // The authenticated Noise device key is always part of the lookup.
        // A guest id is a selector, never a credential or authority on its own.
        let matches = |guest: &&GuestRecord| {
            guest.device_pub == device && guest_id.is_none_or(|id| guest.guest_id == id)
        };
        let mut live = store
            .guests
            .iter()
            .enumerate()
            .filter(|(_, guest)| matches(guest) && !guest.revoked);
        let Some((selected, _)) = live.next() else {
            let known = store.guests.iter().any(|guest| matches(&guest));
            return (
                AdmitOutcome::Refused(if known { "revoked" } else { "unknown" }),
                false,
            );
        };
        // Old clients that omit the selector can reconnect only when unambiguous.
        // Never silently route one agent's request through another agent's grant.
        if live.next().is_some() {
            return (AdmitOutcome::Refused("guest_selection_required"), false);
        }
        let guest = &mut store.guests[selected];
        // The first returning request after acceptance is a connection, and
        // then one per window however often the client polls.
        let connected = guest
            .last_connected_ms
            .is_none_or(|at| now.saturating_sub(at) >= CONNECTED_AUDIT_WINDOW_MS);
        if connected {
            guest.last_connected_ms = Some(now);
        }
        let previous = guest.last_seen_ms.unwrap_or_default();
        let seen = now.saturating_sub(previous) >= LAST_SEEN_RESOLUTION_MS;
        if seen {
            guest.last_seen_ms = Some(now);
        }
        let changed = connected || seen;
        (
            AdmitOutcome::Returning {
                guest: guest.clone(),
                connected,
            },
            changed,
        )
    })
}

pub(crate) enum RevokeTarget<'a> {
    Guest(&'a str),
    Invite(&'a str),
}

/// Revoke a guest (kept, marked revoked) or delete an invite. Returns the
/// revoked guest record, or `None` when the id is unknown.
pub(crate) fn revoke_in(
    dir: &Path,
    target: RevokeTarget<'_>,
    now: u64,
) -> io::Result<Option<Option<GuestRecord>>> {
    update_store(dir, |store| match target {
        RevokeTarget::Guest(guest_id) => {
            match store
                .guests
                .iter_mut()
                .find(|guest| guest.guest_id == guest_id)
            {
                Some(guest) => {
                    if !guest.revoked {
                        guest.revoked = true;
                        guest.revoked_ms = Some(now);
                    }
                    (Some(Some(guest.clone())), true)
                }
                None => (None, false),
            }
        }
        RevokeTarget::Invite(invite_id) => {
            let before = store.invites.len();
            store.invites.retain(|invite| invite.invite_id != invite_id);
            let removed = store.invites.len() != before;
            (removed.then_some(None), removed)
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("herdr-guest-{tag}-{}-{nanos}", std::process::id()));
            Self(path.join("guest"))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = fs::remove_dir_all(parent);
            }
        }
    }

    /// Test clock anchored at the real time, since the store prunes stale
    /// invites against the wall clock.
    fn at(offset_ms: u64) -> u64 {
        static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *START.get_or_init(now_ms) + offset_ms
    }

    pub(crate) fn grant() -> GuestGrantInfo {
        serde_json::from_value(json!({
            "terminal_id": "term_1",
            "agent_name": "llm-opt",
            "agent_session": {"source": "herdr", "agent": "pi", "kind": "id", "value": "s-1"}
        }))
        .unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn invite(dir: &Path, now: u64) -> NewInvite {
        create_invite(
            dir,
            "plotarmordev",
            grant(),
            "Jerry",
            "Mac",
            3600,
            false,
            now,
        )
        .unwrap()
    }

    fn accept(invite: &NewInvite, device: &[u8; 32], now: u64, dir: &Path) -> AdmitOutcome {
        admit_in(
            dir,
            device,
            &json!({"v": 1, "invite_id": invite.record.invite_id, "secret": invite.secret, "device": "iPhone"}),
            now,
        )
        .unwrap()
    }

    #[test]
    fn files_are_private_and_modes_are_repaired_on_load() {
        let dir = TempDir::new("modes");
        let (host, secret) = load_host(&dir.0).unwrap();
        invite(&dir.0, at(1_000));
        assert_eq!(mode(&dir.0), 0o700);
        for file in ["node_key", "host.json", "guests.json"] {
            assert_eq!(mode(&dir.0.join(file)), 0o600, "{file}");
        }
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
        for file in ["node_key", "host.json", "guests.json"] {
            fs::set_permissions(dir.0.join(file), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let (again, again_secret) = load_host(&dir.0).unwrap();
        load_store(&dir.0).unwrap();
        assert_eq!(mode(&dir.0), 0o700);
        for file in ["node_key", "host.json", "guests.json"] {
            assert_eq!(mode(&dir.0.join(file)), 0o600, "{file} repaired");
        }
        assert_eq!(host.host_id, again.host_id, "host identity is stable");
        assert_eq!(secret, again_secret);
        assert_eq!(b64url_decode(&host.host_id).unwrap().len(), 16);
        assert_eq!(b64url_decode(&host.relay_secret).unwrap().len(), 32);
    }

    #[test]
    fn a_corrupt_node_key_is_never_silently_replaced() {
        let dir = TempDir::new("badkey");
        ensure_dir(&dir.0).unwrap();
        write_private(&dir.0.join("node_key"), b"short").unwrap();
        assert!(load_host(&dir.0).is_err());
        assert_eq!(fs::read(dir.0.join("node_key")).unwrap(), b"short");
    }

    #[test]
    fn x25519_public_matches_rfc7748_vector() {
        let secret: [u8; 32] =
            hex_bytes("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        assert_eq!(
            x25519_public(&secret),
            hex_bytes::<32>("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
    }

    fn hex_bytes<const N: usize>(hex: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
        }
        out
    }

    /// Messaging apps (iMessage) split a link at `#` and send the fragment as a second
    /// message, so the shareable web link must carry the invite in its path. The app link
    /// keeps the fragment: it never leaves the device.
    #[test]
    fn the_web_link_carries_the_invite_in_its_path() {
        let dir = TempDir::new("links");
        let invite = invite(&dir.0, at(1_000));
        let host = HostIdentity {
            host_id: "h".repeat(22),
            relay_secret: String::new(),
        };
        let (app, web) = invite_links("https://relay.test/", &host, &[9; 32], &invite);
        assert!(
            !web.contains('#'),
            "web link must not use a fragment: {web}"
        );
        let payload = web
            .strip_prefix("https://relay.test/i/")
            .expect("path form");
        assert_eq!(app, format!("herdrup://guest-invite#{payload}"));
        let decoded: serde_json::Value =
            serde_json::from_slice(&b64url_decode(payload).expect("b64url")).unwrap();
        assert_eq!(decoded["secret"], json!(invite.secret));
        assert_eq!(decoded["relay"], json!("https://relay.test"));
    }

    #[test]
    fn invite_is_single_use_and_binds_name_and_grant() {
        let dir = TempDir::new("single");
        let invite = invite(&dir.0, at(1_000));
        let AdmitOutcome::Accepted { guest, .. } = accept(&invite, &[7; 32], at(2_000), &dir.0)
        else {
            panic!("first use accepts");
        };
        assert_eq!(guest.name, "plotarmordev");
        assert_eq!(guest.grant, grant());
        assert_eq!(guest.device, "iPhone");
        assert_eq!(
            accept(&invite, &[8; 32], at(3_000), &dir.0),
            AdmitOutcome::Refused("invite_used")
        );
        assert_eq!(
            accept(&invite, &[7; 32], at(3_000), &dir.0),
            AdmitOutcome::Refused("invite_used"),
            "even the accepting device cannot reuse it"
        );
    }

    #[test]
    fn one_device_keeps_independent_agent_grants() {
        let dir = TempDir::new("multiple-grants");
        let first_invite = invite(&dir.0, at(1_000));
        let AdmitOutcome::Accepted { guest: first, .. } =
            accept(&first_invite, &[7; 32], at(2_000), &dir.0)
        else {
            panic!("first share accepts")
        };

        let mut second_grant = grant();
        second_grant.terminal_id = "term_2".into();
        second_grant.agent_name = Some("second-agent".into());
        let second_invite = create_invite(
            &dir.0,
            "plotarmordev",
            second_grant.clone(),
            "Jerry",
            "Mac",
            3600,
            true,
            at(3_000),
        )
        .unwrap();
        let AdmitOutcome::Accepted {
            guest: second,
            replaced,
        } = accept(&second_invite, &[7; 32], at(4_000), &dir.0)
        else {
            panic!("second share accepts")
        };
        assert!(
            replaced.is_empty(),
            "another agent must not revoke the first share"
        );

        for expected in [&first, &second] {
            let hello = json!({"v": 1, "guest_id": expected.guest_id});
            let AdmitOutcome::Returning { guest, .. } =
                admit_in(&dir.0, &[7; 32], &hello, at(5_000)).unwrap()
            else {
                panic!("both agent shares must remain usable")
            };
            assert_eq!(guest.guest_id, expected.guest_id);
            assert_eq!(guest.grant, expected.grant);
            assert_eq!(guest.share_gram, expected.share_gram);
            assert_eq!(
                admit_in(&dir.0, &[8; 32], &hello, at(5_000)).unwrap(),
                AdmitOutcome::Refused("unknown"),
                "a grant id does not authorize another device"
            );
        }
        assert_eq!(
            admit_in(&dir.0, &[7; 32], &json!({"v": 1}), at(5_000)).unwrap(),
            AdmitOutcome::Refused("guest_selection_required")
        );

        revoke_in(&dir.0, RevokeTarget::Guest(&first.guest_id), at(6_000)).unwrap();
        assert_eq!(
            admit_in(
                &dir.0,
                &[7; 32],
                &json!({"v": 1, "guest_id": first.guest_id}),
                at(7_000)
            )
            .unwrap(),
            AdmitOutcome::Refused("revoked")
        );
        let AdmitOutcome::Returning { guest, .. } = admit_in(
            &dir.0,
            &[7; 32],
            &json!({"v": 1, "guest_id": second.guest_id}),
            at(7_000),
        )
        .unwrap() else {
            panic!("revoking one share must leave the other usable")
        };
        assert_eq!(guest.grant, second_grant);
        assert!(guest.share_gram);
    }

    #[test]
    fn invite_expires() {
        let dir = TempDir::new("expiry");
        let invite = invite(&dir.0, at(1_000));
        assert_eq!(
            accept(&invite, &[7; 32], invite.record.expires_ms, &dir.0),
            AdmitOutcome::Refused("invite_expired")
        );
        assert!(!link_wanted_in(&dir.0, invite.record.expires_ms));
        assert!(link_wanted_in(&dir.0, invite.record.expires_ms - 1));
    }

    #[test]
    fn wrong_or_malformed_secret_is_invalid_and_reveals_nothing() {
        let dir = TempDir::new("secret");
        let invite = invite(&dir.0, at(1_000));
        // Flip one character of a valid secret.
        let mut forged = invite.secret.clone().into_bytes();
        forged[0] = if forged[0] == b'A' { b'B' } else { b'A' };
        let forged = String::from_utf8(forged).unwrap();
        for secret in [forged.as_str(), "", "not-base64!", &b64url(&[1; 31])] {
            let outcome = admit_in(
                &dir.0,
                &[7; 32],
                &json!({"v": 1, "invite_id": invite.record.invite_id, "secret": secret}),
                at(2_000),
            )
            .unwrap();
            assert_eq!(
                outcome,
                AdmitOutcome::Refused("invite_invalid"),
                "{secret:?}"
            );
        }
        // A wrong secret for an expired or used invite still answers
        // invite_invalid, so state is never revealed without the secret.
        let expired = admit_in(
            &dir.0,
            &[7; 32],
            &json!({"v": 1, "invite_id": invite.record.invite_id, "secret": forged}),
            invite.record.expires_ms + 1,
        )
        .unwrap();
        assert_eq!(expired, AdmitOutcome::Refused("invite_invalid"));
        let stored = fs::read_to_string(dir.0.join("guests.json")).unwrap();
        assert!(!stored.contains(&invite.secret), "secret is never stored");
        assert!(stored.contains(&sha256_hex(&b64url_decode(&invite.secret).unwrap())));
    }

    #[test]
    fn returning_devices_unknown_and_revoked() {
        let dir = TempDir::new("returning");
        let hello = json!({"v": 1});
        assert_eq!(
            admit_in(&dir.0, &[9; 32], &hello, at(1_000)).unwrap(),
            AdmitOutcome::Refused("unknown")
        );
        let invite = invite(&dir.0, at(1_000));
        let AdmitOutcome::Accepted { guest, .. } = accept(&invite, &[9; 32], at(2_000), &dir.0)
        else {
            panic!("accepts");
        };
        let AdmitOutcome::Returning { guest: back, .. } =
            admit_in(&dir.0, &[9; 32], &hello, at(3_000)).unwrap()
        else {
            panic!("returning guest admitted");
        };
        assert_eq!(back.guest_id, guest.guest_id);
        assert!(
            revoke_in(&dir.0, RevokeTarget::Guest(&guest.guest_id), at(3_500))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            admit_in(&dir.0, &[9; 32], &hello, at(4_000)).unwrap(),
            AdmitOutcome::Refused("revoked")
        );
        assert!(is_revoked(&dir.0, &guest.guest_id));
    }

    #[test]
    fn a_polling_guest_is_audited_connected_once_per_window() {
        let dir = TempDir::new("connected");
        let invite = invite(&dir.0, at(0));
        assert!(matches!(
            accept(&invite, &[2; 32], at(0), &dir.0),
            AdmitOutcome::Accepted { .. }
        ));
        let hello = json!({"v": 1});
        // A client polling every 20 s for 25 minutes.
        let connected: Vec<u64> = (1..=75)
            .map(|tick| tick * 20_000)
            .filter(|offset| {
                matches!(
                    admit_in(&dir.0, &[2; 32], &hello, at(*offset)).unwrap(),
                    AdmitOutcome::Returning {
                        connected: true,
                        ..
                    }
                )
            })
            .collect();
        let window = CONNECTED_AUDIT_WINDOW_MS;
        assert_eq!(
            connected,
            vec![20_000, 20_000 + window, 20_000 + 2 * window]
        );
    }

    #[test]
    fn the_link_stays_wanted_for_a_day_after_the_latest_revoke() {
        let dir = TempDir::new("grace");
        let invite = invite(&dir.0, at(0));
        let AdmitOutcome::Accepted { guest, .. } = accept(&invite, &[1; 32], at(0), &dir.0) else {
            panic!("accepts");
        };
        let revoked_at = at(10_000);
        revoke_in(&dir.0, RevokeTarget::Guest(&guest.guest_id), revoked_at).unwrap();
        // A repeated revoke does not extend the grace period.
        revoke_in(
            &dir.0,
            RevokeTarget::Guest(&guest.guest_id),
            revoked_at + 5_000,
        )
        .unwrap();
        assert!(link_wanted_in(
            &dir.0,
            revoked_at + REVOKED_LINK_GRACE_MS - 1
        ));
        assert!(!link_wanted_in(&dir.0, revoked_at + REVOKED_LINK_GRACE_MS));
        assert_eq!(
            admit_in(&dir.0, &[1; 32], &json!({"v": 1}), revoked_at + 1).unwrap(),
            AdmitOutcome::Refused("revoked")
        );
    }

    #[test]
    fn a_resumed_session_updates_only_an_active_grant() {
        let dir = TempDir::new("resume");
        let invite = invite(&dir.0, at(0));
        let AdmitOutcome::Accepted { guest, .. } = accept(&invite, &[1; 32], at(0), &dir.0) else {
            panic!("accepts");
        };
        let mut session = grant().agent_session;
        assert!(!update_grant_session(&dir.0, &guest.guest_id, &session).unwrap());
        session.value = "s-2".into();
        assert!(update_grant_session(&dir.0, &guest.guest_id, &session).unwrap());
        assert!(!update_grant_session(&dir.0, &guest.guest_id, &session).unwrap());
        let stored = load_store(&dir.0).unwrap();
        assert_eq!(stored.guests[0].grant.agent_session.value, "s-2");
        revoke_in(&dir.0, RevokeTarget::Guest(&guest.guest_id), at(1_000)).unwrap();
        session.value = "s-3".into();
        assert!(!update_grant_session(&dir.0, &guest.guest_id, &session).unwrap());
    }

    #[test]
    fn a_new_invite_replaces_the_same_devices_old_grant() {
        let dir = TempDir::new("replace");
        let first = invite(&dir.0, at(1_000));
        let AdmitOutcome::Accepted { guest: old, .. } = accept(&first, &[5; 32], at(2_000), &dir.0)
        else {
            panic!("accepts");
        };
        let second = invite(&dir.0, at(3_000));
        let AdmitOutcome::Accepted {
            guest: new,
            replaced,
        } = accept(&second, &[5; 32], at(4_000), &dir.0)
        else {
            panic!("accepts");
        };
        assert!(is_revoked(&dir.0, &old.guest_id));
        assert!(!is_revoked(&dir.0, &new.guest_id));
        assert_eq!(replaced, vec![old.guest_id]);
    }

    #[test]
    fn revoking_an_invite_deletes_it_and_unknown_ids_are_reported() {
        let dir = TempDir::new("revoke-invite");
        let invite = invite(&dir.0, at(1_000));
        assert_eq!(
            revoke_in(
                &dir.0,
                RevokeTarget::Invite(&invite.record.invite_id),
                at(3_500)
            )
            .unwrap(),
            Some(None)
        );
        assert_eq!(
            accept(&invite, &[7; 32], at(2_000), &dir.0),
            AdmitOutcome::Refused("invite_invalid")
        );
        assert_eq!(
            revoke_in(&dir.0, RevokeTarget::Guest("nope"), at(3_500)).unwrap(),
            None
        );
        assert_eq!(
            revoke_in(&dir.0, RevokeTarget::Invite("nope"), at(3_500)).unwrap(),
            None
        );
    }

    #[test]
    fn guest_name_rule() {
        for name in ["plotarmordev", "a", "A.b_c-9", &"x".repeat(32)] {
            assert!(valid_guest_name(name), "{name}");
        }
        for name in ["", ".a", "-a", "a b", "a/b", "Jerry:", &"x".repeat(33), "é"] {
            assert!(!valid_guest_name(name), "{name}");
        }
    }

    #[test]
    fn fingerprint_shape() {
        let print = fingerprint(&[0; 32]);
        assert!(print.starts_with("SHA256:"));
        let groups: Vec<&str> = print["SHA256:".len()..].split('·').collect();
        assert_eq!(groups.len(), 4);
        assert!(groups
            .iter()
            .all(|group| group.len() == 4 && group.chars().all(|c| c.is_ascii_hexdigit())));
        assert_eq!(groups.concat(), sha256_hex(&[0; 32])[..16]);
    }
}
