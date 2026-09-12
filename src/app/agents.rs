use std::time::{Duration, Instant};

use bytes::Bytes;

use super::{terminal_targets::TerminalTargetError, App};
use crate::api::schema::AgentStartParams;

pub(crate) const DEFAULT_AGENT_START_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_AGENT_START_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const AGENT_START_SETTLE_DELAY: Duration = Duration::from_secs(3);
const INVALID_AGENT_TIMEOUT_MESSAGE: &str =
    "agent start timeout must be greater than 3000ms and at most 300000ms";
const INVALID_AGENT_NAME_MESSAGE: &str = "agent name must start with a lowercase letter and contain only lowercase letters, digits, '-' or '_' (1-32 characters)";

fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

impl App {
    pub(super) fn collect_agent_infos(&self) -> Vec<crate::api::schema::AgentInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.agent_info(ws_idx, pane_id))
                })
            })
            .collect()
    }

    pub(crate) fn reconcile_due_managed_agents(&mut self, now: Instant) -> bool {
        let due_terminal_ids = self
            .state
            .terminals
            .values()
            .filter(|terminal| {
                terminal
                    .next_managed_agent_deadline()
                    .is_some_and(|deadline| now >= deadline)
            })
            .map(|terminal| terminal.id.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for terminal_id in due_terminal_ids {
            changed |= self
                .state
                .terminals
                .get_mut(&terminal_id)
                .is_some_and(|terminal| terminal.reconcile_managed_agent_at(now, false));
        }
        if changed {
            self.state.mark_session_dirty();
        }
        changed
    }

    pub(super) fn reconcile_managed_agent_target(&mut self, target: &str) {
        let Ok(resolved) = self.resolve_agent_target(target) else {
            return;
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return;
        };
        let changed = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .is_some_and(|terminal| terminal.reconcile_managed_agent_at(Instant::now(), false));
        if changed {
            self.state.mark_session_dirty();
            self.schedule_session_save();
            self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
        }
    }

    pub(super) fn agent_info_for_target(
        &self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_agent_target(target)?;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn focus_agent_target(
        &mut self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_agent_target(target)?;
        self.state
            .focus_pane_in_workspace(resolved.ws_idx, resolved.pane_id);
        self.state.mark_active_tab_seen();
        self.state.mode = crate::app::Mode::Terminal;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn rename_agent_target(
        &mut self,
        target: &str,
        name: Option<String>,
    ) -> Result<crate::api::schema::AgentInfo, AgentRenameError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentRenameError::Target)?;
        let normalized_name = match name {
            Some(name) if valid_agent_name(&name) => Some(name),
            Some(_) => return Err(AgentRenameError::InvalidName),
            None => None,
        };

        if let Some(name) = normalized_name.as_deref() {
            let conflicts = self.agent_name_conflicts(name, &resolved.terminal_id);
            if !conflicts.is_empty() {
                return Err(AgentRenameError::DuplicateName {
                    name: name.to_string(),
                    candidates: conflicts,
                });
            }
        }

        let Some(terminal) = self
            .state
            .terminals
            .values_mut()
            .find(|terminal| terminal.id.to_string() == resolved.terminal_id)
        else {
            return Err(AgentRenameError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            }));
        };
        if terminal.managed_agent_launch_pending() {
            return Err(AgentRenameError::PendingLaunch);
        }
        if terminal.effective_agent_label().is_none() {
            return Err(AgentRenameError::NotAgent);
        }
        match normalized_name {
            Some(name) => terminal.set_agent_name(name),
            None => terminal.clear_agent_name(),
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| {
                AgentRenameError::Target(TerminalTargetError::NotFound {
                    target: target.to_string(),
                })
            })
    }

    pub(super) fn start_agent(
        &mut self,
        params: AgentStartParams,
    ) -> Result<(crate::api::schema::AgentInfo, Vec<String>), AgentStartError> {
        self.start_agent_with_registry(params, crate::agents::registry())
    }

    fn start_agent_with_registry(
        &mut self,
        params: AgentStartParams,
        registry: std::sync::Arc<crate::agents::RegistrySnapshot>,
    ) -> Result<(crate::api::schema::AgentInfo, Vec<String>), AgentStartError> {
        let name = params.name;
        if !valid_agent_name(&name) {
            return Err(AgentStartError::InvalidName);
        }
        let normalized_kind = params.kind.trim().to_ascii_lowercase();
        let Some(profile) = registry
            .profile_by_normalized_alias(&normalized_kind)
            .filter(|profile| profile.is_startable())
        else {
            return Err(AgentStartError::UnsupportedKind(params.kind));
        };
        if params
            .args
            .iter()
            .any(|arg| arg.chars().any(char::is_control))
        {
            return Err(AgentStartError::InvalidArgument);
        }
        let kind = profile.legacy_agent();
        let persisted_agent_session =
            crate::agent_resume::persisted_session_from_profile_launch_args(profile, &params.args);
        let pinned_recipe = crate::agent_resume::PinnedAgentResumeRecipe::capture(profile);
        let strict_input_readiness =
            crate::detect::manifest::requires_screen_visible_idle(&registry, kind);
        let conflicts = self.agent_name_conflicts(&name, "");
        if !conflicts.is_empty() {
            return Err(AgentStartError::DuplicateName {
                name,
                candidates: conflicts,
            });
        }
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return Err(AgentStartError::TargetNotFound(params.pane_id));
        };
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
            .ok_or_else(|| AgentStartError::TargetNotFound(params.pane_id.clone()))?;
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetNotFound(params.pane_id.clone()))?;
        if terminal.is_agent_terminal() || terminal.managed_agent_kind().is_some() {
            return Err(AgentStartError::TargetBusy(params.pane_id));
        }
        let runtime = self
            .terminal_runtimes
            .get(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetUnavailable(params.pane_id.clone()))?;
        let shell_name = available_shell_name(runtime)
            .ok_or_else(|| AgentStartError::TargetBusy(params.pane_id.clone()))?;

        let mut argv = vec![profile.launch().executable().to_string()];
        argv.extend(params.args);
        let command = crate::platform::interactive_shell_command(&argv, &shell_name)
            .ok_or(AgentStartError::InvalidArgument)?;
        let bytes = crate::app::api_helpers::encode_api_submission(runtime, &command);
        let timeout = Duration::from_millis(
            params
                .timeout_ms
                .unwrap_or(DEFAULT_AGENT_START_TIMEOUT.as_millis() as u64),
        );
        if timeout <= AGENT_START_SETTLE_DELAY || timeout > MAX_AGENT_START_TIMEOUT {
            return Err(AgentStartError::InvalidTimeout);
        }

        let now = Instant::now();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return Err(AgentStartError::InputFailed(err.to_string()));
        }
        let terminal = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetUnavailable(params.pane_id.clone()))?;
        terminal.begin_managed_agent_with_readiness(
            Some(name.clone()),
            kind,
            strict_input_readiness,
            now,
            AGENT_START_SETTLE_DELAY,
            timeout,
        );
        terminal.admit_agent_resume_recipe(
            kind,
            pinned_recipe.clone(),
            persisted_agent_session.clone(),
            now,
        );
        terminal.pinned_agent_resume_recipe = pinned_recipe;
        if let Some(session) = persisted_agent_session {
            terminal.set_managed_agent_launch_session(session);
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();

        let mut agent = self
            .agent_info(ws_idx, pane_id)
            .ok_or(AgentStartError::TargetUnavailable(params.pane_id))?;
        // Acknowledge the server-admitted identity without claiming live process detection.
        agent.agent = Some(kind.as_str().to_string());
        Ok((agent, argv))
    }

    pub(super) fn agent_start_error_body(
        &self,
        err: AgentStartError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentStartError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: INVALID_AGENT_NAME_MESSAGE.into(),
            },
            AgentStartError::UnsupportedKind(kind) => crate::api::schema::ErrorBody {
                code: "unsupported_agent_kind".into(),
                message: format!("unsupported interactive agent kind {kind}"),
            },
            AgentStartError::InvalidArgument => crate::api::schema::ErrorBody {
                code: "invalid_agent_argument".into(),
                message: "agent arguments cannot be encoded safely for the target shell".into(),
            },
            AgentStartError::InvalidTimeout => crate::api::schema::ErrorBody {
                code: "invalid_agent_timeout".into(),
                message: INVALID_AGENT_TIMEOUT_MESSAGE.into(),
            },
            AgentStartError::TargetNotFound(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_not_found".into(),
                message: format!("agent target pane {target} not found"),
            },
            AgentStartError::TargetBusy(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_busy".into(),
                message: format!("agent target pane {target} is not an available shell"),
            },
            AgentStartError::TargetUnavailable(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_unavailable".into(),
                message: format!("agent target pane {target} has no live terminal"),
            },
            AgentStartError::InputFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_start_input_failed".into(),
                message,
            },
            AgentStartError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_target_error_body(
        &self,
        err: TerminalTargetError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            TerminalTargetError::NotFound { target } => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: format!("agent target {target} not found"),
            },
            TerminalTargetError::Ambiguous { target, candidates } => {
                crate::api::schema::ErrorBody {
                    code: "agent_target_ambiguous".into(),
                    message: format!(
                        "agent target {target} is ambiguous; candidates: {}",
                        candidates
                            .into_iter()
                            .map(|candidate| format!(
                                "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                                candidate.terminal_id,
                                candidate.pane_id,
                                candidate.workspace_id,
                                candidate.tab_id,
                                candidate.cwd.unwrap_or_else(|| "unknown".into()),
                                candidate.agent_status,
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                }
            }
        }
    }

    pub(super) fn agent_rename_error_body(
        &self,
        err: AgentRenameError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentRenameError::Target(err) => self.agent_target_error_body(err),
            AgentRenameError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: INVALID_AGENT_NAME_MESSAGE.into(),
            },
            AgentRenameError::NotAgent => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: "agent target does not currently host an agent".into(),
            },
            AgentRenameError::PendingLaunch => crate::api::schema::ErrorBody {
                code: "agent_launch_pending".into(),
                message: "agent name cannot change while startup is pending".into(),
            },
            AgentRenameError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::AgentInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_state = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane_state.attached_terminal_id)?;
        if !terminal.is_agent_terminal() {
            return None;
        }
        let pane = self.pane_info(ws_idx, pane_id)?;
        Some(crate::api::schema::AgentInfo {
            terminal_id: pane.terminal_id,
            name: terminal.agent_name.clone(),
            agent: pane.agent,
            title: pane.title,
            terminal_title: pane.terminal_title,
            terminal_title_stripped: pane.terminal_title_stripped,
            display_agent: pane.display_agent,
            agent_status: pane.agent_status,
            screen_detection_skipped: terminal.full_lifecycle_hook_authority_active()
                && !terminal.screen_detection_required_for_managed_startup(),
            state_labels: pane.state_labels,
            tokens: pane.tokens,
            agent_session: pane.agent_session,
            workspace_id: pane.workspace_id,
            tab_id: pane.tab_id,
            pane_id: pane.pane_id,
            focused: pane.focused,
            launch_pending: terminal.managed_agent_launch_pending(),
            interactive_ready: terminal.managed_agent_interactive_ready(),
            state_change_seq: terminal.last_agent_state_change_seq.unwrap_or(0),
            cwd: pane.cwd,
            foreground_cwd: pane.foreground_cwd,
            revision: pane.revision,
        })
    }

    fn agent_name_conflicts(
        &self,
        name: &str,
        except_terminal_id: &str,
    ) -> Vec<crate::api::schema::AgentInfo> {
        self.collect_agent_infos()
            .into_iter()
            .filter(|agent| {
                agent.name.as_deref() == Some(name) && agent.terminal_id != except_terminal_id
            })
            .collect()
    }
}

