use crossterm::event::{KeyCode, KeyModifiers};

mod io;
mod keybinds;
mod model;
mod sidebar;
mod sound;
mod tab_bar;
mod theme;
mod window_title;
mod write;

pub(crate) use self::model::MAX_NUDGE_AFTER_MINUTES;
pub use self::{
    io::{
        config_diagnostic_summary, config_dir, config_path, load_live_config,
        remove_keybinding_config_sections, remove_section_key, state_dir, upsert_section_bool,
        upsert_section_value, write_actions_atomically,
    },
    keybinds::{
        format_key_combo, normalize_key_combo, terminal_key_matches_combo,
        validate_user_action_key, ActionKeybinds, BindingConfig, CommandKeybindConfig,
        CustomCommandAction, CustomCommandKeybind, IndexedKeybind, Keybinds, LiveKeybindConfig,
        UserAction,
    },
    model::{
        validated_sidebar_bounds, ActionConfig, AgentPanelSortConfig, Config, ConfigReloadReport,
        ConfigReloadStatus, FilesIconConfig, FleetConfig, FleetHostConfig, GoalsPanelConfig,
        HostCursorModeConfig, LaunchProfileConfig, LinearLayoutConfig, MergeMethodConfig,
        MissiveConfig, NewTerminalCwdConfig, NewThreadWorkspaceConfig, NotepadConfig, PanelConfig,
        PanelSurfaceConfig, PomodoroConfig, ProjectConfig, ShellModeConfig,
        SidebarCollapsedModeConfig, StatusIndicatorStyle, TabBarPositionConfig,
        TerminalNotificationBackend, ToastClipboardPosition, ToastConfig, ToastDelivery,
        ToastHerdrPosition, UpdateChannelConfig, UsageConfig, UsageModelPricing,
        WindowCycleModeConfig, WorkIndexConfig, MAX_TOAST_DELAY_SECONDS,
    },
    sidebar::{
        AgentSidebarToken, AgentsSidebarConfig, SidebarAreasConfig, SidebarConfig,
        SidebarHeaderConfig, SidebarLayoutConfig, SpaceSidebarToken, SpacesSidebarConfig,
    },
    sound::SoundConfig,
    tab_bar::TabBarRightEntryConfig,
    theme::{
        parse_color, CustomThemeColors, HostAppearanceOverride, ModeThemeColors, ThemeConfig,
        THEME_NAMES,
    },
    window_title::{WindowTitlePart, WindowTitleTemplate, WindowTitleToken},
};

pub(crate) use self::sidebar::SidebarArea;

pub(crate) use self::keybinds::parse_key_combo;
#[cfg(test)]
pub(crate) use self::model::DEFAULT_BRANCH_PREFIX;
#[cfg(test)]
pub(crate) use self::sidebar::SidebarTokenStyle;
pub(crate) use self::write::update_file_at;
pub(crate) use self::{
    io::upsert_top_level_bool,
    tab_bar::{
        parse_tab_bar_datetime_format, tab_bar_right_diagnostics,
        MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS, MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS,
        MAX_TAB_BAR_RIGHT_ENTRIES,
    },
    theme::canonical_theme_name,
    window_title::{sanitize_window_title_text, window_title_diagnostics},
};

pub const CONFIG_PATH_ENV_VAR: &str = "HERDR_CONFIG_PATH";
pub const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;
pub const DEFAULT_MOUSE_SCROLL_LINES: usize = 3;
pub const DEFAULT_MOBILE_WIDTH_THRESHOLD: u16 = 64;
pub const DEFAULT_HEADLESS_COLS: u16 = 120;
pub const DEFAULT_HEADLESS_ROWS: u16 = 40;
pub const DEFAULT_THEME_AUTO_SWITCH_POLL_INTERVAL_SECONDS: u64 = 60;
pub const MIN_THEME_AUTO_SWITCH_POLL_INTERVAL_SECONDS: u64 = 5;
pub const MAX_THEME_AUTO_SWITCH_POLL_INTERVAL_SECONDS: u64 = 3600;

#[cfg(test)]
pub(crate) fn app_dir_name() -> &'static str {
    io::app_dir_name()
}

#[cfg(test)]
pub(crate) fn test_config_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// A monotonically increasing, process-unique suffix for test scratch paths.
///
/// `SystemTime::now()` is not a unique-name source: its resolution is coarse
/// enough that two threads starting in the same tick derive the same path, so
/// two tests race on one directory or one socket. A counter cannot collide.
#[cfg(test)]
pub(crate) fn test_unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Serialises tests that read or write the process-global environment that
/// resolves config, state, and socket paths.
///
/// `config_dir()`, `state_dir()`, and every socket path derived from them read
/// `XDG_CONFIG_HOME`/`XDG_STATE_HOME`/`HERDR_CONFIG_PATH` at call time. Those
/// are per-process, not per-test, so a test that repoints them moves the
/// filesystem under every other test in the same binary. Holding this guard is
/// the only way to make such a test's view of those paths stable.
///
/// The guard restores every variable it touched on drop, so a panicking test
/// leaves the environment clean, and it recovers from mutex poisoning, so a
/// panicking test does not turn one failure into a cascade of `PoisonError`s
/// in unrelated tests.
#[cfg(test)]
pub(crate) struct TestConfigEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(String, Option<std::ffi::OsString>)>,
}

