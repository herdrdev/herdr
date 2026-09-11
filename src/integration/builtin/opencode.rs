use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::agents::integration::IntegrationAdapter;
use crate::integration::opencode_dir;
use crate::integration::parse_integration_version;
use crate::integration::{install_opencode, uninstall_opencode};

pub(crate) const PLUGIN_INSTALL_NAME: &str = "herdr-agent-state.js";
pub(crate) const PLUGIN_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/opencode/assets/herdr-agent-state.js"
));
pub(crate) const TUI_PLUGIN_INSTALL_NAME: &str = "herdr-tui-session.js";
pub(crate) const TUI_PLUGIN_SPEC: &str = "./herdr-tui-session.js";
pub(crate) const TUI_PLUGIN_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/opencode/assets/herdr-tui-session.js"
));

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path)
        .with_current_install_extra_validator(tui_integration_is_valid);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_opencode()?;
    Ok(vec![
        format!(
            "installed opencode integration plugin to {}",
            installed.plugin_path.display()
        ),
        format!(
            "installed opencode tui integration plugin to {}",
            installed.tui_plugin_path.display()
        ),
        format!(
            "ensured opencode tui plugin config at {}",
            installed.tui_config_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_opencode()?;
    let mut messages = vec![if result.removed_plugin {
        format!(
            "removed opencode integration plugin at {}",
            result.plugin_path.display()
        )
    } else {
        format!(
            "no opencode integration plugin found at {}",
            result.plugin_path.display()
        )
    }];
    messages.push(if result.removed_tui_plugin {
        format!(
            "removed opencode tui integration plugin at {}",
            result.tui_plugin_path.display()
        )
    } else {
        format!(
            "no opencode tui integration plugin found at {}",
            result.tui_plugin_path.display()
        )
    });
    if result.updated_tui_config {
        messages.push(format!(
            "removed herdr opencode plugin entry from {}",
            result.tui_config_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    opencode_dir().map(|dir| dir.join("plugins").join(PLUGIN_INSTALL_NAME))
}

fn tui_integration_is_valid(plugin_path: &Path, expected_version: u32) -> bool {
    let Some(config_dir) = plugin_path.parent().and_then(Path::parent) else {
        return false;
    };
    let tui_plugin_path = config_dir.join(TUI_PLUGIN_INSTALL_NAME);
    let tui_plugin_current = fs::read_to_string(tui_plugin_path)
        .ok()
        .and_then(|content| parse_integration_version(&content))
        .is_some_and(|version| version >= expected_version);
    tui_plugin_current && crate::integration::tui_plugin_is_configured(config_dir, TUI_PLUGIN_SPEC)
}
