use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use tracing::warn;

use super::{model::LoadedConfig, Config, CONFIG_PATH_ENV_VAR};

const KNOWN_TOP_LEVEL_CONFIG_KEYS: &[&str] = &[
    "actions",
    "advanced",
    "agent_detection",
    "day_board",
    "experimental",
    "files",
    "goals_panel",
    "keys",
    "launch_profiles",
    "linear",
    "missive",
    "notepad",
    "scratch",
    "onboarding",
    "panel",
    "planning_lock",
    "pomodoro",
    "projects",
    "remote",
    "server",
    "session",
    "source_control",
    "terminal",
    "theme",
    "ui",
    "update",
    "usage",
    "worktrees",
    "work_index",
];

pub fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_config_dir()
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_state_dir()
}

#[cfg(windows)]
fn platform_config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("APPDATA") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Roaming")
            .join(app_dir_name());
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(format!(".config/{}", app_dir_name()));
    }
    std::env::temp_dir().join(app_dir_name())
}

#[cfg(not(windows))]
fn platform_config_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".config/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(app_dir_name())
    }
}

#[cfg(windows)]
fn platform_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Local")
            .join(app_dir_name());
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(format!(".local/state/{}", app_dir_name()));
    }
    std::env::temp_dir().join(format!("{}-state", app_dir_name()))
}

#[cfg(not(windows))]
fn platform_state_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".local/state/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(format!("{}-state", app_dir_name()))
    }
}

fn read_optional_config(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

impl Config {
    pub fn load() -> LoadedConfig {
        let path = config_path();
        let content = match read_optional_config(&path) {
            Ok(Some(content)) => content,
            Ok(None) => {
                return LoadedConfig {
                    config: Self::default(),
                    diagnostics: Vec::new(),
                    invalid_sections: Vec::new(),
                };
            }
            Err(err) => {
                warn!(err = %err, "config read error, using defaults");
                return LoadedConfig {
                    config: Self::default(),
                    diagnostics: vec![format!("config read error: {err}; using defaults")],
                    invalid_sections: Vec::new(),
                };
            }
        };

        match deserialize_with_ignored::<Config, _>(toml::Deserializer::new(&content)) {
            Ok((mut config, ignored_keys)) => {
                config.clamp_safety_bounds();
                let (unknown_sections, mut diagnostics) =
                    unknown_top_level_sections_from_str(&content);
                diagnostics.extend(unknown_config_key_diagnostics(
                    ignored_keys
                        .into_iter()
                        .filter(|path| {
                            !matches!(path.as_slice(), [ConfigKeyPathSegment::Key(key)] if unknown_sections.contains(key))
                        })
                        .collect(),
                    None,
                ));
                diagnostics.extend(config.collect_diagnostics());
                LoadedConfig {
                    config,
                    diagnostics,
                    invalid_sections: Vec::new(),
                }
            }
            Err(err) => {
                warn!(err = %err, "config parse error, using defaults");
                LoadedConfig {
                    config: Self::default(),
                    diagnostics: vec![format!("config parse error: {err}; using defaults")],
                    invalid_sections: Vec::new(),
                }
            }
        }
    }
}

pub(super) fn resolve_config_relative_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(path)
}

pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var(CONFIG_PATH_ENV_VAR) {
        return PathBuf::from(path);
    }
    config_dir().join("config.toml")
}

pub fn config_diagnostic_summary(diagnostics: &[String]) -> Option<String> {
    if diagnostics.is_empty() {
        return None;
    }

    let target = config_path()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml")
        .to_string();
    let read_error = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.starts_with("config read error:"));
    let impact = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("using defaults"))
    {
        if read_error {
            " unreadable; using defaults"
        } else {
            " invalid; using defaults"
        }
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("keeping current config"))
    {
        if read_error {
            " unreadable; keeping current config"
        } else {
            " invalid; keeping current config"
        }
    } else if diagnostics
        .iter()
        .all(|diagnostic| diagnostic.starts_with("unknown config key "))
    {
        " has unknown keys"
    } else {
        ""
    };

    Some(format!("{target}{impact}; herdr config check"))
}

