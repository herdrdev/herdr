use std::collections::HashMap;

use super::*;

fn protocol_schema_entry<T: schemars::JsonSchema>(name: &str) -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    rewrite_schema_refs(&mut schema, name);
    schema
}

fn rewrite_schema_refs(value: &mut serde_json::Value, schema_name: &str) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(reference)) = object.get_mut("$ref") {
                if let Some(path) = reference.strip_prefix("#/") {
                    *reference = format!("#/schemas/{schema_name}/{path}");
                }
            }
            for child in object.values_mut() {
                rewrite_schema_refs(child, schema_name);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_schema_refs(item, schema_name);
            }
        }
        _ => {}
    }
}

fn protocol_schema_document() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Herdr API",
        "schema_version": 1,
        "protocol": crate::protocol::PROTOCOL_VERSION,
        "schemas": {
            "request": protocol_schema_entry::<Request>("request"),
            "success_response": protocol_schema_entry::<SuccessResponse>("success_response"),
            "error_response": protocol_schema_entry::<ErrorResponse>("error_response"),
            "event": protocol_schema_entry::<EventEnvelope>("event"),
            "subscription_event": protocol_schema_entry::<SubscriptionEventEnvelope>("subscription_event"),
        },
    })
}

#[test]
fn request_uses_dot_method_names() {
    let request = Request {
        id: "req_1".into(),
        method: Method::WorkspaceCreate(WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: Some("/tmp".into()),
            focus: true,
            label: Some("api".into()),
            env: Default::default(),
            work_context: Some(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["SCA-42".into()],
                pr_urls: vec!["https://github.com/o/r/pull/42".into()],
                branch: Some("feat/binding".into()),
                role: Some(crate::work_context::PaneWorkRole::Ship),
                active_owner: true,
                ..Default::default()
            }),
        }),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "workspace.create");
    assert_eq!(json["params"]["work_context"]["role"], "ship");
    assert_eq!(json["params"]["work_context"]["active_owner"], true);
}

#[test]
fn ac1_loop_run_history_is_typed_on_the_json_api() {
    let request = Request {
        id: "loop-history".into(),
        method: Method::LoopRunHistory(LoopRunHistoryParams {
            loop_id: Some("daily".into()),
        }),
    };
    let request_json = serde_json::to_value(&request).unwrap();
    assert_eq!(request_json["method"], "loop.run_history");
    assert_eq!(
        serde_json::from_value::<Request>(request_json).unwrap(),
        request
    );

    let response = SuccessResponse {
        id: "loop-history".into(),
        result: ResponseResult::LoopRunHistory {
            loop_id: Some("daily".into()),
            runs: vec![LoopRunInfo {
                run_id: "run-1".into(),
                skill: "aship".into(),
                session: Some("session".into()),
                pr: Some(800),
                ticket: None,
                loop_id: Some("daily".into()),
                start: "2026-08-10T10:00:00Z".into(),
                end: None,
                wall_min: None,
                blocked_min: None,
                gates: vec![LoopGateInfo {
                    kind: "preference".into(),
                    defaulted: true,
                    recommendation_matched: None,
                }],
                human_touches: Some(2),
                touches_by_type: Default::default(),
                interrupted_focus: None,
                review_rounds: None,
                out_tokens: None,
                outcome: "in_flight".into(),
            }],
            skipped_lines: 1,
        },
    };
    let restored: SuccessResponse =
        serde_json::from_value(serde_json::to_value(&response).unwrap()).unwrap();
    assert_eq!(restored, response);
}

#[test]
fn symphony_list_is_a_typed_read_only_api_method() {
    let request = Request {
        id: "symphony".to_string(),
        method: Method::SymphonyList(EmptyParams::default()),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["method"], "symphony.list");
    assert!(value.to_string().contains("symphony.list"));
    assert!(!value.to_string().contains("signal"));
    assert!(!value.to_string().contains("cancel"));
}

#[test]
fn fleet_list_is_a_typed_read_only_api_method() {
    let request = Request {
        id: "fleet".to_string(),
        method: Method::FleetList(EmptyParams::default()),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["method"], "fleet.list");
    assert_eq!(serde_json::from_value::<Request>(value).unwrap(), request);
}

#[test]
fn workspace_close_group_intent_defaults_false_and_round_trips() {
    let request: Request = serde_json::from_value(serde_json::json!({
        "id": "close",
        "method": "workspace.close",
        "params": { "workspace_id": "w1" }
    }))
    .unwrap();
    assert!(matches!(
        request.method,
        Method::WorkspaceClose(WorkspaceCloseParams {
            close_group: false,
            ..
        })
    ));

    let explicit = Request {
        id: "close-group".into(),
        method: Method::WorkspaceClose(WorkspaceCloseParams {
            workspace_id: "w1".into(),
            close_group: true,
        }),
    };
    let json = serde_json::to_value(&explicit).unwrap();
    assert_eq!(json["params"]["close_group"], true);
    assert_eq!(serde_json::from_value::<Request>(json).unwrap(), explicit);
}

#[test]
fn agent_start_and_prompt_requests_round_trip() {
    let start = Request {
        id: "start".into(),
        method: Method::AgentStart(AgentStartParams {
            name: "reviewer".into(),
            kind: "pi".into(),
            pane_id: "w1:p2".into(),
            args: vec!["--no-session".into()],
            timeout_ms: Some(30_000),
        }),
    };
    let start_json = serde_json::to_value(&start).unwrap();
    assert_eq!(start_json["method"], "agent.start");
    assert_eq!(start_json["params"]["pane_id"], "w1:p2");
    assert_eq!(
        serde_json::from_value::<Request>(start_json).unwrap(),
        start
    );

    let prompt = Request {
        id: "prompt".into(),
        method: Method::AgentPrompt(AgentPromptParams {
            target: "reviewer".into(),
            text: "review this".into(),
            wait: None,
        }),
    };
    let prompt_json = serde_json::to_value(&prompt).unwrap();
    assert_eq!(prompt_json["method"], "agent.prompt");
    assert_eq!(
        serde_json::from_value::<Request>(prompt_json).unwrap(),
        prompt
    );

    let prompt_and_wait = Request {
        id: "prompt-and-wait".into(),
        method: Method::AgentPrompt(AgentPromptParams {
            target: "reviewer".into(),
            text: "review this".into(),
            wait: Some(AgentPromptWaitOptions {
                until: vec![AgentStatus::Idle, AgentStatus::Done],
                timeout_ms: Some(120_000),
                submission_deadline: None,
            }),
        }),
    };
    let prompt_and_wait_json = serde_json::to_value(&prompt_and_wait).unwrap();
    assert_eq!(
        prompt_and_wait_json["params"]["wait"]["until"],
        serde_json::json!(["idle", "done"])
    );
    assert_eq!(
        prompt_and_wait_json["params"]["wait"]["timeout_ms"],
        120_000
    );
    assert_eq!(
        serde_json::from_value::<Request>(prompt_and_wait_json).unwrap(),
        prompt_and_wait
    );
}