#[cfg(test)]
impl TestConfigEnvGuard {
    /// Acquire the guard without changing anything, for tests that only *read*
    /// env-derived paths and must not observe another test's override.
    pub(crate) fn acquire() -> Self {
        let lock = test_config_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            _lock: lock,
            saved: Vec::new(),
        }
    }

    fn remember(&mut self, key: &str) {
        if self.saved.iter().any(|(saved, _)| saved == key) {
            return;
        }
        self.saved.push((key.to_owned(), std::env::var_os(key)));
    }

    pub(crate) fn set(&mut self, key: &str, value: impl AsRef<std::ffi::OsStr>) {
        self.remember(key);
        std::env::set_var(key, value);
    }

    pub(crate) fn remove(&mut self, key: &str) {
        self.remember(key);
        std::env::remove_var(key);
    }

    /// Restore every variable this guard changed, keeping the lock held.
    ///
    /// Idempotent, so callers that need to run cleanup *after* the environment
    /// is back to normal but *before* the lock is released can call it and
    /// still rely on `Drop`.
    pub(crate) fn restore(&mut self) {
        for (key, previous) in self.saved.drain(..).rev() {
            match previous {
                Some(previous) => std::env::set_var(&key, previous),
                None => std::env::remove_var(&key),
            }
        }
    }
}

#[cfg(test)]
impl Drop for TestConfigEnvGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

impl Config {
    pub fn should_show_onboarding(&self) -> bool {
        self.onboarding.unwrap_or(true)
    }

    pub fn prefix_key(&self) -> (KeyCode, KeyModifiers) {
        self.validated_keybinds().1
    }

    /// Parsed keybinds for Herdr actions.
    pub fn keybinds(&self) -> Keybinds {
        self.validated_keybinds().3
    }

    pub fn collect_diagnostics(&self) -> Vec<String> {
        let (prefix_diag, _, keybind_diags, _) = self.validated_keybinds();
        prefix_diag
            .into_iter()
            .chain(keybind_diags)
            .chain(self.remote_image_paste_key().err())
            .chain(self.theme.diagnostics())
            .chain(self.ui.sound.diagnostics())
            .chain(tab_bar_right_diagnostics(&self.ui.tab_bar_right))
            .chain(window_title_diagnostics(&self.ui.window_title))
            .chain(self.invalid_sidebar_bounds_diagnostic())
            .chain(self.invalid_headless_size_diagnostic())
            .collect()
    }

    pub(crate) fn headless_size(&self) -> (u16, u16) {
        if self.invalid_headless_size_diagnostic().is_some() {
            (DEFAULT_HEADLESS_COLS, DEFAULT_HEADLESS_ROWS)
        } else {
            (self.server.headless_cols, self.server.headless_rows)
        }
    }

    pub(crate) fn invalid_headless_size_diagnostic(&self) -> Option<String> {
        (self.server.headless_cols == 0 || self.server.headless_rows == 0).then(|| {
            format!(
                "server.headless_cols and server.headless_rows must be greater than zero (got {}x{})",
                self.server.headless_cols, self.server.headless_rows
            )
        })
    }

    pub(crate) fn invalid_sidebar_bounds_diagnostic(&self) -> Option<String> {
        validated_sidebar_bounds(self.ui.sidebar_min_width, self.ui.sidebar_max_width)
            .is_none()
            .then(|| {
                format!(
                    "ui.sidebar_min_width ({}) is greater than sidebar_max_width ({})",
                    self.ui.sidebar_min_width, self.ui.sidebar_max_width
                )
            })
    }

    pub(crate) fn remote_image_paste_key(&self) -> Result<Option<(KeyCode, KeyModifiers)>, String> {
        let raw = self.keys.remote_image_paste.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        parse_key_combo(raw).map(Some).ok_or_else(|| {
            format!("invalid keybinding: keys.remote_image_paste = {raw:?}; disabling binding")
        })
    }

    #[cfg(test)]
    pub fn live_keybinds(&self) -> Result<LiveKeybindConfig, Vec<String>> {
        self.live_keybinds_with_diagnostics()
            .map(|(live, _diagnostics)| live)
    }

    pub(crate) fn live_keybinds_with_diagnostics(
        &self,
    ) -> Result<(LiveKeybindConfig, Vec<String>), Vec<String>> {
        let (prefix_diag, prefix, keybind_diags, keybinds) = self.validated_keybinds();
        if let Some(prefix_diag) = prefix_diag {
            Err(std::iter::once(prefix_diag).chain(keybind_diags).collect())
        } else {
            Ok((LiveKeybindConfig { prefix, keybinds }, keybind_diags))
        }
    }