pub fn load_live_config() -> Result<LoadedConfig, Vec<String>> {
    let path = config_path();
    let content = match read_optional_config(&path) {
        Ok(Some(content)) => content,
        Ok(None) => {
            return Ok(LoadedConfig {
                config: Config::default(),
                diagnostics: Vec::new(),
                invalid_sections: Vec::new(),
            });
        }
        Err(err) => {
            return Err(vec![format!(
                "config read error: {err}; keeping current config"
            )]);
        }
    };
    load_live_config_from_str(&content)
}

fn load_live_config_from_str(content: &str) -> Result<LoadedConfig, Vec<String>> {
    let value = content
        .parse::<toml::Value>()
        .map_err(|err| vec![format!("config parse error: {err}; keeping current config")])?;
    let table = value.as_table().ok_or_else(|| {
        vec![
            "config parse error: top-level config must be a table; keeping current config"
                .to_string(),
        ]
    })?;

    let mut config = Config::default();
    let mut diagnostics = unknown_top_level_section_diagnostics(table);
    diagnostics.extend(unknown_top_level_config_key_diagnostics(table));
    let mut invalid_sections = Vec::new();

    if let Some(value) = table.get("onboarding") {
        match value.clone().try_into::<Option<bool>>() {
            Ok(onboarding) => config.onboarding = onboarding,
            Err(err) => diagnostics.push(format!(
                "invalid onboarding setting: {err}; keeping current onboarding state"
            )),
        }
    }

    load_live_section(
        table,
        "theme",
        "theme config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.theme = section,
    );
    load_live_section(
        table,
        "keys",
        "keybinding config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.keys = section,
    );
    load_live_section(
        table,
        "actions",
        "user action config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.actions = section,
    );
    load_live_section(
        table,
        "terminal",
        "terminal config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.terminal = section,
    );
    load_live_section(
        table,
        "scratch",
        "scratch config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.scratch = section,
    );
    load_live_section(
        table,
        "notepad",
        "notepad config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.notepad = section,
    );
    load_live_section(
        table,
        "goals_panel",
        "goals panel config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.goals_panel = section,
    );
    load_live_section(
        table,
        "pomodoro",
        "break timer config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.pomodoro = section,
    );
    load_live_section(
        table,
        "session",
        "session config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.session = section,
    );
    load_live_section(
        table,
        "server",
        "server config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.server = section,
    );
    load_live_section(
        table,
        "update",
        "update config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.update = section,
    );
    load_live_section(
        table,
        "ui",
        "ui config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.ui = section,
    );
    load_live_section(
        table,
        "agent_detection",
        "agent detection config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.agent_detection = section,
    );
    load_live_section(
        table,
        "advanced",
        "advanced config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.advanced = section,
    );
    load_live_section(
        table,
        "worktrees",
        "worktree config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.worktrees = section,
    );
    load_live_section(
        table,
        "experimental",
        "experimental config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.experimental = section,
    );
    load_live_section(
        table,
        "remote",
        "remote config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.remote = section,
    );
    load_live_section(
        table,
        "source_control",
        "source control config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.source_control = section,
    );
    load_live_section(
        table,
        "files",
        "files config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.files = section,
    );
    load_live_section(
        table,
        "panel",
        "panel config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.panel = section,
    );
    load_live_section(
        table,
        "usage",
        "usage config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.usage = section,
    );
    load_live_section(
        table,
        "work_index",
        "work index config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.work_index = section,
    );
    load_live_section(
        table,
        "day_board",
        "day board config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.day_board = section,
    );
    load_live_section(
        table,
        "missive",
        "Missive config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.missive = section,
    );

    diagnostics.extend(config.theme.diagnostics());
    config.clamp_safety_bounds();

    Ok(LoadedConfig {
        config,
        diagnostics,
        invalid_sections,
    })
}

fn unknown_top_level_sections_from_str(content: &str) -> (Vec<String>, Vec<String>) {
    let Ok(value) = content.parse::<toml::Value>() else {
        return (Vec::new(), Vec::new());
    };
    let Some(table) = value.as_table() else {
        return (Vec::new(), Vec::new());
    };

    let mut keys = Vec::new();
    let mut diagnostics = Vec::new();
    for (key, value) in table {
        if let Some(diagnostic) = unknown_top_level_section_diagnostic(key, value) {
            keys.push(key.clone());
            diagnostics.push(diagnostic);
        }
    }
    (keys, diagnostics)
}

