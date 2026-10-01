use std::path::PathBuf;

use crate::api::schema::{EventData, EventEnvelope, EventKind};
#[cfg(test)]
use tracing::error;

use super::{api_helpers::pane_agent_status_with_stale, App, Mode};
use crate::{config::NewTerminalCwdConfig, workspace::Workspace};

pub(crate) fn resolve_new_terminal_cwd(
    policy: &NewTerminalCwdConfig,
    follow_cwd: Option<PathBuf>,
) -> PathBuf {
    match policy {
        NewTerminalCwdConfig::Follow => follow_cwd
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Home => std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Current => {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        }
        NewTerminalCwdConfig::Path(path) => crate::worktree::expand_tilde_path(path),
    }
}

pub(super) fn launch_cwd_for_terminal(
    terminal_id: &crate::terminal::TerminalId,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
) -> Option<PathBuf> {
    terminal_runtimes
        .get(terminal_id)
        .and_then(|runtime| runtime.follow_cwd())
        .or_else(|| {
            terminals
                .get(terminal_id)
                .map(|terminal| terminal.cwd.clone())
        })
}

impl App {
    pub(super) fn prepare_spawn_work_context(
        &self,
        context: Option<crate::work_context::PaneWorkContext>,
    ) -> Result<Option<crate::work_context::PaneWorkContext>, String> {
        let Some(mut context) = context else {
            return Ok(None);
        };
        context = context.normalized_spawn_binding()?;
        let Some(pr_url) = context.primary_pr() else {
            return Ok(Some(context));
        };
        if context.active_owner
            && self.state.workspaces.iter().any(|workspace| {
                workspace.tabs.iter().any(|tab| {
                    tab.panes.values().any(|pane| {
                        self.state
                            .terminals
                            .get(&pane.attached_terminal_id)
                            .map(crate::terminal::TerminalState::effective_work_context)
                            .is_some_and(|existing| existing.is_active_owner_of(pr_url))
                    })
                })
            })
        {
            context.active_owner = false;
        }
        Ok(Some(context))
    }

    pub(super) fn bind_spawn_work_context(
        terminal: &mut crate::terminal::TerminalState,
        context: Option<crate::work_context::PaneWorkContext>,
    ) {
        if let Some(context) = context {
            terminal.replace_prevalidated_manual_work_context(context);
        }
    }

