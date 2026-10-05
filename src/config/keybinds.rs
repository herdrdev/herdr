#[cfg(test)]
use crossterm::event::KeyEvent;
use crossterm::event::{KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::Config;
use crate::input::TerminalKey;
use crate::popup_size::PopupSize;

pub type KeyCombo = (KeyCode, KeyModifiers);

#[derive(Debug, Clone)]
pub struct LiveKeybindConfig {
    pub prefix: KeyCombo,
    pub keybinds: Keybinds,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BindingConfig {
    One(String),
    Many(Vec<String>),
}

impl Default for BindingConfig {
    fn default() -> Self {
        Self::One(String::new())
    }
}

impl BindingConfig {
    pub fn one(value: impl Into<String>) -> Self {
        Self::One(value.into())
    }

    pub fn empty() -> Self {
        Self::One(String::new())
    }

    fn values(&self) -> Vec<&str> {
        match self {
            Self::One(value) => vec![value.as_str()],
            Self::Many(values) => values.iter().map(String::as_str).collect(),
        }
    }

    pub(crate) fn has_values(&self) -> bool {
        self.values().iter().any(|value| !value.trim().is_empty())
    }

    pub(crate) fn indexed_labels(&self) -> Vec<String> {
        let mut labels = Vec::new();
        for raw in self.values() {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            match parse_binding_string(raw) {
                Some(ParsedBinding::Single(binding)) => {
                    if matches!(binding.trigger.combo().0, KeyCode::Char('1'..='9')) {
                        labels.push(binding.label);
                    }
                }
                Some(ParsedBinding::Range(range)) => {
                    labels.extend(range.into_iter().filter_map(|binding| {
                        matches!(binding.trigger.combo().0, KeyCode::Char('1'..='9'))
                            .then_some(binding.label)
                    }));
                }
                None => {}
            }
        }
        labels
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandKeybindType {
    #[default]
    Shell,
    Pane,
    Popup,
    PluginAction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CommandKeybindConfig {
    /// Key that runs a command. Use `prefix+g` for prefix mode or a modified chord for direct mode.
    pub key: BindingConfig,
    /// Command executed either in the background shell or inside a pane.
    pub command: String,
    /// Command execution mode. Default: "shell".
    #[serde(rename = "type")]
    pub action_type: CommandKeybindType,
    /// Optional user-defined description for this custom command.
    pub description: Option<String>,
    /// Optional popup width as cells or a percentage string when type = "popup".
    pub width: Option<PopupSize>,
    /// Optional popup height as cells or a percentage string when type = "popup".
    pub height: Option<PopupSize>,
}

impl Default for CommandKeybindConfig {
    fn default() -> Self {
        Self {
            key: BindingConfig::empty(),
            command: String::new(),
            action_type: CommandKeybindType::Shell,
            description: None,
            width: None,
            height: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomCommandAction {
    Shell,
    Pane,
    Popup,
    PluginAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingTrigger {
    Direct(KeyCombo),
    Prefix(KeyCombo),
}

impl BindingTrigger {
    pub fn combo(self) -> KeyCombo {
        match self {
            Self::Direct(combo) | Self::Prefix(combo) => combo,
        }
    }

    pub fn is_direct(self) -> bool {
        matches!(self, Self::Direct(_))
    }

    pub fn is_prefix(self) -> bool {
        matches!(self, Self::Prefix(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinding {
    pub trigger: BindingTrigger,
    pub label: String,
}

impl ResolvedBinding {
    #[cfg(test)]
    fn matches_key_event(&self, key: &KeyEvent) -> bool {
        key_event_matches_combo(key, self.trigger.combo())
    }

    fn matches_terminal_key(&self, key: &TerminalKey) -> bool {
        terminal_key_matches_combo(key, self.trigger.combo())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionKeybinds {
    pub bindings: Vec<ResolvedBinding>,
}

impl ActionKeybinds {
    #[cfg(test)]
    pub fn prefix(label: &str) -> Self {
        let raw = if label.starts_with("prefix+") {
            label.to_string()
        } else {
            format!("prefix+{label}")
        };
        let trigger = parse_binding_string(&raw)
            .and_then(|parsed| match parsed {
                ParsedBinding::Single(binding) => Some(binding),
                ParsedBinding::Range(_) => None,
            })
            .expect("prefix binding should parse");
        Self {
            bindings: vec![trigger],
        }
    }

    #[cfg(test)]
    pub fn direct(label: &str) -> Self {
        let trigger = parse_binding_string(label)
            .and_then(|parsed| match parsed {
                ParsedBinding::Single(binding) => Some(binding),
                ParsedBinding::Range(_) => None,
            })
            .expect("direct binding should parse");
        Self {
            bindings: vec![trigger],
        }
    }

    #[cfg(test)]
    pub fn matches_prefix(&self, key: &KeyEvent) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.trigger.is_prefix() && binding.matches_key_event(key))
    }

    pub fn matches_prefix_key(&self, key: &TerminalKey) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.trigger.is_prefix() && binding.matches_terminal_key(key))
    }

    pub fn matches_direct_key(&self, key: &TerminalKey) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.trigger.is_direct() && binding.matches_terminal_key(key))
    }

    pub fn labels(&self) -> Vec<String> {
        self.bindings
            .iter()
            .map(|binding| binding.label.clone())
            .collect()
    }

    pub fn label(&self) -> Option<String> {
        let labels = self.labels();
        if labels.is_empty() {
            None
        } else {
            Some(labels.join(" / "))
        }
    }

    pub fn prefix_rhs_label(&self) -> Option<String> {
        let labels: Vec<String> = self
            .bindings
            .iter()
            .filter(|binding| binding.trigger.is_prefix())
            .map(|binding| {
                binding
                    .label
                    .strip_prefix("prefix+")
                    .unwrap_or(&binding.label)
                    .to_string()
            })
            .collect();
        if labels.is_empty() {
            None
        } else {
            Some(labels.join(" / "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedKeybind {
    pub trigger: BindingTrigger,
    pub label: String,
}

impl IndexedKeybind {
    pub fn matched_index(&self, key: &TerminalKey) -> Option<usize> {
        let combo = self.trigger.combo();
        let (expected_code, _) = normalize_key_combo(combo);
        let KeyCode::Char(key_number @ '1'..='9') = expected_code else {
            return None;
        };
        let legacy_shifted_number = matches!(key.code, KeyCode::Char(c)
            if shifted_number_symbol(c) == Some(key_number)
                && indexed_shifted_number_matches(key, combo, key_number));
        if terminal_key_matches_combo(key, combo) || legacy_shifted_number {
            Some((key_number as usize) - ('1' as usize))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct CustomCommandKeybind {
    pub bindings: ActionKeybinds,
    pub label: String,
    pub command: String,
    pub action: CustomCommandAction,
    pub description: Option<String>,
    pub width: Option<PopupSize>,
    pub height: Option<PopupSize>,
}

#[derive(Debug, Clone)]
pub struct UserAction {
    pub name: String,
    pub command: String,
    pub bindings: ActionKeybinds,
    pub run_on_worktree_create: bool,
    pub open_in_bottom_pane: bool,
    pub repo: Option<String>,
}

impl UserAction {
    pub fn key_label(&self) -> String {
        self.bindings.label().unwrap_or_else(|| "unset".into())
    }

    pub fn applies_to_repo(&self, repo: Option<&str>) -> bool {
        match self.repo.as_deref() {
            None => true,
            Some(scope) => repo.is_some_and(|repo| {
                crate::work_context::repo_slugs_match(scope, repo)
                    || scope
                        .rsplit_once('/')
                        .is_some_and(|(_, name)| name.eq_ignore_ascii_case(repo))
            }),
        }
    }
}

/// Parsed keybinds for Herdr actions.
#[derive(Debug, Clone)]
pub struct NavigateKeybinds {
    pub workspace_up: ActionKeybinds,
    pub workspace_down: ActionKeybinds,
    pub pane_left: ActionKeybinds,
    pub pane_down: ActionKeybinds,
    pub pane_up: ActionKeybinds,
    pub pane_right: ActionKeybinds,
}

/// Parsed keybinds for Herdr actions.
#[derive(Debug, Clone)]
pub struct Keybinds {
    pub navigate: NavigateKeybinds,
    pub help: ActionKeybinds,
    pub settings: ActionKeybinds,
    pub new_workspace: ActionKeybinds,
    pub new_thread: ActionKeybinds,
    pub new_agent_dock: ActionKeybinds,
    pub new_worktree: ActionKeybinds,
    pub open_worktree: ActionKeybinds,
    pub remove_worktree: ActionKeybinds,
    pub rename_workspace: ActionKeybinds,
    pub close_workspace: ActionKeybinds,
    pub workspace_picker: ActionKeybinds,
    pub goto: ActionKeybinds,
    pub command_palette: ActionKeybinds,
    pub agent_finder: ActionKeybinds,
    pub detach: ActionKeybinds,
    pub reload_config: ActionKeybinds,
    pub open_notification_target: ActionKeybinds,
    pub open_work_link: ActionKeybinds,
    pub copy_work_link: ActionKeybinds,
    pub open_work_url: ActionKeybinds,
    pub copy_work_url: ActionKeybinds,
    pub copy_work_ticket: ActionKeybinds,
    pub copy_work_pr: ActionKeybinds,
    pub copy_work_preview: ActionKeybinds,
    pub toggle_theme: ActionKeybinds,
    pub previous_workspace: ActionKeybinds,
    pub next_workspace: ActionKeybinds,
    pub previous_agent: ActionKeybinds,
    pub next_agent: ActionKeybinds,
    pub next_review_agent: ActionKeybinds,
    pub focus_agent: Vec<IndexedKeybind>,
    pub new_tab: ActionKeybinds,
    pub rename_tab: ActionKeybinds,
    pub toggle_tab_prio: ActionKeybinds,
    pub toggle_prio_panel: ActionKeybinds,
    pub previous_tab: ActionKeybinds,
    pub next_tab: ActionKeybinds,
    pub move_tab_previous: ActionKeybinds,
    pub move_tab_next: ActionKeybinds,
    pub previous_window: ActionKeybinds,
    pub next_window: ActionKeybinds,
    pub previous_any_window: ActionKeybinds,
    pub next_any_window: ActionKeybinds,
    pub next_blocked_window: ActionKeybinds,
    pub switch_tab: Vec<IndexedKeybind>,
    pub switch_workspace: Vec<IndexedKeybind>,
    pub close_tab: ActionKeybinds,
    pub rename_pane: ActionKeybinds,
    pub edit_scrollback: ActionKeybinds,
    pub copy_mode: ActionKeybinds,
    pub focus_pane_left: ActionKeybinds,
    pub focus_pane_down: ActionKeybinds,
    pub focus_pane_up: ActionKeybinds,
    pub focus_pane_right: ActionKeybinds,
    pub swap_pane_left: ActionKeybinds,
    pub swap_pane_down: ActionKeybinds,
    pub swap_pane_up: ActionKeybinds,
    pub swap_pane_right: ActionKeybinds,
    pub cycle_pane_next: ActionKeybinds,
    pub cycle_pane_previous: ActionKeybinds,
    pub last_pane: ActionKeybinds,
    pub split_vertical: ActionKeybinds,
    pub split_horizontal: ActionKeybinds,
    pub split_left: ActionKeybinds,
    pub split_up: ActionKeybinds,
    pub close_pane: ActionKeybinds,
    pub zoom: ActionKeybinds,
    pub toggle_pin_tab: ActionKeybinds,
    pub resize_mode: ActionKeybinds,
    pub resize_pane_left: ActionKeybinds,
    pub resize_pane_down: ActionKeybinds,
    pub resize_pane_up: ActionKeybinds,
    pub resize_pane_right: ActionKeybinds,
    pub toggle_sidebar: ActionKeybinds,
    pub focus_sidebar: ActionKeybinds,
    pub assign_pane_to_pod: ActionKeybinds,
    pub focus_owning_repo_group: ActionKeybinds,
    pub sidebar_cycle_group_mode: ActionKeybinds,
    pub sidebar_refresh: ActionKeybinds,
    pub toggle_blocked_filter: ActionKeybinds,
    pub toggle_dock: ActionKeybinds,
    pub previous_dock_tab: ActionKeybinds,
    pub next_dock_tab: ActionKeybinds,
    pub editor_open_repo: ActionKeybinds,
    pub edit_scratchpad: ActionKeybinds,
    pub show_scratchpad: ActionKeybinds,
    pub toggle_notepad: ActionKeybinds,
    pub toggle_pomodoro: ActionKeybinds,
    pub symphony: ActionKeybinds,
    pub runs: ActionKeybinds,
    pub work: ActionKeybinds,
    pub usage: ActionKeybinds,
    pub tickets: ActionKeybinds,
    pub missive: ActionKeybinds,
    pub inbox: ActionKeybinds,
    pub home: ActionKeybinds,
    pub git_pull: ActionKeybinds,
    pub git_commit: ActionKeybinds,
    pub git_push: ActionKeybinds,
    pub git_create_pr: ActionKeybinds,
    pub dock_home: ActionKeybinds,
    pub dock_terminal: ActionKeybinds,
    pub dock_files: ActionKeybinds,
    pub dock_diff: ActionKeybinds,
    pub dock_pr: ActionKeybinds,
    pub dock_linear: ActionKeybinds,
    pub dock_missive: ActionKeybinds,
    pub dock_agents: ActionKeybinds,
    pub dock_shortcuts: ActionKeybinds,
    pub dock_context: ActionKeybinds,
    pub dock_symphony: ActionKeybinds,
    pub toggle_status_detail: ActionKeybinds,
    pub custom_commands: Vec<CustomCommandKeybind>,
    pub user_actions: Vec<UserAction>,
}

impl Default for Keybinds {
    fn default() -> Self {
        Config::default().keybinds()
    }
}

#[derive(Clone)]
enum ParsedBinding {
    Single(ResolvedBinding),
    Range(Vec<ResolvedBinding>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingSource {
    Default,
    User,
}

struct RegisteredBinding {
    field: String,
    source: BindingSource,
}

struct BindingRegistry {
    prefix_combo: KeyCombo,
    prefix_source: BindingSource,
    direct: std::collections::HashMap<KeyCombo, RegisteredBinding>,
    prefix: std::collections::HashMap<KeyCombo, RegisteredBinding>,
}

impl BindingRegistry {
    fn new(prefix_combo: KeyCombo, prefix_source: BindingSource) -> Self {
        Self {
            prefix_combo: normalize_key_combo(prefix_combo),
            prefix_source,
            direct: std::collections::HashMap::new(),
            prefix: std::collections::HashMap::new(),
        }
    }

    fn reserve_direct(&mut self, combo: KeyCombo, field: &str, source: BindingSource) {
        self.direct
            .entry(normalize_key_combo(combo))
            .or_insert_with(|| RegisteredBinding {
                field: field.to_string(),
                source,
            });
    }

    fn prefix_rhs_is_reserved(&self, combo: KeyCombo) -> bool {
        normalize_key_combo(combo) == self.prefix_combo
    }

    fn conflict(&self, binding: &ResolvedBinding) -> Option<&RegisteredBinding> {
        match binding.trigger {
            BindingTrigger::Direct(combo) => self.direct.get(&normalize_key_combo(combo)),
            BindingTrigger::Prefix(combo) => self.prefix.get(&normalize_key_combo(combo)),
        }
    }

    fn register(&mut self, binding: &ResolvedBinding, field: &str, source: BindingSource) {
        let registered = || RegisteredBinding {
            field: field.to_string(),
            source,
        };
        match binding.trigger {
            BindingTrigger::Direct(combo) => {
                self.direct.insert(normalize_key_combo(combo), registered());
            }
            BindingTrigger::Prefix(combo) => {
                self.prefix.insert(normalize_key_combo(combo), registered());
            }
        }
    }
}

impl Config {
    pub(super) fn validated_keybinds(&self) -> (Option<String>, KeyCombo, Vec<String>, Keybinds) {
        let mut diagnostics = Vec::new();
        let (prefix, prefix_diag) = parse_key_combo_with_diagnostic(
            &self.keys.prefix,
            "keys.prefix",
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        if let Some(diag) = &prefix_diag {
            warn!(message = %diag, "config diagnostic");
        }

        let prefix_source = if self.keys.key_field_is_user_configured("prefix") {
            BindingSource::User
        } else {
            BindingSource::Default
        };
        let mut registry = BindingRegistry::new(prefix, prefix_source);
        registry.reserve_direct(prefix, "keys.prefix", prefix_source);
        let mut navigate_registry = BindingRegistry::new(prefix, prefix_source);
        navigate_registry.reserve_direct(prefix, "keys.prefix", prefix_source);
        reserve_navigate_runtime_keys(&mut navigate_registry);

        macro_rules! empty_action {
            () => {
                ActionKeybinds::default()
            };
        }

        let mut keybinds = Keybinds {
            navigate: NavigateKeybinds {
                workspace_up: empty_action!(),
                workspace_down: empty_action!(),
                pane_left: empty_action!(),
                pane_down: empty_action!(),
                pane_up: empty_action!(),
                pane_right: empty_action!(),
            },
            help: empty_action!(),
            settings: empty_action!(),
            new_workspace: empty_action!(),
            new_thread: empty_action!(),
            new_agent_dock: empty_action!(),
            new_worktree: empty_action!(),
            open_worktree: empty_action!(),
            remove_worktree: empty_action!(),
            rename_workspace: empty_action!(),
            close_workspace: empty_action!(),
            workspace_picker: empty_action!(),
            goto: empty_action!(),
            command_palette: empty_action!(),
            agent_finder: empty_action!(),
            detach: empty_action!(),
            reload_config: empty_action!(),
            open_notification_target: empty_action!(),
            open_work_link: empty_action!(),
            copy_work_link: empty_action!(),
            open_work_url: empty_action!(),
            copy_work_url: empty_action!(),
            copy_work_ticket: empty_action!(),
            copy_work_pr: empty_action!(),
            copy_work_preview: empty_action!(),
            toggle_theme: empty_action!(),
            previous_workspace: empty_action!(),
            next_workspace: empty_action!(),
            previous_agent: empty_action!(),
            next_agent: empty_action!(),
            next_review_agent: empty_action!(),
            focus_agent: Vec::new(),
            new_tab: empty_action!(),
            rename_tab: empty_action!(),
            toggle_tab_prio: empty_action!(),
            toggle_prio_panel: empty_action!(),
            previous_tab: empty_action!(),
            next_tab: empty_action!(),
            move_tab_previous: empty_action!(),
            move_tab_next: empty_action!(),
            previous_window: empty_action!(),
            next_window: empty_action!(),
            previous_any_window: empty_action!(),
            next_any_window: empty_action!(),
            next_blocked_window: empty_action!(),
            switch_tab: Vec::new(),
            switch_workspace: Vec::new(),
            close_tab: empty_action!(),
            rename_pane: empty_action!(),
            edit_scrollback: empty_action!(),
            copy_mode: empty_action!(),
            focus_pane_left: empty_action!(),
            focus_pane_down: empty_action!(),
            focus_pane_up: empty_action!(),
            focus_pane_right: empty_action!(),
            swap_pane_left: empty_action!(),
            swap_pane_down: empty_action!(),
            swap_pane_up: empty_action!(),
            swap_pane_right: empty_action!(),
            cycle_pane_next: empty_action!(),
            cycle_pane_previous: empty_action!(),
            last_pane: empty_action!(),
            split_vertical: empty_action!(),
            split_horizontal: empty_action!(),
            split_left: empty_action!(),
            split_up: empty_action!(),
            close_pane: empty_action!(),
            zoom: empty_action!(),
            toggle_pin_tab: empty_action!(),
            resize_mode: empty_action!(),
            resize_pane_left: empty_action!(),
            resize_pane_down: empty_action!(),
            resize_pane_up: empty_action!(),
            resize_pane_right: empty_action!(),
            toggle_sidebar: empty_action!(),
            focus_sidebar: empty_action!(),
            assign_pane_to_pod: empty_action!(),
            focus_owning_repo_group: empty_action!(),
            sidebar_cycle_group_mode: empty_action!(),
            sidebar_refresh: empty_action!(),
            toggle_blocked_filter: empty_action!(),
            toggle_dock: empty_action!(),
            previous_dock_tab: empty_action!(),
            next_dock_tab: empty_action!(),
            editor_open_repo: empty_action!(),
            edit_scratchpad: empty_action!(),
            show_scratchpad: empty_action!(),
            toggle_notepad: empty_action!(),
            toggle_pomodoro: empty_action!(),
            symphony: empty_action!(),
            runs: empty_action!(),
            work: empty_action!(),
            usage: empty_action!(),
            tickets: empty_action!(),
            missive: empty_action!(),
            inbox: empty_action!(),
            home: empty_action!(),
            git_pull: empty_action!(),
            git_commit: empty_action!(),
            git_push: empty_action!(),
            git_create_pr: empty_action!(),
            dock_home: empty_action!(),
            dock_terminal: empty_action!(),
            dock_files: empty_action!(),
            dock_diff: empty_action!(),
            dock_pr: empty_action!(),
            dock_linear: empty_action!(),
            dock_missive: empty_action!(),
            dock_agents: empty_action!(),
            dock_shortcuts: empty_action!(),
            dock_context: empty_action!(),
            dock_symphony: empty_action!(),
            toggle_status_detail: empty_action!(),
            custom_commands: Vec::new(),
            user_actions: Vec::new(),
        };

        macro_rules! field_source {
            ($field:ident) => {
                if self.keys.key_field_is_user_configured(stringify!($field)) {
                    BindingSource::User
                } else {
                    BindingSource::Default
                }
            };
        }
        macro_rules! apply_action {
            ($target:expr, $field:ident, $source:expr) => {
                if field_source!($field) == $source {
                    $target = parse_action_bindings(
                        concat!("keys.", stringify!($field)),
                        &self.keys.$field,
                        &mut registry,
                        &mut diagnostics,
                        $source,
                    );
                }
            };
        }
        macro_rules! apply_indexed {
            (
                $target:expr,
                $field:ident,
                $legacy_config:expr,
                $source:expr
            ) => {
                if field_source!($field) == $source {
                    if $source == BindingSource::Default && !$legacy_config.trim().is_empty() {
                        // A legacy [keys.indexed] entry is user configuration for
                        // this target and should displace the modern default.
                    } else {
                        $target = parse_indexed_bindings(
                            concat!("keys.", stringify!($field)),
                            &self.keys.$field,
                            &mut registry,
                            &mut diagnostics,
                            $source,
                        );
                    }
                }
            };
        }
        macro_rules! apply_navigate {
            ($target:expr, $field:ident, $source:expr) => {
                if field_source!($field) == $source {
                    $target = parse_navigate_bindings(
                        concat!("keys.", stringify!($field)),
                        &self.keys.$field,
                        &mut navigate_registry,
                        &mut diagnostics,
                        $source,
                    );
                }
            };
        }

        for source in [BindingSource::User, BindingSource::Default] {
            apply_navigate!(
                keybinds.navigate.workspace_up,
                navigate_workspace_up,
                source
            );
            apply_navigate!(
                keybinds.navigate.workspace_down,
                navigate_workspace_down,
                source
            );
            apply_navigate!(keybinds.navigate.pane_left, navigate_pane_left, source);
            apply_navigate!(keybinds.navigate.pane_down, navigate_pane_down, source);
            apply_navigate!(keybinds.navigate.pane_up, navigate_pane_up, source);
            apply_navigate!(keybinds.navigate.pane_right, navigate_pane_right, source);
            apply_action!(keybinds.help, help, source);
            apply_action!(keybinds.settings, settings, source);
            apply_action!(keybinds.new_workspace, new_workspace, source);
            apply_action!(keybinds.new_thread, new_thread, source);
            apply_action!(keybinds.new_agent_dock, new_agent_dock, source);
            apply_action!(keybinds.new_worktree, new_worktree, source);
            apply_action!(keybinds.open_worktree, open_worktree, source);
            apply_action!(keybinds.remove_worktree, remove_worktree, source);
            apply_action!(keybinds.rename_workspace, rename_workspace, source);
            apply_action!(keybinds.close_workspace, close_workspace, source);
            apply_action!(keybinds.workspace_picker, workspace_picker, source);
            apply_action!(keybinds.goto, goto, source);
            apply_action!(keybinds.command_palette, command_palette, source);
            apply_action!(keybinds.agent_finder, agent_finder, source);
            apply_action!(keybinds.detach, detach, source);
            apply_action!(keybinds.reload_config, reload_config, source);
            apply_action!(
                keybinds.open_notification_target,
                open_notification_target,
                source
            );
            apply_action!(keybinds.open_work_link, open_work_link, source);
            apply_action!(keybinds.copy_work_link, copy_work_link, source);
            apply_action!(keybinds.open_work_url, open_work_url, source);
            apply_action!(keybinds.copy_work_url, copy_work_url, source);
            apply_action!(keybinds.copy_work_ticket, copy_work_ticket, source);
            apply_action!(keybinds.copy_work_pr, copy_work_pr, source);
            apply_action!(keybinds.copy_work_preview, copy_work_preview, source);
            apply_action!(keybinds.toggle_theme, toggle_theme, source);
            apply_action!(keybinds.previous_workspace, previous_workspace, source);
            apply_action!(keybinds.next_workspace, next_workspace, source);
            apply_action!(keybinds.previous_agent, previous_agent, source);
            apply_action!(keybinds.next_agent, next_agent, source);
            apply_action!(keybinds.next_review_agent, next_review_agent, source);
            apply_indexed!(
                keybinds.focus_agent,
                focus_agent,
                &self.keys.indexed.agents,
                source
            );
            apply_action!(keybinds.new_tab, new_tab, source);
            apply_action!(keybinds.rename_tab, rename_tab, source);
            apply_action!(keybinds.toggle_tab_prio, toggle_tab_prio, source);
            apply_action!(keybinds.toggle_prio_panel, toggle_prio_panel, source);
            apply_action!(keybinds.previous_tab, previous_tab, source);
            apply_action!(keybinds.next_tab, next_tab, source);
            apply_action!(keybinds.move_tab_previous, move_tab_previous, source);
            apply_action!(keybinds.move_tab_next, move_tab_next, source);
            apply_action!(keybinds.previous_window, previous_window, source);
            apply_action!(keybinds.next_window, next_window, source);
            apply_action!(keybinds.previous_any_window, previous_any_window, source);
            apply_action!(keybinds.next_any_window, next_any_window, source);
            apply_action!(keybinds.next_blocked_window, next_blocked_window, source);
            apply_indexed!(
                keybinds.switch_tab,
                switch_tab,
                &self.keys.indexed.tabs,
                source
            );
            apply_indexed!(
                keybinds.switch_workspace,
                switch_workspace,
                &self.keys.indexed.workspaces,
                source
            );
            apply_action!(keybinds.close_tab, close_tab, source);
            apply_action!(keybinds.rename_pane, rename_pane, source);
            apply_action!(keybinds.edit_scrollback, edit_scrollback, source);
            apply_action!(keybinds.copy_mode, copy_mode, source);
            apply_action!(keybinds.focus_pane_left, focus_pane_left, source);
            apply_action!(keybinds.focus_pane_down, focus_pane_down, source);
            apply_action!(keybinds.focus_pane_up, focus_pane_up, source);
            apply_action!(keybinds.focus_pane_right, focus_pane_right, source);
            apply_action!(keybinds.swap_pane_left, swap_pane_left, source);
            apply_action!(keybinds.swap_pane_down, swap_pane_down, source);
            apply_action!(keybinds.swap_pane_up, swap_pane_up, source);
            apply_action!(keybinds.swap_pane_right, swap_pane_right, source);
            apply_action!(keybinds.last_pane, last_pane, source);
            apply_action!(keybinds.cycle_pane_next, cycle_pane_next, source);
            apply_action!(keybinds.cycle_pane_previous, cycle_pane_previous, source);
            apply_action!(keybinds.split_vertical, split_vertical, source);
            apply_action!(keybinds.split_horizontal, split_horizontal, source);
            apply_action!(keybinds.split_left, split_left, source);
            apply_action!(keybinds.split_up, split_up, source);
            apply_action!(keybinds.close_pane, close_pane, source);
            apply_action!(keybinds.zoom, zoom, source);
            apply_action!(keybinds.toggle_pin_tab, toggle_pin_tab, source);
            apply_action!(keybinds.resize_mode, resize_mode, source);
            apply_action!(keybinds.resize_pane_left, resize_pane_left, source);
            apply_action!(keybinds.resize_pane_down, resize_pane_down, source);
            apply_action!(keybinds.resize_pane_up, resize_pane_up, source);
            apply_action!(keybinds.resize_pane_right, resize_pane_right, source);
            apply_action!(keybinds.toggle_sidebar, toggle_sidebar, source);
            apply_action!(keybinds.focus_sidebar, focus_sidebar, source);
            apply_action!(keybinds.assign_pane_to_pod, assign_pane_to_pod, source);
            apply_action!(
                keybinds.focus_owning_repo_group,
                focus_owning_repo_group,
                source
            );
            apply_action!(
                keybinds.sidebar_cycle_group_mode,
                sidebar_cycle_group_mode,
                source
            );
            apply_action!(keybinds.sidebar_refresh, sidebar_refresh, source);
            apply_action!(
                keybinds.toggle_blocked_filter,
                toggle_blocked_filter,
                source
            );
            apply_action!(keybinds.toggle_dock, toggle_dock, source);
            apply_action!(keybinds.previous_dock_tab, previous_dock_tab, source);
            apply_action!(keybinds.next_dock_tab, next_dock_tab, source);
            apply_action!(keybinds.editor_open_repo, editor_open_repo, source);
            apply_action!(keybinds.edit_scratchpad, edit_scratchpad, source);
            apply_action!(keybinds.show_scratchpad, show_scratchpad, source);
            apply_action!(keybinds.toggle_notepad, toggle_notepad, source);
            apply_action!(keybinds.toggle_pomodoro, toggle_pomodoro, source);
            apply_action!(keybinds.symphony, symphony, source);
            apply_action!(keybinds.runs, runs, source);
            apply_action!(keybinds.work, work, source);
            apply_action!(keybinds.usage, usage, source);
            apply_action!(keybinds.tickets, tickets, source);
            apply_action!(keybinds.missive, missive, source);
            apply_action!(keybinds.inbox, inbox, source);
            apply_action!(keybinds.home, home, source);
            apply_action!(keybinds.git_pull, git_pull, source);
            apply_action!(keybinds.git_commit, git_commit, source);
            apply_action!(keybinds.git_push, git_push, source);
            apply_action!(keybinds.git_create_pr, git_create_pr, source);
            apply_action!(keybinds.dock_home, dock_home, source);
            apply_action!(keybinds.dock_terminal, dock_terminal, source);
            apply_action!(keybinds.dock_files, dock_files, source);
            apply_action!(keybinds.dock_diff, dock_diff, source);
            apply_action!(keybinds.dock_pr, dock_pr, source);
            apply_action!(keybinds.dock_linear, dock_linear, source);
            apply_action!(keybinds.dock_missive, dock_missive, source);
            apply_action!(keybinds.dock_agents, dock_agents, source);
            apply_action!(keybinds.dock_shortcuts, dock_shortcuts, source);
            apply_action!(keybinds.dock_context, dock_context, source);
            apply_action!(keybinds.dock_symphony, dock_symphony, source);
            apply_action!(keybinds.toggle_status_detail, toggle_status_detail, source);

            if source == field_source!(indexed) {
                append_legacy_indexed_bindings(
                    &mut keybinds.switch_tab,
                    "keys.indexed.tabs",
                    &self.keys.indexed.tabs,
                    &mut registry,
                    &mut diagnostics,
                    source,
                );
                append_legacy_indexed_bindings(
                    &mut keybinds.switch_workspace,
                    "keys.indexed.workspaces",
                    &self.keys.indexed.workspaces,
                    &mut registry,
                    &mut diagnostics,
                    source,
                );
                append_legacy_indexed_bindings(
                    &mut keybinds.focus_agent,
                    "keys.indexed.agents",
                    &self.keys.indexed.agents,
                    &mut registry,
                    &mut diagnostics,
                    source,
                );
            }

            if source == BindingSource::User {
                append_custom_command_bindings(
                    self,
                    &mut keybinds,
                    &mut registry,
                    &mut diagnostics,
                );
            }
        }

        // Keep the pre-picker field names usable for existing configurations;
        // canonical defaults and new configurations live under *_work_link.
        if !self.keys.key_field_is_user_configured("open_work_url")
            && !self.keys.key_field_is_user_configured("open_work_link")
        {
            keybinds.open_work_url = keybinds.open_work_link.clone();
        }
        if !self.keys.key_field_is_user_configured("copy_work_url")
            && !self.keys.key_field_is_user_configured("copy_work_link")
        {
            keybinds.copy_work_url = keybinds.copy_work_link.clone();
        }

        // Legacy aliases still dispatch as picker actions. Keep the canonical
        // help entry populated when a legacy user binding displaced its
        // canonical default during collision validation.
        if keybinds.open_work_link.bindings.is_empty()
            && !keybinds.open_work_url.bindings.is_empty()
        {
            keybinds.open_work_link = keybinds.open_work_url.clone();
        }
        if keybinds.copy_work_link.bindings.is_empty()
            && !keybinds.copy_work_url.bindings.is_empty()
        {
            keybinds.copy_work_link = keybinds.copy_work_url.clone();
        }

        append_user_actions(self, prefix, &mut keybinds, &mut registry, &mut diagnostics);

        (prefix_diag, prefix, diagnostics, keybinds)
    }
}

pub fn validate_user_action_key(config: &mut Config, key: &str) -> Result<String, String> {
    let mut suffix = 0usize;
    let name = loop {
        let name = format!("__herdr_action_key_capture_{suffix}");
        if !config
            .actions
            .iter()
            .any(|action| action.name.eq_ignore_ascii_case(&name))
        {
            break name;
        }
        suffix = suffix.saturating_add(1);
    };
    config.actions.push(crate::config::ActionConfig {
        name: name.clone(),
        command: "true".into(),
        key: Some(key.into()),
        ..Default::default()
    });
    let (.., diagnostics, keybinds) = config.validated_keybinds();
    let result = keybinds
        .user_actions
        .iter()
        .find(|action| action.name == name)
        .and_then(|action| action.bindings.label())
        .ok_or_else(|| {
            diagnostics
                .last()
                .cloned()
                .unwrap_or_else(|| "keybinding is already bound".into())
        });
    config.actions.pop();
    result
}

fn append_user_actions(
    config: &Config,
    prefix: KeyCombo,
    keybinds: &mut Keybinds,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
) {
    let mut names = std::collections::HashSet::new();
    for (index, action) in config.actions.iter().enumerate() {
        let name = action.name.trim();
        if name.is_empty() {
            let diagnostic =
                format!("empty user action name: actions[{index}].name; disabling action");
            warn!(message = %diagnostic, "config diagnostic");
            diagnostics.push(diagnostic);
            continue;
        }
        let command = action.command.trim();
        if command.is_empty() {
            let diagnostic =
                format!("empty user action command: actions[{index}].command; disabling action");
            warn!(message = %diagnostic, "config diagnostic");
            diagnostics.push(diagnostic);
            continue;
        }
        if !names.insert(name.to_ascii_lowercase()) {
            let diagnostic = format!(
                "duplicate user action name: actions[{index}].name = {name:?}; disabling action"
            );
            warn!(message = %diagnostic, "config diagnostic");
            diagnostics.push(diagnostic);
            continue;
        }

        let key_field = format!("actions[{index}].key");
        let key = action
            .key
            .as_deref()
            .map(|raw| normalize_user_action_key(raw, prefix))
            .unwrap_or_default();
        let bindings = parse_action_bindings(
            &key_field,
            &BindingConfig::one(key.clone()),
            registry,
            diagnostics,
            BindingSource::User,
        );
        if !key.trim().is_empty() && bindings.bindings.is_empty() {
            let diagnostic = format!("unbound user action key: {key_field} = {key:?}");
            warn!(message = %diagnostic, "config diagnostic");
            diagnostics.push(diagnostic);
        }
        keybinds.user_actions.push(UserAction {
            name: name.to_string(),
            command: command.to_string(),
            bindings,
            run_on_worktree_create: action.run_on_worktree_create,
            open_in_bottom_pane: action.open_in_bottom_pane,
            repo: action
                .repo
                .as_deref()
                .map(str::trim)
                .filter(|repo| !repo.is_empty())
                .map(str::to_string),
        });
    }
}

fn normalize_user_action_key(raw: &str, prefix: KeyCombo) -> String {
    let parts = raw.split_whitespace().collect::<Vec<_>>();
    if let [physical_prefix, rhs] = parts.as_slice() {
        let physical_prefix = physical_prefix.replace('-', "+");
        if parse_key_combo(&physical_prefix).map(normalize_key_combo)
            == Some(normalize_key_combo(prefix))
        {
            return format!("prefix+{rhs}");
        }
    }
    raw.trim().to_string()
}

fn reserve_navigate_runtime_keys(registry: &mut BindingRegistry) {
    for combo in [
        (KeyCode::Esc, KeyModifiers::empty()),
        (KeyCode::Enter, KeyModifiers::empty()),
        (KeyCode::Tab, KeyModifiers::empty()),
        (KeyCode::BackTab, KeyModifiers::empty()),
        (KeyCode::Tab, KeyModifiers::SHIFT),
        (KeyCode::Left, KeyModifiers::empty()),
        (KeyCode::Right, KeyModifiers::empty()),
    ] {
        registry.reserve_direct(combo, "navigate reserved keys", BindingSource::Default);
    }

    for idx in '1'..='9' {
        registry.reserve_direct(
            (KeyCode::Char(idx), KeyModifiers::empty()),
            "navigate reserved keys",
            BindingSource::Default,
        );
    }
}

fn append_custom_command_bindings(
    config: &Config,
    keybinds: &mut Keybinds,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
) {
    for (index, command) in config.keys.command.iter().enumerate() {
        let key_field = format!("keys.command[{index}].key");
        let command_field = format!("keys.command[{index}].command");

        if command.command.trim().is_empty() {
            let diag = format!("empty custom command: {command_field}; disabling custom command");
            warn!(message = %diag, "config diagnostic");
            diagnostics.push(diag);
            continue;
        }

        let bindings = parse_action_bindings(
            &key_field,
            &command.key,
            registry,
            diagnostics,
            BindingSource::User,
        );
        if bindings.bindings.is_empty() {
            continue;
        }

        let action = match command.action_type {
            CommandKeybindType::Shell => CustomCommandAction::Shell,
            CommandKeybindType::Pane => CustomCommandAction::Pane,
            CommandKeybindType::Popup => CustomCommandAction::Popup,
            CommandKeybindType::PluginAction => CustomCommandAction::PluginAction,
        };
        let (width, height) = if action == CustomCommandAction::Popup {
            (command.width, command.height)
        } else {
            if command.width.is_some() || command.height.is_some() {
                let diag = format!(
                    "popup size on non-popup custom command: keys.command[{index}]; ignoring width and height"
                );
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
            (None, None)
        };
        let label = bindings.label().unwrap_or_else(|| "unset".to_string());
        keybinds.custom_commands.push(CustomCommandKeybind {
            bindings,
            label,
            command: command.command.clone(),
            action,
            description: command.description.clone(),
            width,
            height,
        });
    }
}

fn parse_action_bindings(
    field: &str,
    config: &BindingConfig,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
    source: BindingSource,
) -> ActionKeybinds {
    let mut bindings = Vec::new();
    for raw in config.values() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match parse_binding_string(raw) {
            Some(ParsedBinding::Single(binding)) => {
                if reject_binding(field, &binding, registry, diagnostics, source) {
                    continue;
                }
                registry.register(&binding, field, source);
                bindings.push(binding);
            }
            Some(ParsedBinding::Range(_)) => {
                let diag = format!("range keybinding is only valid for indexed actions: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
            None => {
                let diag = format!("invalid keybinding: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
        }
    }
    ActionKeybinds { bindings }
}

fn parse_navigate_bindings(
    field: &'static str,
    config: &BindingConfig,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
    source: BindingSource,
) -> ActionKeybinds {
    let mut bindings = Vec::new();
    for raw in config.values() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match parse_binding_string(raw) {
            Some(ParsedBinding::Single(binding)) => {
                if reject_navigate_binding(field, &binding, registry, diagnostics, source) {
                    continue;
                }
                registry.register(&binding, field, source);
                bindings.push(binding);
            }
            Some(ParsedBinding::Range(_)) => {
                let diag = format!("range keybinding is only valid for indexed actions: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
            None => {
                let diag = format!("invalid keybinding: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
        }
    }
    ActionKeybinds { bindings }
}

fn parse_indexed_bindings(
    field: &'static str,
    config: &BindingConfig,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
    source: BindingSource,
) -> Vec<IndexedKeybind> {
    let mut bindings = Vec::new();
    for raw in config.values() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match parse_binding_string(raw) {
            Some(ParsedBinding::Single(binding)) => {
                push_indexed_binding(field, binding, registry, diagnostics, source, &mut bindings);
            }
            Some(ParsedBinding::Range(range)) => {
                for binding in range {
                    push_indexed_binding(
                        field,
                        binding,
                        registry,
                        diagnostics,
                        source,
                        &mut bindings,
                    );
                }
            }
            None => {
                let diag = format!("invalid keybinding: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
        }
    }
    bindings
}

fn push_indexed_binding(
    field: &str,
    binding: ResolvedBinding,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
    source: BindingSource,
    bindings: &mut Vec<IndexedKeybind>,
) {
    if !matches!(binding.trigger.combo().0, KeyCode::Char('1'..='9')) {
        let diag = format!(
            "indexed keybinding must use 1..9: {field} = {:?}; disabling binding",
            binding.label
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return;
    }
    if reject_binding(field, &binding, registry, diagnostics, source) {
        return;
    }
    registry.register(&binding, field, source);
    bindings.push(IndexedKeybind {
        trigger: binding.trigger,
        label: binding.label,
    });
}

fn append_legacy_indexed_bindings(
    target: &mut Vec<IndexedKeybind>,
    field: &'static str,
    configured_label: &str,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
    source: BindingSource,
) {
    if configured_label.trim().is_empty() {
        return;
    }
    let Some(modifiers) = parse_modifier_combo(configured_label) else {
        let diag = format!(
            "invalid indexed keybinding: {field} = {configured_label:?}; disabling binding"
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return;
    };

    for idx in 1..=9 {
        let combo = (
            KeyCode::Char(char::from_digit(idx, 10).unwrap_or('1')),
            modifiers,
        );
        let binding = ResolvedBinding {
            trigger: BindingTrigger::Direct(combo),
            label: format!("{}+{idx}", configured_label.trim()),
        };
        if reject_binding(field, &binding, registry, diagnostics, source) {
            continue;
        }
        registry.register(&binding, field, source);
        target.push(IndexedKeybind {
            trigger: binding.trigger,
            label: binding.label,
        });
    }
}

fn reject_navigate_binding(
    field: &str,
    binding: &ResolvedBinding,
    registry: &BindingRegistry,
    diagnostics: &mut Vec<String>,
    source: BindingSource,
) -> bool {
    if binding.trigger.is_prefix() {
        let diag = format!(
            "navigate keybinding must not include prefix: {field} = {:?}; disabling binding",
            binding.label
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if matches!(normalize_key_combo(binding.trigger.combo()).0, KeyCode::Esc) {
        let diag = format!(
            "navigate keybinding cannot use esc: {field} = {:?}; disabling binding",
            binding.label
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if let Some(first_binding) = registry.conflict(binding) {
        if source == BindingSource::Default && first_binding.source == BindingSource::User {
            return true;
        }
        let first_field = &first_binding.field;
        let diag = format!("{}: kept {first_field}, disabled {field}", binding.label);
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    false
}

fn reject_binding(
    field: &str,
    binding: &ResolvedBinding,
    registry: &BindingRegistry,
    diagnostics: &mut Vec<String>,
    source: BindingSource,
) -> bool {
    if binding.trigger.is_prefix() && registry.prefix_rhs_is_reserved(binding.trigger.combo()) {
        if source == BindingSource::Default && registry.prefix_source == BindingSource::User {
            return true;
        }
        let diag = format!(
            "reserved keybinding: {field} = {:?} uses keys.prefix as the prefix-mode key; pressing the prefix twice sends a literal prefix key, so this binding is disabled",
            binding.label
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if let Some(first_binding) = registry.conflict(binding) {
        if source == BindingSource::Default && first_binding.source == BindingSource::User {
            return true;
        }
        let first_field = &first_binding.field;
        let diag = format!("{}: kept {first_field}, disabled {field}", binding.label);
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if binding.trigger.is_direct() && is_unmodified_printable(binding.trigger.combo()) {
        let suggestion = format!("prefix+{}", binding.label);
        let diag = format!(
            "unsafe direct keybinding: {field} = {:?} would intercept typing; use {:?} to require the prefix; disabling binding",
            binding.label, suggestion
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    false
}

fn parse_binding_string(raw: &str) -> Option<ParsedBinding> {
    let trimmed = raw.trim();
    let (trigger_prefix, body) = if let Some(rest) = trimmed.strip_prefix("prefix+") {
        (true, rest)
    } else {
        (false, trimmed)
    };

    if let Some(range_modifiers) = parse_range_modifiers(body) {
        let bindings = (1..=9)
            .map(|idx| {
                let combo = (
                    KeyCode::Char(char::from_digit(idx, 10).unwrap_or('1')),
                    range_modifiers,
                );
                let key_label = format_key_combo(combo);
                ResolvedBinding {
                    trigger: if trigger_prefix {
                        BindingTrigger::Prefix(combo)
                    } else {
                        BindingTrigger::Direct(combo)
                    },
                    label: if trigger_prefix {
                        format!("prefix+{key_label}")
                    } else {
                        key_label
                    },
                }
            })
            .collect();
        return Some(ParsedBinding::Range(bindings));
    }

    let combo = parse_key_combo(body)?;
    let label = if trigger_prefix {
        format!("prefix+{}", format_key_combo(combo))
    } else {
        format_key_combo(combo)
    };
    Some(ParsedBinding::Single(ResolvedBinding {
        trigger: if trigger_prefix {
            BindingTrigger::Prefix(combo)
        } else {
            BindingTrigger::Direct(combo)
        },
        label,
    }))
}

pub fn format_key_combo(binding: KeyCombo) -> String {
    let (code, modifiers) = binding;
    let mut parts = Vec::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if modifiers.contains(KeyModifiers::SHIFT) && !matches!(code, KeyCode::BackTab) {
        parts.push("shift".to_string());
    }
    if modifiers.contains(KeyModifiers::SUPER) {
        parts.push(super_modifier_label().to_string());
    }
    if modifiers.contains(KeyModifiers::HYPER) {
        parts.push("hyper".to_string());
    }
    if modifiers.contains(KeyModifiers::META) {
        parts.push("meta".to_string());
    }

    let key = match code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "shift+tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        _ => format!("{:?}", code).to_lowercase(),
    };

    if matches!(code, KeyCode::BackTab) {
        return if parts.is_empty() {
            key
        } else {
            format!("{}+{key}", parts.join("+"))
        };
    }

    parts.push(key);
    parts.join("+")
}

fn super_modifier_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "super"
    }
}

fn parse_modifier_token(token: &str) -> Option<KeyModifiers> {
    match token.to_lowercase().as_str() {
        "ctrl" | "control" => Some(KeyModifiers::CONTROL),
        "shift" => Some(KeyModifiers::SHIFT),
        "alt" | "option" | "meta" => Some(KeyModifiers::ALT),
        "cmd" | "command" | "super" => Some(KeyModifiers::SUPER),
        "hyper" => Some(KeyModifiers::HYPER),
        _ => None,
    }
}

fn parse_range_modifiers(s: &str) -> Option<KeyModifiers> {
    let mut modifiers = KeyModifiers::empty();
    let mut saw_range = false;
    for part in s.split('+') {
        let trimmed = part.trim();
        if trimmed == "1..9" {
            if saw_range {
                return None;
            }
            saw_range = true;
        } else {
            modifiers |= parse_modifier_token(trimmed)?;
        }
    }
    saw_range.then_some(modifiers)
}

fn parse_modifier_combo(s: &str) -> Option<KeyModifiers> {
    let mut modifiers = KeyModifiers::empty();
    let parts: Vec<&str> = s.split('+').collect();
    if parts.is_empty() {
        return None;
    }

    for part in &parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return None;
        }
        modifiers |= parse_modifier_token(trimmed)?;
    }

    if modifiers.is_empty() {
        None
    } else {
        Some(modifiers)
    }
}

pub(crate) fn parse_key_combo(s: &str) -> Option<KeyCombo> {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::empty();
    let mut key_str: Option<&str> = None;

    for part in &parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(modifier) = parse_modifier_token(trimmed) {
            modifiers |= modifier;
        } else if key_str.is_some() {
            return None;
        } else {
            key_str = Some(trimmed);
        }
    }

    let key_str = key_str?;
    let single_char = single_key_char(key_str);
    let lower = key_str.to_lowercase();
    let code = match lower.as_str() {
        "space" | " " => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" if modifiers.contains(KeyModifiers::SHIFT) => {
            modifiers.remove(KeyModifiers::SHIFT);
            KeyCode::BackTab
        }
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "minus" => KeyCode::Char('-'),
        "comma" => KeyCode::Char(','),
        "period" => KeyCode::Char('.'),
        "slash" => KeyCode::Char('/'),
        "backslash" => KeyCode::Char('\\'),
        "quote" => KeyCode::Char('\''),
        "double_quote" | "double-quote" => KeyCode::Char('"'),
        "semicolon" => KeyCode::Char(';'),
        "colon" => KeyCode::Char(':'),
        "percent" => KeyCode::Char('%'),
        "ampersand" => KeyCode::Char('&'),
        "backtick" => KeyCode::Char('`'),
        "plus" => KeyCode::Char('+'),
        _ if single_char.is_some() => {
            let ch = single_char?;
            if ch.is_ascii_uppercase() {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Char(ch.to_ascii_lowercase())
            } else {
                KeyCode::Char(ch)
            }
        }
        s if s.starts_with('f') => s[1..].parse::<u8>().ok().map(KeyCode::F)?,
        _ => return None,
    };

    Some(normalize_key_combo((code, modifiers)))
}

fn single_key_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let ch = chars.next()?;
    if chars.next().is_none() {
        Some(ch)
    } else {
        None
    }
}

fn parse_key_combo_with_diagnostic(
    s: &str,
    field: &str,
    fallback: KeyCombo,
) -> (KeyCombo, Option<String>) {
    match parse_key_combo(s) {
        Some(binding) => (binding, None),
        None => {
            let diag = format!("invalid keybinding: {field} = {s:?}; using fallback");
            warn!(message = %diag, "config diagnostic");
            (fallback, Some(diag))
        }
    }
}

pub fn normalize_key_combo((mut code, mut modifiers): KeyCombo) -> KeyCombo {
    if matches!(code, KeyCode::Tab) && modifiers.contains(KeyModifiers::SHIFT) {
        code = KeyCode::BackTab;
        modifiers.remove(KeyModifiers::SHIFT);
    } else if matches!(code, KeyCode::BackTab) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    (code, modifiers)
}

#[cfg(test)]
pub fn key_event_matches_combo(key: &KeyEvent, combo: KeyCombo) -> bool {
    key_parts_match_combo(key.code, key.modifiers, None, combo)
}

pub fn terminal_key_matches_combo(key: &TerminalKey, combo: KeyCombo) -> bool {
    key_parts_match_combo(key.code, key.modifiers, key.shifted_codepoint, combo)
}

fn key_parts_match_combo(
    actual_code: KeyCode,
    actual_modifiers: KeyModifiers,
    shifted_codepoint: Option<u32>,
    combo: KeyCombo,
) -> bool {
    let (actual_code, actual_modifiers) = normalize_key_combo((actual_code, actual_modifiers));
    let (expected_code, expected_modifiers) = normalize_key_combo(combo);

    if actual_modifiers == expected_modifiers
        && key_codes_match(
            actual_code,
            actual_modifiers,
            expected_code,
            expected_modifiers,
            shifted_codepoint,
        )
    {
        return true;
    }

    let actual_without_shift = actual_modifiers.difference(KeyModifiers::SHIFT);
    actual_modifiers.contains(KeyModifiers::SHIFT)
        && actual_without_shift == expected_modifiers
        && shifted_char_matches_expected(actual_code, shifted_codepoint, expected_code)
        || legacy_shifted_ascii_letter_matches(
            actual_code,
            actual_modifiers,
            expected_code,
            expected_modifiers,
        )
}

fn key_codes_match(
    actual: KeyCode,
    actual_modifiers: KeyModifiers,
    expected: KeyCode,
    expected_modifiers: KeyModifiers,
    shifted_codepoint: Option<u32>,
) -> bool {
    match (actual, expected) {
        (KeyCode::Char(actual), KeyCode::Char(expected))
            if actual.is_ascii_alphabetic() && expected.is_ascii_alphabetic() =>
        {
            actual == expected
                || actual_modifiers.contains(KeyModifiers::SHIFT)
                    && expected_modifiers.contains(KeyModifiers::SHIFT)
                    && actual.eq_ignore_ascii_case(&expected)
        }
        (KeyCode::Char(actual), KeyCode::Char(expected)) => {
            actual == expected
                || shifted_char_matches_expected(
                    KeyCode::Char(actual),
                    shifted_codepoint,
                    KeyCode::Char(expected),
                )
        }
        (actual, expected) => actual == expected,
    }
}

fn legacy_shifted_ascii_letter_matches(
    actual_code: KeyCode,
    actual_modifiers: KeyModifiers,
    expected_code: KeyCode,
    expected_modifiers: KeyModifiers,
) -> bool {
    if actual_modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    let (KeyCode::Char(actual), KeyCode::Char(expected)) = (actual_code, expected_code) else {
        return false;
    };
    actual.is_ascii_uppercase()
        && expected.is_ascii_lowercase()
        && actual.to_ascii_lowercase() == expected
        && actual_modifiers | KeyModifiers::SHIFT == expected_modifiers
}

const SHIFTED_NUMBER_SYMBOLS: [(char, char); 9] = [
    ('1', '!'),
    ('2', '@'),
    ('3', '#'),
    ('4', '$'),
    ('5', '%'),
    ('6', '^'),
    ('7', '&'),
    ('8', '*'),
    ('9', '('),
];

fn shifted_number_symbol(ch: char) -> Option<char> {
    SHIFTED_NUMBER_SYMBOLS
        .iter()
        .find_map(|(number, symbol)| (*symbol == ch).then_some(*number))
}

fn indexed_shifted_number_matches(key: &TerminalKey, combo: KeyCombo, number: char) -> bool {
    let (expected_code, expected_modifiers) = normalize_key_combo(combo);
    matches!(expected_code, KeyCode::Char(expected) if expected == number)
        && expected_modifiers.contains(KeyModifiers::SHIFT)
        && key.modifiers == expected_modifiers.difference(KeyModifiers::SHIFT)
}

fn shifted_char_matches_expected(
    actual_code: KeyCode,
    shifted_codepoint: Option<u32>,
    expected_code: KeyCode,
) -> bool {
    let KeyCode::Char(expected) = expected_code else {
        return false;
    };
    if let Some(shifted) = shifted_codepoint.and_then(char::from_u32) {
        return shifted == expected;
    }
    matches!(actual_code, KeyCode::Char(actual) if actual == expected && is_shifted_punctuation(expected))
}

fn is_shifted_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '!' | '@'
            | '#'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '('
            | ')'
            | '_'
            | '+'
            | '{'
            | '}'
            | '|'
            | ':'
            | '"'
            | '<'
            | '>'
            | '?'
            | '~'
    )
}

fn is_unmodified_printable(combo: KeyCombo) -> bool {
    matches!(combo.0, KeyCode::Char(ch) if !ch.is_control())
        && combo.1.difference(KeyModifiers::SHIFT).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, input::TerminalKey};

    fn binding_triggers(bindings: &ActionKeybinds) -> Vec<BindingTrigger> {
        bindings
            .bindings
            .iter()
            .map(|binding| binding.trigger)
            .collect()
    }

    #[test]
    fn parse_simple_char_combo() {
        assert_eq!(
            parse_key_combo("v"),
            Some((KeyCode::Char('v'), KeyModifiers::empty()))
        );
    }

    #[test]
    fn parse_unicode_char_combo() {
        assert_eq!(
            parse_key_combo("ö"),
            Some((KeyCode::Char('ö'), KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("alt+é"),
            Some((KeyCode::Char('é'), KeyModifiers::ALT))
        );
    }

    #[test]
    fn unicode_prefix_config_is_valid() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ö"
"#,
        )
        .unwrap();
        assert_eq!(
            config.prefix_key(),
            (KeyCode::Char('ö'), KeyModifiers::empty())
        );
        assert!(config.collect_diagnostics().is_empty());
    }

    #[test]
    fn parse_shift_tab_as_backtab() {
        assert_eq!(
            parse_key_combo("shift+tab"),
            Some((KeyCode::BackTab, KeyModifiers::empty()))
        );
    }

    #[test]
    fn parse_named_punctuation() {
        assert_eq!(
            parse_key_combo("minus"),
            Some((KeyCode::Char('-'), KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("comma"),
            Some((KeyCode::Char(','), KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("ampersand"),
            Some((KeyCode::Char('&'), KeyModifiers::empty()))
        );
    }

    #[test]
    fn prefix_binding_is_not_direct_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
next_tab = "prefix+n"
"#,
        )
        .unwrap();
        let kb = config.keybinds();
        assert_eq!(
            binding_triggers(&kb.next_tab),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('n'),
                KeyModifiers::empty()
            ))]
        );
    }

    #[test]
    fn new_worktree_defaults_to_prefix_shift_g() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.new_worktree),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('g'),
                KeyModifiers::SHIFT
            ))]
        );
    }

    #[test]
    fn new_thread_defaults_to_prefix_c_and_new_tab_moves_to_prefix_alt_c() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.new_thread),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('c'),
                KeyModifiers::empty()
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.new_tab),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('c'),
                KeyModifiers::ALT
            ))]
        );
        // The Missive view gave up its shortcut rather than share the letter.
        assert!(binding_triggers(&kb.missive).is_empty());
    }

    #[test]
    fn goto_defaults_to_prefix_g() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.goto),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('g'),
                KeyModifiers::empty()
            ))]
        );
    }

    #[test]
    fn open_and_remove_worktree_keybinds_are_unset_by_default() {
        let kb = Config::default().keybinds();
        assert!(kb.open_worktree.bindings.is_empty());
        assert!(kb.remove_worktree.bindings.is_empty());
    }

    #[test]
    fn copy_mode_uses_tmux_prefix_bracket_by_default() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.copy_mode),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('['),
                KeyModifiers::empty()
            ))]
        );
    }

    #[test]
    fn ac4_work_link_defaults_and_collision_validation_are_registered() {
        let defaults = Config::default().keybinds();
        assert!(defaults
            .open_work_link
            .matches_prefix_key(&crate::input::TerminalKey::new(
                KeyCode::Char('u'),
                KeyModifiers::empty(),
            )));
        assert!(defaults
            .copy_work_link
            .matches_prefix_key(&crate::input::TerminalKey::new(
                KeyCode::Char('U'),
                KeyModifiers::SHIFT,
            )));
        assert!(defaults
            .open_work_url
            .matches_prefix_key(&crate::input::TerminalKey::new(
                KeyCode::Char('u'),
                KeyModifiers::empty(),
            )));
        assert!(defaults
            .copy_work_url
            .matches_prefix_key(&crate::input::TerminalKey::new(
                KeyCode::Char('U'),
                KeyModifiers::SHIFT,
            )));
        assert!(defaults
            .copy_work_ticket
            .matches_prefix_key(&crate::input::TerminalKey::new(
                KeyCode::Char('u'),
                KeyModifiers::CONTROL,
            )));
        assert!(defaults
            .copy_work_pr
            .matches_prefix_key(&crate::input::TerminalKey::new(
                KeyCode::Char('u'),
                KeyModifiers::ALT,
            )));
        assert!(defaults
            .copy_work_preview
            .matches_prefix_key(&crate::input::TerminalKey::new(
                KeyCode::Char('u'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )));

        let config: Config = toml::from_str(
            r#"
            [keys]
            open_work_url = "prefix+u"
            copy_work_url = "prefix+u"
            copy_work_ticket = "prefix+u"
            "#,
        )
        .unwrap();
        let loaded = config.validated_keybinds();
        assert!(!loaded.3.open_work_url.bindings.is_empty());
        assert!(loaded.3.copy_work_url.bindings.is_empty());
        assert!(loaded
            .2
            .iter()
            .any(|diagnostic| diagnostic.contains("copy_work_url")
                && diagnostic.contains("disabled")));
        assert!(loaded
            .2
            .iter()
            .any(|diagnostic| diagnostic.contains("copy_work_ticket")
                && diagnostic.contains("disabled")));
    }

    #[test]
    fn ac26_legacy_work_url_binding_remains_visible_as_effective_picker_binding() {
        let config: Config = toml::from_str(
            r#"
            [keys]
            open_work_url = "prefix+u"
            "#,
        )
        .unwrap();

        let keybinds = config.keybinds();
        assert_eq!(keybinds.open_work_url.labels(), vec!["prefix+u"]);
        assert_eq!(keybinds.open_work_link.labels(), vec!["prefix+u"]);
    }

    #[test]
    fn back_and_forth_keybinds_are_unset_by_default() {
        let kb = Config::default().keybinds();
        assert!(kb.last_pane.bindings.is_empty());
    }

    #[test]
    fn repository_editor_keybinding_is_configurable_and_unset_by_default() {
        assert!(Config::default()
            .keybinds()
            .editor_open_repo
            .bindings
            .is_empty());

        let config: Config = toml::from_str(
            r#"
[keys]
editor_open_repo = "ctrl+alt+v"
"#,
        )
        .expect("repository editor keybinding config");

        assert!(config
            .keybinds()
            .editor_open_repo
            .matches_direct_key(&TerminalKey::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            )));
    }

    #[test]
    fn array_bindings_allow_prefix_and_modified_direct() {
        let config: Config = toml::from_str(
            r#"
[keys]
next_tab = ["prefix+n", "ctrl+alt+]"]
"#,
        )
        .unwrap();
        let kb = config.keybinds();
        assert_eq!(
            binding_triggers(&kb.next_tab),
            vec![
                BindingTrigger::Prefix((KeyCode::Char('n'), KeyModifiers::empty())),
                BindingTrigger::Direct((
                    KeyCode::Char(']'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                )),
            ]
        );
        assert_eq!(kb.next_tab.prefix_rhs_label().as_deref(), Some("n"));
    }

    #[test]
    fn unsafe_direct_printable_binding_is_disabled_with_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[keys]
new_tab = "c"
close_tab = "X"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        let keybinds = config.keybinds();
        assert!(keybinds.new_tab.bindings.is_empty());
        assert!(keybinds.close_tab.bindings.is_empty());
        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.contains("unsafe direct keybinding")
                    && diag.contains("keys.new_tab"))
        );
        assert!(diagnostics.iter().any(
            |diag| diag.contains("unsafe direct keybinding") && diag.contains("keys.close_tab")
        ));
    }

    #[test]
    fn unicode_prefix_bindings_match_non_us_keys() {
        for ch in ['ğ', 'ç', 'ş', 'ı', 'é', 'ø'] {
            let bindings = ActionKeybinds::prefix(&ch.to_string());
            assert!(bindings
                .matches_prefix_key(&TerminalKey::new(KeyCode::Char(ch), KeyModifiers::empty(),)));
        }
    }

    #[test]
    fn shifted_unicode_prefix_bindings_match_layout_aware_input() {
        for (base, shifted) in [('ğ', 'Ğ'), ('ç', 'Ç'), ('ş', 'Ş'), ('ı', 'I'), ('ø', 'Ø')]
        {
            let bindings = ActionKeybinds::prefix(&format!("shift+{base}"));
            assert!(bindings.matches_prefix_key(
                &TerminalKey::new(KeyCode::Char(base), KeyModifiers::SHIFT)
                    .with_shifted_codepoint(shifted as u32)
            ));
        }
    }

    #[test]
    fn shifted_letter_binding_matches_uppercase_key_event() {
        let bindings = ActionKeybinds::prefix("shift+n");
        assert!(bindings.matches_prefix(&KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT)));
    }

    #[test]
    fn shifted_letter_binding_matches_legacy_uppercase_key_event() {
        let bindings = ActionKeybinds::prefix("shift+n");
        assert!(bindings
            .matches_prefix_key(&TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty(),)));
    }

    #[test]
    fn shifted_letter_direct_binding_matches_legacy_uppercase_key_event() {
        let bindings = ActionKeybinds::direct("shift+n");
        assert!(bindings
            .matches_direct_key(&TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty(),)));
    }

    #[test]
    fn shifted_letter_binding_matches_modern_modified_key_event() {
        let bindings = ActionKeybinds::direct("cmd+shift+j");
        assert!(bindings.matches_direct_key(&TerminalKey::new(
            KeyCode::Char('J'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        )));
    }

    #[test]
    fn legacy_uppercase_key_event_does_not_match_unshifted_letter_binding() {
        let bindings = ActionKeybinds::prefix("n");
        assert!(!bindings
            .matches_prefix_key(&TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty(),)));
    }

    #[test]
    fn legacy_uppercase_shift_fallback_is_limited_to_ascii_letters() {
        let shifted_number = ActionKeybinds::prefix("shift+1");
        assert!(!shifted_number
            .matches_prefix_key(&TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty(),)));

        let shifted_non_ascii = ActionKeybinds::prefix("shift+ö");
        assert!(!shifted_non_ascii
            .matches_prefix_key(&TerminalKey::new(KeyCode::Char('Ö'), KeyModifiers::empty(),)));
    }

    #[test]
    fn shifted_tab_inputs_match_backtab_canonical_binding() {
        let bindings = ActionKeybinds::prefix("shift+tab");
        assert!(
            bindings.matches_prefix_key(&TerminalKey::new(KeyCode::BackTab, KeyModifiers::empty()))
        );
        assert!(
            bindings.matches_prefix_key(&TerminalKey::new(KeyCode::BackTab, KeyModifiers::SHIFT))
        );
        assert!(bindings.matches_prefix_key(&TerminalKey::new(KeyCode::Tab, KeyModifiers::SHIFT)));
        assert!(!ActionKeybinds::prefix("tab")
            .matches_prefix_key(&TerminalKey::new(KeyCode::Tab, KeyModifiers::SHIFT)));
        assert_eq!(
            normalize_key_combo((KeyCode::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            (KeyCode::BackTab, KeyModifiers::CONTROL)
        );
    }

    #[test]
    fn format_modified_backtab_keeps_shift_label() {
        assert_eq!(
            format_key_combo((KeyCode::BackTab, KeyModifiers::CONTROL)),
            "ctrl+shift+tab"
        );
        assert_eq!(
            format_key_combo((KeyCode::BackTab, KeyModifiers::CONTROL | KeyModifiers::ALT)),
            "ctrl+alt+shift+tab"
        );
    }

    #[test]
    fn shifted_punctuation_matches_enhanced_input() {
        let help = ActionKeybinds::prefix("?");
        assert!(help.matches_prefix_key(&TerminalKey::new(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        assert!(help.matches_prefix_key(
            &TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32)
        ));

        let bang = ActionKeybinds::prefix("!");
        assert!(bang.matches_prefix_key(
            &TerminalKey::new(KeyCode::Char('1'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('!' as u32)
        ));
    }

    #[test]
    fn prefix_rhs_equal_to_configured_prefix_is_rejected() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
help = "prefix+ctrl+a"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        assert!(config.keybinds().help.bindings.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("reserved keybinding")
                && diag.contains("keys.help")
                && diag.contains("keys.prefix")
        }));

        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
help = "prefix+ctrl+b"
"#,
        )
        .unwrap();
        assert!(!config.keybinds().help.bindings.is_empty());
    }

    #[test]
    fn navigate_bindings_allow_plain_keys_and_reject_local_conflicts() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_up = "j"
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds
            .navigate
            .workspace_up
            .matches_direct_key(&TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty())));
        assert!(keybinds.navigate.workspace_down.bindings.is_empty());
        assert!(keybinds
            .navigate
            .pane_down
            .matches_direct_key(&TerminalKey::new(KeyCode::Char('j'), KeyModifiers::CONTROL)));
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.navigate_workspace_up")
                && diag.contains("disabled keys.navigate_workspace_down")
        }));
    }

    #[test]
    fn navigate_bindings_reject_runtime_reserved_keys() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_up = ["esc", "alt+esc", "enter", "1", "tab", "shift+tab", "left", "right"]
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds.navigate.workspace_up.bindings.is_empty());
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diag| {
                    (diag.contains("navigate reserved keys")
                        || diag.contains("navigate keybinding cannot use esc"))
                        && diag.contains("keys.navigate_workspace_up")
                })
                .count(),
            8
        );
    }

    #[test]
    fn navigate_bindings_can_reuse_navigate_mode_prefix_rhs_keys() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = ["n", "f"]

