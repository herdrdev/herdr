use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};
use crate::cli::sidebar_bindings as bindings;

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SidebarWidgetSlot {
    #[default]
    Top,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct SidebarReportWidgetParams => SidebarReportWidgetProjection {
        pub source: String => Field::new(String::new())
            .input(bindings::source())
            .validate(|source| bindings::validate_source(source)),
        #[serde(default)]
        pub slot: SidebarWidgetSlot => Field::new(SidebarWidgetSlot::Top)
            .input(bindings::slot()),
        pub title: String => Field::new(String::new())
            .input(bindings::title())
            .validate(|title| bindings::validate_title(title)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub right: Option<String> => Field::new(None)
            .input(bindings::right()),
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub lines: Vec<String> => Field::new(Vec::new())
            .input(bindings::line()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(min = 1, max = 86_400_000))]
        pub ttl_ms: Option<u64> => Field::new(None)
            .input(bindings::ttl_ms()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct SidebarClearWidgetParams => SidebarClearWidgetProjection {
        pub source: String => Field::new(String::new())
            .input(bindings::source())
            .validate(|source| bindings::validate_source(source)),
        #[serde(default)]
        pub slot: SidebarWidgetSlot => Field::new(SidebarWidgetSlot::Top)
            .input(bindings::slot()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SidebarWidgetInfo {
    pub source: String,
    #[serde(default)]
    pub slot: SidebarWidgetSlot,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<String>,
}
