use std::collections::HashSet;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::ProfileId;

const CATALOG_VERSION: u32 = 1;
const SELECTION_VERSION: u32 = 1;
const MAX_CATALOG_BYTES: u64 = 64 * 1024;
const MAX_PROFILES: usize = 64;
const MAX_LABEL_BYTES: usize = 128;
const MAX_TARGET_BYTES: usize = 1024;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedSshEndpoint {
    pub(crate) id: ProfileId,
    pub(crate) label: String,
    pub(crate) target: String,
    pub(crate) session: String,
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) windows_desktop: bool,
}

impl SavedSshEndpoint {
    pub(crate) fn new(
        label: impl Into<String>,
        target: impl Into<String>,
        session: impl Into<String>,
    ) -> Result<Self, String> {
        let profile = Self {
            id: ProfileId::generate(),
            label: label.into(),
            target: target.into(),
            session: session.into(),
            enabled: true,
            windows_desktop: false,
        };
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), String> {
        ProfileId::parse(self.id.to_string())?;
        let label = self.label.trim();
        if label.is_empty() {
            return Err("SSH endpoint label cannot be empty".into());
        }
        if label.len() > MAX_LABEL_BYTES || label.chars().any(char::is_control) {
            return Err(format!(
                "SSH endpoint label must be at most {MAX_LABEL_BYTES} bytes and contain no control characters"
            ));
        }
        if self.target.len() > MAX_TARGET_BYTES || self.target.chars().any(char::is_control) {
            return Err(format!(
                "SSH target must be at most {MAX_TARGET_BYTES} bytes and contain no control characters"
            ));
        }
        crate::remote::validate_remote_target(&self.target).map(|_| ())?;
        let authority = self.target.strip_prefix("ssh://").unwrap_or(&self.target);
        if authority
            .rsplit_once('@')
            .is_some_and(|(userinfo, _)| userinfo.contains(':'))
        {
            return Err("SSH target must not contain a password".into());
        }
        crate::session::validate_name(&self.session)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EndpointCatalog {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_profile: Option<ProfileId>,
    #[serde(default)]
    pub(crate) ssh: Vec<SavedSshEndpoint>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointSelection {
    version: u32,
    selected_profile: Option<ProfileId>,
}

impl Default for EndpointCatalog {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            selected_profile: None,
            ssh: Vec::new(),
        }
    }
}

impl EndpointCatalog {
    pub(crate) fn load() -> Result<Self, String> {
        Self::load_from_paths(&catalog_path(), &selection_path())
    }

    pub(crate) fn load_profiles() -> Result<Vec<SavedSshEndpoint>, String> {
        // Live clients keep their own selection, independent of other attached clients.
        Self::load_from_path(&catalog_path()).map(|catalog| catalog.ssh)
    }

