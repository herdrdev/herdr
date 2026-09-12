use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::api::schema::InstalledPluginInfo;

pub const MANIFEST_UNAVAILABLE_WARNING_PREFIX: &str = "manifest unavailable: ";
const REGISTRY_LOCK_FILE: &str = ".plugins.lock";

fn registry_path() -> PathBuf {
    crate::config::config_dir().join("plugins.json")
}

fn registry_lock_path() -> PathBuf {
    crate::config::config_dir().join(REGISTRY_LOCK_FILE)
}

fn with_registry_lock<T>(operation: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    let lock_path = registry_lock_path();
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock()?;
    operation()
}

// Resolve the file link before replacing it, including a missing target in a
// stow-managed directory. Keep this separate from session persistence, which
// has its own write/error behavior.
fn resolve_write_target(path: &Path) -> std::io::Result<PathBuf> {
    let mut target = path.to_path_buf();
    for _ in 0..40 {
        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(target),
            Err(err) => return Err(err),
        };
        if !metadata.file_type().is_symlink() {
            return Ok(target);
        }
        let link = std::fs::read_link(&target)?;
        target = if link.is_absolute() {
            link
        } else {
            target.parent().unwrap_or_else(|| Path::new(".")).join(link)
        };
    }
    Err(std::io::Error::other("too many plugin registry symlinks"))
}

fn save_json_to_path<T: serde::Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    let target = resolve_write_target(path)?;
    let path = target.as_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json)?;
    #[cfg(windows)]
    if path.exists() {
        if let Err(err) = std::fs::remove_file(path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

pub fn save_to_path(path: &Path, plugins: &[InstalledPluginInfo]) -> std::io::Result<()> {
    save_json_to_path(path, plugins)
}

pub fn update<T>(
    mutation: impl FnOnce(&mut Vec<InstalledPluginInfo>) -> T,
) -> std::io::Result<(T, Vec<InstalledPluginInfo>)> {
    with_registry_lock(|| {
        let mut plugins = load_from_path_strict(&registry_path())?;
        let result = mutation(&mut plugins);
        plugins.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        save_to_path(&registry_path(), &plugins)?;
        Ok((result, plugins))
    })
}

pub fn try_load() -> std::io::Result<Vec<InstalledPluginInfo>> {
    with_registry_lock(|| load_from_path_strict(&registry_path()))
}

/// Load the global registry. Returns an empty vec on failure so a corrupt or
/// missing file never blocks server startup; mutations still use strict reads.
pub fn load() -> Vec<InstalledPluginInfo> {
    match try_load() {
        Ok(plugins) => plugins,
        Err(err) => {
            warn!(path = %registry_path().display(), err = %err, "failed to load plugin registry");
            Vec::new()
        }
    }
}

#[cfg(test)]
pub fn load_from_path(path: &Path) -> Vec<InstalledPluginInfo> {
    match load_from_path_strict(path) {
        Ok(entries) => entries,
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to read plugin registry");
            Vec::new()
        }
    }
}

fn load_from_path_strict(path: &Path) -> std::io::Result<Vec<InstalledPluginInfo>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)?;
    serde_json::from_str::<Vec<InstalledPluginInfo>>(&content)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