#[test]
fn bundled_protocol_schema_refs_resolve_inside_bundle() {
    fn assert_no_standalone_refs(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(serde_json::Value::String(reference)) = object.get("$ref") {
                    assert!(
                        !reference.starts_with("#/$defs/"),
                        "schema bundle contains standalone ref {reference}"
                    );
                }
                for child in object.values() {
                    assert_no_standalone_refs(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    assert_no_standalone_refs(item);
                }
            }
            _ => {}
        }
    }

    assert_no_standalone_refs(&protocol_schema_document());
}

#[test]
fn generated_protocol_schema_artifact_is_current() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&protocol_schema_document()).unwrap()
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/next/api/herdr-api.schema.json");

    if std::env::var_os("HERDR_UPDATE_API_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read {}; run `HERDR_UPDATE_API_SCHEMA=1 just test-one generated_protocol_schema_artifact_is_current`: {err}",
            path.display()
        )
    });
    assert_eq!(
        expected,
        actual,
        "generated API schema artifact is stale; run `HERDR_UPDATE_API_SCHEMA=1 just test-one generated_protocol_schema_artifact_is_current`"
    );
}

#[test]
fn request_round_trips_for_server_stop() {
    let request = Request {
        id: "req_stop".into(),
        method: Method::ServerStop(EmptyParams::default()),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "server.stop");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn request_round_trips_for_server_reload_config() {
    let request = Request {
        id: "req_reload".into(),
        method: Method::ServerReloadConfig(EmptyParams::default()),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "server.reload_config");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn group_mutation_requests_are_typed_and_carry_expected_revisions() {
    let group_id: crate::groups::GroupId = serde_json::from_value(serde_json::json!({
        "owner": "AQEBAQEBAQEBAQEBAQEBAQ",
        "local": 7
    }))
    .unwrap();
    let requests = [
        Request {
            id: "create".into(),
            method: Method::GroupCreate(GroupCreateParams {
                name: "Work".into(),
                expected_revision: 3,
            }),
        },
        Request {
            id: "rename".into(),
            method: Method::GroupRename(GroupRenameParams {
                group_id: group_id.clone(),
                name: "Focus".into(),
                expected_revision: 4,
            }),
        },
        Request {
            id: "delete".into(),
            method: Method::GroupDelete(GroupDeleteParams {
                group_id: group_id.clone(),
                expected_revision: 5,
            }),
        },
        Request {
            id: "assign".into(),
            method: Method::PaneGroupSet(PaneGroupSetParams {
                pane_id: "workspace:p1".into(),
                group_id: Some(group_id),
                expected_revision: 6,
                expected_pane_authority: None,
                expected_pane_incarnation: None,
            }),
        },
    ];

    for request in requests {
        let json = serde_json::to_value(&request).unwrap();
        assert!(json["params"]["expected_revision"].is_u64());
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
    }
}

#[test]
fn groups_capability_defaults_off_for_older_servers() {
    let capabilities: ServerCapabilities = serde_json::from_value(serde_json::json!({
        "live_handoff": true
    }))
    .unwrap();

    assert!(!capabilities.groups_v1);
}

#[test]
fn theme_requests_round_trip() {
    for method in [
        Method::ThemeStatus(EmptyParams::default()),
        Method::ThemeSet(ThemeSetParams {
            host_appearance: crate::config::HostAppearanceOverride::Light,
        }),
    ] {
        let request = Request {
            id: "req_theme".into(),
            method,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert!(matches!(
            json["method"].as_str(),
            Some("theme.status" | "theme.set")
        ));
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, request);
    }
}

#[test]
fn request_round_trips_for_server_reload_agent_manifests() {
    let request = Request {
        id: "req_reload_agent_manifests".into(),
        method: Method::ServerReloadAgentManifests(EmptyParams::default()),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "server.reload_agent_manifests");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn request_round_trips_for_server_agent_manifests() {
    let request = Request {
        id: "req_agent_manifests".into(),
        method: Method::ServerAgentManifests(EmptyParams::default()),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "server.agent_manifests");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn request_round_trips_for_agent_explain() {
    let request = Request {
        id: "req_agent_explain".into(),
        method: Method::AgentExplain(AgentTarget {
            target: "agent-1".into(),
        }),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "agent.explain");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn agent_focus_requests_support_legacy_target_and_agent_ref_forms() {
    let legacy = serde_json::json!({
        "id": "focus-target",
        "method": "agent.focus",
        "params": {"target": "w1:p3"}
    });
    let request: Request = serde_json::from_value(legacy.clone()).expect("legacy focus request");
    assert_eq!(
        serde_json::to_value(request).expect("legacy request JSON"),
        legacy
    );

    let remote = serde_json::json!({
        "id": "focus-ref",
        "method": "agent.focus",
        "params": {"agent_ref": "buildbox::w1:p3"}
    });
    let request: Request = serde_json::from_value(remote.clone()).expect("agent ref request");
    assert_eq!(
        serde_json::to_value(request).expect("agent ref request JSON"),
        remote
    );

    let status = serde_json::json!({
        "id": "focus-status",
        "method": "agent.focus.status",
        "params": {"operation_id": "remote-focus-1"}
    });
    let request: Request = serde_json::from_value(status.clone()).expect("focus status request");
    assert_eq!(
        serde_json::to_value(request).expect("focus status request JSON"),
        status
    );
}

#[test]
fn remote_focus_schema_requires_exclusive_targets_and_state_details() {
    let schema = protocol_schema_document();
    let focus_params = schema
        .pointer("/schemas/request/$defs/AgentFocusParams")
        .expect("agent focus params schema");
    assert_eq!(
        focus_params["oneOf"],
        serde_json::json!([
            {
                "properties": {"target": {"type": "string"}},
                "required": ["target"],
                "not": {"required": ["agent_ref"]}
            },
            {
                "properties": {"agent_ref": {"not": {"type": "null"}}},
                "required": ["agent_ref"],
                "not": {"required": ["target"]}
            }
        ])
    );

    let variants = schema
        .pointer("/schemas/success_response/$defs/ResponseResult/oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("response result variants");
    let status = variants
        .iter()
        .find(|variant| {
            variant["properties"]["type"]["const"]
                == serde_json::Value::String("agent_focus_status".into())
        })
        .expect("agent focus status schema");
    assert_eq!(
        status["allOf"],
        serde_json::json!([
            {
                "if": {"properties": {"state": {"const": "active"}}},
                "then": {
                    "properties": {"context": {"not": {"type": "null"}}},
                    "required": ["context"]
                }
            },
            {
                "if": {"properties": {"state": {"const": "failed"}}},
                "then": {
                    "properties": {"error": {"not": {"type": "null"}}},
                    "required": ["error"]
                }
            }
        ])
    );
}

#[test]
fn pane_snooze_schema_and_requests_support_exclusive_duration_or_deadline() {
    for params in [
        serde_json::json!({"pane_id": "w1:p3", "duration_s": 900}),
        serde_json::json!({"pane_id": "w1:p3", "snoozed_until": 1_725_000_900_u64}),
    ] {
        let request = serde_json::json!({
            "id": "snooze",
            "method": "pane.snooze",
            "params": params,
        });
        let parsed: Request = serde_json::from_value(request.clone()).expect("snooze request");
        assert_eq!(serde_json::to_value(parsed).expect("snooze JSON"), request);
    }

    for params in [
        serde_json::json!({"pane_id": "w1:p3"}),
        serde_json::json!({"pane_id": "w1:p3", "duration_s": null}),
        serde_json::json!({
            "pane_id": "w1:p3",
            "duration_s": 60,
            "snoozed_until": null
        }),
        serde_json::json!({
            "pane_id": "w1:p3",
            "duration_s": 60,
            "snoozed_until": 1_725_000_060_u64
        }),
    ] {
        let request = serde_json::json!({
            "id": "snooze-invalid",
            "method": "pane.snooze",
            "params": params,
        });
        assert!(
            serde_json::from_value::<Request>(request).is_err(),
            "invalid snooze shape was accepted"
        );
    }

    let params = protocol_schema_document()
        .pointer("/schemas/request/$defs/PaneSnoozeParams")
        .expect("pane snooze params schema")
        .clone();
    assert_eq!(
        params["oneOf"],
        serde_json::json!([
            {
                "required": ["duration_s"],
                "not": {"required": ["snoozed_until"]}
            },
            {
                "required": ["snoozed_until"],
                "not": {"required": ["duration_s"]}
            }
        ])
    );
    assert_eq!(params["properties"]["duration_s"]["type"], "integer");
    assert_eq!(params["properties"]["snoozed_until"]["type"], "integer");
}

#[test]
fn notification_show_request_parses() {
    let json = r#"{"id":"req_1","method":"notification.show","params":{"title":"build failed","body":"api workspace","position":"top-left","sound":"request"}}"#;
    let request: Request = serde_json::from_str(json).unwrap();
    let Method::NotificationShow(params) = request.method else {
        panic!("wrong method parsed");
    };
    assert_eq!(params.title, "build failed");
    assert_eq!(params.body.as_deref(), Some("api workspace"));
    assert_eq!(
        params.position,
        Some(crate::config::ToastHerdrPosition::TopLeft)
    );
    assert_eq!(params.sound, NotificationShowSound::Request);
}

#[test]
fn notification_show_sound_defaults_to_none() {
    let json = r#"{"id":"req_1","method":"notification.show","params":{"title":"build failed"}}"#;
    let request: Request = serde_json::from_str(json).unwrap();
    let Method::NotificationShow(params) = request.method else {
        panic!("wrong method parsed");
    };

    assert_eq!(params.sound, NotificationShowSound::None);
}

#[test]
fn client_window_title_requests_round_trip() {
    let set = Request {
        id: "req_title_set".into(),
        method: Method::ClientWindowTitleSet(ClientWindowTitleSetParams {
            title: "herdr api".into(),
        }),
    };
    let json = serde_json::to_value(&set).unwrap();
    assert_eq!(json["method"], "client.window_title.set");
    assert_eq!(json["params"]["title"], "herdr api");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, set);

    let clear = Request {
        id: "req_title_clear".into(),
        method: Method::ClientWindowTitleClear(EmptyParams::default()),
    };
    let json = serde_json::to_value(&clear).unwrap();
    assert_eq!(json["method"], "client.window_title.clear");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, clear);
}

#[test]
fn agent_view_requests_round_trip() {
    let set_json = serde_json::json!({
        "id": "view-set",
        "method": "agent.view.set",
        "params": {
            "source": "example.views",
            "label": "current + attention",
            "filter": {
                "op": "any",
                "filters": [
                    {
                        "op": "eq",
                        "field": "workspace_id",
                        "value": {"context": "current_workspace_id"}
                    },
                    {
                        "op": "in",
                        "field": "status",
                        "values": ["blocked", "done"]
                    }
                ]
            },
            "sort": [
                {"field": "attention", "order": "desc"},
                {"field": "state_change_seq", "order": "desc"}
            ]
        }
    });
    let request: Request = serde_json::from_value(set_json.clone()).unwrap();
    assert!(matches!(request.method, Method::AgentViewSet(_)));
    assert_eq!(serde_json::to_value(request).unwrap(), set_json);

    let clear_json = serde_json::json!({
        "id": "view-clear",
        "method": "agent.view.clear",
        "params": {"source": "example.views"}
    });
    let request: Request = serde_json::from_value(clear_json.clone()).unwrap();
    assert!(matches!(request.method, Method::AgentViewClear(_)));
    assert_eq!(serde_json::to_value(request).unwrap(), clear_json);
}

#[test]
fn unknown_method_is_rejected() {
    let json = r#"{"id":"req_1","method":"nope","params":{}}"#;
    let err = serde_json::from_str::<Request>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown variant"));
}

#[test]
fn pane_send_text_if_is_a_distinct_method_that_legacy_servers_reject_before_send() {
    #[derive(serde::Deserialize)]
    struct LegacyRequest {
        #[serde(flatten)]
        method: LegacyMethod,
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "method", content = "params")]
    enum LegacyMethod {
        #[serde(rename = "pane.send_text")]
        PaneSendText(PaneSendTextParams),
    }

    fn legacy_dispatch(encoded: &str, sent: &mut Vec<u8>) -> Result<(), serde_json::Error> {
        let legacy = serde_json::from_str::<LegacyRequest>(encoded)?;
        match legacy.method {
            LegacyMethod::PaneSendText(params) => sent.extend_from_slice(params.text.as_bytes()),
        }
        Ok(())
    }

    let legacy_request = Request {
        id: "legacy".into(),
        method: Method::PaneSendText(PaneSendTextParams {
            pane_id: "workspace-1".into(),
            text: "legacy".into(),
        }),
    };
    let mut sent = Vec::new();
    legacy_dispatch(&serde_json::to_string(&legacy_request).unwrap(), &mut sent).unwrap();
    assert_eq!(sent, b"legacy");
    sent.clear();

    let request = Request {
        id: "guarded".into(),
        method: Method::PaneSendTextIf(PaneSendTextIfParams {
            pane_id: "workspace-1".into(),
            text: "answer".into(),
            workspace_id: "workspace".into(),
            terminal_id: "terminal".into(),
            agent_ref: AgentRef::new("host", "workspace-1").unwrap(),
            agent_session: AgentSessionInfo {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                kind: crate::agent_resume::AgentSessionRefKind::Id,
                value: "session".into(),
            },
            condition: PaneSendTextCondition::DetectionSnapshotUnchanged,
            observation_token: "token".into(),
        }),
    };
    let encoded = serde_json::to_string(&request).unwrap();
    assert!(encoded.contains(r#""method":"pane.send_text_if""#));

    assert!(legacy_dispatch(&encoded, &mut sent).is_err());
    assert!(sent.is_empty());
}

#[test]
fn missing_required_params_are_rejected() {
    let json = r#"{"id":"req_1","method":"pane.send_text","params":{"pane_id":"p_1"}}"#;
    let err = serde_json::from_str::<Request>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("text"));
}

#[test]
fn pane_send_input_defaults_to_empty_text_and_keys() {
    let json = r#"
    {
        "id": "req_1",
        "method": "pane.send_input",
        "params": {
            "pane_id": "p_1"
        }
    }
    "#;

    let request: Request = serde_json::from_str(json).unwrap();
    let Method::PaneSendInput(params) = request.method else {
        panic!("wrong method parsed");
    };
    assert_eq!(params.pane_id, "p_1");
    assert!(params.text.is_empty());
    assert!(params.keys.is_empty());
}

#[test]
fn pane_wait_for_output_defaults_strip_ansi_to_true() {
    let json = r#"
    {
        "id": "req_1",
        "method": "pane.wait_for_output",
        "params": {
            "pane_id": "p_1",
            "source": "recent",
            "match": { "type": "substring", "value": "ready" }
        }
    }
    "#;

    let request: Request = serde_json::from_str(json).unwrap();
    let Method::PaneWaitForOutput(params) = request.method else {
        panic!("wrong method parsed");
    };
    assert!(params.strip_ansi);
}

#[test]
fn pane_read_defaults_to_text_format() {
    let json = r#"
    {
        "id": "req_1",
        "method": "pane.read",
        "params": {
            "pane_id": "p_1",
            "source": "visible"
        }
    }
    "#;

    let request: Request = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_value(&request).unwrap();
    assert!(serialized["params"].get("intent").is_none());
    let Method::PaneRead(params) = request.method else {
        panic!("wrong method parsed");
    };
    assert_eq!(params.format, ReadFormat::Text);
    assert_eq!(params.intent, ReadIntent::Interactive);
}

#[test]
fn pane_current_request_round_trips() {
    let request = Request {
        id: "req_current".into(),
        method: Method::PaneCurrent(PaneCurrentParams {
            caller_pane_id: Some("w1-1".into()),
        }),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "pane.current");
    assert_eq!(json["params"]["caller_pane_id"], "w1-1");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn pane_process_info_request_round_trips() {
    let request = Request {
        id: "req_process_info".into(),
        method: Method::PaneProcessInfo(PaneProcessInfoParams {
            pane_id: Some("w1-1".into()),
        }),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "pane.process_info");
    assert_eq!(json["params"]["pane_id"], "w1-1");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn event_envelope_round_trips() {
    let events = [
        EventEnvelope {
            event: EventKind::PaneOutputChanged,
            data: EventData::PaneOutputChanged {
                pane_id: "p_1".into(),
                workspace_id: "w_1".into(),
                revision: 42,
            },
        },
        EventEnvelope {
            event: EventKind::PaneSettled,
            data: EventData::PaneSettled {
                pane_id: "p_1".into(),
                workspace_id: "w_1".into(),
                settled_at: 1_725_000_000,
            },
        },
        EventEnvelope {
            event: EventKind::PaneUnsettled,
            data: EventData::PaneUnsettled {
                pane_id: "p_1".into(),
                workspace_id: "w_1".into(),
            },
        },
        EventEnvelope {
            event: EventKind::PaneSnoozed,
            data: EventData::PaneSnoozed {
                pane_id: "p_1".into(),
                workspace_id: "w_1".into(),
                snoozed_until: 1_725_000_060,
            },
        },
        EventEnvelope {
            event: EventKind::PaneUnsnoozed,
            data: EventData::PaneUnsnoozed {
                pane_id: "p_1".into(),
                workspace_id: "w_1".into(),
                reason: PaneUnsnoozeReason::Expired,
            },
        },
        EventEnvelope {
            event: EventKind::WorkspaceMoved,
            data: EventData::WorkspaceMoved {
                workspace_id: "w_1".into(),
                insert_index: 2,
                workspaces: vec![],
            },
        },
        EventEnvelope {
            event: EventKind::WorkspaceReordered,
            data: EventData::WorkspaceReordered {
                workspace_ids: vec!["w_1".into(), "w_2".into()],
                before_workspace_id: Some("w_3".into()),
                workspaces: vec![],
            },
        },
        EventEnvelope {
            event: EventKind::TabMoved,
            data: EventData::TabMoved {
                tab_id: "w_1:1".into(),
                workspace_id: "w_1".into(),
                insert_index: 1,
                tabs: vec![],
            },
        },
        EventEnvelope {
            event: EventKind::LayoutUpdated,
            data: EventData::LayoutUpdated {
                layout: PaneLayoutSnapshot {
                    workspace_id: "w_1".into(),
                    tab_id: "w_1:1".into(),
                    zoomed: false,
                    area: PaneLayoutRect {
                        x: 0,
                        y: 0,
                        width: 100,
                        height: 24,
                    },
                    focused_pane_id: "w_1-1".into(),
                    panes: vec![PaneLayoutPane {
                        pane_id: "w_1-1".into(),
                        focused: true,
                        rect: PaneLayoutRect {
                            x: 0,
                            y: 0,
                            width: 100,
                            height: 24,
                        },
                    }],
                    splits: vec![],
                },
            },
        },
    ];

    for event in events {
        let json = serde_json::to_string(&event).unwrap();
        let restored: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, event);
    }
}

#[test]
fn settlement_and_snooze_subscriptions_use_dot_event_names() {
    let request = Request {
        id: "sub_settlement".into(),
        method: Method::EventsSubscribe(EventsSubscribeParams {
            subscriptions: vec![
                Subscription::PaneSettled {},
                Subscription::PaneUnsettled {},
                Subscription::PaneSnoozed {},
                Subscription::PaneUnsnoozed {},
            ],
        }),
    };
    let json = serde_json::to_string(&request).expect("settlement subscription JSON");
    assert!(json.contains("\"type\":\"pane.settled\""));
    assert!(json.contains("\"type\":\"pane.unsettled\""));
    assert!(json.contains("\"type\":\"pane.snoozed\""));
    assert!(json.contains("\"type\":\"pane.unsnoozed\""));
    assert_eq!(EventKind::PaneSettled.dot_name(), "pane.settled");
    assert_eq!(EventKind::PaneUnsettled.dot_name(), "pane.unsettled");
    assert_eq!(EventKind::PaneSnoozed.dot_name(), "pane.snoozed");
    assert_eq!(EventKind::PaneUnsnoozed.dot_name(), "pane.unsnoozed");
}

#[test]
fn subscribe_request_parses_parameterized_subscriptions() {
    let json = r#"
    {
        "id": "sub_1",
        "method": "events.subscribe",
        "params": {
            "subscriptions": [
                {
                    "type": "pane.output_matched",
                    "pane_id": "p_1_1",
                    "source": "recent",
                    "lines": 200,
                    "match": { "type": "substring", "value": "auth: received" }
                },
                {
                    "type": "pane.agent_status_changed",
                    "pane_id": "p_1_1",
                    "agent_status": "done"
                },
                {
                    "type": "pane.scroll_changed",
                    "pane_id": "p_1_1"
                }
            ]
        }
    }
    "#;

    let request: Request = serde_json::from_str(json).unwrap();
    let Method::EventsSubscribe(params) = request.method else {
        panic!("wrong method parsed");
    };
    assert_eq!(params.subscriptions.len(), 3);
    assert!(matches!(
        &params.subscriptions[0],
        Subscription::PaneOutputMatched {
            pane_id,
            source: ReadSource::Recent,
            lines: Some(200),
            r#match: OutputMatch::Substring { value },
            strip_ansi: true,
        } if pane_id == "p_1_1" && value == "auth: received"
    ));
    assert!(matches!(
        &params.subscriptions[1],
        Subscription::PaneAgentStatusChanged {
            pane_id,
            agent_status: Some(AgentStatus::Done),
        } if pane_id == "p_1_1"
    ));
    assert!(matches!(
        &params.subscriptions[2],
        Subscription::PaneScrollChanged { pane_id } if pane_id == "p_1_1"
    ));
}

#[test]
fn subscription_event_envelope_round_trips() {
    let event = SubscriptionEventEnvelope {
        event: SubscriptionEventKind::PaneOutputMatched,
        data: SubscriptionEventData::PaneOutputMatched(PaneOutputMatchedEvent {
            pane_id: "p_1_1".into(),
            matched_line: "auth: received".into(),
            read: PaneReadResult {
                pane_id: "p_1_1".into(),
                terminal_id: "term_1".into(),
                workspace_id: "w_1".into(),
                tab_id: "t_1_1".into(),
                agent_ref: None,
                agent_session: None,
                source: ReadSource::Recent,
                format: ReadFormat::Text,
                text: "auth: received\n".into(),
                revision: 0,
                truncated: false,
                input_observation: None,
            },
        }),
    };

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"event\":\"pane.output_matched\""));
    let restored: SubscriptionEventEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, event);
}

#[test]
fn scroll_changed_subscription_event_round_trips() {
    let event = SubscriptionEventEnvelope {
        event: SubscriptionEventKind::ScrollChanged,
        data: SubscriptionEventData::ScrollChanged(PaneScrollChangedEvent {
            pane_id: "p_1_1".into(),
            workspace_id: "w_1".into(),
            scroll: PaneScrollInfo {
                offset_from_bottom: 12,
                max_offset_from_bottom: 240,
                viewport_rows: 30,
            },
        }),
    };

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"event\":\"pane.scroll_changed\""));
    let restored: SubscriptionEventEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, event);
}

