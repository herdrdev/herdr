//! Offline status adapters for the legacy manifest API.
//! Historical website cache/status files are neither read nor deleted;
//! automatic and manual registry updates are server-owned.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{agent_label, Agent};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct ManifestUpdateStatus {
    pub(crate) last_check_unix: Option<u64>,
    pub(crate) last_result: Option<String>,
    #[serde(default)]
    pub(crate) agents: BTreeMap<String, AgentRemoteStatus>,
}

impl ManifestUpdateStatus {
    pub(crate) fn agent_status(&self, agent: Agent) -> Option<AgentRemoteStatus> {
        self.agents.get(agent_label(&agent)).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentRemoteStatus {
    pub(crate) cached_version: Option<String>,
    pub(crate) attempted_version: Option<String>,
    pub(crate) last_checked_unix: Option<u64>,
    pub(crate) last_result: String,
    pub(crate) last_error: Option<String>,
}

/// Derive the legacy status shape from the accepted immutable registry only.
/// Must not be called while constructing a registry/cache.
pub(crate) fn load_status() -> ManifestUpdateStatus {
    let agents = super::manifest::manifest_summaries()
        .into_iter()
        .filter_map(|summary| {
            status_for_version(summary.cached_remote_version)
                .map(|status| (agent_label(&summary.agent).to_string(), status))
        })
        .collect();
    ManifestUpdateStatus {
        last_check_unix: None,
        last_result: Some("active registry snapshot".into()),
        agents,
    }
}

pub(crate) fn status_for_version(version: Option<String>) -> Option<AgentRemoteStatus> {
    version.map(|version| AgentRemoteStatus {
        cached_version: Some(version),
        attempted_version: None,
        last_checked_unix: None,
        last_result: "accepted_registry".into(),
        last_error: None,
    })
}
