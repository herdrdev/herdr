//! Open agent identities and immutable, session-owned registry snapshots.

mod bundled;
pub(crate) mod files;
pub(crate) mod id;
pub(crate) mod integration;
pub(crate) mod presentation;
pub(crate) mod process;
pub(crate) mod remote;
mod report;
pub(crate) mod session;
pub(crate) mod source;
pub(crate) mod store;
pub(crate) use store::RegistrySnapshot;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use crate::api::schema::IntegrationTarget;
use crate::detect::Agent;
use integration::IntegrationProfile;
use presentation::SoundProfile;
use process::ProcessProfile;
use report::{ReportAuthority, ReportPolicy};
use session::SessionProfile;
use source::{LaunchDefinition as LaunchProfile, Package};

impl LaunchProfile {
    #[cfg(not(windows))]
    pub(crate) fn executable(&self) -> &str {
        &self.unix
    }
    #[cfg(windows)]
    pub(crate) fn executable(&self) -> &str {
        &self.windows
    }
}

#[derive(Debug)]
pub(crate) struct AgentProfile {
    legacy_agent: Agent,
    definition: Package,
    integration: Option<Arc<IntegrationProfile>>,
}

impl AgentProfile {
    pub(crate) fn legacy_agent(&self) -> Agent {
        self.legacy_agent
    }
    pub(crate) fn canonical_id(&self) -> &str {
        &self.definition.identity.id
    }
    #[cfg(test)]
    pub(crate) fn aliases(&self) -> &[String] {
        &self.definition.identity.aliases
    }
    pub(crate) fn sound(&self) -> Option<&SoundProfile> {
        self.definition.identity.sound.as_ref()
    }
    pub(crate) fn process(&self) -> Option<&ProcessProfile> {
        self.definition.process.as_ref()
    }
    pub(crate) fn launch(&self) -> &LaunchProfile {
        &self.definition.identity.launch
    }
    fn report(&self) -> ReportPolicy {
        report::policy(self.legacy_agent)
    }
    pub(crate) fn is_startable(&self) -> bool {
        self.definition.identity.startable
    }
    pub(crate) fn detection(&self) -> Option<&str> {
        self.definition.detection.as_deref()
    }
    pub(crate) fn is_screen_detectable(&self) -> bool {
        self.detection().is_some()
    }
    pub(crate) fn session(&self) -> Option<&SessionProfile> {
        self.definition.resume.as_ref()
    }
    pub(crate) fn integration(&self) -> Option<&Arc<IntegrationProfile>> {
        self.integration.as_ref()
    }
}

#[derive(Debug, Default)]
pub(crate) struct AgentRegistry {
    profiles: Vec<AgentProfile>,
    profile_lookup: HashMap<String, usize>,
    process_lookup: HashMap<String, usize>,
    versioned_processes: Vec<usize>,
    integration_order: Vec<usize>,
}

impl AgentRegistry {
    pub(crate) fn from_packages(packages: Vec<Package>) -> Result<Self, String> {
        let mut profiles = Vec::new();
        for mut definition in packages {
            let agent = Agent::parse(&definition.identity.id)?;
            let definition_assets = std::mem::take(&mut definition.assets);
            let integration = definition.integration.take().and_then(|definition| {
                crate::integration::builtin::binding(agent).map(|(target, adapter)| {
                    Arc::new(IntegrationProfile {
                        target,
                        definition,
                        assets: definition_assets,
                        adapter,
                    })
                })
            });
            profiles.push(AgentProfile {
                legacy_agent: agent,
                definition,
                integration,
            });
        }
        profiles.sort_by(|a, b| {
            let rank = |id| {
                Agent::ALL
                    .iter()
                    .position(|known| *known == id)
                    .unwrap_or(usize::MAX)
            };
            rank(a.legacy_agent)
                .cmp(&rank(b.legacy_agent))
                .then_with(|| a.canonical_id().cmp(b.canonical_id()))
        });
        let mut registry = Self {
            profiles,
            ..Self::default()
        };
        for (index, profile) in registry.profiles.iter().enumerate() {
            for name in std::iter::once(&profile.definition.identity.id)
                .chain(&profile.definition.identity.aliases)
            {
                if registry
                    .profile_lookup
                    .insert(name.clone(), index)
                    .is_some()
                {
                    return Err(format!("duplicate agent alias: {name}"));
                }
            }
            if let Some(process) = profile.process() {
                for name in &process.names {
                    if registry
                        .process_lookup
                        .insert(name.clone(), index)
                        .is_some()
                    {
                        return Err(format!("duplicate process matcher: {name}"));
                    }
                }
                if process.versioned_basename_prefix.is_some() {
                    registry.versioned_processes.push(index);
                }
            }
            if profile.integration.is_some() {
                registry.integration_order.push(index);
            }
        }
        registry.integration_order.sort_by_key(|index| {
            registry.profiles[*index]
                .integration()
                .map(|profile| profile.target() as usize)
        });
        Ok(registry)
    }

