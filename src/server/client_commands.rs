use std::io;
use std::sync::mpsc;

use tokio::sync::mpsc as tokio_mpsc;

use crate::api::schema::{ErrorBody, ErrorResponse, Method};

use super::client_transport::ServerEvent;

pub(crate) const MAX_ENDPOINT_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ENDPOINT_BOOT_ID_BYTES: usize = 128;
pub(crate) const MAX_ENDPOINT_REQUEST_ID_BYTES: usize = 128;
const ENDPOINT_RESPONSE_CHUNK_BYTES: usize = 512 * 1024;

const CLIENT_SHELL_METHODS: &[&str] = &[
    "command.invoke",
    "integration.install",
    "integration.list",
    "layout.set_split_ratio",
    "pane.close",
    "pane.copy_motion",
    "pane.copy_search",
    "pane.edit_scrollback",
    "pane.focus",
    "pane.focus_direction",
    "pane.input.set",
    "pane.link.activate",
    "pane.rename",
    "pane.resize",
    "pane.scroll",
    "pane.selection.read",
    "pane.split",
    "pane.swap",
    "pane.zoom",
    "product_announcement.dismiss",
    "release_notes.dismiss",
    "server.reload_config",
    "tab.close",
    "tab.create",
    "tab.focus",
    "tab.move",
    "tab.rename",
    "workspace.close",
    "workspace.create",
    "workspace.focus",
    "workspace.move",
    "workspace.move_block",
    "workspace.rename",
    "worktree.create",
    "worktree.list",
    "worktree.open",
    "worktree.remove",
];

pub(crate) fn supported_client_shell_method_names() -> &'static [&'static str] {
    CLIENT_SHELL_METHODS
}

pub(crate) fn supports_client_shell_method_name(method: &str) -> bool {
    CLIENT_SHELL_METHODS.contains(&method)
}

pub(crate) fn supports_client_shell_method(method: &Method) -> bool {
    supports_client_shell_method_name(crate::api::api_method_name(method))
}

pub(crate) fn error_response(id: String, code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"serialization_error","message":"failed to serialize endpoint response"}}"#.into()
    })
}

pub(crate) fn error_message(
    boot_id: String,
    request_id: String,
    code: &str,
    message: impl Into<String>,
) -> crate::protocol::ServerMessage {
    let response = error_response(request_id.clone(), code, message);
    crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
        boot_id,
        request_id,
        final_chunk: true,
        data: response.into_bytes(),
    }
}

fn correlate_response_id(response: String, request_id: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&response) else {
        return response;
    };
    let Some(id) = value.get_mut("id") else {
        return response;
    };
    if id.as_str() == Some(request_id) {
        return response;
    }
    *id = serde_json::Value::String(request_id.to_owned());
    serde_json::to_string(&value).unwrap_or(response)
}

