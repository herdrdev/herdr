use std::io;
use std::path::PathBuf;

use crate::agents::integration::AgentVersionRequirement;
use crate::agents::integration::{IntegrationAdapter, IntegrationProfile};
use crate::integration::kimi_dir;
use crate::integration::{install_kimi, uninstall_kimi};

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
        "/vendor/agent-registry/agents/kimi/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/kimi/assets/herdr-agent-state.sh"
    ))
};
pub(crate) const CONFIG_BLOCK_BEGIN: &str = "# >>> herdr kimi integration";
pub(crate) const CONFIG_BLOCK_END: &str = "# <<< herdr kimi integration";
pub(crate) const MIN_VERSION: &str = "0.14.0";
pub(crate) const ASK_USER_QUESTION_MATCHER: &str = "^AskUserQuestion$";
pub(crate) const OTHER_TOOL_MATCHER: &str = "^(?!AskUserQuestion$).*$";
pub(crate) const HOOK_EVENTS: [(&str, Option<&str>, &str); 12] = [
    ("SessionStart", None, "session"),
    ("UserPromptSubmit", None, "working"),
    ("PreToolUse", Some(OTHER_TOOL_MATCHER), "working"),
    ("PreToolUse", Some(ASK_USER_QUESTION_MATCHER), "blocked"),
    ("PostToolUse", Some(ASK_USER_QUESTION_MATCHER), "working"),
    (
        "PostToolUseFailure",
        Some(ASK_USER_QUESTION_MATCHER),
        "working",
    ),
    ("SubagentStart", None, "working"),
    ("PreCompact", None, "working"),
    ("PermissionRequest", None, "blocked"),
    ("PermissionResult", None, "working"),
    ("Stop", None, "idle"),
    ("Interrupt", None, "idle"),
];

static AGENT_VERSION_REQUIREMENT: AgentVersionRequirement = AgentVersionRequirement {
    label: "kimi code",
    binary: "kimi",
    args: &["--version"],
    min_version: MIN_VERSION,
};

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path)
        .with_version_requirement(&AGENT_VERSION_REQUIREMENT);

fn install_adapter(profile: &IntegrationProfile) -> io::Result<Vec<String>> {
    let installed = install_kimi(profile)?;
    Ok(vec![
        format!(
            "installed kimi integration hook to {}",
            installed.hook_path.display()
        ),
        format!("ensured kimi config at {}", installed.config_path.display()),
        format!("requires kimi code {MIN_VERSION} or newer"),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_kimi()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed kimi hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no kimi hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_config {
        messages.push(format!(
            "removed herdr kimi hook entries from {}",
            result.config_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr kimi hook entries found in {}",
            result.config_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    kimi_dir().map(|dir| dir.join("hooks").join(HOOK_INSTALL_NAME))
}