[[keys.command]]
key = "prefix+f"
command = "echo hi"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds
            .navigate
            .workspace_down
            .matches_direct_key(&TerminalKey::new(KeyCode::Char('n'), KeyModifiers::empty())));
        assert!(keybinds
            .navigate
            .workspace_down
            .matches_direct_key(&TerminalKey::new(KeyCode::Char('f'), KeyModifiers::empty())));
        assert!(!keybinds.custom_commands.is_empty());
        assert!(!diagnostics.iter().any(|diag| {
            diag.contains("disabled keys.navigate_workspace_down")
                && (diag.contains("keys.next_tab") || diag.contains("keys.command"))
        }));
    }

    #[test]
    fn navigate_bindings_do_not_conflict_with_general_focus_pane_bindings() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_down = "j"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();

        assert!(keybinds
            .navigate
            .pane_down
            .matches_direct_key(&TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty())));
    }

    #[test]
    fn navigate_bindings_reject_prefix_syntax_and_prefix_key() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
navigate_workspace_up = "prefix+j"
navigate_workspace_down = "ctrl+a"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds.navigate.workspace_up.bindings.is_empty());
        assert!(keybinds.navigate.workspace_down.bindings.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("navigate keybinding must not include prefix")
                && diag.contains("keys.navigate_workspace_up")
        }));
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.prefix") && diag.contains("keys.navigate_workspace_down")
        }));
    }

    #[test]
    fn custom_command_prefix_rhs_equal_to_configured_prefix_is_rejected() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+b"