    pub(super) fn bind_workspace_root_work_context(
        &mut self,
        ws_idx: usize,
        context: Option<crate::work_context::PaneWorkContext>,
    ) {
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.first())
            .and_then(|tab| tab.terminal_id(tab.root_pane))
            .cloned();
        if let Some(terminal) =
            terminal_id.and_then(|terminal_id| self.state.terminals.get_mut(&terminal_id))
        {
            Self::bind_spawn_work_context(terminal, context);
        }
    }

    pub(super) fn orphan_pane_work_owner(
        &mut self,
        pane_id: crate::layout::PaneId,
    ) -> Option<usize> {
        let (ws_idx, pane) = self.find_pane(pane_id)?;
        let terminal_id = pane.attached_terminal_id.clone();
        self.state
            .terminals
            .get_mut(&terminal_id)?
            .clear_active_work_owner()
            .then_some(ws_idx)
    }

    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        self.state
            .workspaces
            .get(ws_idx)?
            .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
    }

    pub(super) fn launch_cwd_for_pane_in_workspace(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<PathBuf> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let tab = workspace
            .tabs
            .get(workspace.find_tab_index_for_pane(pane_id)?)?;
        launch_cwd_for_terminal(
            tab.terminal_id(pane_id)?,
            &self.state.terminals,
            &self.terminal_runtimes,
        )
    }

    pub(super) fn focused_pane_cwd_in_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub(super) fn resolve_new_terminal_cwd(&self, follow_cwd: Option<PathBuf>) -> PathBuf {
        resolve_new_terminal_cwd(&self.state.new_terminal_cwd, follow_cwd)
    }

    pub(super) fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.server_mode() == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    pub(super) fn begin_tui_workspace_create(&mut self, request_id: &'static str) {
        if self.state.prompt_new_workspace_name {
            let follow_cwd = self.workspace_creation_source().and_then(|ws_idx| {
                self.focused_pane_cwd_in_workspace(ws_idx)
                    .or_else(|| self.seed_cwd_from_workspace(ws_idx))
            });
            let cwd = self.resolve_new_terminal_cwd(follow_cwd);
            super::input::open_new_workspace_dialog(&mut self.state, cwd);
            return;
        }

        let response = self.runtime_workspace_create(
            request_id,
            crate::api::schema::WorkspaceCreateParams {
                cwd: None,
                focus: true,
                label: None,
                env: Default::default(),
                work_context: None,
                source_workspace_id: None,
            },
        );
        if let Ok(error) = serde_json::from_str::<crate::api::schema::ErrorResponse>(&response) {
            crate::logging::workspace_create_failed(
                request_id,
                &error.error.code,
                &error.error.message,
            );
        }
    }

    /// Create a workspace with a real PTY (needs event_tx).
    #[cfg(test)]
    pub(crate) fn create_workspace(&mut self) {
        let follow_cwd = self.workspace_creation_source().and_then(|ws_idx| {
            self.focused_pane_cwd_in_workspace(ws_idx)
                .or_else(|| self.seed_cwd_from_workspace(ws_idx))
        });
        let initial_cwd = self.resolve_new_terminal_cwd(follow_cwd);
        if let Err(e) = self.create_workspace_with_events(initial_cwd, true) {
            error!(err = %e, "failed to create workspace");
            self.state.set_server_mode(Mode::Navigate);
        }
    }

    #[cfg(test)]
    pub(crate) fn create_tab(&mut self) {
        let custom_name = self.state.requested_new_tab_name.take();
        let active_before = self.state.active;
        let follow_cwd = self.state.active.and_then(|ws_idx| {
            self.focused_pane_cwd_in_workspace(ws_idx)
                .or_else(|| self.seed_cwd_from_workspace(ws_idx))
        });
        let initial_cwd = self.resolve_new_terminal_cwd(follow_cwd);
        match self.create_tab_with_options(initial_cwd, true) {
            Ok(created_idx) => {
                let created_workspace = active_before.is_none();
                let ws_idx = if created_workspace {
                    Some(created_idx)
                } else {
                    self.state.active
                };
                let tab_idx = if created_workspace { 0 } else { created_idx };
                if let Some(name) = custom_name {
                    if let Some(ws) =
                        ws_idx.and_then(|ws_idx| self.state.workspaces.get_mut(ws_idx))
                    {
                        if let Some(tab) = ws.tabs.get_mut(tab_idx) {
                            tab.set_custom_name(name);
                        }
                        self.schedule_session_save();
                    }
                }
                if let Some(ws_idx) = ws_idx {
                    if created_workspace {
                        self.emit_workspace_open_events(ws_idx);
                    } else {
                        self.emit_tab_created_events(ws_idx, tab_idx);
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "failed to create tab");
            }
        }
    }

    #[cfg(test)]
    pub(super) fn create_tab_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let Some(ws_idx) = self.state.active else {
            return self.create_workspace_with_options(initial_cwd, focus);
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let host_terminal_theme = self.state.pane_terminal_theme();
        let host_terminal_appearance = Some(self.state.pane_terminal_appearance());
        let ws = &mut self.state.workspaces[ws_idx];
        let (idx, terminal, runtime) = ws.create_tab(
            rows,
            cols,
            initial_cwd,
            self.state.pane_scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            Vec::new(),
        )?;
        let root_pane = ws.tabs[idx].root_pane;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.remove_alias_shadowed_by_new_pane(root_pane);
        if focus {
            self.state.switch_workspace_tab(ws_idx, idx);
            self.focus_client_on_pane();
        }
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self
            .public_tab_id(ws_idx, idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&workspace_id, idx + 1));
        let root_pane = self.state.workspaces[ws_idx].tabs[idx].root_pane.raw();
        crate::logging::tab_created(&workspace_id, &tab_id, root_pane);
        self.schedule_session_save();
        Ok(idx)
    }

    pub(crate) fn create_workspace_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        self.create_workspace_with_launch_env(initial_cwd, focus, Vec::new())
    }

    pub(crate) fn open_fleet_host(&mut self, name: &str) {
        self.open_fleet_host_focused(name, None);
    }

    pub(crate) fn open_agent_run_log(&mut self, host_name: &str, run_id: &str) {
        let Some(host) = self.fleet_poller_config.host(host_name) else {
            self.show_fleet_launch_error("run host is no longer configured".to_string());
            return;
        };
        let argv = match crate::agent_runs::log_argv(&host, run_id) {
            Ok(argv) => argv,
            Err(error) => {
                self.show_fleet_launch_error(error);
                return;
            }
        };
        if let Err(error) = self.open_fleet_tab_for_argv(&argv) {
            self.show_fleet_launch_error(error.to_string());
        }
    }

    /// Attach to a fleet host, or to one agent on it.
    ///
    /// With an agent the pane streams that agent's remote terminal directly.
    /// Without one it opens the whole remote session, which is what the Hosts
    /// surface asks for.
    pub(crate) fn open_fleet_host_focused(&mut self, name: &str, focus_agent: Option<&str>) {
        if let Some(agent_ref) =
            focus_agent.and_then(|agent| crate::api::schema::AgentRef::new(name, agent).ok())
        {
            let shared_mode = self.state.server_mode();
            let state = &self.state;
            let proxy_pane =
                self.remote_focus_operations
                    .proxy_pane_for_agent(&agent_ref, |pane_id| {
                        state
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.pane_state(pane_id).is_some())
                    });
            if let Some(pane_id) = proxy_pane {
                if let Some((ws_idx, _)) = self.find_pane(pane_id) {
                    self.state.focus_pane_in_workspace(ws_idx, pane_id);
                    self.state
                        .restore_server_mode_preserving_client_focus(shared_mode);
                    self.focus_client_on_pane();
                    self.state.sidebar_selected_remote_agent = Some(agent_ref);
                    return;
                }
            }
            let start = self.start_remote_focus_operation(agent_ref.clone());
            self.state
                .restore_server_mode_preserving_client_focus(shared_mode);
            match start {
                Ok(_) => {
                    self.focus_client_on_pane();
                    self.state.sidebar_selected_remote_agent = Some(agent_ref);
                }
                Err(error) => self.show_fleet_launch_error(error.message),
            }
            return;
        }
        let Some(configured_host) = self.fleet_poller_config.host(name) else {
            self.show_fleet_launch_error(
                "host is no longer in the fleet configuration".to_string(),
            );
            return;
        };
        let Some(host) = self
            .state
            .fleet_snapshot
            .hosts
            .iter()
            .find(|host| host.name == name)
            .cloned()
        else {
            self.show_fleet_launch_error("host is no longer in the fleet inventory".to_string());
            return;
        };
        if !host.matches_config(&configured_host) {
            self.show_fleet_launch_error(
                "host configuration changed; wait for a fresh fleet poll".to_string(),
            );
            return;
        }
        if host.state == crate::fleet::HostState::Unreachable {
            self.show_fleet_launch_error(
                host.error
                    .clone()
                    .unwrap_or_else(|| format!("{} is unreachable", host.name)),
            );
            return;
        }
        let argv = match focus_agent {
            Some(agent) => crate::fleet::agent_attach_argv_from_config(&configured_host, agent),
            None => crate::fleet::host_attach_argv_from_config(&configured_host),
        };
        let argv = match argv {
            Ok(argv) => argv,
            Err(error) => {
                self.show_fleet_launch_error(error);
                return;
            }
        };
        // An attach target is one thing. Clicking its row again must return to the
        // pane already attached to it rather than dial a second ssh connection
        // and leave the operator with two views of the same agent.
        if let Err(error) = self.open_fleet_agent_tab_for_argv(
            &argv,
            focus_agent.and_then(|agent| crate::api::schema::AgentRef::new(name, agent).ok()),
        ) {
            tracing::warn!(host = %host.name, %error, "could not open fleet host");
            self.show_fleet_launch_error(error.to_string());
        }
    }

    fn open_fleet_tab_for_argv(&mut self, argv: &[String]) -> std::io::Result<()> {
        self.open_fleet_agent_tab_for_argv(argv, None)
    }

    fn open_fleet_agent_tab_for_argv(
        &mut self,
        argv: &[String],
        agent_ref: Option<crate::api::schema::AgentRef>,
    ) -> std::io::Result<()> {
        if self.focus_attached_fleet_host_pane(argv) {
            if let (Some(agent_ref), Some((_, pane_id))) = (agent_ref, self.focused_pane_target()) {
                self.fleet_attach_agents.insert(pane_id, agent_ref);
            }
            Ok(())
        } else {
            let pane_id = self.create_fleet_host_tab(argv)?;
            if let Some(agent_ref) = agent_ref {
                self.fleet_attach_agents.insert(pane_id, agent_ref);
            }
            Ok(())
        }
    }

    /// Focus the pane already running `argv`, if one is open. The launch argv
    /// is the host's identity here: it carries the target and session the
    /// attach was built from, so two hosts can never collide on it.
    fn focus_attached_fleet_host_pane(&mut self, argv: &[String]) -> bool {
        let target = self
            .state
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, workspace)| workspace.is_fleet)
            .find_map(|(ws_idx, workspace)| {
                workspace
                    .tabs
                    .iter()
                    .enumerate()
                    .find_map(|(tab_idx, tab)| {
                        tab.panes
                            .iter()
                            .find(|(_, pane)| {
                                self.state
                                    .terminals
                                    .get(&pane.attached_terminal_id)
                                    .and_then(|terminal| terminal.launch_argv.as_deref())
                                    == Some(argv)
                            })
                            .map(|(pane_id, _)| (ws_idx, tab_idx, *pane_id))
                    })
            });
        let Some((ws_idx, tab_idx, pane_id)) = target else {
            return false;
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.focus_client_on_pane();
        true
    }

    fn create_fleet_host_tab(&mut self, argv: &[String]) -> std::io::Result<crate::layout::PaneId> {
        let (rows, cols) = self.state.estimate_pane_size();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.pane_terminal_theme();
        let host_terminal_appearance = Some(self.state.pane_terminal_appearance());
        let fleet_ws_idx = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.is_fleet);
        let cwd = fleet_ws_idx
            .or(self.state.active)
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
            .and_then(|workspace| {
                workspace.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
            })
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| std::io::Error::other("no working directory for host pane"))?;
        let (ws_idx, tab_idx, terminal, runtime, root_pane, created_workspace) =
            if let Some(ws_idx) = fleet_ws_idx {
                let workspace = &mut self.state.workspaces[ws_idx];
                if workspace.custom_name.is_none() {
                    workspace.custom_name = Some("Fleet".to_owned());
                }
                let (tab_idx, terminal, runtime) = workspace.create_tab_argv_command(
                    rows,
                    cols,
                    cwd,
                    argv,
                    Vec::new(),
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                )?;
                let root_pane = workspace.tabs[tab_idx].root_pane;
                (ws_idx, tab_idx, terminal, runtime, root_pane, false)
            } else {
                let (mut workspace, terminal, runtime) = Workspace::new_argv_command(
                    cwd,
                    rows,
                    cols,
                    argv,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    self.event_tx.clone(),
                    self.render_notify.clone(),
                    self.render_dirty.clone(),
                )?;
                workspace.is_fleet = true;
                workspace.custom_name = Some("Fleet".to_owned());
                let root_pane = workspace.tabs[0].root_pane;
                self.state.workspaces.push(workspace);
                (
                    self.state.workspaces.len() - 1,
                    0,
                    terminal,
                    runtime,
                    root_pane,
                    true,
                )
            };
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.pending_first_frame_pane = Some(root_pane);
        self.state.remove_alias_shadowed_by_new_pane(root_pane);
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.focus_client_on_pane();
        if created_workspace {
            let workspace_id = self.state.workspaces[ws_idx].id.clone();
            crate::logging::workspace_created(&workspace_id, root_pane.raw());
            self.emit_workspace_open_events(ws_idx);
        } else {
            self.emit_tab_created_events(ws_idx, tab_idx);
        }
        self.schedule_session_save();
        Ok(root_pane)
    }

    fn show_fleet_launch_error(&mut self, context: String) {
        let previous_toast = self.state.toast.clone();
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "host launch failed".to_string(),
            context,
            position: None,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
    }

    pub(crate) fn dispatch_home_composer(
        &mut self,
        plan: crate::app::home::HomeDispatchPlan,
    ) -> std::io::Result<()> {
        let (rows, cols) = self.state.estimate_pane_size();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.pane_terminal_theme();
        let host_terminal_appearance = Some(self.state.pane_terminal_appearance());

        match plan.target {
            crate::app::home::HomeTarget::NewSpace => {
                let (workspace, mut terminal, runtime) =
                    Workspace::new_argv_command_with_extra_env(
                        plan.directory,
                        rows,
                        cols,
                        &plan.argv,
                        scrollback_limit_bytes,
                        host_terminal_theme,
                        host_terminal_appearance,
                        self.event_tx.clone(),
                        self.render_notify.clone(),
                        self.render_dirty.clone(),
                        plan.env.clone(),
                    )?;
                apply_home_work_context(
                    &mut terminal,
                    &plan.work_context_patch,
                    plan.pr.as_ref(),
                    plan.ticket.as_ref(),
                    plan.missive.as_ref(),
                )?;
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.pending_first_frame_pane = Some(workspace.tabs[0].root_pane);
                crate::logging::home_dispatch_completed(workspace.tabs[0].root_pane.raw());
                self.state.workspaces.push(workspace);
                let ws_idx = self.state.workspaces.len() - 1;
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[0].root_pane,
                );
                self.state.switch_workspace(ws_idx);
                self.focus_client_on_pane();
                self.emit_workspace_open_events(ws_idx);
            }
            crate::app::home::HomeTarget::Existing(workspace_id) => {
                let Some(ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == workspace_id)
                else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "selected space no longer exists",
                    ));
                };
                let (tab_idx, mut terminal, runtime, root_pane) = {
                    let workspace = &mut self.state.workspaces[ws_idx];
                    let (tab_idx, terminal, runtime) = workspace.create_tab_argv_command(
                        rows,
                        cols,
                        plan.directory,
                        &plan.argv,
                        plan.env.clone(),
                        scrollback_limit_bytes,
                        host_terminal_theme,
                        host_terminal_appearance,
                    )?;
                    let root_pane = workspace.tabs[tab_idx].root_pane;
                    (tab_idx, terminal, runtime, root_pane)
                };
                apply_home_work_context(
                    &mut terminal,
                    &plan.work_context_patch,
                    plan.pr.as_ref(),
                    plan.ticket.as_ref(),
                    plan.missive.as_ref(),
                )?;
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.pending_first_frame_pane = Some(root_pane);
                crate::logging::home_dispatch_completed(root_pane.raw());
                self.state.remove_alias_shadowed_by_new_pane(root_pane);
                self.state.switch_workspace_tab(ws_idx, tab_idx);
                self.focus_client_on_pane();
                self.emit_tab_created_events(ws_idx, tab_idx);
            }
        }
        self.schedule_session_save();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn create_workspace_with_events(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<()> {
        let ws_idx = self.create_workspace_with_options(initial_cwd, focus)?;
        self.emit_workspace_open_events(ws_idx);
        Ok(())
    }

    pub(crate) fn create_workspace_with_launch_env(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<usize> {
        self.create_workspace_with_launch_env_and_work_context(initial_cwd, focus, extra_env, None)
    }

    pub(crate) fn create_workspace_with_launch_env_and_work_context(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
        extra_env: Vec<(String, String)>,
        work_context: Option<crate::work_context::PaneWorkContext>,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let (ws, mut terminal, runtime) = Workspace::new_with_extra_env(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.pane_terminal_theme(),
            Some(self.state.pane_terminal_appearance()),
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
            extra_env,
        )?;
        Self::bind_spawn_work_context(&mut terminal, work_context);
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        self.state
            .remove_alias_shadowed_by_new_pane(self.state.workspaces[idx].tabs[0].root_pane);
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].tabs[0].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.focus_client_on_pane();
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .tabs
                .iter()
                .flat_map(|tab| tab.layout.pane_ids().into_iter())
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids().into_iter())
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    pub(super) fn tab_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let (agg_state, seen, _) = tab.aggregate_state_and_attention(&self.state.terminals);
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            number: tab.number,
            label: ws
                .tab_display_projection(&self.state.terminals, tab_idx)
                .map(|projection| projection.full_label())
                .unwrap_or_else(|| (tab_idx + 1).to_string()),
            prio: tab.prio,
            starred: tab.starred,
            focused: self.state.active == Some(ws_idx) && ws.active_tab == tab_idx,
            pane_count: tab.panes.len(),
            agent_status: aggregate_tab_agent_status(tab, &self.state.terminals, agg_state, seen),
        })
    }

    pub(crate) fn emit_workspace_open_events(&mut self, ws_idx: usize) {
        let workspace_info = self.workspace_info(ws_idx);
        let Some(tab) = self.tab_info(ws_idx, 0) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, 0) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceCreated,
            data: EventData::WorkspaceCreated {
                workspace: workspace_info,
            },
        });
        self.emit_tab_and_pane_created_events(tab, root_pane);
        self.emit_layout_updated_event(ws_idx, 0);
    }

    pub(crate) fn emit_tab_created_events(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, tab_idx) else {
            return;
        };
        self.emit_tab_and_pane_created_events(tab, root_pane);
        self.emit_layout_updated_event(ws_idx, tab_idx);
    }

    fn emit_tab_and_pane_created_events(
        &mut self,
        tab: crate::api::schema::TabInfo,
        root_pane: crate::api::schema::PaneInfo,
    ) {
        self.emit_event(EventEnvelope {
            event: EventKind::TabCreated,
            data: EventData::TabCreated { tab },
        });
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: root_pane },
        });
    }

    pub(super) fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    pub(super) fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info(ws_idx, tab_idx)?,
            root_pane: self.root_pane_info(ws_idx, tab_idx)?,
        })
    }

    pub(super) fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        self.pane_info(ws_idx, tab.root_pane)
    }

    pub(super) fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let scroll = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .and_then(|runtime| runtime.scroll_metrics())
            .map(|metrics| crate::api::schema::PaneScrollInfo {
                offset_from_bottom: metrics.offset_from_bottom as u64,
                max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                viewport_rows: metrics.viewport_rows as u64,
            });
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        let presentation = terminal.effective_presentation();
        let projection = pane.agent_projection(terminal);
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            terminal_id: terminal.id.to_string(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            settled_at: pane.settled_at,
            snoozed_until: pane.snoozed_until(),
            work_context: terminal.effective_work_context().clone(),
            cwd: ws.tabs[tab_idx]
                .cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            foreground_cwd: ws.tabs[tab_idx]
                .foreground_cwd_for_pane(pane_id, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            label: terminal.manual_label.clone(),
            agent: terminal.effective_agent_label().map(str::to_string),
            title: presentation.title,
            terminal_title: terminal.terminal_title.clone(),
            terminal_title_stripped: terminal.terminal_title_stripped(),
            display_agent: presentation.display_agent,
            agent_status: pane_agent_status_with_stale(
                projection.state,
                projection.seen,
                projection.stale,
            ),
            waiting_on_agents: pane.settled_at.is_none() && terminal.waiting_on_agents(),
            wait: terminal
                .hook_authority
                .as_ref()
                .and_then(|report| report.wait.clone()),
            eta_s: terminal
                .hook_authority
                .as_ref()
                .and_then(|report| report.eta_s),
            reported_at: terminal
                .hook_authority
                .as_ref()
                .and_then(|report| report.reported_at_wire.clone()),
            last_turn_at: terminal.last_turn_at().map(str::to_string),
            state_labels: presentation.state_labels,
            tokens: terminal.metadata_tokens_for_api(),
            gates: terminal.closing_gates().to_vec(),
            items: terminal.closing_items().to_vec(),
            decisions: terminal.closing_decisions().to_vec(),
            agent_session: terminal_agent_session_info(terminal),
            scroll,
            revision: terminal.revision,
            restore_error: terminal.restore_error.clone(),
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::terminal::TerminalRuntime, String)> {
        let runtime =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state(&self.state.terminals);
        let stale = ws.tabs.iter().any(|tab| {
            tab.panes.values().any(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .is_some_and(|terminal| terminal.supervisor_stale)
            })
        });
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            is_fleet: ws.is_fleet,
            number: index + 1,
            label: ws.display_name_from(&self.state.terminals, &self.terminal_runtimes),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.tabs.len(),
            active_tab_id: self.public_tab_id(index, ws.active_tab).unwrap_or_else(|| {
                crate::workspace::public_tab_id_for_number(&ws.id, ws.active_tab + 1)
            }),
            agent_status: pane_agent_status_with_stale(agg_state, seen, stale),
            tokens: ws.metadata_tokens.values(),
            worktree: ws
                .worktree_space()
                .map(|space| crate::api::schema::WorkspaceWorktreeInfo {
                    repo_key: space.key.clone(),
                    repo_name: space.label.clone(),
                    repo_root: space.repo_root.display().to_string(),
                    checkout_path: space.checkout_path.display().to_string(),
                    is_linked_worktree: space.is_linked_worktree,
                }),
            repo_binding: ws.repo_binding.clone(),
        }
    }
}

