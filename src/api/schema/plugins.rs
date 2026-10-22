use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;
use super::common::SplitDirection;
use super::panes::PaneInfo;
use super::workspaces::WorkspaceWorktreeInfo;
use crate::cli::plugin::bindings;
use crate::cli::projection::{cli_params, Field};
use crate::popup_size::PopupSize;

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PluginLinkParams => PluginLinkProjection {
        pub path: String => Field::new(String::new()).input(bindings::path()),
        #[serde(default = "super::common::default_true")]
        pub enabled: bool => Field::new(true)
            .input(bindings::boolean("disabled", false))
            .input(bindings::boolean("enabled", true)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source: Option<PluginSourceInfo> => Field::new(None::<PluginSourceInfo>),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PluginListParams => PluginListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub plugin_id: Option<String> => Field::new(None).input(bindings::optional_text("plugin", "ID")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PluginUnlinkParams => PluginUnlinkProjection {
        pub plugin_id: String => Field::new(String::new()).input(bindings::id("plugin_id", "PLUGIN_ID")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PluginSetEnabledParams => PluginSetEnabledProjection {
        pub plugin_id: String => Field::new(String::new()).input(bindings::id("plugin_id", "PLUGIN_ID")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InstalledPluginInfo {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub min_herdr_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub manifest_path: String,
    pub plugin_root: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build: Vec<PluginManifestBuild>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup: Vec<PluginManifestStartup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<PluginManifestAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<PluginManifestEventHook>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<PluginManifestPane>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_handlers: Vec<PluginManifestLinkHandler>,
    #[serde(default)]
    pub source: PluginSourceInfo,
    /// Warnings collected at link time or on registry load (e.g. unknown event names,
    /// missing manifest file). Non-fatal — the entry is kept and surfaced by plugin.list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginSourceInfo {
    #[serde(default)]
    pub kind: PluginSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_unix_ms: Option<u64>,
}

impl Default for PluginSourceInfo {
    fn default() -> Self {
        Self {
            kind: PluginSourceKind::Local,
            owner: None,
            repo: None,
            subdir: None,
            requested_ref: None,
            resolved_commit: None,
            managed_path: None,
            installed_unix_ms: None,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum PluginSourceKind {
    #[default]
    Local,
    Github,
}

pub(crate) fn plugin_managed_path_component(value: &str) -> String {
    let slug = readable_plugin_path_slug(value);
    let hash = short_plugin_id_hash_for_path_component(value);
    format!("{slug}-{hash}")
}

fn readable_plugin_path_slug(value: &str) -> String {
    let mut slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug
        .trim_matches(|ch| matches!(ch, '-' | '_' | '.'))
        .to_string();
    let slug = if slug.is_empty() {
        "plugin".to_string()
    } else {
        slug.chars().take(80).collect()
    };
    if has_windows_reserved_stem_for_path_component(&slug) {
        slug.replace('.', "-")
    } else {
        slug
    }
}

pub(crate) fn short_plugin_id_hash_for_path_component(value: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(value.as_bytes());
    let mut hash = String::with_capacity(12);
    for byte in &digest[..6] {
        use std::fmt::Write as _;
        let _ = write!(hash, "{byte:02x}");
    }
    hash
}

pub(crate) fn has_windows_reserved_stem_for_path_component(value: &str) -> bool {
    let stem = value.split('.').next().unwrap_or(value);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_managed_path_component_is_windows_safe_and_collision_free() {
        let dotdot = plugin_managed_path_component("..");
        assert_ne!(dotdot, ".");
        assert_ne!(dotdot, "..");
        assert!(dotdot.starts_with("plugin-"));
        assert_ne!(
            plugin_managed_path_component("a:b"),
            plugin_managed_path_component("a_b")
        );
        assert_ne!(plugin_managed_path_component("con"), "con");
        assert!(!plugin_managed_path_component("example.").ends_with('.'));
        assert!(plugin_managed_path_component("con.example").starts_with("con-example-"));
        assert!(plugin_managed_path_component("aux.plugin").starts_with("aux-plugin-"));
        assert!(plugin_managed_path_component("nul.x").starts_with("nul-x-"));
        assert!(plugin_managed_path_component("com1.tool").starts_with("com1-tool-"));
    }

    #[test]
    fn plugin_managed_path_component_keeps_readable_slug() {
        let component = plugin_managed_path_component("example.worktree-bootstrap");
        assert!(component.starts_with("example.worktree-bootstrap-"));
        assert!(component.len() <= "example.worktree-bootstrap-".len() + 12);
    }

    #[test]
    fn plugin_managed_path_component_hash_distinguishes_same_slug_shape() {
        assert_ne!(
            plugin_managed_path_component("example:a"),
            plugin_managed_path_component("example/a")
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginManifestBuild {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginManifestStartup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginManifestAction {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<PluginActionContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginManifestEventHook {
    pub on: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginManifestPane {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
    #[serde(default)]
    pub placement: PluginPanePlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<PopupSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<PopupSize>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginManifestLinkHandler {
    pub id: String,
    pub title: String,
    pub pattern: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PluginActionListParams => PluginActionListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub plugin_id: Option<String> => Field::new(None).input(bindings::optional_text("plugin", "ID")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PluginLogListParams => PluginLogListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub plugin_id: Option<String> => Field::new(None).input(bindings::optional_text("plugin", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub limit: Option<usize> => Field::new(None).input(bindings::limit()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PluginActionInvokeParams => PluginActionInvokeProjection {
        pub action_id: String => Field::new(String::new()).input(bindings::id("action_id", "ACTION_ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub plugin_id: Option<String> => Field::new(None).input(bindings::optional_text("plugin", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub context: Option<PluginInvocationContext> => Field::new(Some(bindings::invocation_context())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginCommandLogInfo {
    pub log_id: String,
    pub plugin_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    pub command: Vec<String>,
    pub status: PluginCommandStatus,
    pub started_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginCommandStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginPlatform {
    Linux,
    Macos,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginActionContext {
    Global,
    Workspace,
    Tab,
    Pane,
    Selection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginInvocationContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorkspaceWorktreeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_status: Option<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clicked_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_handler_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginActionInfo {
    pub plugin_id: String,
    pub action_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<PluginActionContext>,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<PluginPlatform>>,
}

impl PluginActionInfo {
    pub fn qualified_id(&self) -> String {
        format!("{}.{}", self.plugin_id, self.action_id)
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PluginPaneOpenParams => PluginPaneOpenProjection {
        pub plugin_id: String => Field::new(String::new()).input(bindings::required_text("plugin", "ID").order(0)),
        pub entrypoint: String => Field::new(String::new()).input(bindings::required_text("entrypoint", "ID").order(1)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub placement: Option<PluginPanePlacement> => Field::new(None).input(bindings::placement().order(2)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub width: Option<PopupSize> => Field::new(None).input(bindings::width().hidden()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub height: Option<PopupSize> => Field::new(None).input(bindings::height().hidden()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(bindings::optional_text("workspace", "ID").order(3)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target_pane_id: Option<String> => Field::new(None).input(bindings::optional_text("target-pane", "PANE").order(4)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub direction: Option<SplitDirection> => Field::new(None).input(bindings::direction().order(5)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String> => Field::new(None).input(bindings::optional_text("cwd", "PATH").path().order(6)),
        #[serde(default)]
        pub focus: bool => Field::new(true)
            .input(bindings::boolean("focus", true).order(8))
            .input(bindings::boolean("no-focus", false).order(9)),
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        pub env: HashMap<String, String> => Field::new(HashMap::new()).input(bindings::environment().order(7)),
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum PluginPanePlacement {
    #[default]
    Overlay,
    Popup,
    Split,
    Tab,
    Zoomed,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PluginPaneFocusParams => PluginPaneFocusProjection {
        pub pane_id: String => Field::new(String::new()).input(bindings::pane_id()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PluginPaneCloseParams => PluginPaneCloseProjection {
        pub pane_id: String => Field::new(String::new()).input(bindings::pane_id()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PluginPaneInfo {
    pub plugin_id: String,
    pub entrypoint: String,
    pub pane: PaneInfo,
}
