use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use crossterm::event::KeyModifiers;
use serde::{de, Deserialize, Deserializer, Serialize};

use super::{
    ActionKeybinds, BindingConfig, CommandKeybindConfig, IndexedKeybind, Keybinds, SidebarConfig,
    SoundConfig, TabBarRightEntryConfig, ThemeConfig, DEFAULT_MOBILE_WIDTH_THRESHOLD,
    DEFAULT_MOUSE_SCROLL_LINES, DEFAULT_SCROLLBACK_LIMIT_BYTES,
};

pub const MAX_TOAST_DELAY_SECONDS: u64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannelConfig {
    #[default]
    Stable,
    Preview,
}

impl UpdateChannelConfig {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    pub channel: UpdateChannelConfig,
    pub version_check: bool,
    pub manifest_check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            channel: default_update_channel(),
            version_check: true,
            manifest_check: true,
        }
    }
}

fn default_update_channel() -> UpdateChannelConfig {
    default_update_channel_for_build(crate::build_info::is_preview())
}

fn default_update_channel_for_build(is_preview: bool) -> UpdateChannelConfig {
    // A direct preview binary must keep following the preview manifest on every
    // platform. Defaulting a macOS/Linux preview install back to stable can
    // replace the herdrup-compatible fork with an upstream stable binary.
    if is_preview {
        UpdateChannelConfig::Preview
    } else {
        UpdateChannelConfig::Stable
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToastDelivery {
    #[default]
    Off,
    Herdr,
    Terminal,
    System,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum ToastHerdrPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ToastClipboardPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    #[default]
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentPanelSortConfig {
    #[default]
    #[serde(alias = "workspaces")]
    Spaces,
    Priority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LegacyAgentPanelScopeConfig {
    Current,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StatusIndicatorStyle {
    #[default]
    Dots,
    Symbols,
}

impl StatusIndicatorStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dots => "dots",
            Self::Symbols => "symbols",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HostCursorModeConfig {
    #[default]
    Auto,
    Native,
    Drawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarCollapsedModeConfig {
    #[default]
    Compact,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RightClickPassthroughModifierConfig(Option<KeyModifiers>);

impl RightClickPassthroughModifierConfig {
    pub fn modifiers(self) -> Option<KeyModifiers> {
        self.0
    }
}

impl<'de> Deserialize<'de> for RightClickPassthroughModifierConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_right_click_passthrough_modifier(&value)
            .map(Self)
            .ok_or_else(|| {
                de::Error::custom(
                    "right_click_passthrough_modifier must be empty, off, none, disabled, ctrl/control, alt/option, cmd/command/super, meta, hyper, or a + separated combination without shift",
                )
            })
    }
}

fn parse_right_click_passthrough_modifier(value: &str) -> Option<Option<KeyModifiers>> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("off")
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("disabled")
    {
        return Some(None);
    }

    let mut modifiers = KeyModifiers::empty();
    for token in trimmed.split('+') {
        let token = token.trim().to_ascii_lowercase();
        let modifier = match token.as_str() {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" | "option" => KeyModifiers::ALT,
            "cmd" | "command" | "super" => KeyModifiers::SUPER,
            "meta" => KeyModifiers::META,
            "hyper" => KeyModifiers::HYPER,
            "shift" => return None,
            _ => return None,
        };
        modifiers |= modifier;
    }

    (!modifiers.is_empty()).then_some(Some(modifiers))
}

#[derive(Debug, Clone)]
pub struct ToastConfig {
    pub delivery: ToastDelivery,
    pub delay_seconds: u64,
    pub herdr: HerdrToastConfig,
    pub clipboard: ClipboardToastConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct HerdrToastConfig {
    pub position: ToastHerdrPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct ClipboardToastConfig {
    pub enabled: bool,
    pub position: ToastClipboardPosition,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NewTerminalCwdConfig {
    #[default]
    Follow,
    Home,
    Current,
    Path(String),
}

impl<'de> Deserialize<'de> for NewTerminalCwdConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.trim() {
            "" | "follow" => Ok(Self::Follow),
            "home" => Ok(Self::Home),
            "current" => Ok(Self::Current),
            _ => Ok(Self::Path(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellModeConfig {
    #[default]
    Auto,
    Login,
    NonLogin,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    /// Executable used for new interactive panes. Empty means SHELL, then /bin/sh.
    pub default_shell: String,
    /// Startup mode for new interactive pane shells.
    pub shell_mode: ShellModeConfig,
    /// CWD policy for new interactive panes, tabs, and workspaces.
    pub new_cwd: NewTerminalCwdConfig,
    /// Render Kitty graphics in compatible outer terminals. Default: true.
    pub kitty_graphics: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Resume supported AI-agent panes into their native conversation sessions
    /// when restoring a Herdr session. Default: true.
    pub resume_agents_on_restore: bool,
    /// Milliseconds between automatic agent restores. Zero disables spacing.
    pub startup_per_agent_delay_ms: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            resume_agents_on_restore: true,
            startup_per_agent_delay_ms: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigReloadStatus {
    Applied,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ConfigReloadReport {
    pub status: ConfigReloadStatus,
    pub diagnostics: Vec<String>,
}

/// Validate `[ui]` sidebar bound configuration.
///
/// Returns `Some((min, max))` when `min <= max`, `None` otherwise. The two
/// values are funneled through this helper before they reach any
/// `u16::clamp(min, max)` call site (`u16::clamp` panics when `min > max`).
pub fn validated_sidebar_bounds(min: u16, max: u16) -> Option<(u16, u16)> {
    if min <= max {
        Some((min, max))
    } else {
        None
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub onboarding: Option<bool>,
    pub theme: ThemeConfig,
    pub terminal: TerminalConfig,
    pub session: SessionConfig,
    pub server: ServerConfig,
    pub update: UpdateConfig,
    pub keys: KeysConfig,
    pub ui: UiConfig,
    pub worktrees: WorktreesConfig,
    pub advanced: AdvancedConfig,
    pub experimental: ExperimentalConfig,
    pub remote: RemoteConfig,
    pub push: PushConfig,
    pub federation: FederationConfig,
    pub gram_relay: GramRelayConfig,
    pub guest: GuestConfig,
    pub accounts: Vec<AccountConfig>,
}

/// A per-harness credential/config-home account.
///
/// An account points a coding-agent harness at an alternate config-home
/// directory (Claude Code `CLAUDE_CONFIG_DIR`, Codex `CODEX_HOME`, Kimi
/// `KIMI_CODE_HOME`), so a single herdr install can run several subscriptions of
/// the same harness side by side and swap between them per agent. Only the
/// directory PATH is recorded here — never a token or credential value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AccountConfig {
    /// Stable identifier used to select this account on `agent.start` /
    /// `agent.restart` and to key it in `accounts.list`.
    pub id: String,
    /// Harness this account belongs to: `claude`, `codex`, `omp`, or `kimi`.
    pub kind: String,
    /// Human-facing label shown in listings.
    pub label: String,
    /// Filesystem path to the harness config-home directory for this account.
    pub config_dir: String,
}

/// The config-home environment variable a harness reads to locate its
/// config/credentials directory, or `None` for a kind that has no such lever.
pub fn env_var_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "claude" => Some("CLAUDE_CONFIG_DIR"),
        "codex" => Some("CODEX_HOME"),
        "omp" => Some("PI_CODING_AGENT_DIR"),
        "kimi" => Some("KIMI_CODE_HOME"),
        _ => None,
    }
}

/// The environment variables that applying a config-home override for `kind`
/// must CLEAR so the selected account is authoritative. A machine-global
/// `CLAUDE_CODE_OAUTH_TOKEN` otherwise overrides every per-account credential
/// regardless of `CLAUDE_CONFIG_DIR`. OMP's profile selectors similarly override
/// `PI_CODING_AGENT_DIR`, including the account's credentials and extensions.
/// Empty for kinds with no such overriding lever today (codex/kimi/unknown).
pub fn auth_env_vars_to_clear(kind: &str) -> &'static [&'static str] {
    match kind {
        "claude" => &["CLAUDE_CODE_OAUTH_TOKEN"],
        "omp" => &["OMP_PROFILE", "PI_PROFILE"],
        _ => &[],
    }
}

/// The harness kind whose config-home lever is `var` (the reverse of
/// [`env_var_for_kind`]), or `None` when `var` is not a config-home override.
/// Lets a launch surface that only sees the resolved env vars decide which auth
/// tokens to clear alongside the config-dir it is applying.
pub fn kind_for_config_env_var(var: &str) -> Option<&'static str> {
    match var {
        "CLAUDE_CONFIG_DIR" => Some("claude"),
        "CODEX_HOME" => Some("codex"),
        "PI_CODING_AGENT_DIR" => Some("omp"),
        "KIMI_CODE_HOME" => Some("kimi"),
        _ => None,
    }
}

/// The default config-home directory a harness uses with no override
/// (`$HOME/.claude`, `$HOME/.codex`, `$HOME/.omp/agent`,
/// `$HOME/.kimi-code`). OMP honors its two native directory overrides so the
/// integration installer, transcript verifier, and launched runtime all resolve
/// one authoritative account home. `None` when a home-relative path needs an
/// unset `HOME`, or the kind has no config-home lever.
pub fn default_config_dir(kind: &str) -> Option<PathBuf> {
    if kind == "omp" {
        if let Some(agent_dir) =
            std::env::var_os("PI_CODING_AGENT_DIR").filter(|value| !value.is_empty())
        {
            let agent_dir = PathBuf::from(agent_dir);
            return Some(if let Ok(relative) = agent_dir.strip_prefix("~") {
                PathBuf::from(std::env::var_os("HOME")?).join(relative)
            } else {
                agent_dir
            });
        }
        let config_dir = std::env::var_os("PI_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".omp"));
        return Some(if config_dir.is_absolute() {
            config_dir.join("agent")
        } else {
            PathBuf::from(std::env::var_os("HOME")?)
                .join(config_dir)
                .join("agent")
        });
    }
    let sub = match kind {
        "claude" => ".claude",
        "codex" => ".codex",
        "kimi" => ".kimi-code",
        _ => return None,
    };
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(sub))
}

/// Resolve OMP's transcript trust root from its complete agent directory.
/// `PI_CODING_AGENT_DIR` relocates config, credentials, extensions, and sessions
/// together; native OMP does not use `XDG_DATA_HOME` for its session files.
pub(crate) fn omp_sessions_dir(agent_dir: &Path) -> PathBuf {
    agent_dir.join("sessions")
}

/// Whether `config_dir` points at the harness's DEFAULT config-home for `kind`.
///
/// A "primary" account registered at this directory must inject NO env override:
/// some harnesses (Claude Code) keep their main config file as a SIBLING of this
/// directory (`~/.claude.json`), so forcing `CLAUDE_CONFIG_DIR=~/.claude` on a
/// default install strands that config and boots a blank one. Compares
/// canonicalized paths when both resolve (so a symlinked `HOME` still matches),
/// else falls back to a component-wise path comparison. See issue #94.
pub fn is_default_config_dir(kind: &str, config_dir: &str) -> bool {
    let Some(default) = default_config_dir(kind) else {
        return false;
    };
    let candidate = Path::new(config_dir);
    match (candidate.canonicalize(), default.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => candidate == default.as_path(),
    }
}

/// What launching under a selected account must do to the child's environment.
///
/// Two INDEPENDENT halves, and conflating them was a live routing bug. `vars` is
/// empty for a primary account on the harness default config-home (issue #94),
/// but `clear` is populated for EVERY selected account of a kind that has an auth
/// lever. Deriving "which tokens to clear" from "which vars were set" means a
/// default-config-home account clears nothing and a machine-global
/// `CLAUDE_CODE_OAUTH_TOKEN` silently outranks the selection — the exact mechanism
/// that re-homed this fleet's agents onto the wrong account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountLaunchEnv {
    /// Config-home overrides to SET. Empty means "this account needs no override",
    /// never "no account was selected".
    pub vars: Vec<(String, String)>,
    /// Auth variables to REMOVE so the selected account's own credentials are
    /// authoritative. Independent of `vars` being empty.
    pub clear_vars: Vec<String>,
}

impl AccountLaunchEnv {
    /// The launch env for a pane that selected no account: change nothing.
    pub fn unselected() -> Self {
        Self::default()
    }

    /// Wrap already-resolved override vars, deriving the clear-list from the
    /// config-home levers they set. For env that was resolved earlier and stored
    /// without its account (a pending `agent.restart` env), this is the most that
    /// can be recovered — it cannot see an account whose override set was empty.
    pub fn from_resolved_vars(vars: Vec<(String, String)>) -> Self {
        let clear_vars = vars
            .iter()
            .filter_map(|(key, _)| kind_for_config_env_var(key))
            .flat_map(|kind| {
                auth_env_vars_to_clear(kind)
                    .iter()
                    .map(|var| (*var).to_string())
            })
            .collect();
        Self { vars, clear_vars }
    }

    /// Whether this selects an account at all (anything to set or to clear).
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty() && self.clear_vars.is_empty()
    }
}

impl AccountConfig {
    /// The launch env that points this account's harness at its config-home and
    /// clears credentials that would outrank it. `None` when the kind has no
    /// config-home lever (an unknown/misconfigured kind).
    ///
    /// A primary account whose `config_dir` is the harness DEFAULT config-home
    /// sets NO override: it stays selectable/rememberable while launching exactly
    /// as a default install, because injecting the override would strand a harness
    /// whose config file is a sibling of the dir rather than inside it (issue #94).
    /// It still CLEARS conflicting auth tokens — clearing a token that authenticates
    /// as a different account strands nothing, and skipping it is what let a global
    /// token override an explicit selection of the primary account.
    pub fn launch_env(&self) -> Option<AccountLaunchEnv> {
        let env_var = env_var_for_kind(&self.kind)?;
        let clear_vars = auth_env_vars_to_clear(&self.kind)
            .iter()
            .map(|var| (*var).to_string())
            .collect();
        if is_default_config_dir(&self.kind, &self.config_dir) {
            return Some(AccountLaunchEnv {
                vars: Vec::new(),
                clear_vars,
            });
        }
        Some(AccountLaunchEnv {
            vars: vec![(env_var.to_string(), self.config_dir.clone())],
            clear_vars,
        })
    }
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub diagnostics: Vec<String>,
    pub invalid_sections: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeysConfig {
    /// Prefix key(s) to enter prefix mode (e.g. "ctrl+b", "f12", "esc", or an
    /// array to accept several).
    pub prefix: BindingConfig,
    /// Open keybinding help. Default: "prefix+?"
    pub help: BindingConfig,
    /// Open settings. Default: "prefix+s"
    pub settings: BindingConfig,
    /// Create a new workspace. Default: "prefix+shift+n"
    pub new_workspace: BindingConfig,
    /// Create a Git worktree from the selected workspace. Default: "prefix+shift+g"
    pub new_worktree: BindingConfig,
    /// Open an existing Git worktree from the selected workspace. Unset by default.
    pub open_worktree: BindingConfig,
    /// Delete the selected managed worktree checkout after confirmation. Unset by default.
    pub remove_worktree: BindingConfig,
    /// Rename the selected workspace. Default: "prefix+shift+w"
    pub rename_workspace: BindingConfig,
    /// Close the selected workspace. Default: "prefix+shift+d"
    pub close_workspace: BindingConfig,
    /// Open the workspace navigation surface. Default: "prefix+w"
    pub workspace_picker: BindingConfig,
    /// Open the session navigator. Default: "prefix+g"
    pub goto: BindingConfig,
    /// Move workspace selection up in navigate mode. Default: "up".
    pub navigate_workspace_up: BindingConfig,
    /// Move workspace selection down in navigate mode. Default: "down".
    pub navigate_workspace_down: BindingConfig,
    /// Focus the pane to the left in navigate mode. Default: "h". Left arrow is always an alias.
    pub navigate_pane_left: BindingConfig,
    /// Focus the pane below in navigate mode. Default: "j".
    pub navigate_pane_down: BindingConfig,
    /// Focus the pane above in navigate mode. Default: "k".
    pub navigate_pane_up: BindingConfig,
    /// Focus the pane to the right in navigate mode. Default: "l". Right arrow is always an alias.
    pub navigate_pane_right: BindingConfig,
    /// Detach the current client from its Herdr server. Default: "prefix+q".
    pub detach: BindingConfig,
    /// Reload config.toml in the running app/server. Default: "prefix+shift+r".
    pub reload_config: BindingConfig,
    /// Focus the currently visible notification target. Default: "prefix+o".
    pub open_notification_target: BindingConfig,
    /// Select the previous workspace. Unset by default.
    pub previous_workspace: BindingConfig,
    /// Select the next workspace. Unset by default.
    pub next_workspace: BindingConfig,
    /// Focus the previous agent shown in the agent panel. Unset by default.
    pub previous_agent: BindingConfig,
    /// Focus the next agent shown in the agent panel. Unset by default.
    pub next_agent: BindingConfig,
    /// Focus an agent by index 1-9. Unset by default.
    pub focus_agent: BindingConfig,
    /// Local-client shortcut that sends a clipboard image to a remote Herdr session. Default: "ctrl+v".
    pub remote_image_paste: String,
    /// Create a new tab in the active workspace. Default: "prefix+c"
    pub new_tab: BindingConfig,
    /// Rename the active tab. Default: "prefix+shift+t".
    pub rename_tab: BindingConfig,
    /// Select the previous tab. Default: "prefix+p".
    pub previous_tab: BindingConfig,
    /// Select the next tab. Default: "prefix+n".
    pub next_tab: BindingConfig,
    /// Move the active tab one position toward the front. Unset by default.
    pub move_tab_previous: BindingConfig,
    /// Move the active tab one position toward the back. Unset by default.
    pub move_tab_next: BindingConfig,
    /// Switch to tab 1-9. Default: "prefix+1..9".
    pub switch_tab: BindingConfig,
    /// Switch to workspace 1-9 from prefix mode. Unset by default.
    pub switch_workspace: BindingConfig,
    /// Close the active tab. Default: "prefix+shift+x".
    pub close_tab: BindingConfig,
    /// Rename the focused pane. Default: "prefix+shift+p".
    pub rename_pane: BindingConfig,
    /// Open the focused pane scrollback in $EDITOR. Default: "prefix+e".
    pub edit_scrollback: BindingConfig,
    pub clear_pane: BindingConfig,
    /// Enter keyboard copy mode for the focused pane. Default: "prefix+[".
    pub copy_mode: BindingConfig,
    /// Focus the pane to the left. Default: "prefix+h".
    pub focus_pane_left: BindingConfig,
    /// Focus the pane below. Default: "prefix+j".
    pub focus_pane_down: BindingConfig,
    /// Focus the pane above. Default: "prefix+k".
    pub focus_pane_up: BindingConfig,
    /// Focus the pane to the right. Default: "prefix+l".
    pub focus_pane_right: BindingConfig,
    /// Swap the focused pane with the pane to the left. Default: "prefix+shift+h".
    pub swap_pane_left: BindingConfig,
    /// Swap the focused pane with the pane below. Default: "prefix+shift+j".
    pub swap_pane_down: BindingConfig,
    /// Swap the focused pane with the pane above. Default: "prefix+shift+k".
    pub swap_pane_up: BindingConfig,
    /// Swap the focused pane with the pane to the right. Default: "prefix+shift+l".
    pub swap_pane_right: BindingConfig,
    /// Cycle to the next pane. Default: "prefix+tab".
    pub cycle_pane_next: BindingConfig,
    /// Cycle to the previous pane. Default: "prefix+shift+tab".
    pub cycle_pane_previous: BindingConfig,
    /// Focus the last focused pane across workspaces and tabs. Unset by default.
    pub last_pane: BindingConfig,
    /// Split pane vertically (side by side). Default: "prefix+v"
    pub split_vertical: BindingConfig,
    /// Split pane horizontally (stacked). Default: "prefix+minus"
    pub split_horizontal: BindingConfig,
    /// Close the focused pane. Default: "prefix+x"
    pub close_pane: BindingConfig,
    /// Toggle zoom for the focused pane. Default: "prefix+z"
    #[serde(alias = "fullscreen")]
    pub zoom: BindingConfig,
    /// Enter resize mode. Default: "prefix+r"
    pub resize_mode: BindingConfig,
    /// Resize the focused pane toward the left. Unset by default.
    pub resize_pane_left: BindingConfig,
    /// Resize the focused pane downward. Unset by default.
    pub resize_pane_down: BindingConfig,
    /// Resize the focused pane upward. Unset by default.
    pub resize_pane_up: BindingConfig,
    /// Resize the focused pane toward the right. Unset by default.
    pub resize_pane_right: BindingConfig,
    /// Toggle sidebar collapse. Default: "prefix+b"
    pub toggle_sidebar: BindingConfig,
    /// Optional indexed shortcuts expanded over number keys 1-9.
    pub indexed: IndexedKeysConfig,
    /// Prefix-mode custom command bindings.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<CommandKeybindConfig>,
    #[serde(skip_serializing)]
    pub(crate) user_fields: BTreeSet<&'static str>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct KeysConfigOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<BindingConfig>,
    /// Additional prefix keys published for cross-version compatibility.
    /// Older clients parse `prefix` as a single string and ignore this field;
    /// new clients merge it into the effective prefix list.
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_prefixes: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_worktree: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_worktree: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remove_worktree: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rename_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_picker: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    goto: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    navigate_workspace_up: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    navigate_workspace_down: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    navigate_pane_left: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    navigate_pane_down: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    navigate_pane_up: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    navigate_pane_right: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detach: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reload_config: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_notification_target: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_agent: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_agent: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus_agent: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_image_paste: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rename_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    move_tab_previous: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    move_tab_next: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    switch_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    switch_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rename_pane: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit_scrollback: Option<BindingConfig>,
    clear_pane: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_mode: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus_pane_left: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus_pane_down: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus_pane_up: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus_pane_right: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap_pane_left: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap_pane_down: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap_pane_up: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap_pane_right: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycle_pane_next: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycle_pane_previous: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_pane: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    split_vertical: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    split_horizontal: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close_pane: Option<BindingConfig>,
    #[serde(alias = "fullscreen", skip_serializing_if = "Option::is_none")]
    zoom: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resize_mode: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resize_pane_left: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resize_pane_down: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resize_pane_up: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resize_pane_right: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_sidebar: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    indexed: Option<IndexedKeysConfig>,
    #[serde(skip_serializing)]
    command: Option<Vec<CommandKeybindConfig>>,
}

impl KeysConfigOverlay {
    pub(crate) fn set_prefixes(&mut self, prefixes: &[super::keybinds::KeyCombo]) {
        let mut labels = prefixes
            .iter()
            .map(|combo| super::keybinds::format_key_combo(*combo));
        self.prefix = Some(BindingConfig::One(labels.next().unwrap_or_default()));
        let extra: Vec<String> = labels.collect();
        self.extra_prefixes = (!extra.is_empty()).then_some(BindingConfig::Many(extra));
    }
}

impl<'de> Deserialize<'de> for KeysConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = KeysConfigOverlay::deserialize(deserializer)?;
        let mut keys = KeysConfig::default();

        let prefix_was_supplied = input.prefix.is_some() || input.extra_prefixes.is_some();
        let mut prefix_values = Vec::new();
        if let Some(prefix) = input.prefix {
            prefix_values.extend(prefix.into_values());
        }
        if let Some(extra) = input.extra_prefixes {
            prefix_values.extend(extra.into_values());
        }
        if prefix_was_supplied {
            // An explicitly empty list stays empty so prefix validation rejects
            // it and a reload keeps the current keybindings.
            keys.prefix = match prefix_values.len() {
                0 => BindingConfig::Many(Vec::new()),
                1 => BindingConfig::One(prefix_values.remove(0)),
                _ => BindingConfig::Many(prefix_values),
            };
            keys.user_fields.insert("prefix");
        }

        macro_rules! apply_field {
            ($field:ident) => {
                if let Some(value) = input.$field {
                    keys.$field = value;
                    keys.user_fields.insert(stringify!($field));
                }
            };
        }

        apply_field!(help);
        apply_field!(settings);
        apply_field!(new_workspace);
        apply_field!(new_worktree);
        apply_field!(open_worktree);
        apply_field!(remove_worktree);
        apply_field!(rename_workspace);
        apply_field!(close_workspace);
        apply_field!(workspace_picker);
        apply_field!(goto);
        apply_field!(navigate_workspace_up);
        apply_field!(navigate_workspace_down);
        apply_field!(navigate_pane_left);
        apply_field!(navigate_pane_down);
        apply_field!(navigate_pane_up);
        apply_field!(navigate_pane_right);
        apply_field!(detach);
        apply_field!(reload_config);
        apply_field!(open_notification_target);
        apply_field!(previous_workspace);
        apply_field!(next_workspace);
        apply_field!(previous_agent);
        apply_field!(next_agent);
        apply_field!(focus_agent);
        apply_field!(remote_image_paste);
        apply_field!(new_tab);
        apply_field!(rename_tab);
        apply_field!(previous_tab);
        apply_field!(next_tab);
        apply_field!(move_tab_previous);
        apply_field!(move_tab_next);
        apply_field!(switch_tab);
        apply_field!(switch_workspace);
        apply_field!(close_tab);
        apply_field!(rename_pane);
        apply_field!(edit_scrollback);
        apply_field!(clear_pane);
        apply_field!(copy_mode);
        apply_field!(focus_pane_left);
        apply_field!(focus_pane_down);
        apply_field!(focus_pane_up);
        apply_field!(focus_pane_right);
        apply_field!(swap_pane_left);
        apply_field!(swap_pane_down);
        apply_field!(swap_pane_up);
        apply_field!(swap_pane_right);
        apply_field!(cycle_pane_next);
        apply_field!(cycle_pane_previous);
        apply_field!(last_pane);
        apply_field!(split_vertical);
        apply_field!(split_horizontal);
        apply_field!(close_pane);
        apply_field!(zoom);
        apply_field!(resize_mode);
        apply_field!(resize_pane_left);
        apply_field!(resize_pane_down);
        apply_field!(resize_pane_up);
        apply_field!(resize_pane_right);
        apply_field!(toggle_sidebar);
        apply_field!(indexed);
        apply_field!(command);

        Ok(keys)
    }
}

impl KeysConfig {
    pub(crate) fn key_field_is_user_configured(&self, field: &str) -> bool {
        self.user_fields.contains(field)
    }

    pub(crate) fn local_profile(&self, keybinds: &Keybinds) -> KeysConfigOverlay {
        let mut profile = KeysConfigOverlay::default();

        macro_rules! copy_user_field {
            ($field:ident) => {
                if self.user_fields.contains(stringify!($field)) {
                    profile.$field = Some(self.$field.clone());
                }
            };
        }
        macro_rules! copy_effective_action_field {
            ($field:ident, $target:expr) => {
                if self.user_fields.contains(stringify!($field)) {
                    profile.$field = Some(self.$field.clone());
                } else if binding_config_is_effective(&self.$field, &$target) {
                    profile.$field = Some(self.$field.clone());
                } else if binding_config_has_values(&self.$field) {
                    profile.$field = Some(BindingConfig::empty());
                }
            };
        }
        macro_rules! copy_effective_indexed_field {
            ($field:ident, $target:expr) => {
                if self.user_fields.contains(stringify!($field)) {
                    profile.$field = Some(self.$field.clone());
                } else if let Some(effective) = effective_indexed_config(&self.$field, &$target) {
                    profile.$field = Some(effective);
                } else if binding_config_has_values(&self.$field) {
                    profile.$field = Some(BindingConfig::empty());
                }
            };
        }

        profile.prefix = Some(self.prefix.clone());
        copy_effective_action_field!(help, keybinds.help);
        copy_effective_action_field!(settings, keybinds.settings);
        copy_effective_action_field!(new_workspace, keybinds.new_workspace);
        copy_effective_action_field!(new_worktree, keybinds.new_worktree);
        copy_effective_action_field!(open_worktree, keybinds.open_worktree);
        copy_effective_action_field!(remove_worktree, keybinds.remove_worktree);
        copy_effective_action_field!(rename_workspace, keybinds.rename_workspace);
        copy_effective_action_field!(close_workspace, keybinds.close_workspace);
        copy_effective_action_field!(workspace_picker, keybinds.workspace_picker);
        copy_effective_action_field!(goto, keybinds.goto);
        copy_effective_action_field!(navigate_workspace_up, keybinds.navigate.workspace_up);
        copy_effective_action_field!(navigate_workspace_down, keybinds.navigate.workspace_down);
        copy_effective_action_field!(navigate_pane_left, keybinds.navigate.pane_left);
        copy_effective_action_field!(navigate_pane_down, keybinds.navigate.pane_down);
        copy_effective_action_field!(navigate_pane_up, keybinds.navigate.pane_up);
        copy_effective_action_field!(navigate_pane_right, keybinds.navigate.pane_right);
        copy_effective_action_field!(detach, keybinds.detach);
        copy_effective_action_field!(reload_config, keybinds.reload_config);
        copy_effective_action_field!(open_notification_target, keybinds.open_notification_target);
        copy_effective_action_field!(previous_workspace, keybinds.previous_workspace);
        copy_effective_action_field!(next_workspace, keybinds.next_workspace);
        copy_effective_action_field!(previous_agent, keybinds.previous_agent);
        copy_effective_action_field!(next_agent, keybinds.next_agent);
        copy_effective_indexed_field!(focus_agent, keybinds.focus_agent);
        copy_user_field!(remote_image_paste);
        copy_effective_action_field!(new_tab, keybinds.new_tab);
        copy_effective_action_field!(rename_tab, keybinds.rename_tab);
        copy_effective_action_field!(previous_tab, keybinds.previous_tab);
        copy_effective_action_field!(next_tab, keybinds.next_tab);
        copy_effective_action_field!(move_tab_previous, keybinds.move_tab_previous);
        copy_effective_action_field!(move_tab_next, keybinds.move_tab_next);
        copy_effective_indexed_field!(switch_tab, keybinds.switch_tab);
        copy_effective_indexed_field!(switch_workspace, keybinds.switch_workspace);
        copy_effective_action_field!(close_tab, keybinds.close_tab);
        copy_effective_action_field!(rename_pane, keybinds.rename_pane);
        copy_effective_action_field!(edit_scrollback, keybinds.edit_scrollback);
        copy_effective_action_field!(clear_pane, keybinds.clear_pane);
        copy_effective_action_field!(copy_mode, keybinds.copy_mode);
        copy_effective_action_field!(focus_pane_left, keybinds.focus_pane_left);
        copy_effective_action_field!(focus_pane_down, keybinds.focus_pane_down);
        copy_effective_action_field!(focus_pane_up, keybinds.focus_pane_up);
        copy_effective_action_field!(focus_pane_right, keybinds.focus_pane_right);
        copy_effective_action_field!(swap_pane_left, keybinds.swap_pane_left);
        copy_effective_action_field!(swap_pane_down, keybinds.swap_pane_down);
        copy_effective_action_field!(swap_pane_up, keybinds.swap_pane_up);
        copy_effective_action_field!(swap_pane_right, keybinds.swap_pane_right);
        copy_effective_action_field!(cycle_pane_next, keybinds.cycle_pane_next);
        copy_effective_action_field!(cycle_pane_previous, keybinds.cycle_pane_previous);
        copy_effective_action_field!(last_pane, keybinds.last_pane);
        copy_effective_action_field!(split_vertical, keybinds.split_vertical);
        copy_effective_action_field!(split_horizontal, keybinds.split_horizontal);
        copy_effective_action_field!(close_pane, keybinds.close_pane);
        copy_effective_action_field!(zoom, keybinds.zoom);
        copy_effective_action_field!(resize_mode, keybinds.resize_mode);
        copy_effective_action_field!(resize_pane_left, keybinds.resize_pane_left);
        copy_effective_action_field!(resize_pane_down, keybinds.resize_pane_down);
        copy_effective_action_field!(resize_pane_up, keybinds.resize_pane_up);
        copy_effective_action_field!(resize_pane_right, keybinds.resize_pane_right);
        copy_effective_action_field!(toggle_sidebar, keybinds.toggle_sidebar);
        copy_user_field!(indexed);

        profile
    }
}

fn binding_config_has_values(config: &BindingConfig) -> bool {
    config.has_values()
}

fn binding_config_is_effective(config: &BindingConfig, keybinds: &ActionKeybinds) -> bool {
    !binding_config_has_values(config) || !keybinds.bindings.is_empty()
}

fn effective_indexed_config(
    config: &BindingConfig,
    keybinds: &[IndexedKeybind],
) -> Option<BindingConfig> {
    if !binding_config_has_values(config) {
        return Some(config.clone());
    }

    let expected_labels = config.indexed_labels();
    if expected_labels.is_empty() {
        return None;
    }

    let effective_labels: Vec<String> = expected_labels
        .iter()
        .filter(|expected| {
            keybinds
                .iter()
                .any(|binding| binding.label.as_str() == expected.as_str())
        })
        .cloned()
        .collect();

    if effective_labels.is_empty() {
        None
    } else if effective_labels.len() == expected_labels.len() {
        Some(config.clone())
    } else {
        Some(BindingConfig::Many(effective_labels))
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct IndexedKeysConfig {
    /// Modifier combo for tab shortcuts 1-9. Unset by default.
    pub tabs: String,
    /// Modifier combo for workspace shortcuts 1-9. Unset by default.
    pub workspaces: String,
    /// Modifier combo for agent shortcuts 1-9. Unset by default.
    pub agents: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct WorktreesConfig {
    /// Root directory under which Herdr creates <repo>/<branch-slug> checkouts.
    pub directory: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TabBarPositionConfig {
    #[default]
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneBordersConfig {
    #[default]
    Auto,
    Always,
    Off,
}

impl PaneBordersConfig {
    pub fn draws_borders(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn shows_borders(self, multi_pane: bool) -> bool {
        self.draws_borders() && (multi_pane || matches!(self, Self::Always))
    }
}

impl<'de> Deserialize<'de> for PaneBordersConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PaneBordersVisitor;

        impl<'de> de::Visitor<'de> for PaneBordersVisitor {
            type Value = PaneBordersConfig;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("\"auto\", \"always\", \"off\", or a legacy boolean")
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(if value {
                    PaneBordersConfig::Auto
                } else {
                    PaneBordersConfig::Off
                })
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "auto" => Ok(PaneBordersConfig::Auto),
                    "always" => Ok(PaneBordersConfig::Always),
                    "off" => Ok(PaneBordersConfig::Off),
                    other => Err(E::invalid_value(de::Unexpected::Str(other), &self)),
                }
            }
        }

        deserializer.deserialize_any(PaneBordersVisitor)
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub sidebar_width: u16,
    /// Minimum sidebar width (columns) when expanded. Default: 18.
    pub sidebar_min_width: u16,
    /// Maximum sidebar width (columns) when expanded. Default: 36.
    pub sidebar_max_width: u16,
    /// Start with the sidebar collapsed. Default: false.
    pub sidebar_start_collapsed: bool,
    /// Collapsed sidebar presentation. Default: compact.
    pub sidebar_collapsed_mode: SidebarCollapsedModeConfig,
    /// Terminal width at or below which Herdr uses the mobile single-column layout. Default: 64.
    pub mobile_width_threshold: u16,
    /// Capture mouse input for Herdr's mouse UI. Default: true.
    pub mouse_capture: bool,
    /// Copy text selected with the mouse. Default: true.
    pub copy_on_select: bool,
    /// Host cursor policy. Default: auto.
    pub host_cursor: HostCursorModeConfig,
    /// Modifier that lets right-click gestures pass through to pane apps. Empty disables it.
    pub right_click_passthrough_modifier: RightClickPassthroughModifierConfig,
    /// Force a full host-terminal redraw when the outer terminal regains focus. Default: true.
    pub redraw_on_focus_gained: bool,
    /// Lines to scroll per mouse wheel notch. Default: 3.
    pub mouse_scroll_lines: Option<NonZeroUsize>,
    /// Ask for confirmation before closing a workspace. Default: true.
    pub confirm_close: bool,
    /// Ask for a tab name before creating a new tab. Default: true.
    pub prompt_new_tab_name: bool,
    /// Ask for a workspace name before interactive creation. Default: false.
    pub prompt_new_workspace_name: bool,
    /// Draw borders around split panes. auto draws them only for split panes,
    /// always also frames a lone pane (only while pane_outer_borders is
    /// enabled, since every edge of a lone pane is an outer edge), off
    /// disables them. Legacy booleans map true to auto and false to off.
    /// Default: auto.
    pub pane_borders: PaneBordersConfig,
    /// Draw borders along the outside edge of the pane area. Default: true.
    pub pane_outer_borders: bool,
    /// Draw interactive scrollbars beside terminal panes. Default: true.
    pub pane_scrollbars: bool,
    /// Keep split panes visually separated instead of sharing divider borders. Default: true.
    pub pane_gaps: bool,
    /// Show agent labels in split pane borders when no manual pane label is set. Default: false.
    pub show_agent_labels_on_pane_borders: bool,
    /// Hide the tab row when the workspace has one tab. Default: false.
    pub hide_tab_bar_when_single_tab: bool,
    /// Desktop tab row placement. Default: top.
    pub tab_bar_position: TabBarPositionConfig,
    /// Ordered entries shown at the right edge of the desktop tab row. Empty by default.
    pub tab_bar_right: Vec<TabBarRightEntryConfig>,
    /// Text inserted between visible right-side tab bar entries. Default: one space.
    pub tab_bar_right_separator: String,
    /// Format for the outer terminal window title. Empty leaves the title alone.
    /// Default: "{hostname}: {workspace}".
    pub window_title: String,
    /// Agent sidebar ordering. Saved values are "spaces" or "priority". Default: "spaces".
    pub agent_panel_sort: AgentPanelSortConfig,
    /// Retired setting that Herdr wrote before the workspace filter was removed.
    #[serde(rename = "agent_panel_scope")]
    _legacy_agent_panel_scope: Option<LegacyAgentPanelScopeConfig>,
    /// Agent status indicator style. Saved values are "dots" or "symbols". Default: "dots".
    pub status_indicators: StatusIndicatorStyle,
    /// Expanded sidebar row composition.
    pub sidebar: SidebarConfig,
    /// Accent color for highlights, borders, and navigation UI.
    /// Accepts hex (#89b4fa), named colors (cyan, blue), or RGB (rgb(137,180,250)).
    pub accent: String,
    /// Optional visual toast notifications for background workspace events.
    pub toast: ToastConfig,
    /// Play sounds when agents change state in background workspaces.
    pub sound: SoundConfig,
}

/// Cursor shape (DECSCUSR) used for the forced IME anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImeCursorShape {
    Block,
    #[default]
    SteadyBlock,
    Underline,
    SteadyUnderline,
    Bar,
    SteadyBar,
}

impl ImeCursorShape {
    /// Convert to DECSCUSR parameter (1–6).
    pub fn to_decscusr(self) -> u8 {
        match self {
            Self::Block => 1,
            Self::SteadyBlock => 2,
            Self::Underline => 3,
            Self::SteadyUnderline => 4,
            Self::Bar => 5,
            Self::SteadyBar => 6,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Windows: allow ordinary same-account clients to control an elevated server. Default: false.
    #[cfg(windows)]
    pub allow_unelevated_clients: bool,
    /// Virtual terminal width used when no client is attached. Default: 120.
    pub headless_cols: u16,
    /// Virtual terminal height used when no client is attached. Default: 40.
    pub headless_rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AdvancedConfig {
    /// Maximum scrollback buffer size in bytes retained per pane terminal. Default: 10000000.
    #[serde(alias = "scrollback_lines")]
    pub scrollback_limit_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    /// Add keepalive fallbacks and private connection reuse for `herdr --remote`.
    /// Set false to run plain ssh unchanged. Default: true.
    pub manage_ssh_config: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            manage_ssh_config: true,
        }
    }
}

/// Remote push notifications (APNs) for registered mobile devices.
///
/// The `.p8` signing key at `key_path` is read only when a push is sent; its
/// contents are never persisted to Herdr state and never logged. `key_id`,
/// `team_id`, and `topic` are Apple developer identifiers (not secrets) but are
/// still host configuration rather than repository constants.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PushConfig {
    /// How pushes reach devices: `auto` (direct APNs when the key config is
    /// complete, else the HerdrUp relay), `direct`, `relay`, or `off`. Default: auto.
    pub mode: PushMode,
    /// Base URL of the HerdrUp push relay used by `relay` (and `auto` without a key).
    pub relay_url: String,
    /// Master switch for direct APNs delivery. Relay delivery needs no key. Default: false.
    pub enabled: bool,
    /// Filesystem path to the APNs auth key (`.p8`, PKCS#8 PEM). Read at send time only.
    pub key_path: Option<String>,
    /// APNs auth key ID (the 10-character Key ID issued by Apple).
    pub key_id: Option<String>,
    /// Apple developer Team ID used as the JWT issuer.
    pub team_id: Option<String>,
    /// APNs topic — the app bundle identifier, e.g. com.example.herdr.
    pub topic: Option<String>,
    /// Deliver through the APNs sandbox host instead of production. Default: false.
    pub sandbox: bool,
}

pub const DEFAULT_PUSH_RELAY_URL: &str = "https://push.herdrup.themartian.app";

/// `[guest]`: guest access through the HerdrUp guest relay (unix only).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GuestConfig {
    /// Base URL of the HerdrUp guest relay. Invites carry it, and the daemon
    /// links to it only while a guest or an unexpired invite exists.
    #[cfg_attr(not(unix), allow(dead_code))] // Guest access is unix only.
    pub relay_url: String,
}

pub const DEFAULT_GUEST_RELAY_URL: &str = "https://guest.herdrup.themartian.app";

impl Default for GuestConfig {
    fn default() -> Self {
        Self {
            relay_url: DEFAULT_GUEST_RELAY_URL.to_string(),
        }
    }
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            mode: PushMode::default(),
            relay_url: DEFAULT_PUSH_RELAY_URL.to_string(),
            enabled: false,
            key_path: None,
            key_id: None,
            team_id: None,
            topic: None,
            sandbox: false,
        }
    }
}

/// Delivery path for remote push. `Auto` prefers direct APNs when the key config
/// is complete and otherwise uses the relay for capability-bearing devices.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum PushMode {
    #[default]
    Auto,
    Direct,
    Relay,
    Off,
}

/// Capability tier granted to a federation peer's shared token.
///
/// The tiers are ordered so a higher tier includes every capability of the
/// lower ones: `Admin` ⊇ `Interact` ⊇ `Observe`. A method exposed to federation
/// at some tier is reachable by any peer at that tier or above; everything not
/// explicitly exposed stays denied to every tier (default-deny). Only inbound
/// TCP federation connections are filtered by tier — the local unix socket and
/// the SSH control path are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityTier {
    /// Read-only: list/read/inspect panes, agents, events, layout. Default.
    #[default]
    Observe,
    /// Observe plus driving agents and panes (prompts, keystrokes, text).
    Interact,
    /// Interact plus focus, rename, and input/authority mutations.
    Admin,
}

/// Durable, default-off consent for the machine-scoped Gram file relay.
///
/// Independent of `[federation]` reverse agent access and per-agent grants.
/// Same-user processes on a trusted machine can impersonate that machine's
/// panes; this is not per-process isolation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct GramRelayConfig {
    /// Coordinator: saved-machine profile ids whose pinned SSH peer may relay
    /// Gram through a restricted reverse gateway. Unset grants nothing; a
    /// present empty list explicitly grants nothing and overrides the legacy
    /// environment variable instead of deferring to it.
    #[serde(deserialize_with = "deserialize_gram_relay_peers")]
    pub peers: Option<Vec<String>>,
    /// Remote: install id of the trusted coordinator. The reverse socket path is
    /// derived from it and this daemon's own install id. Unset grants nothing;
    /// an empty string explicitly disables the relay, like an empty list above.
    #[serde(deserialize_with = "deserialize_gram_relay_coordinator")]
    pub coordinator_machine_id: Option<String>,
}

fn deserialize_gram_relay_peers<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let peers = Vec::<String>::deserialize(deserializer)?;
    for peer in &peers {
        crate::client::endpoint::ProfileId::parse(peer.clone()).map_err(|_| {
            de::Error::custom(format!(
                "gram_relay.peers entry {peer:?} must be a saved-machine profile id (32 lowercase hex characters)"
            ))
        })?;
    }
    Ok(Some(peers))
}