[[keys.command]]
key = "prefix+ctrl+b"
command = "echo no"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        assert!(config.keybinds().custom_commands.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("reserved keybinding") && diag.contains("keys.command[0].key")
        }));
    }

    #[test]
    fn direct_custom_printable_binding_is_rejected_as_unsafe() {
        let config: Config = toml::from_str(
            r#"
[keys]

[[keys.command]]
key = "g"
command = "echo no"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        assert!(config.keybinds().custom_commands.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("unsafe direct keybinding") && diag.contains("keys.command[0].key")
        }));
    }

    #[test]
    fn direct_custom_binding_conflicting_with_builtin_is_disabled() {
        let config: Config = toml::from_str(
            r#"
[keys]
new_tab = "ctrl+alt+g"

[[keys.command]]
key = "ctrl+alt+g"
command = "echo no"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        let keybinds = config.keybinds();
        assert!(!keybinds.new_tab.bindings.is_empty());
        assert!(keybinds.custom_commands.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.new_tab") && diag.contains("disabled keys.command[0].key")
        }));
    }

    #[test]
    fn prefixed_indexed_bindings_support_modifiers() {
        let config: Config = toml::from_str(
            r#"
[keys]
switch_workspace = "prefix+shift+1..9"
"#,
        )
        .unwrap();
        let kb = config.keybinds();
        assert_eq!(kb.switch_workspace.len(), 9);
        assert_eq!(
            kb.switch_workspace[0].trigger,
            BindingTrigger::Prefix((KeyCode::Char('1'), KeyModifiers::SHIFT))
        );
        assert_eq!(kb.switch_workspace[0].label, "prefix+shift+1");
    }

    #[test]
    fn legacy_indexed_user_bindings_displace_modern_defaults() {
        let config: Config = toml::from_str(
            r#"
[keys.indexed]
workspaces = "ctrl"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(kb.switch_workspace.len(), 9);
        assert_eq!(
            kb.switch_workspace[0].trigger,
            BindingTrigger::Direct((KeyCode::Char('1'), KeyModifiers::CONTROL))
        );
        assert_eq!(kb.switch_workspace[0].label, "ctrl+1");
    }

    #[test]
    fn invalid_legacy_indexed_user_binding_displaces_modern_default() {
        let config: Config = toml::from_str(
            r#"
[keys.indexed]
tabs = "bogus"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert!(kb.switch_tab.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("invalid indexed keybinding") && diag.contains("keys.indexed.tabs")
        }));
    }

    #[test]
    fn invalid_indexed_binding_does_not_displace_default_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