    pub(crate) fn local_keybindings_profile_toml(&self) -> Result<String, toml::ser::Error> {
        #[derive(serde::Serialize)]
        struct KeysProfile {
            keys: model::KeysConfigOverlay,
        }

        toml::to_string_pretty(&KeysProfile {
            keys: self.keys.local_profile(&self.keybinds()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_keybindings_profile_includes_defaults_and_excludes_commands() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_tab = "prefix+t"

[[keys.command]]
key = "prefix+g"
command = "lazygit"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        assert!(profile.contains("[keys]"));
        assert!(profile.contains("prefix = \"ctrl+a\""));
        assert!(profile.contains("new_tab = \"prefix+t\""));
        assert!(profile.contains("next_tab = \"prefix+ctrl+n\""));
        assert!(profile.contains("next_window = \"prefix+n\""));
        assert!(profile.contains("next_blocked_window = \"prefix+b\""));
        assert!(profile.contains("toggle_sidebar = \"prefix+shift+b\""));
        assert!(!profile.contains("lazygit"));
        assert!(!profile.contains("command ="));
        assert!(!profile.contains("[[keys.command]]"));
    }

    #[test]
    fn local_keybindings_profile_preserves_user_default_provenance() {
        let config: Config = toml::from_str(
            r#"
[keys]
zoom = "prefix+?"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(profile.contains("zoom = \"prefix+?\""));
        assert!(!profile.contains("help = \"prefix+?\""));
        assert!(round_tripped
            .keybinds()
            .zoom
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+?"));
        assert!(round_tripped.keybinds().help.bindings.is_empty());
    }

    #[test]
    fn local_keybindings_profile_omits_default_displaced_by_user_prefix() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "n"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(profile.contains("prefix = \"n\""));
        assert!(!profile.contains("next_window = \"prefix+n\""));
        assert!(round_tripped.keybinds().next_window.bindings.is_empty());
    }

    #[test]
    fn local_keybindings_profile_preserves_legacy_indexed_tab_source() {
        let config: Config = toml::from_str(
            r#"
[keys.indexed]
tabs = "ctrl"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();
        let keybinds = round_tripped.keybinds();
        let switch_tab_labels: Vec<_> = keybinds
            .switch_tab
            .iter()
            .map(|binding| binding.label.as_str())
            .collect();

        assert!(profile.contains("[keys.indexed]"));
        assert!(profile.contains("tabs = \"ctrl\""));
        assert!(!profile.contains("switch_tab = \"prefix+1..9\""));
        assert_eq!(switch_tab_labels.len(), 9);
        assert!(switch_tab_labels
            .iter()
            .all(|label| label.starts_with("ctrl+")));
    }

    #[test]
    fn local_keybindings_profile_keeps_invalid_legacy_indexed_default_disabled() {
        let config: Config = toml::from_str(
            r#"
[keys.indexed]
tabs = "bogus"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(profile.contains("[keys.indexed]"));
        assert!(profile.contains("tabs = \"bogus\""));
        assert!(!profile.contains("switch_tab = \"prefix+1..9\""));
        assert!(round_tripped.keybinds().switch_tab.is_empty());
    }

    #[test]
    fn local_keybindings_profile_keeps_default_displaced_by_omitted_command_disabled() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+n"
command = "echo next"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(!profile.contains("[[keys.command]]"));
        assert!(!profile.contains("command ="));
        assert!(profile.contains("next_window = \"\""));
        assert!(round_tripped.keybinds().next_window.bindings.is_empty());
    }

    #[test]
    fn local_keybindings_profile_preserves_partially_displaced_indexed_default() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+1"
command = "echo one"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();
        let keybinds = round_tripped.keybinds();
        let switch_tab_labels: Vec<_> = keybinds
            .switch_tab
            .iter()
            .map(|binding| binding.label.as_str())
            .collect();

        assert!(!profile.contains("[[keys.command]]"));
        assert!(!profile.contains("switch_tab = \"prefix+1..9\""));
        assert!(profile.contains("\"prefix+2\""));
        assert!(profile.contains("\"prefix+9\""));
        assert!(!switch_tab_labels.contains(&"prefix+1"));
        assert_eq!(switch_tab_labels.len(), 8);
        assert!(switch_tab_labels
            .iter()
            .all(|label| label.starts_with("prefix+")));
    }

    #[test]
    fn remote_image_paste_key_defaults_to_ctrl_v() {
        let config = Config::default();
        assert_eq!(
            config.remote_image_paste_key().unwrap(),
            Some((KeyCode::Char('v'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn remote_image_paste_key_can_be_disabled() {
        let config: Config = toml::from_str("[keys]\nremote_image_paste = ''\n").unwrap();
        assert_eq!(config.remote_image_paste_key().unwrap(), None);
    }

    #[test]
    fn ui_host_cursor_defaults_to_auto_and_parses_overrides() {
        let default_config = Config::default();
        assert_eq!(default_config.ui.host_cursor, HostCursorModeConfig::Auto);

        let native: Config = toml::from_str("[ui]\nhost_cursor = 'native'\n").unwrap();
        assert_eq!(native.ui.host_cursor, HostCursorModeConfig::Native);

        let drawn: Config = toml::from_str("[ui]\nhost_cursor = 'drawn'\n").unwrap();
        assert_eq!(drawn.ui.host_cursor, HostCursorModeConfig::Drawn);
    }
}