fn available_shell_name(runtime: &crate::terminal::TerminalRuntime) -> Option<String> {
    #[cfg(test)]
    if runtime.child_pid().is_none() {
        return Some("sh".into());
    }
    crate::platform::available_pane_shell(runtime.child_pid()?)
}

pub(super) fn runtime_hosts_agent(
    runtime: &crate::terminal::TerminalRuntime,
    expected: crate::detect::Agent,
    binding: Option<&crate::agent_resume::LiveAgentResumeBinding>,
) -> bool {
    #[cfg(test)]
    if runtime.child_pid().is_none() {
        return true;
    }
    if let Some(binding) = binding.filter(|binding| binding.agent == expected) {
        if let Some((_, process)) = &binding.process {
            let Some(pid) = runtime.child_pid() else {
                return false;
            };
            let registry = crate::agents::registry();
            return crate::platform::foreground_job_with_registry(
                &registry,
                pid,
                Some((process, &registry)),
            )
            .is_some_and(|job| {
                retained_binding_in_foreground(binding, &job, crate::platform::process_identity)
            });
        }
    }
    live_runtime_agent(runtime) == Some(expected)
}

fn retained_binding_in_foreground(
    binding: &crate::agent_resume::LiveAgentResumeBinding,
    job: &crate::platform::ForegroundJob,
    identity: impl FnOnce(u32) -> Option<crate::platform::ProcessIdentity>,
) -> bool {
    let Some((process_group_id, process)) = &binding.process else {
        return false;
    };
    let Some(expected_identity) = binding.process_identity else {
        return false;
    };
    job.process_group_id == *process_group_id
        && job.processes.iter().any(|member| member.pid == process.pid)
        && identity(process.pid) == Some(expected_identity)
}

