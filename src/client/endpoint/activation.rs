use std::time::{Duration, Instant};

use super::{ClientEndpointId, ClientEndpointStatus, EndpointRegistry, EndpointSendOutcome};

mod model;
mod protocol;
pub(crate) use model::{
    ActivationBeginError, ActivationCompletion, ActivationRollback, EndpointActivationIntent,
    PendingEndpointActivation, SurfaceActivationProgress,
};
use model::{ActivationEvidence, ActivationPhase, EndpointLease};
use protocol::*;

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);

fn release_surface_best_effort(
    lease: &EndpointLease,
    endpoints: &mut EndpointRegistry,
    request_id: String,
) {
    if !endpoints.accepts(&lease.endpoint_id, lease.generation) {
        return;
    }
    endpoints.set_surface_active(&lease.endpoint_id, false);
    let _ = endpoints.send_to(
        &lease.endpoint_id,
        &crate::protocol::ClientMessage::ClientShellFocus { focused: false },
    );
    match surface_interest_request(&lease.boot_id, request_id, false) {
        Ok(request) => {
            let _ = endpoints.send_to(&lease.endpoint_id, &request);
        }
        Err(error) => tracing::warn!(%error, "could not request abandoned surface cleanup"),
    }
}

impl PendingEndpointActivation {
    pub(crate) fn prepare(
        shell: &crate::client::shell::ClientShellState,
        endpoints: &EndpointRegistry,
        target: ClientEndpointId,
        focus: Option<crate::client::shell::ClientEndpointFocusTarget>,
        resize: crate::protocol::ClientMessage,
        serial: u64,
        now: Instant,
    ) -> Result<Self, ActivationBeginError> {
        resize_geometry(&resize).ok_or_else(|| {
            ActivationBeginError::Preflight(
                "endpoint activation did not include a surface resize".to_owned(),
            )
        })?;
        let source_id = endpoints.active_id().clone();
        let source_has_live_surface = endpoints
            .connection(&source_id)
            .is_some_and(|connection| connection.surface_active);
        let (source, source_available) = if source_has_live_surface {
            (
                endpoint_lease(shell, endpoints, &source_id)
                    .map_err(ActivationBeginError::Preflight)?,
                true,
            )
        } else {
            (disconnected_endpoint_lease(shell, &source_id), false)
        };
        let target_lease =
            endpoint_lease(shell, endpoints, &target).map_err(ActivationBeginError::Preflight)?;
        let source_compatible = !source_available
            || endpoints
                .connection(&source_id)
                .is_some_and(|connection| connection.negotiation.supports_surface_interest());
        let target_compatible = endpoints
            .connection(&target)
            .is_some_and(|connection| connection.negotiation.supports_surface_interest());
        if !source_compatible || !target_compatible {
            return Err(ActivationBeginError::Preflight(
                "endpoint must be updated before it can join the selected surface".into(),
            ));
        }

        let source_is_target = source.endpoint_id == target_lease.endpoint_id;
        // Validate every typed lifecycle and optional focus envelope before the first transport
        // write. Any error above this line is guaranteed not to have changed either endpoint.
        if source_available && !source_is_target {
            surface_interest_request(
                &source.boot_id,
                format!("client-shell-surface:{serial}:off"),
                false,
            )
            .map_err(|error| ActivationBeginError::Preflight(error.to_string()))?;
        }
        surface_interest_request(
            &target_lease.boot_id,
            format!("client-shell-surface:{serial}:on"),
            true,
        )
        .map_err(|error| ActivationBeginError::Preflight(error.to_string()))?;
        if let Some(target) = focus.as_ref() {
            focus_request(
                &target_lease.boot_id,
                format!("client-shell-focus:{serial}:1"),
                target,
            )
            .map_err(|error| ActivationBeginError::Preflight(error.to_string()))?;
        }

        Ok(Self {
            source,
            source_available,
            target: target_lease,
            focus,
            host_focused: shell.host_focus_baseline(),
            resize,
            phase: ActivationPhase::ReleasingSource {
                request_id: format!("client-shell-surface:{serial}:off"),
            },
            deadline: now + ACTIVATION_TIMEOUT,
            epoch: serial,
            next_focus_serial: 0,
            rollback_error: None,
            successor: None,
        })
    }

    pub(crate) fn start(
        mut self,
        endpoints: &mut EndpointRegistry,
    ) -> Result<Self, ActivationBeginError> {
        endpoints.freeze_input();
        match self.start_prepared(endpoints) {
            Ok(()) => Ok(self),
            Err(error) => Err(ActivationBeginError::Partial {
                activation: Box::new(self),
                error,
            }),
        }
    }

