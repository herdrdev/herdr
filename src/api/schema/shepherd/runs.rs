use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdRunListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdRunStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub task: String,
    #[serde(default)]
    pub auto_push: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdGateActionParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub task: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdGateRejectParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub task: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