fn live_runtime_agent(runtime: &crate::terminal::TerminalRuntime) -> Option<crate::detect::Agent> {
    let job = crate::detect::foreground_job(runtime.child_pid()?)?;
    crate::detect::identify_agent_in_job(&job)
        .map(|(agent, _)| agent)
        .or_else(|| {
            job.processes
                .iter()
                .find_map(|process| crate::platform::process_agent_hint(process.pid))
        })
}

pub(super) enum AgentStartError {
    InvalidName,
    UnsupportedKind(String),
    InvalidArgument,
    InvalidTimeout,
    TargetNotFound(String),
    TargetBusy(String),
    TargetUnavailable(String),
    InputFailed(String),
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

pub(super) enum AgentRenameError {
    Target(TerminalTargetError),
    InvalidName,
    NotAgent,
    PendingLaunch,
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

#[cfg(test)]
mod tests {
    use super::valid_agent_name;

    #[tokio::test]
    async fn dynamic_launch_retains_registry_identity_argv_and_managed_ownership() {
        use super::*;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("dynamic")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let (runtime, mut input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let registry = crate::agent_resume::test_registry(
            "novel-42",
            "shared-cli",
            "separate_flag",
            "--session",
        );
        let params = AgentStartParams {
            name: "reviewer".into(),
            kind: " Novel Alias ".into(),
            pane_id: app.public_pane_id(0, pane_id).unwrap(),
            args: vec!["--session".into(), "native-id".into()],
            timeout_ms: None,
        };
        let (info, argv) = app
            .start_agent_with_registry(params.clone(), registry.clone())
            .unwrap_or_else(|_| panic!("dynamic start failed"));
        assert_eq!(argv, ["shared-cli", "--session", "native-id"]);
        assert!(info.launch_pending);
        assert_eq!(info.agent.as_deref(), Some("novel-42"));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.detected_agent.is_none());
        assert_eq!(terminal.managed_agent_kind().unwrap().as_str(), "novel-42");
        assert!(terminal.hook_authority.is_none());
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().unwrap().source,
            "herdr:launch"
        );
        assert_eq!(
            terminal
                .pinned_agent_resume_recipe
                .as_ref()
                .unwrap()
                .executable,
            "shared-cli"
        );
        let runtime = app.terminal_runtimes.get(&terminal_id).unwrap();
        let expected_argv = vec!["shared-cli".into(), "--session".into(), "native-id".into()];
        let expected_command = crate::platform::interactive_shell_command(
            &expected_argv,
            &available_shell_name(runtime).unwrap(),
        )
        .unwrap();
        let expected_input =
            crate::app::api_helpers::encode_api_submission(runtime, &expected_command);
        assert_eq!(
            input.try_recv().unwrap().as_ref(),
            expected_input.as_slice()
        );
        // Failed repeated launch cannot steal the existing owner or enqueue input.
        assert!(app.start_agent_with_registry(params, registry).is_err());
        assert!(input.try_recv().is_err());
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.terminals.len(), 1);
        assert_eq!(
            app.state.terminals[&terminal_id].agent_name.as_deref(),
            Some("reviewer")
        );
    }

