use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FleetHostStateInfo {
    Reachable,
    Unreachable,
    VersionSkew,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FleetAgentInfo {
    pub agent_ref: super::AgentRef,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<u64>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FleetHostInfo {
    pub name: String,
    pub target: String,
    pub local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub state: FleetHostStateInfo,
    pub agent_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub agents: Vec<FleetAgentInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FleetSnapshotInfo {
    pub polled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refreshed_at_unix_ms: Option<u64>,
    pub hosts: Vec<FleetHostInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authority_catalogs: Vec<AuthorityCatalogInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AuthorityCatalogInfo {
    pub connection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_id: Option<crate::groups::AuthorityId>,
    pub state: AuthorityCatalogStateInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::groups::GroupAuthoritySnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityCatalogStateInfo {
    Fresh,
    Stale,
    IdentityConflict,
    Unavailable,
}