switch_tab = "prefix+?"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert!(kb.switch_tab.is_empty());
        assert_eq!(
            binding_triggers(&kb.help),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('?'),
                KeyModifiers::empty()
            ))]
        );
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("indexed keybinding must use 1..9") && diag.contains("keys.switch_tab")
        }));
        assert!(!diagnostics.iter().any(|diag| {
            diag.contains("kept keys.switch_tab") && diag.contains("disabled keys.help")
        }));
    }

    #[test]
    fn global_window_default_bindings() {
        let config = Config::default();
        assert!(config.collect_diagnostics().is_empty());
        let kb = config.keybinds();
        assert_eq!(
            binding_triggers(&kb.next_window),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('n'),
                KeyModifiers::empty()
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.previous_window),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('p'),
                KeyModifiers::empty()
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.next_any_window),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('n'),
                KeyModifiers::SHIFT
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.previous_any_window),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('p'),
                KeyModifiers::SHIFT
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.new_workspace),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('n'),
                KeyModifiers::ALT
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.rename_pane),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('p'),
                KeyModifiers::ALT
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.next_tab),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('n'),
                KeyModifiers::CONTROL
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.previous_tab),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('p'),
                KeyModifiers::CONTROL
            ))]
        );
        assert_eq!(kb.switch_tab.len(), 9);
        assert_eq!(
            binding_triggers(&kb.toggle_tab_prio),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('f'),
                KeyModifiers::SHIFT
            ))]
        );
        assert!(kb
            .switch_tab
            .iter()
            .all(|binding| binding.trigger.is_prefix()));
        assert!(kb
            .new_tab
            .bindings
            .iter()
            .all(|binding| binding.trigger.is_prefix()));
        assert_eq!(
            binding_triggers(&kb.swap_pane_left),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('h'),
                KeyModifiers::SHIFT
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.swap_pane_down),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('j'),
                KeyModifiers::SHIFT
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.swap_pane_up),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('k'),
                KeyModifiers::SHIFT
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.swap_pane_right),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('l'),
                KeyModifiers::SHIFT
            ))]
        );
    }

    #[test]
    fn next_blocked_window_default_binding() {
        let config = Config::default();
        assert!(config.collect_diagnostics().is_empty());
        let kb = config.keybinds();
        assert_eq!(
            binding_triggers(&kb.next_blocked_window),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('b'),
                KeyModifiers::empty()
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.toggle_sidebar),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('b'),
                KeyModifiers::SHIFT
            ))]
        );
    }

    #[test]
    fn agent_finder_uses_prefix_f_and_blocked_filter_is_unbound_by_default() {
        let defaults = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&defaults.agent_finder),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('f'),
                KeyModifiers::empty()
            ))]
        );
        assert!(binding_triggers(&defaults.toggle_blocked_filter).is_empty());

        let config: Config = toml::from_str(
            r#"
[keys]
toggle_blocked_filter = "prefix+shift+f"
"#,
        )
        .unwrap();
        let kb = config.keybinds();

        assert_eq!(
            binding_triggers(&kb.toggle_blocked_filter),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('f'),
                KeyModifiers::SHIFT
            ))]
        );
    }

    #[test]
    fn duplicate_prefix_binding_disables_later_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
