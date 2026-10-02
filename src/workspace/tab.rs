use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::layout::Direction;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};

use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::{PaneLaunchEnv, PaneState};
use crate::render_signal::RenderSignal;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalRuntimeRegistry, TerminalState};

pub(crate) type DetachedPane = (PaneId, TerminalId);

pub(crate) struct MovedPane {
    pub pane_id: PaneId,
    pub pane_state: PaneState,
}

pub struct NewPane {
    pub pane_id: PaneId,
    pub terminal: TerminalState,
    pub runtime: TerminalRuntime,
}

pub(crate) struct AgentTitleContext<'a> {
    pub terminal_title: Option<&'a str>,
    pub work_title: Option<&'a str>,
    pub cwd: Option<&'a Path>,
    pub home: Option<&'a Path>,
    pub agent_name: Option<&'a str>,
    pub agent_label: Option<&'a str>,
    pub display_agent: Option<&'a str>,
    pub detected_agent: Option<crate::detect::Agent>,
}

fn is_agent_identity_segment(context: &AgentTitleContext<'_>, segment: &str) -> bool {
    let same_text = |candidate: &str| segment.trim().eq_ignore_ascii_case(candidate.trim());
    context.agent_name.is_some_and(same_text)
        || context.agent_label.is_some_and(same_text)
        || context.display_agent.is_some_and(same_text)
        || context.detected_agent.is_some_and(|agent| {
            crate::detect::agent_product_titles(agent)
                .iter()
                .any(|product| same_text(product))
        })
}

fn is_novel_title_segment(context: &AgentTitleContext<'_>, title: &str) -> bool {
    let title = title.trim();
    let same_text = |candidate: &str| title.eq_ignore_ascii_case(candidate.trim());

    if is_agent_identity_segment(context, title) {
        return false;
    }

    let Some(cwd) = context.cwd else {
        return true;
    };
    let cwd_text = cwd.to_string_lossy();
    if cwd
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(same_text)
        || same_text(&cwd_text)
    {
        return false;
    }

    if let Some(home) = context.home {
        let home = home.to_string_lossy();
        let cwd_lower = cwd_text.to_ascii_lowercase();
        let home_lower = home.to_ascii_lowercase();
        if let Some(relative) = cwd_lower.strip_prefix(&home_lower) {
            if relative.is_empty() || relative.starts_with('/') || relative.starts_with('\\') {
                let relative = relative.trim_start_matches(['/', '\\']);
                let abbreviated = if relative.is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{relative}")
                };
                if same_text(&abbreviated) {
                    return false;
                }
            }
        }
    }

    let path_parts = |value: &str| {
        value
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    let cwd_parts = path_parts(&cwd_text);
    let title_parts = path_parts(title);
    if !title_parts.is_empty()
        && (cwd_parts.starts_with(&title_parts) || cwd_parts.ends_with(&title_parts))
    {
        return false;
    }

    true
}

