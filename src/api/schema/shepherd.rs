use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShepherdCallParams {
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
}
