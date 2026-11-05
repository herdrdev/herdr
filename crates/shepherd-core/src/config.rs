//! Shepherd YAML configuration and role routing, compatible with the Go config shape.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "empty_vendors")]
    pub vendors: HashMap<String, Vendor>,
    #[serde(default = "empty_roles")]
    pub roles: HashMap<String, Role>,
    #[serde(default = "SkillsConfig::default")]
    pub skills: SkillsConfig,
    #[serde(default = "ShepherdConfig::default")]
    pub shepherd: ShepherdConfig,
    #[serde(default = "WorkflowConfig::default")]
    pub workflow: WorkflowConfig,
    #[serde(default = "Limits::default")]
    pub limits: Limits,
    #[serde(default = "General::default")]
    pub general: General,
    #[serde(default)]
    pub snapshots: SnapshotsConfig,
}

fn empty_vendors() -> HashMap<String, Vendor> {
    HashMap::new()
}
fn empty_roles() -> HashMap<String, Role> {
    HashMap::new()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Vendor {
    pub command: String,
    pub args: Vec<String>,
    pub models: Vec<String>,
    pub default_model: String,
    pub model_flag: String,
    pub print_args: Vec<String>,
    pub mcp: Mcp,
    pub skills: VendorSkills,
    pub shepherd_agent: String,
    pub disabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VendorSkills {
    pub mode: String,
    pub style: String,
    pub file: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Mcp {
    pub supported: bool,
    pub style: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub global: Vec<String>,
    pub disabled: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ShepherdConfig {
    pub backend: String,
    pub session: String,
    pub autostart: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Role {
    pub vendor: String,
    pub model: String,
    pub fallback: Vec<String>,
    pub must_differ_from: String,
    pub worktree: bool,
    pub can_spawn: bool,
    pub skills: Vec<String>,
    pub prompt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowConfig {
    pub max_review_rounds: u32,
    pub confirm: bool,
    pub base_branch: String,
    pub checks: Vec<String>,
    pub pr_template: String,
    pub draft: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    #[serde(alias = "max_depth")]
    pub max_spawn_depth: u32,
    #[serde(alias = "max_concurrent")]
    pub max_concurrent_agents: usize,
    pub agent_timeout: String,
    pub protected_branches: Vec<String>,
    pub non_mcp_done_quiet_secs: u64,
    pub notice_after_secs: u64,
    /// Seconds a pattern run stays quiescent (agents idle, nothing published on its topics) before it is `done`.
    pub run_quiescence_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_spawn_depth: 2,
            max_concurrent_agents: 4,
            agent_timeout: "30m".into(),
            protected_branches: vec!["main".into(), "master".into(), "develop".into()],
            non_mcp_done_quiet_secs: 60,
            notice_after_secs: 3,
            run_quiescence_secs: 30,
        }
    }
}

/// `[shepherd.snapshots]`: limits for local per-agent snapshots (F11a).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotsConfig {
    pub max_per_agent: usize,
    pub max_file_bytes: u64,
    pub max_capture_bytes: u64,
}

impl Default for SnapshotsConfig {
    fn default() -> Self {
        Self {
            max_per_agent: 50,
            max_file_bytes: 2 * 1024 * 1024,
            max_capture_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub log_level: String,
    pub theme: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            theme: "auto".into(),
        }
    }
}

impl Default for ShepherdConfig {
    fn default() -> Self {
        Self {
            backend: "socket".into(),
            session: "shepherd".into(),
            autostart: true,
        }
    }
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            max_review_rounds: 3,
            confirm: false,
            base_branch: String::new(),
            checks: Vec::new(),
            pr_template: String::new(),
            draft: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut vendors = HashMap::new();
        vendors.insert(
            "claude".into(),
            Vendor {
                command: "claude".into(),
                models: vec!["opus".into(), "sonnet".into(), "haiku".into()],
                default_model: "opus".into(),
                model_flag: "--model".into(),
                print_args: vec!["-p".into()],
                mcp: Mcp {
                    supported: true,
                    style: "claude-config".into(),
                },
                skills: VendorSkills {
                    mode: "native".into(),
                    style: "claude-plugin".into(),
                    file: String::new(),
                },
                shepherd_agent: "claude".into(),
                ..Vendor::default()
            },
        );
        vendors.insert(
            "codex".into(),
            Vendor {
                command: "codex".into(),
                model_flag: "-m".into(),
                print_args: vec!["exec".into()],
                mcp: Mcp {
                    supported: true,
                    style: "codex-overrides".into(),
                },
                skills: VendorSkills {
                    mode: "instructions".into(),
                    style: "codex-developer".into(),
                    file: String::new(),
                },
                shepherd_agent: "codex".into(),
                ..Vendor::default()
            },
        );
        vendors.insert(
            "gemini".into(),
            Vendor {
                command: "agy".into(),
                model_flag: "--model".into(),
                print_args: vec!["-p".into()],
                skills: VendorSkills {
                    mode: "prompt".into(),
                    ..VendorSkills::default()
                },
                shepherd_agent: "agy".into(),
                ..Vendor::default()
            },
        );

        let mut roles = HashMap::new();
        roles.insert(
            "brain".into(),
            Role {
                vendor: "claude".into(),
                model: "opus".into(),
                can_spawn: true,
                skills: words("shepherd-comms plan-tasks pr-workflow docs-lookup"),
                ..Role::default()
            },
        );
        roles.insert(
            "coder".into(),
            Role {
                vendor: "codex".into(),
                fallback: words("gemini claude"),
                worktree: true,
                skills: words(
                    "shepherd-comms implement-task write-tests conventional-commits lang-go",
                ),
                ..Role::default()
            },
        );
        roles.insert(
            "reviewer".into(),
            Role {
                vendor: "gemini".into(),
                must_differ_from: "coder".into(),
                skills: words("shepherd-comms code-review security-review"),
                ..Role::default()
            },
        );
        roles.insert(
            "docs".into(),
            Role {
                vendor: "gemini".into(),
                skills: words("shepherd-comms docs-lookup"),
                ..Role::default()
            },
        );
        roles.insert(
            "tester".into(),
            Role {
                vendor: "claude".into(),
                model: "sonnet".into(),
                skills: words("shepherd-comms write-tests debug"),
                ..Role::default()
            },
        );

        Self {
            vendors,
            roles,
            skills: SkillsConfig::default(),
            shepherd: ShepherdConfig::default(),
            workflow: WorkflowConfig::default(),
            limits: Limits::default(),
            general: General::default(),
            snapshots: SnapshotsConfig::default(),
        }
    }
}

fn words(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_owned).collect()
}

#[derive(Debug, Deserialize, Default)]
struct TomlFileConfig {
    #[serde(default)]
    shepherd: Option<TomlShepherdSection>,
}

#[derive(Debug, Deserialize, Default)]
struct TomlShepherdSection {
    #[serde(default)]
    limits: Option<TomlLimitsSection>,
    #[serde(default)]
    snapshots: Option<TomlSnapshotsSection>,
}

#[derive(Debug, Deserialize, Default)]
struct TomlSnapshotsSection {
    max_per_agent: Option<usize>,
    max_file_bytes: Option<u64>,
    max_capture_bytes: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
struct TomlLimitsSection {
    max_depth: Option<u32>,
    max_concurrent: Option<usize>,
    notice_after_secs: Option<u64>,
    run_quiescence_secs: Option<u64>,
}

pub fn global_config_toml_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SHEPHERD_CONFIG_PATH") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    let config_home = if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path)
    } else {
        home_dir().join(".config")
    };
    let candidates = [
        config_home.join("shepherd-dev/config.toml"),
        config_home.join("shepherd/config.toml"),
        config_home.join("herdr-dev/config.toml"), // fallback herdr directory
        config_home.join("herdr/config.toml"),     // fallback herdr directory
    ];
    for candidate in &candidates {
        if candidate.is_file() {
            return Some(candidate.clone());
        }
    }
    if let Ok(path) = std::env::var("SHEPHERD_CONFIG_PATH") {
        return Some(PathBuf::from(path));
    }
    None
}