fn deserialize_gram_relay_coordinator<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let machine_id = Option::<String>::deserialize(deserializer)?;
    if let Some(machine_id) = &machine_id {
        if !machine_id.is_empty() && !is_install_machine_id(machine_id) {
            return Err(de::Error::custom(
                "gram_relay.coordinator_machine_id must be \"machine_\" followed by 32 lowercase hex characters, or \"\" to disable the relay explicitly",
            ));
        }
    }
    Ok(machine_id)
}

/// Shape of a persisted Herdr install identity (`persist::machine`).
pub(crate) fn is_install_machine_id(machine_id: &str) -> bool {
    machine_id.strip_prefix("machine_").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Peer-to-peer federation between Herdr daemons.
///
/// The listener, outbound transports (TCP/SSH), and peer connection handling
/// land in later parts of the federation work; these fields are declared now so
/// the `[federation]` config section parses and defaults cleanly.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FederationConfig {
    /// Accept inbound federation connections. Default: false.
    pub listen: bool,
    /// Opt this daemon into coordinator behavior for saved-machine federation.
    /// Remote daemons do not federate saved profiles back unless explicitly
    /// configured as coordinators themselves.
    pub coordinator: bool,
    /// Remote-side explicit trusted-machine opt-in. The coordinator's SSH reverse
    /// socket is accepted only when its install identity matches this pin.
    /// Same-user processes can impersonate a granted pane; this is not process isolation.
    pub reverse_coordinator_machine_id: Option<String>,
    /// Address the federation listener binds to when `listen` is enabled.
    pub listen_addr: Option<String>,
    /// Federation peers configured directly by endpoint and alias. SSH peers
    /// must use `saved_machines`; this list accepts TCP and inbound-only peers.
    #[serde(default, deserialize_with = "deserialize_explicit_federation_peers")]
    pub peers: Vec<FederationPeer>,
    /// Coordinator policy for saved SSH machines, keyed by the immutable
    /// upstream profile id. Connection details, labels, sessions, and enabled
    /// state remain owned by the endpoint catalog.
    #[serde(default, deserialize_with = "deserialize_saved_machine_policies")]
    pub saved_machines: BTreeMap<crate::client::endpoint::ProfileId, FederationSavedMachinePolicy>,
    /// Named sessions (`herdr --session <name>`) that federate like the default
    /// session. Every session reads this machine-wide config, so by default
    /// only the default session coordinates saved machines, polls outbound
    /// peers, and binds `listen_addr`; other named sessions remain reachable
    /// as a remote coordinator's SSH target. `"default"` is accepted and
    /// redundant.
    #[serde(default, deserialize_with = "deserialize_federation_named_sessions")]
    pub named_sessions: Vec<String>,
}

