//! Optional JSON methods carried by the opaque endpoint lane, outside frozen codecs.

use serde::{Deserialize, Serialize};

use super::schema::{EmptyParams, IntegrationInstallResult, IntegrationState, IntegrationTarget};
use crate::integration::omo::{OmoState, OmoStatus};

pub(crate) const METHOD_NAMES: &[&str] = &["integration.install.v2", "integration.list.v2"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct Request {
    pub id: String,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params")]
pub(crate) enum Method {
    #[serde(rename = "integration.list.v2")]
    List(EmptyParams),
    #[serde(rename = "integration.install.v2")]
    Install(InstallParams),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct InstallParams {
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct IntegrationInfo {
    pub target: String,
    pub label: String,
    pub command: String,
    pub available: bool,
    pub state: IntegrationState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub(crate) enum ResponseResult {
    #[serde(rename = "integration_list_v2")]
    List { integrations: Vec<IntegrationInfo> },
    #[serde(rename = "integration_install_v2")]
    Install {
        target: String,
        details: IntegrationInstallResult,
    },
}

fn list_rows(
    recommendations: Vec<crate::integration::IntegrationRecommendation>,
    omo: OmoStatus,
) -> Vec<IntegrationInfo> {
    let mut integrations: Vec<_> = recommendations
        .into_iter()
        .map(|recommendation| IntegrationInfo {
            target: crate::integration::integration_target_label(recommendation.target).into(),
            label: recommendation.label.into(),
            command: recommendation.command.into(),
            available: recommendation.available,
            state: match recommendation.state {
                crate::integration::IntegrationStatusKind::NotInstalled => {
                    IntegrationState::NotInstalled
                }
                crate::integration::IntegrationStatusKind::Current => IntegrationState::Current,
                crate::integration::IntegrationStatusKind::Outdated => IntegrationState::Outdated,
            },
        })
        .collect();
    let (available, state) = match omo.state {
        OmoState::NotInstalled => (omo.available, IntegrationState::NotInstalled),
        OmoState::Current => (omo.available, IntegrationState::Current),
        OmoState::Outdated => (omo.available, IntegrationState::Outdated),
        // V2 cannot represent ownership conflicts. Disable installation rather than
        // advertising a destructive repair for a foreign or modified file.
        OmoState::Modified | OmoState::Foreign => (false, IntegrationState::NotInstalled),
    };
    integrations.push(IntegrationInfo {
        target: "omo".into(),
        label: "Omo".into(),
        command: "omo".into(),
        available,
        state,
    });
    integrations
}

fn legacy_target(target: &str) -> Option<IntegrationTarget> {
    IntegrationTarget::ALL
        .into_iter()
        .find(|candidate| crate::integration::integration_target_label(*candidate) == target)
        .or_else(|| serde_json::from_value(serde_json::Value::String(target.to_owned())).ok())
}

pub(crate) fn dispatch(request: Request) -> String {
    let result = match request.method {
        Method::List(_) => match crate::integration::omo::status() {
            Ok(omo) => ResponseResult::List {
                integrations: list_rows(crate::integration::integration_recommendations(), omo),
            },
            Err(error) => {
                return crate::server::client_commands::error_response(
                    request.id,
                    "integration_list_failed",
                    error.to_string(),
                );
            }
        },
        Method::Install(params) => {
            let installed = if params.target == "omo" {
                crate::integration::omo::install()
            } else if let Some(target) = legacy_target(&params.target) {
                crate::integration::install_target(target)
            } else {
                return crate::server::client_commands::error_response(
                    request.id,
                    "invalid_target",
                    format!("unknown integration target: {}", params.target),
                );
            };
            match installed {
                Ok(messages) => ResponseResult::Install {
                    target: params.target,
                    details: IntegrationInstallResult { messages },
                },
                Err(error) => {
                    return crate::server::client_commands::error_response(
                        request.id,
                        "integration_install_failed",
                        error.to_string(),
                    );
                }
            }
        }
    };
    serde_json::to_string(&serde_json::json!({ "id": request.id, "result": result }))
        .unwrap_or_else(|error| {
            crate::server::client_commands::error_response(
                request.id,
                "serialization_error",
                error.to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_v2_request_parsing_preserves_string_targets() {
        for target in ["omo", "codex", "antigravity-cli", "future-agent"] {
            let value = serde_json::json!({
                "id": "install",
                "method": "integration.install.v2",
                "params": { "target": target }
            });
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert!(matches!(&request.method, Method::Install(params) if params.target == target));
            assert_eq!(serde_json::to_value(request).unwrap(), value);
        }
        let request: Request =
            serde_json::from_str(r#"{"id":"list","method":"integration.list.v2","params":{}}"#)
                .unwrap();
        assert!(matches!(request.method, Method::List(_)));
    }

    #[test]
    fn integration_v2_install_rejects_missing_or_nonstring_target() {
        for params in [serde_json::json!({}), serde_json::json!({"target": 4})] {
            assert!(serde_json::from_value::<Request>(serde_json::json!({
                "id": "invalid",
                "method": "integration.install.v2",
                "params": params
            }))
            .is_err());
        }
    }

    #[test]
    fn integration_v2_install_rejects_unknown_target_without_side_effects() {
        let response = dispatch(Request {
            id: "unknown".into(),
            method: Method::Install(InstallParams {
                target: "unknown-agent".into(),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], "unknown");
        assert_eq!(response["error"]["code"], "invalid_target");
        assert!(response.get("result").is_none());
    }

    #[test]
    fn integration_v2_dispatch_supports_every_legacy_target() {
        for target in IntegrationTarget::ALL {
            assert_eq!(
                legacy_target(crate::integration::integration_target_label(target)),
                Some(target)
            );
            let json_target = serde_json::to_value(target).unwrap();
            assert_eq!(legacy_target(json_target.as_str().unwrap()), Some(target));
        }
        assert_eq!(legacy_target("omo"), None);
        assert_eq!(legacy_target("unknown-agent"), None);
    }

    #[test]
    fn integration_v2_list_preserves_legacy_recommendations_and_adds_omo() {
        let recommendations: Vec<_> = IntegrationTarget::ALL
            .into_iter()
            .enumerate()
            .map(
                |(index, target)| crate::integration::IntegrationRecommendation {
                    target,
                    label: crate::integration::integration_target_label(target),
                    command: "agent",
                    available: index % 2 == 0,
                    path: std::path::PathBuf::from("unused"),
                    state: match index % 3 {
                        0 => crate::integration::IntegrationStatusKind::NotInstalled,
                        1 => crate::integration::IntegrationStatusKind::Current,
                        _ => crate::integration::IntegrationStatusKind::Outdated,
                    },
                },
            )
            .collect();
        let rows = list_rows(
            recommendations.clone(),
            OmoStatus {
                path: "unused".into(),
                available: true,
                state: OmoState::Outdated,
            },
        );
        assert_eq!(rows.len(), recommendations.len() + 1);
        for (row, recommendation) in rows.iter().zip(&recommendations) {
            assert_eq!(
                row.target,
                crate::integration::integration_target_label(recommendation.target)
            );
            assert_eq!(row.label, recommendation.label);
            assert_eq!(row.command, recommendation.command);
            assert_eq!(row.available, recommendation.available);
            let expected = match recommendation.state {
                crate::integration::IntegrationStatusKind::NotInstalled => {
                    IntegrationState::NotInstalled
                }
                crate::integration::IntegrationStatusKind::Current => IntegrationState::Current,
                crate::integration::IntegrationStatusKind::Outdated => IntegrationState::Outdated,
            };
            assert_eq!(row.state, expected);
        }
        let result = serde_json::to_value(ResponseResult::List { integrations: rows }).unwrap();
        assert_eq!(result["type"], "integration_list_v2");
        assert_eq!(
            result["integrations"].as_array().unwrap().last().unwrap(),
            &serde_json::json!({
                "target": "omo", "label": "Omo", "command": "omo",
                "available": true, "state": "outdated"
            })
        );
    }

    #[test]
    fn integration_v2_omo_states_disable_modified_and_foreign_targets() {
        for (state, expected, available) in [
            (OmoState::NotInstalled, IntegrationState::NotInstalled, true),
            (OmoState::Current, IntegrationState::Current, true),
            (OmoState::Outdated, IntegrationState::Outdated, true),
            (OmoState::Modified, IntegrationState::NotInstalled, false),
            (OmoState::Foreign, IntegrationState::NotInstalled, false),
        ] {
            let rows = list_rows(
                Vec::new(),
                OmoStatus {
                    path: "unused".into(),
                    available: true,
                    state,
                },
            );
            assert_eq!(rows[0].state, expected);
            assert_eq!(rows[0].available, available);
        }
        let rows = list_rows(
            Vec::new(),
            OmoStatus {
                path: "unused".into(),
                available: false,
                state: OmoState::NotInstalled,
            },
        );
        assert!(!rows[0].available);
    }

    #[test]
    fn integration_v2_install_response_has_target_and_details_messages() {
        let response = ResponseResult::Install {
            target: "omo".into(),
            details: IntegrationInstallResult {
                messages: vec!["installed".into()],
            },
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "type": "integration_install_v2",
                "target": "omo",
                "details": { "messages": ["installed"] }
            })
        );
    }

    #[test]
    fn integration_v1_json_and_bincode_targets_remain_frozen() {
        let names = [
            "pi",
            "omp",
            "claude",
            "codex",
            "copilot",
            "devin",
            "droid",
            "kimi",
            "opencode",
            "kilo",
            "hermes",
            "qodercli",
            "qwen",
            "cursor",
            "mastracode",
            "antigravity_cli",
            "grok",
        ];
        assert_eq!(IntegrationTarget::ALL.len(), names.len());
        for (index, (target, name)) in IntegrationTarget::ALL.into_iter().zip(names).enumerate() {
            assert_eq!(serde_json::to_value(target).unwrap(), name);
            assert_eq!(
                bincode::serde::encode_to_vec(target, bincode::config::standard()).unwrap(),
                vec![u8::try_from(index).unwrap()]
            );
        }
        assert!(serde_json::from_value::<IntegrationTarget>(serde_json::json!("omo")).is_err());
        for method in METHOD_NAMES {
            assert!(
                serde_json::from_value::<super::super::schema::Request>(serde_json::json!({
                    "id": "v1", "method": method, "params": { "target": "omo" }
                }))
                .is_err()
            );
        }
        let response = crate::api::schema::SuccessResponse {
            id: "legacy".into(),
            result: crate::api::schema::ResponseResult::IntegrationInstall {
                target: IntegrationTarget::Codex,
                details: IntegrationInstallResult {
                    messages: vec!["installed".into()],
                },
            },
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "id": "legacy", "result": {
                    "type": "integration_install", "target": "codex",
                    "details": { "messages": ["installed"] }
                }
            })
        );
    }
}