    #[tokio::test]
    async fn normal_launch_pins_strict_readiness_and_starts_pending_only_after_input_enqueue() {
        use super::*;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("strict")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let (runtime, mut input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let registry = crate::agents::store::snapshot_for_test(
            vec![
                (
                    "agents/strict-launch/agent.toml".into(),
                    "schema = 1\nid = 'strict-launch'\nname = 'strict-launch'\naliases = []\nstartable = true\n[launch]\nunix = 'strict-launch'\nwindows = 'strict-launch'\n".into(),
                ),
                (
                    "agents/strict-launch/process.toml".into(),
                    "names = ['strict-launch']\n".into(),
                ),
                (
                    "agents/strict-launch/detection.toml".into(),
                    "id = 'strict-launch'\nversion = '2026.09.05.1'\nmin_engine_version = 1\n[[rules]]\nid = 'idle'\nstate = 'idle'\npriority = 10\nregion = 'bottom_lines(4)'\nvisible_idle = true\ncontains = ['ready']\n".into(),
                ),
            ],
            44,
        )
        .unwrap();
        app.start_agent_with_registry(
            AgentStartParams {
                name: "reviewer".into(),
                kind: "strict-launch".into(),
                pane_id: app.public_pane_id(0, pane_id).unwrap(),
                args: Vec::new(),
                timeout_ms: None,
            },
            registry,
        )
        .unwrap_or_else(|_| panic!("strict launch failed"));
        assert!(
            input.try_recv().is_ok(),
            "command must be enqueued before Pending is installed"
        );
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.managed_agent_launch_pending());
        assert!(terminal.screen_detection_required_for_managed_startup());
        assert!(!terminal.managed_agent_interactive_ready());

        // Active registry changes cannot downgrade an admitted startup's pinned policy.
        let _downgraded = crate::agent_resume::test_registry(
            "strict-launch",
            "strict-launch",
            "subcommand",
            "resume",
        );
        assert!(app.state.terminals[&terminal_id].screen_detection_required_for_managed_startup());
    }

