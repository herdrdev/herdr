use std::sync::LazyLock;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Operation {
    pub name: &'static str,
    pub changes_ui: bool,
    pub client_shell: bool,
}

macro_rules! define_operations {
    ($( $variant:ident($params:ty) => $name:literal, $changes_ui:literal, $client_shell:literal; )*) => {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
        #[serde(tag = "method", content = "params")]
        // Request enums are short-lived wire values; keeping variants direct preserves
        // the simple serde shape and avoids boxing churn across every caller.
        #[allow(clippy::large_enum_variant)]
        pub enum Method {
            $(#[serde(rename = $name)] $variant($params),)*
        }

        impl Method {
            pub(crate) const OPERATIONS: &'static [operations::Operation] = &[
                $(operations::Operation {
                    name: $name,
                    changes_ui: $changes_ui,
                    client_shell: $client_shell,
                },)*
            ];

            pub(crate) fn operation(&self) -> operations::Operation {
                match self {
                    $(Self::$variant(_) => operations::Operation {
                        name: $name,
                        changes_ui: $changes_ui,
                        client_shell: $client_shell,
                    },)*
                }
            }
        }
    };
}

pub(super) use define_operations;

pub(crate) fn client_shell_method_names() -> &'static [&'static str] {
    static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let mut names: Vec<_> = super::Method::OPERATIONS
            .iter()
            .filter(|operation| operation.client_shell)
            .map(|operation| operation.name)
            .collect();
        names.sort_unstable();
        names
    });
    &NAMES
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::{Method, Request};

    fn sample_value(
        schema: &serde_json::Value,
        definitions: &serde_json::Value,
    ) -> serde_json::Value {
        if let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
            return sample_value(
                &definitions[reference.rsplit('/').next().unwrap()],
                definitions,
            );
        }
        if let Some(value) = schema.get("const") {
            return value.clone();
        }
        if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
            return values[0].clone();
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(branches) = schema.get(key).and_then(serde_json::Value::as_array) {
                return sample_value(&branches[0], definitions);
            }
        }
        match schema.get("type").and_then(serde_json::Value::as_str) {
            Some("object") => {
                let mut object = serde_json::Map::new();
                if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array)
                {
                    for name in required {
                        let name = name.as_str().unwrap();
                        object.insert(
                            name.into(),
                            sample_value(&schema["properties"][name], definitions),
                        );
                    }
                }
                serde_json::Value::Object(object)
            }
            Some("array") => serde_json::json!([]),
            Some("string") => serde_json::json!("sample"),
            Some("integer" | "number") => serde_json::json!(1),
            Some("boolean") => serde_json::json!(false),
            _ => serde_json::Value::Null,
        }
    }

    #[test]
    fn operation_catalog_matches_every_wire_method() {
        let schema = serde_json::to_value(schemars::schema_for!(Request)).unwrap();
        let branches = schema["oneOf"].as_array().unwrap();
        let names: BTreeSet<_> = Method::OPERATIONS
            .iter()
            .map(|operation| operation.name)
            .collect();
        assert_eq!(
            names.len(),
            Method::OPERATIONS.len(),
            "duplicate operation name"
        );
        assert_eq!(branches.len(), names.len());
        for branch in branches {
            let name = branch["properties"]["method"]["const"].as_str().unwrap();
            assert!(names.contains(name), "missing operation {name}");
            let request: Request = serde_json::from_value(serde_json::json!({
                "id": "catalog",
                "method": name,
                "params": sample_value(&branch["properties"]["params"], &schema["$defs"]),
            }))
            .unwrap_or_else(|error| panic!("sample for {name}: {error}"));
            let operation = request.method.operation();
            assert_eq!(operation.name, name);
            assert_eq!(crate::api::api_method_name(&request.method), name);
            assert_eq!(serde_json::to_value(&request).unwrap()["method"], name);
            assert_eq!(
                crate::api::request_changes_ui(&request),
                operation.changes_ui
            );
            assert_eq!(
                crate::server::client_commands::supports_client_shell_method(&request.method),
                operation.client_shell
            );
            assert_eq!(
                crate::server::client_commands::supports_client_shell_method_name(name),
                operation.client_shell
            );
        }
    }

    #[test]
    fn operation_ui_impact_preserves_existing_classification() {
        let expected = [
            "server.reload_config",
            "server.reload_agent_manifests",
            "notification.show",
            "product_announcement.dismiss",
            "release_notes.dismiss",
            "command.invoke",
            "workspace.create",
            "workspace.focus",
            "workspace.rename",
            "workspace.move",
            "workspace.move_block",
            "workspace.report_metadata",
            "workspace.close",
            "worktree.create",
            "worktree.open",
            "worktree.remove",
            "tab.create",
            "tab.focus",
            "tab.rename",
            "tab.move",
            "tab.close",
            "layout.apply",
            "layout.set_split_ratio",
            "agent.rename",
            "agent.view.set",
            "agent.view.clear",
            "agent.focus",
            "agent.start",
            "agent.prompt",
            "agent.send_keys",
            "pane.split",
            "pane.swap",
            "pane.move",
            "pane.zoom",
            "pane.focus_direction",
            "pane.resize",
            "pane.scroll",
            "pane.clear",
            "pane.edit_scrollback",
            "pane.focus",
            "pane.input.set",
            "pane.rename",
            "pane.report_agent",
            "pane.report_agent_session",
            "pane.report_metadata",
            "pane.clear_agent_authority",
            "pane.release_agent",
            "pane.close",
            "popup.close",
            "plugin.unlink",
            "plugin.disable",
            "plugin.action.invoke",
            "plugin.pane.open",
            "plugin.pane.focus",
            "plugin.pane.close",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let actual = Method::OPERATIONS
            .iter()
            .filter(|operation| operation.changes_ui)
            .map(|operation| operation.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
    }
}
