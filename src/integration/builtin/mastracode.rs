use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::mastracode_dir;
use crate::integration::{install_mastracode, uninstall_mastracode};

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
        "/vendor/agent-registry/agents/mastracode/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/mastracode/assets/herdr-agent-state.sh"
    ))
};
pub(crate) const HOOK_TIMEOUT_MS: u64 = 10_000;
pub(crate) const REMOVED_HOOK_EVENTS: [(&str, &str); 2] =
    [("SessionStart", "idle"), ("SessionEnd", "release")];
pub(crate) const HOOK_EVENTS: [(&str, &str); 11] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "working"),
    ("AgentStart", "working"),
    ("PreToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("PermissionResult", "working"),
    ("SubagentStart", "working"),
    ("SubagentEnd", "working"),
    ("Interrupt", "idle"),
    ("AgentEnd", "idle"),
    ("Stop", "idle"),
];

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_mastracode()?;
    Ok(vec![
        format!(
            "installed mastracode integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "ensured mastracode hooks at {}",
            installed.hooks_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_mastracode()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed mastracode hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no mastracode hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_hooks {
        messages.push(format!(
            "removed herdr mastracode hook entries from {}",
            result.hooks_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr mastracode hook entries found in {}",
            result.hooks_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    mastracode_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}