fn is_informative_terminal_title(context: &AgentTitleContext<'_>, title: &str) -> bool {
    let title = title.trim();
    if title.is_empty() || !is_novel_title_segment(context, title) {
        return false;
    }

    let segments = title
        .split(['—', '–', '·', '|'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    segments.len() <= 1
        || segments
            .iter()
            .any(|segment| is_novel_title_segment(context, segment))
}

/// Pick the same terminal-derived title for local and fleet agent rows.
pub(crate) fn agent_title_from_terminal_or_work(context: AgentTitleContext<'_>) -> Option<String> {
    context
        .detected_agent
        .and(context.terminal_title)
        .filter(|title| is_informative_terminal_title(&context, title))
        .map(|title| {
            let mut segments = title.splitn(2, ['—', '–', '·', '|']);
            let leading = segments.next().unwrap_or_default().trim();
            let remainder = segments
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if is_agent_identity_segment(&context, leading) {
                remainder.unwrap_or(title).to_string()
            } else {
                title.to_string()
            }
        })
        .or_else(|| context.work_title.map(str::to_string))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabNameOrigin {
    #[default]
    Structural,
    User,
    AgentDerived,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TabDisplayProjection {
    Manual(String),
    Derived {
        agent: Option<String>,
        ticket: Option<String>,
        binding: Option<String>,
        title: Option<String>,
    },
    Fallback(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabPrioAction {
    Toggle,
    Set(bool),
}

/// Separator joining the components of a derived tab projection.
pub(crate) const TAB_DISPLAY_SEPARATOR: &str = " · ";

pub(crate) fn title_without_identifier<'a>(identifier: &str, title: &'a str) -> Option<&'a str> {
    let title = title.trim();
    let prefix = title.get(..identifier.len())?;
    if !prefix.eq_ignore_ascii_case(identifier) {
        return None;
    }
    let remainder = title.get(identifier.len()..)?;
    if !remainder.is_empty()
        && !remainder.chars().next().is_some_and(|character| {
            character.is_whitespace() || matches!(character, ':' | '·' | '-')
        })
    {
        return None;
    }
    Some(
        remainder
            .trim_start()
            .strip_prefix([':', '·', '-'])
            .unwrap_or(remainder.trim_start())
            .trim_start(),
    )
}

/// The session's title as every surface names it.
///
/// The sidebar row and the status row read this one function, so a session
/// cannot be called two different things on the same screen. The worktree
/// binding is deliberately absent: it belongs to the Spaces and Repo group
/// headers, and the agent is already stripped from a derived title.
pub(crate) fn session_title(
    projection: Option<&TabDisplayProjection>,
    fallback: Option<String>,
) -> Option<String> {
    let Some(TabDisplayProjection::Derived {
        agent,
        ticket,
        binding: _,
        title,
    }) = projection
    else {
        return fallback;
    };
    let normalized_title = match (ticket.as_deref(), title.as_deref()) {
        (Some(identifier), Some(title)) => title_without_identifier(identifier, title)
            .map(str::to_string)
            .or_else(|| Some(title.to_string())),
        (_, title) => title.map(str::to_string),
    };
    let label = [ticket.clone(), normalized_title]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if label.is_empty() {
        agent.clone().or(fallback)
    } else {
        Some(label)
    }
}

impl TabDisplayProjection {
    pub(crate) fn full_label(&self) -> String {
        match self {
            Self::Manual(name) | Self::Fallback(name) => name.clone(),
            Self::Derived {
                agent,
                ticket,
                binding,
                title,
            } => {
                // The agent identity is surfaced separately (sidebar suffix, agent
                // rows); it only stands in as the label when nothing else exists.
                let label = [ticket, binding, title]
                    .into_iter()
                    .filter_map(|part| part.as_deref())
                    .collect::<Vec<_>>()
                    .join(TAB_DISPLAY_SEPARATOR);
                if label.is_empty() {
                    agent.clone().unwrap_or_default()
                } else {
                    label
                }
            }
        }
    }

    pub(crate) fn leads_with_agent_component(&self) -> bool {
        matches!(
            self,
            Self::Derived {
                agent: Some(_),
                ticket: None,
                binding: None,
                title: None,
            }
        )
    }
}

impl TabNameOrigin {
    pub(crate) fn expires_on_agent_session_change(self) -> bool {
        matches!(self, Self::AgentDerived)
    }
}

enum SplitCommand<'a> {
    Shell {
        command: &'a str,
        launch_env: &'a PaneLaunchEnv,
    },
    Argv {
        argv: &'a [String],
        launch_env: &'a PaneLaunchEnv,
    },
}

pub struct Tab {
    pub custom_name: Option<String>,
    pub name_origin: TabNameOrigin,
    pub number: usize,
    /// Identity source for this tab's pane tree.
    pub root_pane: PaneId,
    pub layout: TileLayout,
    /// Pane viewport state — always present, testable without PTYs.
    pub panes: HashMap<PaneId, PaneState>,
    #[cfg(test)]
    pub runtimes: HashMap<PaneId, TerminalRuntime>,
    pub zoomed: bool,
    pub prio: bool,
    /// Pinned by the user into the sidebar's Pinned group. Unlike the derived
    /// attention priority, nothing but an explicit toggle ever changes it.
    pub pinned: bool,
    /// Starred by the user to mark a focus session. Purely a user tag: it never
    /// moves the row between sidebar groups the way `pinned` does, and nothing
    /// but an explicit toggle ever changes it.
    pub starred: bool,
    /// Awaiting its first human keyboard input; shown in the inbox shelf.
    pub parked: bool,
    /// User-named sidebar subgroup this window belongs to. Organisation, not
    /// presentation: it persists with the session, and it never moves the
    /// window to a different group — it only nests the window one level under
    /// the group it already renders in.
    pub subgroup: Option<String>,
    pub events: mpsc::Sender<AppEvent>,
    pub(crate) render_notify: Arc<Notify>,
    pub(crate) render_dirty: Arc<RenderSignal>,
}

impl Tab {
    /// The subgroup this window renders under, normalized: trimmed, and
    /// absent rather than empty so a blanked-out name cannot leave a window
    /// stranded in a nameless header.
    pub(crate) fn subgroup(&self) -> Option<&str> {
        self.subgroup
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    pub(crate) fn set_subgroup(&mut self, subgroup: Option<String>) {
        self.subgroup = subgroup
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
    }

    fn pane_is_agent(&self, pane: PaneId, terminals: &HashMap<TerminalId, TerminalState>) -> bool {
        self.terminal_id(pane)
            .and_then(|terminal_id| terminals.get(terminal_id))
            .is_some_and(TerminalState::is_agent_terminal)
    }

    /// The pane this window is named after.
    ///
    /// The focused pane wins whenever it can name the window itself, and an
    /// agent pane always can. A plain shell cannot: it has no session name and
    /// no work context, and its own terminal title is rejected below because
    /// only an agent's title is ever novel enough to render. So focusing a
    /// shell next to a running agent used to throw away the one pane in the
    /// window that had something to say and fall back to the bare tab number.
    ///
    /// When the focused pane is not an agent, the window therefore keeps
    /// naming the agent session that was last in view: the pane focus came
    /// from if that was an agent, else the agent pane that was most recently
    /// active (ties, including agents that have never reported activity,
    /// resolve to the last in layout order). A window with no agent pane at
    /// all is unaffected — it still names its focused pane.
    pub(crate) fn title_source_pane(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> PaneId {
        let focused = self.layout.focused();
        if self.pane_is_agent(focused, terminals) {
            return focused;
        }
        if let Some(previous) = self.layout.prev_focused() {
            if previous != focused && self.pane_is_agent(previous, terminals) {
                return previous;
            }
        }
        self.layout
            .pane_ids()
            .into_iter()
            .filter(|pane| self.pane_is_agent(*pane, terminals))
            .max_by_key(|pane| {
                self.terminal_id(*pane)
                    .and_then(|terminal_id| terminals.get(terminal_id))
                    .and_then(TerminalState::agent_activity_at)
            })
            .unwrap_or(focused)
    }

    pub(crate) fn work_context_display_projection(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> Option<TabDisplayProjection> {
        let terminal = self
            .terminal_id(self.title_source_pane(terminals))
            .and_then(|terminal_id| terminals.get(terminal_id))?;
        let context = terminal.effective_work_context();
        let agent = terminal
            .effective_display_agent()
            .or_else(|| terminal.agent_name.clone())
            .or_else(|| terminal.effective_agent_label().map(str::to_string));
        let ticket = context.primary_ticket().map(str::to_string);
        let pr_has_active_owner = context.primary_pr().is_some_and(|pr_url| {
            terminals.values().any(|candidate| {
                let candidate = candidate.effective_work_context();
                candidate.is_active_owner_of(pr_url)
            })
        });
        let binding = context.binding_display(pr_has_active_owner);
        // Precedence: a human pane label, then the name the agent gave its own
        // session, then a novel terminal title, then the prompt-derived work
        // title. The session name outranks the terminal title because the
        // latter is whatever the agent last painted — often the checkout
        // directory before a session has been named.
        let title = terminal
            .manual_label
            .clone()
            .or_else(|| context.session_name.clone())
            .or_else(|| {
                let terminal_title = terminal.terminal_title_stripped();
                let home = std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(PathBuf::from);
                agent_title_from_terminal_or_work(AgentTitleContext {
                    terminal_title: terminal_title.as_deref(),
                    work_title: context.work_title.as_deref(),
                    cwd: Some(terminal.cwd.as_path()),
                    home: home.as_deref(),
                    agent_name: terminal.agent_name.as_deref(),
                    agent_label: terminal.effective_agent_label(),
                    display_agent: terminal.effective_display_agent().as_deref(),
                    detected_agent: terminal.detected_agent,
                })
            });
        (agent.is_some() || ticket.is_some() || binding.is_some() || title.is_some()).then_some(
            TabDisplayProjection::Derived {
                agent,
                ticket,
                binding,
                title,
            },
        )
    }

    // Tab construction threads pane runtime geometry, host context, and render hooks.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        number: usize,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        Self::new_with_runtime(
            number,
            initial_cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            events,
            render_notify,
            render_dirty,
            None,
        )
    }

    // Command tab construction mirrors the shell tab runtime arguments.
    #[allow(clippy::too_many_arguments)]
    pub fn new_argv_command(
        number: usize,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        argv: &[String],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        Self::new_with_runtime(
            number,
            initial_cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            launch_env,
            events,
            render_notify,
            render_dirty,
            Some(argv),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_runtime(
        number: usize,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
        argv: Option<&[String]>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        let (layout, root_id) = TileLayout::new();
        let runtime = if let Some(argv) = argv {
            TerminalRuntime::spawn_argv_command(
                root_id,
                rows,
                cols,
                initial_cwd.clone(),
                argv,
                launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                events.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            )?
        } else {
            TerminalRuntime::spawn(
                root_id,
                rows,
                cols,
                initial_cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                launch_env,
                events.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            )?
        };

        let terminal_id = TerminalId::alloc();
        let terminal = match argv {
            Some(argv) => {
                TerminalState::new(terminal_id.clone(), initial_cwd).with_launch_argv(argv.to_vec())
            }
            None => TerminalState::new(terminal_id.clone(), initial_cwd),
        };
        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new(terminal_id));

        Ok((
            Self {
                custom_name: None,
                name_origin: TabNameOrigin::Structural,
                number,
                root_pane: root_id,
                layout,
                panes,
                #[cfg(test)]
                runtimes: HashMap::new(),
                zoomed: false,
                prio: false,
                pinned: false,
                starred: false,
                parked: false,
                subgroup: None,
                events,
                render_notify,
                render_dirty,
            },
            terminal,
            runtime,
        ))
    }

    pub fn is_auto_named(&self) -> bool {
        self.custom_name.is_none()
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
        self.name_origin = TabNameOrigin::Structural;
    }

    pub(crate) fn apply_prio(&mut self, action: TabPrioAction) -> bool {
        let prio = match action {
            TabPrioAction::Toggle => return self.toggle_prio(),
            TabPrioAction::Set(prio) => prio,
        };
        self.set_prio(prio)
    }

    pub fn set_prio(&mut self, prio: bool) -> bool {
        let changed = self.prio != prio;
        self.prio = prio;
        changed
    }

    pub fn toggle_prio(&mut self) -> bool {
        self.set_prio(!self.prio)
    }

    pub fn set_user_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
        self.name_origin = TabNameOrigin::User;
    }

    /// Drop a manual name so the tab returns to the derived label. A user name
    /// outranks every derived source, so without this a rename is permanent and
    /// a tab can never follow its agent's session name again.
    pub fn clear_custom_name(&mut self) {
        self.custom_name = None;
        self.name_origin = TabNameOrigin::Structural;
    }

    pub(crate) fn expire_agent_scoped_name(&mut self) {
        if self.name_origin.expires_on_agent_session_change() {
            self.custom_name = None;
            self.name_origin = TabNameOrigin::Structural;
        }
    }

    #[cfg(test)]
    pub fn split_focused(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            self.layout.focused(),
            true,
            direction,
            None,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            None,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub fn split_focused_with_placement(
        &mut self,
        direction: Direction,
        before: bool,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            self.layout.focused(),
            true,
            direction,
            None,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            None,
            before,
        )
    }

    pub fn split_focused_command(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        command: &str,
        launch_env: &PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            self.layout.focused(),
            true,
            direction,
            None,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            launch_env,
            Some(SplitCommand::Shell {
                command,
                launch_env,
            }),
            false,
        )
    }

    /// Split `target` with a shell pane. Focus moves to the new pane only when
    /// `focus_new_pane` is set; a spawn failure rolls the layout back without
    /// touching focus or its history.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn split_pane_shell(
        &mut self,
        target: PaneId,
        focus_new_pane: bool,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        before: bool,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            target,
            focus_new_pane,
            direction,
            ratio,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            None,
            before,
        )
    }

    /// Split `target` with an argv-command pane. Same focus contract as
    /// `split_pane_shell`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn split_pane_argv(
        &mut self,
        target: PaneId,
        focus_new_pane: bool,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        argv: &[String],
        launch_env: &PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        before: bool,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            target,
            focus_new_pane,
            direction,
            ratio,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            launch_env,
            Some(SplitCommand::Argv { argv, launch_env }),
            before,
        )
    }

    // Split construction threads geometry, host context, launch policy, and command state.
    #[allow(clippy::too_many_arguments)]
    fn split_pane_with_runtime(
        &mut self,
        target: PaneId,
        focus_new_pane: bool,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        command: Option<SplitCommand<'_>>,
        before: bool,
    ) -> std::io::Result<NewPane> {
        let Some(new_id) =
            self.layout
                .split_pane_with_placement(target, direction, ratio.unwrap_or(0.5), before)
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "split target pane is not in the layout",
            ));
        };
        let actual_cwd =
            cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let launch_argv = if let Some(SplitCommand::Argv { argv, .. }) = &command {
            Some((*argv).to_vec())
        } else {
            None
        };
        let runtime = match command {
            Some(SplitCommand::Shell {
                command,
                launch_env,
            }) => TerminalRuntime::spawn_shell_command(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                command,
                launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
            Some(SplitCommand::Argv { argv, launch_env }) => TerminalRuntime::spawn_argv_command(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                argv,
                launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
            None => TerminalRuntime::spawn(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                launch_env,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
        };
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(err) => {
                self.layout.close_pane(new_id);
                return Err(err);
            }
        };
        let terminal_id = TerminalId::alloc();
        let terminal = match launch_argv {
            Some(argv) => {
                TerminalState::new(terminal_id.clone(), actual_cwd).with_launch_argv(argv)
            }
            None => TerminalState::new(terminal_id.clone(), actual_cwd),
        };
        if focus_new_pane {
            self.layout.focus_pane(new_id);
        }
        self.panes.insert(new_id, PaneState::new(terminal_id));
        self.zoomed = false;
        Ok(NewPane {
            pane_id: new_id,
            terminal,
            runtime,
        })
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> Option<DetachedPane> {
        self.detach_pane(pane_id)
    }

    pub fn remove_pane(&mut self, pane_id: PaneId) -> Option<DetachedPane> {
        self.detach_pane(pane_id)
    }

    pub(crate) fn from_existing_pane(
        number: usize,
        custom_name: Option<String>,
        moved: MovedPane,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> Self {
        let mut panes = HashMap::new();
        let pane_id = moved.pane_id;
        panes.insert(pane_id, moved.pane_state);
        Self {
            custom_name,
            name_origin: TabNameOrigin::Structural,
            number,
            root_pane: pane_id,
            layout: TileLayout::from_saved(Node::Pane(pane_id), pane_id),
            panes,
            #[cfg(test)]
            runtimes: HashMap::new(),
            zoomed: false,
            prio: false,
            pinned: false,
            starred: false,
            parked: false,
            subgroup: None,
            events,
            render_notify,
            render_dirty,
        }
    }

    pub(crate) fn take_pane_for_move(&mut self, pane_id: PaneId) -> Option<MovedPane> {
        if !self.panes.contains_key(&pane_id) {
            return None;
        }

        if self.layout.pane_count() > 1 {
            let next_root = self.promoted_root_if_needed(pane_id);
            self.layout.close_pane(pane_id);
            if let Some(next_root) = next_root {
                self.root_pane = next_root;
            }
        }

        let pane_state = self.panes.remove(&pane_id)?;
        self.zoomed = false;
        Some(MovedPane {
            pane_id,
            pane_state,
        })
    }

    pub(crate) fn insert_existing_pane(
        &mut self,
        target_pane_id: PaneId,
        moved: MovedPane,
        direction: Direction,
        ratio: f32,
        before: bool,
        focus: bool,
    ) -> Result<PaneId, Box<MovedPane>> {
        if !self.layout.insert_pane_near_with_placement(
            target_pane_id,
            moved.pane_id,
            direction,
            ratio,
            before,
            focus,
        ) {
            return Err(Box::new(moved));
        }
        let pane_id = moved.pane_id;
        self.panes.insert(pane_id, moved.pane_state);
        self.zoomed = false;
        Ok(pane_id)
    }

    fn detach_pane(&mut self, pane_id: PaneId) -> Option<DetachedPane> {
        if self.layout.pane_count() <= 1 {
            return None;
        }

        let next_root = self.promoted_root_if_needed(pane_id);

        self.layout.close_pane(pane_id);

        let pane = self.panes.remove(&pane_id)?;
        let terminal_id = pane.attached_terminal_id;
        self.zoomed = false;
        if let Some(next_root) = next_root {
            self.root_pane = next_root;
        }
        Some((pane_id, terminal_id))
    }

    fn promoted_root_if_needed(&self, closing: PaneId) -> Option<PaneId> {
        if self.root_pane != closing {
            return None;
        }
        self.layout.pane_ids().into_iter().find(|id| *id != closing)
    }

    pub fn terminal_id(&self, pane_id: PaneId) -> Option<&TerminalId> {
        self.panes
            .get(&pane_id)
            .map(|pane| &pane.attached_terminal_id)
    }

    pub fn cwd_for_pane(
        &self,
        pane_id: PaneId,
        terminals: &HashMap<TerminalId, TerminalState>,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        let terminal_id = self.terminal_id(pane_id)?;
        terminal_runtimes
            .get(terminal_id)
            .and_then(|rt| rt.cwd())
            .or_else(|| {
                terminals
                    .get(terminal_id)
                    .map(|terminal| terminal.cwd.clone())
            })
    }

    pub fn cached_cwd_for_pane(
        &self,
        pane_id: PaneId,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> Option<PathBuf> {
        let terminal_id = self.terminal_id(pane_id)?;
        terminals
            .get(terminal_id)
            .map(|terminal| terminal.cwd.clone())
    }

    pub fn foreground_cwd_for_pane(
        &self,
        pane_id: PaneId,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        let terminal_id = self.terminal_id(pane_id)?;
        terminal_runtimes
            .get(terminal_id)
            .and_then(|rt| rt.foreground_cwd())
    }
}

#[cfg(test)]
mod title_source_tests {
    use crate::app::AppState;
    use crate::detect::Agent;
    use crate::workspace::Workspace;
    use ratatui::layout::Direction;

    /// A window that holds a running agent must keep naming that agent's
    /// session while the human types in a shell next to it. Before this, the
    /// projection resolved the focused pane only, and a shell has no session
    /// name, no work context and no title the filters will accept — so the
    /// window label collapsed to its bare tab number.
    #[test]
    fn shell_focus_keeps_the_agent_session_title() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("w");
        let agent_pane = ws.tabs[0].root_pane;
        let shell_pane = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(shell_pane);
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        let agent_terminal = app.workspaces[0].tabs[0].panes[&agent_pane]
            .attached_terminal_id
            .clone();
        let shell_terminal = app.workspaces[0].tabs[0].panes[&shell_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&agent_terminal).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.manual_label = Some("rename the pane".to_string());
        app.terminals.get_mut(&shell_terminal).unwrap().manual_label = None;

        let label = app.workspaces[0].tabs[0]
            .work_context_display_projection(&app.terminals)
            .map(|projection| projection.full_label());
        assert_eq!(
            label.as_deref(),
            Some("rename the pane"),
            "a focused shell must not erase the agent session next to it"
        );
    }

    /// The focused pane still wins whenever it can name the window itself, so
    /// moving between two agents keeps following focus rather than sticking to
    /// whichever one was active longest ago.
    #[test]
    fn focused_agent_pane_still_wins() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("w");
        let first = ws.tabs[0].root_pane;
        let second = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(second);
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        for (pane, label) in [(first, "first agent"), (second, "second agent")] {
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.manual_label = Some(label.to_string());
        }

        let label = app.workspaces[0].tabs[0]
            .work_context_display_projection(&app.terminals)
            .map(|projection| projection.full_label());
        assert_eq!(label.as_deref(), Some("second agent"));
    }

    /// A window with no agent in it at all is untouched: it still names its
    /// focused pane, and falls back to the tab number when that pane has
    /// nothing to say.
    #[test]
    fn shell_only_window_is_unchanged() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("w");
        let shell = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(shell);
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        assert!(
            app.workspaces[0].tabs[0]
                .work_context_display_projection(&app.terminals)
                .is_none(),
            "a window of plain shells must not invent a title"
        );
    }
}
