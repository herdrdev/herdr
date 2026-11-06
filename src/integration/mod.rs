mod actions;
mod claude_settings;
mod command;
mod config_edit;
mod config_file;
mod env;
mod file_ops;
mod opencode_config;
mod registry;
mod targets;
mod types;
mod version;

pub(crate) use actions::{
    install_experimental_letta, install_target, uninstall_experimental_letta, uninstall_target,
};
#[cfg(test)]
pub(crate) use env::integration_env_lock;
pub(crate) use env::{
    apply_pane_base_env, SHEPHERD_PANE_ID_ENV_VAR, SHEPHERD_TAB_ID_ENV_VAR,
    SHEPHERD_WORKSPACE_ID_ENV_VAR,
};
pub(crate) use registry::{
    experimental_letta_integration_status, installed_integration_statuses,
    integration_recommendations, integration_target_label, print_outdated_update_notice,
};
pub(crate) use types::{
    ExperimentalIntegrationStatus, IntegrationRecommendation, IntegrationStatus,
    IntegrationStatusKind,
};

/// CLI labels for experimental integrations that are intentionally not part of
/// the frozen client endpoint `IntegrationTarget` enum. Empty this list once the
/// agent registry provides first-class target registration.
pub(crate) const EXPERIMENTAL_INTEGRATION_TARGET_LABELS: &[&str] = &["letta"];

const PI_EXTENSION_INSTALL_NAME: &str = "shepherd-agent-state.ts";
const PI_EXTENSION_ASSET: &str = include_str!("assets/pi/shepherd-agent-state.ts");
const PI_INTEGRATION_VERSION: u32 = 9;
const OMP_EXTENSION_INSTALL_NAME: &str = "shepherd-omp-agent-state.ts";
const OMP_EXTENSION_ASSET: &str = include_str!("assets/omp/shepherd-agent-state.ts");
const OMP_INTEGRATION_VERSION: u32 = 10;
const CLAUDE_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const CLAUDE_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/claude/shepherd-agent-state.ps1")
} else {
    include_str!("assets/claude/shepherd-agent-state.sh")
};
const CLAUDE_INTEGRATION_VERSION: u32 = 11;
const CODEX_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const CODEX_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/codex/shepherd-agent-state.ps1")
} else {
    include_str!("assets/codex/shepherd-agent-state.sh")
};
const CODEX_INTEGRATION_VERSION: u32 = 10;
const KIMI_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const KIMI_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/kimi/shepherd-agent-state.ps1")
} else {
    include_str!("assets/kimi/shepherd-agent-state.sh")
};
const KIMI_INTEGRATION_VERSION: u32 = 8;
const KIMI_CONFIG_BLOCK_BEGIN: &str = "# >>> shepherd kimi integration";
const KIMI_CONFIG_BLOCK_END: &str = "# <<< shepherd kimi integration";
const KIMI_MIN_VERSION: &str = "0.14.0";
const KIMI_ASK_USER_QUESTION_MATCHER: &str = "^AskUserQuestion$";
const KIMI_OTHER_TOOL_MATCHER: &str = "^(?!AskUserQuestion$).*$";
const KIMI_HOOK_EVENTS: [(&str, Option<&str>, &str); 12] = [
    ("SessionStart", None, "session"),
    ("UserPromptSubmit", None, "working"),
    ("PreToolUse", Some(KIMI_OTHER_TOOL_MATCHER), "working"),
    (
        "PreToolUse",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "blocked",
    ),
    (
        "PostToolUse",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "working",
    ),
    (
        "PostToolUseFailure",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "working",
    ),
    ("SubagentStart", None, "working"),
    ("PreCompact", None, "working"),
    ("PermissionRequest", None, "blocked"),
    ("PermissionResult", None, "working"),
    ("Stop", None, "idle"),
    ("Interrupt", None, "idle"),
];
const COPILOT_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const COPILOT_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/copilot/shepherd-agent-state.ps1")
} else {
    include_str!("assets/copilot/shepherd-agent-state.sh")
};
const COPILOT_INTEGRATION_VERSION: u32 = 4;
const COPILOT_HOOK_EVENTS: [&str; 1] = ["SessionStart"];
const COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS: [&str; 9] = [
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
const DEVIN_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const DEVIN_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/devin/shepherd-agent-state.ps1")
} else {
    include_str!("assets/devin/shepherd-agent-state.sh")
};
const DEVIN_INTEGRATION_VERSION: u32 = 3;
const DEVIN_HOOK_EVENTS: [(&str, &str); 6] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "session"),
    ("PreToolUse", "session"),
    ("PostToolUse", "session"),
    ("PermissionRequest", "session"),
    ("Stop", "session"),
];
const DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
const DROID_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const DROID_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/droid/shepherd-agent-state.ps1")
} else {
    include_str!("assets/droid/shepherd-agent-state.sh")
};
const DROID_INTEGRATION_VERSION: u32 = 4;
const DROID_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
const DROID_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 9] = [
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
const OPENCODE_PLUGIN_INSTALL_NAME: &str = "shepherd-agent-state.js";
const OPENCODE_PLUGIN_ASSET: &str = include_str!("assets/opencode/shepherd-agent-state.js");
const OPENCODE_TUI_PLUGIN_INSTALL_NAME: &str = "shepherd-tui-session.js";
const OPENCODE_TUI_PLUGIN_SPEC: &str = "./shepherd-tui-session.js";
const OPENCODE_TUI_PLUGIN_ASSET: &str = include_str!("assets/opencode/shepherd-tui-session.js");
const OPENCODE_V2_TUI_PLUGIN_DIR: &str = "shepherd-opencode";
const OPENCODE_V2_TUI_PLUGIN_SPEC: &str = "./shepherd-opencode";
const OPENCODE_V2_TUI_PLUGIN_ASSET: &str = include_str!("assets/opencode/tui.js");
const OPENCODE_INTEGRATION_VERSION: u32 = 13;
const KILO_PLUGIN_INSTALL_NAME: &str = "shepherd-agent-state.js";
const KILO_PLUGIN_ASSET: &str = include_str!("assets/kilo/shepherd-agent-state.js");
const KILO_INTEGRATION_VERSION: u32 = 4;
const HERMES_PLUGIN_INSTALL_NAME: &str = "shepherd-agent-state";
const HERMES_PLUGIN_MANIFEST_INSTALL_NAME: &str = "plugin.yaml";
const HERMES_PLUGIN_INIT_INSTALL_NAME: &str = "__init__.py";
const HERMES_PLUGIN_MANIFEST_ASSET: &str = include_str!("assets/hermes/plugin.yaml");
const HERMES_PLUGIN_INIT_ASSET: &str = include_str!("assets/hermes/__init__.py");
const HERMES_INTEGRATION_VERSION: u32 = 5;
const QODERCLI_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const QODERCLI_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/qodercli/shepherd-agent-state.ps1")
} else {
    include_str!("assets/qodercli/shepherd-agent-state.sh")
};
const QODERCLI_INTEGRATION_VERSION: u32 = 4;
const QODERCLI_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
const QWEN_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-session.ps1"
} else {
    "shepherd-agent-session.sh"
};
const QWEN_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/qwen/shepherd-agent-session.ps1")
} else {
    include_str!("assets/qwen/shepherd-agent-session.sh")
};
const QWEN_INTEGRATION_VERSION: u32 = 2;
const QWEN_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
const LETTA_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-session.ps1"
} else {
    "shepherd-agent-session.sh"
};
const LETTA_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/letta/shepherd-agent-session.ps1")
} else {
    include_str!("assets/letta/shepherd-agent-session.sh")
};
const LETTA_INTEGRATION_VERSION: u32 = 2;
const LETTA_HOOK_TIMEOUT_MS: u64 = 10_000;
const QODERCLI_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 12] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PostToolUseFailure", "working"),
    ("SubagentStart", "working"),
    ("SubagentStop", "working"),
    ("PreCompact", "working"),
    ("Notification", "blocked"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
const CURSOR_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const CURSOR_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/cursor/shepherd-agent-state.ps1")
} else {
    include_str!("assets/cursor/shepherd-agent-state.sh")
};
const CURSOR_INTEGRATION_VERSION: u32 = 2;
#[cfg(windows)]
const ANTIGRAVITY_CLI_HOOK_INSTALL_NAME: &str = "shepherd-agent-state.ps1";
#[cfg(not(windows))]
const ANTIGRAVITY_CLI_HOOK_INSTALL_NAME: &str = "shepherd-agent-state.sh";
#[cfg(windows)]
const ANTIGRAVITY_CLI_HOOK_ASSET: &str =
    include_str!("assets/antigravity_cli/shepherd-agent-state.ps1");