pub(crate) fn spawn_response_waiter(
    client_id: u64,
    boot_id: String,
    request_id: String,
    response_rx: mpsc::Receiver<String>,
    server_event_tx: tokio_mpsc::Sender<ServerEvent>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("herdr-client-endpoint-response".into())
        .spawn(move || {
            let response = response_rx.recv().unwrap_or_else(|_| {
                error_response(
                    request_id.clone(),
                    "server_unavailable",
                    "endpoint command ended without a response",
                )
            });
            let response = correlate_response_id(response, &request_id).into_bytes();
            if response.is_empty() {
                let _ = server_event_tx.blocking_send(
                    ServerEvent::ClientShellEndpointResponseChunkReady {
                        client_id,
                        boot_id,
                        request_id,
                        final_chunk: true,
                        data: Vec::new(),
                    },
                );
                return;
            }
            let chunk_count = response.len().div_ceil(ENDPOINT_RESPONSE_CHUNK_BYTES);
            for (index, chunk) in response.chunks(ENDPOINT_RESPONSE_CHUNK_BYTES).enumerate() {
                if server_event_tx
                    .blocking_send(ServerEvent::ClientShellEndpointResponseChunkReady {
                        client_id,
                        boot_id: boot_id.clone(),
                        request_id: request_id.clone(),
                        final_chunk: index + 1 == chunk_count,
                        data: chunk.to_vec(),
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use sha2::{Digest, Sha256};

    use super::*;

    fn collect_schema_refs(value: &serde_json::Value, refs: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
                    if let Some(name) = reference.rsplit('/').next() {
                        refs.insert(name.to_owned());
                    }
                }
                for value in object.values() {
                    collect_schema_refs(value, refs);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    collect_schema_refs(value, refs);
                }
            }
            _ => {}
        }
    }

    fn normalized_wire_schema(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => serde_json::Value::Object(
                object
                    .iter()
                    .filter(|(key, _)| {
                        !matches!(
                            key.as_str(),
                            "description" | "examples" | "readOnly" | "title" | "writeOnly"
                        )
                    })
                    .map(|(key, value)| (key.clone(), normalized_wire_schema(value)))
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.iter().map(normalized_wire_schema).collect())
            }
            _ => value.clone(),
        }
    }

    fn endpoint_method_shape_digests() -> BTreeMap<String, String> {
        let schema = serde_json::to_value(schemars::schema_for!(crate::api::schema::Request))
            .expect("request schema");
        let definitions = schema
            .get("$defs")
            .and_then(serde_json::Value::as_object)
            .expect("request definitions");
        let branches = schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .expect("request method branches");
        let mut digests = BTreeMap::new();

        for method in CLIENT_SHELL_METHODS {
            let branch = branches
                .iter()
                .find(|branch| {
                    branch
                        .pointer("/properties/method/const")
                        .and_then(serde_json::Value::as_str)
                        == Some(method)
                })
                .unwrap_or_else(|| panic!("missing request schema branch for {method}"));
            let mut referenced_names = BTreeSet::new();
            collect_schema_refs(branch, &mut referenced_names);
            let mut visited_names = BTreeSet::new();
            let mut selected_definitions = serde_json::Map::new();
            while let Some(name) = referenced_names.pop_first() {
                if !visited_names.insert(name.clone()) {
                    continue;
                }
                let definition = definitions
                    .get(&name)
                    .unwrap_or_else(|| panic!("missing schema definition {name} for {method}"));
                collect_schema_refs(definition, &mut referenced_names);
                selected_definitions.insert(name, normalized_wire_schema(definition));
            }
            let shape = serde_json::json!({
                "request": normalized_wire_schema(branch),
                "definitions": selected_definitions,
            });
            let bytes = serde_json::to_vec(&shape).expect("method shape json");
            digests.insert(method.to_string(), format!("{:x}", Sha256::digest(bytes)));
        }

        digests
    }

    #[test]
    fn advertised_client_shell_method_shapes_stay_at_the_v1_contract() {
        let expected: BTreeMap<String, String> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-method-shapes-v1.json"
        )))
        .expect("endpoint method shape fixture");
        let actual = endpoint_method_shape_digests();

        assert_eq!(
            actual,
            expected,
            "an existing endpoint method changed shape; add load-bearing behavior as a new advertised method or explicitly gate new fields"
        );
    }

    #[test]
    fn advertised_client_shell_methods_are_sorted_unique_and_in_schema() {
        assert!(CLIENT_SHELL_METHODS
            .windows(2)
            .all(|pair| pair[0] < pair[1]));

        fn collect_method_constants(value: &serde_json::Value, methods: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(method) = object
                        .get("const")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| value.contains('.'))
                    {
                        methods.push(method.to_owned());
                    }
                    for value in object.values() {
                        collect_method_constants(value, methods);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        collect_method_constants(value, methods);
                    }
                }
                _ => {}
            }
        }

        let schema = serde_json::to_value(schemars::schema_for!(crate::api::schema::Request))
            .expect("request schema");
        let mut schema_methods = Vec::new();
        collect_method_constants(&schema, &mut schema_methods);
        for method in CLIENT_SHELL_METHODS {
            assert!(
                schema_methods.iter().any(|candidate| candidate == method),
                "advertised endpoint method {method:?} is absent from the request schema"
            );
        }
    }

    #[test]
    fn client_shell_lane_excludes_api_front_door_and_lifecycle_methods() {
        assert!(supports_client_shell_method(&Method::ServerReloadConfig(
            crate::api::schema::EmptyParams::default(),
        )));
        assert!(supports_client_shell_method(&Method::PaneLinkActivate(
            crate::api::schema::PaneLinkActivateParams {
                pane_id: "w1:p1".into(),
                viewport_row: 0,
                col: 0,
                content_revision: None,
                offset_from_bottom: None,
            },
        )));
        assert!(!supports_client_shell_method(&Method::Ping(
            crate::api::schema::PingParams::default(),
        )));
        assert!(!supports_client_shell_method(&Method::ServerStop(
            crate::api::schema::EmptyParams::default(),
        )));
    }

    #[test]
    fn endpoint_response_uses_the_client_request_id() {
        let response = serde_json::json!({
            "id": "endpoint:boot-a:7:client-shell:1",
            "result": { "type": "ok" }
        })
        .to_string();

        let correlated = correlate_response_id(response, "client-shell:1");
        let decoded: serde_json::Value = serde_json::from_str(&correlated).expect("response json");

        assert_eq!(decoded["id"], "client-shell:1");
    }

    #[test]
    fn endpoint_responses_are_chunked_without_truncation() {
        let (response_tx, response_rx) = mpsc::channel();
        let (event_tx, mut event_rx) = tokio_mpsc::channel(8);
        spawn_response_waiter(
            7,
            "boot-a".into(),
            "request-a".into(),
            response_rx,
            event_tx,
        )
        .unwrap();
        let response = "x".repeat(ENDPOINT_RESPONSE_CHUNK_BYTES + 17);
        response_tx.send(response.clone()).unwrap();

        let mut received = Vec::new();
        loop {
            let ServerEvent::ClientShellEndpointResponseChunkReady {
                client_id,
                boot_id,
                request_id,
                final_chunk,
                data,
            } = event_rx.blocking_recv().expect("response chunk")
            else {
                panic!("expected response chunk");
            };
            assert_eq!(client_id, 7);
            assert_eq!(boot_id, "boot-a");
            assert_eq!(request_id, "request-a");
            received.extend(data);
            if final_chunk {
                break;
            }
        }

        assert_eq!(received, response.as_bytes());
    }
}