fn unknown_top_level_section_diagnostics(
    table: &toml::map::Map<String, toml::Value>,
) -> Vec<String> {
    table
        .iter()
        .filter_map(|(key, value)| unknown_top_level_section_diagnostic(key, value))
        .collect()
}

fn unknown_top_level_section_diagnostic(key: &str, value: &toml::Value) -> Option<String> {
    if KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(&key) {
        return None;
    }

    let header = if value.is_table() {
        format!("[{key}]")
    } else if value
        .as_array()
        .is_some_and(|items| !items.is_empty() && items.iter().all(toml::Value::is_table))
    {
        format!("[[{key}]]")
    } else {
        return None;
    };

    if key == "toast" {
        Some(format!(
            "unknown config section {header}; did you mean [ui.toast]? ignoring section"
        ))
    } else {
        Some(format!("unknown config section {header}; ignoring section"))
    }
}

fn unknown_top_level_config_key_diagnostics(
    table: &toml::map::Map<String, toml::Value>,
) -> Vec<String> {
    let paths = table
        .iter()
        .filter(|(key, value)| {
            !KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(&key.as_str())
                && unknown_top_level_section_diagnostic(key, value).is_none()
        })
        .map(|(key, _)| vec![ConfigKeyPathSegment::Key(key.clone())])
        .collect();
    unknown_config_key_diagnostics(paths, None)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ConfigKeyPathSegment {
    Key(String),
    Index(usize),
}

fn config_key_path(path: &serde_ignored::Path<'_>) -> Vec<ConfigKeyPathSegment> {
    fn visit(path: &serde_ignored::Path<'_>, segments: &mut Vec<ConfigKeyPathSegment>) {
        match path {
            serde_ignored::Path::Root => {}
            serde_ignored::Path::Seq { parent, index } => {
                visit(parent, segments);
                segments.push(ConfigKeyPathSegment::Index(*index));
            }
            serde_ignored::Path::Map { parent, key } => {
                visit(parent, segments);
                segments.push(ConfigKeyPathSegment::Key(key.clone()));
            }
            serde_ignored::Path::Some { parent }
            | serde_ignored::Path::NewtypeStruct { parent }
            | serde_ignored::Path::NewtypeVariant { parent } => visit(parent, segments),
        }
    }

    let mut segments = Vec::new();
    visit(path, &mut segments);
    segments
}

fn format_config_key_path(path: &[ConfigKeyPathSegment]) -> String {
    path.iter()
        .map(|segment| match segment {
            ConfigKeyPathSegment::Key(key)
                if !key.is_empty()
                    && key.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                    }) =>
            {
                key.clone()
            }
            ConfigKeyPathSegment::Key(key) => toml::Value::String(key.clone()).to_string(),
            ConfigKeyPathSegment::Index(index) => index.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn unknown_config_key_diagnostics(
    paths: Vec<Vec<ConfigKeyPathSegment>>,
    section: Option<&str>,
) -> Vec<String> {
    let mut paths: Vec<Vec<ConfigKeyPathSegment>> = paths
        .into_iter()
        .map(|mut path| {
            if let Some(section) = section {
                path.insert(0, ConfigKeyPathSegment::Key(section.to_string()));
            }
            path
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            format!(
                "unknown config key {}; ignoring key",
                format_config_key_path(&path)
            )
        })
        .collect()
}

fn deserialize_with_ignored<'de, T, D>(
    deserializer: D,
) -> Result<(T, Vec<Vec<ConfigKeyPathSegment>>), D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    let mut ignored = Vec::new();
    let value = serde_ignored::deserialize(deserializer, |path| {
        ignored.push(config_key_path(&path));
    })?;
    Ok((value, ignored))
}

fn load_live_section<T>(
    table: &toml::map::Map<String, toml::Value>,
    section: &'static str,
    label: &str,
    diagnostics: &mut Vec<String>,
    invalid_sections: &mut Vec<String>,
    apply: impl FnOnce(T),
) where
    T: serde::de::DeserializeOwned,
{
    let Some(value) = table.get(section) else {
        return;
    };

    match deserialize_with_ignored(value.clone()) {
        Ok((section_config, ignored_keys)) => {
            diagnostics.extend(unknown_config_key_diagnostics(ignored_keys, Some(section)));
            apply(section_config);
        }
        Err(err) => {
            diagnostics.push(format!(
                "invalid {label}: {err}; keeping current {section} settings"
            ));
            invalid_sections.push(section.to_string());
        }
    }
}

