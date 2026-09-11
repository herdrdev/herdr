mod actions;
pub(crate) mod builtin;
mod claude_settings;
mod command;
mod config_edit;
mod env;
mod file_ops;
mod opencode_config;
mod registry;
mod targets;
pub(crate) mod types;
mod version;

pub(crate) use actions::{install_target, uninstall_target};
#[cfg(windows)]
pub(crate) use env::hermes_dir;
#[cfg(test)]
pub(crate) use env::integration_env_lock;
pub(crate) use env::{
    antigravity_cli_dir, apply_pane_base_env, claude_dir, codex_dir, copilot_dir, cursor_dir,
    devin_dir, droid_dir, grok_dir, hermes_plugin_dir, kilo_dir, kimi_dir, mastracode_dir,
    omp_extension_dir, opencode_dir, pi_extension_dir, qodercli_dir, qwen_dir,
    HERDR_PANE_ID_ENV_VAR, HERDR_TAB_ID_ENV_VAR, HERDR_WORKSPACE_ID_ENV_VAR,
};
pub(crate) use opencode_config::tui_plugin_is_configured;
pub(crate) use registry::{
    executable_file_exists, installed_integration_statuses, integration_recommendations,
    integration_recommendations_with_registry, integration_target_label, parse_integration_version,
    print_outdated_update_notice,
};
pub(crate) use targets::{
    grok_hook_config, install_antigravity_cli, install_claude, install_codex, install_copilot,
    install_cursor, install_devin, install_droid, install_grok, install_hermes, install_kilo,
    install_kimi, install_mastracode, install_omp, install_opencode, install_pi, install_qodercli,
    install_qwen, uninstall_antigravity_cli, uninstall_claude, uninstall_codex, uninstall_copilot,
    uninstall_cursor, uninstall_devin, uninstall_droid, uninstall_grok, uninstall_hermes,
    uninstall_kilo, uninstall_kimi, uninstall_mastracode, uninstall_omp, uninstall_opencode,
    uninstall_pi, uninstall_qodercli, uninstall_qwen,
};
pub(crate) use types::{IntegrationRecommendation, IntegrationStatus, IntegrationStatusKind};

// Narrow compatibility imports for shared config/environment internals that
// consume trusted built-in installer values.
use crate::integration::builtin::hermes::PLUGIN_INSTALL_NAME as HERMES_PLUGIN_INSTALL_NAME;
use crate::integration::builtin::kimi::{
    CONFIG_BLOCK_BEGIN as KIMI_CONFIG_BLOCK_BEGIN, CONFIG_BLOCK_END as KIMI_CONFIG_BLOCK_END,
    HOOK_EVENTS as KIMI_HOOK_EVENTS,
};

use crate::integration::builtin::opencode::{
    V2_TUI_PLUGIN_DIR as OPENCODE_V2_TUI_PLUGIN_DIR,
    V2_TUI_PLUGIN_SPEC as OPENCODE_V2_TUI_PLUGIN_SPEC,
};

const INTEGRATION_VERSION_MARKER: &str = "HERDR_INTEGRATION_VERSION=";
pub(crate) const INSTALL_WARNING_PREFIX: &str = "warning:";

#[cfg(test)]
mod tests;