/// Re-read each entry's manifest from disk using the provided reload function.
///
/// If the manifest parses successfully, replace cached fields but keep the
/// stored `enabled` flag.  If the file is gone or unparseable, keep the stored
/// entry and append a warning so `plugin.list` surfaces it.
pub fn reload_manifests(
    mut entries: Vec<InstalledPluginInfo>,
    reload_fn: impl Fn(&str, bool) -> Result<InstalledPluginInfo, String>,
) -> Vec<InstalledPluginInfo> {
    for entry in &mut entries {
        entry.warnings.clear();
        match reload_fn(&entry.manifest_path, entry.enabled) {
            Ok(mut fresh) => {
                fresh.enabled = entry.enabled;
                fresh.source = entry.source.clone();
                *entry = fresh;
            }
            Err(warn_msg) => {
                entry
                    .warnings
                    .push(format!("{MANIFEST_UNAVAILABLE_WARNING_PREFIX}{warn_msg}"));
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir()
            .join(format!(
                "herdr-registry-{name}-{}-{nanos}",
                std::process::id()
            ))
            .join("plugins.json")
    }

    fn sample_plugin(id: &str) -> InstalledPluginInfo {
        InstalledPluginInfo {
            plugin_id: id.to_string(),
            name: "Test Plugin".to_string(),
            version: "0.1.0".to_string(),
            min_herdr_version: crate::build_info::BASE_VERSION.to_string(),
            description: None,
            manifest_path: format!("/tmp/{id}/herdr-plugin.toml"),
            plugin_root: format!("/tmp/{id}"),
            enabled: true,
            platforms: None,
            build: vec![],
            startup: vec![],
            actions: vec![],
            events: vec![],
            panes: vec![],
            link_handlers: vec![],
            source: Default::default(),
            warnings: vec![],
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let path = temp_registry_path("roundtrip");
        let plugins = vec![sample_plugin("example.a"), sample_plugin("example.b")];

        save_to_path(&path, &plugins).unwrap();

        let loaded = load_from_path(&path);
        assert_eq!(loaded.len(), 2);
        let ids: Vec<_> = loaded.iter().map(|p| p.plugin_id.as_str()).collect();
        assert!(ids.contains(&"example.a"));
        assert!(ids.contains(&"example.b"));
    }

    #[test]
    fn missing_file_returns_empty() {
        let path = temp_registry_path("missing");
        let loaded = load_from_path(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn corrupt_file_returns_empty_without_panic() {
        let path = temp_registry_path("corrupt");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let corrupt = b"this is not valid json {{{{";
        std::fs::write(&path, corrupt).unwrap();

        assert!(load_from_path_strict(&path).is_err());
        assert!(load_from_path(&path).is_empty());
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
    }

    #[test]
    fn reload_manifests_keeps_entry_with_warning_on_missing_manifest() {
        let entry = sample_plugin("example.missing");
        let entries = vec![entry];

        let result = reload_manifests(entries, |path, _enabled| {
            Err(format!("manifest not found at {path}"))
        });

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].plugin_id, "example.missing");
        assert!(!result[0].warnings.is_empty());
        assert!(result[0].warnings[0].contains("manifest not found"));
    }

    #[test]
    fn reload_manifests_uses_fresh_parse_and_keeps_enabled_flag() {
        let mut entry = sample_plugin("example.reload");
        entry.enabled = false;
        entry.source = crate::api::schema::PluginSourceInfo {
            kind: crate::api::schema::PluginSourceKind::Github,
            owner: Some("ogulcancelik".into()),
            repo: Some("herdr-plugin-examples".into()),
            subdir: Some("worktree-bootstrap".into()),
            requested_ref: Some("main".into()),
            resolved_commit: Some("abc123".into()),
            managed_path: Some("/tmp/herdr/plugins/github/example.reload".into()),
            installed_unix_ms: Some(42),
        };

        let result = reload_manifests(vec![entry], |_path, _enabled| {
            Ok(InstalledPluginInfo {
                plugin_id: "example.reload".to_string(),
                name: "Fresh Name".to_string(),
                version: "0.2.0".to_string(),
                min_herdr_version: crate::build_info::BASE_VERSION.to_string(),
                description: Some("refreshed".to_string()),
                manifest_path: "/tmp/example.reload/herdr-plugin.toml".to_string(),
                plugin_root: "/tmp/example.reload".to_string(),
                enabled: true, // caller would pass stored enabled; fresh parse returns true
                platforms: None,
                build: vec![],
                startup: vec![],
                actions: vec![],
                events: vec![],
                panes: vec![],
                link_handlers: vec![],
                source: Default::default(),
                warnings: vec![],
            })
        });

        assert_eq!(result[0].name, "Fresh Name");
        assert_eq!(result[0].version, "0.2.0");
        // enabled preserved from stored entry
        assert!(!result[0].enabled);
        assert_eq!(
            result[0].source.kind,
            crate::api::schema::PluginSourceKind::Github
        );
        assert_eq!(result[0].source.owner.as_deref(), Some("ogulcancelik"));
        assert!(result[0].warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_relative_registry_symlink() {
        let path = temp_registry_path("relative-symlink");
        let root = path.parent().unwrap();
        let target = root.join("dotfiles/herdr/plugins.json");
        save_to_path(&target, &[sample_plugin("example.first")]).unwrap();
        std::os::unix::fs::symlink("dotfiles/herdr/plugins.json", &path).unwrap();

        let result = save_to_path(&path, &[sample_plugin("example.second")]);
        let preserved = std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink();
        let link_text = std::fs::read_link(&path);
        let loaded = load_from_path(&target);
        let same_content = std::fs::read(&path).unwrap() == std::fs::read(&target).unwrap();
        std::fs::remove_dir_all(root).unwrap();

        result.unwrap();
        assert!(preserved, "saving the registry must preserve its symlink");
        assert_eq!(link_text.unwrap(), Path::new("dotfiles/herdr/plugins.json"));
        assert!(same_content);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].plugin_id, "example.second");
    }

    #[cfg(unix)]
    #[test]
    fn save_follows_absolute_and_relative_links_to_missing_target() {
        let path = temp_registry_path("dangling-chain");
        let root = path.parent().unwrap();
        let middle = root.join("links/registry.json");
        let target = root.join("dotfiles/herdr/plugins.json");
        std::fs::create_dir_all(middle.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&middle, &path).unwrap();
        std::os::unix::fs::symlink("../dotfiles/herdr/plugins.json", &middle).unwrap();

        save_to_path(&path, &[sample_plugin("example.new")]).unwrap();

        assert_eq!(std::fs::read_link(&path).unwrap(), middle);
        assert_eq!(
            std::fs::read_link(&middle).unwrap(),
            Path::new("../dotfiles/herdr/plugins.json")
        );
        assert_eq!(load_from_path(&target)[0].plugin_id, "example.new");
        assert!(!target.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_symlink_cycle_without_replacing_links() {
        let path = temp_registry_path("cycle");
        let root = path.parent().unwrap();
        let other = root.join("other.json");
        std::fs::create_dir_all(root).unwrap();
        std::os::unix::fs::symlink("other.json", &path).unwrap();
        std::os::unix::fs::symlink("plugins.json", &other).unwrap();

        assert!(save_to_path(&path, &[sample_plugin("example.new")]).is_err());

        assert_eq!(std::fs::read_link(&path).unwrap(), Path::new("other.json"));
        assert_eq!(
            std::fs::read_link(&other).unwrap(),
            Path::new("plugins.json")
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_failure_preserves_symlink_and_target_directory() {
        let path = temp_registry_path("invalid-target");
        let root = path.parent().unwrap();
        let target = root.join("directory");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep"), "unchanged").unwrap();
        std::os::unix::fs::symlink("directory", &path).unwrap();

        assert!(save_to_path(&path, &[sample_plugin("example.new")]).is_err());

        assert_eq!(std::fs::read_link(&path).unwrap(), Path::new("directory"));
        assert_eq!(
            std::fs::read_to_string(target.join("keep")).unwrap(),
            "unchanged"
        );
        assert!(!target.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn update_preserves_symlink_and_existing_plugin_settings() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let base = temp_registry_path("update");
        let base = base.parent().unwrap();
        let previous_config_home = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", base);
        let path = registry_path();
        let target = base.join("dotfiles/plugins.json");
        let mut existing = sample_plugin("example.existing");
        existing.enabled = false;
        existing.source.kind = crate::api::schema::PluginSourceKind::Github;
        existing.source.owner = Some("example".into());
        save_to_path(&target, &[existing.clone()]).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let result = update(|plugins| plugins.push(sample_plugin("example.added")));
        match previous_config_home {
            Some(previous) => std::env::set_var("XDG_CONFIG_HOME", previous),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }

        let (_, entries) = result.unwrap();
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        assert_eq!(entries[0].plugin_id, "example.added");
        assert_eq!(
            serde_json::to_value(&entries[1]).unwrap(),
            serde_json::to_value(existing).unwrap()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(&target).unwrap()
        );
        assert_eq!(load_from_path(&target).len(), 2);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn save_replaces_existing_registry_file() {
        let path = temp_registry_path("replace-existing");
        save_to_path(&path, &[sample_plugin("example.first")]).unwrap();
        save_to_path(&path, &[sample_plugin("example.second")]).unwrap();

        let loaded = load_from_path(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].plugin_id, "example.second");
    }
}