pub(crate) fn upsert_top_level_bool(content: &str, key: &str, value: bool) -> String {
    let replacement = format!("{key} = {value}");
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    let mut in_section = false;

    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            continue;
        }
        if in_section {
            continue;
        }
        if trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")) {
            *line = replacement.clone();
            return lines.join("\n") + "\n";
        }
    }

    if lines.is_empty() {
        format!("{replacement}\n")
    } else {
        format!("{replacement}\n{}\n", lines.join("\n").trim_end())
    }
}

/// Write a key = value pair in a TOML section (creates section if missing).
pub fn upsert_section_value(content: &str, section: &str, key: &str, value: &str) -> String {
    upsert_section_raw(content, section, key, value)
}

pub fn upsert_section_bool(content: &str, section: &str, key: &str, value: bool) -> String {
    upsert_section_raw(content, section, key, &value.to_string())
}

pub fn remove_section_key(content: &str, section: &str, key: &str) -> String {
    let header = format!("[{section}]");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut in_section = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == header;
            result.push(line.to_string());
            i += 1;
            continue;
        }

        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            i += 1;
            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    result.join("\n") + "\n"
}

pub fn remove_keybinding_config_sections(content: &str) -> (String, bool) {
    let mut result = Vec::new();
    let mut removed = false;
    let mut skipping_key_section = false;
    let mut in_table = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(table_name) = toml_table_header_name(trimmed) {
            in_table = true;
            skipping_key_section = is_keys_table_name(table_name);
            if skipping_key_section {
                removed = true;
                continue;
            }
        } else if skipping_key_section || (!in_table && is_top_level_keys_assignment(trimmed)) {
            removed = true;
            continue;
        }

        result.push(line.to_string());
    }

    let mut updated = result.join("\n");
    if content.ends_with('\n') || !updated.is_empty() {
        updated.push('\n');
    }
    (updated, removed)
}

pub fn replace_actions_tables(
    content: &str,
    actions: &[crate::config::ActionConfig],
) -> Result<String, toml::ser::Error> {
    let mut replacement = String::new();
    for action in actions {
        replacement.push_str("[[actions]]\n");
        replacement.push_str(&toml::to_string(action)?);
    }

    let mut updated = String::with_capacity(content.len().saturating_add(replacement.len()));
    let mut skipping_action = false;
    let mut inserted = false;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim();
        if toml_table_header_name(trimmed).is_some() {
            if trimmed == "[[actions]]" {
                if !inserted {
                    updated.push_str(&replacement);
                    inserted = true;
                }
                skipping_action = true;
                continue;
            }
            skipping_action = false;
        }
        if !skipping_action {
            updated.push_str(line);
        }
    }

    if !inserted && !replacement.is_empty() {
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        if !updated.is_empty() && !updated.ends_with("\n\n") {
            updated.push('\n');
        }
        updated.push_str(&replacement);
    }
    Ok(updated)
}