impl FederationConfig {
    /// Whether the server for `session` (`None` = default session) runs this
    /// federation config: the default session always does, a named session
    /// only when listed in `named_sessions`.
    pub(crate) fn federates_session(&self, session: Option<&str>) -> bool {
        session.is_none_or(|name| {
            name == crate::session::DEFAULT_SESSION_NAME
                || self.named_sessions.iter().any(|listed| listed == name)
        })
    }

    /// The federation config the server for `session` actually runs.
    ///
    /// A named session that is not listed in `named_sessions` keeps no
    /// coordinator role, no outbound explicit peers, and no inbound listener:
    /// the default session's server already polls every saved machine and owns
    /// `listen_addr`. Remote-side settings such as
    /// `reverse_coordinator_machine_id` stay in effect. Returns
    /// `Cow::Owned` only when the session rule actually suppressed something.
    pub(crate) fn for_session(&self, session: Option<&str>) -> std::borrow::Cow<'_, Self> {
        let suppresses = self.coordinator
            || self.listen
            || self.peers.iter().any(|peer| peer.endpoint.is_some());
        if !suppresses || self.federates_session(session) {
            return std::borrow::Cow::Borrowed(self);
        }
        std::borrow::Cow::Owned(Self {
            listen: false,
            coordinator: false,
            peers: self
                .peers
                .iter()
                .filter(|peer| peer.endpoint.is_none())
                .cloned()
                .collect(),
            reverse_coordinator_machine_id: self.reverse_coordinator_machine_id.clone(),
            listen_addr: self.listen_addr.clone(),
            saved_machines: self.saved_machines.clone(),
            named_sessions: self.named_sessions.clone(),
        })
    }
}