    fn load_from_paths(catalog_path: &Path, selection_path: &Path) -> Result<Self, String> {
        let mut catalog = Self::load_from_path(catalog_path)?;
        match load_selection_from_path(selection_path) {
            Ok(Some(selection)) => {
                let valid = selection.selected_profile.as_ref().is_none_or(|selected| {
                    catalog
                        .ssh
                        .iter()
                        .any(|profile| &profile.id == selected && profile.enabled)
                });
                if valid {
                    catalog.selected_profile = selection.selected_profile;
                } else {
                    tracing::warn!(
                        path = %selection_path.display(),
                        "saved endpoint selection is absent or disabled; using Local"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %selection_path.display(),
                    "saved endpoint selection is unavailable; using Local"
                );
            }
        }
        Ok(catalog)
    }

    pub(crate) fn store_profiles(&self) -> Result<(), String> {
        self.store_to_path(&catalog_path())
    }

    pub(crate) fn store_selection(&self) -> Result<(), String> {
        self.store_selection_to_path(&selection_path())
    }

    fn store_selection_to_path(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let content = serde_json::to_vec_pretty(&EndpointSelection {
            version: SELECTION_VERSION,
            selected_profile: self.selected_profile.clone(),
        })
        .map_err(|error| format!("failed to encode endpoint selection: {error}"))?;
        store_private_json(path, &content, "endpoint selection")
    }

    pub(crate) fn add_ssh(
        &mut self,
        label: impl Into<String>,
        target: impl Into<String>,
        session: impl Into<String>,
    ) -> Result<ProfileId, String> {
        if self.ssh.len() >= MAX_PROFILES {
            return Err(format!("at most {MAX_PROFILES} SSH endpoints can be saved"));
        }
        let profile = SavedSshEndpoint::new(label, target, session)?;
        let id = profile.id.clone();
        self.ssh.push(profile);
        Ok(id)
    }

    pub(crate) fn add_ssh_with_desktop(
        &mut self,
        label: impl Into<String>,
        target: impl Into<String>,
        session: impl Into<String>,
        windows_desktop: bool,
    ) -> Result<ProfileId, String> {
        let id = self.add_ssh(label, target, session)?;
        if let Some(profile) = self.ssh.last_mut() {
            profile.windows_desktop = windows_desktop;
        }
        Ok(id)
    }

    pub(crate) fn rename_ssh(
        &mut self,
        id: &ProfileId,
        label: impl Into<String>,
    ) -> Result<bool, String> {
        let Some(index) = self.ssh.iter().position(|profile| &profile.id == id) else {
            return Ok(false);
        };
        let mut renamed = self.ssh[index].clone();
        renamed.label = label.into();
        renamed.validate()?;
        self.ssh[index] = renamed;
        Ok(true)
    }

    pub(crate) fn remove_ssh(&mut self, id: &ProfileId) -> bool {
        let previous_len = self.ssh.len();
        self.ssh.retain(|profile| &profile.id != id);
        if self.selected_profile.as_ref() == Some(id) {
            self.selected_profile = None;
        }
        self.ssh.len() != previous_len
    }

    pub(crate) fn select_local(&mut self) {
        self.selected_profile = None;
    }

    pub(crate) fn select_endpoint(&mut self, endpoint_id: &super::ClientEndpointId) -> bool {
        match endpoint_id {
            super::ClientEndpointId::Local => {
                self.select_local();
                true
            }
            super::ClientEndpointId::Ssh(profile_id) => self.select_ssh(profile_id),
        }
    }

    pub(crate) fn select_ssh(&mut self, id: &ProfileId) -> bool {
        if !self
            .ssh
            .iter()
            .any(|profile| &profile.id == id && profile.enabled)
        {
            return false;
        }
        self.selected_profile = Some(id.clone());
        true
    }

    pub(crate) fn has_enabled_ssh(&self) -> bool {
        self.ssh.iter().any(|profile| profile.enabled)
    }

    pub(crate) fn contains_enabled_target_session(&self, target: &str, session: &str) -> bool {
        self.ssh.iter().any(|profile| {
            profile.enabled && profile.target == target && profile.session == session
        })
    }

    pub(crate) fn set_enabled(&mut self, id: &ProfileId, enabled: bool) -> bool {
        let Some(profile) = self.ssh.iter_mut().find(|profile| &profile.id == id) else {
            return false;
        };
        profile.enabled = enabled;
        if !enabled && self.selected_profile.as_ref() == Some(id) {
            self.selected_profile = None;
        }
        true
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != CATALOG_VERSION {
            return Err(format!(
                "unsupported endpoint catalog version {}; expected {CATALOG_VERSION}",
                self.version
            ));
        }
        if self.ssh.len() > MAX_PROFILES {
            return Err(format!(
                "endpoint catalog contains more than {MAX_PROFILES} SSH profiles"
            ));
        }
        let mut ids = HashSet::new();
        for profile in &self.ssh {
            profile.validate()?;
            if !ids.insert(profile.id.clone()) {
                return Err(format!("duplicate endpoint profile id {}", profile.id));
            }
        }
        if self.selected_profile.as_ref().is_some_and(|selected| {
            !self
                .ssh
                .iter()
                .any(|profile| &profile.id == selected && profile.enabled)
        }) {
            return Err("selected SSH endpoint is absent or disabled in the catalog".into());
        }
        Ok(())
    }

    fn load_from_path(path: &Path) -> Result<Self, String> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(format!(
                    "failed to open endpoint catalog {}: {error}",
                    path.display()
                ))
            }
        };
        let metadata = file
            .metadata()
            .map_err(|error| format!("failed to inspect endpoint catalog: {error}"))?;
        if metadata.len() > MAX_CATALOG_BYTES {
            return Err("endpoint catalog exceeds the storage limit".into());
        }
        let mut content = String::new();
        file.take(MAX_CATALOG_BYTES + 1)
            .read_to_string(&mut content)
            .map_err(|error| format!("failed to read endpoint catalog: {error}"))?;
        if content.len() as u64 > MAX_CATALOG_BYTES {
            return Err("endpoint catalog exceeds the storage limit".into());
        }
        let catalog: Self = serde_json::from_str(&content)
            .map_err(|error| format!("stored endpoint catalog is invalid: {error}"))?;
        catalog.validate()?;
        Ok(catalog)
    }

    fn store_to_path(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let content = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("failed to encode endpoint catalog: {error}"))?;
        store_private_json(path, &content, "endpoint catalog")
    }
}