    fn start_prepared(&mut self, endpoints: &mut EndpointRegistry) -> Result<(), String> {
        let source_is_target = self.source.endpoint_id == self.target.endpoint_id;
        // Local must not depend on a remote acknowledgement to become usable.
        if source_is_target || !self.source_available || self.target.endpoint_id.is_local() {
            if self.source_available && !source_is_target {
                release_surface_best_effort(
                    &self.source,
                    endpoints,
                    format!("client-shell-surface:{}:off", self.epoch),
                );
            }
            return self.start_target(endpoints, self.resize.clone());
        }
        // Old servers emit PTY focus loss only while the viewer is still active.
        if endpoints.send_to(
            &self.source.endpoint_id,
            &crate::protocol::ClientMessage::ClientShellFocus { focused: false },
        ) != EndpointSendOutcome::Sent
        {
            return Err("source endpoint focus revoke could not be sent".into());
        }
        let request = surface_interest_request(
            &self.source.boot_id,
            format!("client-shell-surface:{}:off", self.epoch),
            false,
        )
        .map_err(|error| error.to_string())?;
        if endpoints.send_to(&self.source.endpoint_id, &request) != EndpointSendOutcome::Sent {
            return Err("source endpoint release could not be sent".into());
        }
        endpoints.set_surface_active(&self.source.endpoint_id, false);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn begin(
        shell: &crate::client::shell::ClientShellState,
        endpoints: &mut EndpointRegistry,
        target: ClientEndpointId,
        focus: Option<crate::client::shell::ClientEndpointFocusTarget>,
        resize: crate::protocol::ClientMessage,
        serial: u64,
        now: Instant,
    ) -> Result<Self, ActivationBeginError> {
        Self::prepare(shell, endpoints, target, focus, resize, serial, now)?.start(endpoints)
    }

    pub(crate) fn abandon(&self, endpoints: &mut EndpointRegistry) {
        endpoints.freeze_input();
        for lease in [&self.source, &self.target] {
            release_surface_best_effort(
                lease,
                endpoints,
                format!("client-shell-surface:{}:abandon", self.epoch),
            );
            if self.source.endpoint_id == self.target.endpoint_id {
                break;
            }
        }
    }

    pub(crate) fn target(&self) -> &ClientEndpointId {
        &self.target.endpoint_id
    }

    fn geometry(&self) -> crate::protocol::ClientSurfaceSize {
        resize_geometry(&self.resize).expect("activation resize was validated before construction")
    }

    pub(crate) fn presentation_sync_endpoint(&self) -> Option<&ClientEndpointId> {
        match self.phase {
            ActivationPhase::ActivatingTarget { .. } => Some(&self.target.endpoint_id),
            ActivationPhase::RestoringSource { .. } => Some(&self.source.endpoint_id),
            _ => None,
        }
    }

    /// The complete source command lane cannot safely cross source-off into a later presentation
    /// epoch. Other endpoint lanes are not part of this retirement.
    pub(crate) fn source_command_lane(&self) -> Option<&ClientEndpointId> {
        (self.source_available && self.source.endpoint_id != self.target.endpoint_id)
            .then_some(&self.source.endpoint_id)
    }

    pub(crate) fn can_retarget(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.target.endpoint_id == *endpoint_id
            && self.successor.is_none()
            && matches!(
                self.phase,
                ActivationPhase::ReleasingSource { .. } | ActivationPhase::ActivatingTarget { .. }
            )
    }

    /// Replace an in-flight handoff with the latest endpoint-qualified intent. The current
    /// transaction is still reversed through target-off/source-on; the replacement is launched
    /// by the caller only after the source's coherent restoration commits.
    pub(crate) fn supersede(
        &mut self,
        endpoint_id: ClientEndpointId,
        target: Option<crate::client::shell::ClientEndpointFocusTarget>,
        endpoints: &mut EndpointRegistry,
    ) -> ActivationRollback {
        self.successor = Some(EndpointActivationIntent {
            endpoint_id,
            target,
        });
        // Source-on is already ordered and must finish before any replacement is allowed to
        // begin. Later rapid selections only replace the retained intent; they never turn a
        // safe restoration into an unavailable state.
        let source_restoration_in_flight = matches!(
            self.phase,
            ActivationPhase::RestoringSource { .. }
        ) || matches!(
            &self.phase,
            ActivationPhase::SynchronizingPresentation { completion, .. }
                if matches!(completion.as_ref(), ActivationCompletion::RestoredSource { .. })
        );
        if source_restoration_in_flight {
            return ActivationRollback::Pending;
        }
        self.rollback(
            endpoints,
            "endpoint handoff superseded by a newer selection".into(),
            false,
        )
    }

    pub(crate) fn accepts_endpoint(&self, endpoint_id: &ClientEndpointId, generation: u64) -> bool {
        (self.source_available
            && self.source.endpoint_id == *endpoint_id
            && self.source.generation == generation)
            || (self.target.endpoint_id == *endpoint_id && self.target.generation == generation)
    }

    pub(crate) fn involves_endpoint(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.source.endpoint_id == *endpoint_id || self.target.endpoint_id == *endpoint_id
    }

    pub(crate) fn accepts_response(
        &self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        boot_id: &str,
        request_id: &str,
    ) -> bool {
        match &self.phase {
            ActivationPhase::ReleasingSource {
                request_id: expected,
            } => {
                endpoint_matches(&self.source, endpoint_id, generation, boot_id)
                    && expected == request_id
            }
            ActivationPhase::ActivatingTarget {
                request_id: expected,
                focus_request_id,
                ..
            } => {
                endpoint_matches(&self.target, endpoint_id, generation, boot_id)
                    && (expected == request_id || focus_request_id.as_deref() == Some(request_id))
            }
            ActivationPhase::ReleasingTargetForRollback {
                request_id: expected,
            } => {
                endpoint_matches(&self.target, endpoint_id, generation, boot_id)
                    && expected == request_id
            }
            ActivationPhase::RestoringSource {
                request_id: expected,
                ..
            } => {
                endpoint_matches(&self.source, endpoint_id, generation, boot_id)
                    && expected == request_id
            }
            ActivationPhase::SynchronizingPresentation {
                lease,
                request_id: expected,
                ..
            } => {
                endpoint_matches(lease, endpoint_id, generation, boot_id) && expected == request_id
            }
            ActivationPhase::AwaitingPresentationEffects { .. } => false,
        }
    }

    pub(crate) fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    #[cfg(test)]
    pub(crate) fn receive_response(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        request_id: &str,
        data: &[u8],
        endpoints: &mut EndpointRegistry,
    ) -> SurfaceActivationProgress {
        let boot_id = if self.source.endpoint_id == *endpoint_id {
            self.source.boot_id.clone()
        } else {
            self.target.boot_id.clone()
        };
        self.receive_response_for_boot(
            endpoint_id,
            generation,
            &boot_id,
            request_id,
            data,
            endpoints,
        )
    }

    pub(crate) fn receive_response_for_boot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        boot_id: &str,
        request_id: &str,
        data: &[u8],
        endpoints: &mut EndpointRegistry,
    ) -> SurfaceActivationProgress {
        if !self.accepts_response(endpoint_id, generation, boot_id, request_id) {
            return SurfaceActivationProgress::Stale;
        }
        let result = match decode_endpoint_response(request_id, data) {
            Ok(result) => result,
            Err(error) => {
                return SurfaceActivationProgress::Rejected {
                    source_release_rejected: matches!(
                        self.phase,
                        ActivationPhase::ReleasingSource { .. }
                    ) && error.code.is_some(),
                    message: error.message,
                };
            }
        };
        match &mut self.phase {
            ActivationPhase::ReleasingSource { .. } => {
                if let Err(message) = surface_set_revision(&result, false) {
                    return SurfaceActivationProgress::Rejected {
                        message,
                        source_release_rejected: false,
                    };
                }
                if let Err(message) = self.start_target(endpoints, self.resize.clone()) {
                    return SurfaceActivationProgress::Rejected {
                        message,
                        source_release_rejected: false,
                    };
                }
                SurfaceActivationProgress::Pending
            }
            ActivationPhase::ActivatingTarget {
                request_id: surface_request_id,
                acknowledged_revision,
                ..
            } if surface_request_id == request_id => {
                let revision = match surface_set_revision(&result, true) {
                    Ok(revision) => revision,
                    Err(message) => {
                        return SurfaceActivationProgress::Rejected {
                            message,
                            source_release_rejected: false,
                        };
                    }
                };
                *acknowledged_revision = Some(revision);
                self.progress()
            }
            ActivationPhase::ActivatingTarget {
                focus_request_id,
                focus_request_target,
                focus_acknowledged,
                ..
            } if focus_request_id.as_deref() == Some(request_id) => {
                let requested = focus_request_target.clone();
                let Some(requested) = requested else {
                    return SurfaceActivationProgress::Stale;
                };
                if !focus_result_matches(Some(&requested), &result) {
                    return SurfaceActivationProgress::Rejected {
                        message: "endpoint focus returned an unexpected result".into(),
                        source_release_rejected: false,
                    };
                }
                *focus_request_id = None;
                *focus_request_target = None;
                if self.focus != Some(requested) {
                    *focus_acknowledged = false;
                    if let Err(message) = self.send_latest_focus(endpoints) {
                        return SurfaceActivationProgress::Rejected {
                            message,
                            source_release_rejected: false,
                        };
                    }
                    return self.progress();
                }
                *focus_acknowledged = true;
                self.progress()
            }
            ActivationPhase::ReleasingTargetForRollback { .. } => {
                if let Err(message) = surface_set_revision(&result, false) {
                    return SurfaceActivationProgress::Rejected {
                        message,
                        source_release_rejected: false,
                    };
                }
                endpoints.set_surface_active(&self.target.endpoint_id, false);
                if !self.source_available {
                    return SurfaceActivationProgress::Rejected {
                        message: self.rollback_error.clone().unwrap_or_else(|| {
                            "the previous endpoint is no longer connected".into()
                        }),
                        source_release_rejected: false,
                    };
                }
                if let Err(message) = self.start_source_restore(endpoints, self.resize.clone()) {
                    return SurfaceActivationProgress::Rejected {
                        message,
                        source_release_rejected: false,
                    };
                }
                SurfaceActivationProgress::Pending
            }
            ActivationPhase::RestoringSource {
                acknowledged_revision,
                ..
            } => {
                let revision = match surface_set_revision(&result, true) {
                    Ok(revision) => revision,
                    Err(message) => {
                        return SurfaceActivationProgress::Rejected {
                            message,
                            source_release_rejected: false,
                        };
                    }
                };
                *acknowledged_revision = Some(revision);
                self.progress()
            }
            ActivationPhase::SynchronizingPresentation {
                acknowledged_revision,
                ..
            } => {
                let revision = match surface_set_revision(&result, true) {
                    Ok(revision) => revision,
                    Err(message) => {
                        return SurfaceActivationProgress::Rejected {
                            message,
                            source_release_rejected: false,
                        };
                    }
                };
                *acknowledged_revision = Some(revision);
                self.progress()
            }
            _ => SurfaceActivationProgress::Stale,
        }
    }

