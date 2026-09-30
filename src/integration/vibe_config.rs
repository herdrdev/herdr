//! Management of the Mistral Vibe CLI's user-level `hooks.toml`.
//!
//! Vibe merges `~/.vibe/hooks.toml` with project-level hook files and has no
//! per-directory hook merge, so Herdr entries live inside the user's file.
//! Edits are name-based and comment-preserving: entries Herdr owns are
//! identified by their hook name, everything else in the file is untouched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

use super::command::hook_command;
use super::config_file::{check_config_target, write_config};
use super::{VIBE_HOOK_EVENTS, VIBE_HOOK_TIMEOUT_SEC};

const HOOKS_CONFIG_NAME: &str = "hooks.toml";
const HOOKS_KEY: &str = "hooks";
const MATCH_ANY: &str = "*";

pub(crate) fn hooks_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(HOOKS_CONFIG_NAME)
}

/// A Herdr-owned hook registration exactly as it must appear in `hooks.toml`.
pub(crate) struct VibeHookDefinition {
    pub name: &'static str,
    pub hook_type: &'static str,
    pub matcher: Option<&'static str>,
    pub command: String,
}

/// The complete set of Herdr-owned hook entries. Installation and status
/// validation share this builder so any drift reports as outdated.
pub(crate) fn hook_definitions(hook_path: &Path) -> Vec<VibeHookDefinition> {
    VIBE_HOOK_EVENTS
        .iter()
        .map(|(name, hook_type, matcher, action)| VibeHookDefinition {
            name,
            hook_type,
            matcher: *matcher,
            command: hook_command(hook_path, Some(action)),
        })
        .collect()
}

/// Append the Herdr-owned hook entries, replacing earlier entries that use the
/// same names. User hooks, comments, and formatting elsewhere are preserved.
pub(crate) fn add_hooks(
    config_dir: &Path,
    definitions: &[VibeHookDefinition],
) -> io::Result<PathBuf> {
    let path = hooks_config_path(config_dir);
    check_config_target(&path)?;
    let content = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let mut document = parse_hooks_config(&content, &path)?;
    let hooks = ensure_hooks_array(&mut document, &path)?;
    remove_owned_entries(hooks, definitions);
    for definition in definitions {
        hooks.push(hook_entry(definition));
    }
    write_config(&path, document.to_string())?;
    Ok(path)
}

/// Remove every Herdr-owned hook entry. Returns the config path and how many
/// entries were removed; a missing file removes nothing.
pub(crate) fn remove_hooks(
    config_dir: &Path,
    definitions: &[VibeHookDefinition],
) -> io::Result<(PathBuf, usize)> {
    let path = hooks_config_path(config_dir);
    if !path.is_file() {
        return Ok((path, 0));
    }
    check_config_target(&path)?;
    let content = fs::read_to_string(&path)?;
    let mut document = parse_hooks_config(&content, &path)?;
    let hooks = ensure_hooks_array(&mut document, &path)?;
    let before_removal = hooks.len();
    remove_owned_entries(hooks, definitions);
    let removed = before_removal - hooks.len();
    write_config(&path, document.to_string())?;
    Ok((path, removed))
}

/// Whether every Herdr-owned hook entry is present with the exact type,
/// matcher, and command the installer writes. A stale timeout or extra keys
/// do not affect hook delivery, so they are not part of validity.
pub(crate) fn hooks_are_configured(config_dir: &Path, definitions: &[VibeHookDefinition]) -> bool {
    let path = hooks_config_path(config_dir);
    let Ok(content) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(document) = content.parse::<DocumentMut>() else {
        return false;
    };
    let Some(hooks) = document.get(HOOKS_KEY).and_then(Item::as_array_of_tables) else {
        return false;
    };
    definitions
        .iter()
        .all(|definition| hooks.iter().any(|entry| entry_matches(entry, definition)))
}

fn hook_entry(definition: &VibeHookDefinition) -> Table {
    let mut entry = Table::new();
    entry.insert("name", value(definition.name));
    entry.insert("type", value(definition.hook_type));
    if let Some(matcher) = definition.matcher {
        entry.insert("match", value(matcher));
    }
    entry.insert("command", value(definition.command.as_str()));
    entry.insert("timeout", value(VIBE_HOOK_TIMEOUT_SEC));
    entry
}

fn remove_owned_entries(hooks: &mut ArrayOfTables, definitions: &[VibeHookDefinition]) {
    hooks.retain(|entry| {
        let name = entry.get("name").and_then(Item::as_str);
        !definitions
            .iter()
            .any(|definition| Some(definition.name) == name)
    });
}

fn entry_matches(entry: &Table, definition: &VibeHookDefinition) -> bool {
    if entry.get("name").and_then(Item::as_str) != Some(definition.name) {
        return false;
    }
    if entry.get("type").and_then(Item::as_str) != Some(definition.hook_type) {
        return false;
    }
    if entry.get("command").and_then(Item::as_str) != Some(definition.command.as_str()) {
        return false;
    }
    match definition.matcher {
        Some(matcher) => entry.get("match").and_then(Item::as_str) == Some(matcher),
        None => matches!(
            entry.get("match").and_then(Item::as_str),
            None | Some(MATCH_ANY)
        ),
    }
}