#[test]
fn success_response_round_trips() {
    let response = SuccessResponse {
        id: "req_1".into(),
        result: ResponseResult::Pong {
            version: "0.1.2".into(),
            protocol: 6,
            capabilities: Some(ServerCapabilities {
                live_handoff: true,
                detached_server_daemon: true,
                groups_v1: true,
            }),
        },
    };

    let json = serde_json::to_string(&response).unwrap();
    let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, response);
}

#[test]
fn session_snapshot_request_and_response_round_trip() {
    let request = Request {
        id: "req_snapshot".into(),
        method: Method::SessionSnapshot(EmptyParams::default()),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"method\":\"session.snapshot\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, request);

    let response = SuccessResponse {
        id: "req_snapshot".into(),
        result: ResponseResult::SessionSnapshot {
            snapshot: Box::new(SessionSnapshot {
                version: "0.1.2".into(),
                protocol: 16,
                focused_workspace_id: None,
                focused_tab_id: None,
                focused_pane_id: None,
                workspaces: Vec::new(),
                tabs: Vec::new(),
                panes: Vec::new(),
                layouts: Vec::new(),
                agents: Vec::new(),
            }),
        },
    };
    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"type\":\"session_snapshot\""));
    let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, response);
}

#[test]
fn worktree_request_and_response_round_trip() {
    let request = Request {
        id: "req_worktree".into(),
        method: Method::WorktreeCreate(WorktreeCreateParams {
            workspace_id: Some("1".into()),
            branch: Some("worktree/api".into()),
            base: Some("HEAD".into()),
            focus: true,
            trust_repository: true,
            ..WorktreeCreateParams::default()
        }),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"trust_repository\":true"));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, request);

    let response = SuccessResponse {
        id: "req_worktree".into(),
        result: ResponseResult::WorktreeCreated {
            workspace: WorkspaceInfo {
                repo_binding: None,
                workspace_id: "w_1".into(),
                number: 2,
                label: "herdr".into(),
                focused: true,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: "w_1:1".into(),
                agent_status: AgentStatus::Unknown,
                tokens: HashMap::new(),
                worktree: Some(WorkspaceWorktreeInfo {
                    repo_key: "/repo/herdr/.git".into(),
                    repo_name: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: "/worktrees/herdr/worktree-api".into(),
                    is_linked_worktree: true,
                }),
            },
            tab: TabInfo {
                tab_id: "w_1:1".into(),
                workspace_id: "w_1".into(),
                number: 1,
                label: "herdr".into(),
                prio: false,
                starred: false,
                focused: true,
                pane_count: 1,
                agent_status: AgentStatus::Unknown,
            },
            root_pane: PaneInfo {
                pane_id: "w_1-1".into(),
                terminal_id: "term_1".into(),
                workspace_id: "w_1".into(),
                tab_id: "w_1:1".into(),
                focused: true,
                settled_at: None,
                snoozed_until: None,
                restore_error: None,
                work_context: Default::default(),
                cwd: Some("/worktrees/herdr/worktree-api".into()),
                foreground_cwd: None,
                label: None,
                agent: None,
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                display_agent: None,
                agent_status: AgentStatus::Unknown,
                waiting_on_agents: false,
                wait: None,
                eta_s: None,
                reported_at: None,
                state_labels: HashMap::new(),
                tokens: HashMap::new(),
                gates: Vec::new(),
                items: Vec::new(),
                decisions: Vec::new(),
                agent_session: None,
                scroll: None,
                revision: 0,
            },
            worktree: WorktreeInfo {
                path: "/worktrees/herdr/worktree-api".into(),
                branch: Some("worktree/api".into()),
                is_bare: false,
                is_detached: false,
                is_prunable: false,
                is_linked_worktree: true,
                open_workspace_id: Some("w_1".into()),
                label: "herdr".into(),
            },
        },
    };
    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"type\":\"worktree_created\""));
    assert!(json.contains("\"worktree\""));
    let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, response);
}

