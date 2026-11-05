use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdSkillListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}
