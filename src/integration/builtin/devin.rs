use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::devin_dir;
use crate::integration::{install_devin, uninstall_devin};

pub(super) const HOOK_INSTALL_NAME_UNIX: &str = "herdr-agent-state.sh";
pub(super) const HOOK_INSTALL_NAME_WINDOWS: &str = "herdr-agent-state.ps1";
pub(crate) const HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    HOOK_INSTALL_NAME_WINDOWS
} else {
    HOOK_INSTALL_NAME_UNIX
};
pub(crate) const HOOK_ASSET: &str = if cfg!(windows) {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/devin/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/devin/assets/herdr-agent-state.sh"
    ))
};
pub(crate) const HOOK_EVENTS: [(&str, &str); 6] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "session"),
    ("PreToolUse", "session"),
    ("PostToolUse", "session"),
    ("PermissionRequest", "session"),
    ("Stop", "session"),
];
pub(crate) const REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_devin()?;
    Ok(vec![
        format!(
            "installed devin integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "ensured devin settings at {}",
            installed.settings_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_devin()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed devin hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no devin hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_settings {
        messages.push(format!(
            "removed herdr devin hook entries from {}",
            result.settings_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr devin hook entries found in {}",
            result.settings_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    devin_dir().map(|dir| dir.join(HOOK_INSTALL_NAME))
}
