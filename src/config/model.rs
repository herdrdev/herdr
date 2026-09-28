use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
};

use crossterm::event::KeyModifiers;
use serde::{de, Deserialize, Deserializer, Serialize};

use super::{
    ActionKeybinds, BindingConfig, CommandKeybindConfig, IndexedKeybind, Keybinds, SidebarConfig,
    SoundConfig, TabBarRightEntryConfig, ThemeConfig, DEFAULT_MOBILE_WIDTH_THRESHOLD,
    DEFAULT_MOUSE_SCROLL_LINES, DEFAULT_SCROLLBACK_LIMIT_BYTES,
};

pub const MAX_TOAST_DELAY_SECONDS: u64 = 3600;
pub(crate) const MAX_NUDGE_AFTER_MINUTES: u64 = 7 * 24 * 60;

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
    default_update_channel_for_build(cfg!(windows), crate::build_info::is_preview())
}

fn default_update_channel_for_build(is_windows: bool, is_preview: bool) -> UpdateChannelConfig {
    if is_windows && is_preview {
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

/// Outer-terminal protocol used for `ui.toast.delivery = "terminal"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TerminalNotificationBackend {
    /// Detect the protocol from the attaching terminal's environment.
    #[default]
    Auto,
    /// OSC 9, supported by WezTerm, iTerm2, and Ghostty.
    Osc9,
    /// OSC 99, supported by Kitty.
    Osc99,
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

impl AgentPanelSortConfig {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spaces => "spaces",
            Self::Priority => "priority",
        }
    }
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
    /// Outer-terminal protocol for terminal delivery. Auto detects the attaching terminal.
    pub terminal_backend: TerminalNotificationBackend,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct StatusBarConfig {
    /// Show the full-width top status row on desktop layouts. Default: true.
    pub enabled: bool,
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
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
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Resume supported AI-agent panes into their native conversation sessions
    /// when restoring a Herdr session. Default: true.
    pub resume_agents_on_restore: bool,
    /// Move Done panes into the collapsed Settled sidebar section after
    /// this many minutes. Default: 30.
    pub hide_done_after_minutes: u64,
    /// Close eligible Done panes after this many minutes. Default: 240 (4 hours).
    pub reap_done_after_minutes: u64,
    /// Automatically close eligible Done panes. Set false to disable reaping.
    /// Default: true.
    pub reap_done_panes: bool,
    /// Settle panes with no activity after this many days. Default: 3.
    pub settle_after_days: u64,
    /// Settle a pane when the work it is linked to finishes (its pull request
    /// merged or closed, its ticket done). Default: true.
    pub auto_settle_finished: bool,
    /// Wait this many minutes after the linked work first reads as finished
    /// before settling the pane, and settle only if it still reads finished
    /// and the pane stayed quiet. Default: 10.
    pub settle_finished_after_minutes: u64,
    /// Settle a pane that has been inactive for `settle_after_days`.
    /// Default: true.
    pub auto_settle_inactive: bool,
    /// Settle an inactive agent pane after it stays idle with no closing gates,
    /// usage limit, active sub-agents, or held shell for
    /// `settle_done_after_minutes`. Applies whether the pane was seen and when
    /// stale supervision resolves to an idle screen. Default: true.
    pub auto_settle_done: bool,
    /// How long an eligible agent pane has to stay quiet before
    /// `auto_settle_done` settles it. Default: 30.
    pub settle_done_after_minutes: u64,
    /// Stop resumable agent processes when their pane settles.
    /// Default: true.
    pub settle_stops_agent: bool,
    /// After a pane is resumed into its native agent session, submit one
    /// `resume_nudge_message` prompt so the agent picks the work back up
    /// instead of waiting at a restored, idle prompt. The nudge is skipped
    /// when the agent comes back blocked, when it resumes straight into work,
    /// and when the pane already holds a draft the human typed. Default: true.
    pub nudge_resumed_agents: bool,
    /// Prompt submitted by `nudge_resumed_agents`. Default: "continue".
    pub resume_nudge_message: String,
    /// Nudge stalled agent panes after both their status declaration and pane
    /// activity have gone quiet. Default: false.
    pub auto_nudge_stalled_agents: bool,
    /// Mark an unattended agent status report stale after this many quiet
    /// minutes when a finished report still has unreported child work. A
    /// working report keeps the 20-minute busy budget. Default: 5.
    pub agent_stale_after_minutes: u64,
    /// Quiet period before a parent waiting on declared subagents becomes
    /// stale. Default: 30.
    pub agent_subagent_stale_after_minutes: u64,
    /// Initial quiet period before a stalled pane is nudged. Default: 5.
    pub nudge_after_minutes: u64,
    /// Maximum nudges sent during one stale-status episode. Default: 3.
    pub max_nudges: u32,
    /// Maximum stalled-agent nudges sent to one pane without human input.
    /// Zero disables this cap. Default: 5.
    pub max_nudges_without_human: u32,
    /// Prompt submitted to a stalled pane. Default: "Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue."
    pub stall_nudge_message: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            resume_agents_on_restore: true,
            hide_done_after_minutes: 30,
            reap_done_after_minutes: 4 * 60,
            reap_done_panes: true,
            settle_after_days: 3,
            auto_settle_finished: true,
            settle_finished_after_minutes: 10,
            auto_settle_inactive: true,
            auto_settle_done: true,
            settle_done_after_minutes: 30,
            settle_stops_agent: true,
            nudge_resumed_agents: true,
            resume_nudge_message: "continue".to_string(),
            auto_nudge_stalled_agents: false,
            agent_stale_after_minutes: 5,
            agent_subagent_stale_after_minutes: 30,
            nudge_after_minutes: 5,
            max_nudges: 3,
            max_nudges_without_human: 5,
            stall_nudge_message:
                "Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue.".to_string(),
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
    pub agent_detection: AgentDetectionConfig,
    pub work_index: WorkIndexConfig,
    pub day_board: DayBoardConfig,
    pub missive: MissiveConfig,
    pub usage: UsageConfig,
    pub source_control: SourceControlConfig,
    pub files: FilesConfig,
    pub panel: PanelConfig,
    pub linear: LinearConfig,
    pub notepad: NotepadConfig,
    pub goals_panel: GoalsPanelConfig,
    pub pomodoro: PomodoroConfig,
    pub actions: Vec<ActionConfig>,
    pub launch_profiles: Vec<LaunchProfileConfig>,
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DayBoardConfig {
    /// Quiet seconds after which a working bound pane is stale. Default: 600.
    pub stale_after: u64,
}

impl Default for DayBoardConfig {
    fn default() -> Self {
        Self { stale_after: 600 }
    }
}

impl Config {
    pub(crate) fn clamp_safety_bounds(&mut self) {
        self.session.agent_stale_after_minutes = self
            .session
            .agent_stale_after_minutes
            .min(MAX_NUDGE_AFTER_MINUTES);
        self.session.agent_subagent_stale_after_minutes = self
            .session
            .agent_subagent_stale_after_minutes
            .min(MAX_NUDGE_AFTER_MINUTES);
        self.session.nudge_after_minutes = self
            .session
            .nudge_after_minutes
            .min(MAX_NUDGE_AFTER_MINUTES);
    }
}

/// One named group of checkouts the home composer can dispatch into.
///
/// Which repositories belong together is a fact about how someone works, not
/// about the filesystem, so it cannot be inferred from a path. `roots` names
/// directories whose immediate children are checkouts; `repos` names single
/// checkouts, absolute or relative to `~/Repos`. With no project configured
/// the composer groups everything under `~/Repos`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub id: String,
    pub label: String,
    pub roots: Vec<String>,
    pub repos: Vec<String>,
}

/// One named way to start one agent, offered by the home composer.
///
/// A provider is not always a binary. A Kimi session is Claude Code pointed at
/// a different base URL through a dozen environment variables, and a Codex
/// account is a `CODEX_HOME` directory. Naming the launcher the operator
/// already has beats restating its environment here, where it would drift.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct LaunchProfileConfig {
    /// Stable identifier, used to remember the selection across openings.
    pub id: String,
    /// What the picker shows. Falls back to `id` when empty.
    pub label: String,
    /// Which agent this lane runs, so detection, icons and flag dialect still
    /// resolve. Parsed with the same labels agent detection uses.
    pub agent: String,
    /// The exact argv to run, prompt appended. When set, this lane owns its own
    /// model, effort and permission flags and the composer hides those pickers
    /// rather than passing flags the lane would reject. Empty means Herdr
    /// builds the argv from the probed catalog, as it does without a profile.
    pub command: Vec<String>,
    /// Environment applied to the spawned pane. Values starting `~/` expand
    /// against the home directory.
    pub env: BTreeMap<String, String>,
    /// Which quota window the picker shows for this lane: `claude`, `codex` or
    /// `kimi`. Absent means the lane reports no usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
}

