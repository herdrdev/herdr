use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Instant;

use super::health::{EndpointHealth, HealthAction};
use super::ClientEndpointId;
use crate::protocol::ClientMessage;

pub(crate) trait EndpointTransport: Send {
    fn send(&mut self, message: &ClientMessage) -> io::Result<()>;

    fn disconnect(&mut self) {}

    fn flush(&mut self, _deadline: Instant) -> io::Result<()> {
        Ok(())
    }

    fn take_error(&mut self) -> Option<io::Error> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EndpointNegotiation {
    methods: HashSet<String>,
    capabilities: HashSet<String>,
}

impl EndpointNegotiation {
    pub(crate) fn new(methods: Vec<String>, capabilities: Vec<String>) -> Self {
        Self {
            methods: methods.into_iter().collect(),
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub(crate) fn methods(&self) -> Vec<String> {
        self.methods.iter().cloned().collect()
    }

    pub(crate) fn supports_method(&self, method: &str) -> bool {
        self.methods.contains(method)
    }

    pub(crate) fn supports_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    pub(crate) fn supports_surface_interest(&self) -> bool {
        self.supports_capability(crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY)
            && self.supports_capability(
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY,
            )
            && self.supports_method("client_shell.surface.set")
    }

    pub(crate) fn supports_health_check(&self) -> bool {
        self.supports_capability(crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY)
    }
}

pub(crate) struct EndpointConnection {
    transport: Box<dyn EndpointTransport>,
    pub(crate) generation: u64,
    pub(crate) surface_active: bool,
    pub(crate) negotiation: EndpointNegotiation,
    health: Option<EndpointHealth>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointTransportFailure {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) generation: u64,
    pub(crate) kind: io::ErrorKind,
    pub(crate) message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointSendOutcome {
    Sent,
    NotSent,
}

pub(crate) struct EndpointRegistry {
    active: ClientEndpointId,
    input_enabled: bool,
    connections: HashMap<ClientEndpointId, EndpointConnection>,
    failures: Vec<EndpointTransportFailure>,
}

impl EndpointRegistry {
    pub(crate) fn empty() -> Self {
        Self {
            active: ClientEndpointId::Local,
            input_enabled: false,
            connections: HashMap::new(),
            failures: Vec::new(),
        }
    }

    pub(crate) fn new(
        local: impl EndpointTransport + 'static,
        generation: u64,
        negotiation: EndpointNegotiation,
    ) -> Self {
        let mut registry = Self::empty();
        registry.input_enabled = true;
        registry.insert(
            ClientEndpointId::Local,
            local,
            generation,
            negotiation,
            true,
        );
        registry
    }

    pub(crate) fn active_id(&self) -> &ClientEndpointId {
        &self.active
    }

    pub(crate) fn active_surface_available(&self) -> bool {
        self.input_enabled
            && self
                .connections
                .get(&self.active)
                .is_some_and(|connection| connection.surface_active)
    }

    pub(crate) fn select_unavailable_local(&mut self) {
        self.active = ClientEndpointId::Local;
        self.freeze_input();
    }

    pub(crate) fn freeze_input(&mut self) {
        self.input_enabled = false;
    }

    pub(crate) fn unfreeze_input(&mut self) {
        self.input_enabled = true;
    }

    pub(crate) fn connection(&self, endpoint_id: &ClientEndpointId) -> Option<&EndpointConnection> {
        self.connections.get(endpoint_id)
    }

    pub(crate) fn insert(
        &mut self,
        endpoint_id: ClientEndpointId,
        transport: impl EndpointTransport + 'static,
        generation: u64,
        negotiation: EndpointNegotiation,
        surface_active: bool,
    ) {
        let health = (!endpoint_id.is_local() && negotiation.supports_health_check())
            .then(|| EndpointHealth::new(Instant::now()));
        if let Some(mut previous) = self.connections.insert(
            endpoint_id,
            EndpointConnection {
                transport: Box::new(transport),
                generation,
                surface_active,
                negotiation,
                health,
            },
        ) {
            previous.transport.disconnect();
        }
    }

    pub(crate) fn accepts(&self, endpoint_id: &ClientEndpointId, generation: u64) -> bool {
        self.connections
            .get(endpoint_id)
            .is_some_and(|connection| connection.generation == generation)
    }

    pub(crate) fn received(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        now: Instant,
    ) {
        if let Some(health) = self
            .connections
            .get_mut(endpoint_id)
            .filter(|connection| connection.generation == generation)
            .and_then(|connection| connection.health.as_mut())
        {
            health.received(now);
        }
    }

    pub(crate) fn mark_ready(&mut self, endpoint_id: &ClientEndpointId, generation: u64) {
        if let Some(health) = self
            .connections
            .get_mut(endpoint_id)
            .filter(|connection| connection.generation == generation)
            .and_then(|connection| connection.health.as_mut())
        {
            health.ready();
        }
    }

    /// The primary --remote endpoint has a Local identity but crosses a network bridge.
    /// Probe it only when the server advertises support; actual Local sockets keep their
    /// existing transport-only liveness behavior.
    pub(crate) fn enable_remote_health(&mut self, endpoint_id: &ClientEndpointId, now: Instant) {
        if let Some(connection) = self.connections.get_mut(endpoint_id) {
            if connection.negotiation.supports_health_check() {
                connection.health = Some(EndpointHealth::new(now));
            }
        }
    }

    pub(crate) fn tick_health(&mut self, now: Instant) {
        let actions = self
            .connections
            .iter()
            .filter_map(|(endpoint_id, connection)| {
                connection
                    .health
                    .as_ref()
                    .map(|health| (endpoint_id.clone(), health.action(now)))
            })
            .filter(|(_, action)| *action != HealthAction::None)
            .collect::<Vec<_>>();
        for (endpoint_id, action) in actions {
            match action {
                HealthAction::None => {}
                HealthAction::Ping => {
                    let ping = ClientMessage::EndpointControl {
                        kind: crate::protocol::endpoint::HEALTH_PING_KIND.into(),
                        data: String::new(),
                    };
                    if self.send_to(&endpoint_id, &ping) == EndpointSendOutcome::Sent {
                        if let Some(health) = self
                            .connections
                            .get_mut(&endpoint_id)
                            .and_then(|connection| connection.health.as_mut())
                        {
                            health.ping_sent(now);
                        }
                    }
                }
                HealthAction::Expired => self.record_failure(
                    endpoint_id,
                    io::Error::new(io::ErrorKind::TimedOut, "endpoint health check timed out"),
                ),
            }
        }
    }

    pub(crate) fn set_active(&mut self, endpoint_id: &ClientEndpointId) -> bool {
        if !self
            .connections
            .get(endpoint_id)
            .is_some_and(|connection| connection.surface_active)
        {
            return false;
        }
        self.active = endpoint_id.clone();
        true
    }

    pub(crate) fn set_surface_active(
        &mut self,
        endpoint_id: &ClientEndpointId,
        active: bool,
    ) -> bool {
        let Some(connection) = self.connections.get_mut(endpoint_id) else {
            return false;
        };
        let changed = connection.surface_active != active;
        connection.surface_active = active;
        changed
    }

    pub(crate) fn send(&mut self, message: &ClientMessage) -> EndpointSendOutcome {
        let endpoint_id = self.active.clone();
        self.send_to(&endpoint_id, message)
    }

    pub(crate) fn send_to(
        &mut self,
        endpoint_id: &ClientEndpointId,
        message: &ClientMessage,
    ) -> EndpointSendOutcome {
        let result = self
            .connections
            .get_mut(endpoint_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "endpoint is unavailable"))
            .and_then(|connection| connection.transport.send(message));
        match result {
            Ok(()) => EndpointSendOutcome::Sent,
            Err(error) => {
                self.record_failure(endpoint_id.clone(), error);
                EndpointSendOutcome::NotSent
            }
        }
    }

    pub(crate) fn disconnect(&mut self, endpoint_id: &ClientEndpointId) {
        self.failures
            .retain(|failure| &failure.endpoint_id != endpoint_id);
        if let Some(mut connection) = self.connections.remove(endpoint_id) {
            connection.transport.disconnect();
        }
    }

    pub(crate) fn fail(&mut self, endpoint_id: &ClientEndpointId, error: io::Error) {
        self.record_failure(endpoint_id.clone(), error);
    }

    pub(crate) fn take_failures(&mut self) -> Vec<EndpointTransportFailure> {
        let errors = self
            .connections
            .iter_mut()
            .filter_map(|(id, connection)| {
                connection
                    .transport
                    .take_error()
                    .map(|error| (id.clone(), error))
            })
            .collect::<Vec<_>>();
        for (endpoint_id, error) in errors {
            self.record_failure(endpoint_id, error);
        }
        std::mem::take(&mut self.failures)
    }

    fn record_failure(&mut self, endpoint_id: ClientEndpointId, error: io::Error) {
        let Some(generation) = self
            .connections
            .get(&endpoint_id)
            .map(|connection| connection.generation)
        else {
            return;
        };
        let failure = EndpointTransportFailure {
            endpoint_id: endpoint_id.clone(),
            generation,
            kind: error.kind(),
            message: error.to_string(),
        };
        if let Some(mut connection) = self.connections.remove(&endpoint_id) {
            connection.transport.disconnect();
        }
        if let Some(existing) = self
            .failures
            .iter_mut()
            .find(|existing| existing.endpoint_id == endpoint_id)
        {
            *existing = failure;
        } else {
            self.failures.push(failure);
        }
    }
}

impl Drop for EndpointRegistry {
    fn drop(&mut self) {
        let deadline = Instant::now() + std::time::Duration::from_millis(250);
        for connection in self.connections.values_mut() {
            let _ = connection.transport.send(&ClientMessage::Detach);
        }
        for connection in self.connections.values_mut() {
            let _ = connection.transport.flush(deadline);
            connection.transport.disconnect();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct FakeTransport {
        sent: Arc<Mutex<Vec<ClientMessage>>>,
        error: Option<io::ErrorKind>,
    }

    impl EndpointTransport for FakeTransport {
        fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
            if let Some(kind) = self.error {
                return Err(io::Error::new(kind, "fake transport failure"));
            }
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }
    }

    fn negotiation() -> EndpointNegotiation {
        EndpointNegotiation::new(
            vec!["client_shell.surface.set".into()],
            vec![
                crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY.into(),
            ],
        )
    }

    fn profile() -> crate::client::endpoint::ProfileId {
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap()
    }

    #[test]
    fn endpoint_failures_do_not_remove_other_connections() {
        let local_sent = Arc::new(Mutex::new(Vec::new()));
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: local_sent.clone(),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: Some(io::ErrorKind::BrokenPipe),
            },
            2,
            negotiation(),
            true,
        );
        assert!(registry.set_active(&ssh_id));

        assert_eq!(
            registry.send(&ClientMessage::ClientShellFocus { focused: true }),
            EndpointSendOutcome::NotSent
        );
        assert!(registry.connection(&ssh_id).is_none());
        assert!(registry.connection(&ClientEndpointId::Local).is_some());
        assert_eq!(registry.take_failures()[0].endpoint_id, ssh_id);

        assert!(registry.set_active(&ClientEndpointId::Local));
        assert_eq!(
            registry.send(&ClientMessage::ClientShellFocus { focused: true }),
            EndpointSendOutcome::Sent
        );
        assert_eq!(local_sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn reconnecting_active_identity_does_not_count_as_an_active_surface() {
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            2,
            negotiation(),
            true,
        );
        assert!(registry.set_active(&ssh_id));
        assert!(registry.active_surface_available());
        registry.disconnect(&ssh_id);
        registry.insert(
            ssh_id,
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            3,
            negotiation(),
            false,
        );
        assert!(!registry.active_surface_available());
    }

    #[test]
    fn recovered_local_uses_transport_failure_not_remote_health_probes() {
        let mut registry = EndpointRegistry::empty();
        let sent = Arc::new(Mutex::new(Vec::new()));
        registry.insert(
            ClientEndpointId::Local,
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            false,
        );
        registry.tick_health(Instant::now() + std::time::Duration::from_secs(300));
        assert!(registry.connection(&ClientEndpointId::Local).is_some());
        assert!(sent.lock().unwrap().is_empty());
        assert!(registry.take_failures().is_empty());
    }

    #[test]
    fn primary_remote_health_detects_a_silent_bridge_without_replaying_input() {
        let now = Instant::now();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            1,
            negotiation(),
        );
        registry.enable_remote_health(&ClientEndpointId::Local, now);
        registry.mark_ready(&ClientEndpointId::Local, 1);
        registry.tick_health(now + super::super::health::HEARTBEAT_INTERVAL);
        assert!(matches!(
            sent.lock().unwrap().as_slice(),
            [ClientMessage::EndpointControl { kind, .. }]
                if kind == crate::protocol::endpoint::HEALTH_PING_KIND
        ));
        registry.tick_health(
            now + super::super::health::HEARTBEAT_INTERVAL
                + super::super::health::HEARTBEAT_TIMEOUT,
        );
        assert!(registry.connection(&ClientEndpointId::Local).is_none());
        assert!(!registry.active_surface_available());
        assert_eq!(registry.take_failures()[0].kind, io::ErrorKind::TimedOut);
        let input = ClientMessage::Input {
            data: b"offline input".to_vec(),
        };
        assert_eq!(registry.send(&input), EndpointSendOutcome::NotSent);
        registry.insert(
            ClientEndpointId::Local,
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            false,
        );
        assert_eq!(
            sent.lock().unwrap().len(),
            1,
            "offline input is never queued"
        );
        assert!(!registry.accepts(&ClientEndpointId::Local, 1));
    }

    #[test]
    fn primary_remote_health_is_optional_for_older_servers() {
        let now = Instant::now();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            1,
            EndpointNegotiation::default(),
        );
        registry.enable_remote_health(&ClientEndpointId::Local, now);
        registry.tick_health(now + std::time::Duration::from_secs(300));
        assert!(registry.connection(&ClientEndpointId::Local).is_some());
        assert!(sent.lock().unwrap().is_empty());
        assert!(registry.take_failures().is_empty());
    }