    #[tokio::test]
    async fn failed_command_injection_never_starts_managed_pending() {
        use super::*;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("failed-input")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let (runtime, input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        drop(input);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let result = app.start_agent_with_registry(
            AgentStartParams {
                name: "reviewer".into(),
                kind: "novel-42".into(),
                pane_id: app.public_pane_id(0, pane_id).unwrap(),
                args: Vec::new(),
                timeout_ms: None,
            },
            crate::agent_resume::test_registry("novel-42", "shared-cli", "subcommand", "resume"),
        );
        assert!(matches!(result, Err(AgentStartError::InputFailed(_))));
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.agent_name, None);
        assert_eq!(terminal.managed_agent_kind(), None);
        assert!(!terminal.managed_agent_launch_pending());
        assert_eq!(terminal.next_managed_agent_deadline(), None);
    }

    #[test]
    fn acquired_resume_recipe_is_protected_across_adversarial_identity_state_and_reload() {
        let mut state = crate::app::AppState::test_with_adversarial_identity_state();
        state.assert_invariants_for_test();
        let pane_id = state.workspaces[0].tabs[state.workspaces[0].active_tab].root_pane;
        let terminal_id = state.workspaces[0].terminal_id(pane_id).unwrap().clone();
        let original =
            crate::agent_resume::test_registry("codex", "old-cli", "subcommand", "resume");
        let changed =
            crate::agent_resume::test_registry("codex", "new-cli", "subcommand", "continue");
        let recipe = original
            .profile_by_id("codex")
            .and_then(crate::agent_resume::PinnedAgentResumeRecipe::capture)
            .unwrap();
        let now = std::time::Instant::now();
        let binding = crate::agent_resume::LiveAgentResumeBinding {
            agent: crate::detect::Agent::Codex,
            recipe: Some(recipe.clone()),
            process: Some((
                100,
                crate::platform::ForegroundProcess {
                    pid: 100,
                    name: "old-cli".into(),
                    argv0: None,
                    argv: None,
                    cmdline: None,
                },
            )),
            process_identity: Some(crate::platform::ProcessIdentity {
                pid: 100,
                birth_token: 1,
            }),
            observed_at: now,
            managed_admission: false,
            report_proof: None,
            resume_options_owner: None,
            resume_options: None,
        };
        state.handle_app_event(crate::events::AppEvent::AgentResumeProcessBound {
            pane_id,
            binding: Box::new(binding.clone()),
        });
        state.handle_app_event(crate::events::AppEvent::AgentProcessDetected {
            pane_id,
            agent: crate::detect::Agent::Codex,
            observed_at: now,
        });
        let mut refreshed = binding;
        refreshed.recipe = changed
            .profile_by_id("codex")
            .and_then(crate::agent_resume::PinnedAgentResumeRecipe::capture);
        refreshed.observed_at += std::time::Duration::from_millis(1);
        state.handle_app_event(crate::events::AppEvent::AgentResumeProcessBound {
            pane_id,
            binding: Box::new(refreshed),
        });
        state.handle_app_event(crate::events::AppEvent::AgentSessionReported {
            pane_id,
            source: "herdr:codex".into(),
            agent_label: "codex".into(),
            seq: None,
            session_ref: crate::agent_resume::AgentSessionRef::id("session"),
            session_start_source: Some("startup".into()),
        });
        state.assert_invariants_for_test();
        let terminal = &state.terminals[&terminal_id];
        assert_eq!(terminal.pinned_agent_resume_recipe.as_ref(), Some(&recipe));
        assert!(crate::agent_resume::pinned_plan(
            &changed,
            terminal.persisted_agent_session.as_ref().unwrap(),
            terminal.pinned_agent_resume_recipe.as_ref()
        )
        .is_err());
    }