/// `[notepad]` — the sidebar note folder.
///
/// Sync is deliberately somebody else's job: point `dir` at a folder a sync
/// daemon already carries, or set `git_sync` and let Herdr pull and push the
/// checkout itself.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NotepadConfig {
    /// Show the notepad panel at the bottom of the sidebar.
    pub enabled: bool,
    /// Stable tab keys to show in the sidebar. Note files use `note:<stem>`;
    /// the built-in tabs use `context`, `agent`, and `usage`. Turning on a note
    /// in Sidebar panels also enables the notepad backend and creates a missing
    /// note file.
    pub visible_tabs: Vec<String>,
    /// Notes directory. Empty uses `<config dir>/notes`. `~` is expanded.
    pub dir: String,
    /// Note names to offer first, in this order. Others follow alphabetically.
    pub files: Vec<String>,
    /// Sidebar rows the panel occupies, header included. Clamped to 3..=200,
    /// then capped at render time by the rows the sidebar can spare.
    pub height: u16,
    /// Pull and push the notes directory as a git checkout.
    pub git_sync: bool,
    /// Seconds between background `git pull` runs when `git_sync` is set.
    pub git_sync_interval_seconds: u64,
}

impl Default for NotepadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            visible_tabs: vec!["usage".to_string()],
            dir: String::new(),
            files: Vec::new(),
            height: 8,
            git_sync: false,
            git_sync_interval_seconds: 120,
        }
    }
}

/// `[goals_panel]` - the focused session's read-only goals panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct GoalsPanelConfig {
    /// Show goals and work streams from `.streams.json` in the sidebar.
    pub enabled: bool,
}

/// `[pomodoro]` — the break reminder.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PomodoroConfig {
    pub enabled: bool,
    /// Show the countdown widget in the sidebar footer. Turning it on in
    /// Sidebar panels also enables the timer; turning it off leaves the timer
    /// setting alone.
    pub sidebar_visible: bool,
    /// Minutes of focus before a break is due.
    pub work_minutes: u64,
    pub short_break_minutes: u64,
    pub long_break_minutes: u64,
    /// Work intervals between long breaks.
    pub long_break_every: u32,
    /// Characters the operator has to type to dismiss a due reminder.
    pub min_confirm_chars: usize,
    /// Note in the notepad directory the confirmations are appended to. Empty
    /// disables the log.
    pub log_file: String,
}