static ACTION_SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn write_actions_atomically(
    path: &Path,
    actions: &[crate::config::ActionConfig],
) -> std::io::Result<()> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let updated = replace_actions_tables(&content, actions)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    updated
        .parse::<toml::Value>()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

    // Keep a dotfiles-managed symlink intact: write to its target.
    let resolved = crate::platform::resolve_write_target(path)?;
    let path = resolved.as_path();
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("config path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let temp_path = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        ACTION_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    if let Ok(metadata) = std::fs::metadata(path) {
        let _ = file.set_permissions(metadata.permissions());
    }
    if let Err(err) = file
        .write_all(updated.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    drop(file);
    if let Err(err) = crate::platform::replace_file_durably(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

fn toml_table_header_name(trimmed: &str) -> Option<&str> {
    if let Some(name) = trimmed
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
    {
        return Some(name.trim());
    }
    trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .map(str::trim)
}

fn is_keys_table_name(name: &str) -> bool {
    name == "keys" || name.starts_with("keys.")
}

fn is_top_level_keys_assignment(trimmed: &str) -> bool {
    trimmed.starts_with("keys ") || trimmed.starts_with("keys=") || trimmed.starts_with("keys.")
}

fn upsert_section_raw(content: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{section}]");
    let assignment = format!("{key} = {value}");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut found_section = false;
    let mut inserted = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed == header {
            found_section = true;
            result.push(line.to_string());
            i += 1;

            while i < lines.len() {
                let current = lines[i];
                let current_trimmed = current.trim();
                if current_trimmed.starts_with('[') && current_trimmed.ends_with(']') {
                    if !inserted {
                        result.push(assignment.clone());
                        inserted = true;
                    }
                    break;
                }

                if current_trimmed.starts_with(&format!("{key} "))
                    || current_trimmed.starts_with(&format!("{key}="))
                {
                    result.push(assignment.clone());
                    inserted = true;
                } else {
                    result.push(current.to_string());
                }
                i += 1;
            }

            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    if !found_section {
        if !result.is_empty() && !result.last().is_some_and(|line| line.trim().is_empty()) {
            result.push(String::new());
        }
        result.push(header);
        result.push(assignment);
    } else if !inserted {
        result.push(assignment);
    }

    result.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn symlink_scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-config-symlink-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[cfg(unix)]
    #[test]
    fn write_actions_atomically_follows_a_symlinked_config() {
        let dir = symlink_scratch_dir();
        let target = dir.join("generated.toml");
        let link = dir.join("config.toml");
        std::fs::write(&target, "onboarding = false\n").expect("seed");
        std::os::unix::fs::symlink("generated.toml", &link).expect("symlink");

        let actions = vec![crate::config::ActionConfig {
            name: "build".into(),
            command: "just check".into(),
            key: Some("prefix+ctrl+b".into()),
            ..Default::default()
        }];
        write_actions_atomically(&link, &actions).expect("write actions");

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "action save replaced the managed symlink with a regular file"
        );
        let written = std::fs::read_to_string(&target).expect("read target");
        assert!(written.contains("onboarding = false"));
        assert!(written.contains("name = \"build\""));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upsert_top_level_bool_replaces_existing_value() {
        let content = "onboarding = true\n[keys]\nprefix = \"ctrl+b\"\n";
        let updated = upsert_top_level_bool(content, "onboarding", false);
        assert!(updated.contains("onboarding = false"));
        assert!(!updated.contains("onboarding = true"));
    }

    #[test]
    fn upsert_section_bool_adds_missing_section() {
        let updated = upsert_section_bool("", "ui.toast", "enabled", true);
        assert!(updated.contains("[ui.toast]"));
        assert!(updated.contains("enabled = true"));
    }

    #[test]
    fn remove_section_key_removes_matching_key_from_section() {
        let content =
            "[ui.toast]\nenabled = true\ndelivery = \"herdr\"\n[ui.sound]\nenabled = true\n";
        let updated = remove_section_key(content, "ui.toast", "enabled");
        assert!(!updated.contains("[ui.toast]\nenabled = true"));
        assert!(updated.contains("delivery = \"herdr\""));
        assert!(updated.contains("[ui.sound]\nenabled = true"));
    }

    #[test]
    fn config_diagnostic_summary_uses_compact_actionable_banner() {
        let diagnostics = vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
            "five".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml; herdr config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_unknown_keys_compactly() {
        let diagnostics = vec![
            "unknown config key ui.mouse_captur; ignoring key".to_string(),
            "unknown config key keys.new_tabb; ignoring key".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml has unknown keys; herdr config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_keeps_mixed_diagnostics_generic() {
        let diagnostics = vec![
            "invalid ui config: invalid type: string; keeping current ui settings".to_string(),
            "unknown config key keys.new_tabb; ignoring key".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml; herdr config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_default_fallback() {
        let diagnostics = vec![
            "config parse error: TOML parse error at line 33, column 8\n   |\n33 | type = \"popup\"\n   |        ^^^^^^^\nunknown variant `popup`; using defaults"
                .to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml invalid; using defaults; herdr config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_unreadable_config_impact() {
        let startup = vec!["config read error: permission denied; using defaults".to_string()];
        assert_eq!(
            config_diagnostic_summary(&startup).as_deref(),
            Some("config.toml unreadable; using defaults; herdr config check")
        );

        let reload =
            vec!["config read error: permission denied; keeping current config".to_string()];
        assert_eq!(
            config_diagnostic_summary(&reload).as_deref(),
            Some("config.toml unreadable; keeping current config; herdr config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_retained_live_config() {
        let diagnostics = vec![
            "config parse error: TOML parse error at line 7, column 4; keeping current config"
                .to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml invalid; keeping current config; herdr config check")
        );
    }

    #[test]
    fn config_loaders_report_unreadable_path() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = std::env::temp_dir().join(format!(
            "herdr-config-unreadable-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&path).unwrap();
        env.set(CONFIG_PATH_ENV_VAR, &path);

        let startup = Config::load();
        assert!(startup
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("config read error")
                && diagnostic.contains("using defaults")));

        let reload = load_live_config().unwrap_err();
        assert!(reload.iter().any(|diagnostic| {
            diagnostic.contains("config read error")
                && diagnostic.contains("keeping current config")
        }));

        drop(env);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn load_live_config_parses_session_section() {
        let loaded = load_live_config_from_str(
            r#"
[session]
resume_agents_on_restore = true
"#,
        )
        .unwrap();

        assert!(loaded.config.session.resume_agents_on_restore);
        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_preserves_day_board_stale_threshold() {
        let loaded = load_live_config_from_str("[day_board]\nstale_after = 90\n")
            .expect("live day board config");

        assert_eq!(loaded.config.day_board.stale_after, 90);
        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_parses_and_clamps_stall_supervision_delays() {
        let loaded = load_live_config_from_str(
            "[session]\nagent_stale_after_minutes = 9223372036854775807\nagent_subagent_stale_after_minutes = 9223372036854775807\nnudge_after_minutes = 7\n",
        )
        .expect("live session config");

        assert_eq!(
            loaded.config.session.agent_stale_after_minutes,
            super::super::MAX_NUDGE_AFTER_MINUTES
        );
        assert_eq!(loaded.config.session.nudge_after_minutes, 7);
        assert_eq!(
            loaded.config.session.agent_subagent_stale_after_minutes,
            super::super::MAX_NUDGE_AFTER_MINUTES
        );
    }

    #[test]
    fn load_live_config_applies_missive_resource_ids_without_a_token_value() {
        let loaded = load_live_config_from_str(
            r#"
[missive]
token_env = "HERDR_TEST_MISSIVE_TOKEN"
team = "team-id"
organization = "organization-id"
"#,
        )
        .expect("live Missive config");

        assert_eq!(loaded.config.missive.token_env, "HERDR_TEST_MISSIVE_TOKEN");
        assert_eq!(loaded.config.missive.team.as_deref(), Some("team-id"));
        assert_eq!(
            loaded.config.missive.organization.as_deref(),
            Some("organization-id")
        );
        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_warns_about_unknown_theme_names() {
        let loaded = load_live_config_from_str(
            r#"
[theme]
name = "catppucin"
"#,
        )
        .unwrap();

        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].contains("theme.name = \"catppucin\""));
    }

    #[test]
    fn load_live_config_warns_about_unknown_top_level_sections() {
        let loaded = load_live_config_from_str(
            r#"
[toast]
delivery = "system"

[ui.toast]
delivery = "herdr"
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [toast]; did you mean [ui.toast]? ignoring section"]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
    }

    #[test]
    fn load_live_config_warns_about_unknown_keys_and_applies_known_siblings() {
        let loaded = load_live_config_from_str(
            r##"
plugin = []

[theme.custom]
accentt = "#ffffff"

[advanced]
scrollback_lines = 42

[keys]
fullscreen = "prefix+z"
new_tabb = "prefix+t"

[[keys.command]]
key = "prefix+g"
command = "git status"
descrption = "status"

[ui]
mouse_capture = false
mouse_captur = true
"foo.bar" = true
"foo.?.bar" = false

[ui.toast]
enabled = true
delivry = "system"

[ui.sidebar.agents.rows_by_agent]
claude = [["terminal_title"]]
"##,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec![
                "unknown config key plugin; ignoring key",
                "unknown config key theme.custom.accentt; ignoring key",
                "unknown config key keys.command.0.descrption; ignoring key",
                "unknown config key keys.new_tabb; ignoring key",
                "unknown config key ui.\"foo.?.bar\"; ignoring key",
                "unknown config key ui.\"foo.bar\"; ignoring key",
                "unknown config key ui.mouse_captur; ignoring key",
                "unknown config key ui.toast.delivry; ignoring key",
            ]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(loaded.config.advanced.scrollback_limit_bytes, 42);
        assert!(!loaded.config.ui.mouse_capture);
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
        assert!(loaded
            .config
            .keybinds()
            .zoom
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+z"));
    }

    #[test]
    fn load_live_config_accepts_legacy_agent_panel_scope_without_warning() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
agent_panel_scope = "current"
agent_panel_sort = "priority"
"#,
        )
        .unwrap();

        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.ui.agent_panel_sort,
            super::super::AgentPanelSortConfig::Priority
        );
    }

    #[test]
    fn load_live_config_reads_the_sidebar_companion_sections() {
        let loaded = load_live_config_from_str(
            r#"
[notepad]
enabled = true
height = 12

[goals_panel]
enabled = false

[pomodoro]
enabled = true
work_minutes = 20
"#,
        )
        .unwrap();

        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
        assert!(loaded.config.notepad.enabled);
        assert_eq!(loaded.config.notepad.height, 12);
        assert!(!loaded.config.goals_panel.enabled);
        assert!(loaded.config.pomodoro.enabled);
        assert_eq!(loaded.config.pomodoro.work_minutes, 20);
    }

    #[test]
    fn load_live_config_discards_ignored_keys_from_an_invalid_section() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
mouse_capture = "yes"
mouse_captur = true
"#,
        )
        .unwrap();

        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].contains("invalid ui config"));
        assert!(!loaded.diagnostics[0].starts_with("unknown config key"));
        assert_eq!(loaded.invalid_sections, vec!["ui"]);
    }

    #[test]
    fn startup_config_accepts_legacy_agent_panel_scope_without_warning() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = std::env::temp_dir().join(format!(
            "herdr-config-legacy-agent-panel-scope-{}.toml",
            crate::config::test_unique_suffix()
        ));
        std::fs::write(&path, "[ui]\nagent_panel_scope = \"all\"\n").unwrap();
        env.set(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        drop(env);
        let _ = std::fs::remove_file(path);

        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    }

    #[test]
    fn startup_config_load_warns_about_unknown_top_level_sections() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = std::env::temp_dir().join(format!(
            "herdr-config-unknown-section-{}.toml",
            crate::config::test_unique_suffix()
        ));
        std::fs::write(
            &path,
            r#"
[[plugin]]
id = "example"

[ui.toast]
delivery = "system"
"#,
        )
        .unwrap();
        env.set(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [[plugin]]; ignoring section"]
        );
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::System
        );

        drop(env);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remove_keybinding_config_sections_removes_keys_tables_only() {
        let content = r#"onboarding = false

[theme]
name = "catppuccin"

[keys]
prefix = "ctrl+a"
new_tab = "c"

[[keys.command]]
key = "g"
command = "lazygit"

[keys.indexed]
tabs = "ctrl"

[ui]
mouse_capture = false
"#;

        let (updated, removed) = remove_keybinding_config_sections(content);

        assert!(removed);
        assert!(updated.contains("onboarding = false"));
        assert!(updated.contains("[theme]\nname = \"catppuccin\""));
        assert!(updated.contains("[ui]\nmouse_capture = false"));
        assert!(!updated.contains("[keys]"));
        assert!(!updated.contains("[[keys.command]]"));
        assert!(!updated.contains("[keys.indexed]"));
        let reset_config: Config = toml::from_str(&updated).expect("reset config");
        let profile = reset_config
            .local_keybindings_profile_toml()
            .expect("reset key profile");
        assert!(profile.contains("next_blocked_window = \"prefix+b\""));
        assert!(profile.contains("toggle_sidebar = \"prefix+shift+b\""));
    }

    #[test]
    fn remove_keybinding_config_sections_reports_noop_without_keys() {
        let content = "[ui]\nmouse_capture = true\n";
        let (updated, removed) = remove_keybinding_config_sections(content);
        assert!(!removed);
        assert_eq!(updated, content);
    }

    #[test]
    fn stale_full_lifecycle_hook_authority_falls_back_to_screen_live_config_section() {
        let valid = load_live_config_from_str(
            "[agent_detection]\nfull_lifecycle_hook_authority_timeout_seconds = 30\n",
        )
        .unwrap();
        assert_eq!(
            valid
                .config
                .agent_detection
                .full_lifecycle_hook_authority_timeout_seconds,
            30
        );
        assert!(valid.invalid_sections.is_empty());

        let invalid = load_live_config_from_str(
            "[agent_detection]\nfull_lifecycle_hook_authority_timeout_seconds = 29\n",
        )
        .unwrap();
        assert_eq!(invalid.invalid_sections, vec!["agent_detection"]);
        assert_eq!(
            invalid
                .config
                .agent_detection
                .full_lifecycle_hook_authority_timeout_seconds,
            600
        );
    }

    #[test]
    fn replace_actions_tables_preserves_every_non_action_byte() {
        let content = "onboarding = false\n\n[[actions]]\nname = \"old\"\ncommand = \"false\"\n\n[ui]\nmouse_capture = true\n";
        let actions = vec![crate::config::ActionConfig {
            name: "test".into(),
            command: "just test".into(),
            key: Some("ctrl-b t".into()),
            run_on_worktree_create: true,
            open_in_bottom_pane: true,
            repo: Some("matthias-scale/herdr".into()),
        }];

        let updated = replace_actions_tables(content, &actions).expect("serialize actions");

        assert!(updated.starts_with("onboarding = false\n\n[[actions]]\n"));
        assert!(updated.ends_with("[ui]\nmouse_capture = true\n"));
        assert!(!updated.contains("name = \"old\""));
        assert!(updated.contains("name = \"test\""));
        assert!(updated.contains("key = \"ctrl-b t\""));
    }

    #[test]
    fn atomic_action_save_round_trips_through_config_loader() {
        let root = std::env::temp_dir().join(format!(
            "herdr-action-save-{}",
            crate::config::test_unique_suffix()
        ));
        let path = root.join("config.toml");
        std::fs::create_dir_all(&root).expect("create fixture root");
        std::fs::write(&path, "onboarding = false\n\n[ui]\nmouse_capture = true\n")
            .expect("seed config");
        let actions = vec![crate::config::ActionConfig {
            name: "lint".into(),
            command: "just lint".into(),
            key: None,
            run_on_worktree_create: false,
            open_in_bottom_pane: true,
            repo: None,
        }];

        write_actions_atomically(&path, &actions).expect("save actions");

        let content = std::fs::read_to_string(&path).expect("read saved config");
        let parsed: Config = toml::from_str(&content).expect("parse saved config");
        assert_eq!(parsed.actions, actions);
        assert!(content.starts_with("onboarding = false\n\n[ui]\nmouse_capture = true\n"));
        assert_eq!(
            std::fs::read_dir(&root)
                .expect("read fixture root")
                .filter_map(Result::ok)
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Every field on `Config` is a top-level section a user can write, so one
    /// missing from the allowlist makes `herdr config check` tell them their
    /// valid config is being ignored. That has now happened twice, so this
    /// reads the struct rather than trusting anyone to update two lists.
    #[test]
    fn every_config_field_is_a_known_top_level_section() {
        let model = include_str!("model.rs");
        let start = model
            .find("pub struct Config {")
            .expect("Config struct in model.rs");
        let body = &model[start..];
        let end = body.find("\n}").expect("end of the Config struct");
        let fields: Vec<&str> = body[..end]
            .lines()
            .skip(1)
            .filter_map(|line| line.trim().strip_prefix("pub "))
            .filter_map(|line| line.split(':').next())
            .collect();
        assert!(fields.len() > 20, "parsed too few fields: {fields:?}");

        let missing: Vec<&&str> = fields
            .iter()
            .filter(|field| !KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(field))
            .collect();
        assert!(
            missing.is_empty(),
            "config sections missing from KNOWN_TOP_LEVEL_CONFIG_KEYS: {missing:?}"
        );
    }
}
