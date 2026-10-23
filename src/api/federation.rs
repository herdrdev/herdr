//! Shared shapes for the federation TCP transport handshake.
//!
//! A federation TCP client sends one `federation.hello` line carrying a
//! versioned, token-bearing envelope before any request. The inbound listener
//! ([`crate::api::server`]) checks the handshake version, validates the token in
//! constant time against the configured peer set, and binds the connection to
//! that peer's [`CapabilityTier`] before it will dispatch a single request. The
//! outbound client ([`crate::api::client`]) writes the same line right after it
//! connects. Both sides agree on the exact wire shape here so neither can drift
//! from the other.
//!
//! Only inbound TCP federation connections are gated this way. The local unix
//! socket is never token-checked or tier-filtered.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::CapabilityTier;

/// The `type` discriminator every federation hello line carries.
pub(crate) const FEDERATION_HELLO_TYPE: &str = "federation.hello";

/// Version of the federation handshake envelope. Independent of the control-plane
/// [`crate::protocol::PROTOCOL_VERSION`]: this versions only the `federation.hello`
/// line shape and its negotiation, so the two can evolve separately. Bump it when
/// the hello envelope changes incompatibly.
pub(crate) const FEDERATION_PROTOCOL_VERSION: u32 = 1;

/// First line a federation TCP client sends: a versioned, token-bearing hello.
///
/// The wire form is exactly
/// `{"type":"federation.hello","machine_id":"<m>","proto_version":<v>,"token":"<t>"}`.
/// A request line (which has no `token` field, or a different `type`) fails to
/// deserialize into this struct or fails the [`FEDERATION_HELLO_TYPE`] check, so
/// a client that skips the hello is rejected rather than dispatched.
///
/// `Debug` is implemented by hand to redact the token; never derive it.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FederationHello {
    #[serde(rename = "type")]
    pub kind: String,
    /// Install-stable identifier for the connecting daemon, persisted across
    /// restarts by [`crate::persist::machine`]. The token, not this field, is the
    /// authenticator; but when the receiving peer pins an `expected_node_id`, this
    /// value must match it or the connection is rejected. Empty when the client
    /// has no persisted id available.
    pub machine_id: String,
    /// Handshake envelope version the client speaks. See
    /// [`FEDERATION_PROTOCOL_VERSION`].
    pub proto_version: u32,
    pub token: String,
}