/// Apply Go-compatible defaults < global YAML < project YAML < config.toml, recursively merging mappings.
/// A YAML null deletes a key; sequences and scalar values replace the previous layer.
/// [shepherd.limits] in config.toml takes priority over YAML limits.
pub fn load_config(
    project_dir: impl AsRef<Path>,
) -> Result<Config, Box<dyn std::error::Error + Send + Sync>> {
    load_config_with_diagnostics(project_dir).map(|(cfg, _)| cfg)
}

pub fn load_config_with_diagnostics(
    project_dir: impl AsRef<Path>,
) -> Result<(Config, Vec<String>), Box<dyn std::error::Error + Send + Sync>> {
    let mut tree = serde_yaml_ng::to_value(Config::default())?;
    let mut deprecations = Vec::new();
    let yaml_paths = [
        global_config_path(),
        project_config_path(project_dir.as_ref()),
    ];

    for path in &yaml_paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let layer: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;

        if let serde_yaml_ng::Value::Mapping(ref map) = layer {
            if map.contains_key("limits") {
                deprecations.push(format!(
                    "'limits' in {} is deprecated; configure '[shepherd.limits]' in config.toml instead",
                    path.display()
                ));
            }
        }

        merge_yaml(&mut tree, layer);
    }

    let mut config: Config = serde_yaml_ng::from_value(tree)?;

    // Layer TOML config over YAML (TOML > YAML priority)
    let mut toml_candidates = Vec::new();
    if let Some(global_toml) = global_config_toml_path() {
        toml_candidates.push(global_toml);
    }
    let project_toml = project_dir.as_ref().join(".shepherd/config.toml");
    if project_toml.is_file() {
        toml_candidates.push(project_toml);
    }

    for toml_path in toml_candidates {
        let content = match std::fs::read_to_string(&toml_path) {
            Ok(c) => c,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(format!("{}: {err}", toml_path.display()).into()),
        };

        let parsed: TomlFileConfig =
            toml::from_str(&content).map_err(|err| format!("{}: {err}", toml_path.display()))?;

        if let Some(shep) = parsed.shepherd {
            if let Some(lim) = shep.limits {
                if let Some(depth) = lim.max_depth {
                    config.limits.max_spawn_depth = depth;
                }
                if let Some(conc) = lim.max_concurrent {
                    config.limits.max_concurrent_agents = conc;
                }
                if let Some(notice) = lim.notice_after_secs {
                    config.limits.notice_after_secs = notice;
                }
                if let Some(quiet) = lim.run_quiescence_secs {
                    config.limits.run_quiescence_secs = quiet;
                }
            }
            if let Some(snap) = shep.snapshots {
                if let Some(v) = snap.max_per_agent {
                    config.snapshots.max_per_agent = v;
                }
                if let Some(v) = snap.max_file_bytes {
                    config.snapshots.max_file_bytes = v;
                }
                if let Some(v) = snap.max_capture_bytes {
                    config.snapshots.max_capture_bytes = v;
                }
            }
        }
    }

    Ok((config, deprecations))
}