pub(crate) fn terminal_agent_session_info(
    terminal: &crate::terminal::TerminalState,
) -> Option<crate::api::schema::AgentSessionInfo> {
    if let Some(authority) = terminal.hook_authority.as_ref() {
        if let Some(session_ref) = authority.session_ref.as_ref() {
            return Some(crate::api::schema::AgentSessionInfo {
                source: authority.source.clone(),
                agent: authority.agent_label.clone(),
                kind: session_ref.kind,
                value: session_ref.value.clone(),
            });
        }
    }

    terminal
        .persisted_agent_session
        .as_ref()
        .map(|session| crate::api::schema::AgentSessionInfo {
            source: session.source.clone(),
            agent: session.agent.clone(),
            kind: session.session_ref.kind,
            value: session.session_ref.value.clone(),
        })
}

fn apply_home_work_context(
    terminal: &mut crate::terminal::TerminalState,
    patch: &crate::work_context::PaneWorkContextPatch,
    pr: Option<&crate::app::home::HomePrContext>,
    ticket: Option<&crate::app::home::HomeTicketContext>,
    missive: Option<&crate::app::home::HomeMissiveContext>,
) -> std::io::Result<()> {
    let mut patch = patch.clone();
    if patch.repo.is_none() {
        patch.repo = pr.map(|pr| pr.repo.clone());
    }
    if patch.pr_urls.is_none() {
        patch.pr_urls = pr.map(|pr| vec![pr.url.clone()]);
    }
    if patch.ticket_ids.is_none() {
        patch.ticket_ids = ticket.map(|ticket| vec![ticket.identifier.clone()]);
    }
    if patch.missive_urls.is_none() {
        patch.missive_urls = missive.map(|conversation| vec![conversation.web_url.clone()]);
    }
    if patch.is_empty() {
        return Ok(());
    }
    terminal
        .apply_manual_work_context_patch(patch)
        .map(|_| ())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))
}