    pub(crate) fn known_profiles(&self) -> impl ExactSizeIterator<Item = &AgentProfile> {
        self.profiles.iter()
    }
    pub(crate) fn profile_by_normalized_process_name(&self, name: &str) -> Option<&AgentProfile> {
        self.process_lookup
            .get(name)
            .map(|index| &self.profiles[*index])
            .or_else(|| self.profile_by_versioned_process_name(name))
    }
    pub(crate) fn profile_by_versioned_process_name(&self, name: &str) -> Option<&AgentProfile> {
        self.versioned_processes
            .iter()
            .map(|index| &self.profiles[*index])
            .find(|profile| {
                profile
                    .process()
                    .is_some_and(|process| process.matches_versioned_basename(name))
            })
    }
    pub(crate) fn process_profiles_with_package_layouts(
        &self,
    ) -> impl Iterator<Item = &AgentProfile> {
        self.profiles.iter().filter(|profile| {
            profile
                .process()
                .is_some_and(|process| !process.known_package_layouts().is_empty())
        })
    }
    pub(crate) fn process_profiles_with_bundled_node_layout(
        &self,
    ) -> impl Iterator<Item = &AgentProfile> {
        self.profiles.iter().filter(|profile| {
            profile
                .process()
                .is_some_and(|process| process.bundled_node_layout().is_some())
        })
    }
    pub(crate) fn screen_detectable_profiles(&self) -> impl Iterator<Item = &AgentProfile> {
        self.profiles
            .iter()
            .filter(|profile| profile.is_screen_detectable())
    }
    pub(crate) fn integration_capable_profiles(&self) -> impl Iterator<Item = &AgentProfile> {
        self.integration_order
            .iter()
            .map(|index| &self.profiles[*index])
    }
    pub(crate) fn profile_by_integration_target(
        &self,
        target: IntegrationTarget,
    ) -> Option<&AgentProfile> {
        self.integration_capable_profiles().find(|profile| {
            profile
                .integration()
                .is_some_and(|integration| integration.target() == target)
        })
    }
    pub(crate) fn profile_by_integration_cli_name(&self, name: &str) -> Option<&AgentProfile> {
        self.integration_capable_profiles().find(|profile| {
            profile.integration().is_some_and(|integration| {
                integration.cli_label() == name
                    || integration.cli_aliases().iter().any(|alias| alias == name)
            })
        })
    }
    pub(crate) fn profile_by_agent(&self, agent: Agent) -> Option<&AgentProfile> {
        self.profile_by_id(agent.as_str())
    }
    pub(crate) fn sound_profile_by_config_key(&self, key: &str) -> Option<&SoundProfile> {
        self.profiles
            .iter()
            .filter_map(|profile| profile.sound())
            .find(|sound| sound.config_key() == key)
    }
    #[cfg(test)]
    pub(crate) fn profile_for_agent(&self, agent: Agent) -> &AgentProfile {
        self.profile_by_agent(agent)
            .expect("bundled compatibility profile")
    }
    pub(crate) fn profile_by_id(&self, id: &str) -> Option<&AgentProfile> {
        let profile = self.profile_by_normalized_alias(id)?;
        (profile.canonical_id() == id).then_some(profile)
    }
    pub(crate) fn profile_by_normalized_alias(&self, name: &str) -> Option<&AgentProfile> {
        self.profile_lookup
            .get(name)
            .and_then(|index| self.profiles.get(*index))
    }
    pub(crate) fn profile_for_exact_report_pair(
        &self,
        source: &str,
        id: &str,
    ) -> Option<&AgentProfile> {
        let profile = self.profile_by_id(id)?;
        (profile.report().official_source() == Some(source)).then_some(profile)
    }
    pub(crate) fn session_profile_for_exact_report_pair(
        &self,
        source: &str,
        id: &str,
    ) -> Option<(&AgentProfile, &SessionProfile)> {
        let profile = self.profile_for_exact_report_pair(source, id)?;
        Some((profile, profile.session()?))
    }
    fn report_policy_for_exact_pair(&self, source: &str, id: &str) -> Option<ReportPolicy> {
        report_policy_for_exact_pair(source, id)
    }
    pub(crate) fn has_full_lifecycle_report_authority(&self, source: &str, id: &str) -> bool {
        self.report_policy_for_exact_pair(source, id)
            .is_some_and(|report| report.authority() == ReportAuthority::FullLifecycle)
    }
    pub(crate) fn is_session_identity_only_integration(&self, source: &str, id: &str) -> bool {
        self.report_policy_for_exact_pair(source, id)
            .is_some_and(|report| report.authority() == ReportAuthority::SessionIdentityOnly)
    }
    pub(crate) fn is_reserved_native_state_source(&self, source: &str, id: &str) -> bool {
        self.report_policy_for_exact_pair(source, id)
            .is_some_and(ReportPolicy::reserves_native_state)
    }
    pub(crate) fn session_report_allows_replacement(
        &self,
        source: &str,
        id: &str,
        event: Option<&str>,
    ) -> bool {
        self.report_policy_for_exact_pair(source, id)
            .is_some_and(|report| report.allows_session_replacement(event))
    }
    pub(crate) fn session_replacement_allows_unsequenced_report(
        &self,
        source: &str,
        id: &str,
        event: Option<&str>,
    ) -> bool {
        self.report_policy_for_exact_pair(source, id)
            .is_some_and(|report| report.allows_unsequenced_session_replacement(event))
    }
    pub(crate) fn initial_lifecycle_report_replaces_session(&self, source: &str, id: &str) -> bool {
        self.report_policy_for_exact_pair(source, id)
            .is_some_and(ReportPolicy::initial_lifecycle_report_replaces_session)
    }
}

