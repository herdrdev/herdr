use std::io;
use std::path::PathBuf;

use crate::agents::integration::{IntegrationAdapter, IntegrationProfile};
use crate::integration::droid_dir;
use crate::integration::{install_droid, uninstall_droid};

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
        "/vendor/agent-registry/agents/droid/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/droid/assets/herdr-agent-state.sh"
    ))
};
pub(crate) const HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
pub(crate) const REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 9] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("Notification", "blocked"),
    ("Stop", "idle"),
    ("SubagentStop", "working"),
    ("PreCompact", "working"),
    ("SessionEnd", "release"),
];

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter(profile: &IntegrationProfile) -> io::Result<Vec<String>> {
    let installed = install_droid(profile)?;
    let mut messages = vec![
        format!(
            "installed droid integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "ensured droid hooks at {}",
            installed.settings_path.display()
        ),
    ];
    if installed.updated_legacy_hooks {
        messages.push(format!(
            "removed legacy herdr droid hook entries from {}",
            installed.hooks_path.display()
        ));
    }
    Ok(messages)
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_droid()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed droid hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no droid hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_hooks {
        messages.push(format!(
            "removed legacy herdr droid hook entries from {}",
            result.hooks_path.display()
        ));
    } else {
        messages.push(format!(
            "no legacy herdr droid hook entries found in {}",
            result.hooks_path.display()
        ));
    }
    if result.updated_settings {
        messages.push(format!(
            "removed herdr droid hook entries from {}",
            result.settings_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr droid hook entries found in {}",
            result.settings_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    droid_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}