pub fn load_config_files(
    paths: &[PathBuf],
) -> Result<Config, Box<dyn std::error::Error + Send + Sync>> {
    let mut tree = serde_yaml_ng::to_value(Config::default())?;
    for path in paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let layer: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        merge_yaml(&mut tree, layer);
    }
    Ok(serde_yaml_ng::from_value(tree)?)
}

fn merge_yaml(dst: &mut serde_yaml_ng::Value, src: serde_yaml_ng::Value) {
    use serde_yaml_ng::Value;
    match src {
        Value::Mapping(source) => {
            if let Value::Mapping(destination) = dst {
                for (key, value) in source {
                    if value.is_null() {
                        destination.remove(&key);
                    } else if destination.contains_key(&key) {
                        merge_yaml(destination.get_mut(&key).expect("key exists"), value);
                    } else {
                        match key.as_str() {
                            Some("max_depth") => {
                                let k = Value::String("max_spawn_depth".into());
                                destination.remove(&k);
                            }
                            Some("max_spawn_depth") => {
                                let k = Value::String("max_depth".into());
                                destination.remove(&k);
                            }
                            Some("max_concurrent") => {
                                let k = Value::String("max_concurrent_agents".into());
                                destination.remove(&k);
                            }
                            Some("max_concurrent_agents") => {
                                let k = Value::String("max_concurrent".into());
                                destination.remove(&k);
                            }
                            _ => {}
                        }
                        destination.insert(key, value);
                    }
                }
            } else {
                *dst = Value::Mapping(source);
            }
        }
        value => *dst = value,
    }
}