#[cfg(not(windows))]
const ANTIGRAVITY_CLI_HOOK_ASSET: &str =
    include_str!("assets/antigravity_cli/shepherd-agent-state.sh");
const ANTIGRAVITY_CLI_INTEGRATION_VERSION: u32 = 4;
/// Antigravity CLI keys `hooks.json` by hook name, so every Shepherd entry lives
/// under one Shepherd-owned block that install rewrites and uninstall removes.
const ANTIGRAVITY_CLI_HOOK_BLOCK_NAME: &str = "shepherd";
const ANTIGRAVITY_CLI_HOOK_TIMEOUT_SEC: u64 = 10;
/// `(event, reported action)`. Session-only: `PreInvocation` is the only event
/// we need because it carries `conversationId`. The others cannot express
/// lifecycle safely — Antigravity CLI has no blocked event, `PostInvocation` is
/// skipped on interruption, and `Stop` is end-of-turn rather than process exit.
/// Screen detection owns agent state instead.
///
/// `PreInvocation` takes a flat handler list; only the `PreToolUse`/`PostToolUse`
/// events accept a `matcher`/`hooks` wrapper, and sending one here would
/// invalidate the whole file.
const ANTIGRAVITY_CLI_HOOK_EVENTS: [(&str, &str); 1] = [("PreInvocation", "session")];
const INTEGRATION_VERSION_MARKER: &str = "SHEPHERD_INTEGRATION_VERSION=";
const MASTRACODE_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const MASTRACODE_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/mastracode/shepherd-agent-state.ps1")
} else {
    include_str!("assets/mastracode/shepherd-agent-state.sh")
};
const MASTRACODE_INTEGRATION_VERSION: u32 = 3;
const MASTRACODE_HOOK_TIMEOUT_MS: u64 = 10_000;
const MASTRACODE_REMOVED_HOOK_EVENTS: [(&str, &str); 2] =
    [("SessionStart", "idle"), ("SessionEnd", "release")];
const MASTRACODE_HOOK_EVENTS: [(&str, &str); 11] = [
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
const GROK_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "shepherd-agent-state.ps1"
} else {
    "shepherd-agent-state.sh"
};
const GROK_HOOK_CONFIG_INSTALL_NAME: &str = "shepherd.json";
const GROK_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/grok/shepherd-agent-state.ps1")
} else {
    include_str!("assets/grok/shepherd-agent-state.sh")
};
const GROK_INTEGRATION_VERSION: u32 = 3;

pub(crate) const INSTALL_WARNING_PREFIX: &str = "warning:";

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