fn deserialize_federation_named_sessions<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let sessions = Vec::<String>::deserialize(deserializer)?;
    for session in &sessions {
        crate::session::validate_name(session).map_err(|error| {
            de::Error::custom(format!(
                "federation.named_sessions entry {session:?} is invalid: {error}"
            ))
        })?;
    }
    Ok(sessions)
}

fn deserialize_explicit_federation_peers<'de, D>(
    deserializer: D,
) -> Result<Vec<FederationPeer>, D::Error>
where
    D: Deserializer<'de>,
{
    let peers = Vec::<FederationPeer>::deserialize(deserializer)?;
    if peers.iter().any(|peer| {
        peer.endpoint
            .as_deref()
            .is_some_and(|endpoint| endpoint.starts_with("ssh://"))
            || peer.profile_id.is_some()
            || peer.remote_session.is_some()
    }) {
        return Err(de::Error::custom(
            "SSH federation peers must use federation.saved_machines keyed by an existing saved profile ID; federation.peers accepts only TCP or inbound-only peers",
        ));
    }
    Ok(peers)
}

/// Federation-only policy attached to one saved SSH profile.
///
/// Presence opts the profile into coordinator federation. SSH remains a
/// full-control OpenSSH trust boundary; no token or private-key setting is
/// accepted here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederationSavedMachinePolicy {
    /// Persisted install identity expected from the remote Herdr daemon.
    /// Required so catalog reconciliation fails closed on a missing or changed
    /// machine identity.
    pub expected_machine_id: String,
    /// Selected live agents allowed through the coordinator's restricted reverse
    /// gateway. Empty by default; a saved profile alone grants no reverse access.
    #[serde(default)]
    pub agent_grants: Vec<FederationAgentGrant>,
}