next_tab = "prefix+n"
new_workspace = "prefix+n"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();
        assert!(kb.next_tab.bindings.is_empty() || kb.new_workspace.bindings.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.new_workspace") && diag.contains("disabled keys.next_tab")
        }));
    }

    #[test]
    fn user_binding_silently_displaces_default_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
previous_workspace = "prefix+shift+l"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            binding_triggers(&kb.previous_workspace),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('l'),
                KeyModifiers::SHIFT
            ))]
        );
        assert!(kb.swap_pane_right.bindings.is_empty());
    }

    #[test]
    fn user_prefix_silently_displaces_default_prefix_rhs_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "n"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(kb.next_window.bindings.is_empty());
    }

    #[test]
    fn duplicate_user_binding_still_reports_conflict() {
        let config: Config = toml::from_str(
            r#"
[keys]
previous_workspace = "prefix+shift+l"
swap_pane_right = "prefix+shift+l"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert_eq!(
            binding_triggers(&kb.previous_workspace),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('l'),
                KeyModifiers::SHIFT
            ))]
        );
        assert!(kb.swap_pane_right.bindings.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.previous_workspace")
                && diag.contains("disabled keys.swap_pane_right")
        }));
    }

    #[test]
    fn custom_command_with_description_parses() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+y"
command = "echo hello"
description = "say hello"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        assert_eq!(keybinds.custom_commands.len(), 1);
        assert_eq!(
            keybinds.custom_commands[0].description,
            Some("say hello".to_string())
        );
    }

    #[test]
    fn custom_popup_command_parses() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+g"