    pub(crate) fn receive_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: &crate::protocol::ClientShellSnapshot,
    ) -> SurfaceActivationProgress {
        let lease = match &self.phase {
            ActivationPhase::ActivatingTarget { .. } => &self.target,
            ActivationPhase::RestoringSource { .. } => &self.source,
            ActivationPhase::SynchronizingPresentation { lease, .. } => lease,
            _ => return SurfaceActivationProgress::Stale,
        };
        if !endpoint_matches(lease, endpoint_id, generation, &snapshot.boot_id)
            || snapshot.revision < lease.minimum_revision
        {
            return SurfaceActivationProgress::Stale;
        }
        let evidence = match &mut self.phase {
            ActivationPhase::ActivatingTarget { evidence, .. }
                if endpoint_matches(&self.target, endpoint_id, generation, &snapshot.boot_id) =>
            {
                evidence
            }
            ActivationPhase::RestoringSource { evidence, .. }
                if endpoint_matches(&self.source, endpoint_id, generation, &snapshot.boot_id) =>
            {
                evidence
            }
            ActivationPhase::SynchronizingPresentation {
                lease, evidence, ..
            } if endpoint_matches(lease, endpoint_id, generation, &snapshot.boot_id) => evidence,
            _ => return SurfaceActivationProgress::Stale,
        };
        evidence.record_snapshot(snapshot);
        self.progress()
    }

    pub(crate) fn receive_surface(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        surface: crate::protocol::PaneSurfaceFrame,
    ) -> SurfaceActivationProgress {
        let lease = match &self.phase {
            ActivationPhase::ActivatingTarget { .. } => &self.target,
            ActivationPhase::RestoringSource { .. } => &self.source,
            ActivationPhase::SynchronizingPresentation { lease, .. } => lease,
            _ => return SurfaceActivationProgress::Stale,
        };
        if !endpoint_matches(lease, endpoint_id, generation, &surface.boot_id) {
            return SurfaceActivationProgress::Stale;
        }
        if !surface_matches_geometry(&surface, self.geometry()) {
            return SurfaceActivationProgress::Pending;
        }
        match &mut self.phase {
            ActivationPhase::ActivatingTarget { evidence, .. }
            | ActivationPhase::RestoringSource { evidence, .. }
            | ActivationPhase::SynchronizingPresentation { evidence, .. } => {
                evidence.record_surface(surface)
            }
            _ => unreachable!("checked activation phase"),
        }
        self.progress()
    }

    pub(crate) fn receive_presentation_effects_ready(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        token: &str,
    ) -> SurfaceActivationProgress {
        let ActivationPhase::AwaitingPresentationEffects {
            lease,
            token: expected,
            ready,
            ..
        } = &mut self.phase
        else {
            return SurfaceActivationProgress::Stale;
        };
        if lease.endpoint_id != *endpoint_id || lease.generation != generation || expected != token
        {
            return SurfaceActivationProgress::Stale;
        }
        *ready = true;
        SurfaceActivationProgress::Ready
    }

    /// A same-endpoint navigation request replaces the desired target but never joins the
    /// in-flight focus RPC. Once that request resolves, `send_latest_focus` sends only the most
    /// recent desired target.
    pub(crate) fn retarget(
        &mut self,
        focus: Option<crate::client::shell::ClientEndpointFocusTarget>,
        endpoints: &mut EndpointRegistry,
    ) -> Result<(), String> {
        self.focus = focus;
        if let ActivationPhase::ActivatingTarget {
            focus_request_id,
            focus_acknowledged,
            ..
        } = &mut self.phase
        {
            *focus_acknowledged = self.focus.is_none() && focus_request_id.is_none();
        } else {
            // The latest target is retained and will be sent immediately after source release.
            return Ok(());
        }
        self.send_latest_focus(endpoints)
    }

    pub(crate) fn update_resize(
        &mut self,
        resize: crate::protocol::ClientMessage,
        endpoints: &mut EndpointRegistry,
    ) -> Result<(), String> {
        resize_geometry(&resize)
            .ok_or_else(|| "endpoint activation did not include a surface resize".to_owned())?;
        self.resize = resize.clone();
        let restart_effects_fence = match &self.phase {
            ActivationPhase::AwaitingPresentationEffects {
                lease, completion, ..
            } => Some((lease.clone(), (**completion).clone())),
            _ => None,
        };
        if let Some((lease, completion)) = restart_effects_fence {
            if endpoints.send_to(&lease.endpoint_id, &resize) != EndpointSendOutcome::Sent {
                return Err("pending endpoint resize could not be sent".into());
            }
            return self.start_presentation_sync(endpoints, lease, completion);
        }
        match &mut self.phase {
            ActivationPhase::ActivatingTarget { evidence, .. }
            | ActivationPhase::RestoringSource { evidence, .. }
            | ActivationPhase::SynchronizingPresentation { evidence, .. } => {
                evidence.invalidate_surface()
            }
            _ => {}
        }
        let destination = match &self.phase {
            ActivationPhase::ActivatingTarget { .. } => Some(&self.target.endpoint_id),
            ActivationPhase::RestoringSource { .. } => Some(&self.source.endpoint_id),
            ActivationPhase::SynchronizingPresentation { lease, .. } => Some(&lease.endpoint_id),
            _ => None,
        };
        if let Some(destination) = destination {
            if endpoints.send_to(destination, &resize) != EndpointSendOutcome::Sent {
                return Err("pending endpoint resize could not be sent".into());
            }
        }
        Ok(())
    }

    pub(crate) fn update_host_focus(
        &mut self,
        focused: bool,
        endpoints: &mut EndpointRegistry,
    ) -> Result<(), String> {
        self.host_focused = focused;
        let restart = self.presentation_restart();
        let destination = match &self.phase {
            ActivationPhase::ActivatingTarget { .. } => Some(&self.target.endpoint_id),
            ActivationPhase::RestoringSource { .. } => Some(&self.source.endpoint_id),
            ActivationPhase::SynchronizingPresentation { lease, .. }
            | ActivationPhase::AwaitingPresentationEffects { lease, .. } => {
                Some(&lease.endpoint_id)
            }
            _ => None,
        };
        if let Some(destination) = destination {
            if endpoints.send_to(
                destination,
                &crate::protocol::ClientMessage::ClientShellFocus { focused },
            ) != EndpointSendOutcome::Sent
            {
                return Err("pending endpoint focus baseline could not be sent".into());
            }
        }
        if let Some((lease, completion)) = restart {
            self.start_presentation_sync(endpoints, lease, completion)?;
        } else {
            self.invalidate_current_evidence();
        }
        Ok(())
    }

    pub(crate) fn update_host_theme(
        &mut self,
        update: crate::protocol::ClientHostThemeUpdate,
        endpoints: &mut EndpointRegistry,
    ) -> Result<(), String> {
        let restart = self.presentation_restart();
        let destination = match &self.phase {
            ActivationPhase::ActivatingTarget { .. } => Some(&self.target.endpoint_id),
            ActivationPhase::RestoringSource { .. } => Some(&self.source.endpoint_id),
            ActivationPhase::SynchronizingPresentation { lease, .. }
            | ActivationPhase::AwaitingPresentationEffects { lease, .. } => {
                Some(&lease.endpoint_id)
            }
            _ => None,
        };
        if let Some(destination) = destination {
            let message = crate::protocol::ClientMessage::ClientShellHostTheme { update };
            if endpoints.send_to(destination, &message) != EndpointSendOutcome::Sent {
                return Err("pending endpoint host theme could not be sent".into());
            }
        }
        if let Some((lease, completion)) = restart {
            self.start_presentation_sync(endpoints, lease, completion)?;
        } else {
            self.invalidate_current_evidence();
        }
        Ok(())
    }

    fn presentation_restart(&self) -> Option<(EndpointLease, ActivationCompletion)> {
        match &self.phase {
            ActivationPhase::SynchronizingPresentation {
                lease, completion, ..
            }
            | ActivationPhase::AwaitingPresentationEffects {
                lease, completion, ..
            } => Some((lease.clone(), (**completion).clone())),
            _ => None,
        }
    }

    fn invalidate_current_evidence(&mut self) {
        match &mut self.phase {
            ActivationPhase::ActivatingTarget { evidence, .. }
            | ActivationPhase::RestoringSource { evidence, .. } => evidence.invalidate_surface(),
            _ => {}
        }
    }

    /// Losing the source revokes its surface and removes the rollback destination; it must not
    /// cancel a healthy target. Losing the target restores the source when it is still available.
    pub(crate) fn endpoint_disconnected(
        &mut self,
        endpoints: &mut EndpointRegistry,
        endpoint_id: &ClientEndpointId,
        error: String,
    ) -> ActivationRollback {
        self.rollback_error = Some(error.clone());
        if self.target.endpoint_id == *endpoint_id && self.source.endpoint_id != *endpoint_id {
            return match self.phase {
                ActivationPhase::RestoringSource { .. } => ActivationRollback::Pending,
                _ => match self.start_source_restore(endpoints, self.resize.clone()) {
                    Ok(()) => ActivationRollback::Pending,
                    Err(restore_error) => ActivationRollback::Unavailable(format!(
                        "{error}; source endpoint could not be restored safely: {restore_error}"
                    )),
                },
            };
        }
        if self.source.endpoint_id != *endpoint_id {
            return ActivationRollback::Unavailable(error);
        }
        self.source_available = false;
        if self.source.endpoint_id != self.target.endpoint_id {
            match &self.phase {
                ActivationPhase::ReleasingSource { .. } => {
                    return match self.start_target(endpoints, self.resize.clone()) {
                        Ok(()) => ActivationRollback::Pending,
                        Err(message) => ActivationRollback::Unavailable(message),
                    };
                }
                ActivationPhase::ActivatingTarget { .. } => return ActivationRollback::Pending,
                ActivationPhase::SynchronizingPresentation { lease, .. }
                | ActivationPhase::AwaitingPresentationEffects { lease, .. }
                    if lease.endpoint_id == self.target.endpoint_id =>
                {
                    return ActivationRollback::Pending
                }
                _ => {}
            }
        }
        match self.phase {
            ActivationPhase::ReleasingSource { .. } => ActivationRollback::Unavailable(error),
            ActivationPhase::ActivatingTarget { .. } => {
                match self.start_target_release(endpoints) {
                    Ok(()) => ActivationRollback::Pending,
                    Err(release_error) => ActivationRollback::Unavailable(format!(
                        "{error}; target endpoint could not be released safely: {release_error}"
                    )),
                }
            }
            ActivationPhase::ReleasingTargetForRollback { .. } => ActivationRollback::Pending,
            ActivationPhase::RestoringSource { .. } => ActivationRollback::Unavailable(error),
            ActivationPhase::SynchronizingPresentation { ref lease, .. }
            | ActivationPhase::AwaitingPresentationEffects { ref lease, .. } => {
                if lease.endpoint_id == self.target.endpoint_id {
                    match self.start_target_release(endpoints) {
                        Ok(()) => ActivationRollback::Pending,
                        Err(release_error) => ActivationRollback::Unavailable(format!(
                            "{error}; target endpoint could not be released safely: {release_error}"
                        )),
                    }
                } else {
                    ActivationRollback::Unavailable(error)
                }
            }
        }
    }

    pub(crate) fn rollback(
        &mut self,
        endpoints: &mut EndpointRegistry,
        error: String,
        source_release_rejected: bool,
    ) -> ActivationRollback {
        self.rollback_error = Some(error.clone());
        if matches!(self.phase, ActivationPhase::ReleasingSource { .. }) && source_release_rejected
        {
            // Rejection proves source-off did not commit, but cached source metadata may have
            // advanced while the frame was frozen. Restore through the same coherent on/sync
            // path rather than immediately exposing a stale source projection.
            return match self.start_source_restore(endpoints, self.resize.clone()) {
                Ok(()) => ActivationRollback::Pending,
                Err(restore_error) => ActivationRollback::Unavailable(format!(
                    "{error}; source endpoint could not resume: {restore_error}"
                )),
            };
        }
        let result = match self.phase {
            ActivationPhase::ReleasingSource { .. } => {
                if self.source_available {
                    self.start_source_restore(endpoints, self.resize.clone())
                } else {
                    Err("the previous endpoint is no longer connected".into())
                }
            }
            ActivationPhase::ActivatingTarget { .. } => self.start_target_release(endpoints),
            ActivationPhase::ReleasingTargetForRollback { .. } => {
                // The target may have observed target-on or target-off. Closing this transport
                // is the only safe local revocation when target-off is not acknowledged.
                endpoints.fail(
                    &self.target.endpoint_id,
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "endpoint did not acknowledge surface revocation",
                    ),
                );
                if !self.source_available {
                    return ActivationRollback::Unavailable(format!(
                        "{error}; the target connection was closed because no presentation owner could be proven"
                    ));
                }
                self.start_source_restore(endpoints, self.resize.clone())
            }
            ActivationPhase::RestoringSource { .. } => {
                return ActivationRollback::Unavailable(format!(
                    "{error}; source endpoint could not be restored"
                ));
            }
            ActivationPhase::SynchronizingPresentation { ref lease, .. }
            | ActivationPhase::AwaitingPresentationEffects { ref lease, .. } => {
                if lease.endpoint_id == self.target.endpoint_id {
                    self.start_target_release(endpoints)
                } else {
                    return ActivationRollback::Unavailable(format!(
                        "{error}; source endpoint presentation could not be synchronized"
                    ));
                }
            }
        };
        match result {
            Ok(()) => ActivationRollback::Pending,
            Err(rollback_error) => ActivationRollback::Unavailable(format!(
                "{error}; source endpoint could not be restored safely: {rollback_error}"
            )),
        }
    }

    pub(crate) fn complete(
        &mut self,
        shell: &mut crate::client::shell::ClientShellState,
        endpoints: &mut EndpointRegistry,
    ) -> Result<ActivationCompletion, String> {
        if let ActivationPhase::SynchronizingPresentation {
            lease,
            acknowledged_revision,
            evidence,
            completion,
            ..
        } = &self.phase
        {
            let lease = lease.clone();
            let completion = (**completion).clone();
            let surface = coherent_completion_surface(
                shell,
                &lease,
                evidence,
                *acknowledged_revision,
                self.geometry(),
            )?;
            if endpoints.active_id() != &lease.endpoint_id
                || !shell.endpoint_projection_available(&lease.endpoint_id)
                || !shell.activate_endpoint_projection(&lease.endpoint_id)
            {
                return Err(
                    "endpoint became unavailable during presentation synchronization".into(),
                );
            }
            shell.set_pane_surface(surface);
            self.start_presentation_effects_fence(endpoints, lease, completion)?;
            return Ok(ActivationCompletion::AwaitingPresentationEffects);
        }
        if let ActivationPhase::AwaitingPresentationEffects {
            ready: true,
            completion,
            ..
        } = &self.phase
        {
            return Ok((**completion).clone());
        }

        let (lease, evidence, acknowledgement_revision, completion) = match &self.phase {
            ActivationPhase::ActivatingTarget {
                evidence,
                acknowledged_revision,
                ..
            } => {
                let completion = ActivationCompletion::Activated;
                (
                    self.target.clone(),
                    evidence,
                    *acknowledged_revision,
                    completion,
                )
            }
            ActivationPhase::RestoringSource {
                evidence,
                acknowledged_revision,
                ..
            } => {
                let completion = ActivationCompletion::RestoredSource {
                    error: self
                        .rollback_error
                        .clone()
                        .unwrap_or_else(|| "endpoint handoff was rolled back".into()),
                    successor: self.successor.clone(),
                };
                (
                    self.source.clone(),
                    evidence,
                    *acknowledged_revision,
                    completion,
                )
            }
            _ => return Err("endpoint activation completed in an invalid phase".into()),
        };
        let surface = coherent_completion_surface(
            shell,
            &lease,
            evidence,
            acknowledgement_revision,
            self.geometry(),
        )?;
        endpoints.set_surface_active(&lease.endpoint_id, true);
        shell.set_endpoint_status(&lease.endpoint_id, ClientEndpointStatus::Online);
        if !shell.endpoint_projection_available(&lease.endpoint_id)
            || !endpoints.set_active(&lease.endpoint_id)
        {
            return Err("endpoint became unavailable during activation".into());
        }
        let activated = shell.activate_endpoint_projection(&lease.endpoint_id);
        debug_assert!(activated, "preflighted endpoint projection must activate");
        debug_assert!(shell.endpoint_is_active(endpoints.active_id()));
        shell.set_pane_surface(surface);
        self.start_presentation_sync(endpoints, lease.clone(), completion)?;
        Ok(ActivationCompletion::AwaitingPresentationSync {
            previous: self.source.endpoint_id.clone(),
            endpoint: lease.endpoint_id,
        })
    }

    fn start_target(
        &mut self,
        endpoints: &mut EndpointRegistry,
        resize: crate::protocol::ClientMessage,
    ) -> Result<(), String> {
        let request_id = format!("client-shell-surface:{}:on", self.epoch);
        self.deadline = Instant::now() + ACTIVATION_TIMEOUT;
        // A transport may fail after writing any baseline or surface message. Enter the target
        // phase first so every uncertain target write is reversed through target-off before
        // source restoration is considered.
        self.phase = ActivationPhase::ActivatingTarget {
            request_id: request_id.clone(),
            acknowledged_revision: None,
            focus_request_id: None,
            focus_request_target: None,
            focus_acknowledged: self.focus.is_none(),
            evidence: ActivationEvidence::default(),
        };
        send_surface_activation(
            endpoints,
            &self.target,
            request_id,
            &resize,
            self.host_focused,
        )?;

        // From this point the target may have processed surface.set(true). Optional navigation
        // is serialized through one coalescing focus lane.
        self.send_latest_focus(endpoints)
    }

    fn start_presentation_sync(
        &mut self,
        endpoints: &mut EndpointRegistry,
        lease: EndpointLease,
        completion: ActivationCompletion,
    ) -> Result<(), String> {
        let request_id = format!("client-shell-surface:{}:presentation-sync", self.epoch);
        let request = surface_interest_request(&lease.boot_id, request_id.clone(), true)
            .map_err(|error| error.to_string())?;
        self.phase = ActivationPhase::SynchronizingPresentation {
            lease: lease.clone(),
            request_id,
            acknowledged_revision: None,
            evidence: ActivationEvidence::default(),
            completion: Box::new(completion),
        };
        self.deadline = Instant::now() + ACTIVATION_TIMEOUT;
        if endpoints.send_to(&lease.endpoint_id, &request) != EndpointSendOutcome::Sent {
            return Err("endpoint presentation synchronization could not be sent".into());
        }
        Ok(())
    }

    fn start_presentation_effects_fence(
        &mut self,
        endpoints: &mut EndpointRegistry,
        lease: EndpointLease,
        completion: ActivationCompletion,
    ) -> Result<(), String> {
        let token = format!("{}:{}:{}", self.epoch, lease.generation, lease.boot_id);
        self.phase = ActivationPhase::AwaitingPresentationEffects {
            lease: lease.clone(),
            token: token.clone(),
            ready: false,
            completion: Box::new(completion),
        };
        self.deadline = Instant::now() + ACTIVATION_TIMEOUT;
        let message = crate::protocol::ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND.into(),
            data: token,
        };
        if endpoints.send_to(&lease.endpoint_id, &message) != EndpointSendOutcome::Sent {
            return Err("endpoint presentation effects fence could not be sent".into());
        }
        Ok(())
    }

    fn start_target_release(&mut self, endpoints: &mut EndpointRegistry) -> Result<(), String> {
        let request_id = format!("client-shell-surface:{}:rollback-target-off", self.epoch);
        let request = surface_interest_request(&self.target.boot_id, request_id.clone(), false)
            .map_err(|error| error.to_string())?;
        // Set the rollback phase before the potentially observed target-off write.
        self.phase = ActivationPhase::ReleasingTargetForRollback { request_id };
        self.deadline = Instant::now() + ACTIVATION_TIMEOUT;
        if endpoints.send_to(&self.target.endpoint_id, &request) != EndpointSendOutcome::Sent {
            return Err("target endpoint release could not be sent".into());
        }
        Ok(())
    }

    fn start_source_restore(
        &mut self,
        endpoints: &mut EndpointRegistry,
        resize: crate::protocol::ClientMessage,
    ) -> Result<(), String> {
        let request_id = format!("client-shell-surface:{}:rollback-source-on", self.epoch);
        // Source baseline writes can also be observed before their send reports an error.
        self.phase = ActivationPhase::RestoringSource {
            request_id: request_id.clone(),
            acknowledged_revision: None,
            evidence: ActivationEvidence::default(),
        };
        self.deadline = Instant::now() + ACTIVATION_TIMEOUT;
        send_surface_activation(
            endpoints,
            &self.source,
            request_id,
            &resize,
            self.host_focused,
        )
    }

    fn send_latest_focus(&mut self, endpoints: &mut EndpointRegistry) -> Result<(), String> {
        let desired = self.focus.clone();
        let Some(desired) = desired else {
            if let ActivationPhase::ActivatingTarget {
                focus_request_id,
                focus_acknowledged,
                ..
            } = &mut self.phase
            {
                if focus_request_id.is_none() {
                    *focus_acknowledged = true;
                }
            }
            return Ok(());
        };
        if !matches!(
            &self.phase,
            ActivationPhase::ActivatingTarget {
                focus_request_id: None,
                ..
            }
        ) {
            return Ok(());
        }
        let request_id = self
            .next_focus_request_id()
            .expect("desired focus creates a request id");
        if let ActivationPhase::ActivatingTarget {
            focus_request_id,
            focus_request_target,
            focus_acknowledged,
            ..
        } = &mut self.phase
        {
            *focus_acknowledged = false;
            *focus_request_id = Some(request_id.clone());
            *focus_request_target = Some(desired.clone());
        }
        let request = focus_request(&self.target.boot_id, request_id, &desired)
            .map_err(|error| error.to_string())?;
        if endpoints.send_to(&self.target.endpoint_id, &request) != EndpointSendOutcome::Sent {
            return Err("endpoint focus could not be sent".into());
        }
        Ok(())
    }

    fn next_focus_request_id(&mut self) -> Option<String> {
        self.focus.as_ref()?;
        self.next_focus_serial = self.next_focus_serial.saturating_add(1);
        Some(format!(
            "client-shell-focus:{}:{}",
            self.epoch, self.next_focus_serial
        ))
    }

    fn progress(&self) -> SurfaceActivationProgress {
        match &self.phase {
            ActivationPhase::ActivatingTarget {
                acknowledged_revision,
                focus_acknowledged,
                evidence,
                ..
            } if acknowledged_revision.is_some_and(|revision| {
                *focus_acknowledged
                    && evidence
                        .coherent_surface(revision, self.geometry())
                        .is_some_and(|surface| self.target_matches(surface))
            }) =>
            {
                SurfaceActivationProgress::Ready
            }
            ActivationPhase::RestoringSource {
                acknowledged_revision,
                evidence,
                ..
            }
            | ActivationPhase::SynchronizingPresentation {
                acknowledged_revision,
                evidence,
                ..
            } if acknowledged_revision.is_some_and(|revision| {
                evidence
                    .coherent_surface(revision, self.geometry())
                    .is_some()
            }) =>
            {
                SurfaceActivationProgress::Ready
            }
            _ => SurfaceActivationProgress::Pending,
        }
    }

    fn target_matches(&self, surface: &crate::protocol::PaneSurfaceFrame) -> bool {
        let evidence = match &self.phase {
            ActivationPhase::ActivatingTarget { evidence, .. } => evidence,
            _ => return false,
        };
        match &self.focus {
            Some(crate::client::shell::ClientEndpointFocusTarget::Pane(pane_id)) => {
                evidence.focused_pane_id.as_deref() == Some(pane_id)
                    && surface
                        .panes
                        .iter()
                        .any(|pane| pane.focused && &pane.pane_id == pane_id)
            }
            Some(crate::client::shell::ClientEndpointFocusTarget::Tab(tab_id)) => {
                evidence.focused_tab_id.as_deref() == Some(tab_id)
            }
            Some(crate::client::shell::ClientEndpointFocusTarget::Workspace(workspace_id)) => {
                evidence.focused_workspace_id.as_deref() == Some(workspace_id)
            }
            None => true,
        }
    }
}

#[cfg(test)]
#[path = "activation_tests.rs"]
mod tests;
