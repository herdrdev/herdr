use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;
use crate::cli::projection::{cli_params, Field};
use crate::cli::workspace_bindings as bindings;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceListParams {
    /// Return only this daemon's own workspaces, without the coordinator's
    /// cached federated workspaces. Federation pollers always set it so a peer
    /// that is itself a coordinator never re-exports its peers' workspaces.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub local_only: bool,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceCreateParams => WorkspaceCreateProjection {
        /// Workspace whose focused pane supplies the `follow` cwd policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source_workspace_id: Option<String> => Field::new(None::<String>),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String> => Field::new(None)
            .input(bindings::optional_text("cwd", "PATH").path().order(0)),
        #[serde(default)]
        pub focus: bool => Field::new(false)
            .input(bindings::boolean("focus", true).order(3))
            .input(bindings::boolean("no-focus", false).order(4)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub label: Option<String> => Field::new(None)
            .input(bindings::optional_text("label", "TEXT").order(1)),
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        pub env: HashMap<String, String> => Field::new(HashMap::new())
            .input(bindings::environment().order(2)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceCloseParams => WorkspaceCloseProjection {
        pub workspace_id: String => Field::new(String::new()).input(bindings::workspace_id()),
        #[serde(default, skip_serializing_if = "super::is_false")]
        pub close_group: bool => Field::new(false)
            .input(bindings::boolean("group", true).hidden()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceRenameParams => WorkspaceRenameProjection {
        pub workspace_id: String => Field::new(String::new())
            .input(bindings::workspace_id().value_name("WORKSPACE_ID")),
        pub label: String => Field::new(String::new()).input(bindings::label()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceMoveParams {
    pub workspace_id: String,
    pub insert_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceMoveBlockParams {
    pub workspace_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_workspace_id: Option<String>,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceReportMetadataParams => WorkspaceMetadataProjection {
        pub workspace_id: String => Field::new(String::new())
            .input(bindings::workspace_id().value_name("WORKSPACE_ID")),
        pub source: String => Field::new(String::new()).input(bindings::source())
            .validate(|source| bindings::validate_source(source)),
        #[schemars(schema_with = "super::common::metadata_token_patch_schema")]
        pub tokens: HashMap<String, Option<String>> => Field::new(HashMap::new())
            .input(bindings::token()).input(bindings::clear_token())
            .validate(bindings::validate_tokens),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64> => Field::new(None).input(bindings::seq()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(min = 1, max = 86_400_000))]
        pub ttl_ms: Option<u64> => Field::new(None).input(bindings::ttl_ms()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub tab_count: usize,
    pub active_tab_id: String,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[schemars(schema_with = "super::common::metadata_token_values_schema")]
    pub tokens: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorkspaceWorktreeInfo>,
    /// Federation: the home-chosen routing alias of the peer that owns this
    /// workspace. `None` for a local workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// Federation: saved-machine profile id of the owning peer, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_profile_id: Option<String>,
    /// Federation: display label of the owning peer. Never used for routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_label: Option<String>,
    /// Federation: reachability of the owning peer as of the last poll. `None`
    /// for a local workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachability: Option<crate::api::federation_store::Reachability>,
    /// Federation: the workspace's last-known aggregate status, preserved when
    /// the peer is not reachable and `agent_status` is surfaced as `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known_status: Option<AgentStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceWorktreeInfo {
    pub repo_key: String,
    pub repo_name: String,
    pub repo_root: String,
    pub checkout_path: String,
    pub is_linked_worktree: bool,
}
