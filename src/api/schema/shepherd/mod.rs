use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, cli_projection, Field};

pub mod agents;
pub mod git;
pub mod patterns;
pub mod runs;
pub mod skills;
pub mod tasks;
pub mod topics;

pub use agents::*;
pub use git::*;
pub use patterns::*;
pub use runs::*;
pub use skills::*;
pub use tasks::*;
pub use topics::*;

/// Common parameter struct for Shepherd methods with no arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdEmptyParams {}

cli_projection! {
    pub(crate) ShepherdEmptyProjection for ShepherdEmptyParams {}
}

cli_params! {
    /// Deprecated generic dispatcher parameter struct.
    /// Retained for backwards compatibility with existing clients and scripts.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdCallParams => ShepherdCallProjection {
        pub op: String => Field::new(String::new()),
        #[serde(default)]
        pub args: serde_json::Value => Field::new(serde_json::Value::Null),
    }
}
