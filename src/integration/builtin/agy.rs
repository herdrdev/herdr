use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::antigravity_cli_dir;
use crate::integration::{install_antigravity_cli, uninstall_antigravity_cli};

pub(super) const HOOK_INSTALL_NAME_UNIX: &str = "herdr-agent-state.sh";
pub(super) const HOOK_INSTALL_NAME_WINDOWS: &str = "herdr-agent-state.ps1";
pub(crate) const HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    HOOK_INSTALL_NAME_WINDOWS
} else {
    HOOK_INSTALL_NAME_UNIX
};
#[cfg(windows)]
pub(crate) const HOOK_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/agy/assets/herdr-agent-state.ps1"
));
#[cfg(not(windows))]
pub(crate) const HOOK_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/agy/assets/herdr-agent-state.sh"
));
/// Antigravity CLI keys `hooks.json` by hook name, so every Herdr entry lives
/// under one Herdr-owned block that install rewrites and uninstall removes.
pub(crate) const HOOK_BLOCK_NAME: &str = "herdr";
pub(crate) const HOOK_TIMEOUT_SEC: u64 = 10;
/// `(event, reported action)`. Session-only: `PreInvocation` is the only event
/// we need because it carries `conversationId`. The others cannot express
/// lifecycle safely — Antigravity CLI has no blocked event, `PostInvocation` is
/// skipped on interruption, and `Stop` is end-of-turn rather than process exit.
/// Screen detection owns agent state instead.
///
/// `PreInvocation` takes a flat handler list; only the `PreToolUse`/`PostToolUse`
/// events accept a `matcher`/`hooks` wrapper, and sending one here would
/// invalidate the whole file.
pub(crate) const HOOK_EVENTS: [(&str, &str); 1] = [("PreInvocation", "session")];

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_antigravity_cli()?;
    Ok(vec![
        format!(
            "installed antigravity-cli integration hook to {}",
            installed.hook_path.display()
        ),
        format!(
            "ensured antigravity-cli hooks at {}",
            installed.hooks_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_antigravity_cli()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed antigravity-cli hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no antigravity-cli hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_hooks {
        messages.push(format!(
            "removed herdr antigravity-cli hook entries from {}",
            result.hooks_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr antigravity-cli hook entries found in {}",
            result.hooks_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    antigravity_cli_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}
