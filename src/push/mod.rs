//! Remote push (APNs) delivery of agent-state transitions to registered devices.
//!
//! Design: Herdr does no in-process outbound networking (see `src/update.rs`,
//! which shells out to curl). This module keeps that property — the APNs auth
//! JWT is signed in-process with `p256` (see `jwt`), and each alert is delivered
//! by spawning `curl --http2` (see `apns`). Delivery is best-effort and always
//! runs off the app loop: failures are logged via `tracing`, never propagated.
//!
//! Hosts without their own APNs key deliver through the HerdrUp push relay
//! instead (see `relay`); `push.mode` and [`route`] pick the path per device.
//!
//! Secrets: the `.p8` key is read from `push.key_path` at send time only; its
//! contents are never persisted or logged. Only device tokens, per-device
//! preferences and opaque relay capabilities live in `devices.json`.

mod apns;
mod ask;
mod jwt;
mod relay;

use std::collections::HashSet;

use crate::api::schema::NotificationsStatusState;
use crate::config::{PushConfig, PushMode};
use crate::persist::activities::RegisteredActivity;
use crate::persist::devices::RegisteredDevice;

use self::apns::DeliveryOutcome;
use self::relay::{RelayOutcome, RelayPushType};

pub(crate) use self::relay::is_valid_capability as is_valid_relay_capability;
pub(crate) use self::ask::{AskSource, PushAsk};

/// Which agent transition triggered a push, used to match per-device prefs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushKind {
    /// Agent is blocked / needs input or attention.
    NeedsInput,
    /// Agent finished a turn.
    Finished,
    /// Agent pane process exited.
    Died,
    /// An agent sent the owner a gram message.
    Gram,
}

/// One agent-state transition to deliver as an APNs alert. `pane_id` and
/// `workspace_id` are the public API ids so the mobile client can deep-link.
#[derive(Debug, Clone)]
pub(crate) struct PushNotification {
    pub title: String,
    pub body: String,
    pub pane_id: String,
    pub workspace_id: String,
    pub kind: PushKind,
    /// The local agent or Gram this alert is about, so the guests granted
    /// that agent get it too. `None` for agents on other machines.
    #[cfg(unix)]
    pub guest_scope: Option<crate::guest::push::GuestScope>,
    /// Set by the app for a NeedsInput alert about a local omp agent: where to
    /// look for a pending `ask` on the push thread. Consumed by delivery.
    pub ask_source: Option<AskSource>,
    /// The question this alert lets the owner answer (`herdr_ask`), filled on
    /// the push thread from `ask_source`.
    pub ask: Option<PushAsk>,
}

/// True when the push config is complete enough to attempt delivery: the master
/// switch is on and every required identifier is present and non-blank. A blank
/// or whitespace-only value is treated as unset (it would only produce broken
/// requests).
pub(crate) fn enabled(cfg: &PushConfig) -> bool {
    cfg.enabled
        && is_present(&cfg.key_path)
        && is_present(&cfg.key_id)
        && is_present(&cfg.team_id)
        && is_present(&cfg.topic)
}

/// Delivery path for one registered device or Live Activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Direct,
    Relay,
    Skip,
}

/// Pick the delivery path for one registration under `push.mode`:
/// `direct` needs the complete key config, `relay` needs a capability, `auto`
/// prefers direct and falls back to the relay, and `off` sends nothing.
pub(crate) fn route(cfg: &PushConfig, relay_capability: Option<&str>) -> Route {
    let via_relay = || {
        if relay_capability.is_some() && relay_configured(cfg) {
            Route::Relay
        } else {
            Route::Skip
        }
    };
    match cfg.mode {
        PushMode::Off => Route::Skip,
        PushMode::Direct if enabled(cfg) => Route::Direct,
        PushMode::Direct => Route::Skip,
        PushMode::Relay => via_relay(),
        PushMode::Auto if enabled(cfg) => Route::Direct,
        PushMode::Auto => via_relay(),
    }
}

fn relay_configured(cfg: &PushConfig) -> bool {
    !cfg.relay_url.trim().is_empty()
}