#[test]
fn worktree_lifecycle_events_round_trip() {
    let subscription = Request {
        id: "sub_worktrees".into(),
        method: Method::EventsSubscribe(EventsSubscribeParams {
            subscriptions: vec![
                Subscription::WorktreeCreated {},
                Subscription::WorktreeOpened {},
                Subscription::WorktreeRemoved {},
            ],
        }),
    };
    let json = serde_json::to_string(&subscription).unwrap();
    assert!(json.contains("\"type\":\"worktree.created\""));
    assert!(json.contains("\"type\":\"worktree.opened\""));
    assert!(json.contains("\"type\":\"worktree.removed\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, subscription);

    let workspace = WorkspaceInfo {
        repo_binding: None,
        workspace_id: "w_2".into(),
        number: 2,
        label: "herdr".into(),
        focused: true,
        pane_count: 1,
        tab_count: 1,
        active_tab_id: "w_2:1".into(),
        agent_status: AgentStatus::Unknown,
        tokens: HashMap::new(),
        worktree: Some(WorkspaceWorktreeInfo {
            repo_key: "/repo/herdr/.git".into(),
            repo_name: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/worktrees/herdr/worktree-api".into(),
            is_linked_worktree: true,
        }),
    };
    let worktree = WorktreeInfo {
        path: "/worktrees/herdr/worktree-api".into(),
        branch: Some("worktree/api".into()),
        is_bare: false,
        is_detached: false,
        is_prunable: false,
        is_linked_worktree: true,
        open_workspace_id: Some("w_2".into()),
        label: "herdr".into(),
    };

    for event in [
        EventEnvelope {
            event: EventKind::WorktreeCreated,
            data: EventData::WorktreeCreated {
                workspace: workspace.clone(),
                worktree: worktree.clone(),
            },
        },
        EventEnvelope {
            event: EventKind::WorktreeOpened,
            data: EventData::WorktreeOpened {
                workspace: workspace.clone(),
                worktree: worktree.clone(),
                already_open: false,
            },
        },
        EventEnvelope {
            event: EventKind::WorktreeRemoved,
            data: EventData::WorktreeRemoved {
                workspace_id: "w_2".into(),
                workspace: Some(workspace.clone()),
                worktree: WorktreeInfo {
                    open_workspace_id: None,
                    ..worktree.clone()
                },
                forced: false,
            },
        },
        EventEnvelope {
            event: EventKind::WorkspaceClosed,
            data: EventData::WorkspaceClosed {
                workspace_id: "w_2".into(),
                workspace: Some(workspace.clone()),
            },
        },
    ] {
        let json = serde_json::to_string(&event).unwrap();
        let restored: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, event);
    }
}

#[test]
fn plugin_link_list_unlink_round_trip() {
    let link = Request {
        id: "plugin_link".into(),
        method: Method::PluginLink(PluginLinkParams {
            path: "/plugins/worktree-bootstrap".into(),
            enabled: true,
            source: None,
        }),
    };
    let json = serde_json::to_string(&link).unwrap();
    assert!(json.contains("\"method\":\"plugin.link\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, link);

    let list = Request {
        id: "plugin_list".into(),
        method: Method::PluginList(PluginListParams {
            plugin_id: Some("example.worktree-bootstrap".into()),
        }),
    };
    let json = serde_json::to_string(&list).unwrap();
    assert!(json.contains("\"method\":\"plugin.list\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, list);

    let unlink = Request {
        id: "plugin_unlink".into(),
        method: Method::PluginUnlink(PluginUnlinkParams {
            plugin_id: "example.worktree-bootstrap".into(),
        }),
    };
    let json = serde_json::to_string(&unlink).unwrap();
    assert!(json.contains("\"method\":\"plugin.unlink\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, unlink);

    let plugin = InstalledPluginInfo {
        plugin_id: "example.worktree-bootstrap".into(),
        name: "Worktree Bootstrap".into(),
        version: "0.1.0".into(),
        min_herdr_version: crate::build_info::BASE_VERSION.into(),
        description: Some("Prepare new worktrees".into()),
        manifest_path: "/plugins/worktree-bootstrap/herdr-plugin.toml".into(),
        plugin_root: "/plugins/worktree-bootstrap".into(),
        enabled: true,
        platforms: None,
        build: vec![PluginManifestBuild {
            platforms: None,
            command: vec!["bun".into(), "install".into()],
        }],
        startup: vec![],
        actions: vec![PluginManifestAction {
            id: "bootstrap".into(),
            title: "Bootstrap worktree".into(),
            description: None,
            contexts: vec![PluginActionContext::Workspace],
            platforms: None,
            command: vec!["bun".into(), "run".into(), "bootstrap.ts".into()],
        }],
        events: vec![PluginManifestEventHook {
            on: "worktree.created".into(),
            platforms: None,
            command: vec!["bun".into(), "run".into(), "bootstrap.ts".into()],
        }],
        panes: vec![PluginManifestPane {
            id: "board".into(),
            title: "Board".into(),
            description: None,
            platforms: None,
            placement: PluginPanePlacement::Overlay,
            width: None,
            height: None,
            command: vec!["bun".into(), "run".into(), "board.ts".into()],
        }],
        link_handlers: vec![PluginManifestLinkHandler {
            id: "github-pr".into(),
            title: "Open GitHub PR".into(),
            pattern: "^https://github.com/[^/]+/[^/]+/(issues|pull)/[0-9]+$".into(),
            action: "bootstrap".into(),
            platforms: None,
        }],
        source: Default::default(),
        warnings: vec![],
    };

    for response in [
        SuccessResponse {
            id: "plugin_link".into(),
            result: ResponseResult::PluginLinked {
                plugin: plugin.clone(),
            },
        },
        SuccessResponse {
            id: "plugin_list".into(),
            result: ResponseResult::PluginList {
                plugins: vec![plugin.clone()],
            },
        },
        SuccessResponse {
            id: "plugin_unlink".into(),
            result: ResponseResult::PluginUnlinked {
                plugin_id: plugin.plugin_id.clone(),
                removed: true,
            },
        },
    ] {
        let json = serde_json::to_string(&response).unwrap();
        let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }
}

#[test]
fn layout_export_apply_round_trip() {
    let root = LayoutNode::Split {
        direction: SplitDirection::Right,
        ratio: 0.6,
        first: Box::new(LayoutNode::Pane {
            pane: LayoutPane {
                label: Some("editor".into()),
                cwd: Some("/repo".into()),
                ..Default::default()
            },
        }),
        second: Box::new(LayoutNode::Pane {
            pane: LayoutPane {
                label: Some("tests".into()),
                command: Some(vec!["sh".into(), "-c".into(), "just test".into()]),
                env: HashMap::from([("HERDR_ROLE".into(), "tests".into())]),
                ..Default::default()
            },
        }),
    };

    let export = Request {
        id: "layout_export".into(),
        method: Method::LayoutExport(LayoutExportParams {
            tab_id: Some("w1:1".into()),
            pane_id: None,
        }),
    };
    let json = serde_json::to_string(&export).unwrap();
    assert!(json.contains("\"method\":\"layout.export\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, export);

    let apply = Request {
        id: "layout_apply".into(),
        method: Method::LayoutApply(LayoutApplyParams {
            workspace_id: Some("w1".into()),
            tab_id: None,
            tab_label: Some("dev".into()),
            focus: true,
            root: root.clone(),
        }),
    };
    let json = serde_json::to_string(&apply).unwrap();
    assert!(json.contains("\"method\":\"layout.apply\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, apply);

    let response = SuccessResponse {
        id: "layout_export".into(),
        result: ResponseResult::LayoutExport {
            layout: LayoutDescription {
                workspace_id: "w1".into(),
                tab_id: "w1:1".into(),
                zoomed: false,
                focused_pane_id: "w1-1".into(),
                root,
            },
        },
    };
    let json = serde_json::to_string(&response).unwrap();
    let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, response);

    let response = SuccessResponse {
        id: "layout_ratio".into(),
        result: ResponseResult::LayoutSplitRatioSet {
            layout: LayoutDescription {
                workspace_id: "w1".into(),
                tab_id: "w1:1".into(),
                zoomed: false,
                focused_pane_id: "w1-1".into(),
                root: LayoutNode::Pane {
                    pane: LayoutPane {
                        pane_id: Some("w1-1".into()),
                        ..Default::default()
                    },
                },
            },
        },
    };
    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"type\":\"layout_split_ratio_set\""));
    let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, response);
}

#[test]
fn layout_schema_round_trips_leading_and_trailing_split_directions() {
    let root = LayoutNode::Split {
        direction: SplitDirection::Left,
        ratio: 0.4,
        first: Box::new(LayoutNode::Split {
            direction: SplitDirection::Up,
            ratio: 0.3,
            first: Box::new(LayoutNode::Pane {
                pane: LayoutPane {
                    label: Some("above".into()),
                    ..Default::default()
                },
            }),
            second: Box::new(LayoutNode::Pane {
                pane: LayoutPane {
                    label: Some("below".into()),
                    ..Default::default()
                },
            }),
        }),
        second: Box::new(LayoutNode::Pane {
            pane: LayoutPane {
                label: Some("right".into()),
                ..Default::default()
            },
        }),
    };

    let json = serde_json::to_value(&root).unwrap();
    assert_eq!(json["direction"], "left");
    assert_eq!(json["first"]["direction"], "up");
    let restored: LayoutNode = serde_json::from_value(json).unwrap();
    assert_eq!(restored, root);
}

#[test]
fn authority_mutation_requests_round_trip() {
    let workspace_move = Request {
        id: "move_ws".into(),
        method: Method::WorkspaceMove(WorkspaceMoveParams {
            workspace_id: "w1".into(),
            insert_index: 2,
        }),
    };
    let json = serde_json::to_value(&workspace_move).unwrap();
    assert_eq!(json["method"], "workspace.move");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, workspace_move);

    let workspace_move_block = Request {
        id: "move_ws_block".into(),
        method: Method::WorkspaceMoveBlock(WorkspaceMoveBlockParams {
            workspace_ids: vec!["w1".into(), "w2".into()],
            before_workspace_id: Some("w3".into()),
        }),
    };
    let json = serde_json::to_value(&workspace_move_block).unwrap();
    assert_eq!(json["method"], "workspace.move_block");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, workspace_move_block);

    let tab_move = Request {
        id: "move_tab".into(),
        method: Method::TabMove(TabMoveParams {
            tab_id: "w1:1".into(),
            insert_index: 1,
        }),
    };
    let json = serde_json::to_value(&tab_move).unwrap();
    assert_eq!(json["method"], "tab.move");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, tab_move);

    let pane_focus = Request {
        id: "focus_pane".into(),
        method: Method::PaneFocus(PaneTarget {
            pane_id: "w1:1".into(),
        }),
    };
    let json = serde_json::to_value(&pane_focus).unwrap();
    assert_eq!(json["method"], "pane.focus");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, pane_focus);

    let split_ratio = Request {
        id: "set_ratio".into(),
        method: Method::LayoutSetSplitRatio(LayoutSetSplitRatioParams {
            tab_id: Some("w1:1".into()),
            pane_id: None,
            path: vec![false, true],
            ratio: 0.6,
        }),
    };
    let json = serde_json::to_value(&split_ratio).unwrap();
    assert_eq!(json["method"], "layout.set_split_ratio");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, split_ratio);

    let subscription = Request {
        id: "sub_moves".into(),
        method: Method::EventsSubscribe(EventsSubscribeParams {
            subscriptions: vec![
                Subscription::WorkspaceMoved {},
                Subscription::WorkspaceReordered {},
                Subscription::TabMoved {},
                Subscription::LayoutUpdated {},
            ],
        }),
    };
    let json = serde_json::to_string(&subscription).unwrap();
    assert!(json.contains("\"type\":\"workspace.moved\""));
    assert!(json.contains("\"type\":\"workspace.reordered\""));
    assert!(json.contains("\"type\":\"tab.moved\""));
    assert!(json.contains("\"type\":\"layout.updated\""));
    let restored: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, subscription);
}

#[test]
fn create_response_round_trips_with_root_pane() {
    let response = SuccessResponse {
        id: "req_2".into(),
        result: ResponseResult::TabCreated {
            tab: TabInfo {
                tab_id: "w_1:2".into(),
                workspace_id: "w_1".into(),
                number: 2,
                label: "review".into(),
                prio: false,
                starred: false,
                focused: false,
                pane_count: 1,
                agent_status: AgentStatus::Unknown,
            },
            root_pane: PaneInfo {
                pane_id: "w_1-3".into(),
                terminal_id: "term_example".into(),
                workspace_id: "w_1".into(),
                tab_id: "w_1:2".into(),
                focused: false,
                settled_at: None,
                snoozed_until: None,
                restore_error: None,
                work_context: Default::default(),
                cwd: Some("/tmp/review".into()),
                foreground_cwd: None,
                label: None,
                agent: None,
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                display_agent: None,
                agent_status: AgentStatus::Unknown,
                waiting_on_agents: false,
                wait: None,
                eta_s: None,
                reported_at: None,
                state_labels: HashMap::new(),
                tokens: HashMap::new(),
                gates: Vec::new(),
                items: Vec::new(),
                decisions: Vec::new(),
                agent_session: None,
                scroll: None,
                revision: 0,
            },
        },
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"type\":\"tab_created\""));
    assert!(json.contains("\"root_pane\""));
    let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, response);
}

