use std::io;
use std::path::PathBuf;

use crate::agents::integration::{IntegrationAdapter, IntegrationProfile};
use crate::integration::copilot_dir;
use crate::integration::{install_copilot, uninstall_copilot};

pub(super) const HOOK_INSTALL_NAME_UNIX: &str = "herdr-agent-state.sh";
pub(super) const HOOK_INSTALL_NAME_WINDOWS: &str = "herdr-agent-state.ps1";
pub(crate) const HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    HOOK_INSTALL_NAME_WINDOWS
} else {
    HOOK_INSTALL_NAME_UNIX
};
#[cfg(test)]
pub(crate) const HOOK_ASSET: &str = if cfg!(windows) {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/copilot/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/copilot/assets/herdr-agent-state.sh"
    ))
};
pub(crate) const HOOK_EVENTS: [&str; 1] = ["SessionStart"];
pub(crate) const REMOVED_LIFECYCLE_HOOK_EVENTS: [&str; 9] = [
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "agentStop",
    "SessionEnd",
    "notification",
    "sessionStart",
];

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter(profile: &IntegrationProfile) -> io::Result<Vec<String>> {
    let installed = install_copilot(profile)?;
    Ok(vec![
        format!(
            "installed copilot integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "ensured copilot settings at {}",
            installed.settings_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_copilot()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed copilot hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no copilot hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_settings {
        messages.push(format!(
            "removed herdr copilot hook entries from {}",
            result.settings_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr copilot hook entries found in {}",
            result.settings_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    copilot_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}
