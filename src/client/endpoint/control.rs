use super::ClientEndpointId;

pub(crate) struct DecodedAgentViewProjection {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    pub(crate) view: Result<Option<crate::api::schema::AgentViewSetParams>, ()>,
}

pub(crate) enum EndpointControlMessage {
    HealthPong,
    AgentViewProjection(DecodedAgentViewProjection),
    Snapshot(Box<crate::protocol::ClientShellSnapshot>),
    Ignored,
}

pub(crate) fn decode_endpoint_control(
    kind: &str,
    data: &str,
) -> Result<EndpointControlMessage, String> {
    if kind == crate::protocol::endpoint::HEALTH_PONG_KIND {
        return Ok(EndpointControlMessage::HealthPong);
    }
    if kind == crate::protocol::endpoint::AGENT_VIEW_PROJECTION_KIND {
        let Ok(projection): Result<crate::protocol::endpoint::EndpointAgentViewProjection, _> =
            serde_json::from_str(data)
        else {
            return Ok(EndpointControlMessage::Ignored);
        };
        let view = match projection.view {
            Some(value) => serde_json::from_value(value)
                .map(Some)
                .map_err(|_| ())
                .and_then(|mut view| {
                    view.as_mut()
                        .map(crate::app::agent_view::validate_agent_view)
                        .transpose()
                        .map(|_| view)
                        .map_err(|_| ())
                }),
            None => Ok(None),
        };
        return Ok(EndpointControlMessage::AgentViewProjection(
            DecodedAgentViewProjection {
                boot_id: projection.boot_id,
                revision: projection.revision,
                view,
            },
        ));
    }
    if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND {
        let snapshot = serde_json::from_str(data)
            .map_err(|error| format!("invalid endpoint snapshot: {error}"))?;
        return Ok(EndpointControlMessage::Snapshot(Box::new(snapshot)));
    }
    if kind.starts_with("shell.snapshot.") {
        return Err(format!(
            "unsupported mandatory endpoint snapshot codec {kind:?}"
        ));
    }
    Ok(EndpointControlMessage::Ignored)
}

pub(crate) fn protocol_failure_is_fatal(endpoint_id: &ClientEndpointId) -> bool {
    endpoint_id.is_local()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::endpoint::ProfileId;

    #[test]
    fn unknown_optional_controls_are_ignored() {
        assert!(matches!(
            decode_endpoint_control("future.optional", "not json").unwrap(),
            EndpointControlMessage::Ignored
        ));
    }

    #[test]
    fn agent_view_projection_decodes_and_validates_the_view() {
        let view = crate::api::schema::AgentViewSetParams {
            source: "example.views".into(),
            label: Some("focus".into()),
            filter: None,
            sort: Vec::new(),
        };
        let crate::protocol::ServerMessage::EndpointControl { kind, data } =
            crate::protocol::endpoint::agent_view_projection_message("boot", 4, Some(&view))
                .unwrap()
        else {
            panic!("projection control");
        };
        let EndpointControlMessage::AgentViewProjection(decoded) =
            decode_endpoint_control(&kind, &data).unwrap()
        else {
            panic!("decoded projection");
        };
        assert_eq!(decoded.boot_id, "boot");
        assert_eq!(decoded.revision, 4);
        assert_eq!(decoded.view, Ok(Some(view)));
    }

    #[test]
    fn unsupported_agent_view_payload_falls_back_without_rejecting_endpoint() {
        let projection = crate::protocol::endpoint::EndpointAgentViewProjection {
            boot_id: "boot".into(),
            revision: 5,
            view: Some(serde_json::json!({
                "source": "example.views",
                "filter": {"op": "future_filter"}
            })),
        };
        let decoded = decode_endpoint_control(
            crate::protocol::endpoint::AGENT_VIEW_PROJECTION_KIND,
            &serde_json::to_string(&projection).unwrap(),
        )
        .unwrap();
        let EndpointControlMessage::AgentViewProjection(decoded) = decoded else {
            panic!("decoded projection");
        };
        assert_eq!(decoded.view, Err(()));
    }

    #[test]
    fn malformed_agent_view_envelope_is_ignored() {
        assert!(matches!(
            decode_endpoint_control(
                crate::protocol::endpoint::AGENT_VIEW_PROJECTION_KIND,
                "not json"
            )
            .unwrap(),
            EndpointControlMessage::Ignored
        ));
    }

    #[test]
    fn unknown_snapshot_codecs_are_rejected() {
        assert_eq!(
            decode_endpoint_control("shell.snapshot.v2", "{}")
                .err()
                .as_deref(),
            Some("unsupported mandatory endpoint snapshot codec \"shell.snapshot.v2\"")
        );
    }

    #[test]
    fn only_local_protocol_failures_end_the_client() {
        let remote =
            ClientEndpointId::Ssh(ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap());
        assert!(protocol_failure_is_fatal(&ClientEndpointId::Local));
        assert!(!protocol_failure_is_fatal(&remote));
    }
}
