use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;
use crate::cli::projection::{cli_params, Field, Input};
use crate::cli::workspace_bindings as bindings;

pub(crate) fn workspace_option(value_name: &'static str) -> Input<Option<String>> {
    Input::value("workspace", value_name, |slot, value| {
        *slot = Some(crate::cli::normalize_workspace_id(value));
        Ok(())
    })
}

pub(crate) fn tab_id() -> Input<String> {
    Input::positional("tab_id", 0, |slot, value| {
        *slot = crate::cli::normalize_tab_id(value);
        Ok(())
    })
    .value_name("TAB_ID")
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct TabCreateParams => TabCreateProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(workspace_option("WORKSPACE_ID").order(0)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String> => Field::new(None).input(bindings::optional_text("cwd", "PATH").path().order(1)),
        #[serde(default)]
        pub focus: bool => Field::new(false)
            .input(bindings::boolean("focus", true).order(4))
            .input(bindings::boolean("no-focus", false).order(5)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub label: Option<String> => Field::new(None).input(bindings::optional_text("label", "TEXT").order(2)),
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        pub env: HashMap<String, String> => Field::new(HashMap::new()).input(bindings::environment().order(3)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct TabListParams => TabListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(workspace_option("WORKSPACE_ID")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct TabRenameParams => TabRenameProjection {
        pub tab_id: String => Field::new(String::new()).input(tab_id()),
        pub label: String => Field::new(String::new()).input(bindings::label()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabMoveParams {
    pub tab_id: String,
    pub insert_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub agent_status: AgentStatus,
}