#[test]
fn error_response_round_trips() {
    let response = ErrorResponse {
        id: "req_1".into(),
        error: ErrorBody {
            code: "pane_not_found".into(),
            message: "pane p_1 not found".into(),
        },
    };

    let json = serde_json::to_string(&response).unwrap();
    let restored: ErrorResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, response);
}

#[test]
fn event_wait_parses_typed_match() {
    let json = r#"
    {
        "id": "req_9",
        "method": "events.wait",
        "params": {
            "match_event": {
                "event": "pane_agent_status_changed",
                "pane_id": "p_1",
                "agent_status": "done"
            },
            "timeout_ms": 30000
        }
    }
    "#;

    let request: Request = serde_json::from_str(json).unwrap();
    let Method::EventsWait(params) = request.method else {
        panic!("wrong method parsed");
    };
    assert_eq!(
        params.match_event,
        EventMatch::PaneAgentStatusChanged {
            pane_id: "p_1".into(),
            agent_status: AgentStatus::Done,
        }
    );
}

#[test]
fn plugin_action_list_and_invoke_round_trips() {
    let list = Request {
        id: "req_plugin_action_list".into(),
        method: Method::PluginActionList(PluginActionListParams {
            plugin_id: Some("example.issue-flow".into()),
        }),
    };
    let json = serde_json::to_value(&list).unwrap();
    assert_eq!(json["method"], "plugin.action.list");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, list);

    let invoke = Request {
        id: "req_plugin_action_invoke".into(),
        method: Method::PluginActionInvoke(PluginActionInvokeParams {
            plugin_id: Some("example.issue-flow".into()),
            action_id: "assign-issue".into(),
            context: None,
        }),
    };
    let json = serde_json::to_value(&invoke).unwrap();
    assert_eq!(json["method"], "plugin.action.invoke");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, invoke);

    let action_info = PluginActionInfo {
        plugin_id: "example.issue-flow".into(),
        action_id: "assign-issue".into(),
        title: "Assign Issue".into(),
        description: Some("Open the issue assignment UI".into()),
        contexts: vec![PluginActionContext::Workspace, PluginActionContext::Pane],
        command: vec!["assign".into(), "--issue".into()],
        platforms: Some(vec![PluginPlatform::Linux, PluginPlatform::Macos]),
    };
    assert_eq!(
        action_info.qualified_id(),
        "example.issue-flow.assign-issue"
    );
    let json = serde_json::to_string(&action_info).unwrap();
    let restored: PluginActionInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, action_info);
}