/// A trusted-machine grant bound to the observed pane and harness session,
/// including its mutable label so a rename/reuse fails closed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederationAgentGrant {
    pub terminal_id: String,
    pub session: crate::api::schema::AgentSessionInfo,
    pub name: Option<String>,
    #[serde(default)]
    pub observe: bool,
    #[serde(default)]
    pub interact: bool,
    /// Other peer aliases whose agents may be observed, in addition to the
    /// coordinator. This never delegates those peers' own federation rosters.
    #[serde(default)]
    pub observe_peers: Vec<String>,
    /// Other peer aliases whose agents may receive prompts.
    #[serde(default)]
    pub interact_peers: Vec<String>,
}
fn deserialize_saved_machine_policies<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<crate::client::endpoint::ProfileId, FederationSavedMachinePolicy>, D::Error>
where
    D: Deserializer<'de>,
{
    let policies =
        BTreeMap::<crate::client::endpoint::ProfileId, FederationSavedMachinePolicy>::deserialize(
            deserializer,
        )?;
    for (profile_id, policy) in &policies {
        crate::client::endpoint::ProfileId::parse(profile_id.to_string())
            .map_err(de::Error::custom)?;
        validate_expected_machine_id(&policy.expected_machine_id).map_err(de::Error::custom)?;
        let mut granted = std::collections::HashSet::new();
        for grant in &policy.agent_grants {
            if grant.terminal_id.is_empty()
                || grant.session.value.is_empty()
                || !granted.insert(&grant.terminal_id)
                || !grant.observe && !grant.interact
                || grant
                    .observe_peers
                    .iter()
                    .chain(&grant.interact_peers)
                    .any(|alias| {
                        alias.is_empty() || alias == &profile_id.to_string() || alias.contains('/')
                    })
            {
                return Err(de::Error::custom(
                    "invalid or duplicate saved federation agent grant",
                ));
            }
        }
    }
    Ok(policies)
}

fn validate_expected_machine_id(machine_id: &str) -> Result<(), String> {
    if machine_id.is_empty()
        || machine_id.len() > 128
        || machine_id.trim() != machine_id
        || machine_id.chars().any(char::is_control)
    {
        return Err(
            "saved federation machine expected_machine_id must be 1-128 visible bytes with no surrounding whitespace"
                .into(),
        );
    }
    Ok(())
}

/// A directly configured federation peer.
///
/// `profile_id` and `remote_session` are populated only by the coordinator's
/// saved-profile adapter. Config deserialization rejects either field and every
/// `ssh://` endpoint in `federation.peers`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FederationPeer {
    /// Local alias used to refer to this peer.
    pub alias: String,
    /// Mutable display label. Routing continues to use `alias`.
    pub display_label: Option<String>,
    /// TCP endpoint to reach an explicitly configured peer.
    pub endpoint: Option<String>,
    /// Immutable saved-machine profile id synthesized by the coordinator.
    pub profile_id: Option<String>,
    /// Named remote session synthesized by the coordinator.
    pub remote_session: Option<String>,
    /// Filesystem path to the shared auth token for this peer.
    pub token_file: Option<String>,
    /// Expected per-install identity for this peer. Inbound connections must
    /// present it in their authenticated hello; outbound polls must report it in
    /// `agent.list`. A missing or changed value fails closed. Optional — omit to
    /// authenticate by the shared token or OpenSSH alone.
    ///
    /// Only pin peers running a build that reports an install identity. Older
    /// peers omit it and are rejected when a pin is configured.
    pub expected_node_id: Option<String>,
    /// Capability tier this peer's token grants over inbound TCP federation.
    /// Default: `observe` (read-only).
    pub capability: CapabilityTier,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    /// Allow launching herdr inside an existing herdr pane. Default: false.
    pub allow_nested: bool,
    /// Deprecated compatibility key for `terminal.kitty_graphics`.
    pub kitty_graphics: Option<bool>,
    /// Persist pane screen history to session-history.json. Default: false.
    pub pane_history: bool,
    /// Expose the focused pane's cursor anchor to the outer terminal even when
    /// the pane requested `?25l`, so macOS native input methods keep tracking
    /// the candidate window when TUIs paint their own cursor (Claude Code, pi,
    /// codex, etc.). Default: false.
    ///
    /// When the pane reports no cursor position, falls back to the pane's
    /// top-left so a stable IME anchor is always available.
    ///
    /// Trade-off when enabled: an extra hardware cursor will be visible in the
    /// outer terminal for apps that hide the cursor without painting a
    /// replacement (vim normal mode, etc.). See #149.
    pub reveal_hidden_cursor_for_cjk_ime: bool,
    /// Restrict `reveal_hidden_cursor_for_cjk_ime` to focused panes whose
    /// detected agent matches one of these names (case-insensitive). Empty
    /// list means apply to any focused pane. Unknown agent names are ignored;
    /// if the list contains no valid names, the reveal does not apply.
    /// Accepted names: pi, claude, codex, gemini, cursor, devin, cline,
    /// opencode, copilot, kimi, kiro, droid, amp, grok, hermes, kilo,
    /// qodercli, qoder, qwen, qwen-code, letta, letta-code, maki.
    /// Default: empty.
    pub cjk_ime_agents: Vec<String>,
    /// Cursor shape rendered for the IME anchor when
    /// `reveal_hidden_cursor_for_cjk_ime` is enabled. Default: "steady_block".
    pub cjk_ime_cursor_shape: ImeCursorShape,
    /// While prefix mode is active, temporarily switch the host input source
    /// to an ASCII-capable mode so prefix commands are read as ASCII even when
    /// an IME is active, then restore the previous input source when prefix
    /// mode exits. On macOS this selects the ASCII-capable keyboard layout; on
    /// Windows it switches the IME to English (ASCII) input. Windows support is
    /// currently limited to the Korean IME; with an IME for any other language,
    /// the input source is left unchanged. macOS and Windows only; a no-op
    /// elsewhere and a best-effort no-op if the switch fails.
    /// Default: false.
    pub switch_ascii_input_source_in_prefix: bool,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            prefix: BindingConfig::one("ctrl+b"),
            help: BindingConfig::one("prefix+?"),
            settings: BindingConfig::one("prefix+s"),
            new_workspace: BindingConfig::one("prefix+shift+n"),
            new_worktree: BindingConfig::one("prefix+shift+g"),
            open_worktree: BindingConfig::empty(),
            remove_worktree: BindingConfig::empty(),
            rename_workspace: BindingConfig::one("prefix+shift+w"),
            close_workspace: BindingConfig::one("prefix+shift+d"),
            workspace_picker: BindingConfig::one("prefix+w"),
            goto: BindingConfig::one("prefix+g"),
            navigate_workspace_up: BindingConfig::one("up"),
            navigate_workspace_down: BindingConfig::one("down"),
            navigate_pane_left: BindingConfig::one("h"),
            navigate_pane_down: BindingConfig::one("j"),
            navigate_pane_up: BindingConfig::one("k"),
            navigate_pane_right: BindingConfig::one("l"),
            detach: BindingConfig::one("prefix+q"),
            reload_config: BindingConfig::one("prefix+shift+r"),
            open_notification_target: BindingConfig::one("prefix+o"),
            previous_workspace: BindingConfig::empty(),
            next_workspace: BindingConfig::empty(),
            previous_agent: BindingConfig::empty(),
            next_agent: BindingConfig::empty(),
            focus_agent: BindingConfig::empty(),
            remote_image_paste: "ctrl+v".into(),
            new_tab: BindingConfig::one("prefix+c"),
            rename_tab: BindingConfig::one("prefix+shift+t"),
            previous_tab: BindingConfig::one("prefix+p"),
            next_tab: BindingConfig::one("prefix+n"),
            move_tab_previous: BindingConfig::empty(),
            move_tab_next: BindingConfig::empty(),
            switch_tab: BindingConfig::one("prefix+1..9"),
            switch_workspace: BindingConfig::empty(),
            close_tab: BindingConfig::one("prefix+shift+x"),
            rename_pane: BindingConfig::one("prefix+shift+p"),
            edit_scrollback: BindingConfig::one("prefix+e"),
            clear_pane: BindingConfig::default(),
            copy_mode: BindingConfig::one("prefix+["),
            focus_pane_left: BindingConfig::one("prefix+h"),
            focus_pane_down: BindingConfig::one("prefix+j"),
            focus_pane_up: BindingConfig::one("prefix+k"),
            focus_pane_right: BindingConfig::one("prefix+l"),
            swap_pane_left: BindingConfig::one("prefix+shift+h"),
            swap_pane_down: BindingConfig::one("prefix+shift+j"),
            swap_pane_up: BindingConfig::one("prefix+shift+k"),
            swap_pane_right: BindingConfig::one("prefix+shift+l"),
            cycle_pane_next: BindingConfig::one("prefix+tab"),
            cycle_pane_previous: BindingConfig::one("prefix+shift+tab"),
            last_pane: BindingConfig::empty(),
            split_vertical: BindingConfig::one("prefix+v"),
            split_horizontal: BindingConfig::one("prefix+minus"),
            close_pane: BindingConfig::one("prefix+x"),
            zoom: BindingConfig::one("prefix+z"),
            resize_mode: BindingConfig::one("prefix+r"),
            resize_pane_left: BindingConfig::empty(),
            resize_pane_down: BindingConfig::empty(),
            resize_pane_up: BindingConfig::empty(),
            resize_pane_right: BindingConfig::empty(),
            toggle_sidebar: BindingConfig::one("prefix+b"),
            indexed: IndexedKeysConfig::default(),
            command: Vec::new(),
            user_fields: BTreeSet::new(),
        }
    }
}

impl Default for WorktreesConfig {
    fn default() -> Self {
        Self {
            directory: "~/.herdr/worktrees".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 26,
            sidebar_min_width: 18,
            sidebar_max_width: 36,
            sidebar_start_collapsed: false,
            sidebar_collapsed_mode: SidebarCollapsedModeConfig::Compact,
            mobile_width_threshold: DEFAULT_MOBILE_WIDTH_THRESHOLD,
            mouse_capture: true,
            copy_on_select: true,
            host_cursor: HostCursorModeConfig::Auto,
            right_click_passthrough_modifier: RightClickPassthroughModifierConfig::default(),
            redraw_on_focus_gained: true,
            mouse_scroll_lines: None,
            confirm_close: true,
            prompt_new_tab_name: true,
            prompt_new_workspace_name: false,
            pane_borders: PaneBordersConfig::Auto,
            pane_outer_borders: true,
            pane_scrollbars: true,
            pane_gaps: true,
            show_agent_labels_on_pane_borders: false,
            hide_tab_bar_when_single_tab: false,
            tab_bar_position: TabBarPositionConfig::Top,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: " ".into(),
            window_title: super::window_title::default_window_title(),
            agent_panel_sort: AgentPanelSortConfig::Spaces,
            _legacy_agent_panel_scope: None,
            status_indicators: StatusIndicatorStyle::Dots,
            sidebar: SidebarConfig::default(),
            accent: "cyan".into(),
            toast: ToastConfig::default(),
            sound: SoundConfig::default(),
        }
    }
}

impl UiConfig {
    pub fn mouse_scroll_lines(&self) -> usize {
        self.mouse_scroll_lines
            .map(NonZeroUsize::get)
            .unwrap_or(DEFAULT_MOUSE_SCROLL_LINES)
    }