    #[test]
    fn negotiated_remote_health_probe_expires_the_connection() {
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        let sent = Arc::new(Mutex::new(Vec::new()));
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            false,
        );
        let now = Instant::now();
        registry.tick_health(now + super::super::health::HEARTBEAT_INTERVAL);
        assert!(matches!(
            sent.lock().unwrap().as_slice(),
            [ClientMessage::EndpointControl { kind, .. }]
                if kind == crate::protocol::endpoint::HEALTH_PING_KIND
        ));

        registry.tick_health(
            now + super::super::health::HEARTBEAT_INTERVAL
                + super::super::health::HEARTBEAT_TIMEOUT,
        );
        assert!(registry.connection(&ssh_id).is_none());
        assert_eq!(registry.take_failures()[0].kind, io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_ready_endpoint_can_stay_connected_after_the_initial_deadline() {
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            2,
            negotiation(),
            false,
        );
        let now = Instant::now();
        registry.mark_ready(&ssh_id, 2);
        registry.received(&ssh_id, 2, now + super::super::health::HEARTBEAT_INTERVAL);
        registry.tick_health(now + super::super::health::HEARTBEAT_TIMEOUT);
        assert!(registry.connection(&ssh_id).is_some());
    }

    #[test]
    fn negotiated_surface_interest_requires_capability_and_method() {
        assert!(negotiation().supports_surface_interest());
        assert!(negotiation().supports_health_check());
        assert!(
            !EndpointNegotiation::new(vec!["client_shell.surface.set".into()], Vec::new())
                .supports_surface_interest()
        );
        assert!(!EndpointNegotiation::new(
            Vec::new(),
            vec![crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into()]
        )
        .supports_surface_interest());
        assert!(!EndpointNegotiation::new(
            vec!["client_shell.surface.set".into()],
            vec![crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into()]
        )
        .supports_surface_interest());
    }

    #[test]
    fn dropping_registry_detaches_every_connected_endpoint() {
        let local_sent = Arc::new(Mutex::new(Vec::new()));
        let remote_sent = Arc::new(Mutex::new(Vec::new()));
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: local_sent.clone(),
                error: None,
            },
            1,
            negotiation(),
        );
        registry.insert(
            ClientEndpointId::Ssh(profile()),
            FakeTransport {
                sent: remote_sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            false,
        );

        drop(registry);

        assert!(matches!(
            local_sent.lock().unwrap().as_slice(),
            [ClientMessage::Detach]
        ));
        assert!(matches!(
            remote_sent.lock().unwrap().as_slice(),
            [ClientMessage::Detach]
        ));
    }

    #[test]
    fn stale_generations_are_rejected() {
        let registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            7,
            negotiation(),
        );
        assert!(registry.accepts(&ClientEndpointId::Local, 7));
        assert!(!registry.accepts(&ClientEndpointId::Local, 6));
    }
}