impl Default for PomodoroConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sidebar_visible: false,
            work_minutes: 25,
            short_break_minutes: 5,
            long_break_minutes: 20,
            long_break_every: 4,
            min_confirm_chars: crate::pomodoro::DEFAULT_MIN_CONFIRM_CHARS,
            log_file: "pomodoro-log.md".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LinearLayoutConfig {
    #[default]
    List,
    Board,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct LinearConfig {
    /// Initial layout for the full-screen Linear view.
    pub default_layout: LinearLayoutConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct PanelConfig {
    /// Surfaces opened on a fresh right panel. Empty starts on the chooser.
    pub default_surfaces: Vec<PanelSurfaceConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSurfaceConfig {
    Home,
    Terminal,
    Files,
    Diff,
    #[serde(alias = "pr")]
    PullRequest,
    Linear,
    Missive,
    Agents,
    Hosts,
    #[serde(alias = "edit")]
    Editor,
    #[serde(alias = "keys")]
    Shortcuts,
    #[serde(alias = "ctx")]
    Context,
    #[serde(alias = "note")]
    Scratchpad,
}

/// Read-only Missive API settings.
///
/// `team` and `organization` are UUIDs copied from Missive's Settings > API >
/// Resource IDs page. `token_env` names an environment variable; the token
/// itself is deliberately absent from this serializable configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct MissiveConfig {
    pub token_env: String,
    pub team: Option<String>,
    pub organization: Option<String>,
}

impl Default for MissiveConfig {
    fn default() -> Self {
        Self {
            token_env: "MISSIVE_API_TOKEN".into(),
            team: None,
            organization: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct ActionConfig {
    pub name: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub run_on_worktree_create: bool,
    pub open_in_bottom_pane: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct UsageModelPricing {
    /// USD per one million uncached input tokens.
    pub input: f64,
    /// USD per one million output tokens.
    pub output: f64,
    /// USD per one million cache-write input tokens.
    pub cache_write: f64,
    /// USD per one million cache-read input tokens.
    pub cache_read: f64,
}

impl Default for UsageModelPricing {
    fn default() -> Self {
        Self {
            input: 0.0,
            output: 0.0,
            cache_write: 0.0,
            cache_read: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageConfig {
    /// API list-price estimates keyed by the model id written to provider logs.
    pub pricing: std::collections::BTreeMap<String, UsageModelPricing>,
}

impl<'de> serde::Deserialize<'de> for UsageConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct RawUsageConfig {
            pricing: std::collections::BTreeMap<String, UsageModelPricing>,
        }

        let raw = RawUsageConfig::deserialize(deserializer)?;
        let mut config = UsageConfig::default();
        config.pricing.extend(raw.pricing);
        Ok(config)
    }
}

impl Default for UsageConfig {
    fn default() -> Self {
        let entries = [
            ("claude-fable-5-1", 1.0, 5.0, 1.25, 0.10),
            ("claude-opus-5", 15.0, 75.0, 18.75, 1.50),
            ("claude-sonnet-5", 3.0, 15.0, 3.75, 0.30),
            ("claude-haiku-4-5", 1.0, 5.0, 1.25, 0.10),
            ("gpt-5.6-sol", 1.25, 10.0, 1.25, 0.125),
            ("gpt-5.6-luna", 0.25, 2.0, 0.25, 0.025),
            ("gpt-5.3-codex-spark", 0.25, 2.0, 0.25, 0.025),
        ];
        Self {
            pricing: entries
                .into_iter()
                .map(|(model, input, output, cache_write, cache_read)| {
                    (
                        model.to_string(),
                        UsageModelPricing {
                            input,
                            output,
                            cache_write,
                            cache_read,
                        },
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesIconConfig {
    Badges,
    #[default]
    Nerd,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    pub icons: FilesIconConfig,
}

/// Prefix Herdr puts in front of a derived worktree branch name.
pub const DEFAULT_BRANCH_PREFIX: &str = "issue/";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethodConfig {
    #[default]
    Merge,
    Squash,
    Rebase,
}

impl MergeMethodConfig {
    pub(crate) const ALL: [Self; 3] = [Self::Merge, Self::Squash, Self::Rebase];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }

    pub(crate) const fn flag(self) -> &'static str {
        match self {
            Self::Merge => "--merge",
            Self::Squash => "--squash",
            Self::Rebase => "--rebase",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SourceControlConfig {
    /// Merge strategy used by the primary pull-request action and auto-merge.
    pub merge_method: MergeMethodConfig,
    /// Model name exported to the Commit action as
    /// `HERDR_COMMIT_MESSAGE_MODEL`, so a `prepare-commit-msg` hook can draft
    /// the message with it. Empty leaves `git commit` exactly as it was.
    pub commit_message_model: String,
    /// Stage every working-tree change before generating and committing.
    pub commit_stage_all: bool,
    /// Prefix for branch names Herdr derives from a ticket.
    pub branch_prefix: String,
}

impl Default for SourceControlConfig {
    fn default() -> Self {
        Self {
            merge_method: MergeMethodConfig::Merge,
            commit_message_model: String::new(),
            commit_stage_all: false,
            branch_prefix: DEFAULT_BRANCH_PREFIX.into(),
        }
    }
}

pub const DEFAULT_FULL_LIFECYCLE_HOOK_AUTHORITY_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AgentDetectionConfig {
    pub full_lifecycle_hook_authority_timeout_seconds: u64,
}

impl Default for AgentDetectionConfig {
    fn default() -> Self {
        Self {
            full_lifecycle_hook_authority_timeout_seconds:
                DEFAULT_FULL_LIFECYCLE_HOOK_AUTHORITY_TIMEOUT_SECONDS,
        }
    }
}

impl<'de> Deserialize<'de> for AgentDetectionConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default)]
        struct Raw {
            full_lifecycle_hook_authority_timeout_seconds: u64,
        }
        impl Default for Raw {
            fn default() -> Self {
                Self {
                    full_lifecycle_hook_authority_timeout_seconds:
                        DEFAULT_FULL_LIFECYCLE_HOOK_AUTHORITY_TIMEOUT_SECONDS,
                }
            }
        }
        let raw = Raw::deserialize(deserializer)?;
        if !(30..=3600).contains(&raw.full_lifecycle_hook_authority_timeout_seconds) {
            return Err(de::Error::custom(
                "full_lifecycle_hook_authority_timeout_seconds must be between 30 and 3600",
            ));
        }
        Ok(Self {
            full_lifecycle_hook_authority_timeout_seconds: raw
                .full_lifecycle_hook_authority_timeout_seconds,
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
    /// Prefix key to enter prefix mode (e.g. "ctrl+b", "f12", "esc").
    pub prefix: String,
    /// Open keybinding help. Default: "prefix+?"
    pub help: BindingConfig,
    /// Open settings. Default: "prefix+s"
    pub settings: BindingConfig,
    /// Create a new workspace. Default: "prefix+shift+n"
    pub new_workspace: BindingConfig,
    /// Open the project picker and start a new Home thread in the project it
    /// selects. Default: "prefix+c"
    pub new_thread: BindingConfig,
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
    /// Open the fuzzy command palette. Default: ["ctrl+alt+p", "super+p"]
    pub command_palette: BindingConfig,
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
    /// Detach from server/client mode, or exit --no-session mode. Default: "prefix+q".
    pub detach: BindingConfig,
    /// Reload config.toml in the running app/server. Default: "prefix+shift+r".
    pub reload_config: BindingConfig,
    /// Focus the currently visible notification target. Default: "prefix+o".
    pub open_notification_target: BindingConfig,
    /// Open a numbered link from the focused pane's work context. Default: "prefix+u".
    pub open_work_link: BindingConfig,
    /// Copy a numbered link from the focused pane's work context. Default: "prefix+shift+u".
    pub copy_work_link: BindingConfig,
    /// Legacy alias for `open_work_link`; unset by default.
    pub open_work_url: BindingConfig,
    /// Legacy alias for `copy_work_link`; unset by default.
    pub copy_work_url: BindingConfig,
    /// Copy the focused pane's bare ticket ID. Default: "prefix+ctrl+u".
    pub copy_work_ticket: BindingConfig,
    /// Copy the focused pane's pull request URL. Default: "prefix+alt+u".
    pub copy_work_pr: BindingConfig,
    /// Copy the focused pane's first preview URL. Default: "prefix+ctrl+shift+u".
    pub copy_work_preview: BindingConfig,
    /// Toggle between the current theme and its dark/light sibling. Unset by default.
    pub toggle_theme: BindingConfig,
    /// Select the previous workspace. Unset by default.
    pub previous_workspace: BindingConfig,
    /// Select the next workspace. Unset by default.
    pub next_workspace: BindingConfig,
    /// Focus the previous agent shown in the agent panel. Unset by default.
    pub previous_agent: BindingConfig,
    /// Focus the next agent shown in the agent panel. Unset by default.
    pub next_agent: BindingConfig,
    /// Focus the next pane registered for review work. Default: "prefix+ctrl+r".
    pub next_review_agent: BindingConfig,
    /// Focus an agent by index 1-9. Unset by default.
    pub focus_agent: BindingConfig,
    /// Local-client shortcut that sends a clipboard image to a remote Herdr session. Default: "ctrl+v".
    pub remote_image_paste: String,
    /// Create a new tab in the active workspace. Default: "prefix+alt+c"
    pub new_tab: BindingConfig,
    /// Rename the active tab. Default: "prefix+shift+t".
    pub rename_tab: BindingConfig,
    /// Toggle the active tab's PRIO flag. Default: "prefix+shift+f".
    pub toggle_tab_prio: BindingConfig,
    /// Toggle the expanded sidebar PRIO panel. Unset by default.
    pub toggle_prio_panel: BindingConfig,
    /// Select the previous tab in the active workspace. Default: "prefix+ctrl+p".
    pub previous_tab: BindingConfig,
    /// Select the next tab in the active workspace. Default: "prefix+ctrl+n".
    pub next_tab: BindingConfig,
    /// Move the active tab one position toward the front. Unset by default.
    pub move_tab_previous: BindingConfig,
    /// Move the active tab one position toward the back. Unset by default.
    pub move_tab_next: BindingConfig,
    /// Focus the previous tab across all workspaces. Default: "prefix+p".
    pub previous_window: BindingConfig,
    /// Focus the next tab across all workspaces. Default: "prefix+n".
    pub next_window: BindingConfig,
    /// Focus the next blocked tab across all workspaces. Default: "prefix+b".
    pub next_blocked_window: BindingConfig,
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
    /// Split pane to the left. Default: "prefix+shift+v".
    pub split_left: BindingConfig,
    /// Split pane upward. Default: "prefix+shift+minus".
    pub split_up: BindingConfig,
    /// Close the focused pane. Default: "prefix+x"
    pub close_pane: BindingConfig,
    /// Toggle zoom for the focused pane. Default: "prefix+z"
    #[serde(alias = "fullscreen")]
    pub zoom: BindingConfig,
    /// Pin or unpin the active tab into the sidebar's Pinned group.
    /// Unset by default; pinning is available from the sidebar and API.
    pub toggle_pin_tab: BindingConfig,
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
    /// Toggle sidebar collapse. Default: "prefix+shift+b"
    pub toggle_sidebar: BindingConfig,
    /// Focus the sidebar and expand it if collapsed. Unset by default.
    pub focus_sidebar: BindingConfig,
    /// Open the pod picker for the focused pane. Unset by default.
    pub assign_pane_to_pod: BindingConfig,
    /// Collapse every repo group except the one owning the focused pane.
    pub focus_owning_repo_group: BindingConfig,
    /// Cycle the sidebar grouping mode. Unset by default.
    pub sidebar_cycle_group_mode: BindingConfig,
    /// Refresh sidebar work and Git metadata. Unset by default.
    pub sidebar_refresh: BindingConfig,
    /// Toggle the sidebar blocked filter. Default: "prefix+f".
    pub toggle_blocked_filter: BindingConfig,
    /// Toggle dock collapse. Default: ["prefix+shift+e", "ctrl+alt+d"]
    pub toggle_dock: BindingConfig,
    /// Select the previous dock tab. Default: "prefix+shift+["
    pub previous_dock_tab: BindingConfig,
    /// Select the next dock tab. Default: "prefix+shift+]"
    pub next_dock_tab: BindingConfig,
    /// Open the focused repository in a right-side Vim-family editor. Unset by default.
    pub editor_open_repo: BindingConfig,
    /// Open the focused repository's scratchpad in a terminal pane. Default: "ctrl+alt+e"
    pub edit_scratchpad: BindingConfig,
    /// Show the scratchpad in the dock without opening an editor. Default: "ctrl+alt+n"
    pub show_scratchpad: BindingConfig,
    /// Focus the sidebar notepad for typing. Default: "ctrl+alt+m"
    pub toggle_notepad: BindingConfig,
    /// Pause or resume the break timer, or dismiss a due reminder. Default: "ctrl+alt+b"
    pub toggle_pomodoro: BindingConfig,
    /// Open the read-only Symphony workflow dashboard. Default: "prefix+shift+s"
    pub symphony: BindingConfig,
    /// Expand and focus the read-only fleet Runs section. Default: "prefix+alt+r"
    pub runs: BindingConfig,
    /// Open the work projection view. Default: "prefix+ctrl+w"
    pub work: BindingConfig,
    /// Open the historical provider usage view. Default: "prefix+ctrl+y"
    pub usage: BindingConfig,
    /// Open the Linear Tickets view. Default: "prefix+ctrl+t"
    pub tickets: BindingConfig,
    /// Open the read-only Missive conversation view. Unset by default.
    pub missive: BindingConfig,
    /// Open the blocked-agent inbox. Default: ["prefix+shift+i", "ctrl+alt+i"]
    pub inbox: BindingConfig,
    /// Open the home view. Default: ["prefix+shift+o", "ctrl+alt+o"]
    pub home: BindingConfig,
    /// Run `git pull --rebase` for the focused repository. Unset by default.
    pub git_pull: BindingConfig,
    /// Run the configured Git commit flow for the focused repository. Unset by default.
    pub git_commit: BindingConfig,
    /// Run `git push` for the focused repository. Unset by default.
    pub git_push: BindingConfig,
    /// Create a pull request for the focused repository. Unset by default.
    pub git_create_pr: BindingConfig,
    /// Open the Home dock surface. Unset by default.
    pub dock_home: BindingConfig,
    /// Open the Terminal dock surface. Unset by default.
    pub dock_terminal: BindingConfig,
    /// Open the Files dock surface. Unset by default.
    pub dock_files: BindingConfig,
    /// Open the Diff dock surface. Unset by default.
    pub dock_diff: BindingConfig,
    /// Open the pull request dock surface. Unset by default.
    pub dock_pr: BindingConfig,
    /// Open the Linear dock surface. Unset by default.
    pub dock_linear: BindingConfig,
    /// Open the Missive dock surface. Unset by default.
    pub dock_missive: BindingConfig,
    /// Open the Agents dock surface. Unset by default.
    pub dock_agents: BindingConfig,
    /// Open the Shortcuts dock surface. Unset by default.
    pub dock_shortcuts: BindingConfig,
    /// Open the Context dock surface. Unset by default.
    pub dock_context: BindingConfig,
    /// Open the Symphony dock surface. Unset by default.
    pub dock_symphony: BindingConfig,
    pub toggle_status_detail: BindingConfig,
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
    prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_thread: Option<BindingConfig>,
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
    command_palette: Option<BindingConfig>,
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
    open_work_link: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_work_link: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_work_url: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_work_url: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_work_ticket: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_work_pr: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_work_preview: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_theme: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_workspace: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_agent: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_agent: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_review_agent: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus_agent: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_image_paste: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rename_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_tab_prio: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_prio_panel: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    move_tab_previous: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    move_tab_next: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_window: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_window: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_blocked_window: Option<BindingConfig>,
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
    split_left: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    split_up: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close_pane: Option<BindingConfig>,
    #[serde(alias = "fullscreen", skip_serializing_if = "Option::is_none")]
    zoom: Option<BindingConfig>,
    toggle_pin_tab: Option<BindingConfig>,
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
    focus_sidebar: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assign_pane_to_pod: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus_owning_repo_group: Option<BindingConfig>,
    sidebar_cycle_group_mode: Option<BindingConfig>,
    sidebar_refresh: Option<BindingConfig>,
    toggle_status_detail: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_blocked_filter: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_dock: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_dock_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_dock_tab: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    editor_open_repo: Option<BindingConfig>,
    edit_scratchpad: Option<BindingConfig>,
    show_scratchpad: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_notepad: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toggle_pomodoro: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    symphony: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runs: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<BindingConfig>,
    tickets: Option<BindingConfig>,
    missive: Option<BindingConfig>,
    inbox: Option<BindingConfig>,
    home: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_pull: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_commit: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_push: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_create_pr: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_home: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_terminal: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_files: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_diff: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_pr: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_linear: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_missive: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_agents: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_shortcuts: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_context: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dock_symphony: Option<BindingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    indexed: Option<IndexedKeysConfig>,
    #[serde(skip_serializing)]
    command: Option<Vec<CommandKeybindConfig>>,
}

impl<'de> Deserialize<'de> for KeysConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = KeysConfigOverlay::deserialize(deserializer)?;
        let mut keys = KeysConfig::default();

        macro_rules! apply_field {
            ($field:ident) => {
                if let Some(value) = input.$field {
                    keys.$field = value;
                    keys.user_fields.insert(stringify!($field));
                }
            };
        }

        apply_field!(prefix);
        apply_field!(help);
        apply_field!(settings);
        apply_field!(new_workspace);
        apply_field!(new_thread);
        apply_field!(new_worktree);
        apply_field!(open_worktree);
        apply_field!(remove_worktree);
        apply_field!(rename_workspace);
        apply_field!(close_workspace);
        apply_field!(workspace_picker);
        apply_field!(goto);
        apply_field!(command_palette);
        apply_field!(navigate_workspace_up);
        apply_field!(navigate_workspace_down);
        apply_field!(navigate_pane_left);
        apply_field!(navigate_pane_down);
        apply_field!(navigate_pane_up);
        apply_field!(navigate_pane_right);
        apply_field!(detach);
        apply_field!(reload_config);
        apply_field!(open_notification_target);
        apply_field!(open_work_link);
        apply_field!(copy_work_link);
        apply_field!(open_work_url);
        apply_field!(copy_work_url);
        apply_field!(copy_work_ticket);
        apply_field!(copy_work_pr);
        apply_field!(copy_work_preview);
        apply_field!(toggle_theme);
        apply_field!(previous_workspace);
        apply_field!(next_workspace);
        apply_field!(previous_agent);
        apply_field!(next_agent);
        apply_field!(next_review_agent);
        apply_field!(focus_agent);
        apply_field!(remote_image_paste);
        apply_field!(new_tab);
        apply_field!(rename_tab);
        apply_field!(toggle_tab_prio);
        apply_field!(toggle_prio_panel);
        apply_field!(previous_tab);
        apply_field!(next_tab);
        apply_field!(move_tab_previous);
        apply_field!(move_tab_next);
        apply_field!(previous_window);
        apply_field!(next_window);
        apply_field!(next_blocked_window);
        apply_field!(switch_tab);
        apply_field!(switch_workspace);
        apply_field!(close_tab);
        apply_field!(rename_pane);
        apply_field!(edit_scrollback);
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
        apply_field!(split_left);
        apply_field!(split_up);
        apply_field!(close_pane);
        apply_field!(zoom);
        apply_field!(toggle_pin_tab);
        apply_field!(resize_mode);
        apply_field!(resize_pane_left);
        apply_field!(resize_pane_down);
        apply_field!(resize_pane_up);
        apply_field!(resize_pane_right);
        apply_field!(toggle_sidebar);
        apply_field!(focus_sidebar);
        apply_field!(assign_pane_to_pod);
        apply_field!(focus_owning_repo_group);
        apply_field!(sidebar_cycle_group_mode);
        apply_field!(sidebar_refresh);
        apply_field!(toggle_blocked_filter);
        apply_field!(toggle_dock);
        apply_field!(previous_dock_tab);
        apply_field!(next_dock_tab);
        apply_field!(editor_open_repo);
        apply_field!(edit_scratchpad);
        apply_field!(show_scratchpad);
        apply_field!(toggle_notepad);
        apply_field!(toggle_pomodoro);
        apply_field!(symphony);
        apply_field!(runs);
        apply_field!(work);
        apply_field!(usage);
        apply_field!(tickets);
        apply_field!(missive);
        apply_field!(inbox);
        apply_field!(home);
        apply_field!(git_pull);
        apply_field!(git_commit);
        apply_field!(git_push);
        apply_field!(git_create_pr);
        apply_field!(dock_home);
        apply_field!(dock_terminal);
        apply_field!(dock_files);
        apply_field!(dock_diff);
        apply_field!(dock_pr);
        apply_field!(dock_linear);
        apply_field!(dock_missive);
        apply_field!(dock_agents);
        apply_field!(dock_shortcuts);
        apply_field!(dock_context);
        apply_field!(dock_symphony);
        apply_field!(toggle_status_detail);
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
        copy_effective_action_field!(new_thread, keybinds.new_thread);
        copy_effective_action_field!(new_worktree, keybinds.new_worktree);
        copy_effective_action_field!(open_worktree, keybinds.open_worktree);
        copy_effective_action_field!(remove_worktree, keybinds.remove_worktree);
        copy_effective_action_field!(rename_workspace, keybinds.rename_workspace);
        copy_effective_action_field!(close_workspace, keybinds.close_workspace);
        copy_effective_action_field!(workspace_picker, keybinds.workspace_picker);
        copy_effective_action_field!(goto, keybinds.goto);
        copy_effective_action_field!(command_palette, keybinds.command_palette);
        copy_effective_action_field!(navigate_workspace_up, keybinds.navigate.workspace_up);
        copy_effective_action_field!(navigate_workspace_down, keybinds.navigate.workspace_down);
        copy_effective_action_field!(navigate_pane_left, keybinds.navigate.pane_left);
        copy_effective_action_field!(navigate_pane_down, keybinds.navigate.pane_down);
        copy_effective_action_field!(navigate_pane_up, keybinds.navigate.pane_up);
        copy_effective_action_field!(navigate_pane_right, keybinds.navigate.pane_right);
        copy_effective_action_field!(detach, keybinds.detach);
        copy_effective_action_field!(reload_config, keybinds.reload_config);
        copy_effective_action_field!(open_notification_target, keybinds.open_notification_target);
        copy_effective_action_field!(open_work_link, keybinds.open_work_link);
        copy_effective_action_field!(copy_work_link, keybinds.copy_work_link);
        copy_effective_action_field!(open_work_url, keybinds.open_work_url);
        copy_effective_action_field!(copy_work_url, keybinds.copy_work_url);
        copy_effective_action_field!(copy_work_ticket, keybinds.copy_work_ticket);
        copy_effective_action_field!(copy_work_pr, keybinds.copy_work_pr);
        copy_effective_action_field!(copy_work_preview, keybinds.copy_work_preview);
        copy_effective_action_field!(toggle_theme, keybinds.toggle_theme);
        copy_effective_action_field!(previous_workspace, keybinds.previous_workspace);
        copy_effective_action_field!(next_workspace, keybinds.next_workspace);
        copy_effective_action_field!(previous_agent, keybinds.previous_agent);
        copy_effective_action_field!(next_agent, keybinds.next_agent);
        copy_effective_action_field!(next_review_agent, keybinds.next_review_agent);
        copy_effective_indexed_field!(focus_agent, keybinds.focus_agent);
        copy_user_field!(remote_image_paste);
        copy_effective_action_field!(new_tab, keybinds.new_tab);
        copy_effective_action_field!(rename_tab, keybinds.rename_tab);
        copy_effective_action_field!(toggle_tab_prio, keybinds.toggle_tab_prio);
        copy_effective_action_field!(toggle_prio_panel, keybinds.toggle_prio_panel);
        copy_effective_action_field!(previous_tab, keybinds.previous_tab);
        copy_effective_action_field!(next_tab, keybinds.next_tab);
        copy_effective_action_field!(move_tab_previous, keybinds.move_tab_previous);
        copy_effective_action_field!(move_tab_next, keybinds.move_tab_next);
        copy_effective_action_field!(previous_window, keybinds.previous_window);
        copy_effective_action_field!(next_window, keybinds.next_window);
        copy_effective_action_field!(next_blocked_window, keybinds.next_blocked_window);
        copy_effective_indexed_field!(switch_tab, keybinds.switch_tab);
        copy_effective_indexed_field!(switch_workspace, keybinds.switch_workspace);
        copy_effective_action_field!(close_tab, keybinds.close_tab);
        copy_effective_action_field!(rename_pane, keybinds.rename_pane);
        copy_effective_action_field!(edit_scrollback, keybinds.edit_scrollback);
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
        copy_effective_action_field!(split_left, keybinds.split_left);
        copy_effective_action_field!(split_up, keybinds.split_up);
        copy_effective_action_field!(close_pane, keybinds.close_pane);
        copy_effective_action_field!(zoom, keybinds.zoom);
        copy_effective_action_field!(toggle_pin_tab, keybinds.toggle_pin_tab);
        copy_effective_action_field!(resize_mode, keybinds.resize_mode);
        copy_effective_action_field!(resize_pane_left, keybinds.resize_pane_left);
        copy_effective_action_field!(resize_pane_down, keybinds.resize_pane_down);
        copy_effective_action_field!(resize_pane_up, keybinds.resize_pane_up);
        copy_effective_action_field!(resize_pane_right, keybinds.resize_pane_right);
        copy_effective_action_field!(toggle_sidebar, keybinds.toggle_sidebar);
        copy_effective_action_field!(focus_sidebar, keybinds.focus_sidebar);
        copy_effective_action_field!(assign_pane_to_pod, keybinds.assign_pane_to_pod);
        copy_effective_action_field!(focus_owning_repo_group, keybinds.focus_owning_repo_group);
        copy_effective_action_field!(sidebar_cycle_group_mode, keybinds.sidebar_cycle_group_mode);
        copy_effective_action_field!(sidebar_refresh, keybinds.sidebar_refresh);
        copy_effective_action_field!(toggle_blocked_filter, keybinds.toggle_blocked_filter);
        copy_effective_action_field!(toggle_dock, keybinds.toggle_dock);
        copy_effective_action_field!(previous_dock_tab, keybinds.previous_dock_tab);
        copy_effective_action_field!(next_dock_tab, keybinds.next_dock_tab);
        copy_effective_action_field!(editor_open_repo, keybinds.editor_open_repo);
        copy_effective_action_field!(edit_scratchpad, keybinds.edit_scratchpad);
        copy_effective_action_field!(show_scratchpad, keybinds.show_scratchpad);
        copy_effective_action_field!(toggle_notepad, keybinds.toggle_notepad);
        copy_effective_action_field!(toggle_pomodoro, keybinds.toggle_pomodoro);
        copy_effective_action_field!(symphony, keybinds.symphony);
        copy_effective_action_field!(runs, keybinds.runs);
        copy_effective_action_field!(work, keybinds.work);
        copy_effective_action_field!(usage, keybinds.usage);
        copy_effective_action_field!(tickets, keybinds.tickets);
        copy_effective_action_field!(missive, keybinds.missive);
        copy_effective_action_field!(inbox, keybinds.inbox);
        copy_effective_action_field!(home, keybinds.home);
        copy_effective_action_field!(git_pull, keybinds.git_pull);
        copy_effective_action_field!(git_commit, keybinds.git_commit);
        copy_effective_action_field!(git_push, keybinds.git_push);
        copy_effective_action_field!(git_create_pr, keybinds.git_create_pr);
        copy_effective_action_field!(dock_home, keybinds.dock_home);
        copy_effective_action_field!(dock_terminal, keybinds.dock_terminal);
        copy_effective_action_field!(dock_files, keybinds.dock_files);
        copy_effective_action_field!(dock_diff, keybinds.dock_diff);
        copy_effective_action_field!(dock_pr, keybinds.dock_pr);
        copy_effective_action_field!(dock_linear, keybinds.dock_linear);
        copy_effective_action_field!(dock_missive, keybinds.dock_missive);
        copy_effective_action_field!(dock_agents, keybinds.dock_agents);
        copy_effective_action_field!(dock_shortcuts, keybinds.dock_shortcuts);
        copy_effective_action_field!(dock_context, keybinds.dock_context);
        copy_effective_action_field!(dock_symphony, keybinds.dock_symphony);
        copy_effective_action_field!(toggle_status_detail, keybinds.toggle_status_detail);
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

/// Which workspace the Home composer preselects for a new thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NewThreadWorkspaceConfig {
    /// Launch in the selected directory itself.
    #[default]
    CurrentCheckout,
    /// Create a linked worktree first and launch there.
    NewWorktree,
}

impl NewThreadWorkspaceConfig {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CurrentCheckout => "current_checkout",
            Self::NewWorktree => "new_worktree",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CurrentCheckout => "current checkout",
            Self::NewWorktree => "new worktree",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TabBarPositionConfig {
    Top,
    Bottom,
    /// No tab row at all. The sidebar already lists every tab in every space, so
    /// the row repeats it while costing a terminal line on every workspace.
    #[default]
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WindowCycleModeConfig {
    #[default]
    ThisMachine,
    ThisMachineAndFleet,
}

fn deserialize_working_row_opacity<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if (25..=100).contains(&value) {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "ui.working_row_opacity_percent must be between 25 and 100",
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub sidebar_width: u16,
    /// Foreground opacity of blue-dot Working session rows. Default: 100.
    #[serde(deserialize_with = "deserialize_working_row_opacity")]
    pub working_row_opacity_percent: u8,
    /// Whether the sidebar draws its idle animation. The panel also has a pause
    /// button; this is the switch that removes it entirely.
    pub sidebar_animation: bool,
    /// Whether next/previous and next-blocked include fleet agents.
    pub window_cycle_mode: WindowCycleModeConfig,
    /// Exclude collapsed spaces and sections from next/previous and next-blocked.
    pub skip_collapsed_cycle: bool,
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
    /// Ask for confirmation before closing a workspace, and before deleting a
    /// settled thread from the sidebar menu. Default: true.
    pub confirm_close: bool,
    /// Group workspaces that check out the same repository under one project
    /// header even when their checkouts live on different hosts or roots.
    /// Default: false, which keeps one header per checkout root.
    pub combine_repos_across_hosts: bool,
    /// Hide whitespace-only changes in the diff surface. Default: false.
    pub hide_whitespace_in_diff: bool,
    /// Render Nerd Font glyphs in the UI and file lists. Default: true.
    pub nerd_font: bool,
    /// Custom icon overrides for Space headers.
    pub icons: UiIconsConfig,
    /// Workspace preselected in the Home composer for a new thread.
    /// Default: current_checkout.
    pub new_thread_workspace: NewThreadWorkspaceConfig,
    /// Directory the add-project / directory picker starts in. Empty means the
    /// last used directory, which is the current behaviour. Default: empty.
    pub add_project_start_dir: String,
    /// Ask for a tab name before creating a new tab. Default: true.
    pub prompt_new_tab_name: bool,
    /// Ask for a workspace name before interactive creation. Default: false.
    pub prompt_new_workspace_name: bool,
    /// Draw borders around split panes. Default: true.
    pub pane_borders: bool,
    /// Draw the outer border around the pane grid. Default: true.
    pub pane_outer_borders: bool,
    /// Draw interactive scrollbars beside terminal panes. Default: true.
    pub pane_scrollbars: bool,
    /// Keep split panes visually separated instead of sharing divider borders. Default: true.
    pub pane_gaps: bool,
    /// Show agent labels in split pane borders when no manual pane label is set. Default: false.
    pub show_agent_labels_on_pane_borders: bool,
    /// Hide the tab row when the workspace has one tab. Default: false.
    pub hide_tab_bar_when_single_tab: bool,
    /// Show the pull button in the top-right action row. It is the only entry
    /// point to the git menu, so turn it on to reach that menu. Default: false.
    pub show_pull_button: bool,
    /// Open the dock automatically when the focused pane carries a pull
    /// request, ticket, or conversation. Off by default: the status row names
    /// the link beside the title instead, and clicking that name opens it. The
    /// dock tab strip's `◧` toggle writes this setting too. Default: false.
    pub open_dock_on_work_link: bool,
    /// Show the split-below and split-right buttons in the top-right action
    /// row. Default: false.
    pub show_pane_toggle_buttons: bool,
    /// Expand the top status row from bars alone to percentages and reset
    /// times. Toggled at runtime; this is only the starting state. Default: false.
    #[serde(default)]
    pub status_bar_expanded: bool,
    /// Full-width top status row.
    pub status_bar: StatusBarConfig,
    /// Legacy indexed-Agent projection ordering. The visible sidebar remains canonical.
    /// Saved values are "spaces" or "priority". Default: "spaces".
    /// Desktop tab row placement. Default: top.
    pub tab_bar_position: TabBarPositionConfig,
    /// Right-aligned tab-row status entries.
    pub tab_bar_right: Vec<TabBarRightEntryConfig>,
    /// Separator between right-aligned tab-row status entries.
    pub tab_bar_right_separator: String,
    /// Template for the host terminal window title.
    pub window_title: String,
    /// Open the home view on launch. Default: false.
    pub show_home_on_start: bool,
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
    /// Read-only cross-host agent inventory used by `herdr fleet status`.
    pub fleet: FleetConfig,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            manage_ssh_config: true,
            fleet: FleetConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FleetConfig {
    /// Stable name for this server in cross-host agent references.
    /// Defaults to the configured local fleet host, then the system hostname.
    pub self_name: Option<String>,
    /// Fleet inventory refresh interval. Default: 5000 milliseconds.
    pub refresh_interval_ms: u64,
    /// Per-host read deadline. Default: 5000 milliseconds.
    pub timeout_ms: u64,
    /// A non-terminal run heartbeat older than this has unknown liveness.
    /// Default: 1800000 milliseconds (30 minutes).
    pub heartbeat_stale_ms: u64,
    /// Optional configured host whose localhost Temporal service backs Symphony.
    pub symphony_host: Option<String>,
    /// Host that produces aloop findings and run records. Read over SSH unless
    /// it names this machine (`self_name`); then the local store is read.
    /// Default: "ub2".
    pub aloop_host: Option<String>,
    /// Configured local and SSH hosts. Empty by default.
    pub hosts: Vec<FleetHostConfig>,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            self_name: None,
            refresh_interval_ms: 5_000,
            timeout_ms: 5_000,
            heartbeat_stale_ms: 30 * 60 * 1_000,
            symphony_host: None,
            aloop_host: Some("ub2".to_string()),
            hosts: Vec::new(),
        }
    }
}

impl FleetConfig {
    pub(crate) fn resolved_self_name(&self) -> String {
        self.resolved_self_name_with_hostname(crate::platform::hostname())
    }

    /// The producer host for the Aloops sidebar section. An empty or absent
    /// value falls back to the default rather than disabling the section.
    pub(crate) fn resolved_aloop_host(&self) -> String {
        self.aloop_host
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("ub2")
            .to_string()
    }

    pub(crate) fn resolved_self_name_with_hostname(&self, hostname: Option<String>) -> String {
        self.self_name
            .clone()
            .filter(|name| !name.trim().is_empty() && !name.contains("::"))
            .or_else(|| {
                self.hosts
                    .iter()
                    .find(|host| host.local)
                    .map(|host| host.name.clone())
                    .filter(|name| !name.trim().is_empty() && !name.contains("::"))
            })
            .or(hostname)
            .filter(|name| !name.trim().is_empty() && !name.contains("::"))
            .unwrap_or_else(|| "localhost".to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorkIndexConfig {
    /// Enable repo-wide GitHub, Linear, agent-pane, and day-link work indexing. Default: false.
    pub enabled: bool,
    /// Refresh interval in seconds. Default: 300.
    pub refresh_interval_seconds: u64,
    /// Linear team key used for started-ticket discovery.
    pub linear_team: Option<String>,
    /// GitHub repositories to include as `owner/repo` values.
    pub repos: Vec<String>,
}

impl Default for WorkIndexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            refresh_interval_seconds: 300,
            linear_team: None,
            repos: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FleetHostConfig {
    /// Stable display name and `--hosts` selector.
    pub name: String,
    /// Optional sidebar machine glyph for this host (with Nerd Font enabled).
    pub icon: Option<String>,
    /// OpenSSH target. Required unless `local = true`.
    pub target: String,
    /// Read this machine's socket and run-state directory without SSH.
    pub local: bool,
    /// Optional Herdr socket override for this host.
    pub socket: Option<String>,
    /// Optional named Herdr session opened when this host is activated.
    pub session: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    /// Allow launching herdr inside an existing herdr pane. Default: false.
    pub allow_nested: bool,
    /// Experimental local Kitty graphics rendering for attached clients. Default: false.
    pub kitty_graphics: bool,
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
    /// qodercli, qoder, qwen, qwen-code, maki.
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
            prefix: "ctrl+b".into(),
            help: BindingConfig::one("prefix+?"),
            settings: BindingConfig::one("prefix+s"),
            new_workspace: BindingConfig::one("prefix+shift+n"),
            new_thread: BindingConfig::one("prefix+c"),
            new_worktree: BindingConfig::one("prefix+shift+g"),
            open_worktree: BindingConfig::empty(),
            remove_worktree: BindingConfig::empty(),
            rename_workspace: BindingConfig::one("prefix+shift+w"),
            close_workspace: BindingConfig::one("prefix+shift+d"),
            workspace_picker: BindingConfig::one("prefix+w"),
            goto: BindingConfig::one("prefix+g"),
            command_palette: BindingConfig::Many(vec!["ctrl+alt+p".into(), "super+p".into()]),
            navigate_workspace_up: BindingConfig::one("up"),
            navigate_workspace_down: BindingConfig::one("down"),
            navigate_pane_left: BindingConfig::one("h"),
            navigate_pane_down: BindingConfig::one("j"),
            navigate_pane_up: BindingConfig::one("k"),
            navigate_pane_right: BindingConfig::one("l"),
            detach: BindingConfig::one("prefix+q"),
            reload_config: BindingConfig::one("prefix+shift+r"),
            open_notification_target: BindingConfig::one("prefix+o"),
            open_work_link: BindingConfig::one("prefix+u"),
            copy_work_link: BindingConfig::one("prefix+shift+u"),
            open_work_url: BindingConfig::empty(),
            copy_work_url: BindingConfig::empty(),
            copy_work_ticket: BindingConfig::one("prefix+ctrl+u"),
            copy_work_pr: BindingConfig::one("prefix+alt+u"),
            copy_work_preview: BindingConfig::one("prefix+ctrl+shift+u"),
            toggle_theme: BindingConfig::empty(),
            previous_workspace: BindingConfig::empty(),
            next_workspace: BindingConfig::empty(),
            previous_agent: BindingConfig::empty(),
            next_agent: BindingConfig::empty(),
            next_review_agent: BindingConfig::one("prefix+ctrl+r"),
            focus_agent: BindingConfig::empty(),
            remote_image_paste: "ctrl+v".into(),
            new_tab: BindingConfig::one("prefix+alt+c"),
            rename_tab: BindingConfig::one("prefix+shift+t"),
            toggle_tab_prio: BindingConfig::one("prefix+shift+f"),
            toggle_prio_panel: BindingConfig::empty(),
            previous_tab: BindingConfig::one("prefix+ctrl+p"),
            next_tab: BindingConfig::one("prefix+ctrl+n"),
            move_tab_previous: BindingConfig::empty(),
            move_tab_next: BindingConfig::empty(),
            previous_window: BindingConfig::one("prefix+p"),
            next_window: BindingConfig::one("prefix+n"),
            next_blocked_window: BindingConfig::one("prefix+b"),
            switch_tab: BindingConfig::one("prefix+1..9"),
            switch_workspace: BindingConfig::empty(),
            close_tab: BindingConfig::one("prefix+shift+x"),
            rename_pane: BindingConfig::one("prefix+shift+p"),
            edit_scrollback: BindingConfig::one("prefix+e"),
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
            split_left: BindingConfig::one("prefix+shift+v"),
            split_up: BindingConfig::one("prefix+shift+minus"),
            close_pane: BindingConfig::one("prefix+x"),
            zoom: BindingConfig::one("prefix+z"),
            toggle_pin_tab: BindingConfig::empty(),
            resize_mode: BindingConfig::one("prefix+r"),
            resize_pane_left: BindingConfig::empty(),
            resize_pane_down: BindingConfig::empty(),
            resize_pane_up: BindingConfig::empty(),
            resize_pane_right: BindingConfig::empty(),
            toggle_sidebar: BindingConfig::one("prefix+shift+b"),
            focus_sidebar: BindingConfig::empty(),
            assign_pane_to_pod: BindingConfig::empty(),
            focus_owning_repo_group: BindingConfig::one("prefix+i"),
            sidebar_cycle_group_mode: BindingConfig::empty(),
            sidebar_refresh: BindingConfig::empty(),
            toggle_blocked_filter: BindingConfig::one("prefix+f"),
            toggle_dock: BindingConfig::Many(vec!["prefix+shift+e".into(), "ctrl+alt+d".into()]),
            previous_dock_tab: BindingConfig::one("prefix+shift+["),
            next_dock_tab: BindingConfig::one("prefix+shift+]"),
            editor_open_repo: BindingConfig::empty(),
            edit_scratchpad: BindingConfig::one("ctrl+alt+e"),
            show_scratchpad: BindingConfig::one("ctrl+alt+n"),
            toggle_notepad: BindingConfig::one("ctrl+alt+m"),
            toggle_pomodoro: BindingConfig::one("ctrl+alt+b"),
            symphony: BindingConfig::one("prefix+shift+s"),
            runs: BindingConfig::one("prefix+alt+r"),
            work: BindingConfig::one("prefix+ctrl+w"),
            usage: BindingConfig::one("prefix+ctrl+y"),
            tickets: BindingConfig::one("prefix+ctrl+t"),
            missive: BindingConfig::empty(),
            inbox: BindingConfig::Many(vec!["prefix+shift+i".into(), "ctrl+alt+i".into()]),
            home: BindingConfig::one("ctrl+alt+h"),
            git_pull: BindingConfig::empty(),
            git_commit: BindingConfig::empty(),
            git_push: BindingConfig::empty(),
            git_create_pr: BindingConfig::empty(),
            dock_home: BindingConfig::empty(),
            dock_terminal: BindingConfig::empty(),
            dock_files: BindingConfig::empty(),
            dock_diff: BindingConfig::empty(),
            dock_pr: BindingConfig::empty(),
            dock_linear: BindingConfig::empty(),
            dock_missive: BindingConfig::empty(),
            dock_agents: BindingConfig::empty(),
            dock_shortcuts: BindingConfig::empty(),
            dock_context: BindingConfig::empty(),
            dock_symphony: BindingConfig::empty(),
            toggle_status_detail: BindingConfig::one("prefix+shift+m"),
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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UiIconsConfig {
    /// Repo names, owner/repo bindings, or Space labels mapped to icon glyphs.
    pub spaces: BTreeMap<String, String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 26,
            working_row_opacity_percent: 100,
            sidebar_animation: true,
            window_cycle_mode: WindowCycleModeConfig::default(),
            skip_collapsed_cycle: false,
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
            combine_repos_across_hosts: false,
            hide_whitespace_in_diff: false,
            nerd_font: true,
            icons: UiIconsConfig::default(),
            new_thread_workspace: NewThreadWorkspaceConfig::CurrentCheckout,
            add_project_start_dir: String::new(),
            prompt_new_tab_name: true,
            prompt_new_workspace_name: false,
            pane_borders: true,
            pane_outer_borders: true,
            pane_scrollbars: true,
            show_pull_button: false,
            open_dock_on_work_link: false,
            show_pane_toggle_buttons: false,
            pane_gaps: true,
            show_agent_labels_on_pane_borders: false,
            hide_tab_bar_when_single_tab: false,
            status_bar_expanded: false,
            status_bar: StatusBarConfig::default(),
            tab_bar_position: TabBarPositionConfig::Hidden,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: " ".into(),
            window_title: super::window_title::default_window_title(),
            show_home_on_start: false,
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
            terminal_backend: TerminalNotificationBackend::Auto,
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
            terminal_backend: TerminalNotificationBackend,
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
            terminal_backend: raw.terminal_backend,
            delay_seconds,
            herdr: raw.herdr,
            clipboard: raw.clipboard,
        })
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
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
    fn stalled_agent_nudge_config_defaults_are_opt_in_and_bounded() {
        let session = SessionConfig::default();
        assert!(!session.auto_nudge_stalled_agents);
        assert_eq!(session.agent_stale_after_minutes, 5);
        assert_eq!(session.agent_subagent_stale_after_minutes, 30);
        assert_eq!(session.nudge_after_minutes, 5);
        assert_eq!(session.max_nudges, 3);
        assert_eq!(session.max_nudges_without_human, 5);
        assert_eq!(
            session.stall_nudge_message,
            "Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue."
        );
    }

    #[test]
    fn four_way_split_defaults_are_collision_safe_and_legacy_names_keep_edges() {
        let keys = Config::default().keys;
        assert_eq!(keys.split_vertical, BindingConfig::one("prefix+v"));
        assert_eq!(keys.split_left, BindingConfig::one("prefix+shift+v"));
        assert_eq!(keys.split_horizontal, BindingConfig::one("prefix+minus"));
        assert_eq!(keys.split_up, BindingConfig::one("prefix+shift+minus"));
        let all = [
            "prefix+v",
            "prefix+shift+v",
            "prefix+minus",
            "prefix+shift+minus",
        ];
        assert_eq!(
            all.into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            4
        );
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
    fn update_channel_default_follows_windows_build_identity() {
        assert_eq!(
            default_update_channel_for_build(true, true),
            UpdateChannelConfig::Preview
        );
        assert_eq!(
            default_update_channel_for_build(true, false),
            UpdateChannelConfig::Stable
        );
        assert_eq!(
            default_update_channel_for_build(false, true),
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
    fn sidebar_panel_defaults_load_when_older_configs_omit_the_keys() {
        let config: Config = toml::from_str(
            "[notepad]\nenabled = true\n[goals_panel]\n[pomodoro]\nenabled = true\n",
        )
        .expect("older config parses with sidebar panel defaults");

        assert_eq!(config.notepad.visible_tabs, ["usage"]);
        assert!(!config.goals_panel.enabled);
        assert!(config.pomodoro.enabled);
        assert!(!config.pomodoro.sidebar_visible);
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
        assert!(config.usage.pricing.contains_key("claude-opus-5"));
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
        assert_eq!(default_config.session.hide_done_after_minutes, 30);
        assert_eq!(default_config.session.reap_done_after_minutes, 240);
        assert!(default_config.session.reap_done_panes);
        assert_eq!(default_config.session.settle_after_days, 3);
        assert_eq!(default_config.session.settle_finished_after_minutes, 10);
        assert!(default_config.session.settle_stops_agent);
        assert!(default_config.session.auto_settle_done);
        assert_eq!(default_config.session.settle_done_after_minutes, 30);

        let toml = r#"
[session]
resume_agents_on_restore = false
hide_done_after_minutes = 15
reap_done_after_minutes = 90
reap_done_panes = false
settle_after_days = 7
settle_finished_after_minutes = 20
settle_stops_agent = false
auto_settle_done = false
settle_done_after_minutes = 45
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.session.resume_agents_on_restore);
        assert_eq!(config.session.hide_done_after_minutes, 15);
        assert_eq!(config.session.reap_done_after_minutes, 90);
        assert!(!config.session.reap_done_panes);
        assert_eq!(config.session.settle_after_days, 7);
        assert_eq!(config.session.settle_finished_after_minutes, 20);
        assert!(!config.session.settle_stops_agent);
        assert!(!config.session.auto_settle_done);
        assert_eq!(config.session.settle_done_after_minutes, 45);
    }

    #[test]
    fn day_board_stale_after_defaults_to_ten_minutes_and_parses_seconds() {
        assert_eq!(Config::default().day_board.stale_after, 600);
        let config: Config =
            toml::from_str("[day_board]\nstale_after = 90\n").expect("day board config");
        assert_eq!(config.day_board.stale_after, 90);
    }

    #[test]
    fn work_index_stays_opt_in_so_link_derived_day_completion_is_opt_in_too() {
        assert!(!Config::default().work_index.enabled);
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
    fn panel_default_surfaces_parse_in_configured_order() {
        let config: Config = toml::from_str(
            r#"
[panel]
default_surfaces = ["home", "pull_request", "hosts", "keys", "note"]
"#,
        )
        .expect("panel config");

        assert_eq!(
            config.panel.default_surfaces,
            vec![
                PanelSurfaceConfig::Home,
                PanelSurfaceConfig::PullRequest,
                PanelSurfaceConfig::Hosts,
                PanelSurfaceConfig::Shortcuts,
                PanelSurfaceConfig::Scratchpad,
            ]
        );
        assert!(Config::default().panel.default_surfaces.is_empty());
    }

    #[test]
    fn fleet_polling_and_named_session_parse_with_defaults() {
        let defaults = Config::default().remote.fleet;
        assert!(defaults.self_name.is_none());
        assert!(!defaults.resolved_self_name().is_empty());
        assert_eq!(defaults.refresh_interval_ms, 5_000);
        assert!(defaults.hosts.is_empty());
        // MAT-159 SCH2: the aloop producer host defaults to ub2.
        assert_eq!(defaults.resolved_aloop_host(), "ub2");

        let config: Config = toml::from_str(
            r#"
[remote.fleet]
self_name = "laptop"
refresh_interval_ms = 30000
symphony_host = "workbox"
aloop_host = "buildbox"

[[remote.fleet.hosts]]
name = "workbox"
target = "workbox"
session = "agents"
icon = "◆"
"#,
        )
        .expect("fleet config");
        assert_eq!(config.remote.fleet.self_name.as_deref(), Some("laptop"));
        assert_eq!(config.remote.fleet.refresh_interval_ms, 30_000);
        assert_eq!(
            config.remote.fleet.symphony_host.as_deref(),
            Some("workbox")
        );
        assert_eq!(config.remote.fleet.aloop_host.as_deref(), Some("buildbox"));
        assert_eq!(config.remote.fleet.resolved_aloop_host(), "buildbox");
        assert_eq!(
            config.remote.fleet.hosts[0].session.as_deref(),
            Some("agents")
        );
        assert_eq!(config.remote.fleet.hosts[0].icon.as_deref(), Some("◆"));

        let blank: FleetConfig = toml::from_str("aloop_host = \"  \"").expect("blank aloop host");
        assert_eq!(blank.resolved_aloop_host(), "ub2");
    }

    #[test]
    fn fleet_self_name_prefers_the_local_host_before_the_os_hostname() {
        let fleet = FleetConfig {
            hosts: vec![
                FleetHostConfig {
                    name: "ub1".into(),
                    target: "ub1".into(),
                    ..FleetHostConfig::default()
                },
                FleetHostConfig {
                    name: "mbpro".into(),
                    local: true,
                    ..FleetHostConfig::default()
                },
            ],
            ..FleetConfig::default()
        };
        assert_eq!(
            fleet.resolved_self_name_with_hostname(Some("Darwins-MacBook-Pro.local".into())),
            "mbpro"
        );

        let without_local = FleetConfig {
            hosts: vec![FleetHostConfig {
                name: "ub1".into(),
                target: "ub1".into(),
                ..FleetHostConfig::default()
            }],
            ..FleetConfig::default()
        };
        assert_eq!(
            without_local.resolved_self_name_with_hostname(Some("workstation".into())),
            "workstation"
        );
    }

    #[test]
    fn linear_default_layout_defaults_to_list_and_parses_board() {
        assert_eq!(
            Config::default().linear.default_layout,
            LinearLayoutConfig::List
        );
        let config: Config = toml::from_str(
            r#"
[linear]
default_layout = "board"
"#,
        )
        .expect("linear config");
        assert_eq!(config.linear.default_layout, LinearLayoutConfig::Board);
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
    fn pane_appearance_defaults_and_parse() {
        let default_config = Config::default();
        assert!(default_config.ui.pane_borders);
        assert!(default_config.ui.pane_scrollbars);
        assert!(default_config.ui.pane_gaps);
        assert!(!default_config.ui.show_agent_labels_on_pane_borders);
        assert!(!default_config.ui.hide_tab_bar_when_single_tab);
        assert!(!default_config.ui.show_pull_button);
        assert!(!default_config.ui.show_pane_toggle_buttons);
        assert_eq!(
            default_config.ui.tab_bar_position,
            TabBarPositionConfig::Hidden
        );

        let toml = r#"
[ui]
pane_borders = false
pane_scrollbars = false
pane_gaps = true
show_agent_labels_on_pane_borders = true
hide_tab_bar_when_single_tab = true
show_pull_button = true
show_pane_toggle_buttons = true
tab_bar_position = "bottom"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.show_pull_button);
        assert!(config.ui.show_pane_toggle_buttons);
        assert!(!config.ui.pane_borders);
        assert!(!config.ui.pane_scrollbars);
        assert!(config.ui.pane_gaps);
        assert!(config.ui.show_agent_labels_on_pane_borders);
        assert!(config.ui.hide_tab_bar_when_single_tab);
        assert_eq!(config.ui.tab_bar_position, TabBarPositionConfig::Bottom);

        // The default is also nameable, so a config that opted into a row can
        // be moved back to hidden without deleting the key.
        let config: Config = toml::from_str("[ui]\ntab_bar_position = \"hidden\"\n").unwrap();
        assert_eq!(config.ui.tab_bar_position, TabBarPositionConfig::Hidden);
    }

    #[test]
    fn show_home_on_start_defaults_off_and_can_be_turned_on() {
        assert!(!Config::default().ui.show_home_on_start);
        let config: Config =
            toml::from_str("[ui]\nshow_home_on_start = true\n").expect("parse config");
        assert!(config.ui.show_home_on_start);
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
    fn working_row_opacity_defaults_parses_and_rejects_invisible_values() {
        assert_eq!(Config::default().ui.working_row_opacity_percent, 100);
        let config: Config = toml::from_str("[ui]\nworking_row_opacity_percent = 25\n")
            .expect("valid working row opacity");
        assert_eq!(config.ui.working_row_opacity_percent, 25);
        for value in [0, 24, 101] {
            let text = format!("[ui]\nworking_row_opacity_percent = {value}\n");
            assert!(toml::from_str::<Config>(&text).is_err(), "accepted {value}");
        }
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
    fn fleet_workspace_ac10_cycle_settings_default_and_parse_from_ui_config() {
        let default = Config::default();
        assert_eq!(
            default.ui.window_cycle_mode,
            WindowCycleModeConfig::ThisMachine
        );
        assert!(!default.ui.skip_collapsed_cycle);

        let configured: Config = toml::from_str(
            "[ui]\nwindow_cycle_mode = \"this-machine-and-fleet\"\nskip_collapsed_cycle = true\n",
        )
        .expect("cycle settings parse");
        assert_eq!(
            configured.ui.window_cycle_mode,
            WindowCycleModeConfig::ThisMachineAndFleet
        );
        assert!(configured.ui.skip_collapsed_cycle);
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
    fn stale_full_lifecycle_hook_authority_falls_back_to_screen_config_bounds() {
        assert_eq!(
            Config::default()
                .agent_detection
                .full_lifecycle_hook_authority_timeout_seconds,
            600
        );
        for value in [30, 600, 3600] {
            let config: Config = toml::from_str(&format!(
                "[agent_detection]\nfull_lifecycle_hook_authority_timeout_seconds = {value}\n"
            ))
            .unwrap();
            assert_eq!(
                config
                    .agent_detection
                    .full_lifecycle_hook_authority_timeout_seconds,
                value
            );
        }
        for value in [0, 29, 3601] {
            assert!(toml::from_str::<Config>(&format!(
                "[agent_detection]\nfull_lifecycle_hook_authority_timeout_seconds = {value}\n"
            ))
            .is_err());
        }
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
        assert_eq!(
            config.ui.toast.terminal_backend,
            TerminalNotificationBackend::Auto
        );
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
        assert_eq!(
            config.ui.toast.terminal_backend,
            TerminalNotificationBackend::Auto
        );
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
    fn toast_config_parses_explicit_terminal_backend() {
        let config: Config = toml::from_str(
            r#"
[ui.toast]
delivery = "terminal"
terminal_backend = "osc99"
"#,
        )
        .unwrap();

        assert_eq!(
            config.ui.toast.terminal_backend,
            TerminalNotificationBackend::Osc99
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
    fn kitty_graphics_default_off_and_parse() {
        let config = Config::default();
        assert!(!config.experimental.kitty_graphics);

        let toml = r#"
[experimental]
kitty_graphics = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.experimental.kitty_graphics);
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
        assert!(config.experimental.kitty_graphics);
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

    #[test]
    fn nerd_font_defaults_on_and_accepts_an_ascii_fallback() {
        assert!(Config::default().ui.nerd_font);
        assert_eq!(Config::default().files.icons, FilesIconConfig::Nerd);
        let config: Config = toml::from_str("[ui]\nnerd_font = false\n").unwrap();
        assert!(!config.ui.nerd_font);

        let config: Config =
            toml::from_str("[ui.icons.spaces]\n\"owner/herdr\" = \"◆\"\nFriendly = \"◇\"\n")
                .unwrap();
        assert_eq!(config.ui.icons.spaces["owner/herdr"], "◆");
        assert_eq!(config.ui.icons.spaces["Friendly"], "◇");
    }

    #[test]
    fn source_control_keys_default_to_todays_behaviour_and_parse() {
        let defaults = Config::default().source_control;
        assert_eq!(defaults.merge_method, MergeMethodConfig::Merge);
        assert_eq!(defaults.commit_message_model, "");
        assert!(!defaults.commit_stage_all);
        assert_eq!(defaults.branch_prefix, "issue/");

        let config: Config = toml::from_str(
            "[source_control]\nmerge_method = \"rebase\"\ncommit_message_model = \"claude-opus-5\"\ncommit_stage_all = true\nbranch_prefix = \"feat/\"\n",
        )
        .unwrap();
        assert_eq!(
            config.source_control.merge_method,
            MergeMethodConfig::Rebase
        );
        assert_eq!(config.source_control.commit_message_model, "claude-opus-5");
        assert!(config.source_control.commit_stage_all);
        assert_eq!(config.source_control.branch_prefix, "feat/");
    }

    #[test]
    fn missive_config_names_token_environment_and_resource_ids() {
        let defaults = Config::default();
        assert_eq!(defaults.missive.token_env, "MISSIVE_API_TOKEN");
        assert_eq!(defaults.missive.team, None);
        assert_eq!(defaults.missive.organization, None);

        let config: Config = toml::from_str(
            "[missive]\ntoken_env = 'HERDR_TEST_MISSIVE'\nteam = 'team-id'\norganization = 'org-id'\n",
        )
        .unwrap();
        assert_eq!(config.missive.token_env, "HERDR_TEST_MISSIVE");
        assert_eq!(config.missive.team.as_deref(), Some("team-id"));
        assert_eq!(config.missive.organization.as_deref(), Some("org-id"));
    }

    #[test]
    fn usage_pricing_has_documented_defaults_and_accepts_model_overrides() {
        let defaults = Config::default();
        for model in [
            "claude-fable-5-1",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-haiku-4-5",
            "gpt-5.6-sol",
            "gpt-5.6-luna",
            "gpt-5.3-codex-spark",
        ] {
            assert!(defaults.usage.pricing.contains_key(model), "{model}");
        }
        let config: Config = toml::from_str(
            "[usage.pricing]\ncustom = { input = 2.0, output = 4.0, cache_write = 1.0, cache_read = 0.5 }\n",
        )
        .unwrap();
        assert_eq!(
            config.usage.pricing.get("custom"),
            Some(&UsageModelPricing {
                input: 2.0,
                output: 4.0,
                cache_write: 1.0,
                cache_read: 0.5,
            })
        );
    }
}