    #[test]
    fn internal_launch_session_is_not_replaceable_by_report_events() {
        let mut state = crate::app::AppState::test_with_adversarial_identity_state();
        state.assert_invariants_for_test();
        let pane_id = state.workspaces[0].tabs[state.workspaces[0].active_tab].root_pane;
        let terminal_id = state.workspaces[0].terminal_id(pane_id).unwrap().clone();
        let registry = crate::agent_resume::test_registry(
            "novel-42",
            "shared-cli",
            "separate_flag",
            "--session",
        );
        let profile = registry.profile_by_id("novel-42").unwrap();
        let captured = crate::agent_resume::persisted_session_from_profile_launch_args(
            profile,
            &["--session".into(), "real-id".into()],
        )
        .unwrap();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.restore_managed_agent("reviewer".into(), profile.legacy_agent());
        terminal.set_persisted_agent_session(captured.clone());
        terminal.pinned_agent_resume_recipe =
            crate::agent_resume::PinnedAgentResumeRecipe::capture(profile);
        let pinned = terminal.pinned_agent_resume_recipe.clone();
        for event in [
            crate::events::AppEvent::AgentSessionReported {
                pane_id,
                source: "herdr:launch".into(),
                agent_label: "novel-42".into(),
                seq: Some(99),
                session_ref: crate::agent_resume::AgentSessionRef::id("forged-id"),
                session_start_source: Some("new".into()),
            },
            crate::events::AppEvent::HookStateReported {
                pane_id,
                source: "herdr:launch".into(),
                agent_label: "novel-42".into(),
                state: crate::detect::AgentState::Working,
                message: None,
                seq: Some(100),
                session_ref: crate::agent_resume::AgentSessionRef::id("forged-id"),
            },
        ] {
            assert!(state.handle_app_event(event).is_empty());
            state.assert_invariants_for_test();
            let terminal = &state.terminals[&terminal_id];
            assert_eq!(terminal.persisted_agent_session.as_ref(), Some(&captured));
            assert_eq!(terminal.pinned_agent_resume_recipe, pinned);
            assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
            assert!(terminal.hook_authority.is_none());
            assert!(!terminal.full_lifecycle_hook_authority_active());
        }
    }