fn load_selection_from_path(path: &Path) -> Result<Option<EndpointSelection>, String> {
    let content = match std::fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read endpoint selection: {error}")),
    };
    if content.len() as u64 > MAX_CATALOG_BYTES {
        return Err("endpoint selection exceeds the storage limit".into());
    }
    let selection: EndpointSelection = serde_json::from_slice(&content)
        .map_err(|error| format!("stored endpoint selection is invalid: {error}"))?;
    if selection.version != SELECTION_VERSION {
        return Err(format!(
            "unsupported endpoint selection version {}; expected {SELECTION_VERSION}",
            selection.version
        ));
    }
    Ok(Some(selection))
}

fn store_private_json(path: &Path, content: &[u8], description: &str) -> Result<(), String> {
    if content.len() as u64 > MAX_CATALOG_BYTES {
        return Err(format!("{description} exceeds the storage limit"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid {description} path: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {description} directory: {error}"))?;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "refusing to replace {description} through a non-file path"
            ));
        }
    }

    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let temp_path = parent.join(format!(".endpoints-{}-{sequence}.tmp", std::process::id()));
    let mut temp = crate::platform::create_private_state_file(&temp_path)
        .map_err(|error| format!("failed to create {description}: {error}"))?;
    if let Err(error) = temp.write_all(content).and_then(|()| temp.sync_all()) {
        drop(temp);
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!("failed to write {description}: {error}"));
    }
    drop(temp);
    if let Err(error) = crate::platform::replace_file(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!("failed to activate {description}: {error}"));
    }
    crate::platform::sync_parent_directory(parent)
        .map_err(|error| format!("failed to persist {description} directory: {error}"))
}

pub(crate) fn catalog_path() -> PathBuf {
    crate::config::state_dir()
        .join("client")
        .join("endpoints.json")
}

