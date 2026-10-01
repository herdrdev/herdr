use crate::api::schema::{ResponseResult, WorkspaceViewClearParams, WorkspaceViewSetParams};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_workspace_view_set(
        &mut self,
        id: String,
        mut params: WorkspaceViewSetParams,
    ) -> String {
        if let Err(message) = crate::app::workspace_view::validate_workspace_view(&mut params) {
            return encode_error(id, "invalid_workspace_view", message);
        }
        if let Some(plugin_id) = params.source.strip_prefix("plugin:") {
            let Some(plugin_id) = super::plugins::normalize_plugin_id(plugin_id) else {
                return encode_error(
                    id,
                    "invalid_workspace_view",
                    "plugin-owned workspace view source has an invalid plugin id",
                );
            };
            let Some(plugin) = self.state.installed_plugins.get(&plugin_id) else {
                return encode_error(id, "plugin_not_found", "plugin not found");
            };
            if !plugin.enabled {
                return encode_error(id, "plugin_disabled", "plugin is disabled");
            }
        }
        let source = params.source.clone();
        let label = params.label.clone();
        self.replace_workspace_view_override(Some(params));
        encode_success(
            id,
            ResponseResult::WorkspaceView {
                active: true,
                source: Some(source),
                label,
            },
        )
    }

    pub(super) fn handle_workspace_view_clear(
        &mut self,
        id: String,
        params: WorkspaceViewClearParams,
    ) -> String {
        let source = match params.source {
            Some(source) => match crate::app::workspace_view::validate_workspace_view_source(&source)
            {
                Ok(source) => Some(source),
                Err(message) => return encode_error(id, "invalid_workspace_view", message),
            },
            None => None,
        };
        if source.as_deref().is_none_or(|source| {
            self.state
                .workspace_view_override
                .as_ref()
                .is_some_and(|active| active.source == source)
        }) {
            self.replace_workspace_view_override(None);
        }
        let active = self.state.workspace_view_override.as_ref();
        encode_success(
            id,
            ResponseResult::WorkspaceView {
                active: active.is_some(),
                source: active.map(|view| view.source.clone()),
                label: active.and_then(|view| view.label.clone()),
            },
        )
    }

    pub(crate) fn clear_workspace_view_for_source(&mut self, source: &str) -> bool {
        if self
            .state
            .workspace_view_override
            .as_ref()
            .is_some_and(|active| active.source == source)
        {
            self.replace_workspace_view_override(None);
            true
        } else {
            false
        }
    }

    fn replace_workspace_view_override(&mut self, view: Option<WorkspaceViewSetParams>) {
        self.state.workspace_view_override = view;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn zen_view(source: &str) -> WorkspaceViewSetParams {
        WorkspaceViewSetParams {
            source: source.to_string(),
            label: Some("zen".to_string()),
            workspace_ids: vec!["w1".into(), "w3".into()],
        }
    }

    #[test]
    fn set_and_source_guarded_clear_replace_transient_view() {
        let mut app = test_app();

        let set = app.handle_workspace_view_set("set".to_string(), zen_view("example.views"));
        let set: crate::api::schema::SuccessResponse = serde_json::from_str(&set).unwrap();
        assert_eq!(
            set.result,
            ResponseResult::WorkspaceView {
                active: true,
                source: Some("example.views".to_string()),
                label: Some("zen".to_string()),
            }
        );
        assert_eq!(
            app.state
                .workspace_view_override
                .as_ref()
                .map(|view| view.workspace_ids.as_slice()),
            Some(["w1".to_string(), "w3".to_string()].as_slice())
        );

        app.handle_workspace_view_clear(
            "wrong-source".to_string(),
            WorkspaceViewClearParams {
                source: Some("other.views".to_string()),
            },
        );
        assert!(app.state.workspace_view_override.is_some());

        app.handle_workspace_view_clear(
            "right-source".to_string(),
            WorkspaceViewClearParams {
                source: Some("example.views".to_string()),
            },
        );
        assert!(app.state.workspace_view_override.is_none());
    }

    #[test]
    fn invalid_view_does_not_replace_active_view() {
        let mut app = test_app();
        app.handle_workspace_view_set("set".to_string(), zen_view("example.views"));

        let mut invalid = zen_view("example.other");
        invalid.workspace_ids = vec!["bad id!".into()];
        let response = app.handle_workspace_view_set("invalid".to_string(), invalid);
        let response: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(response.error.code, "invalid_workspace_view");
        assert_eq!(
            app.state
                .workspace_view_override
                .as_ref()
                .map(|view| view.source.as_str()),
            Some("example.views")
        );
    }
}