command = "lazygit"
type = "popup"
width = 90
height = "80%"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        assert_eq!(keybinds.custom_commands.len(), 1);
        assert_eq!(
            keybinds.custom_commands[0].action,
            CustomCommandAction::Popup
        );
        assert_eq!(
            keybinds.custom_commands[0].width,
            Some(PopupSize::Cells(90))
        );
        assert_eq!(
            keybinds.custom_commands[0].height,
            Some(PopupSize::Percent(80))
        );
    }

    #[test]
    fn non_popup_custom_command_ignores_popup_size_with_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+g"
command = "lazygit"
type = "pane"
width = "80%"
"#,
        )
        .unwrap();

        let keybinds = config.keybinds();
        assert_eq!(keybinds.custom_commands[0].width, None);
        assert!(config
            .collect_diagnostics()
            .iter()
            .any(|diag| diag.contains("popup size on non-popup custom command")));
    }

    #[test]
    fn user_actions_parse_in_order_with_physical_prefix_syntax() {
        let config: Config = toml::from_str(
            r#"
[[actions]]
name = "test"
command = "just test"
key = "ctrl-b t"
run_on_worktree_create = true
open_in_bottom_pane = true
repo = "matthias-scale/herdr"

[[actions]]
name = "lint"
command = "just lint"
"#,
        )
        .expect("action config parses");

        let actions = config.keybinds().user_actions;
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].name, "test");
        assert_eq!(actions[0].key_label(), "prefix+t");
        assert!(actions[0].run_on_worktree_create);
        assert!(actions[0].open_in_bottom_pane);
        assert!(actions[0].applies_to_repo(Some("matthias-scale/herdr")));
        assert_eq!(actions[1].name, "lint");
        assert_eq!(actions[1].key_label(), "unset");
    }

    #[test]
    fn user_action_diagnostics_keep_valid_buttons_but_unbind_bad_keys() {
        let config: Config = toml::from_str(
            r#"
[[actions]]
name = "help collision"
command = "echo collision"
key = "ctrl-b ?"

[[actions]]
name = "help collision"
command = "echo duplicate"

[[actions]]
name = "empty"
command = "  "
"#,
        )
        .expect("action config parses");

        let diagnostics = config.collect_diagnostics();
        let actions = config.keybinds().user_actions;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "help collision");
        assert!(actions[0].bindings.bindings.is_empty());
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("disabled actions[0].key") && diagnostic.contains("keys.help")
        }));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unbound user action key")));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("duplicate user action name")));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("empty user action command")));
    }
}
