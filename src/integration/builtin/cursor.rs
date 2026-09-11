use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::cursor_dir;
use crate::integration::{install_cursor, uninstall_cursor};

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
        "/vendor/agent-registry/agents/cursor/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/cursor/assets/herdr-agent-state.sh"
    ))
};

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_cursor()?;
    Ok(vec![
        format!(
            "installed cursor integration hook to {}",
            installed.hook_path.display()
        ),
        format!("updated cursor hooks at {}", installed.hooks_path.display()),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_cursor()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed cursor hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no cursor hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_hooks {
        messages.push(format!(
            "removed herdr cursor hook entries from {}",
            result.hooks_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr cursor hook entries found in {}",
            result.hooks_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    cursor_dir().map(|dir| dir.join(HOOK_INSTALL_NAME))
}
