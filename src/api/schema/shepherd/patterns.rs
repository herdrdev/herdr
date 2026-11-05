use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdPatternListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdPatternGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdPatternSaveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub pattern: shepherd_core::patterns::Pattern,
    #[serde(default)]
    pub global: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdPatternDeleteParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdPatternValidateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<shepherd_core::patterns::Pattern>,
}
