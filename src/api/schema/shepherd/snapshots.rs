use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};
use crate::cli::shepherd_bindings as bindings;

fn default_reason() -> String {
    "manual".to_string()
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ShepherdSnapshotCaptureParams => ShepherdSnapshotCaptureProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub agent: String => Field::new(String::new()).input(bindings::text_position("agent", "AGENT", 0)),
        #[serde(default = "default_reason")]
        pub reason: String => Field::new(default_reason())
            .input(bindings::text_value("reason", "REASON")),
    }
}

impl Default for ShepherdSnapshotCaptureParams {
    fn default() -> Self {
        Self {
            root: None,
            agent: String::new(),
            reason: default_reason(),
        }
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdSnapshotListParams => ShepherdSnapshotListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String> => Field::new(None).input(bindings::optional_text("agent", "AGENT")),
        #[serde(default)]
        pub all: bool => Field::new(false).input(bindings::boolean_flag("all")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdSnapshotShowParams => ShepherdSnapshotShowProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub id: String => Field::new(String::new()).input(bindings::text_position("id", "ID", 0)),
        #[serde(default)]
        pub full: bool => Field::new(false).input(bindings::boolean_flag("full")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdSnapshotDiffParams => ShepherdSnapshotDiffProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub from: String => Field::new(String::new()).input(bindings::text_position("from", "FROM", 0)),
        /// Second snapshot; omitted = compare with the current worktree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub to: Option<String> => Field::new(None)
            .input(bindings::optional_text_position("to", "TO", 1)),
        #[serde(default)]
        pub full: bool => Field::new(false).input(bindings::boolean_flag("full")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdSnapshotRestoreParams => ShepherdSnapshotRestoreProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub id: String => Field::new(String::new()).input(bindings::text_position("id", "ID", 0)),
        /// Files or directories to restore; empty = the whole worktree (needs `confirm`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub paths: Vec<String> => Field::new(Vec::new()).input(bindings::string_list("path", "FILE")),
        #[serde(default)]
        pub confirm: bool => Field::new(false).input(bindings::boolean_flag("confirm")),
        /// Full restore only: also delete files that are not in the snapshot.
        #[serde(default)]
        pub delete_extra: bool => Field::new(false).input(bindings::boolean_flag("delete-extra")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdSnapshotDeleteParams => ShepherdSnapshotDeleteProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub id: String => Field::new(String::new()).input(bindings::text_position("id", "ID", 0)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdSnapshotPruneParams => ShepherdSnapshotPruneProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String> => Field::new(None).input(bindings::optional_text("agent", "AGENT")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub keep: Option<usize> => Field::new(None).input(bindings::optional_usize("keep", "N")),
    }
}