fn load_packages(files: &[(&str, &str)]) -> Result<Vec<Package>, String> {
    let packages = source::load_packages(files)?;
    for package in &packages {
        crate::integration::builtin::validate_package(package)?;
    }
    Ok(packages)
}

pub(crate) fn validate_packages(files: &[(&str, &str)]) -> Result<Vec<Package>, String> {
    let packages = load_packages(files)?;
    for package in &packages {
        if let Some(detection) = &package.detection {
            crate::detect::manifest::validate_package_manifest(
                detection,
                &package.identity.id,
                &package.identity.aliases,
            )?;
        }
    }
    Ok(packages)
}

pub(crate) fn registry() -> Arc<RegistrySnapshot> {
    store::snapshot()
}

// A fixed baseline is used only to migrate old unpinned saved sessions. It
// never registers identities absent from the active runtime snapshot.
static BUNDLED_REGISTRY: LazyLock<AgentRegistry> = LazyLock::new(|| {
    load_packages(bundled::FILES)
        .and_then(AgentRegistry::from_packages)
        .unwrap_or_else(|error| {
            tracing::error!(%error, "invalid bundled resume baseline");
            AgentRegistry::default()
        })
});

pub(crate) fn bundled_profile(id: &str) -> Option<&'static AgentProfile> {
    BUNDLED_REGISTRY.profile_by_id(id)
}

pub(crate) fn bundled_report_pair(source: &str, id: &str) -> bool {
    report_policy_for_exact_pair(source, id).is_some()
}

pub(crate) fn report_policy_for_exact_pair(source: &str, id: &str) -> Option<ReportPolicy> {
    // Package availability cannot revoke core reporter reservations or policy.
    let policy = report::policy(Agent::parse(id).ok()?);
    (policy.official_source() == Some(source)).then_some(policy)
}

#[cfg(test)]
mod tests;