pub fn global_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("shepherd/config.yaml");
    }
    home_dir().join(".config/shepherd/config.yaml")
}

pub fn state_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(path).join("shepherd");
    }
    home_dir().join(".local/state/shepherd")
}

pub fn project_config_path(start: &Path) -> PathBuf {
    let mut current = start.to_path_buf();
    let mut git_fallback = None;
    loop {
        let config = current.join(".shepherd/config.yaml");
        if config.exists() {
            return config;
        }
        if current.join(".git").exists() && git_fallback.is_none() {
            git_fallback = Some(config);
        }
        if !current.pop() {
            break;
        }
    }
    git_fallback.unwrap_or_else(|| start.join(".shepherd/config.yaml"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assignment {
    pub role: String,
    pub vendor: String,
    pub model: String,
    pub fallback: bool,
    pub note: String,
    pub fresh_session: bool,
}

#[derive(Debug)]
pub enum ResolveError {
    UnknownRole(String),
    NoVendor(String),
    DifferenceCycle(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownRole(name) => write!(f, "unknown role {name:?}"),
            Self::NoVendor(name) => write!(
                f,
                "role {name:?}: no usable vendor (none installed/enabled)"
            ),
            Self::DifferenceCycle(name) => write!(f, "must_differ_from cycle at role {name:?}"),
        }
    }
}
impl std::error::Error for ResolveError {}

pub fn resolve_role(
    config: &Config,
    active: &[String],
    role_name: &str,
) -> Result<Assignment, ResolveError> {
    resolve_inner(config, active, role_name, &mut HashSet::new())
}

fn resolve_inner(
    config: &Config,
    active: &[String],
    role_name: &str,
    seen: &mut HashSet<String>,
) -> Result<Assignment, ResolveError> {
    let role = config
        .roles
        .get(role_name)
        .ok_or_else(|| ResolveError::UnknownRole(role_name.into()))?;
    if !seen.insert(role_name.into()) {
        return Err(ResolveError::DifferenceCycle(role_name.into()));
    }
    let active_set: HashSet<&str> = active.iter().map(String::as_str).collect();
    let mut candidates = vec![role.vendor.clone()];
    candidates.extend(role.fallback.clone());
    let mut rest = active.to_vec();
    rest.sort();
    candidates.extend(rest);
    let mut unique = Vec::new();
    for vendor in candidates {
        if active_set.contains(vendor.as_str()) && !unique.contains(&vendor) {
            unique.push(vendor);
        }
    }
    let mut selected = unique
        .first()
        .cloned()
        .ok_or_else(|| ResolveError::NoVendor(role_name.into()))?;
    let other = if role.must_differ_from.is_empty() {
        None
    } else {
        let other = resolve_inner(config, active, &role.must_differ_from, seen)?;
        if let Some(vendor) = unique.iter().find(|vendor| **vendor != other.vendor) {
            selected = vendor.clone();
        }
        Some(other)
    };
    let fallback = selected != role.vendor;
    let mut note = if fallback {
        format!("{} unavailable, using {}", role.vendor, selected)
    } else {
        String::new()
    };
    let mut model = if selected == role.vendor && !role.model.is_empty() {
        role.model.clone()
    } else {
        config
            .vendors
            .get(&selected)
            .map(|vendor| vendor.default_model.clone())
            .unwrap_or_default()
    };
    let mut fresh_session = false;
    if let Some(other) = other {
        fresh_session = true;
        if other.vendor == selected {
            let models = config
                .vendors
                .get(&selected)
                .map(|vendor| vendor.models.as_slice())
                .unwrap_or_default();
            if model == other.model {
                if let Some(different) = models.iter().find(|candidate| **candidate != other.model)
                {
                    model = different.clone();
                }
            }
            note = format!("same vendor as {}", role.must_differ_from);
            note.push_str(if model == other.model {
                ", same model (no other model known)"
            } else {
                ", different model, fresh session"
            });
        }
    }
    Ok(Assignment {
        role: role_name.into(),
        vendor: selected,
        model,
        fallback,
        note,
        fresh_session,
    })
}

pub fn resolve_all(config: &Config, active: &[String]) -> Vec<Result<Assignment, ResolveError>> {
    let mut names: Vec<_> = config.roles.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| resolve_role(config, active, name))
        .collect()
}

pub fn available_vendors(config: &Config) -> Vec<String> {
    config
        .vendors
        .iter()
        .filter(|(_, vendor)| !vendor.disabled && executable_on_path(&vendor.command))
        .map(|(name, _)| name.clone())
        .collect()
}

fn executable_on_path(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .any(|dir| dir.join(command).is_file())
}

pub fn validate(config: &Config) -> Vec<String> {
    let mut issues = Vec::new();
    if !matches!(config.shepherd.backend.as_str(), "" | "socket" | "hc") {
        issues.push(format!(
            "shepherd.backend must be \"socket\" or \"hc\" (got {:?})",
            config.shepherd.backend
        ));
    }
    if config.limits.max_concurrent_agents == 0 {
        issues.push("limits.max_concurrent_agents must be at least 1".into());
    }
    issues.extend(crate::snapshot::limits::validate(&config.snapshots));
    for (name, role) in &config.roles {
        if !config.vendors.contains_key(&role.vendor) {
            issues.push(format!(
                "role {name:?} refers to unknown vendor {:?}",
                role.vendor
            ));
        }
        for fallback in &role.fallback {
            if !config.vendors.contains_key(fallback) {
                issues.push(format!(
                    "role {name:?} refers to unknown fallback vendor {fallback:?}"
                ));
            }
        }
        if !role.must_differ_from.is_empty() && !config.roles.contains_key(&role.must_differ_from) {
            issues.push(format!(
                "role {name:?} must_differ_from unknown role {:?}",
                role.must_differ_from
            ));
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_roles_through_fallbacks_and_independent_reviewers() {
        let config = Config::default();
        let active = vec!["codex".into(), "gemini".into()];
        let coder = resolve_role(&config, &active, "coder").unwrap();
        let reviewer = resolve_role(&config, &active, "reviewer").unwrap();
        assert_eq!(coder.vendor, "codex");
        assert_eq!(reviewer.vendor, "gemini");
        assert!(reviewer.fresh_session);
        assert!(resolve_role(&config, &active, "missing").is_err());
    }

    #[test]
    fn single_vendor_mode_uses_a_different_model_when_known() {
        let config = Config::default();
        let active = vec!["claude".into()];
        let reviewer = resolve_role(&config, &active, "reviewer").unwrap();
        assert_eq!(reviewer.vendor, "claude");
        assert_eq!(reviewer.model, "sonnet");
        assert!(reviewer.fresh_session);
        assert!(reviewer.note.contains("different model"));
    }

    #[test]
    fn detects_must_differ_cycles() {
        let mut config = Config::default();
        config.roles.insert(
            "a".into(),
            Role {
                vendor: "claude".into(),
                must_differ_from: "b".into(),
                ..Role::default()
            },
        );
        config.roles.insert(
            "b".into(),
            Role {
                vendor: "claude".into(),
                must_differ_from: "a".into(),
                ..Role::default()
            },
        );
        assert!(matches!(
            resolve_role(&config, &["claude".into()], "a"),
            Err(ResolveError::DifferenceCycle(_))
        ));
    }

    #[test]
    fn deep_merges_yaml_defaults_and_resets_null_keys() {
        let root = std::env::temp_dir().join(format!("shepherd-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let global = root.join("global.yaml");
        let project = root.join("project.yaml");
        std::fs::write(&global, "roles:\n  coder:\n    model: fast-model\n    skills: [lang-go]\nworkflow:\n  checks: [cargo test]\n").unwrap();
        std::fs::write(&project, "roles:\n  coder:\n    model: null\n    skills: [lang-rust]\nvendors:\n  gemini: null\n").unwrap();
        let config = load_config_files(&[global, project]).unwrap();
        assert_eq!(config.roles["coder"].vendor, "codex");
        assert_eq!(config.roles["coder"].model, "");
        assert_eq!(config.roles["coder"].skills, ["lang-rust"]);
        assert_eq!(config.workflow.checks, ["cargo test"]);
        assert!(!config.vendors.contains_key("gemini"));
        assert_eq!(config.limits.max_concurrent_agents, 4);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct EnvGuard {
        var: &'static str,
        orig: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(var: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
            let lock = crate::TEST_ENV_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let orig = std::env::var_os(var);
            std::env::set_var(var, val);
            Self {
                var,
                orig,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref val) = self.orig {
                std::env::set_var(self.var, val);
            } else {
                std::env::remove_var(self.var);
            }
        }
    }

    #[test]
    fn test_shepherd_limits_defaults() {
        let config = Config::default();
        assert_eq!(config.limits.max_spawn_depth, 2);
        assert_eq!(config.limits.max_concurrent_agents, 4);
        assert_eq!(config.limits.notice_after_secs, 3);
    }

    #[test]
    fn test_shepherd_limits_read_from_toml() {
        let root = std::env::temp_dir().join(format!("shep-test-toml-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let toml_file = root.join("config.toml");
        std::fs::write(
            &toml_file,
            "[shepherd.limits]\nmax_depth = 5\nmax_concurrent = 10\nnotice_after_secs = 15\n",
        )
        .unwrap();

        let _guard = EnvGuard::set("SHEPHERD_CONFIG_PATH", &toml_file);
        let config = load_config(&root).unwrap();
        assert_eq!(config.limits.max_spawn_depth, 5);
        assert_eq!(config.limits.max_concurrent_agents, 10);
        assert_eq!(config.limits.notice_after_secs, 15);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_snapshots_defaults_toml_and_invalid_values() {
        let d = SnapshotsConfig::default();
        assert_eq!(
            (d.max_per_agent, d.max_file_bytes, d.max_capture_bytes),
            (50, 2 * 1024 * 1024, 64 * 1024 * 1024)
        );
        assert!(validate(&Config::default()).is_empty());

        let root = std::env::temp_dir().join(format!("shep-test-snapcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let toml_file = root.join("config.toml");
        std::fs::write(
            &toml_file,
            "[shepherd.snapshots]\nmax_per_agent = 7\nmax_file_bytes = 1000\nmax_capture_bytes = 5000\n",
        )
        .unwrap();
        let guard = EnvGuard::set("SHEPHERD_CONFIG_PATH", &toml_file);
        let config = load_config(&root).unwrap();
        assert_eq!(config.snapshots.max_per_agent, 7);
        assert_eq!(config.snapshots.max_file_bytes, 1000);
        assert_eq!(config.snapshots.max_capture_bytes, 5000);
        assert!(validate(&config).is_empty());

        // Semantically invalid values are reported by `validate`.
        std::fs::write(
            &toml_file,
            "[shepherd.snapshots]\nmax_per_agent = 0\nmax_file_bytes = 100\nmax_capture_bytes = 10\n",
        )
        .unwrap();
        let issues = validate(&load_config(&root).unwrap());
        assert!(
            issues.iter().any(|i| i.contains("max_per_agent")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.contains("max_capture_bytes")),
            "{issues:?}"
        );

        // Wrongly typed values are a load error that names the file.
        std::fs::write(&toml_file, "[shepherd.snapshots]\nmax_per_agent = -3\n").unwrap();
        let err = load_config(&root).unwrap_err().to_string();
        assert!(err.contains("config.toml"), "{err}");
        drop(guard);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_shepherd_limits_priority_toml_over_yaml() {
        let root = std::env::temp_dir().join(format!("shep-test-priority-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shep_dir = root.join(".shepherd");
        std::fs::create_dir_all(&shep_dir).unwrap();

        std::fs::write(
            shep_dir.join("config.yaml"),
            "limits:\n  max_depth: 3\n  max_concurrent: 6\n",
        )
        .unwrap();

        let toml_file = root.join("custom_config.toml");
        std::fs::write(&toml_file, "[shepherd.limits]\nmax_concurrent = 12\n").unwrap();

        let _guard = EnvGuard::set("SHEPHERD_CONFIG_PATH", &toml_file);
        let config = load_config(&root).unwrap();
        // TOML overrides YAML for max_concurrent (12 > 6)
        assert_eq!(config.limits.max_concurrent_agents, 12);
        // YAML fallback preserves max_depth (3)
        assert_eq!(config.limits.max_spawn_depth, 3);
        // Default preserved for notice_after_secs (3)
        assert_eq!(config.limits.notice_after_secs, 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_shepherd_limits_invalid_toml_error() {
        let root =
            std::env::temp_dir().join(format!("shep-test-invalid-toml-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let toml_file = root.join("config.toml");
        std::fs::write(&toml_file, "[shepherd.limits\nmalformed = syntax").unwrap();

        let _guard = EnvGuard::set("SHEPHERD_CONFIG_PATH", &toml_file);
        let err = load_config(&root).unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("config.toml")
                || err_msg.contains("syntax")
                || err_msg.contains("expected"),
            "Error should clearly identify TOML parse error: {err_msg}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_shepherd_limits_config_validate_yaml_deprecation_warning() {
        let root =
            std::env::temp_dir().join(format!("shep-test-deprecation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shep_dir = root.join(".shepherd");
        std::fs::create_dir_all(&shep_dir).unwrap();

        std::fs::write(
            shep_dir.join("config.yaml"),
            "limits:\n  max_depth: 3\n  max_concurrent: 5\n",
        )
        .unwrap();

        // Clear any SHEPHERD_CONFIG_PATH
        let _guard = EnvGuard::set("SHEPHERD_CONFIG_PATH", "/tmp/nonexistent-config.toml");
        let (config, deprecations) = load_config_with_diagnostics(&root).unwrap();
        assert_eq!(config.limits.max_spawn_depth, 3);
        assert_eq!(config.limits.max_concurrent_agents, 5);
        assert_eq!(deprecations.len(), 1);
        assert!(deprecations[0].contains("deprecated"));
        assert!(deprecations[0].contains("[shepherd.limits]"));

        // Test cli config_validate in JSON mode succeeds with valid=true
        let mut json_buf = Vec::new();
        crate::cli::config_validate(root.clone(), true, &mut json_buf).unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&json_buf).unwrap();
        assert_eq!(json_val["valid"], true);
        assert!(json_val["issues"].as_array().unwrap().is_empty());

        // Test cli config_validate in text mode writes valid message
        let mut text_buf = Vec::new();
        crate::cli::config_validate(root.clone(), false, &mut text_buf).unwrap();
        let text_str = String::from_utf8(text_buf).unwrap();
        assert!(text_str.contains("configuration is valid"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