/// App-loop guard: true unless `cfg` routes every registration to [`Route::Skip`].
/// The stores are read off-loop, so this only rules out configs that can never send.
pub(crate) fn may_deliver(cfg: &PushConfig) -> bool {
    match cfg.mode {
        PushMode::Off => false,
        PushMode::Direct => enabled(cfg),
        PushMode::Relay => relay_configured(cfg),
        PushMode::Auto => enabled(cfg) || relay_configured(cfg),
    }
}

/// The `notifications.status` state. `relay_devices` counts registered devices
/// carrying a relay capability; `no_session` daemons keep no device registry.
pub(crate) fn status_state(
    no_session: bool,
    cfg: &PushConfig,
    relay_devices: usize,
) -> NotificationsStatusState {
    if no_session {
        return NotificationsStatusState::Unsupported;
    }
    let relay_ready = relay_configured(cfg) && relay_devices > 0;
    match cfg.mode {
        PushMode::Off => NotificationsStatusState::Off,
        PushMode::Direct | PushMode::Auto if enabled(cfg) => NotificationsStatusState::DirectReady,
        PushMode::Relay | PushMode::Auto if relay_ready => NotificationsStatusState::RelayReady,
        PushMode::Direct | PushMode::Relay | PushMode::Auto => {
            NotificationsStatusState::Unconfigured
        }
    }
}

/// Partition registrations into direct and relay sends, dropping skipped ones.
fn split_by_route<'a, T>(
    cfg: &PushConfig,
    items: &'a [T],
    relay_capability: impl Fn(&T) -> Option<&str>,
) -> (Vec<&'a T>, Vec<&'a T>) {
    let mut direct = Vec::new();
    let mut relayed = Vec::new();
    for item in items {
        match route(cfg, relay_capability(item)) {
            Route::Direct => direct.push(item),
            Route::Relay => relayed.push(item),
            Route::Skip => {}
        }
    }
    (direct, relayed)
}