    pub fn right_click_passthrough_modifiers(&self) -> Option<KeyModifiers> {
        self.right_click_passthrough_modifier.modifiers()
    }
}

impl Default for ToastConfig {
    fn default() -> Self {
        Self {
            delivery: ToastDelivery::Off,
            delay_seconds: 1,
            herdr: HerdrToastConfig::default(),
            clipboard: ClipboardToastConfig::default(),
        }
    }
}

impl Default for HerdrToastConfig {
    fn default() -> Self {
        Self {
            position: ToastHerdrPosition::BottomRight,
        }
    }
}

impl Default for ClipboardToastConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position: ToastClipboardPosition::BottomCenter,
        }
    }
}

impl<'de> Deserialize<'de> for ToastConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct RawToastConfig {
            delivery: Option<ToastDelivery>,
            enabled: Option<bool>,
            delay_seconds: Option<u64>,
            herdr: HerdrToastConfig,
            clipboard: ClipboardToastConfig,
        }

        let raw = RawToastConfig::deserialize(deserializer)?;
        let legacy_delivery = match raw.enabled {
            Some(true) => ToastDelivery::Herdr,
            Some(false) | None => ToastDelivery::Off,
        };
        let delivery = raw.delivery.unwrap_or(legacy_delivery);
        let default = Self::default();
        let delay_seconds = raw.delay_seconds.unwrap_or(default.delay_seconds);
        if delay_seconds > MAX_TOAST_DELAY_SECONDS {
            return Err(de::Error::custom(format!(
                "ui.toast.delay_seconds must be between 0 and {MAX_TOAST_DELAY_SECONDS}"
            )));
        }
        Ok(Self {
            delivery,
            delay_seconds,
            herdr: raw.herdr,
            clipboard: raw.clipboard,
        })
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            allow_unelevated_clients: false,
            headless_cols: crate::config::DEFAULT_HEADLESS_COLS,
            headless_rows: crate::config::DEFAULT_HEADLESS_ROWS,
        }
    }
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            scrollback_limit_bytes: DEFAULT_SCROLLBACK_LIMIT_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_federation_machine_policy_is_keyed_by_valid_profile_id() {
        let profile_id = "0123456789abcdef0123456789abcdef";
        let config: FederationConfig = toml::from_str(&format!(
            r#"
                [saved_machines."{profile_id}"]
                expected_machine_id = "machine_0123456789abcdef0123456789abcdef"
            "#
        ))
        .unwrap();

        let key = crate::client::endpoint::ProfileId::parse(profile_id).unwrap();
        assert_eq!(
            config.saved_machines[&key].expected_machine_id,
            "machine_0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn saved_federation_machine_policy_fails_closed_without_identity() {
        let missing_identity = toml::from_str::<FederationConfig>(
            r#"
                [saved_machines."0123456789abcdef0123456789abcdef"]
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(missing_identity.contains("expected_machine_id"));

        let invalid_profile = toml::from_str::<FederationConfig>(
            r#"
                [saved_machines.not-a-profile-id]
                expected_machine_id = "machine_0123456789abcdef0123456789abcdef"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(invalid_profile.contains("32 lowercase hexadecimal"));
    }

    #[test]
    fn saved_federation_machine_policy_rejects_credentials() {
        let error = toml::from_str::<FederationConfig>(
            r#"
                [saved_machines."0123456789abcdef0123456789abcdef"]
                expected_machine_id = "machine_0123456789abcdef0123456789abcdef"
                token_file = "/secret/token"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn explicit_ssh_federation_peer_requires_saved_machine_policy() {
        let error = toml::from_str::<FederationConfig>(
            r#"
                coordinator = true
                [[peers]]
                alias = "build"
                endpoint = "ssh://dev@build.example"
                profile_id = "0123456789abcdef0123456789abcdef"
                remote_session = "agent-work"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("must use federation.saved_machines"));
    }

    fn coordinator_federation_config(named_sessions: &str) -> FederationConfig {
        toml::from_str(&format!(
            r#"
                coordinator = true
                listen = true
                listen_addr = "127.0.0.1:7020"
                reverse_coordinator_machine_id = "machine_0123456789abcdef0123456789abcdef"
                named_sessions = {named_sessions}

                [[peers]]
                alias = "laptop"
                token_file = "/etc/herdr/peers/laptop.token"

                [[peers]]
                alias = "build"
                endpoint = "tcp://build.internal:7020"
                token_file = "/etc/herdr/peers/build.token"

                [saved_machines."0123456789abcdef0123456789abcdef"]
                expected_machine_id = "machine_0123456789abcdef0123456789abcdef"
            "#
        ))
        .unwrap()
    }

    #[test]
    fn federation_for_unlisted_named_session_drops_coordinator_outbound_and_listener() {
        let config = coordinator_federation_config(r#"["ops"]"#);
        let effective = config.for_session(Some("work"));

        assert!(matches!(effective, std::borrow::Cow::Owned(_)));
        assert!(!effective.coordinator);
        assert!(!effective.listen);
        assert_eq!(
            effective
                .peers
                .iter()
                .map(|peer| peer.alias.as_str())
                .collect::<Vec<_>>(),
            ["laptop"],
            "only the inbound-only peer survives; no outbound polling"
        );
        // Remote-side acceptance and trust policy stay, so the session remains
        // a valid SSH target and `machine status` reads "coordinator disabled".
        assert_eq!(
            effective.reverse_coordinator_machine_id,
            config.reverse_coordinator_machine_id
        );
        assert_eq!(effective.saved_machines, config.saved_machines);
    }

    #[test]
    fn federation_for_listed_named_session_is_unchanged() {
        let config = coordinator_federation_config(r#"["ops", "work"]"#);
        let effective = config.for_session(Some("work"));

        assert!(matches!(effective, std::borrow::Cow::Borrowed(_)));
        assert!(effective.coordinator && effective.listen);
        assert_eq!(effective.peers.len(), 2);
    }

    #[test]
    fn federation_for_default_session_is_unchanged() {
        let config = coordinator_federation_config("[]");
        for session in [None, Some(crate::session::DEFAULT_SESSION_NAME)] {
            let effective = config.for_session(session);
            assert!(matches!(effective, std::borrow::Cow::Borrowed(_)));
            assert!(effective.coordinator && effective.listen);
            assert_eq!(effective.peers.len(), 2);
        }

        // `"default"` in the list is redundant and harmless.
        let redundant = coordinator_federation_config(r#"["default"]"#);
        assert!(redundant.federates_session(None));
        assert!(!redundant.federates_session(Some("work")));
    }

    #[test]
    fn federation_session_rule_is_silent_when_nothing_is_suppressed() {
        let config: FederationConfig = toml::from_str(
            r#"
                reverse_coordinator_machine_id = "machine_0123456789abcdef0123456789abcdef"
                [[peers]]
                alias = "laptop"
                token_file = "/etc/herdr/peers/laptop.token"
            "#,
        )
        .unwrap();
        assert!(matches!(
            config.for_session(Some("work")),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn federation_named_sessions_reject_invalid_names() {
        let error = toml::from_str::<FederationConfig>(r#"named_sessions = ["../escape"]"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("federation.named_sessions"), "{error}");
        assert!(toml::from_str::<FederationConfig>(r#"named_sessions = [""]"#).is_err());
    }

    #[test]
    fn env_var_for_kind_maps_supported_harnesses() {
        assert_eq!(env_var_for_kind("claude"), Some("CLAUDE_CONFIG_DIR"));
        assert_eq!(env_var_for_kind("codex"), Some("CODEX_HOME"));
        assert_eq!(env_var_for_kind("omp"), Some("PI_CODING_AGENT_DIR"));
        assert_eq!(env_var_for_kind("kimi"), Some("KIMI_CODE_HOME"));
        assert_eq!(env_var_for_kind("gemini"), None);
        assert_eq!(env_var_for_kind(""), None);
    }

    #[test]
    fn auth_env_vars_to_clear_covers_account_overrides() {
        assert_eq!(
            auth_env_vars_to_clear("claude"),
            &["CLAUDE_CODE_OAUTH_TOKEN"]
        );
        assert_eq!(auth_env_vars_to_clear("codex"), &[] as &[&str]);
        assert_eq!(
            auth_env_vars_to_clear("omp"),
            &["OMP_PROFILE", "PI_PROFILE"]
        );
        assert_eq!(auth_env_vars_to_clear("kimi"), &[] as &[&str]);
        assert_eq!(auth_env_vars_to_clear("gemini"), &[] as &[&str]);
        assert_eq!(auth_env_vars_to_clear(""), &[] as &[&str]);
    }

    #[test]
    fn kind_for_config_env_var_is_the_reverse_of_env_var_for_kind() {
        for kind in ["claude", "codex", "omp", "kimi"] {
            let var = env_var_for_kind(kind).expect("supported kind has a config var");
            assert_eq!(kind_for_config_env_var(var), Some(kind));
        }
        assert_eq!(kind_for_config_env_var("PATH"), None);
        assert_eq!(kind_for_config_env_var(""), None);
    }

    #[test]
    fn account_launch_env_builds_config_home_pair() {
        let account = AccountConfig {
            id: "work".into(),
            kind: "codex".into(),
            label: "Work".into(),
            config_dir: "/home/x/.codex-work".into(),
        };
        assert_eq!(
            account.launch_env(),
            Some(AccountLaunchEnv {
                vars: vec![("CODEX_HOME".to_string(), "/home/x/.codex-work".to_string())],
                // codex has no auth-token lever today, so nothing to clear.
                clear_vars: Vec::new(),
            })
        );

        let omp = AccountConfig {
            id: "omp-work".into(),
            kind: "omp".into(),
            label: "OMP Work".into(),
            config_dir: "/home/x/.omp-work/agent".into(),
        };
        let omp_env = AccountLaunchEnv {
            vars: vec![(
                "PI_CODING_AGENT_DIR".to_string(),
                "/home/x/.omp-work/agent".to_string(),
            )],
            clear_vars: vec!["OMP_PROFILE".to_string(), "PI_PROFILE".to_string()],
        };
        assert_eq!(omp.launch_env(), Some(omp_env.clone()));
        assert_eq!(
            AccountLaunchEnv::from_resolved_vars(omp_env.vars.clone()),
            omp_env
        );

        let unknown = AccountConfig {
            kind: "gemini".into(),
            ..omp
        };
        assert_eq!(unknown.launch_env(), None);
    }

    fn restore_home(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn default_config_dir_is_home_relative_per_kind() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let previous = ["HOME", "PI_CODING_AGENT_DIR", "PI_CONFIG_DIR"]
            .map(|key| (key, std::env::var_os(key)));
        std::env::set_var("HOME", "/home/tester");
        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::env::remove_var("PI_CONFIG_DIR");
        assert_eq!(
            default_config_dir("claude"),
            Some(PathBuf::from("/home/tester/.claude"))
        );
        assert_eq!(
            default_config_dir("codex"),
            Some(PathBuf::from("/home/tester/.codex"))
        );
        assert_eq!(
            default_config_dir("kimi"),
            Some(PathBuf::from("/home/tester/.kimi-code"))
        );
        assert_eq!(
            default_config_dir("omp"),
            Some(PathBuf::from("/home/tester/.omp/agent"))
        );
        assert_eq!(default_config_dir("gemini"), None);
        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn omp_default_config_dir_honors_native_overrides() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let previous = ["HOME", "PI_CODING_AGENT_DIR", "PI_CONFIG_DIR"]
            .map(|key| (key, std::env::var_os(key)));
        std::env::set_var("HOME", "/home/tester");
        std::env::set_var("PI_CODING_AGENT_DIR", "~/omp-work");
        std::env::remove_var("PI_CONFIG_DIR");
        assert_eq!(
            default_config_dir("omp"),
            Some(PathBuf::from("/home/tester/omp-work"))
        );

        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::env::set_var("PI_CONFIG_DIR", ".omp-alt");
        assert_eq!(
            default_config_dir("omp"),
            Some(PathBuf::from("/home/tester/.omp-alt/agent"))
        );

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn omp_session_root_is_below_the_complete_agent_directory() {
        let agent_dir = PathBuf::from("/home/tester/.omp/agent");
        assert_eq!(omp_sessions_dir(&agent_dir), agent_dir.join("sessions"));
    }

    #[test]
    fn primary_default_dir_account_injects_no_env_but_secondary_does() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/tester");

        // Primary account at the DEFAULT config-home -> NO config-home override, so it
        // launches like a default install (issue #94) while staying selectable. It still
        // clears the conflicting auth token: a global CLAUDE_CODE_OAUTH_TOKEN authenticates
        // as whichever account minted it and would silently outrank this explicit choice.
        let primary = AccountConfig {
            id: "claude-main".into(),
            kind: "claude".into(),
            label: "main".into(),
            config_dir: "/home/tester/.claude".into(),
        };
        let primary_env = primary
            .launch_env()
            .expect("claude has a config-home lever");
        assert!(
            primary_env.vars.is_empty(),
            "injecting the override on a default config-home strands ~/.claude.json (issue #94)"
        );
        assert_eq!(
            primary_env.clear_vars,
            vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()],
            "a selected account must clear credentials that outrank it, even when it sets \
             no override — skipping this is how a global token silently re-homed the fleet"
        );
        assert!(is_default_config_dir("claude", "/home/tester/.claude"));
        // Trailing slash still resolves as the default (component-wise compare).
        assert!(is_default_config_dir("claude", "/home/tester/.claude/"));
        // Kind matters: $HOME/.claude is not codex's default home.
        assert!(!is_default_config_dir("codex", "/home/tester/.claude"));

        // A distinct secondary dir still injects the override.
        let secondary = AccountConfig {
            config_dir: "/home/tester/.claude-2".into(),
            ..primary.clone()
        };
        assert_eq!(
            secondary.launch_env(),
            Some(AccountLaunchEnv {
                vars: vec![(
                    "CLAUDE_CONFIG_DIR".to_string(),
                    "/home/tester/.claude-2".to_string()
                )],
                clear_vars: vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()],
            })
        );
        assert!(!is_default_config_dir("claude", "/home/tester/.claude-2"));

        restore_home(prev);
    }

    #[test]
    fn accounts_registry_round_trips_from_toml() {
        let toml = r#"
[[accounts]]
id = "work"
kind = "codex"
label = "Work"
config_dir = "/home/x/.codex-work"

[[accounts]]
id = "personal"
kind = "claude"
label = "Personal"
config_dir = "/home/x/.claude-personal"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.accounts[0].id, "work");
        assert_eq!(config.accounts[0].kind, "codex");
        assert_eq!(config.accounts[0].config_dir, "/home/x/.codex-work");
        assert_eq!(config.accounts[1].id, "personal");
        assert_eq!(config.accounts[1].kind, "claude");
        assert_eq!(
            config.accounts[1].launch_env(),
            Some(AccountLaunchEnv {
                vars: vec![(
                    "CLAUDE_CONFIG_DIR".to_string(),
                    "/home/x/.claude-personal".to_string()
                )],
                clear_vars: vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()],
            })
        );

        // Absent section defaults to an empty registry.
        assert!(Config::default().accounts.is_empty());
    }

    #[test]
    fn update_config_defaults_and_parses() {
        let default_config = Config::default();
        assert_eq!(default_config.update.channel, default_update_channel());
        assert!(default_config.update.version_check);
        assert!(default_config.update.manifest_check);

        let toml = r#"
[update]
channel = "preview"
version_check = false
manifest_check = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.update.channel, UpdateChannelConfig::Preview);
        assert_eq!(config.update.channel.as_str(), "preview");
        assert!(!config.update.version_check);
        assert!(!config.update.manifest_check);
    }

    #[test]
    fn push_config_defaults_off_and_parses() {
        let default_config = Config::default();
        assert!(!default_config.push.enabled);
        assert!(default_config.push.key_path.is_none());
        assert!(!default_config.push.sandbox);
        assert_eq!(default_config.push.mode, PushMode::Auto);
        assert_eq!(default_config.push.relay_url, DEFAULT_PUSH_RELAY_URL);

        // A partial [push] table keeps the relay defaults rather than blanking them.
        let partial: Config = toml::from_str("[push]\nenabled = true\n").unwrap();
        assert_eq!(partial.push.mode, PushMode::Auto);
        assert_eq!(partial.push.relay_url, DEFAULT_PUSH_RELAY_URL);

        let relay: Config =
            toml::from_str("[push]\nmode = \"relay\"\nrelay_url = \"https://relay.test\"\n")
                .unwrap();
        assert_eq!(relay.push.mode, PushMode::Relay);
        assert_eq!(relay.push.relay_url, "https://relay.test");
        assert!(toml::from_str::<Config>("[push]\nmode = \"sometimes\"\n").is_err());

        let toml = r#"
[push]
enabled = true
key_path = "~/secrets/AuthKey_ABC123DEFG.p8"
key_id = "ABC123DEFG"
team_id = "TEAM123456"
topic = "com.example.herdr"
sandbox = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.push.enabled);
        assert_eq!(
            config.push.key_path.as_deref(),
            Some("~/secrets/AuthKey_ABC123DEFG.p8")
        );
        assert_eq!(config.push.key_id.as_deref(), Some("ABC123DEFG"));
        assert_eq!(config.push.team_id.as_deref(), Some("TEAM123456"));
        assert_eq!(config.push.topic.as_deref(), Some("com.example.herdr"));
        assert!(config.push.sandbox);
    }

    #[test]
    fn update_channel_default_follows_build_identity_on_every_platform() {
        assert_eq!(
            default_update_channel_for_build(true),
            UpdateChannelConfig::Preview
        );
        assert_eq!(
            default_update_channel_for_build(false),
            UpdateChannelConfig::Stable
        );
    }

    #[test]
    fn missing_update_channel_uses_build_default() {
        let empty: Config = toml::from_str("").unwrap();
        let without_update_channel: Config =
            toml::from_str("[update]\nversion_check = false").unwrap();

        assert_eq!(Config::default().update.channel, default_update_channel());
        assert_eq!(empty.update.channel, default_update_channel());
        assert_eq!(
            without_update_channel.update.channel,
            default_update_channel()
        );
    }

    #[test]
    fn terminal_default_shell_defaults_empty_and_parses() {
        let default_config = Config::default();
        assert!(default_config.terminal.default_shell.is_empty());
        assert_eq!(default_config.terminal.shell_mode, ShellModeConfig::Auto);

        let toml = r#"
[terminal]
default_shell = "nu"
shell_mode = "non_login"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.terminal.default_shell, "nu");
        assert_eq!(config.terminal.shell_mode, ShellModeConfig::NonLogin);
    }

    #[test]
    fn terminal_new_cwd_defaults_follow_and_parses() {
        let default_config = Config::default();
        assert_eq!(
            default_config.terminal.new_cwd,
            NewTerminalCwdConfig::Follow
        );

        let config: Config = toml::from_str(
            r#"
[terminal]
new_cwd = "home"
"#,
        )
        .unwrap();
        assert_eq!(config.terminal.new_cwd, NewTerminalCwdConfig::Home);

        let config: Config = toml::from_str(
            r#"
[terminal]
new_cwd = "~/Projects"
"#,
        )
        .unwrap();
        assert_eq!(
            config.terminal.new_cwd,
            NewTerminalCwdConfig::Path("~/Projects".into())
        );
    }

    #[test]
    fn resume_agents_on_restore_defaults_on_and_parses() {
        let default_config = Config::default();
        assert!(default_config.session.resume_agents_on_restore);
        assert_eq!(default_config.session.startup_per_agent_delay_ms, 100);

        let toml = r#"
[session]
resume_agents_on_restore = false
startup_per_agent_delay_ms = 0
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.session.resume_agents_on_restore);
        assert_eq!(config.session.startup_per_agent_delay_ms, 0);
    }

    #[test]
    fn agent_panel_sort_config_parses_alias_and_defaults() {
        assert_eq!(
            Config::default().ui.agent_panel_sort,
            AgentPanelSortConfig::Spaces
        );

        let toml = r#"
[ui]
agent_panel_sort = "priority"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.agent_panel_sort, AgentPanelSortConfig::Priority);

        let toml = r#"
[ui]
agent_panel_sort = "workspaces"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.agent_panel_sort, AgentPanelSortConfig::Spaces);

        let toml = r#"
[ui]
agent_panel_scope = "current"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.agent_panel_sort, AgentPanelSortConfig::Spaces);
    }

    #[test]
    fn status_indicator_style_defaults_to_dots_and_parses_symbols() {
        assert_eq!(
            Config::default().ui.status_indicators,
            StatusIndicatorStyle::Dots
        );

        let config: Config = toml::from_str(
            r#"
[ui]
status_indicators = "symbols"
"#,
        )
        .unwrap();
        assert_eq!(config.ui.status_indicators, StatusIndicatorStyle::Symbols);
    }

    #[test]
    fn pane_borders_legacy_booleans_map_to_modes() {
        let enabled: Config = toml::from_str("[ui]\npane_borders = true").unwrap();
        assert_eq!(enabled.ui.pane_borders, PaneBordersConfig::Auto);

        let disabled: Config = toml::from_str("[ui]\npane_borders = false").unwrap();
        assert_eq!(disabled.ui.pane_borders, PaneBordersConfig::Off);

        let auto: Config = toml::from_str("[ui]\npane_borders = \"auto\"").unwrap();
        assert_eq!(auto.ui.pane_borders, PaneBordersConfig::Auto);

        let off: Config = toml::from_str("[ui]\npane_borders = \"off\"").unwrap();
        assert_eq!(off.ui.pane_borders, PaneBordersConfig::Off);

        let unknown = toml::from_str::<Config>("[ui]\npane_borders = \"framed\"")
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("\"auto\", \"always\", \"off\", or a legacy boolean"));

        let wrong_type = toml::from_str::<Config>("[ui]\npane_borders = 3")
            .unwrap_err()
            .to_string();
        assert!(wrong_type.contains("\"auto\", \"always\", \"off\", or a legacy boolean"));
    }

    #[test]
    fn pane_appearance_defaults_and_parse() {
        let default_config = Config::default();
        assert_eq!(default_config.ui.pane_borders, PaneBordersConfig::Auto);
        assert!(default_config.ui.pane_outer_borders);
        assert!(default_config.ui.pane_scrollbars);
        assert!(default_config.ui.pane_gaps);
        assert!(!default_config.ui.show_agent_labels_on_pane_borders);
        assert!(!default_config.ui.hide_tab_bar_when_single_tab);
        assert_eq!(
            default_config.ui.tab_bar_position,
            TabBarPositionConfig::Top
        );
        assert!(default_config.ui.tab_bar_right.is_empty());
        assert_eq!(default_config.ui.tab_bar_right_separator, " ");

        let toml = r#"
[ui]
pane_borders = "always"
pane_outer_borders = false
pane_scrollbars = false
pane_gaps = true
show_agent_labels_on_pane_borders = true
hide_tab_bar_when_single_tab = true
tab_bar_position = "bottom"
tab_bar_right = [
  { type = "zoom" },
  { type = "hostname" },
  { type = "datetime", format = "%H:%M" },
  { type = "text", text = "prod" },
  { type = "command", command = "status.sh", interval_seconds = 10, timeout_seconds = 3 },
]
tab_bar_right_separator = " · "
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.pane_borders, PaneBordersConfig::Always);
        assert!(!config.ui.pane_outer_borders);
        assert!(!config.ui.pane_scrollbars);
        assert!(config.ui.pane_gaps);
        assert!(config.ui.show_agent_labels_on_pane_borders);
        assert!(config.ui.hide_tab_bar_when_single_tab);
        assert_eq!(config.ui.tab_bar_position, TabBarPositionConfig::Bottom);
        assert_eq!(config.ui.tab_bar_right.len(), 5);
        assert!(matches!(
            config.ui.tab_bar_right[1],
            TabBarRightEntryConfig::Hostname
        ));
        assert_eq!(config.ui.tab_bar_right_separator, " · ");
    }

    #[test]
    fn worktrees_directory_defaults_and_parses() {
        let default_config = Config::default();
        assert_eq!(default_config.worktrees.directory, "~/.herdr/worktrees");

        let toml = r#"
[worktrees]
directory = "~/Projects/herdr-worktrees"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.worktrees.directory, "~/Projects/herdr-worktrees");
    }

    #[test]
    fn prompt_new_tab_name_defaults_on_and_parses() {
        let default_config = Config::default();
        assert!(default_config.ui.prompt_new_tab_name);

        let toml = r#"
[ui]
prompt_new_tab_name = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.ui.prompt_new_tab_name);
    }

    #[test]
    fn prompt_new_workspace_name_defaults_off_and_parses() {
        let default_config = Config::default();
        assert!(!default_config.ui.prompt_new_workspace_name);

        let toml = r#"
[ui]
prompt_new_workspace_name = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.prompt_new_workspace_name);
    }

    #[test]
    fn reveal_hidden_cursor_for_cjk_ime_default_off_and_parse() {
        let default_config = Config::default();
        assert!(!default_config.experimental.reveal_hidden_cursor_for_cjk_ime);

        let toml = r#"
[experimental]
reveal_hidden_cursor_for_cjk_ime = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.experimental.reveal_hidden_cursor_for_cjk_ime);
    }

    #[test]
    fn switch_ascii_input_source_in_prefix_default_off_and_parse() {
        let default_config = Config::default();
        assert!(
            !default_config
                .experimental
                .switch_ascii_input_source_in_prefix
        );

        let toml = r#"
[experimental]
switch_ascii_input_source_in_prefix = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.experimental.switch_ascii_input_source_in_prefix);
    }

    #[test]
    fn cjk_ime_cursor_shape_default_steady_block_and_parse() {
        let default_config = Config::default();
        assert_eq!(
            default_config.experimental.cjk_ime_cursor_shape,
            ImeCursorShape::SteadyBlock
        );

        let toml = r#"
[experimental]
cjk_ime_cursor_shape = "bar"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.experimental.cjk_ime_cursor_shape,
            ImeCursorShape::Bar
        );
    }

    #[test]
    fn cjk_ime_agents_default_empty_and_parse() {
        let default_config = Config::default();
        assert!(default_config.experimental.cjk_ime_agents.is_empty());

        let toml = r#"
[experimental]
cjk_ime_agents = ["claude", "codex"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.experimental.cjk_ime_agents,
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn sidebar_bounds_default_and_parse() {
        let default_config = Config::default();
        assert_eq!(default_config.ui.sidebar_min_width, 18);
        assert_eq!(default_config.ui.sidebar_max_width, 36);
        assert_eq!(
            default_config.ui.mobile_width_threshold,
            DEFAULT_MOBILE_WIDTH_THRESHOLD
        );

        let toml = r#"
[ui]
sidebar_min_width = 12
sidebar_max_width = 80
mobile_width_threshold = 96
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.sidebar_min_width, 12);
        assert_eq!(config.ui.sidebar_max_width, 80);
        assert_eq!(config.ui.mobile_width_threshold, 96);
    }

    #[test]
    fn sidebar_start_collapsed_defaults_off_and_parses_on() {
        let default_config = Config::default();
        assert!(!default_config.ui.sidebar_start_collapsed);

        let toml = r#"
[ui]
sidebar_start_collapsed = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.sidebar_start_collapsed);
    }

    #[test]
    fn sidebar_collapsed_mode_defaults_compact_and_parses_hidden() {
        let default_config = Config::default();
        assert_eq!(
            default_config.ui.sidebar_collapsed_mode,
            SidebarCollapsedModeConfig::Compact
        );

        let toml = r#"
[ui]
sidebar_collapsed_mode = "hidden"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.ui.sidebar_collapsed_mode,
            SidebarCollapsedModeConfig::Hidden
        );
    }

    #[test]
    fn validated_sidebar_bounds_rejects_inverted() {
        assert_eq!(validated_sidebar_bounds(18, 36), Some((18, 36)));
        assert_eq!(validated_sidebar_bounds(20, 20), Some((20, 20)));
        assert_eq!(validated_sidebar_bounds(0, u16::MAX), Some((0, u16::MAX)));
        assert_eq!(validated_sidebar_bounds(50, 30), None);
        assert_eq!(validated_sidebar_bounds(u16::MAX, 0), None);
    }

    #[test]
    fn mouse_capture_default_on_and_parse() {
        let default_config = Config::default();
        assert!(default_config.ui.mouse_capture);

        let toml = r#"
[ui]
mouse_capture = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.ui.mouse_capture);
    }

    #[test]
    fn copy_on_select_default_on_and_parse() {
        let default_config = Config::default();
        assert!(default_config.ui.copy_on_select);

        let toml = r#"
[ui]
copy_on_select = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.ui.copy_on_select);
    }

    #[test]
    fn right_click_passthrough_modifier_defaults_off_and_parses() {
        let default_config = Config::default();
        assert_eq!(default_config.ui.right_click_passthrough_modifiers(), None);

        for value in ["", "off", "none", "disabled"] {
            let toml = format!(
                r#"
[ui]
right_click_passthrough_modifier = "{value}"
"#
            );
            let config: Config = toml::from_str(&toml).unwrap();
            assert_eq!(
                config.ui.right_click_passthrough_modifiers(),
                None,
                "value {value:?} should disable passthrough"
            );
        }

        for (value, expected) in [
            ("ctrl", KeyModifiers::CONTROL),
            ("control", KeyModifiers::CONTROL),
            ("alt", KeyModifiers::ALT),
            ("option", KeyModifiers::ALT),
            ("cmd", KeyModifiers::SUPER),
            ("command", KeyModifiers::SUPER),
            ("super", KeyModifiers::SUPER),
            ("meta", KeyModifiers::META),
            ("hyper", KeyModifiers::HYPER),
        ] {
            let toml = format!(
                r#"
[ui]
right_click_passthrough_modifier = "{value}"
"#
            );
            let config: Config = toml::from_str(&toml).unwrap();
            assert_eq!(
                config.ui.right_click_passthrough_modifiers(),
                Some(expected),
                "value {value:?} should parse"
            );
        }

        let toml = r#"
[ui]
right_click_passthrough_modifier = "cmd+alt"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.ui.right_click_passthrough_modifiers(),
            Some(KeyModifiers::SUPER | KeyModifiers::ALT)
        );
    }

    #[test]
    fn right_click_passthrough_modifier_rejects_shift() {
        for value in ["shift", "shift+ctrl", "ctrl+", "ctrl++alt", "banana"] {
            let toml = format!(
                r#"
[ui]
right_click_passthrough_modifier = "{value}"
"#
            );
            assert!(
                toml::from_str::<Config>(&toml).is_err(),
                "value {value:?} should be rejected"
            );
        }
    }

    #[test]
    fn redraw_on_focus_gained_default_on_and_parse() {
        let default_config = Config::default();
        assert!(default_config.ui.redraw_on_focus_gained);

        let toml = r#"
[ui]
redraw_on_focus_gained = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.ui.redraw_on_focus_gained);
    }

    #[test]
    fn mouse_scroll_lines_defaults_to_three_and_parses() {
        let default_config = Config::default();
        assert_eq!(
            default_config.ui.mouse_scroll_lines(),
            DEFAULT_MOUSE_SCROLL_LINES
        );

        let toml = r#"
[ui]
mouse_scroll_lines = 1
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.mouse_scroll_lines(), 1);
    }

    #[test]
    fn mouse_scroll_lines_rejects_zero() {
        let toml = r#"
[ui]
mouse_scroll_lines = 0
"#;
        assert!(toml::from_str::<Config>(toml).is_err());
    }

    #[test]
    fn toast_config_parses() {
        let toml = r#"
[ui.toast]
delivery = "terminal"
delay_seconds = 2

[ui.toast.herdr]
position = "top-left"

[ui.toast.clipboard]
enabled = false
position = "top-center"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Terminal);
        assert_eq!(config.ui.toast.delay_seconds, 2);
        assert_eq!(config.ui.toast.herdr.position, ToastHerdrPosition::TopLeft);
        assert!(!config.ui.toast.clipboard.enabled);
        assert_eq!(
            config.ui.toast.clipboard.position,
            ToastClipboardPosition::TopCenter
        );
    }

    #[test]
    fn toast_config_defaults_preserve_existing_behavior_with_delay() {
        let config = Config::default();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Off);
        assert_eq!(config.ui.toast.delay_seconds, 1);
        assert_eq!(
            config.ui.toast.herdr.position,
            ToastHerdrPosition::BottomRight
        );
        assert!(config.ui.toast.clipboard.enabled);
        assert_eq!(
            config.ui.toast.clipboard.position,
            ToastClipboardPosition::BottomCenter
        );
    }

    #[test]
    fn toast_config_parses_system_delivery() {
        let toml = r#"
[ui.toast]
delivery = "system"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::System);
    }

    #[test]
    fn toast_config_legacy_enabled_true_maps_to_herdr() {
        let toml = r#"
[ui.toast]
enabled = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Herdr);
    }

    #[test]
    fn toast_config_legacy_enabled_false_maps_to_off() {
        let toml = r#"
[ui.toast]
enabled = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Off);
    }

    #[test]
    fn toast_config_delivery_wins_over_legacy_enabled() {
        let toml = r#"
[ui.toast]
enabled = true
delivery = "terminal"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Terminal);
    }

    #[test]
    fn toast_config_rejects_unbounded_delay() {
        let toml = format!(
            r#"
[ui.toast]
delay_seconds = {}
"#,
            MAX_TOAST_DELAY_SECONDS + 1
        );

        let error = toml::from_str::<Config>(&toml).unwrap_err().to_string();

        assert!(error.contains("ui.toast.delay_seconds must be between 0 and 3600"));
    }

    #[test]
    fn missing_onboarding_shows_setup() {
        let config = Config::default();
        assert!(config.should_show_onboarding());
    }

    #[test]
    fn onboarding_false_skips_setup() {
        let config: Config = toml::from_str("onboarding = false").unwrap();
        assert!(!config.should_show_onboarding());
    }

    #[test]
    fn server_headless_size_defaults_and_parses() {
        let default_config = Config::default();
        assert_eq!(
            default_config.server.headless_cols,
            crate::config::DEFAULT_HEADLESS_COLS
        );
        assert_eq!(
            default_config.server.headless_rows,
            crate::config::DEFAULT_HEADLESS_ROWS
        );

        let config: Config = toml::from_str(
            r#"[server]
headless_cols = 160
headless_rows = 50
"#,
        )
        .unwrap();
        assert_eq!(config.server.headless_cols, 160);
        assert_eq!(config.server.headless_rows, 50);

        let invalid: Config = toml::from_str(
            r#"[server]
headless_cols = 0
headless_rows = 50
"#,
        )
        .unwrap();
        assert!(invalid.invalid_headless_size_diagnostic().is_some());
        assert_eq!(
            invalid.headless_size(),
            (
                crate::config::DEFAULT_HEADLESS_COLS,
                crate::config::DEFAULT_HEADLESS_ROWS
            )
        );
    }

    #[test]
    fn advanced_defaults_include_scrollback_limit_bytes() {
        let config = Config::default();
        assert_eq!(
            config.advanced.scrollback_limit_bytes,
            DEFAULT_SCROLLBACK_LIMIT_BYTES
        );
    }

    #[test]
    fn pane_history_persistence_is_opt_in() {
        assert!(!Config::default().experimental.pane_history);

        let toml = r#"
[experimental]
pane_history = true
"#;
        let config: Config = toml::from_str(toml).unwrap();

        assert!(config.experimental.pane_history);
    }

    #[test]
    fn kitty_graphics_default_on_with_stable_opt_out() {
        assert!(Config::default().kitty_graphics_enabled());

        let config: Config = toml::from_str(
            r#"
[terminal]
kitty_graphics = false
"#,
        )
        .unwrap();
        assert!(!config.kitty_graphics_enabled());
    }

    #[test]
    fn legacy_experimental_kitty_graphics_setting_remains_compatible() {
        let disabled: Config = toml::from_str(
            r#"
[experimental]
kitty_graphics = false
"#,
        )
        .unwrap();
        assert!(!disabled.kitty_graphics_enabled());

        let stable_setting_wins: Config = toml::from_str(
            r#"
[terminal]
kitty_graphics = false

[experimental]
kitty_graphics = true
"#,
        )
        .unwrap();
        assert!(!stable_setting_wins.kitty_graphics_enabled());
    }

    #[test]
    fn experimental_config_parses() {
        let toml = r#"
[experimental]
allow_nested = true
kitty_graphics = true
pane_history = true
switch_ascii_input_source_in_prefix = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.experimental.allow_nested);
        assert_eq!(config.experimental.kitty_graphics, Some(true));
        assert!(config.kitty_graphics_enabled());
        assert!(config.experimental.pane_history);
        assert!(config.experimental.switch_ascii_input_source_in_prefix);
    }

    #[test]
    fn advanced_config_parses() {
        let toml = r#"
[advanced]
scrollback_limit_bytes = 12345
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.advanced.scrollback_limit_bytes, 12345);
    }

    #[test]
    fn advanced_legacy_scrollback_lines_alias_parses() {
        let toml = r#"
[advanced]
scrollback_lines = 12345
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.advanced.scrollback_limit_bytes, 12345);
    }
}
