use serde::{Deserialize, Serialize};

pub mod agents;
pub mod git;
pub mod runs;
pub mod skills;
pub mod tasks;

pub use agents::*;
pub use git::*;
pub use runs::*;
pub use skills::*;
pub use tasks::*;

/// Common parameter struct for Shepherd methods with no arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdEmptyParams {}

/// Deprecated generic dispatcher parameter struct.
/// Retained for backwards compatibility with existing clients and scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdCallParams {
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
}
