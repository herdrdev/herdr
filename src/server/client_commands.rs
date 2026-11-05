use std::io;
use std::sync::mpsc;

use tokio::sync::mpsc as tokio_mpsc;

use crate::api::schema::{ErrorBody, ErrorResponse, Method};

use super::client_transport::ServerEvent;

pub(crate) const MAX_ENDPOINT_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ENDPOINT_BOOT_ID_BYTES: usize = 128;
pub(crate) const MAX_ENDPOINT_REQUEST_ID_BYTES: usize = 128;
const ENDPOINT_RESPONSE_CHUNK_BYTES: usize = 512 * 1024;

pub(crate) fn supported_client_shell_method_names() -> &'static [&'static str] {
    crate::api::schema::operations::client_shell_method_names()
}

pub(crate) fn supports_client_shell_method_name(method: &str) -> bool {
    supported_client_shell_method_names().contains(&method)
}

pub(crate) fn supports_client_shell_method(method: &Method) -> bool {
    method.operation().client_shell
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

pub(crate) fn success_message_with_result(
    boot_id: String,
    request_id: String,
    result: crate::api::schema::ResponseResult,
) -> crate::protocol::ServerMessage {
    let response = serde_json::to_string(&crate::api::schema::SuccessResponse {
        id: request_id.clone(),
        result,
    })
    .unwrap_or_else(|_| {
        error_response(
            request_id.clone(),
            "serialization_error",
            "failed to serialize endpoint response",
        )
    });
    crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
        boot_id,
        request_id,
        final_chunk: true,
        data: response.into_bytes(),
    }
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
        .name("shepherd-client-endpoint-response".into())
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

        for method in supported_client_shell_method_names() {
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
        let mut actual = endpoint_method_shape_digests();
        // Freeze additive methods separately without rewriting the published fixture.
        assert_eq!(
            actual.remove("pane.clear").as_deref(),
            Some("0301d288ba198ddaa427dd7421c71911cccaf4ea03544531efa8b67ca21b08f6")
        );
        assert_eq!(
            actual.remove("pane.link.resolve").as_deref(),
            Some("f5e4a3e01453ae7b188f127ce951c12c20e0bebcc17cc364eeb6d1a01fd5bf81")
        );
        assert_eq!(
            actual.remove("shepherd.call").as_deref(),
            Some("3fd905e0f2b2ff38463cb28ff9c30d51b3f89155d536c4fe0aca751842c4f1fe")
        );
        assert_eq!(
            actual.remove("shepherd.agent.clean").as_deref(),
            Some("e153d9ce126213ce5efa29ecf562eac02a95cb8e93c340f0a16dc916b0dee275")
        );
        assert_eq!(
            actual.remove("shepherd.agent.focus").as_deref(),
            Some("708b276b9062c8779ca14da948bb5285a159f9b18945925af2583b268be61440")
        );
        assert_eq!(
            actual.remove("shepherd.agent.kill").as_deref(),
            Some("4c04fdb8b354f8c44619c1a1666480f89feb4b554986f6aba0bf799bf8f85a13")
        );
        assert_eq!(
            actual.remove("shepherd.agent.list").as_deref(),
            Some("79ef8afef4e46c41427934a191e4549d72a02ee61b46cba1b18655990f16103b")
        );
        assert_eq!(
            actual.remove("shepherd.agent.reconcile").as_deref(),
            Some("680009ac892cc8055c506eed5733ffa4d900bf1c6b7d8e41f1639c6075207610")
        );
        assert_eq!(
            actual.remove("shepherd.agent.resume").as_deref(),
            Some("a0df2ed784ecd239be63ba82dd178b9c25ad178a93342030521057fae60ea878")
        );
        assert_eq!(
            actual.remove("shepherd.agent.spawn").as_deref(),
            Some("840d71c5609cfaf30c8a8c3307bb717db3a7d6bb653be1d35ec665b9070b5b22")
        );
        assert_eq!(
            actual.remove("shepherd.git.status").as_deref(),
            Some("9a23d30ceab2e8185bc3efb41023523e75dc8f89110dccd2cbfda0d2012feb42")
        );
        assert_eq!(
            actual.remove("shepherd.run.approve").as_deref(),
            Some("6bab0c13537073cf0a4333c302713fb5fcac01d931d67a9c05b318a429a1145f")
        );
        assert_eq!(
            actual.remove("shepherd.run.list").as_deref(),
            Some("e21720b95bcca606d9e4a52e77edc6f1e3b6083d8799359ae606269bf1cd7b0d")
        );
        assert_eq!(
            actual.remove("shepherd.run.reject").as_deref(),
            Some("b8c0ec64ef78d27f767d1cea7ea216e76968385e9abf4a49b8f0383ad4c90018")
        );
        assert_eq!(
            actual.remove("shepherd.run.start").as_deref(),
            Some("85468c27ec93c9920361d3c16591f1e4c62e23b75467995c6fea03fc00514c72")
        );
        assert_eq!(
            actual.remove("shepherd.skill.list").as_deref(),
            Some("6a2a7432ac2fc06380a018c9871361fb531a40e6e34d99511ca57fd0a7ddd038")
        );
        assert_eq!(
            actual.remove("shepherd.task.archive").as_deref(),
            Some("7b5637c22ed7e0a315fa02feabbb4e2c8902c49856bd095ce14f3f5f345d7745")
        );
        assert_eq!(
            actual.remove("shepherd.task.create").as_deref(),
            Some("1dac4b8982d0e4cb4c82271009152dd5a3976e6f9d1848966085a256df3c28e5")
        );
        assert_eq!(
            actual.remove("shepherd.task.list").as_deref(),
            Some("d5a20d5985bef170d0a875c679e0238f795e6917b2c875acd3d97c186b232646")
        );
        assert_eq!(
            actual.remove("shepherd.pattern.delete").as_deref(),
            Some("4239180c954ccea02f1f2a5c5ca9a6e5b90e294181228f5e9f496e333cbe69bb")
        );
        assert_eq!(
            actual.remove("shepherd.pattern.get").as_deref(),
            Some("59584787f8b06958e554b6780022edbcfcaf44bfc6c33c94173b29c77d8173fc")
        );
        assert_eq!(
            actual.remove("shepherd.pattern.list").as_deref(),
            Some("26484e5b246e5ee74868a24c966243e2a611257e9e112631196969f85a037a07")
        );
        assert_eq!(
            actual.remove("shepherd.pattern.save").as_deref(),
            Some("bf7b742c475ce0e560177e8ddd0b3a9df21f85ce3267509eec8910a779e2745b")
        );
        assert_eq!(
            actual.remove("shepherd.pattern.validate").as_deref(),
            Some("acae7d7b5598828187644aa8bf77bbd509f407fb8e6d2e5cfc57b95e26772504")
        );
        assert_eq!(
            actual.remove("shepherd.topic.create").as_deref(),
            Some("103480c99abc40c6af01219ab678806c4e19d5db92b4ba1db7e88c461cc6ec6f")
        );
        assert_eq!(
            actual.remove("shepherd.topic.groups").as_deref(),
            Some("655b28877f7bccdae9272a5ed73318e012f8140365d63036b5a4462d0883cd86")
        );
        assert_eq!(
            actual.remove("shepherd.topic.list").as_deref(),
            Some("1dd8ded7b062ffac82d361925e05f7c366ae7420a418ed484a15dcc2ffe4f411")
        );
        assert_eq!(
            actual.remove("shepherd.topic.publish").as_deref(),
            Some("6c60b1c2fdd7ef730bb8a4588ce08c2d63ee54e78fb1f2ca6b88dc938f89e414")
        );
        assert_eq!(
            actual.remove("shepherd.topic.replay").as_deref(),
            Some("9119f97f31dcb9a70462f43f66bdea9abc8651b77d4393fe97c92f1569869085")
        );
        assert_eq!(
            actual.remove("shepherd.topic.tail").as_deref(),
            Some("ad498af9deabf61f97d3ea37ba31d916ab5cf103b801529cecdd0c2e23484823")
        );

        assert_eq!(
            actual, expected,
            "an existing endpoint method changed shape; add load-bearing behavior as a new advertised method or explicitly gate new fields"
        );
    }

    #[test]
    fn advertised_client_shell_methods_are_sorted_unique_and_in_schema() {
        assert!(supported_client_shell_method_names()
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
        for method in supported_client_shell_method_names() {
            assert!(
                schema_methods.iter().any(|candidate| candidate == method),
                "advertised endpoint method {method:?} is absent from the request schema"
            );
        }
    }

    #[test]
    fn client_shell_lane_excludes_api_front_door_and_lifecycle_methods() {
        assert!(supports_client_shell_method(
            &Method::ClientShellSurfaceSet(crate::api::schema::ClientShellSurfaceSetParams {
                active: false,
            })
        ));
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
        assert!(supports_client_shell_method(&Method::PaneLinkResolve(
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
