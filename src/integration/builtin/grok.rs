use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::agents::integration::{IntegrationAdapter, IntegrationProfile};
use crate::integration::grok_dir;
use crate::integration::{grok_hook_config, install_grok, uninstall_grok};

pub(super) const HOOK_INSTALL_NAME_UNIX: &str = "herdr-agent-state.sh";
pub(super) const HOOK_INSTALL_NAME_WINDOWS: &str = "herdr-agent-state.ps1";
pub(crate) const HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    HOOK_INSTALL_NAME_WINDOWS
} else {
    HOOK_INSTALL_NAME_UNIX
};
pub(crate) const HOOK_CONFIG_INSTALL_NAME: &str = "herdr.json";
#[cfg(test)]
pub(crate) const HOOK_ASSET: &str = if cfg!(windows) {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/grok/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/grok/assets/herdr-agent-state.sh"
    ))
};

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path)
        .with_current_install_extra_validator(hook_config_is_valid);

fn install_adapter(profile: &IntegrationProfile) -> io::Result<Vec<String>> {
    let installed = install_grok(profile)?;
    Ok(vec![
        format!(
            "installed grok integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "registered grok hook config at {}",
            installed.config_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_grok()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed grok hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no grok hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.removed_config_file {
        messages.push(format!(
            "removed grok hook config at {}",
            result.config_path.display()
        ));
    } else {
        messages.push(format!(
            "no grok hook config found at {}",
            result.config_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    grok_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}

fn hook_config_is_valid(hook_path: &Path, _expected_version: u32) -> bool {
    let Some(hooks_dir) = hook_path.parent() else {
        return false;
    };
    let config_path = hooks_dir.join(HOOK_CONFIG_INSTALL_NAME);
    fs::read_to_string(config_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .is_some_and(|config| config == grok_hook_config(hook_path))
}