#[test]
fn plugin_pane_open_request_round_trips() {
    let request = Request {
        id: "req_plugin_pane".into(),
        method: Method::PluginPaneOpen(PluginPaneOpenParams {
            plugin_id: "example.board".into(),
            entrypoint: "board".into(),
            placement: Some(PluginPanePlacement::Popup),
            width: Some(crate::popup_size::PopupSize::Cells(90)),
            height: Some(crate::popup_size::PopupSize::Percent(80)),
            workspace_id: None,
            target_pane_id: None,
            direction: None,
            cwd: Some("/tmp".into()),
            focus: true,
            env: [("HERDR_ROLE".to_string(), "board".to_string())].into(),
        }),
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["method"], "plugin.pane.open");
    assert_eq!(json["params"]["placement"], "popup");
    assert_eq!(json["params"]["width"], 90);
    assert_eq!(json["params"]["height"], "80%");
    assert_eq!(json["params"]["env"]["HERDR_ROLE"], "board");
    let restored: Request = serde_json::from_value(json).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn popup_close_request_round_trips() {
    let request = Request {
        id: "popup-close".into(),
        method: Method::PopupClose(EmptyParams::default()),
    };

    let json = serde_json::to_value(request).unwrap();

    assert_eq!(json["method"], "popup.close");
    assert_eq!(json["params"], serde_json::json!({}));
}

#[test]
fn closing_block_item_defaults_are_null_on_read_and_write() {
    let item: ClosingBlockItem = serde_json::from_value(serde_json::json!({
        "n": 1,
        "label": "Gate",
        "text": "approve",
        "default": "approve",
        "default_at": "2026-08-09T12:00:00Z"
    }))
    .unwrap();

    assert_eq!(item.default, None);
    assert_eq!(item.default_at, None);
    let encoded = serde_json::to_value(item).unwrap();
    assert_eq!(encoded["default"], serde_json::Value::Null);
    assert_eq!(encoded["default_at"], serde_json::Value::Null);
}

#[test]
fn closing_item_human_input_excludes_only_informational_labels() {
    let item = |label: &str, blocking: Option<bool>| -> ClosingBlockItem {
        let mut value = serde_json::json!({ "n": 1, "label": label, "text": "t" });
        if let Some(blocking) = blocking {
            value["blocking"] = serde_json::Value::Bool(blocking);
        }
        serde_json::from_value(value).unwrap()
    };

    let legacy = item("Answer", None);
    assert!(legacy.blocking);
    assert_eq!(serde_json::to_value(&legacy).unwrap()["blocking"], true);

    // The wire `blocking` flag is ignored: the label alone decides.
    let legacy_false = item("Verify", Some(false));
    assert!(legacy_false.blocking);
    assert_eq!(
        serde_json::to_value(&legacy_false).unwrap()["blocking"],
        true
    );

    for label in ["Gate", "Answer", "Verify", "Approve", "Decide", "Confirm"] {
        let parsed = item(label, None);
        assert!(parsed.requires_human_input(), "{label} must need the human");
        assert!(parsed.blocking, "{label} must serialise as blocking");
    }

    let informational = item(" What to test ", Some(true));
    assert!(!informational.requires_human_input());
    assert!(!informational.blocking);
    assert_eq!(
        serde_json::to_value(informational).unwrap()["blocking"],
        false
    );
}

#[test]
fn report_agent_params_with_foreign_version_arrays_still_parse() {
    let params: PaneReportAgentParams = serde_json::from_value(serde_json::json!({
        "pane_id": "w1:p1",
        "source": "herdr:claude-closing-block",
        "agent": "claude",
        "state": "blocked",
        "seq": 7,
        "v": 1,
        "gates": ["a v1 string gate"],
        "message": "a v1 string gate"
    }))
    .unwrap();

    assert_eq!(params.v, Some(1));
    assert_eq!(params.gates, None);
    assert_eq!(params.items, None);
    assert_eq!(params.decisions, None);
}

#[test]
fn report_agent_params_with_foreign_version_defers_typed_fields() {
    let params: PaneReportAgentParams = serde_json::from_value(serde_json::json!({
        "pane_id": "w1:p1",
        "source": "herdr:claude-closing-block",
        "agent": "claude",
        "state": {"phase": "paused"},
        "v": 3,
        "wait": {"until": "ci"},
        "eta_s": {"seconds": 120},
        "reported_at": {"at": "2026-08-10T10:00:00Z"}
    }))
    .expect("foreign versions must defer incompatible typed fields");

    assert_eq!(params.v, Some(3));
    assert_eq!(params.wait, None);
    assert_eq!(params.eta_s, None);
    assert_eq!(params.reported_at, None);

    let legacy: PaneReportAgentParams = serde_json::from_value(serde_json::json!({
        "pane_id": "w1:p1",
        "source": "herdr:claude-closing-block",
        "agent": "claude",
        "state": "blocked",
        "v": 1,
        "wait": ["legacy"],
        "eta_s": "legacy",
        "reported_at": ["legacy"]
    }))
    .expect("v1 reports must keep skipping version-dependent fields");

    assert_eq!(legacy.wait, None);
    assert_eq!(legacy.eta_s, None);
    assert_eq!(legacy.reported_at, None);
}

#[test]
fn report_agent_params_with_current_version_keep_strict_arrays() {
    let malformed = serde_json::from_value::<PaneReportAgentParams>(serde_json::json!({
        "pane_id": "w1:p1",
        "source": "herdr:claude-closing-block",
        "agent": "claude",
        "state": "blocked",
        "v": 2,
        "gates": ["not an object"]
    }));
    assert!(malformed.is_err());

    let params: PaneReportAgentParams = serde_json::from_value(serde_json::json!({
        "pane_id": "w1:p1",
        "source": "herdr:claude-closing-block",
        "agent": "claude",
        "state": "blocked",
        "v": 2,
        "gates": [{"n": 1, "label": "Gate", "text": "approve"}],
        "items": [],
        "decisions": []
    }))
    .unwrap();
    assert_eq!(params.gates.as_deref().unwrap().len(), 1);
    assert_eq!(params.items.as_deref(), Some(&[][..]));
}

#[test]
fn report_agent_params_without_version_keep_strict_arrays() {
    for field in ["gates", "items", "decisions"] {
        let mut payload = serde_json::json!({
            "pane_id": "w1:p1",
            "source": "herdr:claude-closing-block",
            "agent": "claude",
            "state": "blocked"
        });
        payload[field] = serde_json::json!(["not an object"]);

        assert!(
            serde_json::from_value::<PaneReportAgentParams>(payload).is_err(),
            "missing v must strictly reject malformed {field}"
        );
    }
}