fn selection_path() -> PathBuf {
    crate::config::state_dir()
        .join("client")
        .join("endpoint-selection.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "herdr-endpoint-catalog-{}-{name}",
                std::process::id()
            ))
            .join("endpoints.json")
    }

    #[test]
    fn catalog_roundtrip_persists_profiles_without_secret_fields() {
        let path = path("roundtrip");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let mut catalog = EndpointCatalog::default();
        let id = catalog
            .add_ssh_with_desktop("Build", "ssh://dev@build.example:2222", "agents", true)
            .unwrap();
        assert!(catalog.select_ssh(&id));
        catalog.store_to_path(&path).unwrap();

        let encoded = std::fs::read_to_string(&path).unwrap();
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("private_key"));
        assert!(!encoded.contains("control_socket"));
        let loaded = EndpointCatalog::load_from_path(&path).unwrap();
        assert_eq!(loaded, catalog);
        assert_eq!(loaded.ssh[0].id, id);
        assert!(loaded.ssh[0].windows_desktop);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn catalogs_from_before_desktop_hosting_default_to_normal_ssh() {
        let catalog: EndpointCatalog = serde_json::from_str(
            r#"{
              "version": 1,
              "ssh": [{
                "id": "0123456789abcdef0123456789abcdef",
                "label": "Build",
                "target": "build",
                "session": "default",
                "enabled": true
              }]
            }"#,
        )
        .unwrap();

        assert!(!catalog.ssh[0].windows_desktop);
        catalog.validate().unwrap();
    }

    #[test]
    fn duplicate_target_and_session_profiles_keep_distinct_opaque_ids() {
        let mut catalog = EndpointCatalog::default();
        let first = catalog.add_ssh("One", "build", "default").unwrap();
        let second = catalog.add_ssh("Two", "build", "default").unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn catalog_rejects_passwords_embedded_in_ssh_targets() {
        let mut catalog = EndpointCatalog::default();
        assert!(catalog
            .add_ssh("Build", "ssh://dev:secret@build.example", "default")
            .unwrap_err()
            .contains("must not contain a password"));
        assert!(catalog
            .add_ssh("Build", "dev:secret@build.example", "default")
            .is_err());
        assert!(catalog
            .add_ssh("Build", "ssh://dev@[::1]:2222", "default")
            .is_ok());
    }

    #[test]
    fn interactive_bootstrap_matches_only_enabled_target_and_session() {
        let mut catalog = EndpointCatalog::default();
        let id = catalog.add_ssh("Build", "build", "agents").unwrap();
        assert!(catalog.contains_enabled_target_session("build", "agents"));
        assert!(!catalog.contains_enabled_target_session("build", "default"));
        assert!(catalog.set_enabled(&id, false));
        assert!(!catalog.contains_enabled_target_session("build", "agents"));
    }

    #[test]
    fn rename_changes_only_the_machine_label() {
        let mut catalog = EndpointCatalog::default();
        let id = catalog.add_ssh("Old", "build", "agents").unwrap();
        let original = catalog.ssh[0].clone();

        assert!(catalog.rename_ssh(&id, "New").unwrap());
        assert_eq!(catalog.ssh[0].label, "New");
        assert_eq!(catalog.ssh[0].id, original.id);
        assert_eq!(catalog.ssh[0].target, original.target);
        assert_eq!(catalog.ssh[0].session, original.session);
        assert!(catalog.rename_ssh(&id, "\n").is_err());
        assert_eq!(catalog.ssh[0].label, "New");
    }

    #[test]
    fn removal_and_disable_return_selection_to_local() {
        let mut catalog = EndpointCatalog::default();
        let first = catalog.add_ssh("One", "one", "default").unwrap();
        assert!(catalog.select_ssh(&first));
        assert!(catalog.set_enabled(&first, false));
        assert_eq!(catalog.selected_profile, None);

        assert!(catalog.set_enabled(&first, true));
        assert!(catalog.select_ssh(&first));
        assert!(catalog.remove_ssh(&first));
        assert_eq!(catalog.selected_profile, None);
    }

    #[test]
    fn catalog_rejects_unknown_fields_instead_of_retaining_possible_secrets() {
        let path = path("unknown-field");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "version": 1,
              "ssh": [{
                "id": "0123456789abcdef0123456789abcdef",
                "label": "Build",
                "target": "build",
                "session": "default",
                "enabled": true,
                "password": "must-not-be-accepted"
              }]
            }"#,
        )
        .unwrap();
        assert!(EndpointCatalog::load_from_path(&path)
            .unwrap_err()
            .contains("unknown field"));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn storing_selection_does_not_rewrite_profile_membership() {
        let catalog_path = path("separate-selection");
        let selection_path = catalog_path.with_file_name("selection.json");
        let _ = std::fs::remove_dir_all(catalog_path.parent().unwrap());
        let mut catalog = EndpointCatalog::default();
        let id = catalog.add_ssh("Build", "build", "agents").unwrap();
        catalog.store_to_path(&catalog_path).unwrap();
        let profiles_before = std::fs::read(&catalog_path).unwrap();

        assert!(catalog.select_ssh(&id));
        catalog.store_selection_to_path(&selection_path).unwrap();

        assert_eq!(std::fs::read(&catalog_path).unwrap(), profiles_before);
        assert_eq!(
            load_selection_from_path(&selection_path)
                .unwrap()
                .unwrap()
                .selected_profile,
            Some(id)
        );
        std::fs::remove_dir_all(catalog_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn malformed_selection_does_not_discard_saved_profiles() {
        let catalog_path = path("malformed-selection");
        let selection_path = catalog_path.with_file_name("selection.json");
        let _ = std::fs::remove_dir_all(catalog_path.parent().unwrap());
        let mut catalog = EndpointCatalog::default();
        let id = catalog.add_ssh("Build", "build", "agents").unwrap();
        catalog.store_to_path(&catalog_path).unwrap();
        std::fs::write(&selection_path, b"not json").unwrap();

        let loaded = EndpointCatalog::load_from_paths(&catalog_path, &selection_path).unwrap();
        assert_eq!(loaded.ssh.len(), 1);
        assert_eq!(loaded.ssh[0].id, id);
        assert_eq!(loaded.selected_profile, None);
        std::fs::remove_dir_all(catalog_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn absent_selected_profile_falls_back_without_discarding_catalog() {
        let catalog_path = path("absent-selection");
        let selection_path = catalog_path.with_file_name("selection.json");
        let _ = std::fs::remove_dir_all(catalog_path.parent().unwrap());
        let mut catalog = EndpointCatalog::default();
        let saved = catalog.add_ssh("Build", "build", "agents").unwrap();
        catalog.store_to_path(&catalog_path).unwrap();
        let missing = ProfileId::parse("fedcba9876543210fedcba9876543210").unwrap();
        store_private_json(
            &selection_path,
            &serde_json::to_vec(&EndpointSelection {
                version: SELECTION_VERSION,
                selected_profile: Some(missing),
            })
            .unwrap(),
            "endpoint selection",
        )
        .unwrap();

        let loaded = EndpointCatalog::load_from_paths(&catalog_path, &selection_path).unwrap();
        assert_eq!(loaded.ssh[0].id, saved);
        assert_eq!(loaded.selected_profile, None);
        std::fs::remove_dir_all(catalog_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn invalid_or_missing_selected_profile_is_rejected() {
        let catalog = EndpointCatalog {
            selected_profile: Some(ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap()),
            ..EndpointCatalog::default()
        };
        assert!(catalog.validate().is_err());
    }
}
