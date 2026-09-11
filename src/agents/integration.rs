//! Loaded integration metadata bound to a trusted, compiled installer.

use std::io;
use std::path::{Path, PathBuf};

use super::source::IntegrationDefinition;
use crate::api::schema::IntegrationTarget;

#[derive(Debug)]
pub(crate) struct AgentVersionRequirement {
    pub label: &'static str,
    pub binary: &'static str,
    pub args: &'static [&'static str],
    pub min_version: &'static str,
}

type Action = fn() -> io::Result<Vec<String>>;
type PathResolver = fn() -> io::Result<PathBuf>;
type AvailabilityProbe = fn() -> bool;
type ExtraValidator = fn(&Path, u32) -> bool;

#[derive(Debug, Clone, Copy)]
pub(crate) struct IntegrationAdapter {
    install_action: Action,
    uninstall_action: Action,
    primary_path_resolver: PathResolver,
    install_layout_probe: Option<AvailabilityProbe>,
    current_install_extra_validator: Option<ExtraValidator>,
    version_requirement: Option<&'static AgentVersionRequirement>,
}

impl IntegrationAdapter {
    pub(crate) const fn new(
        install_action: Action,
        uninstall_action: Action,
        primary_path_resolver: PathResolver,
    ) -> Self {
        Self {
            install_action,
            uninstall_action,
            primary_path_resolver,
            install_layout_probe: None,
            current_install_extra_validator: None,
            version_requirement: None,
        }
    }

    pub(crate) const fn with_install_layout_probe(mut self, probe: AvailabilityProbe) -> Self {
        self.install_layout_probe = Some(probe);
        self
    }

    pub(crate) const fn with_current_install_extra_validator(
        mut self,
        validator: ExtraValidator,
    ) -> Self {
        self.current_install_extra_validator = Some(validator);
        self
    }

    pub(crate) const fn with_version_requirement(
        mut self,
        requirement: &'static AgentVersionRequirement,
    ) -> Self {
        self.version_requirement = Some(requirement);
        self
    }

    pub(crate) fn install(self) -> io::Result<Vec<String>> {
        (self.install_action)()
    }

    pub(crate) fn uninstall(self) -> io::Result<Vec<String>> {
        (self.uninstall_action)()
    }

    pub(crate) fn primary_installed_artifact_path(self) -> io::Result<PathBuf> {
        (self.primary_path_resolver)()
    }

    pub(crate) fn install_layout_available(self) -> bool {
        self.install_layout_probe.is_some_and(|probe| probe())
    }

    pub(crate) fn current_install_extra_is_valid(self, path: &Path, expected_version: u32) -> bool {
        self.current_install_extra_validator
            .is_none_or(|validator| validator(path, expected_version))
    }

    pub(crate) fn agent_version_requirement(self) -> Option<&'static AgentVersionRequirement> {
        self.version_requirement
    }
}

#[derive(Debug)]
pub(crate) struct IntegrationProfile {
    pub(super) target: IntegrationTarget,
    pub(super) definition: IntegrationDefinition,
    pub(super) adapter: IntegrationAdapter,
}

impl IntegrationProfile {
    pub(crate) fn target(&self) -> IntegrationTarget {
        self.target
    }
    pub(crate) fn cli_label(&self) -> &str {
        &self.definition.cli_name
    }
    pub(crate) fn cli_aliases(&self) -> &[String] {
        &self.definition.aliases
    }

    #[cfg(not(windows))]
    pub(crate) fn command_names(&self) -> &[String] {
        &self.definition.commands.unix
    }
    #[cfg(windows)]
    pub(crate) fn command_names(&self) -> &[String] {
        &self.definition.commands.windows
    }

    #[cfg(not(windows))]
    pub(crate) fn supported(&self) -> bool {
        self.definition.supported.unix
    }
    #[cfg(windows)]
    pub(crate) fn supported(&self) -> bool {
        self.definition.supported.windows
    }

    #[cfg(not(windows))]
    pub(crate) fn expected_version(&self) -> u32 {
        self.definition.versions.unix
    }
    #[cfg(windows)]
    pub(crate) fn expected_version(&self) -> u32 {
        self.definition.versions.windows
    }

    pub(crate) fn adapter(&self) -> IntegrationAdapter {
        self.adapter
    }
}
