use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdGitStatusParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}