impl fmt::Debug for FederationHello {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Keep the envelope fields visible for diagnostics, but never print the
        // shared secret — a stray `{hello:?}` in a log must not leak the token.
        f.debug_struct("FederationHello")
            .field("kind", &self.kind)
            .field("machine_id", &self.machine_id)
            .field("proto_version", &self.proto_version)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl FederationHello {
    pub(crate) fn new(token: impl Into<String>) -> Self {
        Self {
            kind: FEDERATION_HELLO_TYPE.to_string(),
            // Empty by default; the outbound client stamps the persisted install
            // id via `with_machine_id` right before it writes the line.
            machine_id: String::new(),
            proto_version: FEDERATION_PROTOCOL_VERSION,
            token: token.into(),
        }
    }

    /// Stamp the persisted per-install machine id onto the hello. Called by the
    /// outbound client so `FederationHello::new` stays independent of the persist
    /// layer.
    pub(crate) fn with_machine_id(mut self, machine_id: String) -> Self {
        self.machine_id = machine_id;
        self
    }

    /// Serialize to the single-line wire form (no trailing newline).
    pub(crate) fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Parse a received hello line and return the whole envelope only when the
    /// line is a well-formed hello with the expected `type`. Any other shape (a
    /// stray request line, wrong `type`, missing fields) yields `None`.
    pub(crate) fn from_line(line: &str) -> Option<FederationHello> {
        match serde_json::from_str::<FederationHello>(line) {
            Ok(hello) if hello.kind == FEDERATION_HELLO_TYPE => Some(hello),
            _ => None,
        }
    }
}

/// The peer a federation connection is bound to after a successful hello: the
/// configured alias, the capability tier the matching token grants, and an
/// optional pinned node identity. When `expected_node_id` is `Some`, the hello's
/// `machine_id` must equal it or the connection is rejected after the token
/// check; when `None`, no identity pin is applied (back-compat).
#[derive(Debug, Clone)]
pub(crate) struct PeerContext {
    pub alias: String,
    pub tier: CapabilityTier,
    pub expected_node_id: Option<String>,
}

/// Capability required to invoke an API method over a federation connection, or
/// [`FederationAccess::Denied`] when the method is not exposed to federated peers
/// at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FederationAccess {
    /// Reachable by any peer whose tier is `>=` this tier.
    AllowedAt(CapabilityTier),
    /// Never reachable over federation, at any tier.
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FederationMethodPolicy {
    pub(crate) access: FederationAccess,
    pub(crate) outbound_routable: bool,
}

/// Canonical federation ownership and capability policy for one wire method.
///
/// `outbound_routable` decides whether an alias-qualified target may leave the
/// home daemon. `access` decides the minimum capability accepted by the remote
/// daemon. Unknown methods are denied in both directions.
pub(crate) fn federation_method_policy(method_wire_name: &str) -> FederationMethodPolicy {
    use CapabilityTier::{Admin, Interact, Observe};
    use FederationAccess::AllowedAt;
    let (access, outbound_routable) = match method_wire_name {
        // Observe methods whose target can be owned by a remote peer.
        // `workspace.get`/`tab.get`/`tab.list` route an `<alias>/…` workspace or
        // tab id to its owning peer; a bare `tab.list` stays local.
        "agent.get" | "agent.read" | "agent.transcript" | "agent.explain" | "pane.read"
        | "pane.history" | "pane.turns" | "pane.stream" | "workspace.get" | "tab.list"
        | "tab.get" => (AllowedAt(Observe), true),
        // Other read-only inspection stays local when called at the home.
        "ping"
        | "agent.list"
        | "accounts.list"
        | "agent.kinds"
        | "agent.wait"
        | "pane.get"
        | "pane.list"
        | "pane.current"
        | "pane.wait_for_output"
        | "pane.process_info"
        | "pane.neighbor"
        | "pane.edges"
        | "events.subscribe"
        | "events.wait"
        | "session.snapshot"
        | "workspace.list"
        | "worktree.list"
        | "layout.export" => (AllowedAt(Observe), false),
        // Interact methods with an explicit remote-owned target.
        "agent.prompt" | "agent.send_keys" | "pane.send_text" | "pane.send_keys" => {
            (AllowedAt(Interact), true)
        }
        // Admin methods with an explicit remote-owned target.
        "agent.focus" | "agent.rename" | "agent.restart" | "agent.answer" | "pane.send_input"
        | "pane.input.set" | "pane.rename" | "pane.set_pty_size" => (AllowedAt(Admin), true),
        // Admin methods whose payload has no remote ownership target.
        "agent.view.set" | "agent.view.clear" => (AllowedAt(Admin), false),
        // Everything else (all server.*, plugin.*, integration.*, gram.*,
        // notification(s).*, client.window_title.*, agent.start,
        // agent.transfer_session, pane.close, popup.close, and other mutations)
        // is denied to federation regardless of tier.
        _ => (FederationAccess::Denied, false),
    };
    FederationMethodPolicy {
        access,
        outbound_routable,
    }
}

pub(crate) fn federation_access(method_wire_name: &str) -> FederationAccess {
    federation_method_policy(method_wire_name).access
}

/// Compare two byte strings without an early return, so the time taken does not
/// reveal how many leading bytes matched. Length still short-circuits — a token
/// length is not a useful secret — but the byte comparison itself is uniform.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Return the peer whose token matches `candidate`, or `None` when none does.
///
/// Every entry is scanned with no short-circuit (the loop never breaks on the
/// first match) so acceptance timing does not reveal which token matched or how
/// many peers were configured. If two peers share a token the last one wins, but
/// the scan cost is identical either way.
pub(crate) fn authorized_peer(
    candidate: &str,
    peers: &[(String, PeerContext)],
) -> Option<PeerContext> {
    let mut matched: Option<PeerContext> = None;
    for (token, peer) in peers {
        if constant_time_eq(candidate.as_bytes(), token.as_bytes()) {
            matched = Some(peer.clone());
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peers(entries: &[(&str, CapabilityTier)]) -> Vec<(String, PeerContext)> {
        entries
            .iter()
            .map(|(token, tier)| {
                (
                    (*token).to_string(),
                    PeerContext {
                        alias: format!("alias-{token}"),
                        tier: *tier,
                        expected_node_id: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn hello_round_trips_to_the_exact_wire_shape() {
        let line = FederationHello::new("s3cret").to_line().unwrap();
        assert_eq!(
            line,
            r#"{"type":"federation.hello","machine_id":"","proto_version":1,"token":"s3cret"}"#
        );
        let parsed = FederationHello::from_line(&line).expect("parses back");
        assert_eq!(parsed.token, "s3cret");
        assert_eq!(parsed.machine_id, "");
        assert_eq!(parsed.proto_version, FEDERATION_PROTOCOL_VERSION);
    }

    #[test]
    fn with_machine_id_populates_and_round_trips() {
        let hello = FederationHello::new("tok").with_machine_id("machine_abc123".into());
        let line = hello.to_line().unwrap();
        let parsed = FederationHello::from_line(&line).expect("parses back");
        assert_eq!(parsed.machine_id, "machine_abc123");
        assert_eq!(parsed.token, "tok");
    }

    #[test]
    fn debug_redacts_the_token() {
        let hello = FederationHello::new("super-secret-token");
        let rendered = format!("{hello:?}");
        assert!(
            rendered.contains("<redacted>"),
            "debug output should redact the token: {rendered}"
        );
        assert!(
            !rendered.contains("super-secret-token"),
            "debug output leaked the token: {rendered}"
        );
        // The non-secret envelope fields stay visible for diagnostics.
        assert!(rendered.contains("federation.hello"));
        assert!(rendered.contains("proto_version"));
    }

    #[test]
    fn a_request_line_is_not_a_hello() {
        // A normal request line has no token field and a different (absent) type.
        assert!(FederationHello::from_line(r#"{"id":"1","method":"ping"}"#).is_none());
        // Right shape, wrong type discriminator.
        assert!(FederationHello::from_line(
            r#"{"type":"other","machine_id":"","proto_version":1,"token":"x"}"#
        )
        .is_none());
        // Not JSON at all.
        assert!(FederationHello::from_line("garbage").is_none());
    }

    #[test]
    fn constant_time_eq_matches_std_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn authorized_peer_returns_the_matching_peer() {
        let set = peers(&[
            ("alpha", CapabilityTier::Observe),
            ("beta", CapabilityTier::Admin),
        ]);
        assert_eq!(
            authorized_peer("alpha", &set).map(|p| p.tier),
            Some(CapabilityTier::Observe)
        );
        assert_eq!(
            authorized_peer("beta", &set).map(|p| p.tier),
            Some(CapabilityTier::Admin)
        );
        assert_eq!(
            authorized_peer("beta", &set).map(|p| p.alias).as_deref(),
            Some("alias-beta")
        );
        assert!(authorized_peer("gamma", &set).is_none());
        assert!(authorized_peer("alpha", &[]).is_none());
    }

    #[test]
    fn capability_tiers_are_ordered_observe_lt_interact_lt_admin() {
        assert!(CapabilityTier::Observe < CapabilityTier::Interact);
        assert!(CapabilityTier::Interact < CapabilityTier::Admin);
        assert_eq!(CapabilityTier::default(), CapabilityTier::Observe);
    }

    #[test]
    fn federation_access_classifies_representative_methods() {
        use FederationAccess::{AllowedAt, Denied};
        assert_eq!(
            federation_access("ping"),
            AllowedAt(CapabilityTier::Observe)
        );
        assert_eq!(
            federation_access("agent.prompt"),
            AllowedAt(CapabilityTier::Interact)
        );
        assert_eq!(
            federation_access("agent.rename"),
            AllowedAt(CapabilityTier::Admin)
        );
        // Default-deny for anything unlisted, including an unknown/new name.
        assert_eq!(federation_access("server.stop"), Denied);
        assert_eq!(federation_access("agent.start"), Denied);
        assert_eq!(federation_access("agent.transfer_session"), Denied);
        assert_eq!(federation_access("pane.close"), Denied);
        assert_eq!(federation_access("some.method.added.later"), Denied);
    }
}
