use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, cli_projection, Field};
use crate::cli::shepherd_bindings as bindings;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdAgentListParams {}

cli_projection! {
    pub(crate) ShepherdAgentListProjection for ShepherdAgentListParams {}
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdAgentSpawnParams => ShepherdAgentSpawnProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub role: String => Field::new(String::new()).input(bindings::text_position("role", "ROLE", 0)),
        pub task: String => Field::new(String::new()).input(bindings::rest_position("task", "TASK", 1)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub vendor: Option<String> => Field::new(None).input(bindings::optional_text("vendor", "NAME")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model: Option<String> => Field::new(None).input(bindings::optional_text("model", "NAME")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub task_id: Option<String> => Field::new(None).input(bindings::optional_text("task-id", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub parent_id: Option<String> => Field::new(None).input(bindings::optional_text("parent-id", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub worktree: Option<bool> => Field::new(None).input(bindings::optional_bool_flag("worktree")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mcp_url: Option<String> => Field::new(None).input(bindings::optional_text("mcp-url", "URL")),
        #[serde(default)]
        pub confirm_over_limit: bool => Field::new(false),
        #[serde(default = "crate::api::schema::common::default_true")]
        pub deliver_prompt: bool => Field::new(true),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdAgentTargetParams => ShepherdAgentTargetProjection {
        pub id: String => Field::new(String::new())
            .input(bindings::text_position("id", "ID", 0)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdAgentCleanParams => ShepherdAgentCleanProjection {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub ids: Vec<String> => Field::new(Vec::new()).input(bindings::optional_positional_list("id", "ID", 0)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace: Option<String> => Field::new(None)
            .input(bindings::optional_text("workspace", "NAME")),
    }
}