fn aggregate_tab_agent_status(
    tab: &crate::workspace::Tab,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    state: crate::detect::AgentState,
    seen: bool,
) -> crate::api::schema::AgentStatus {
    let stale = tab.panes.values().any(|pane| {
        terminals
            .get(&pane.attached_terminal_id)
            .is_some_and(|terminal| terminal.supervisor_stale)
    });
    pane_agent_status_with_stale(state, seen, stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_test_home() -> PathBuf {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("test host HOME or USERPROFILE")
    }

    #[test]
    fn unreachable_fleet_host_surfaces_error_without_spawning() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "ub2".into(),
            target: "ub2".into(),
            ..Default::default()
        }];
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.fleet_snapshot.hosts = vec![crate::fleet::HostSnapshot {
            name: "ub2".to_string(),
            target: "ub2".to_string(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Unreachable,
            version: None,
            protocol: None,
            error: Some("ssh: connection refused".to_string()),
            remote_identity: None,
            sessions: None,
            reachable: false,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }];

        app.open_fleet_host("ub2");

        assert!(app.state.workspaces.is_empty());
        let toast = app.state.toast.expect("launch failure toast");
        assert_eq!(toast.title, "host launch failed");
        assert_eq!(toast.context, "ssh: connection refused");
    }

    #[test]
    fn retargeted_fleet_host_is_not_attachable_before_a_fresh_poll() {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "office".into(),
            target: "machine-a".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            configured_hosts: vec!["office".into()],
            hosts: vec![crate::fleet::HostSnapshot {
                name: "office".into(),
                target: "machine-a".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: Vec::new(),
            }],
            ..crate::fleet::Snapshot::default()
        };

        let mut reloaded = config;
        reloaded.remote.fleet.hosts[0].target = "machine-b".into();
        app.apply_live_config(&reloaded, &[], &[], false);
        app.open_fleet_host("office");

        assert!(app.state.workspaces.is_empty());
        let toast = app.state.toast.expect("launch failure toast");
        assert_eq!(toast.title, "host launch failed");
        assert!(toast.context.contains("fleet inventory"));
    }

    #[test]
    fn current_generation_row_with_old_connection_is_not_used_for_attach() {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "office".into(),
            target: "machine-a".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        let mut reloaded = config;
        reloaded.remote.fleet.hosts[0].target = "machine-b".into();
        app.apply_live_config(&reloaded, &[], &[], false);
        app.state.fleet_snapshot.hosts = vec![crate::fleet::HostSnapshot {
            name: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }];

        app.open_fleet_host("office");

        assert!(app.state.workspaces.is_empty());
        let toast = app.state.toast.expect("launch failure toast");
        assert_eq!(
            toast.context,
            "host configuration changed; wait for a fresh fleet poll"
        );
    }

    fn app_with_fleet_attach_tuple(
        configured_socket: Option<&str>,
        configured_session: Option<&str>,
        observed_socket: Option<&str>,
        observed_session: Option<&str>,
    ) -> App {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "office".into(),
            target: "current-target".into(),
            socket: configured_socket.map(str::to_string),
            session: configured_session.map(str::to_string),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.workspaces = vec![Workspace::test_new("existing-space")];
        app.state.workspaces[0].is_fleet = true;
        app.state.workspaces[0].custom_name = Some("Fleet".to_owned());
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.fleet_snapshot.hosts = vec![crate::fleet::HostSnapshot {
            name: "office".into(),
            target: "current-target".into(),
            local: false,
            socket: observed_socket.map(str::to_string),
            session: observed_session.map(str::to_string),
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }];
        app
    }

    fn app_for_fleet_workspace_creation() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![
            Workspace::test_new("local-one"),
            Workspace::test_new("local-two"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    fn fleet_workspace_test_argv(label: &str) -> Vec<String> {
        vec![
            crate::app::api::test_support::exiting_test_command().into(),
            label.into(),
        ]
    }

    #[tokio::test]
    async fn fleet_workspace_ac1_host_agent_and_run_log_use_one_dedicated_workspace() {
        use crate::app::api::test_support::shutdown_test_runtimes;

        let mut app = app_for_fleet_workspace_creation();
        let local_tab_counts = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.tabs.len())
            .collect::<Vec<_>>();

        for label in ["fleet-host", "fleet-agent", "fleet-run-log"] {
            app.open_fleet_tab_for_argv(&fleet_workspace_test_argv(label))
                .expect("fleet tab should spawn in the test process");
        }

        assert_eq!(
            app.state
                .workspaces
                .iter()
                .filter(|workspace| workspace.is_fleet)
                .count(),
            1
        );
        assert_eq!(
            app.state.workspaces[..2]
                .iter()
                .map(|workspace| workspace.tabs.len())
                .collect::<Vec<_>>(),
            local_tab_counts
        );
        let fleet_workspace = app
            .state
            .workspaces
            .iter()
            .find(|workspace| workspace.is_fleet)
            .expect("dedicated fleet workspace");
        assert_eq!(fleet_workspace.tabs.len(), 3);
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn fleet_workspace_ac3_reclick_focuses_existing_tab_without_spawning() {
        use crate::app::api::test_support::shutdown_test_runtimes;

        let mut app = app_for_fleet_workspace_creation();
        let argv = fleet_workspace_test_argv("same-fleet-entry");
        app.open_fleet_tab_for_argv(&argv)
            .expect("initial fleet tab should spawn");
        let fleet_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.is_fleet)
            .expect("fleet workspace");
        let root_pane = app.state.workspaces[fleet_idx].tabs[0].root_pane;
        let terminal_count = app.state.terminals.len();
        let workspace_count = app.state.workspaces.len();
        app.state.active = Some(0);

        app.open_fleet_tab_for_argv(&argv)
            .expect("re-click should reuse the open fleet tab");

        assert_eq!(app.state.active, Some(fleet_idx));
        assert_eq!(
            app.state.workspaces[fleet_idx].focused_pane_id(),
            Some(root_pane)
        );
        assert_eq!(app.state.workspaces[fleet_idx].tabs.len(), 1);
        assert_eq!(app.state.terminals.len(), terminal_count);
        assert_eq!(app.state.workspaces.len(), workspace_count);
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn fleet_workspace_ac4_closing_last_fleet_tab_leaves_no_visible_fleet_space() {
        use crate::app::api::test_support::shutdown_test_runtimes;

        let mut app = app_for_fleet_workspace_creation();
        app.open_fleet_tab_for_argv(&fleet_workspace_test_argv("last-fleet-tab"))
            .expect("fleet tab should spawn");
        let fleet_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.is_fleet)
            .expect("fleet workspace");
        app.state.selected = fleet_idx;

        app.state.close_selected_workspace();

        assert!(app
            .state
            .workspaces
            .iter()
            .all(|workspace| !workspace.is_fleet));
        assert!(crate::ui::sidebar::workspace_list_entries(&app.state)
            .iter()
            .all(
                |entry| !matches!(entry, crate::ui::WorkspaceListEntry::Workspace { ws_idx, .. }
                if app.state.workspaces[*ws_idx].is_fleet)
            ));
        shutdown_test_runtimes(&mut app);
    }

    #[test]
    // A8: a matching snapshot tuple attaches with argv built from current config.
    fn matching_fleet_tuple_attaches_with_current_config_argv() {
        let mut app = app_with_fleet_attach_tuple(
            Some("/tmp/current.sock"),
            Some("current-session"),
            Some("/tmp/current.sock"),
            Some("current-session"),
        );
        // Mark the existing pane as already attached with the argv current
        // config produces. Attach reuses a pane only on an exact argv match,
        // so reuse proves the tuple gate passed and the argv came from config
        // without spawning a real ssh process.
        let expected_argv = vec![
            "herdr".to_string(),
            "--remote".to_string(),
            "current-target".to_string(),
            "--session".to_string(),
            "current-session".to_string(),
        ];
        let tab = &app.state.workspaces[0].tabs[0];
        let terminal_id = tab
            .terminal_id(tab.root_pane)
            .expect("existing pane has a terminal")
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("existing terminal")
            .launch_argv = Some(expected_argv);

        app.open_fleet_host("office");

        assert!(app.state.toast.is_none(), "{:?}", app.state.toast);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    // A8: socket-only and session-only snapshot tuple drift both refuse attach.
    fn socket_or_session_tuple_disagreement_refuses_attach() {
        for mut app in [
            app_with_fleet_attach_tuple(
                Some("/tmp/current.sock"),
                Some("current-session"),
                Some("/tmp/stale.sock"),
                Some("current-session"),
            ),
            app_with_fleet_attach_tuple(
                Some("/tmp/current.sock"),
                Some("current-session"),
                Some("/tmp/current.sock"),
                Some("stale-session"),
            ),
        ] {
            app.open_fleet_host("office");

            assert_eq!(app.state.workspaces[0].tabs.len(), 1);
            let toast = app.state.toast.expect("tuple drift should refuse attach");
            assert_eq!(toast.title, "host launch failed");
            assert_eq!(
                toast.context,
                "host configuration changed; wait for a fresh fleet poll"
            );
        }
    }

    fn fixed_home_dispatch_plan(
        target: crate::app::home::HomeTarget,
    ) -> crate::app::home::HomeDispatchPlan {
        let mut home = crate::app::home::HomeState::default();
        home.directory = std::env::temp_dir();
        home.target = target;
        home.prompt = "characterize home dispatch".into();
        let mut plan = home.dispatch_plan().expect("fixed inputs should dispatch");
        // The plan is characterized for its directory, target and workspace; the
        // agent binary itself is not under test and is absent on CI runners, so
        // spawn a command that exists everywhere.
        plan.argv = vec![crate::app::api::test_support::exiting_test_command().into()];
        plan
    }

    #[tokio::test]
    async fn dispatch_home_composer_creates_requested_workspace_or_tab() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut new_space_app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let new_space_plan = fixed_home_dispatch_plan(crate::app::home::HomeTarget::NewSpace);
        new_space_app.state.set_server_mode(Mode::Settings);
        new_space_app
            .dispatch_home_composer(new_space_plan.clone())
            .expect("new-space dispatch should succeed");

        assert_eq!(new_space_app.state.workspaces.len(), 1);
        assert_eq!(new_space_app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(
            new_space_app.state.workspaces[0].identity_cwd,
            new_space_plan.directory
        );
        let new_pane = new_space_app.state.workspaces[0].tabs[0].root_pane;
        let new_terminal = new_space_app.state.workspaces[0]
            .terminal_id(new_pane)
            .and_then(|terminal_id| new_space_app.state.terminals.get(terminal_id))
            .expect("new space should have a terminal");
        assert_eq!(new_terminal.cwd, new_space_plan.directory);
        assert_eq!(
            new_terminal.launch_argv.as_ref(),
            Some(&new_space_plan.argv)
        );
        assert_eq!(new_space_app.state.server_mode(), Mode::Settings);

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut existing_app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        existing_app.state.workspaces = vec![Workspace::test_new("existing-space")];
        existing_app.state.ensure_test_terminals();
        existing_app.state.set_server_mode(Mode::Settings);
        let workspace_id = existing_app.state.workspaces[0].id.clone();
        let existing_plan =
            fixed_home_dispatch_plan(crate::app::home::HomeTarget::Existing(workspace_id));
        existing_app
            .dispatch_home_composer(existing_plan.clone())
            .expect("existing-space dispatch should succeed");

        assert_eq!(existing_app.state.workspaces.len(), 1);
        assert_eq!(existing_app.state.workspaces[0].tabs.len(), 2);
        let added_tab = &existing_app.state.workspaces[0].tabs[1];
        let added_terminal = added_tab
            .terminal_id(added_tab.root_pane)
            .and_then(|terminal_id| existing_app.state.terminals.get(terminal_id))
            .expect("added tab should have a terminal");
        assert_eq!(added_terminal.cwd, existing_plan.directory);
        assert_eq!(
            added_terminal.launch_argv.as_ref(),
            Some(&existing_plan.argv)
        );
        assert_eq!(existing_app.state.active, Some(0));
        assert_eq!(existing_app.state.workspaces[0].active_tab, 1);
        assert_eq!(existing_app.state.server_mode(), Mode::Settings);
    }

    #[tokio::test]
    async fn dispatch_home_composer_applies_pr_url_to_spawned_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut plan = fixed_home_dispatch_plan(crate::app::home::HomeTarget::NewSpace);
        plan.pr = Some(crate::app::home::HomePrContext {
            url: "https://github.com/owner/repo/pull/42".into(),
            number: 42,
            repo: "owner/repo".into(),
        });

        app.dispatch_home_composer(plan)
            .expect("PR dispatch should create a pane");

        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.workspaces[0]
            .terminal_id(pane)
            .and_then(|terminal_id| app.state.terminals.get(terminal_id))
            .expect("spawned pane terminal");
        assert_eq!(
            terminal.effective_work_context().pr_urls,
            ["https://github.com/owner/repo/pull/42"]
        );
        assert_eq!(
            terminal.effective_work_context().repo.as_deref(),
            Some("owner/repo")
        );
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn dispatch_home_composer_applies_ticket_to_spawned_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut plan = fixed_home_dispatch_plan(crate::app::home::HomeTarget::NewSpace);
        plan.ticket = Some(crate::app::home::HomeTicketContext {
            identifier: "SCA-3165".into(),
            title: "image edit reference".into(),
            url: "https://linear.app/scalable/issue/SCA-3165".into(),
        });

        app.dispatch_home_composer(plan)
            .expect("ticket dispatch should create a pane");

        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.workspaces[0]
            .terminal_id(pane)
            .and_then(|terminal_id| app.state.terminals.get(terminal_id))
            .expect("spawned pane terminal");
        assert_eq!(terminal.effective_work_context().ticket_ids, ["SCA-3165"]);
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn dispatch_home_composer_applies_missive_url_to_spawned_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut plan = fixed_home_dispatch_plan(crate::app::home::HomeTarget::NewSpace);
        plan.missive = Some(crate::app::home::HomeMissiveContext {
            app_url: "missive://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            subject: "Billing question".into(),
        });

        app.dispatch_home_composer(plan)
            .expect("Missive dispatch should create a pane");

        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.workspaces[0]
            .terminal_id(pane)
            .and_then(|terminal_id| app.state.terminals.get(terminal_id))
            .expect("spawned pane terminal");
        assert_eq!(
            terminal.effective_work_context().missive_urls,
            ["https://mail.missiveapp.com/#inbox/conversations/sample"]
        );
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn home_dispatch_preserves_adversarial_identity_invariants() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state = crate::app::state::AppState::test_with_adversarial_identity_state();
        let workspace_id = app.state.workspaces[0].id.clone();
        let plan = crate::app::home::HomeDispatchPlan {
            agent: crate::detect::Agent::Codex,
            model: "gpt-5.3-codex".into(),
            effort: Some("high".into()),
            directory: std::env::temp_dir(),
            workspace: crate::app::home::HomeWorkspace::CurrentCheckout,
            git_ref: None,
            pr: None,
            ticket: None,
            missive: None,
            work_context_patch: crate::work_context::PaneWorkContextPatch::default(),
            target: crate::app::home::HomeTarget::Existing(workspace_id),
            prompt: "verify identity invariants".into(),
            argv: vec![crate::app::api::test_support::exiting_test_command().into()],
            env: Vec::new(),
            remote: None,
        };

        app.dispatch_home_composer(plan)
            .expect("adversarial dispatch should succeed");

        app.state.assert_invariants_for_test();
    }

    fn title_test_app() -> (App, crate::terminal::TerminalId) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        (app, terminal_id)
    }

    fn configure_title_test_terminal(
        app: &mut App,
        terminal_id: &crate::terminal::TerminalId,
        cwd: std::path::PathBuf,
        title: &str,
        work_title: Option<&str>,
    ) {
        let terminal = app.state.terminals.get_mut(terminal_id).unwrap();
        terminal.cwd = cwd;
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.set_terminal_title(Some(title.into()));
        if let Some(work_title) = work_title {
            terminal
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    work_title: Some(work_title.to_string()),
                    ..Default::default()
                })
                .unwrap();
        }
    }

    #[test]
    fn tab_info_label_uses_live_detected_agent_title() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.ensure_test_terminals();

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        terminal.set_terminal_title(Some("⠋ Add Subabe management token to Doppler".into()));

        assert_eq!(
            app.tab_info(0, 0).unwrap().label,
            "Add Subabe management token to Doppler"
        );
    }

    #[test]
    fn api_aggregates_keep_a_stale_human_blocker_blocked() {
        let (mut app, terminal_id) = title_test_app();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Codex),
            crate::detect::AgentState::Idle,
        );
        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Answer".into(),
            text: "Choose the release path".into(),
            blocking: true,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        terminal.supervisor_stale = true;
        terminal.stale_resolution = Some((crate::detect::AgentState::Unknown, true));

        assert_eq!(
            app.pane_info(0, pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
        assert_eq!(
            app.tab_info(0, 0).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
        assert_eq!(
            app.workspace_info(0).agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
    }

    #[test]
    fn tab_info_label_rejects_cwd_title_in_favor_of_work_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "herdr-test",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_rejects_composed_agent_and_cwd_titles_for_all_separators() {
        let _env_lock = crate::integration::integration_env_lock();
        let home = title_test_home();
        let cwd = home.join(".herdr-test");

        for separator in ['—', '–', '·', '|'] {
            let (mut app, terminal_id) = title_test_app();
            let title = format!("codex {separator} ~/.herdr-test");
            configure_title_test_terminal(
                &mut app,
                &terminal_id,
                cwd.clone(),
                &title,
                Some("Write Poem"),
            );

            let label = app.tab_info(0, 0).unwrap().label;
            assert_eq!(label, "Write Poem", "title: {title:?}");
            assert!(!label.contains("codex · codex"), "title: {title:?}");
        }
    }

    #[test]
    fn tab_info_label_strips_leading_agent_from_composed_title() {
        let _env_lock = crate::integration::integration_env_lock();
        let home = title_test_home();
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            home.join(".herdr-test"),
            "codex — Write Poem",
            Some("Fallback title"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_keeps_nonleading_agent_word_in_composed_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "Deploy — codex helper",
            Some("Fallback title"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Deploy — codex helper");
    }

    #[test]
    fn tab_info_label_rejects_composed_agent_and_cwd_title_case_insensitively() {
        let _env_lock = crate::integration::integration_env_lock();
        let home = title_test_home();
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            home.join(".herdr-test"),
            "CODEX — ~/.HERDR-TEST",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_rejects_empty_terminal_titles() {
        let _env_lock = crate::integration::integration_env_lock();
        let home = title_test_home();

        for title in ["", "   "] {
            let (mut app, terminal_id) = title_test_app();
            configure_title_test_terminal(
                &mut app,
                &terminal_id,
                home.join(".herdr-test"),
                title,
                Some("Write Poem"),
            );

            assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
        }
    }

    #[test]
    fn tab_info_label_keeps_distinct_terminal_title_ahead_of_work_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "Review the patch",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Review the patch");
    }

    #[test]
    fn tab_info_label_rejects_full_and_tilde_cwd_titles() {
        let _env_lock = crate::integration::integration_env_lock();
        let home = title_test_home();
        let cwd = home.join("Repos/herdr-test");
        let relative = cwd.strip_prefix(&home).unwrap().display().to_string();
        let expected = "Write Poem";

        let (mut full_app, full_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut full_app,
            &full_terminal_id,
            cwd.clone(),
            &cwd.display().to_string(),
            Some("Write Poem"),
        );
        assert_eq!(full_app.tab_info(0, 0).unwrap().label, expected);

        let (mut tilde_app, tilde_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut tilde_app,
            &tilde_terminal_id,
            cwd,
            &format!("~/{relative}"),
            Some("Write Poem"),
        );
        assert_eq!(tilde_app.tab_info(0, 0).unwrap().label, expected);
    }

    #[test]
    fn tab_info_label_rejects_cwd_path_fragment_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "Repos/herdr-test",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_keeps_manual_label_and_degrades_without_other_title() {
        let (mut manual_app, manual_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut manual_app,
            &manual_terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "herdr-test",
            Some("Write Poem"),
        );
        manual_app
            .state
            .terminals
            .get_mut(&manual_terminal_id)
            .unwrap()
            .set_manual_label("Pinned".into());
        assert_eq!(manual_app.tab_info(0, 0).unwrap().label, "Pinned");

        let (mut no_title_app, no_title_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut no_title_app,
            &no_title_terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "herdr-test",
            None,
        );
        assert_eq!(no_title_app.tab_info(0, 0).unwrap().label, "codex");
    }

    #[test]
    fn tab_info_label_uses_tab_number_for_plain_shell() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.ensure_test_terminals();

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_terminal_title(Some("/Users/example/herdr".into()));

        assert_eq!(app.tab_info(0, 0).unwrap().label, "1");
    }

    #[test]
    fn second_spawn_for_same_pr_is_history_without_displacing_owner() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("owner"), Workspace::test_new("review")];
        app.state.ensure_test_terminals();
        let terminal_ids = app
            .state
            .workspaces
            .iter()
            .map(|workspace| {
                let pane_id = workspace.tabs[0].root_pane;
                workspace.terminal_id(pane_id).cloned().unwrap()
            })
            .collect::<Vec<_>>();
        let requested = |pr_url: &str, role| crate::work_context::PaneWorkContext {
            pr_urls: vec![pr_url.into()],
            role: Some(role),
            active_owner: true,
            ..Default::default()
        };

        let first = app
            .prepare_spawn_work_context(Some(requested(
                "https://github.com/O/R/pull/42",
                crate::work_context::PaneWorkRole::Ship,
            )))
            .unwrap()
            .unwrap();
        App::bind_spawn_work_context(
            app.state.terminals.get_mut(&terminal_ids[0]).unwrap(),
            Some(first),
        );
        let second = app
            .prepare_spawn_work_context(Some(requested(
                "https://github.com/o/r/pull/42",
                crate::work_context::PaneWorkRole::Review,
            )))
            .unwrap()
            .unwrap();
        assert!(!second.active_owner);
        App::bind_spawn_work_context(
            app.state.terminals.get_mut(&terminal_ids[1]).unwrap(),
            Some(second),
        );

        let contexts = terminal_ids
            .iter()
            .map(|id| app.state.terminals[id].effective_work_context())
            .collect::<Vec<_>>();
        assert_eq!(
            contexts
                .iter()
                .filter(|context| context.active_owner)
                .count(),
            1
        );
        assert_eq!(
            contexts[0].role,
            Some(crate::work_context::PaneWorkRole::Ship)
        );
        assert!(contexts[0].active_owner);
        assert_eq!(
            contexts[1].role,
            Some(crate::work_context::PaneWorkRole::Review)
        );
        assert!(!contexts[1].active_owner);
    }
}
