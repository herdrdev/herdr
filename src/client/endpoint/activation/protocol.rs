use super::super::{ClientEndpointId, EndpointRegistry, EndpointSendOutcome};
use super::model::{ActivationEvidence, EndpointLease};

pub(super) fn focus_result_matches(
    focus: Option<&crate::client::shell::ClientEndpointFocusTarget>,
    result: &crate::api::schema::ResponseResult,
) -> bool {
    match (focus, result) {
        (
            Some(crate::client::shell::ClientEndpointFocusTarget::Pane(expected)),
            crate::api::schema::ResponseResult::PaneInfo { pane },
        ) => pane.focused && &pane.pane_id == expected,
        (
            Some(crate::client::shell::ClientEndpointFocusTarget::Workspace(expected)),
            crate::api::schema::ResponseResult::WorkspaceInfo { workspace },
        ) => workspace.focused && &workspace.workspace_id == expected,
        (
            Some(crate::client::shell::ClientEndpointFocusTarget::Tab(expected)),
            crate::api::schema::ResponseResult::TabInfo { tab },
        ) => tab.focused && &tab.tab_id == expected,
        _ => false,
    }
}

pub(super) fn endpoint_lease(
    shell: &crate::client::shell::ClientShellState,
    endpoints: &EndpointRegistry,
    endpoint_id: &ClientEndpointId,
) -> Result<EndpointLease, String> {
    let connection = endpoints
        .connection(endpoint_id)
        .ok_or_else(|| "endpoint connection is unavailable".to_owned())?;
    let (boot_id, minimum_revision) = shell
        .endpoint_snapshot_identity(endpoint_id, connection.generation)
        .ok_or_else(|| "endpoint metadata is not ready for this connection".to_owned())?;
    Ok(EndpointLease {
        endpoint_id: endpoint_id.clone(),
        generation: connection.generation,
        boot_id: boot_id.to_owned(),
        minimum_revision,
    })
}

pub(super) fn disconnected_endpoint_lease(
    shell: &crate::client::shell::ClientShellState,
    endpoint_id: &ClientEndpointId,
) -> EndpointLease {
    EndpointLease {
        endpoint_id: endpoint_id.clone(),
        generation: 0,
        boot_id: shell
            .endpoint_boot_id(endpoint_id)
            .unwrap_or_default()
            .to_owned(),
        minimum_revision: 0,
    }
}

pub(super) fn endpoint_matches(
    lease: &EndpointLease,
    endpoint_id: &ClientEndpointId,
    generation: u64,
    boot_id: &str,
) -> bool {
    lease.endpoint_id == *endpoint_id && lease.generation == generation && lease.boot_id == boot_id
}

pub(super) fn coherent_completion_surface(
    shell: &crate::client::shell::ClientShellState,
    lease: &EndpointLease,
    evidence: &mut ActivationEvidence,
    acknowledgement_revision: Option<u64>,
    geometry: crate::protocol::ClientSurfaceSize,
) -> Result<crate::protocol::PaneSurfaceFrame, String> {
    let acknowledgement_revision = acknowledgement_revision.ok_or_else(|| {
        "endpoint activation completed without a surface acknowledgement".to_owned()
    })?;
    let surface = evidence
        .surface
        .as_ref()
        .ok_or_else(|| "endpoint activation completed without a surface".to_owned())?;
    if surface.projection_revision < acknowledgement_revision
        || !surface_matches_geometry(surface, geometry)
    {
        return Err("endpoint activation lost its acknowledged surface evidence".into());
    }
    if !shell.endpoint_snapshot_matches(
        &lease.endpoint_id,
        lease.generation,
        &lease.boot_id,
        surface.projection_revision,
    ) {
        return Err("endpoint activation lost its coherent snapshot/surface pair".into());
    }
    let surface = evidence.surface.take().expect("validated surface evidence");
    Ok(surface)
}

