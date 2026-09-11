use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::ParseOptions;
use serde_json::Value;

const TUI_CONFIG_NAME: &str = "tui.jsonc";

pub(crate) fn tui_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(TUI_CONFIG_NAME)
}

pub(crate) fn validate_tui_plugin_config(config_dir: &Path) -> io::Result<()> {
    validate_plugin_config(&tui_config_path(config_dir), "plugin")?;
    validate_plugin_config(&config_dir.join("cli.json"), "plugins")
}

fn validate_plugin_config(config_path: &Path, key: &str) -> io::Result<()> {
    if !config_path.is_file() {
        return Ok(());
    }

    let content = fs::read_to_string(config_path)?;
    let root = parse_root(&content, config_path)?;
    let object = root_object(&root, config_path)?;
    if object
        .get(key)
        .is_some_and(|property| property.array_value().is_none())
    {
        return Err(invalid_plugin_list(config_path));
    }
    Ok(())
}

pub(crate) fn add_tui_plugin(config_dir: &Path, plugin_spec: &str) -> io::Result<PathBuf> {
    add_plugin(tui_config_path(config_dir), "plugin", plugin_spec)
}

pub(crate) fn add_cli_plugin(
    config_dir: &Path,
    state_dir: &Path,
    plugin_spec: &str,
) -> io::Result<Option<PathBuf>> {
    let path = config_dir.join("cli.json");
    // OpenCode imports V1 TUI preferences (`tui.json`, `kv.json`) into cli.json on
    // its first V2 start, but only while cli.json is absent. Defer registration
    // while those sources still exist so we do not skip the migration; otherwise
    // create cli.json ourselves, since OpenCode will never do it for a fresh V2
    // install with nothing to migrate.
    if !path.is_file() && cli_migration_pending(config_dir, state_dir) {
        return Ok(None);
    }
    add_plugin(path, "plugins", plugin_spec).map(Some)
}

fn cli_migration_pending(config_dir: &Path, state_dir: &Path) -> bool {
    config_dir.join("tui.json").is_file() || state_dir.join("kv.json").is_file()
}

fn add_plugin(config_path: PathBuf, key: &str, plugin_spec: &str) -> io::Result<PathBuf> {
    let content = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        "{}\n".to_string()
    };
    let root = parse_root(&content, &config_path)?;
    let object = root_object(&root, &config_path)?;

    match object.get(key) {
        Some(property) => {
            let plugins = property
                .array_value()
                .ok_or_else(|| invalid_plugin_list(&config_path))?;
            if plugins.elements().iter().any(|entry| {
                entry
                    .to_serde_value()
                    .is_some_and(|entry| plugin_entry_matches(&entry, plugin_spec))
            }) {
                return Ok(config_path);
            }
            plugins.append(CstInputValue::String(plugin_spec.to_string()));
        }
        None => {
            object.append(
                key,
                CstInputValue::Array(vec![CstInputValue::String(plugin_spec.to_string())]),
            );
        }
    }

    fs::write(&config_path, root.to_string())?;
    Ok(config_path)
}

pub(crate) fn remove_tui_plugin(config_dir: &Path, plugin_spec: &str) -> io::Result<bool> {
    remove_plugin(&tui_config_path(config_dir), "plugin", plugin_spec)
}

pub(crate) fn remove_cli_plugin(config_dir: &Path, plugin_spec: &str) -> io::Result<bool> {
    remove_plugin(&config_dir.join("cli.json"), "plugins", plugin_spec)
}

fn remove_plugin(config_path: &Path, key: &str, plugin_spec: &str) -> io::Result<bool> {
    if !config_path.is_file() {
        return Ok(false);
    }

    let content = fs::read_to_string(config_path)?;
    let root = parse_root(&content, config_path)?;
    let object = root_object(&root, config_path)?;
    let Some(property) = object.get(key) else {
        return Ok(false);
    };
    let plugins = property
        .array_value()
        .ok_or_else(|| invalid_plugin_list(config_path))?;
    let mut removed = false;
    for entry in plugins.elements() {
        if entry
            .to_serde_value()
            .is_some_and(|entry| plugin_entry_matches(&entry, plugin_spec))
        {
            entry.remove();
            removed = true;
        }
    }
    if !removed {
        return Ok(false);
    }
    if plugins.elements().is_empty() {
        property.remove();
    }

    fs::write(config_path, root.to_string())?;
    Ok(true)
}

