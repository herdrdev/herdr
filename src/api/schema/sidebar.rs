use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneCoverage {
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub placement: Option<Placement>,
    pub dropped_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Placement {
    pub section: String,
    pub group: String,
    pub collapsed: bool,
}