    #[tokio::test]
    async fn dynamic_launch_rejection_does_not_mutate_app_state_or_write_input() {
        use super::*;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("dynamic")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let (runtime, mut input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let registry = crate::agent_resume::test_registry(
            "novel-42",
            "shared-cli",
            "separate_flag",
            "--session",
        );
        for (kind, args, timeout_ms) in [
            ("absent", vec![], None),
            ("novel-42", vec!["bad\nargument".into()], None),
            ("novel-42", vec![], Some(1)),
        ] {
            let params = AgentStartParams {
                name: "reviewer".into(),
                kind: kind.into(),
                pane_id: app.public_pane_id(0, pane_id).unwrap(),
                args,
                timeout_ms,
            };
            assert!(app
                .start_agent_with_registry(params, registry.clone())
                .is_err());
            let terminal = &app.state.terminals[&terminal_id];
            assert!(terminal.agent_name.is_none());
            assert!(terminal.managed_agent_kind().is_none());
            assert!(terminal.persisted_agent_session.is_none());
            assert!(terminal.pinned_agent_resume_recipe.is_none());
            assert!(input.try_recv().is_err());
            assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), 1);
        }
    }

    #[test]
    fn retained_prompt_identity_requires_foreground_membership_and_birth_not_mutable_argv() {
        let process = crate::platform::ForegroundProcess {
            pid: 100,
            name: "old-cli".into(),
            argv0: None,
            argv: None,
            cmdline: None,
        };
        let identity = crate::platform::ProcessIdentity {
            pid: 100,
            birth_token: 1,
        };
        let binding = crate::agent_resume::LiveAgentResumeBinding {
            agent: crate::detect::Agent::parse("removed-agent").unwrap(),
            recipe: None,
            process: Some((100, process.clone())),
            process_identity: Some(identity),
            observed_at: std::time::Instant::now(),
            managed_admission: false,
            report_proof: None,
            resume_options_owner: None,
            resume_options: None,
        };
        let mut job = crate::platform::ForegroundJob {
            process_group_id: 100,
            processes: vec![process],
        };
        job.processes[0].name = "mutable-title".into();
        job.processes[0].argv = Some(vec!["different presentation".into()]);
        assert!(super::retained_binding_in_foreground(
            &binding,
            &job,
            |_| Some(identity)
        ));
        assert!(!super::retained_binding_in_foreground(
            &binding,
            &job,
            |_| Some(crate::platform::ProcessIdentity {
                birth_token: 2,
                ..identity
            })
        ));
        assert!(!super::retained_binding_in_foreground(
            &binding,
            &job,
            |_| None
        ));
        job.process_group_id = 101;
        assert!(!super::retained_binding_in_foreground(
            &binding,
            &job,
            |_| Some(identity)
        ));
    }

    #[test]
    fn agent_names_use_a_small_cli_safe_grammar() {
        for name in ["a", "reviewer-one", "reviewer_2", &"a".repeat(32)] {
            assert!(valid_agent_name(name), "expected {name:?} to be valid");
        }
        for name in [
            "",
            " reviewer",
            "reviewer ",
            "reviewer one",
            "Reviewer",
            "1reviewer",
            "reviewer.one",
            &"a".repeat(33),
        ] {
            assert!(!valid_agent_name(name), "expected {name:?} to be invalid");
        }
    }
}