pub(crate) fn tui_plugin_is_configured(config_dir: &Path, plugin_spec: &str) -> bool {
    plugin_is_configured(&tui_config_path(config_dir), "plugin", plugin_spec)
}

pub(crate) fn cli_plugin_is_configured(config_dir: &Path, plugin_spec: &str) -> bool {
    plugin_is_configured(&config_dir.join("cli.json"), "plugins", plugin_spec)
}

fn plugin_is_configured(config_path: &Path, key: &str, plugin_spec: &str) -> bool {
    let Ok(content) = fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(root) = parse_root(&content, config_path) else {
        return false;
    };
    let Ok(object) = root_object(&root, config_path) else {
        return false;
    };
    object
        .get(key)
        .and_then(|property| property.array_value())
        .is_some_and(|plugins| {
            plugins.elements().iter().any(|entry| {
                entry
                    .to_serde_value()
                    .is_some_and(|entry| plugin_entry_matches(&entry, plugin_spec))
            })
        })
}

fn parse_root(content: &str, path: &Path) -> io::Result<CstRootNode> {
    CstRootNode::parse(content, &jsonc_parse_options()).map_err(|err| {
        io::Error::other(format!(
            "failed to parse OpenCode TUI config at {}: {err}",
            path.display()
        ))
    })
}

fn root_object(root: &CstRootNode, path: &Path) -> io::Result<jsonc_parser::cst::CstObject> {
    root.value()
        .and_then(|value| value.as_object())
        .ok_or_else(|| invalid_root(path))
}

fn jsonc_parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

fn plugin_entry_matches(entry: &Value, plugin_spec: &str) -> bool {
    entry.as_str() == Some(plugin_spec)
        || entry.get("package").and_then(Value::as_str) == Some(plugin_spec)
        || entry
            .as_array()
            .and_then(|parts| parts.first())
            .and_then(Value::as_str)
            == Some(plugin_spec)
}

fn invalid_root(path: &Path) -> io::Error {
    io::Error::other(format!(
        "OpenCode TUI config at {} must be a JSON object",
        path.display()
    ))
}

