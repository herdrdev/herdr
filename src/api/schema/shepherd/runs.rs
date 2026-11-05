use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};
use crate::cli::shepherd_bindings as bindings;

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdRunListParams => ShepherdRunListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None)
            .input(bindings::root_option()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdRunStartParams => ShepherdRunStartProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub task: String => Field::new(String::new()).input(bindings::text_position("task", "TASK", 0)),
        #[serde(default)]
        pub auto_push: bool => Field::new(false).input(bindings::boolean_flag("auto-push")),
        #[serde(skip)]
        #[schemars(skip)]
        pub wait: bool => Field::new(false).input(bindings::boolean_flag("wait")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mcp_url: Option<String> => Field::new(None)
            .input(bindings::optional_text("mcp-url", "URL")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdGateActionParams => ShepherdGateActionProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub task: String => Field::new(String::new()).input(bindings::text_position("task", "TASK", 0)),
        pub target: String => Field::new(String::new())
            .input(bindings::text_position("target", "TARGET", 1)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdGateRejectParams => ShepherdGateRejectProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub task: String => Field::new(String::new()).input(bindings::text_position("task", "TASK", 0)),
        pub target: String => Field::new(String::new()).input(bindings::text_position("target", "TARGET", 1)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String> => Field::new(None)
            .input(bindings::optional_text("reason", "TEXT")),
    }
}
