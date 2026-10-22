use super::*;
use crate::raw_input::RawInputEvent;
use crossterm::event::{MouseButton, MouseEventKind};

#[derive(Default)]
pub(super) struct MachineDiagnostics {
    errors: HashMap<ClientEndpointId, String>,
    hover: Option<ClientEndpointId>,
    /// Retries the client loop has not yet handed to the endpoint supervisors.
    retry_requests: Vec<ClientEndpointId>,
    /// Requested retries whose failure should reopen the fresh diagnostic.
    retry_reports: HashSet<ClientEndpointId>,
}

impl MachineDiagnostics {
    pub(super) fn required_for(&self, endpoint: &ClientShellEndpoint) -> bool {
        self.errors
            .get(&endpoint.endpoint_id)
            .is_some_and(|message| crate::remote::ssh_error_requires_authentication(message))
    }

    pub(super) fn badge_style(
        &self,
        endpoint: &ClientShellEndpoint,
        palette: &Palette,
        style: Style,
    ) -> Style {
        if self.hover.as_ref() == Some(&endpoint.endpoint_id) && machine_retryable(endpoint) {
            style
                .bg(palette.active_row_bg)
                .add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    }
}

/// Saved machines wait out a reconnect backoff whenever they are not online or disabled.
fn machine_retryable(endpoint: &ClientShellEndpoint) -> bool {
    !endpoint.endpoint_id.is_local()
        && matches!(
            endpoint.status,
            ClientEndpointStatus::Connecting
                | ClientEndpointStatus::Reconnecting
                | ClientEndpointStatus::Attention
        )
}

fn machine_diagnostic_code(profile_id: &impl std::fmt::Display) -> String {
    format!("machine-diagnostic:{profile_id}")
}

impl ClientShellState {
    pub(crate) fn receive_machine_unavailable(
        &mut self,
        id: &ClientEndpointId,
        message: String,
    ) -> bool {
        self.push_endpoint_notice(
            ClientEndpointNoticeKind::Unavailable,
            format!("machine-unavailable:{}:{message}", id.storage_key()),
            "Endpoint unavailable",
            message,
        )
    }

    pub(crate) fn set_machine_diagnostic(&mut self, id: &ClientEndpointId, message: String) {
        if !id.is_local() {
            self.machine_diagnostics.errors.insert(
                id.clone(),
                message
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n')
                    .take(4096)
                    .collect(),
            );
        }
    }

    pub(super) fn clear_machine_diagnostic(&mut self, id: &ClientEndpointId) {
        self.machine_diagnostics.errors.remove(id);
        self.machine_diagnostics.retry_reports.remove(id);
    }

    /// Asks for an immediate connection attempt instead of waiting out the backoff.
    /// Returns false when the machine has no pending reconnect to bring forward.
    pub(super) fn retry_machine(&mut self, id: &ClientEndpointId) -> bool {
        let ClientEndpointId::Ssh(profile_id) = id else {
            return false;
        };
        if !self
            .endpoints
            .iter()
            .any(|endpoint| &endpoint.endpoint_id == id && machine_retryable(endpoint))
        {
            return false;
        }
        // Clear the selected machine's diagnostic or active disconnect notice.
        let code = machine_diagnostic_code(profile_id);
        let unavailable = format!("machine-unavailable:{}:", id.storage_key());
        if self.visible_endpoint_notice.as_ref().is_some_and(|notice| {
            notice.key.code == code || notice.key.code.starts_with(&unavailable)
        }) {
            self.visible_endpoint_notice = None;
        }
        if !self.machine_diagnostics.retry_requests.contains(id) {
            self.machine_diagnostics.retry_requests.push(id.clone());
        }
        self.machine_diagnostics.retry_reports.insert(id.clone());
        self.set_endpoint_status(id, ClientEndpointStatus::Connecting);
        true
    }

    pub(crate) fn take_machine_retries(&mut self) -> Vec<ClientEndpointId> {
        std::mem::take(&mut self.machine_diagnostics.retry_requests)
    }

    /// Reports a failed connection attempt when the user asked for it.
    pub(crate) fn report_machine_retry(&mut self, id: &ClientEndpointId) -> bool {
        let requested = self.machine_diagnostics.retry_reports.remove(id);
        if requested {
            self.show_machine_diagnostic(id);
        }
        requested
    }

    pub(super) fn handle_machine_badge_event(
        &mut self,
        event: &RawInputEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let RawInputEvent::Mouse(mouse) = event else {
            return false;
        };
        if self.overlay.is_some()
            || self.popup_pending
            || self.hits.popup.is_some()
            || contains(self.hits.notification_toast, (mouse.column, mouse.row))
        {
            return false;
        }
        let hit = self
            .hits
            .machines
            .iter()
            .find(|hit| contains(hit.status_badge, (mouse.column, mouse.row)))
            .filter(|hit| {
                self.endpoints.iter().any(|endpoint| {
                    endpoint.endpoint_id == hit.endpoint_id && machine_retryable(endpoint)
                })
            })
            .map(|hit| hit.endpoint_id.clone());
        if mouse.kind == MouseEventKind::Moved {
            if self.machine_diagnostics.hover != hit {
                self.machine_diagnostics.hover = hit;
                outcome.repaint = true;
            }
            return false;
        }
        let Some(id) = hit else {
            return false;
        };
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            return true;
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }
        outcome.repaint |= self.retry_machine(&id);
        true
    }

    fn show_machine_diagnostic(&mut self, id: &ClientEndpointId) {
        let ClientEndpointId::Ssh(profile_id) = id else {
            return;
        };
        let Some(error) = self.machine_diagnostics.errors.get(id) else {
            return;
        };
        let code = machine_diagnostic_code(profile_id);
        let command = if crate::remote::ssh_error_requires_authentication(error) {
            format!("herdr machine reconnect {profile_id}")
        } else {
            format!("herdr machine status {profile_id}")
        };
        // A requested retry can report its diagnostic, but must not replace another notice.
        if self
            .visible_endpoint_notice
            .as_ref()
            .is_some_and(|notice| notice.key.code != code)
        {
            return;
        }
        self.visible_endpoint_notice = Some(ClientVisibleEndpointNotice {
            key: ClientEndpointNoticeKey {
                boot_id: "machine".into(),
                kind: ClientEndpointNoticeKind::Unavailable,
                code,
            },
            title: format!("{}: {command}", self.endpoint_label(id)),
            body: error.clone(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(15),
        });
    }
}
