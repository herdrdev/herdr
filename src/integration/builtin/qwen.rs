use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::qwen_dir;
use crate::integration::{install_qwen, uninstall_qwen};

pub(super) const HOOK_INSTALL_NAME_UNIX: &str = "herdr-agent-session.sh";
pub(super) const HOOK_INSTALL_NAME_WINDOWS: &str = "herdr-agent-session.ps1";
pub(crate) const HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    HOOK_INSTALL_NAME_WINDOWS
} else {
    HOOK_INSTALL_NAME_UNIX
};
pub(crate) const HOOK_ASSET: &str = if cfg!(windows) {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/qwen/assets/herdr-agent-session.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/qwen/assets/herdr-agent-session.sh"
    ))
};
pub(crate) const HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_qwen()?;
    Ok(vec![
        format!(
            "installed qwen integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "ensured qwen settings at {}",
            installed.settings_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_qwen()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed qwen hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no qwen hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_settings {
        messages.push(format!(
            "removed herdr qwen hook entries from {}",
            result.settings_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr qwen hook entries found in {}",
            result.settings_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    qwen_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}
