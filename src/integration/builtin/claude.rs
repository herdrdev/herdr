use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::claude_dir;
use crate::integration::{install_claude, uninstall_claude};

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
        "/vendor/agent-registry/agents/claude/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/claude/assets/herdr-agent-state.sh"
    ))
};

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_claude()?;
    Ok(vec![
        format!(
            "installed claude integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "ensured claude settings at {}",
            installed.settings_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_claude()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed claude hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no claude hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_settings {
        messages.push(format!(
            "removed herdr claude hook entries from {}",
            result.settings_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr claude hook entries found in {}",
            result.settings_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    claude_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}