pub(super) fn resize_geometry(
    message: &crate::protocol::ClientMessage,
) -> Option<crate::protocol::ClientSurfaceSize> {
    match message {
        crate::protocol::ClientMessage::ClientShellResize { surface_size, .. } => {
            Some(*surface_size)
        }
        _ => None,
    }
}

pub(super) fn surface_matches_geometry(
    surface: &crate::protocol::PaneSurfaceFrame,
    geometry: crate::protocol::ClientSurfaceSize,
) -> bool {
    surface.frame.width == geometry.cols && surface.frame.height == geometry.rows
}

pub(super) fn send_surface_activation(
    endpoints: &mut EndpointRegistry,
    target: &EndpointLease,
    request_id: String,
    resize: &crate::protocol::ClientMessage,
    focused: bool,
) -> Result<(), String> {
    if endpoints.send_to(&target.endpoint_id, resize) != EndpointSendOutcome::Sent {
        return Err("endpoint resize could not be sent".into());
    }
    let request = surface_interest_request(&target.boot_id, request_id, true)
        .map_err(|error| error.to_string())?;
    if endpoints.send_to(&target.endpoint_id, &request) != EndpointSendOutcome::Sent {
        return Err("endpoint activation could not be sent".into());
    }
    // Inactive endpoints reject focus events. Activate first, then establish the host baseline
    // on the same ordered transport before navigation or presentation can commit.
    if endpoints.send_to(
        &target.endpoint_id,
        &crate::protocol::ClientMessage::ClientShellFocus { focused },
    ) != EndpointSendOutcome::Sent
    {
        return Err("endpoint focus baseline could not be sent".into());
    }
    Ok(())
}

pub(super) fn decode_endpoint_response(
    request_id: &str,
    data: &[u8],
) -> Result<crate::api::schema::ResponseResult, crate::client::shell::ClientShellEndpointError> {
    crate::client::endpoint_commands::parse_response(request_id, data)
}

pub(super) fn surface_set_revision(
    result: &crate::api::schema::ResponseResult,
    expected_active: bool,
) -> Result<u64, String> {
    match result {
        crate::api::schema::ResponseResult::ClientShellSurfaceSet {
            active,
            projection_revision,
        } if *active == expected_active => Ok(*projection_revision),
        _ => Err("surface activation returned an invalid acknowledgement".into()),
    }
}

pub(super) fn focus_request(
    boot_id: &str,
    request_id: String,
    focus: &crate::client::shell::ClientEndpointFocusTarget,
) -> std::io::Result<crate::protocol::ClientMessage> {
    let method = match focus {
        crate::client::shell::ClientEndpointFocusTarget::Workspace(workspace_id) => {
            crate::api::schema::Method::WorkspaceFocus(crate::api::schema::WorkspaceTarget {
                workspace_id: workspace_id.clone(),
            })
        }
        crate::client::shell::ClientEndpointFocusTarget::Pane(pane_id) => {
            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            })
        }
        crate::client::shell::ClientEndpointFocusTarget::Tab(tab_id) => {
            crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                tab_id: tab_id.clone(),
            })
        }
    };
    endpoint_request(
        boot_id,
        crate::api::schema::Request {
            id: request_id,
            method,
        },
    )
}

pub(super) fn surface_interest_request(
    boot_id: &str,
    request_id: String,
    active: bool,
) -> std::io::Result<crate::protocol::ClientMessage> {
    endpoint_request(
        boot_id,
        crate::api::schema::Request {
            id: request_id,
            method: crate::api::schema::Method::ClientShellSurfaceSet(
                crate::api::schema::ClientShellSurfaceSetParams { active },
            ),
        },
    )
}

fn endpoint_request(
    boot_id: &str,
    request: crate::api::schema::Request,
) -> std::io::Result<crate::protocol::ClientMessage> {
    Ok(crate::protocol::ClientMessage::ClientShellEndpointRequest {
        boot_id: boot_id.to_owned(),
        request: serde_json::to_string(&request)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
    })
}