fn ensure_hooks_array<'a>(
    document: &'a mut DocumentMut,
    path: &Path,
) -> io::Result<&'a mut ArrayOfTables> {
    if document.get(HOOKS_KEY).is_none() {
        document.insert(HOOKS_KEY, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    document
        .get_mut(HOOKS_KEY)
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| invalid_hooks_list(path))
}

fn parse_hooks_config(content: &str, path: &Path) -> io::Result<DocumentMut> {
    content.parse::<DocumentMut>().map_err(|err| {
        io::Error::other(format!(
            "failed to parse Vibe hooks config at {}: {err}",
            path.display()
        ))
    })
}

fn invalid_hooks_list(path: &Path) -> io::Error {
    io::Error::other(format!(
        "Vibe hooks config at {} must declare hooks as [[hooks]] tables",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-vibe-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("temporary config directory should be created");
        dir
    }

    fn definitions() -> Vec<VibeHookDefinition> {
        let dir = unique_dir();
        let definitions = hook_definitions(&dir.join("herdr").join("herdr-agent-state.sh"));
        fs::remove_dir_all(dir).unwrap();
        definitions
    }

    #[test]
    fn add_and_remove_hooks_preserve_user_entries_and_comments() {
        let dir = unique_dir();
        let config_path = dir.join(HOOKS_CONFIG_NAME);
        fs::write(
            &config_path,
            concat!(
                "# Keep this comment.\n",
                "[[hooks]]\n",
                "name = \"lint\"\n",
                "type = \"post_agent\"\n",
                "command = \"eslint --quiet .\"\n",
                "\n",
                "[[hooks]]\n",
                "name = \"herdr-agent-state-working\"\n",
                "type = \"pre_tool\"\n",
                "match = \"*\"\n",
                "command = \"echo stale\"\n",
            ),
        )
        .unwrap();
        let definitions = definitions();

        let registered = add_hooks(&dir, &definitions).unwrap();
        assert_eq!(registered, config_path);
        let installed = fs::read_to_string(&config_path).unwrap();
        assert!(installed.contains("# Keep this comment."));
        assert!(installed.contains("eslint --quiet ."));
        assert!(!installed.contains("echo stale"));
        assert!(installed.contains("command = \"bash "));
        let parsed: DocumentMut = installed.parse().unwrap();
        let names: Vec<&str> = parsed[HOOKS_KEY]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .map(|entry| entry.get("name").and_then(Item::as_str).unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "lint",
                "herdr-agent-state-blocked",
                "herdr-agent-state-working",
                "herdr-agent-state-tool-working",
                "herdr-agent-state-idle",
            ]
        );
        assert!(hooks_are_configured(&dir, &definitions));

        let (removed_path, removed) = remove_hooks(&dir, &definitions).unwrap();
        assert_eq!(removed_path, config_path);
        assert_eq!(removed, definitions.len());
        let remaining = fs::read_to_string(&config_path).unwrap();
        assert!(remaining.contains("# Keep this comment."));
        assert!(remaining.contains("eslint --quiet ."));
        assert!(!remaining.contains("herdr-agent-state"));
        let parsed: DocumentMut = remaining.parse().unwrap();
        let hooks = parsed[HOOKS_KEY].as_array_of_tables().unwrap();
        assert_eq!(hooks.len(), 1);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reinstall_is_idempotent() {
        let dir = unique_dir();
        let definitions = definitions();

        add_hooks(&dir, &definitions).unwrap();
        let first = fs::read_to_string(dir.join(HOOKS_CONFIG_NAME)).unwrap();
        add_hooks(&dir, &definitions).unwrap();
        let second = fs::read_to_string(dir.join(HOOKS_CONFIG_NAME)).unwrap();
        assert_eq!(first, second);
        let parsed: DocumentMut = second.parse().unwrap();
        assert_eq!(parsed[HOOKS_KEY].as_array_of_tables().unwrap().len(), 4);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_hooks_reports_invalid_hooks_list() {
        let dir = unique_dir();
        fs::write(dir.join(HOOKS_CONFIG_NAME), "hooks = \"not a list\"\n").unwrap();
        let err = add_hooks(&dir, &definitions()).unwrap_err();
        assert!(err.to_string().contains("[[hooks]] tables"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_hooks_reports_unparsable_config() {
        let dir = unique_dir();
        fs::write(dir.join(HOOKS_CONFIG_NAME), "not toml\n").unwrap();
        let err = add_hooks(&dir, &definitions()).unwrap_err();
        assert!(err.to_string().contains("failed to parse"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hooks_are_configured_rejects_drift() {
        let dir = unique_dir();
        let definitions = definitions();

        assert!(!hooks_are_configured(&dir, &definitions));

        add_hooks(&dir, &definitions).unwrap();
        assert!(hooks_are_configured(&dir, &definitions));

        // A user-rewritten command never reaches the hook script, so the
        // install must report outdated rather than current.
        let config_path = dir.join(HOOKS_CONFIG_NAME);
        let content = fs::read_to_string(&config_path).unwrap();
        fs::write(
            &config_path,
            content.replace(
                &format!(
                    "\"{}\"",
                    definitions
                        .iter()
                        .map(|definition| definition.command.as_str())
                        .find(|command| command.ends_with("idle"))
                        .unwrap()
                ),
                "\"echo idle\"",
            ),
        )
        .unwrap();
        assert!(!hooks_are_configured(&dir, &definitions));

        fs::remove_dir_all(dir).unwrap();
    }
}
