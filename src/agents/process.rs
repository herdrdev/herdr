//! Closed process-recognition primitives loaded from agent packages.

pub(crate) use super::source::{
    BundledNodeDefinition as BundledNodeLayout, PackageMatch as KnownPackageMatch,
    PackagePath as KnownPackageLayout, ProcessDefinition as ProcessProfile,
};

impl KnownPackageLayout {
    pub(crate) fn components(&self) -> &[String] {
        &self.components
    }

    pub(crate) fn match_kind(&self) -> KnownPackageMatch {
        self.kind
    }
}

impl BundledNodeLayout {
    pub(crate) fn runtime_basename(&self) -> &str {
        &self.runtime_basename
    }

    pub(crate) fn entrypoint_basename(&self) -> &str {
        &self.entrypoint_basename
    }

    pub(crate) fn package_directory(&self) -> &str {
        &self.package_directory
    }

    pub(crate) fn versions_directory(&self) -> &str {
        &self.versions_directory
    }
}

impl ProcessProfile {
    pub(crate) fn known_package_layouts(&self) -> &[KnownPackageLayout] {
        &self.package_paths
    }

    pub(crate) fn bundled_node_layout(&self) -> Option<&BundledNodeLayout> {
        self.bundled_node.as_ref()
    }

    pub(crate) fn uses_secondary_runtime_argv_fallback(&self) -> bool {
        self.secondary_runtime_argv_fallback
    }

    pub(crate) fn matches_versioned_basename(&self, name: &str) -> bool {
        self.versioned_basename_prefix
            .as_ref()
            .is_some_and(|prefix| {
                name.strip_prefix(prefix.as_str())
                    .is_some_and(|suffix| suffix.starts_with(|c: char| c.is_ascii_digit()))
            })
    }
}