fn invalid_plugin_list(path: &Path) -> io::Error {
    io::Error::other(format!(
        "OpenCode TUI config plugin list at {} must be an array",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn unique_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-opencode-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("temporary config directory should be created");
        dir
    }

    fn parse_config(path: &Path) -> Value {
        let content = fs::read_to_string(path).unwrap();
        parse_root(&content, path)
            .unwrap()
            .value()
            .and_then(|value| value.to_serde_value())
            .unwrap()
    }

    #[test]
    fn add_and_remove_tui_plugin_preserves_jsonc_config() {
        let dir = unique_dir();
        let config_path = dir.join(TUI_CONFIG_NAME);
        fs::write(
            &config_path,
            concat!(
                "{\n",
                "  // Keep this comment.\n",
                "  \"theme\": \"system\",\n",
                "  \"plugin\": [\"example\", [\"configured\", {\"enabled\": true}]],\n",
                "}\n",
            ),
        )
        .unwrap();

        add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();
        add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();
        let installed_content = fs::read_to_string(&config_path).unwrap();
        assert!(installed_content.contains("// Keep this comment."));
        let installed = parse_config(&config_path);
        assert_eq!(installed["theme"], "system");
        assert_eq!(
            installed["plugin"],
            json!([
                "example",
                ["configured", {"enabled": true}],
                "./herdr-tui-state.js"
            ])
        );

        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());
        let removed_content = fs::read_to_string(&config_path).unwrap();
        assert!(removed_content.contains("// Keep this comment."));
        let removed = parse_config(&config_path);
        assert_eq!(removed["theme"], "system");
        assert_eq!(
            removed["plugin"],
            json!(["example", ["configured", {"enabled": true}]])
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn managed_jsonc_leaves_opencode_migration_target_absent() {
        let dir = unique_dir();
        let legacy_config_path = dir.join("opencode.json");
        let legacy_config = "{\n  \"theme\": \"system\"\n}\n";
        fs::write(&legacy_config_path, legacy_config).unwrap();

        let config_path = add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();

        assert_eq!(config_path, dir.join("tui.jsonc"));
        assert!(!dir.join("tui.json").exists());
        assert_eq!(
            fs::read_to_string(legacy_config_path).unwrap(),
            legacy_config
        );
        assert_eq!(
            parse_config(&config_path),
            json!({ "plugin": ["./herdr-tui-state.js"] })
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_tui_plugin_leaves_empty_managed_config() {
        let dir = unique_dir();
        let config_path = add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();

        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());
        assert!(config_path.is_file());
        assert_eq!(parse_config(&config_path), json!({}));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configured_tui_plugin_accepts_option_tuple() {
        let dir = unique_dir();
        fs::write(
            dir.join(TUI_CONFIG_NAME),
            r#"{"plugin":[["./herdr-tui-state.js",{"enabled":true}]]}"#,
        )
        .unwrap();

        assert!(tui_plugin_is_configured(&dir, "./herdr-tui-state.js"));
        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cli_registration_preserves_options_and_other_preferences() {
        let dir = unique_dir();
        let state = unique_dir();
        let path = dir.join("cli.json");
        fs::write(&path, r#"{"theme":{"name":"catppuccin"},"plugins":[{"package":"./herdr-opencode","options":{"custom":true}},"example"]}"#).unwrap();
        add_cli_plugin(&dir, &state, "./herdr-opencode").unwrap();
        assert!(cli_plugin_is_configured(&dir, "./herdr-opencode"));
        assert_eq!(parse_config(&path)["plugins"].as_array().unwrap().len(), 2);
        assert_eq!(parse_config(&path)["plugins"][0]["options"]["custom"], true);
        assert!(remove_cli_plugin(&dir, "./herdr-opencode").unwrap());
        assert_eq!(parse_config(&path)["plugins"], json!(["example"]));
        assert_eq!(parse_config(&path)["theme"]["name"], "catppuccin");
        add_cli_plugin(&dir, &state, "./herdr-opencode").unwrap();
        add_cli_plugin(&dir, &state, "./herdr-opencode").unwrap();
        assert_eq!(
            parse_config(&path)["plugins"],
            json!(["example", "./herdr-opencode"])
        );
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn cli_registration_creates_missing_config_when_no_migration_pending() {
        let dir = unique_dir();
        let state = unique_dir();
        let path = add_cli_plugin(&dir, &state, "./herdr-opencode")
            .unwrap()
            .expect("cli.json should be created when OpenCode has nothing to migrate");
        assert_eq!(path, dir.join("cli.json"));
        assert_eq!(
            parse_config(&path),
            json!({ "plugins": ["./herdr-opencode"] })
        );
        assert!(cli_plugin_is_configured(&dir, "./herdr-opencode"));
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn cli_registration_defers_while_migration_pending() {
        let dir = unique_dir();
        let state = unique_dir();
        fs::write(dir.join("tui.json"), "{}").unwrap();
        assert!(add_cli_plugin(&dir, &state, "./herdr-opencode")
            .unwrap()
            .is_none());
        assert!(!dir.join("cli.json").exists());

        fs::remove_file(dir.join("tui.json")).unwrap();
        fs::write(state.join("kv.json"), "{}").unwrap();
        assert!(add_cli_plugin(&dir, &state, "./herdr-opencode")
            .unwrap()
            .is_none());
        assert!(!dir.join("cli.json").exists());

        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn invalid_cli_plugin_list_fails_preflight() {
        let dir = unique_dir();
        fs::write(dir.join("cli.json"), r#"{"plugins":{}}"#).unwrap();
        assert!(validate_tui_plugin_config(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