fn is_present(value: &Option<String>) -> bool {
    value
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn device_wants(device: &RegisteredDevice, kind: PushKind) -> bool {
    match kind {
        PushKind::NeedsInput => device.notify_needs_input,
        PushKind::Finished => device.notify_finishes,
        PushKind::Died => device.notify_dies,
        PushKind::Gram => device.notify_gram,
    }
}

/// True when the owner has muted this notification's pane on this device.
/// A blank `pane_id` (gram pushes carry none) is never muted, so gram alerts
/// are unaffected by per-agent mutes.
fn device_muted(device: &RegisteredDevice, pane_id: &str) -> bool {
    !pane_id.is_empty() && device.muted_panes.iter().any(|muted| muted == pane_id)
}

/// The device opted into this kind and has not muted its pane.
pub(crate) fn device_accepts(device: &RegisteredDevice, notification: &PushNotification) -> bool {
    device_wants(device, notification.kind) && !device_muted(device, &notification.pane_id)
}

fn unix_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Hand a batch of alerts to a detached sender thread, so the app loop never
/// waits on curl or disk. Every alert (agent transitions, remote or local, and
/// gram messages) leaves through here; [`deliver`] then routes each device to
/// direct APNs or the relay with the same payload.
pub(crate) fn dispatch(cfg: PushConfig, notifications: Vec<PushNotification>) {
    if notifications.is_empty() {
        return;
    }
    #[cfg(test)]
    if test_sink::capture(|sink| sink.alerts.extend(notifications.iter().cloned())) {
        return;
    }
    // A named Builder means a thread-creation failure (EAGAIN) is handled
    // instead of panicking the app-loop thread.
    if let Err(err) = std::thread::Builder::new()
        .name("herdr-push".to_string())
        .spawn(move || deliver(cfg, notifications))
    {
        tracing::warn!(error = %err, "failed to spawn push sender thread; dropping batch");
    }
}

/// Like [`dispatch`], for alerts only guests get: a Gram whose owner copy
/// lives on another machine that notifies the owner itself.
#[cfg(unix)]
pub(crate) fn dispatch_guests(cfg: PushConfig, notifications: Vec<PushNotification>) {
    if notifications.is_empty() || !may_deliver(&cfg) {
        return;
    }
    #[cfg(test)]
    if test_sink::capture(|sink| sink.guest_alerts.extend(notifications.iter().cloned())) {
        return;
    }
    if let Err(err) = std::thread::Builder::new()
        .name("herdr-guest-push".to_string())
        .spawn(move || crate::guest::push::deliver(&cfg, &notifications))
    {
        tracing::warn!(error = %err, "failed to spawn guest push thread; dropping batch");
    }
}

/// Hand one Live Activity content-state to a detached sender thread; see
/// [`dispatch`].
pub(crate) fn dispatch_live_activity(
    cfg: PushConfig,
    content_state: serde_json::Value,
    timestamp: u64,
) {
    #[cfg(test)]
    if test_sink::capture(|sink| sink.live_activities.push(content_state.clone())) {
        return;
    }
    if let Err(err) = std::thread::Builder::new()
        .name("herdr-liveactivity".to_string())
        .spawn(move || deliver_live_activity(cfg, content_state, timestamp))
    {
        tracing::warn!(error = %err, "failed to spawn live-activity sender thread; dropping update");
    }
}

/// Test-only stand-in for the sender threads: while a [`test_sink::Capture`]
/// is alive on the current thread, [`dispatch`] and [`dispatch_live_activity`]
/// record what they would send instead of spawning curl.
#[cfg(test)]
pub(crate) mod test_sink {
    use std::cell::RefCell;

    use super::PushNotification;

    #[derive(Default)]
    pub(crate) struct Sent {
        pub alerts: Vec<PushNotification>,
        pub live_activities: Vec<serde_json::Value>,
        /// Alerts for guests alone (see [`super::dispatch_guests`]).
        #[cfg(unix)]
        pub guest_alerts: Vec<PushNotification>,
    }

    thread_local! {
        static SINK: RefCell<Option<Sent>> = const { RefCell::new(None) };
    }

    /// Records into the installed sink; `false` when none is installed.
    pub(super) fn capture(record: impl FnOnce(&mut Sent)) -> bool {
        SINK.with(|sink| match sink.borrow_mut().as_mut() {
            Some(sent) => {
                record(sent);
                true
            }
            None => false,
        })
    }

    /// Captures this thread's dispatches until dropped.
    pub(crate) struct Capture(());

    impl Capture {
        pub(crate) fn install() -> Self {
            SINK.with(|sink| *sink.borrow_mut() = Some(Sent::default()));
            Self(())
        }

        /// Everything dispatched since the last call.
        pub(crate) fn take(&self) -> Sent {
            SINK.with(|sink| sink.borrow_mut().as_mut().map(std::mem::take))
                .unwrap_or_default()
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            SINK.with(|sink| *sink.borrow_mut() = None);
        }
    }
}

/// Alert text on one line: newlines and tabs become spaces, other control
/// characters drop, whitespace runs collapse, and the text stops at
/// `max_chars`. `None` when nothing printable is left.
pub(crate) fn sanitized_notification_text(value: &str, max_chars: usize) -> Option<String> {
    let mut sanitized = String::new();
    let mut previous_space = false;
    for ch in value.chars() {
        let replacement = if ch == '\n' || ch == '\r' || ch == '\t' {
            Some(' ')
        } else if ch.is_control() {
            None
        } else {
            Some(ch)
        };
        let Some(ch) = replacement else {
            continue;
        };
        if ch.is_whitespace() {
            if previous_space {
                continue;
            }
            previous_space = true;
            sanitized.push(' ');
        } else {
            previous_space = false;
            sanitized.push(ch);
        }
        if sanitized.chars().count() >= max_chars {
            break;
        }
    }
    let sanitized = sanitized.trim().to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

/// Deliver a batch of agent transitions to every registered device that opted
/// into the matching notification kind, each over the path [`route`] picks
/// (direct APNs or the relay). Intended to be called on a detached thread: it
/// reads the device store, sends each alert via curl, and prunes any tokens APNs
/// or the relay reports as permanently invalid.
///
/// Best-effort throughout: every failure is logged and swallowed so a slow or
/// failing push never affects the app loop.
pub(crate) fn deliver(cfg: PushConfig, mut notifications: Vec<PushNotification>) {
    if notifications.is_empty() || !may_deliver(&cfg) {
        return;
    }
    ask::attach(&mut notifications);
    let devices = crate::persist::devices::load();
    let plan = plan_alerts(&cfg, &notifications, &devices);
    prune_device_tokens(send_plan(&cfg, &plan));
    #[cfg(unix)]
    crate::guest::push::deliver(&cfg, &notifications);
}

/// Send every alert in `plan` over its route. Returns the device tokens APNs
/// or the relay reported gone, for the caller to prune from its store.
pub(crate) fn send_plan(cfg: &PushConfig, plan: &AlertPlan<'_>) -> HashSet<String> {
    let mut gone = HashSet::new();
    if !plan.direct.is_empty() {
        gone.extend(deliver_direct(cfg, &plan.payloads, &plan.direct));
    }
    if !plan.relayed.is_empty() {
        gone.extend(deliver_relay(&cfg.relay_url, &plan.payloads, &plan.relayed));
    }
    gone
}

/// Every alert one batch sends: each notification's payload once, and each
/// `(payload index, device)` send on each route, notification by notification.
/// Direct APNs and the relay share this one payload and device filter (kind
/// preference and muted panes), so both modes deliver the same alerts.
pub(crate) struct AlertPlan<'a> {
    pub payloads: Vec<String>,
    pub direct: Vec<(usize, &'a RegisteredDevice)>,
    pub relayed: Vec<(usize, &'a RegisteredDevice)>,
}

pub(crate) fn plan_alerts<'a>(
    cfg: &PushConfig,
    notifications: &[PushNotification],
    devices: &'a [RegisteredDevice],
) -> AlertPlan<'a> {
    let (direct, relayed) =
        split_by_route(cfg, devices, |device| device.relay_capability.as_deref());
    let mut plan = AlertPlan {
        payloads: Vec::with_capacity(notifications.len()),
        direct: Vec::new(),
        relayed: Vec::new(),
    };
    for (index, notification) in notifications.iter().enumerate() {
        plan.payloads.push(apns::payload_body(notification));
        let accepting = |devices: &[&'a RegisteredDevice]| {
            devices
                .iter()
                .filter(|device| device_accepts(device, notification))
                .map(|device| (index, *device))
                .collect::<Vec<_>>()
        };
        plan.direct.extend(accepting(&direct));
        plan.relayed.extend(accepting(&relayed));
    }
    plan
}

/// Direct APNs delivery: mint/reuse one JWT from the host's `.p8` key and send
/// each alert straight to Apple. Returns the tokens APNs reported gone.
fn deliver_direct(
    cfg: &PushConfig,
    payloads: &[String],
    sends: &[(usize, &RegisteredDevice)],
) -> HashSet<String> {
    // Direct routing implies `enabled`, which guarantees these are all `Some`.
    let (Some(key_path), Some(key_id), Some(team_id), Some(topic)) = (
        cfg.key_path.as_deref(),
        cfg.key_id.as_deref(),
        cfg.team_id.as_deref(),
        cfg.topic.as_deref(),
    ) else {
        return HashSet::new();
    };

    // `key_path` is host config, not a secret: expand `~` (the form the docs and
    // config example use) and log the resolved path on failure.
    let resolved_key_path = crate::worktree::expand_tilde_path(key_path);
    let pem = match std::fs::read_to_string(&resolved_key_path) {
        Ok(pem) => pem,
        Err(err) => {
            tracing::warn!(
                path = %resolved_key_path.display(),
                error = %err,
                "failed to read APNs signing key; skipping push"
            );
            return HashSet::new();
        }
    };

    let mut jwt = match jwt::auth_token(&pem, key_id, team_id, unix_secs_now()) {
        Ok(jwt) => jwt,
        Err(err) => {
            tracing::warn!(error = %err, "failed to build APNs auth token; skipping push");
            return HashSet::new();
        }
    };
    // A 403 (bad token / clock skew) invalidates the cached JWT; re-mint it once
    // and retry. If it 403s again the credentials are wrong, so we stop rather
    // than hammer APNs with the whole batch.
    let mut reminted = false;

    let mut tokens_to_prune: HashSet<String> = HashSet::new();
    for (index, device) in sends {
        let payload = &payloads[*index];
        // A token already flagged for pruning gets no further sends.
        if tokens_to_prune.contains(&device.device_token) {
            continue;
        }
        let mut outcome =
            apns::deliver_one(&device.device_token, &jwt, topic, cfg.sandbox, payload);
        if outcome == DeliveryOutcome::AuthExpired && !reminted {
            reminted = true;
            jwt::clear_cache();
            match jwt::auth_token(&pem, key_id, team_id, unix_secs_now()) {
                Ok(fresh) => {
                    jwt = fresh;
                    outcome =
                        apns::deliver_one(&device.device_token, &jwt, topic, cfg.sandbox, payload);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to re-mint APNs auth token after 403; aborting push batch");
                    break;
                }
            }
        }
        match outcome {
            DeliveryOutcome::Delivered => {}
            DeliveryOutcome::PruneToken => {
                tokens_to_prune.insert(device.device_token.clone());
            }
            DeliveryOutcome::AuthExpired => {
                // Still 403 after re-mint (or a repeat): credentials are
                // wrong, not transient. Stop the batch (deliver_one already
                // logged the status and reason).
                tracing::warn!("apns auth token rejected (403) after re-mint; aborting push batch");
                break;
            }
            DeliveryOutcome::Failed => {
                // deliver_one already logged the status and APNs reason.
            }
        }
    }
    tokens_to_prune
}

/// Relay delivery of alerts to capability-bearing devices. The payload is the
/// same JSON the direct path sends. Returns the tokens the relay reported gone.
fn deliver_relay(
    relay_url: &str,
    payloads: &[String],
    sends: &[(usize, &RegisteredDevice)],
) -> HashSet<String> {
    let mut tokens_to_prune: HashSet<String> = HashSet::new();
    for (index, device) in sends {
        if tokens_to_prune.contains(&device.device_token) {
            continue;
        }
        let Some(capability) = device.relay_capability.as_deref() else {
            continue;
        };
        if relay::send(
            relay_url,
            capability,
            RelayPushType::Alert,
            &payloads[*index],
        ) == RelayOutcome::PruneToken
        {
            tokens_to_prune.insert(device.device_token.clone());
        }
    }
    tokens_to_prune
}

/// Deliver ONE Live Activity content-state update to every registered activity push token,
/// so the lock-screen / Dynamic Island widget refreshes while the app is closed. Mirrors
/// [`deliver`]: read the activity store, route each token (direct APNs with
/// `apns-push-type: liveactivity` on the widget sub-topic, or the relay), and prune any
/// token reported gone (410). Best-effort; intended to run on a detached thread.
pub(crate) fn deliver_live_activity(
    cfg: PushConfig,
    content_state: serde_json::Value,
    timestamp: u64,
) {
    if !may_deliver(&cfg) {
        return;
    }
    let activities = crate::persist::activities::load();
    let (direct, relayed) = split_by_route(&cfg, &activities, |activity| {
        activity.relay_capability.as_deref()
    });
    if direct.is_empty() && relayed.is_empty() {
        return;
    }
    // The whole session shares ONE content-state, so build the payload once. The timestamp
    // is assigned by the caller in SOURCE ORDER (see emit_live_activity_updates), not here
    // per-thread, so out-of-order sender threads can't let a stale snapshot win.
    let payload = apns::live_activity_payload(&content_state, timestamp);
    if !direct.is_empty() {
        deliver_live_activity_direct(&cfg, &payload, &direct);
    }
    if !relayed.is_empty() {
        let mut tokens_to_prune: HashSet<String> = HashSet::new();
        for activity in relayed {
            let Some(capability) = activity.relay_capability.as_deref() else {
                continue;
            };
            if relay::send(
                &cfg.relay_url,
                capability,
                RelayPushType::LiveActivity,
                &payload,
            ) == RelayOutcome::PruneToken
            {
                tokens_to_prune.insert(activity.activity_push_token.clone());
            }
        }
        prune_activity_tokens(tokens_to_prune);
    }
}

/// Direct APNs delivery of one Live Activity payload: mint/reuse one JWT and POST to each
/// token on the widget sub-topic.
fn deliver_live_activity_direct(
    cfg: &PushConfig,
    payload: &str,
    activities: &[&RegisteredActivity],
) {
    // Direct routing implies `enabled`, which guarantees these are all `Some`.
    let (Some(key_path), Some(key_id), Some(team_id), Some(topic)) = (
        cfg.key_path.as_deref(),
        cfg.key_id.as_deref(),
        cfg.team_id.as_deref(),
        cfg.topic.as_deref(),
    ) else {
        return;
    };

    let resolved_key_path = crate::worktree::expand_tilde_path(key_path);
    let pem = match std::fs::read_to_string(&resolved_key_path) {
        Ok(pem) => pem,
        Err(err) => {
            tracing::warn!(
                path = %resolved_key_path.display(),
                error = %err,
                "failed to read APNs signing key; skipping live-activity push"
            );
            return;
        }
    };

    let mut jwt = match jwt::auth_token(&pem, key_id, team_id, unix_secs_now()) {
        Ok(jwt) => jwt,
        Err(err) => {
            tracing::warn!(error = %err, "failed to build APNs auth token; skipping live-activity push");
            return;
        }
    };
    let mut reminted = false;

    // The Live Activity topic is a distinct sub-topic of the app bundle id.
    let la_topic = format!("{topic}.push-type.liveactivity");

    let mut tokens_to_prune: HashSet<String> = HashSet::new();
    for activity in activities {
        if tokens_to_prune.contains(&activity.activity_push_token) {
            continue;
        }
        let mut outcome = apns::deliver_one_typed(
            &activity.activity_push_token,
            &jwt,
            &la_topic,
            cfg.sandbox,
            "liveactivity",
            "5",
            payload,
        );
        if outcome == DeliveryOutcome::AuthExpired && !reminted {
            reminted = true;
            jwt::clear_cache();
            match jwt::auth_token(&pem, key_id, team_id, unix_secs_now()) {
                Ok(fresh) => {
                    jwt = fresh;
                    outcome = apns::deliver_one_typed(
                        &activity.activity_push_token,
                        &jwt,
                        &la_topic,
                        cfg.sandbox,
                        "liveactivity",
                        "5",
                        payload,
                    );
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to re-mint APNs auth token; aborting live-activity batch");
                    break;
                }
            }
        }
        match outcome {
            DeliveryOutcome::Delivered => {}
            DeliveryOutcome::PruneToken => {
                tokens_to_prune.insert(activity.activity_push_token.clone());
            }
            DeliveryOutcome::AuthExpired => {
                tracing::warn!(
                    "apns auth token rejected (403) after re-mint; aborting live-activity batch"
                );
                break;
            }
            DeliveryOutcome::Failed => {}
        }
    }

    prune_activity_tokens(tokens_to_prune);
}

fn prune_device_tokens(tokens: HashSet<String>) {
    for token in tokens {
        match crate::persist::devices::remove_token(&token) {
            Ok(true) => tracing::info!("pruned an unregistered APNs device token"),
            Ok(false) => {}
            Err(err) => tracing::warn!(error = %err, "failed to prune APNs device token"),
        }
    }
}

fn prune_activity_tokens(tokens: HashSet<String>) {
    for token in tokens {
        match crate::persist::activities::remove_token(&token) {
            Ok(true) => tracing::info!("pruned an unregistered live-activity token"),
            Ok(false) => {}
            Err(err) => tracing::warn!(error = %err, "failed to prune live-activity token"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, fill: bool) -> PushConfig {
        PushConfig {
            enabled,
            key_path: fill.then(|| "/tmp/AuthKey.p8".to_string()),
            key_id: fill.then(|| "ABC123DEFG".to_string()),
            team_id: fill.then(|| "TEAM123456".to_string()),
            topic: fill.then(|| "com.example.herdr".to_string()),
            ..PushConfig::default()
        }
    }

    fn device(needs_input: bool, dies: bool, finishes: bool) -> RegisteredDevice {
        RegisteredDevice {
            device_token: "token".to_string(),
            platform: "ios".to_string(),
            notify_needs_input: needs_input,
            notify_dies: dies,
            notify_finishes: finishes,
            notify_gram: false,
            muted_panes: Vec::new(),
            registered_unix_ms: 0,
            relay_capability: None,
        }
    }

    fn with_mode(mut cfg: PushConfig, mode: PushMode) -> PushConfig {
        cfg.mode = mode;
        cfg
    }

    const CAP: Option<&str> = Some("hpr1.AbC");

    #[test]
    fn route_per_mode_with_and_without_key_and_capability() {
        let keyed = cfg(true, true);
        let keyless = cfg(false, false);
        let cases = [
            // (mode, key config complete, capability, expected)
            (PushMode::Auto, &keyed, CAP, Route::Direct),
            (PushMode::Auto, &keyed, None, Route::Direct),
            (PushMode::Auto, &keyless, CAP, Route::Relay),
            (PushMode::Auto, &keyless, None, Route::Skip),
            (PushMode::Direct, &keyed, CAP, Route::Direct),
            (PushMode::Direct, &keyed, None, Route::Direct),
            (PushMode::Direct, &keyless, CAP, Route::Skip),
            (PushMode::Direct, &keyless, None, Route::Skip),
            (PushMode::Relay, &keyed, CAP, Route::Relay),
            (PushMode::Relay, &keyed, None, Route::Skip),
            (PushMode::Relay, &keyless, CAP, Route::Relay),
            (PushMode::Relay, &keyless, None, Route::Skip),
            (PushMode::Off, &keyed, CAP, Route::Skip),
            (PushMode::Off, &keyless, CAP, Route::Skip),
        ];
        for (mode, base, capability, expected) in cases {
            let cfg = with_mode(base.clone(), mode);
            assert_eq!(
                route(&cfg, capability),
                expected,
                "{mode:?} key={} cap={}",
                enabled(&cfg),
                capability.is_some()
            );
        }
    }

    #[test]
    fn relay_route_needs_a_relay_url() {
        let mut cfg = with_mode(cfg(false, false), PushMode::Relay);
        cfg.relay_url = "  ".to_string();
        assert_eq!(route(&cfg, CAP), Route::Skip);
        assert!(!may_deliver(&cfg));
        cfg.mode = PushMode::Auto;
        assert_eq!(route(&cfg, CAP), Route::Skip);
        assert!(!may_deliver(&cfg));
    }

    #[test]
    fn may_deliver_is_false_only_when_every_route_skips() {
        for base in [cfg(true, true), cfg(false, false)] {
            for mode in [
                PushMode::Auto,
                PushMode::Direct,
                PushMode::Relay,
                PushMode::Off,
            ] {
                let cfg = with_mode(base.clone(), mode);
                let any_route = route(&cfg, CAP) != Route::Skip || route(&cfg, None) != Route::Skip;
                assert_eq!(
                    may_deliver(&cfg),
                    any_route,
                    "{mode:?} key={}",
                    enabled(&cfg)
                );
            }
        }
    }

    #[test]
    fn split_by_route_partitions_devices_per_capability() {
        let mut plain = device(true, true, true);
        plain.device_token = "plain".to_string();
        let mut relayed = device(true, true, true);
        relayed.device_token = "relayed".to_string();
        relayed.relay_capability = CAP.map(str::to_string);
        let devices = [plain, relayed];
        fn tokens(items: Vec<&RegisteredDevice>) -> Vec<&str> {
            items
                .into_iter()
                .map(|device| device.device_token.as_str())
                .collect()
        }
        fn capability(device: &RegisteredDevice) -> Option<&str> {
            device.relay_capability.as_deref()
        }

        let (direct, relay) = split_by_route(&cfg(true, true), &devices, capability);
        assert_eq!(
            (tokens(direct), tokens(relay)),
            (vec!["plain", "relayed"], vec![])
        );

        let (direct, relay) = split_by_route(&cfg(false, false), &devices, capability);
        assert_eq!((tokens(direct), tokens(relay)), (vec![], vec!["relayed"]));

        let off = with_mode(cfg(true, true), PushMode::Off);
        let (direct, relay) = split_by_route(&off, &devices, capability);
        assert!(direct.is_empty() && relay.is_empty());
    }

    #[test]
    fn status_state_machine() {
        use NotificationsStatusState::*;
        let keyed = cfg(true, true);
        let keyless = cfg(false, false);
        let cases = [
            // (mode, key config, relay devices, expected)
            (PushMode::Auto, &keyed, 0, DirectReady),
            (PushMode::Auto, &keyed, 2, DirectReady),
            (PushMode::Auto, &keyless, 1, RelayReady),
            (PushMode::Auto, &keyless, 0, Unconfigured),
            (PushMode::Direct, &keyed, 0, DirectReady),
            (PushMode::Direct, &keyless, 3, Unconfigured),
            (PushMode::Relay, &keyed, 0, Unconfigured),
            (PushMode::Relay, &keyed, 1, RelayReady),
            (PushMode::Relay, &keyless, 1, RelayReady),
            (PushMode::Off, &keyed, 1, Off),
            (PushMode::Off, &keyless, 0, Off),
        ];
        for (mode, base, relay_devices, expected) in cases {
            let cfg = with_mode(base.clone(), mode);
            assert_eq!(
                status_state(false, &cfg, relay_devices),
                expected,
                "{mode:?} key={} relay_devices={relay_devices}",
                enabled(&cfg)
            );
            // No-session daemons keep no registry, whatever the config says.
            assert_eq!(status_state(true, &cfg, relay_devices), Unsupported);
        }

        let mut no_url = with_mode(keyless, PushMode::Relay);
        no_url.relay_url = String::new();
        assert_eq!(status_state(false, &no_url, 1), Unconfigured);
    }

    #[test]
    fn enabled_requires_switch_and_all_identifiers() {
        assert!(enabled(&cfg(true, true)));
        assert!(!enabled(&cfg(false, true)));
        assert!(!enabled(&cfg(true, false)));

        // A blank / whitespace-only identifier counts as unset.
        let mut blank = cfg(true, true);
        blank.topic = Some("   ".to_string());
        assert!(!enabled(&blank));
        let mut empty = cfg(true, true);
        empty.key_id = Some(String::new());
        assert!(!enabled(&empty));
    }

    #[test]
    fn device_wants_matches_kind_to_pref() {
        let d = device(true, false, false);
        assert!(device_wants(&d, PushKind::NeedsInput));
        assert!(!device_wants(&d, PushKind::Died));
        assert!(!device_wants(&d, PushKind::Finished));

        let d = device(false, true, true);
        assert!(device_wants(&d, PushKind::Died));
        assert!(device_wants(&d, PushKind::Finished));
        assert!(!device_wants(&d, PushKind::NeedsInput));
        // notify_gram is independent of the agent-transition prefs.
        assert!(!device_wants(&d, PushKind::Gram));
    }

    #[test]
    fn device_wants_gram_follows_notify_gram() {
        let mut d = device(false, false, false);
        assert!(!device_wants(&d, PushKind::Gram));
        d.notify_gram = true;
        assert!(device_wants(&d, PushKind::Gram));
    }

    #[test]
    fn device_muted_skips_only_muted_panes() {
        let mut d = device(true, true, true);
        d.muted_panes = vec!["w1:p2".to_string(), "w1:p5".to_string()];

        assert!(device_muted(&d, "w1:p2")); // muted
        assert!(device_muted(&d, "w1:p5")); // muted
        assert!(!device_muted(&d, "w1:p3")); // a different pane is not muted
                                             // Gram pushes carry an empty pane id and are never muted.
        assert!(!device_muted(&d, ""));

        // No mutes → nothing skipped.
        let clear = device(true, true, true);
        assert!(!device_muted(&clear, "w1:p2"));
    }
}
