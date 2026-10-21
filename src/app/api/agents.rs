use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{
    AgentArchiveParams, AgentForgetParams, AgentListParams, AgentPromptDelivery, AgentPromptParams,
    AgentRenameParams, AgentRestartParams, AgentSendKeysParams, AgentStartParams, AgentTarget,
    AgentUnarchiveParams, PaneReadResult, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

const AGENT_PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);
fn input_kind_suffix(kind: Option<crate::detect::InputPromptKind>) -> &'static str {
    match kind {
        Some(crate::detect::InputPromptKind::Confirm) => " (confirm)",
        Some(crate::detect::InputPromptKind::Select) => " (select)",
        Some(crate::detect::InputPromptKind::FreeText) => " (free text)",
        Some(crate::detect::InputPromptKind::Unknown) | None => "",
    }
}

// Codex's Windows input reader does not surface bracketed paste. It detects the prompt as a
// "paste burst" and, while that burst is buffered, rewrites a following Enter into a newline
// instead of submitting. The burst only flushes after an idle timeout, so any size-based delay is
// a timing guess that fails when ConPTY delivery lags it. Codex flushes a buffered burst
// synchronously when it receives a non-character key, so appending one after the paste gives the
// submission a deterministic paste boundary regardless of prompt size or delivery speed.
#[cfg(windows)]
fn append_codex_paste_boundary(runtime: &crate::terminal::TerminalRuntime, text: &mut Vec<u8>) {
    let keys = match crate::app::api_helpers::encode_api_keys(runtime, &["right".to_string()]) {
        Ok(keys) => keys,
        Err(key) => {
            tracing::warn!(key = %key, "failed to encode Codex paste boundary key");
            return;
        }
    };
    if let Some(key) = keys.into_iter().find(|bytes| !bytes.is_empty()) {
        text.extend_from_slice(&key);
    }
}

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String, params: AgentListParams) -> String {
        // Local agents first. Ordinary callers also receive the coordinator's
        // cached remote directory, while federation pollers explicitly request
        // local-only state so peers cannot recursively re-export aggregates.
        let mut agents = self.collect_agent_infos();
        if !params.local_only {
            let store = self
                .federation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            agents.extend(store.merged_agents());
        }
        encode_success(
            id,
            ResponseResult::AgentList {
                agents,
                origin_machine_id: Some(crate::persist::machine::get_or_create()),
                origin_boot_id: Some(crate::api::server_boot_id().to_owned()),
                origin_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                origin_protocol: Some(crate::protocol::PROTOCOL_VERSION),
                origin_capabilities: crate::api::default_capabilities(),
            },
        )
    }

    /// Internal guest-gate lookup. `running` is the same live-agent check
    /// `agent.prompt` applies: a known agent, no launch pending, and the agent
    /// still the pane's foreground process.
    #[cfg(unix)]
    pub(super) fn handle_guest_agent_probe(
        &mut self,
        id: String,
        params: crate::api::schema::GuestAgentProbeParams,
    ) -> String {
        let label = params
            .terminal_id
            .as_deref()
            .or(params.target.as_deref())
            .unwrap_or_default()
            .to_string();
        let resolved = match (&params.terminal_id, &params.target) {
            (Some(terminal_id), _) => self.terminal_target_for_terminal_id(terminal_id),
            (None, Some(target)) => self.resolve_agent_target(target).ok(),
            (None, None) => None,
        };
        let Some(resolved) = resolved else {
            return agent_not_found(id, &label);
        };
        let Some(pane_id) = self.public_pane_id(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &label);
        };
        let agent = self.agent_info(resolved.ws_idx, resolved.pane_id);
        let running =
            agent.is_some() && self.agent_pane_runs_agent(resolved.ws_idx, resolved.pane_id);
        encode_success(
            id,
            ResponseResult::GuestAgentProbed {
                pane_id,
                agent,
                running,
            },
        )
    }

    #[cfg(unix)]
    fn agent_pane_runs_agent(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> bool {
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
        else {
            return false;
        };
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return false;
        };
        if terminal.managed_agent_launch_pending() {
            return false;
        }
        self.lookup_runtime_sender(ws_idx, pane_id)
            .is_some_and(|runtime| {
                super::super::agents::runtime_hosts_agent(runtime, expected_agent)
            })
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        self.reconcile_managed_agent_target(&target.target);
        let agent = match self.agent_info_for_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    /// `agent.archive` — take an agent out of active rotation, preserving its
    /// session (issue #173).
    pub(super) fn handle_agent_archive(
        &mut self,
        id: String,
        params: AgentArchiveParams,
    ) -> String {
        let by = params.by.unwrap_or_else(|| "api".to_string());
        let at = now_rfc3339();
        match self.archive_agent_target(
            &params.target,
            params.reason,
            by,
            params.parked_work,
            params.force,
            at,
        ) {
            Ok(agent) => encode_success(id, ResponseResult::AgentInfo { agent }),
            Err(err) => encode_error_body(id, self.agent_archive_error_body(err)),
        }
    }

    /// `agent.unarchive` — resume a previously archived agent (issue #173).
    pub(super) fn handle_agent_unarchive(
        &mut self,
        id: String,
        params: AgentUnarchiveParams,
    ) -> String {
        match self.unarchive_agent_target(&params.target, params.fresh) {
            Ok(agent) => encode_success(id, ResponseResult::AgentInfo { agent }),
            Err(err) => encode_error_body(id, self.agent_unarchive_error_body(err)),
        }
    }

    /// `agent.forget` — drop an archived agent's record (issue #291).
    pub(super) fn handle_agent_forget(&mut self, id: String, params: AgentForgetParams) -> String {
        match self.forget_archived_agent(&params.target) {
            Ok(agent) => encode_success(id, ResponseResult::AgentInfo { agent }),
            Err(err) => encode_error_body(id, self.agent_forget_error_body(err)),
        }
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let (agent, argv) = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(crate) fn handle_deferred_agent_api_request(
        &mut self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        let crate::api::schema::Method::AgentPrompt(params) = request.method else {
            return false;
        };
        let validated = match self.validate_agent_prompt(request.id, &params) {
            Ok(validated) => validated,
            Err(response) => {
                let _ = respond_to.send(response);
                return true;
            }
        };
        // Start the completion waiter before submitting, so a refused thread
        // fails the request while nothing has been sent yet.
        let (handoff_tx, handoff_rx) = std::sync::mpsc::channel::<(
            String,
            crate::api::schema::AgentInfo,
            std::sync::mpsc::Receiver<std::io::Result<()>>,
        )>();
        let waiter_respond_to = respond_to.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-agent-prompt", move || {
            let Ok((id, agent, completion)) = handoff_rx.recv() else {
                return;
            };
            let response = match completion.recv() {
                Ok(Ok(())) => encode_success(
                    id,
                    ResponseResult::AgentPrompted {
                        agent,
                        delivery: Some(AgentPromptDelivery::WrittenToPty),
                    },
                ),
                Ok(Err(err)) if err.kind() == std::io::ErrorKind::TimedOut => {
                    encode_error(id, "timeout", err.to_string())
                }
                Ok(Err(err)) => encode_error(id, "agent_prompt_failed", err.to_string()),
                Err(_) => encode_error(id, "agent_prompt_failed", "pty actor closed"),
            };
            let _ = waiter_respond_to.send(response);
        });
        if let Err(err) = spawned {
            tracing::warn!(err = %err, "failed to spawn agent prompt thread");
            let _ = respond_to.send(encode_error(
                validated.id,
                "agent_prompt_failed",
                format!("could not start prompt completion waiter: {err}"),
            ));
            return true;
        }
        match self.submit_agent_prompt(validated, &params) {
            Ok(submission) => {
                let _ = handoff_tx.send(submission);
            }
            Err(response) => {
                let _ = respond_to.send(response);
            }
        }
        true
    }

    /// Restart an agent in place: kill its harness process and reopen the same
    /// session with `--resume`, keeping the pane. The resume plan is built from
    /// the LIVE session identity already resident on the terminal (no
    /// session.json read). The kill fires exactly one `PaneDied`, which the
    /// resume-aware respawn path turns into a single `--resume` relaunch — no
    /// double-spawn, pane + agent identity preserved. Errors when the agent has
    /// no resumable session (not a herdr-launched agent, or none reported).
    pub(super) fn handle_agent_restart(
        &mut self,
        id: String,
        params: AgentRestartParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return agent_not_found(id, &params.target);
        };

        let Some((source, agent, session_ref)) = self
            .state
            .terminals
            .get(&terminal_id)
            .and_then(Self::terminal_resume_source)
        else {
            return encode_error(
                id,
                "no_resumable_session",
                "agent has no resumable session — not a herdr-launched agent, or no session was reported",
            );
        };
        let Some(plan) = crate::agent_resume::plan(&source, &agent, &session_ref) else {
            return encode_error(
                id,
                "no_resumable_session",
                "agent has no resumable session for its harness",
            );
        };

        // Resolve which account to resume under: an explicit `account` param is a
        // swap; absent, default to the account this agent already runs under so a
        // plain restart stays on the same subscription.
        let remembered_account = self
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.agent_account.clone());
        let selected_account = params.account.clone().or(remembered_account.clone());
        let account_env = match selected_account.as_deref() {
            Some(account_id) => match self.resolve_account_launch_env(account_id, &agent) {
                Ok(env) => Some((account_id.to_string(), env)),
                Err(err) => return encode_error_body(id, err.into_error_body()),
            },
            None => None,
        };

        // FAIL CLOSED on a logged-out target: when this restart swaps to a DIFFERENT account, verify that
        // account actually holds credentials BEFORE we tear down the running process. A logged-out profile
        // (its `.credentials.json` deleted, e.g. by a `/logout`) would otherwise strand the seat at a login
        // screen once we kill it (gitmoot workflow-note row 86147). Only claude has this credential layout
        // today; other kinds skip the check. Checked here rather than by spawning `claude auth status` so the
        // request handler never blocks on a subprocess.
        if params.account.is_some() {
            if let Some(account_id) = selected_account.as_deref() {
                if let Some(account) = self
                    .loaded_accounts
                    .iter()
                    .find(|account| account.id == account_id)
                {
                    if account.kind == "claude" {
                        // The cwd the replacement will resume in — trust is per directory,
                        // so it can only be judged against this.
                        let cwd = self
                            .state
                            .terminals
                            .get(&terminal_id)
                            .map(|terminal| terminal.cwd.to_string_lossy().into_owned());
                        if let Some(blocker) =
                            claude_account_launch_blocker(&account.config_dir, cwd.as_deref())
                        {
                            return encode_error(id, blocker.code(), blocker.message(account_id));
                        }
                    }
                }
            }
        }

        // SWAP SAFETY: when this restart moves the agent to a DIFFERENT account, its session transcript
        // only exists under the CURRENT account's config-home. Copy it into the target's first, or the
        // `--resume` under the new account can't find it and the seat breaks into a dead state (this
        // silently killed a coordinator). On any failure FAIL-LOUD: return an error and leave the agent
        // exactly as it is — never arm the resume / kill the runtime into a broken swap.
        if params.account.is_some() {
            if let Err(reason) = migrate_session_for_account_swap(
                &self.loaded_accounts,
                &agent,
                &session_ref,
                remembered_account.as_deref(),
                selected_account.as_deref(),
            ) {
                return encode_error(
                    id,
                    "session_migrate_failed",
                    format!(
                        "couldn't move this agent's session to the new account: {reason}; the agent was left on its current account"
                    ),
                );
            }
        }

        // Snapshot the agent info to return before the process is killed.
        let agent_info = match self.agent_info_for_target(&params.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        // Arm the resume plan and guarantee the imminent PaneDied respawns the
        // pane (rather than closing it), then kill the process. The single
        // PaneDied drives one resume relaunch via the resume-aware respawn path.
        // The account env (if any) rides on `pending_launch_env`, read at that
        // relaunch's fresh-shell spawn; `agent_account` remembers the choice so a
        // later plain restart keeps it.
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.pending_agent_resume_plan = Some(plan);
            terminal.respawn_shell_on_exit = true;
            match account_env {
                Some((account_id, env)) => {
                    // Store the override vars only. The clear-list is re-derived at resume:
                    // for a secondary account from the config-home key, and for a primary
                    // account (empty vars) from `agent_account` via the registry.
                    terminal.pending_launch_env = env.vars;
                    terminal.agent_account = Some(account_id);
                }
                None => terminal.pending_launch_env.clear(),
            }
        }
        self.shutdown_terminal_runtime(terminal_id);
        encode_success(id, ResponseResult::AgentInfo { agent: agent_info })
    }

    /// The `(source, agent, session_ref)` needed to resume a terminal's agent,
    /// from the live hook authority, else the persisted session.
    pub(in crate::app) fn terminal_resume_source(
        terminal: &crate::terminal::TerminalState,
    ) -> Option<(String, String, crate::agent_resume::AgentSessionRef)> {
        if let Some(authority) = terminal.hook_authority.as_ref() {
            if let Some(session_ref) = authority.session_ref.as_ref() {
                return Some((
                    authority.source.clone(),
                    authority.agent_label.clone(),
                    session_ref.clone(),
                ));
            }
        }
        terminal.persisted_agent_session.as_ref().map(|session| {
            (
                session.source.clone(),
                session.agent.clone(),
                session.session_ref.clone(),
            )
        })
    }

    /// Runs every check that can reject a prompt without touching the pane.
    fn validate_agent_prompt(
        &self,
        id: String,
        params: &AgentPromptParams,
    ) -> Result<ValidatedAgentPrompt, String> {
        if params.text.is_empty() {
            return Err(encode_error(
                id,
                "empty_agent_prompt",
                "agent prompt must not be empty",
            ));
        }
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return Err(encode_error_body(id, self.agent_target_error_body(err))),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return Err(agent_not_found(id, &params.target));
        };
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if terminal.state == crate::detect::AgentState::Blocked {
            return Err(encode_error(
                id,
                "agent_blocked",
                format!(
                    "agent {} is blocked and requires interactive input",
                    params.target
                ),
            ));
        }
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return Err(agent_not_ready(id, &params.target));
        };
        if terminal.managed_agent_launch_pending() {
            return Err(agent_not_ready(id, &params.target));
        }
        if terminal.input_pending {
            return Err(encode_error(
                id,
                "agent_input_pending",
                format!(
                    "agent input is pending{}; answer it before prompting again",
                    input_kind_suffix(terminal.input_prompt_kind)
                ),
            ));
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return Err(encode_error(
                id,
                "agent_not_ready",
                format!(
                    "agent {} is no longer the pane foreground process",
                    params.target
                ),
            ));
        }
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        Ok(ValidatedAgentPrompt {
            id,
            ws_idx: resolved.ws_idx,
            pane_id: resolved.pane_id,
            expected_agent,
            agent,
        })
    }

    fn submit_agent_prompt(
        &mut self,
        validated: ValidatedAgentPrompt,
        params: &AgentPromptParams,
    ) -> Result<
        (
            String,
            crate::api::schema::AgentInfo,
            std::sync::mpsc::Receiver<std::io::Result<()>>,
        ),
        String,
    > {
        let ValidatedAgentPrompt {
            id,
            ws_idx,
            pane_id,
            expected_agent,
            agent,
        } = validated;
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        #[cfg(windows)]
        let submit_deadline = params
            .wait
            .as_ref()
            .and_then(|wait| wait.submission_deadline);
        #[cfg(not(windows))]
        let submit_deadline = None;
        if expected_agent == crate::detect::Agent::GithubCopilot {
            // Copilot ignores synthetic Enter after focus loss until it receives focus gained.
            let focus = match crate::ghostty::encode_focus(crate::ghostty::FocusEvent::Gained) {
                Ok(focus) => focus,
                Err(err) => {
                    return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
                }
            };
            if let Err(err) = runtime.try_send_bytes(Bytes::from(focus)) {
                return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
            }
        }
        // Bind the occupant baseline HERE, before a single byte is written.
        // Capturing it after the blocking acknowledgement would adopt whatever
        // occupies the pane by then as the expected occupant.
        let expected_group = super::super::agents::capture_occupant_group(runtime);
        let (text, enter) =
            crate::app::api_helpers::encode_api_submission_parts(runtime, &params.text);
        #[cfg(windows)]
        let text = if expected_agent == crate::detect::Agent::Codex {
            let mut text = text;
            append_codex_paste_boundary(runtime, &mut text);
            text
        } else {
            text
        };
        let composer_baseline = runtime.detection_content_seq();
        let submit_watch = std::sync::Arc::new(crate::terminal::PromptSubmitWatch::default());
        let guard = crate::pty::actor::SubmissionGuard {
            occupant_unchanged: std::sync::Arc::new(super::super::agents::runtime_agent_guard(
                runtime,
                expected_agent,
                expected_group,
            )),
            watch: Some(std::sync::Arc::clone(&submit_watch)),
        };
        let completion = runtime
            .queue_user_input_submission_guarded(
                Bytes::from(text),
                Bytes::from(enter),
                AGENT_PROMPT_SUBMIT_DELAY,
                submit_deadline,
                Some(guard),
            )
            .map_err(|err| encode_error(id.clone(), "agent_prompt_failed", err.to_string()))?;
        self.record_pane_prompt_submit_watch(ws_idx, pane_id, submit_watch);
        self.record_pane_composer_write(
            ws_idx,
            pane_id,
            crate::terminal::ComposerInputSource::AgentPrompt,
            composer_baseline,
            true,
            true,
        );
        // Report the agent as it stands after the composer write was recorded;
        // the validated snapshot only covers a pane that vanished meanwhile, so
        // no error is returned once bytes have been queued.
        let agent = self.agent_info(ws_idx, pane_id).unwrap_or(agent);
        Ok((id, agent, completion))
    }

    pub(super) fn handle_agent_read(
        &mut self,
        id: String,
        params: crate::api::schema::AgentReadParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let snapshot = crate::app::api_helpers::read_terminal_snapshot(
            pane,
            params.source,
            params.format,
            params.lines,
        );

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text: snapshot.text,
                    revision: 0,
                    truncated: snapshot.truncated,
                },
            },
        )
    }

    pub(super) fn handle_agent_explain(&mut self, id: String, target: AgentTarget) -> String {
        let resolved = match self.resolve_agent_target(&target.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, _workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return agent_not_found(id, &target.target);
        };
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return encode_error(
                id,
                "agent_explain_unavailable",
                format!(
                    "agent target {} does not have a detected agent label",
                    target.target
                ),
            );
        };

        let screen = pane.detection_text();
        let osc_title = pane.agent_osc_title();
        let osc_progress = pane.agent_osc_progress();
        let explain = crate::detect::manifest::explain_with_input(
            agent,
            crate::detect::manifest::DetectionInput {
                screen: &screen,
                osc_title: &osc_title,
                osc_progress: &osc_progress,
            },
        );
        let mut value = crate::detect::manifest::explain_to_json_value(&explain);
        if terminal.full_lifecycle_hook_authority_active() {
            value["state"] = serde_json::Value::String(
                crate::detect::manifest::agent_state_label(terminal.state).to_string(),
            );
            value["matched_rule"] = serde_json::Value::Null;
            value["visible_idle"] = serde_json::Value::Bool(false);
            value["visible_blocker"] = serde_json::Value::Bool(false);
            value["visible_working"] = serde_json::Value::Bool(false);
            value["screen_detection_skipped"] = serde_json::Value::Bool(true);
            value["screen_detection_skip_reason"] =
                serde_json::Value::String("full_lifecycle_hook_authority".to_string());
            value["skip_state_update"] = serde_json::Value::Bool(false);
            value["skipped_update_reason"] = serde_json::Value::Null;
            value["fallback_reason"] = serde_json::Value::Null;
            value["evaluated_rules"] = serde_json::json!([]);
        }

        encode_success(id, ResponseResult::AgentExplain { explain: value })
    }

    pub(super) fn handle_agent_send_keys(
        &mut self,
        id: String,
        params: AgentSendKeysParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = self
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.effective_known_agent())
        else {
            return agent_not_ready(id, &params.target);
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return agent_not_ready(id, &params.target);
        }
        let encoded = match super::super::api_helpers::encode_api_keys(runtime, &params.keys) {
            Ok(encoded) => encoded,
            Err(key) => {
                return encode_error(id, "invalid_key", format!("unsupported key {key}"));
            }
        };
        let bytes: Vec<u8> = encoded.into_iter().flatten().collect();
        let composer_baseline = runtime.detection_content_seq();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "agent_send_keys_failed", err.to_string());
        }
        self.record_pane_composer_write(
            resolved.ws_idx,
            resolved.pane_id,
            crate::terminal::ComposerInputSource::Api,
            composer_baseline,
            false,
            false,
        );
        if super::super::api_helpers::api_keys_abort_turn(&params.keys) {
            if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                terminal.mark_turn_aborted();
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

struct ValidatedAgentPrompt {
    id: String,
    ws_idx: usize,
    pane_id: crate::layout::PaneId,
    expected_agent: crate::detect::Agent,
    agent: crate::api::schema::AgentInfo,
}

fn agent_not_ready(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_ready",
        format!("agent {target} is not an active named agent"),
    )
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

/// Current wall-clock time as an RFC3339 string, for stamping an archive's `at`.
/// Uses the same `now`-source family the rest of the daemon uses (system clock);
/// formatting an always-valid `now_utc()` cannot fail in practice, so an empty
/// string on the impossible error path is a safe, non-panicking fallback.
fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Whether a claude account's config-home currently holds usable credentials. A logged-out profile
/// (e.g. after a `/logout`, which deletes `.credentials.json`) has no credential file; swapping a seat
/// onto it would strand it at a login screen, so restart fails closed on this (workflow-note row 86147).
/// A present file is treated as authenticated here — a deeper `claude auth status` probe (to also catch
/// present-but-expired credentials) is left as a follow-up so the request handler stays subprocess-free.
pub(crate) fn claude_account_has_credentials(config_dir: &str) -> bool {
    let creds = std::path::Path::new(config_dir).join(".credentials.json");
    std::fs::metadata(&creds)
        .map(|meta| meta.len() > 0)
        .unwrap_or(false)
}

/// Where Claude Code actually keeps `.claude.json` for a given config-home.
///
/// TWO LAYOUTS, AND READING THE WRONG ONE REFUSES A SWAP THAT WOULD HAVE WORKED.
/// A config-home reached via `CLAUDE_CONFIG_DIR` keeps the file INSIDE it. The DEFAULT
/// home does not: Claude Code keeps its main config as a SIBLING, at `~/.claude.json`.
/// That is the same quirk `AccountConfig::launch_env` already handles for issue #94 —
/// which is why a default-home account injects no override at all.
///
/// Measured when this was fixed (issue #127): `/root/.claude.json` held 289 projects and
/// trusted the seat's directory, while `/root/.claude/.claude.json` held a stale 102 and
/// did not. The gate read the stale one and refused the swap. A non-default home
/// (`/root/.claude-9`) had the file inside and NO sibling, so both cases are real and the
/// choice cannot be "whichever exists".
///
/// Note `.credentials.json` is NOT like this — it lives inside every config-home,
/// including the default one, so `claude_account_has_credentials` is correct as written.
pub(crate) fn claude_config_file(config_dir: &str) -> std::path::PathBuf {
    if crate::config::is_default_config_dir("claude", config_dir) {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::Path::new(&home).join(".claude.json");
        }
    }
    std::path::Path::new(config_dir).join(".claude.json")
}

/// Why a target account cannot host a resumed agent yet. Each variant names the stage that
/// failed, so a caller can say what to fix instead of reporting a bare failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClaudeLaunchBlocker {
    /// No credentials in the target config-home.
    LoggedOut,
    /// Credentials are present but the profile has never completed first-run setup, so
    /// `claude` opens its theme/onboarding picker instead of resuming.
    OnboardingIncomplete,
    /// The profile has not accepted the trust prompt for this working directory, so
    /// `claude` opens the trust dialog instead of resuming.
    DirectoryNotTrusted { cwd: String },
}

impl ClaudeLaunchBlocker {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::LoggedOut => "account_not_authenticated",
            Self::OnboardingIncomplete => "account_onboarding_incomplete",
            Self::DirectoryNotTrusted { .. } => "account_directory_not_trusted",
        }
    }

    pub(super) fn message(&self, account_id: &str) -> String {
        match self {
            Self::LoggedOut => format!(
                "target account {account_id} is logged out (no credentials found); the agent was left on its current account"
            ),
            Self::OnboardingIncomplete => format!(
                "target account {account_id} has credentials but has not completed first-run setup, so it would open the theme/onboarding picker instead of resuming; the agent was left on its current account"
            ),
            Self::DirectoryNotTrusted { cwd } => format!(
                "target account {account_id} has not trusted {cwd}, so it would open the trust prompt instead of resuming; the agent was left on its current account"
            ),
        }
    }
}

/// Whether a claude account is READY TO HOST a resumed agent in `cwd`, or the first reason
/// it is not.
///
/// Credentials alone were the old gate, and they are not sufficient: the Aug 27 bulk-switch
/// incident moved eleven panes onto an account whose credentials were valid the whole time.
/// The profile had never completed first-run setup and trusted none of the working
/// directories, so every replacement `claude` opened its theme picker or trust prompt
/// instead of resuming — eleven live agents destroyed to reach a modal that looks, to the
/// daemon, like an idle agent.
///
/// These are cheap file reads on purpose. The request handler must not block on a
/// subprocess, so this cannot be `claude auth status`; it catches the states that strand a
/// seat, not expired credentials (see the note on `claude_account_has_credentials`).
pub(super) fn claude_account_launch_blocker(
    config_dir: &str,
    cwd: Option<&str>,
) -> Option<ClaudeLaunchBlocker> {
    if !claude_account_has_credentials(config_dir) {
        return Some(ClaudeLaunchBlocker::LoggedOut);
    }
    let config = std::fs::read_to_string(claude_config_file(config_dir))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    // No parsable config at all is the un-onboarded case: a fresh config-home has no
    // `.claude.json` until the harness writes one on first run.
    let Some(config) = config else {
        return Some(ClaudeLaunchBlocker::OnboardingIncomplete);
    };
    if config
        .get("hasCompletedOnboarding")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Some(ClaudeLaunchBlocker::OnboardingIncomplete);
    }
    // Trust is per working directory, so it can only be judged against the cwd the agent
    // will actually resume in. With no cwd to check, do not invent a failure.
    if let Some(cwd) = cwd {
        let trusted = config
            .get("projects")
            .and_then(|projects| projects.get(cwd))
            .and_then(|project| project.get("hasTrustDialogAccepted"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        if !trusted {
            return Some(ClaudeLaunchBlocker::DirectoryNotTrusted {
                cwd: cwd.to_string(),
            });
        }
    }
    None
}

/// Resolve the on-disk config-home for an account id (or the harness default when `None`).
fn resolve_config_home(
    accounts: &[crate::config::AccountConfig],
    account_id: Option<&str>,
    kind: &str,
) -> Option<std::path::PathBuf> {
    match account_id {
        Some(id) => accounts
            .iter()
            .find(|account| account.id == id)
            .map(|account| std::path::PathBuf::from(&account.config_dir)),
        None => crate::config::default_config_dir(kind),
    }
}

/// Find `<config_home>/projects/<slug>/<id>.jsonl`, returning `(file, slug)`. Globs by session id so it
/// does not depend on reproducing claude's exact cwd→slug rule.
fn find_claude_session_file(
    config_home: &std::path::Path,
    id: &str,
) -> Option<(std::path::PathBuf, String)> {
    let projects = config_home.join("projects");
    let file_name = format!("{id}.jsonl");
    for entry in std::fs::read_dir(&projects).ok()?.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let candidate = entry.path().join(&file_name);
            if candidate.is_file() {
                return Some((candidate, entry.file_name().to_string_lossy().into_owned()));
            }
        }
    }
    None
}

fn session_backup_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "bak".to_string())
}

/// Copy an agent's session transcript from its current account's config-home into the target's, so a
/// `--resume` under the target account can find it. `Ok(())` on success or when no copy is needed (same
/// config-home, or a Path-kind ref that is not config-home-relative). `Err(reason)` means FAIL-LOUD: the
/// caller must NOT tear the agent down — leave it on its current account.
fn migrate_session_for_account_swap(
    accounts: &[crate::config::AccountConfig],
    kind: &str,
    session_ref: &crate::agent_resume::AgentSessionRef,
    current_account: Option<&str>,
    target_account: Option<&str>,
) -> Result<(), String> {
    // Path-kind sessions (pi/omp) are absolute files, not config-home-relative — an account swap does not
    // relocate them; nothing to copy.
    if session_ref.kind == crate::agent_resume::AgentSessionRefKind::Path {
        return Ok(());
    }
    // Id-kind: only claude's on-disk session layout is known. Leave other harnesses (codex/kimi/…) on
    // the pre-existing swap behaviour rather than block a swap that may already work for them — adding
    // their per-harness locators here is a follow-up (H3).
    if kind != "claude" {
        return Ok(());
    }
    let current = resolve_config_home(accounts, current_account, kind)
        .ok_or_else(|| "couldn't resolve the current account's config-home".to_string())?;
    let target = resolve_config_home(accounts, target_account, kind)
        .ok_or_else(|| "couldn't resolve the target account's config-home".to_string())?;
    if current == target {
        return Ok(());
    }

    let id = session_ref.value.as_str();
    let (source, slug) = find_claude_session_file(&current, id)
        .ok_or_else(|| format!("couldn't find session {id} under the current account"))?;
    let target_dir = target.join("projects").join(&slug);
    let target_file = target_dir.join(format!("{id}.jsonl"));

    // Stale-copy guard: the target may already hold an OLD copy from a prior stint. The current account's
    // file is authoritative (the agent is live on one account at a time → superset), so overwrite — but
    // refuse if the source is SMALLER (suspicious), and always back up the target's copy first.
    if target_file.exists() {
        let src_len = std::fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
        let dst_len = std::fs::metadata(&target_file)
            .map(|m| m.len())
            .unwrap_or(0);
        if src_len < dst_len {
            return Err(format!(
                "the target account already holds a LARGER copy of this session ({dst_len} > {src_len} bytes); refusing to overwrite — resolve manually"
            ));
        }
        let backup = target_dir.join(format!("{id}.jsonl.bak-{}", session_backup_suffix()));
        std::fs::rename(&target_file, &backup)
            .map_err(|err| format!("couldn't back up the target's existing session copy: {err}"))?;
    }

    std::fs::create_dir_all(&target_dir)
        .map_err(|err| format!("couldn't create the target project dir: {err}"))?;
    std::fs::copy(&source, &target_file)
        .map_err(|err| format!("couldn't copy the session file: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {

    /// A platform-absolute session path.
    ///
    /// `AgentSessionRef::path` requires `Path::is_absolute`, and "/tmp/x.jsonl" is
    /// NOT absolute on Windows (no drive), so the literal returned `None` there and
    /// the fixture's `unwrap` panicked in CI while passing on unix.
    fn absolute_session_path(name: &str) -> String {
        std::env::temp_dir().join(name).display().to_string()
    }
    use super::*;
    use crate::{
        api::schema::{
            AgentSessionTransferHarness, AgentSessionTransferPhase, AgentStatus,
            AgentTransferSessionParams, PaneReportAgentSessionParams, SuccessResponse,
        },
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn swap_temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-swap-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn swap_account(id: &str, config_dir: &std::path::Path) -> crate::config::AccountConfig {
        crate::config::AccountConfig {
            id: id.to_string(),
            kind: "claude".to_string(),
            label: id.to_string(),
            config_dir: config_dir.to_string_lossy().into_owned(),
        }
    }

    fn id_session(value: &str) -> crate::agent_resume::AgentSessionRef {
        crate::agent_resume::AgentSessionRef {
            kind: crate::agent_resume::AgentSessionRefKind::Id,
            value: value.to_string(),
        }
    }

    fn write_session(config_home: &std::path::Path, slug: &str, id: &str, body: &str) {
        let dir = config_home.join("projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
    }

    #[test]
    fn claude_account_has_credentials_tracks_the_creds_file() {
        let home = swap_temp_root("creds");
        let creds = home.join(".credentials.json");
        // Missing → logged out.
        assert!(!claude_account_has_credentials(&home.to_string_lossy()));
        // Empty file → still treated as logged out.
        std::fs::write(&creds, "").unwrap();
        assert!(!claude_account_has_credentials(&home.to_string_lossy()));
        // Non-empty creds → authenticated.
        std::fs::write(&creds, "{\"token\":\"x\"}").unwrap();
        assert!(claude_account_has_credentials(&home.to_string_lossy()));
        std::fs::remove_dir_all(&home).ok();
    }

    /// A DEFAULT config-home is judged by the SIBLING `~/.claude.json`, not the one
    /// inside the directory (issue #127).
    ///
    /// This is the regression that refused to swap live seats onto `claude-primary`:
    /// `/root/.claude.json` trusted the seat's cwd, `/root/.claude/.claude.json` was a
    /// stale copy that did not, and the gate read the stale one. Both files existed, so
    /// nothing looked wrong — the refusal was simply, confidently, false.
    #[test]
    fn a_default_config_home_is_judged_by_the_sibling_config_file() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let home = swap_temp_root("sibling");
        std::env::set_var("HOME", &home);

        // The DEFAULT config-home for claude, i.e. what `claude-primary` registers.
        let config_dir = home.join(".claude");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join(".credentials.json"), "{\"token\":\"x\"}").unwrap();
        let dir = config_dir.to_string_lossy().to_string();
        let cwd = "/work/repo";

        // Onboarded + trusted, written ONLY into the sibling — the real file.
        std::fs::write(
            home.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{\"/work/repo\":{\"hasTrustDialogAccepted\":true}}}",
        )
        .unwrap();
        // A STALE file inside the directory that trusts nothing. Before the fix this is
        // what was read, and it produced a DirectoryNotTrusted refusal.
        std::fs::write(
            config_dir.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{}}",
        )
        .unwrap();

        assert_eq!(
            claude_account_launch_blocker(&dir, Some(cwd)),
            None,
            "a default-home account must be judged by ~/.claude.json, which trusts this cwd"
        );

        // THE MIRROR, and the half that keeps the fix honest: trust present ONLY inside
        // the directory must NOT satisfy a default-home account. Without this, "always
        // read whichever file has the answer" would pass too, and the gate would be
        // trusting a file Claude Code never reads.
        std::fs::write(
            home.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{}}",
        )
        .unwrap();
        std::fs::write(
            config_dir.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{\"/work/repo\":{\"hasTrustDialogAccepted\":true}}}",
        )
        .unwrap();
        assert_eq!(
            claude_account_launch_blocker(&dir, Some(cwd)),
            Some(ClaudeLaunchBlocker::DirectoryNotTrusted {
                cwd: cwd.to_string()
            }),
            "the in-directory file must not speak for a default-home account"
        );

        match prev_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&home).ok();
    }

    /// The readiness gate, stage by stage.
    ///
    /// Credentials alone were the old gate and they are NOT sufficient: in the Aug 27
    /// incident eleven live agents were killed to move onto an account whose credentials
    /// were valid the entire time. The profile had never completed first-run setup and
    /// trusted nothing, so each replacement opened a theme picker or trust prompt instead
    /// of resuming.
    #[test]
    fn launch_blocker_names_each_stage_that_would_strand_a_seat() {
        let home = swap_temp_root("readiness");
        let dir = home.to_string_lossy().to_string();
        let cwd = "/work/repo";

        // No credentials at all.
        assert_eq!(
            claude_account_launch_blocker(&dir, Some(cwd)),
            Some(ClaudeLaunchBlocker::LoggedOut)
        );

        // Credentials, but no profile yet — the incident's shape.
        std::fs::write(home.join(".credentials.json"), "{\"token\":\"x\"}").unwrap();
        assert_eq!(
            claude_account_launch_blocker(&dir, Some(cwd)),
            Some(ClaudeLaunchBlocker::OnboardingIncomplete)
        );

        // A profile that exists but has NOT completed onboarding is still blocked —
        // presence of the file is not readiness.
        std::fs::write(home.join(".claude.json"), "{\"projects\":{}}").unwrap();
        assert_eq!(
            claude_account_launch_blocker(&dir, Some(cwd)),
            Some(ClaudeLaunchBlocker::OnboardingIncomplete)
        );

        // Onboarded, but this directory is untrusted.
        std::fs::write(
            home.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{}}",
        )
        .unwrap();
        assert_eq!(
            claude_account_launch_blocker(&dir, Some(cwd)),
            Some(ClaudeLaunchBlocker::DirectoryNotTrusted {
                cwd: cwd.to_string()
            })
        );

        // Trusted for a DIFFERENT directory is not trust for this one.
        std::fs::write(
            home.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{\"/other\":{\"hasTrustDialogAccepted\":true}}}",
        )
        .unwrap();
        assert!(matches!(
            claude_account_launch_blocker(&dir, Some(cwd)),
            Some(ClaudeLaunchBlocker::DirectoryNotTrusted { .. })
        ));

        // Fully ready.
        std::fs::write(
            home.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{\"/work/repo\":{\"hasTrustDialogAccepted\":true}}}",
        )
        .unwrap();
        assert_eq!(claude_account_launch_blocker(&dir, Some(cwd)), None);

        // With no cwd to judge, trust is not invented as a failure.
        std::fs::write(
            home.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true,\"projects\":{}}",
        )
        .unwrap();
        assert_eq!(claude_account_launch_blocker(&dir, None), None);

        std::fs::remove_dir_all(&home).ok();
    }

    /// A READY claude target must still swap successfully.
    ///
    /// The gate this file adds only runs for `kind == "claude"`, and the pre-existing
    /// success test uses codex — so it sails straight past and proves nothing about claude.
    /// Without this test, a gate that rejected EVERY claude target would look completely
    /// healthy: all the refusal tests would pass and switching would simply never work
    /// again. Guarding only the failure direction is how you fix one bug by shipping a
    /// worse one.
    #[tokio::test]
    async fn restart_onto_a_ready_claude_account_still_swaps() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        let tid = crate::terminal::TerminalId::from_persisted(terminal_id.clone());
        // Trust is judged against the cwd the replacement resumes in, so seed the target
        // profile with exactly this agent's cwd.
        let cwd = app
            .state
            .terminals
            .get(&tid)
            .expect("agent terminal")
            .cwd
            .to_string_lossy()
            .into_owned();

        // The agent currently lives on a SOURCE account holding its transcript — a swap
        // copies the session across, so without this the run fails at migration and never
        // exercises the swap it is meant to prove.
        let source = swap_temp_root("restart-ready-src");
        write_session(&source, "-work", "sess-123", "line1\nline2\n");
        app.loaded_accounts.push(swap_account("source", &source));
        app.state
            .terminals
            .get_mut(&tid)
            .expect("agent terminal")
            .agent_account = Some("source".to_string());

        let home = swap_temp_root("restart-ready");
        std::fs::write(home.join(".credentials.json"), "{\"token\":\"x\"}").unwrap();
        std::fs::write(
            home.join(".claude.json"),
            serde_json::json!({
                "hasCompletedOnboarding": true,
                "projects": { cwd.clone(): { "hasTrustDialogAccepted": true } }
            })
            .to_string(),
        )
        .unwrap();
        app.loaded_accounts.push(swap_account("target", &home));

        let response = app.handle_agent_restart(
            "req".into(),
            crate::api::schema::AgentRestartParams {
                target: "reviewer".into(),
                account: Some("target".into()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response)
            .unwrap_or_else(|_| panic!("a ready claude target must swap, got: {response}"));
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));

        // The swap actually armed: resume plan set, account remembered, env pointed at the
        // target config-home.
        let terminal = app.state.terminals.get(&tid).expect("terminal survives");
        assert!(
            terminal.pending_agent_resume_plan.is_some(),
            "a permitted swap must arm the resume"
        );
        assert_eq!(terminal.agent_account.as_deref(), Some("target"));
        assert_eq!(
            terminal.pending_launch_env,
            vec![(
                "CLAUDE_CONFIG_DIR".to_string(),
                home.to_string_lossy().into_owned()
            )]
        );
        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&source).ok();
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// THE ASSERTION THE INCIDENT LACKED: on a failed preflight the OLD SEAT KEEPS RUNNING.    /// THE ASSERTION THE INCIDENT LACKED: on a failed preflight the OLD SEAT KEEPS RUNNING.
    ///
    /// The restart path kills the current runtime before anything proves the replacement
    /// can resume, so a preflight that passes wrongly costs a live agent. Asserting only
    /// the error code would not catch a version that returns the error *after* tearing the
    /// seat down — which is exactly the failure mode being fixed.
    #[tokio::test]
    async fn restart_refuses_an_unready_account_and_leaves_the_agent_running() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        let tid = crate::terminal::TerminalId::from_persisted(terminal_id.clone());

        // A target with VALID credentials but no completed onboarding — credentials alone
        // used to be enough to proceed.
        let home = swap_temp_root("restart-unready");
        std::fs::write(home.join(".credentials.json"), "{\"token\":\"x\"}").unwrap();
        app.loaded_accounts.push(swap_account("target", &home));

        let response = app.handle_agent_restart(
            "req".into(),
            crate::api::schema::AgentRestartParams {
                target: "reviewer".into(),
                account: Some("target".into()),
            },
        );

        let err: crate::api::schema::ErrorResponse = serde_json::from_str(&response)
            .expect("an unready target must be refused, not attempted");
        assert_eq!(err.error.code, "account_onboarding_incomplete");

        // The seat is untouched: no resume plan armed, and the terminal still present.
        let terminal = app.state.terminals.get(&tid).expect("agent still exists");
        assert!(
            terminal.pending_agent_resume_plan.is_none(),
            "a refused restart must not arm a resume — that is the destructive half"
        );
        assert!(
            !terminal.respawn_shell_on_exit,
            "a refused restart must not mark the pane for respawn"
        );
        std::fs::remove_dir_all(&home).ok();
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn account_swap_copies_session_into_target_config_home() {
        let root = swap_temp_root("copy");
        let (a, b) = (root.join("a"), root.join("b"));
        let id = "5cf9801c-abc";
        write_session(&a, "-root-gitmoot", id, "line1\nline2\n");
        let accounts = vec![swap_account("A", &a), swap_account("B", &b)];

        migrate_session_for_account_swap(
            &accounts,
            "claude",
            &id_session(id),
            Some("A"),
            Some("B"),
        )
        .expect("migration should succeed");

        let dst = b
            .join("projects")
            .join("-root-gitmoot")
            .join(format!("{id}.jsonl"));
        assert!(
            dst.is_file(),
            "session should be copied to the target config-home"
        );
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "line1\nline2\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn account_swap_fails_loud_when_session_missing() {
        let root = swap_temp_root("missing");
        let accounts = vec![
            swap_account("A", &root.join("a")),
            swap_account("B", &root.join("b")),
        ];
        let err = migrate_session_for_account_swap(
            &accounts,
            "claude",
            &id_session("nope-id"),
            Some("A"),
            Some("B"),
        )
        .unwrap_err();
        assert!(err.contains("couldn't find session"), "got: {err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn account_swap_noop_for_path_kind_and_same_home() {
        let root = swap_temp_root("noop");
        let accounts = vec![
            swap_account("A", &root.join("a")),
            swap_account("B", &root.join("b")),
        ];
        let path_ref = crate::agent_resume::AgentSessionRef {
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: "/some/abs/session.jsonl".to_string(),
        };
        migrate_session_for_account_swap(&accounts, "claude", &path_ref, Some("A"), Some("B"))
            .unwrap();
        migrate_session_for_account_swap(
            &accounts,
            "claude",
            &id_session("x"),
            Some("A"),
            Some("A"),
        )
        .unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn account_swap_noop_for_unsupported_kind() {
        // codex/kimi keep their pre-existing swap behaviour (no migration) until their locators land.
        let accounts: Vec<crate::config::AccountConfig> = vec![];
        migrate_session_for_account_swap(
            &accounts,
            "codex",
            &id_session("x"),
            Some("A"),
            Some("B"),
        )
        .expect("non-claude kinds are a no-op, not an error");
    }

    #[test]
    fn account_swap_backs_up_stale_target_then_overwrites() {
        let root = swap_temp_root("stale-ok");
        let (a, b) = (root.join("a"), root.join("b"));
        let id = "sess-stale";
        write_session(&a, "-root-gitmoot", id, "NEW longer content\n");
        write_session(&b, "-root-gitmoot", id, "old\n");
        let accounts = vec![swap_account("A", &a), swap_account("B", &b)];

        migrate_session_for_account_swap(
            &accounts,
            "claude",
            &id_session(id),
            Some("A"),
            Some("B"),
        )
        .unwrap();
        let dir = b.join("projects").join("-root-gitmoot");
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("{id}.jsonl"))).unwrap(),
            "NEW longer content\n"
        );
        let baks = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".jsonl.bak-"))
            .count();
        assert_eq!(baks, 1, "the stale target copy should be backed up");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn account_swap_refuses_when_source_smaller_than_target() {
        let root = swap_temp_root("stale-refuse");
        let (a, b) = (root.join("a"), root.join("b"));
        let id = "sess-x";
        write_session(&a, "-root-gitmoot", id, "sm\n");
        write_session(
            &b,
            "-root-gitmoot",
            id,
            "much larger existing target content\n",
        );
        let accounts = vec![swap_account("A", &a), swap_account("B", &b)];
        let err = migrate_session_for_account_swap(
            &accounts,
            "claude",
            &id_session(id),
            Some("A"),
            Some("B"),
        )
        .unwrap_err();
        assert!(err.contains("LARGER copy"), "got: {err}");
        std::fs::remove_dir_all(&root).ok();
    }

    fn app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app
    }

    #[test]
    fn agent_list_local_only_excludes_federated_cache() {
        let mut app = app_with_agent();
        let remote = serde_json::from_value(serde_json::json!({
            "terminal_id": "peer/remote-terminal",
            "agent_status": "idle",
            "workspace_id": "peer/remote-workspace",
            "tab_id": "peer/remote-tab",
            "pane_id": "peer/remote-pane",
            "focused": false,
            "revision": 1,
            "machine_id": "peer"
        }))
        .expect("valid remote agent");
        app.federation.lock().unwrap().set_peer(
            "peer",
            crate::api::federation_store::PeerCacheEntry::reachable(
                vec![remote],
                std::time::Instant::now(),
            ),
        );

        let aggregate: SuccessResponse = serde_json::from_str(
            &app.handle_agent_list("aggregate".into(), AgentListParams::default()),
        )
        .expect("aggregate agent.list response");
        let local: SuccessResponse = serde_json::from_str(
            &app.handle_agent_list("local".into(), AgentListParams { local_only: true }),
        )
        .expect("local-only agent.list response");
        let ResponseResult::AgentList {
            agents: aggregate_agents,
            origin_machine_id: aggregate_origin,
            origin_boot_id: aggregate_boot,
            ..
        } = aggregate.result
        else {
            panic!("expected aggregate agent list");
        };
        let ResponseResult::AgentList {
            agents: local_agents,
            origin_machine_id: local_origin,
            origin_boot_id: local_boot,
            ..
        } = local.result
        else {
            panic!("expected local-only agent list");
        };

        assert!(aggregate_agents
            .iter()
            .any(|agent| agent.terminal_id == "peer/remote-terminal"));
        assert!(!local_agents.iter().any(|agent| agent.machine_id.is_some()));
        assert_eq!(aggregate_origin, local_origin);
        assert!(local_origin.is_some());
        assert_eq!(aggregate_boot, local_boot);
        assert!(local_boot.is_some());
    }

    fn start_deferred_agent_prompt(
        app: &mut App,
        id: &str,
        params: AgentPromptParams,
    ) -> std::sync::mpsc::Receiver<String> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(app.handle_deferred_agent_api_request(
            crate::api::schema::Request {
                id: id.into(),
                method: crate::api::schema::Method::AgentPrompt(params),
            },
            respond_to,
        ));
        response_rx
    }

    fn run_deferred_agent_prompt(app: &mut App, id: &str, params: AgentPromptParams) -> String {
        start_deferred_agent_prompt(app, id, params)
            .recv_timeout(Duration::from_secs(1))
            .expect("agent prompt responds after submission")
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_codex_prompt_flushes_paste_burst_before_enter() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        // The non-character key must precede Enter so Codex commits the paste burst first.
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B\x1b[C"));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
    }

    #[tokio::test]
    async fn a_false_process_exit_makes_a_named_live_agent_unreachable_by_name() {
        // Reproduces the registration loss reported on #3225 by rszrszrsz:
        // a live agent pane with an assigned name stops resolving by that name
        // while its process keeps running, and renaming is the only recovery.
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let observed_at = std::time::Instant::now();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());

        let found = app.handle_agent_get(
            "req:before".into(),
            AgentTarget {
                target: "reviewer".into(),
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&found).is_ok(),
            "the assigned name must resolve while the agent is running: {found}"
        );

        // One process-exit observation, then the same agent is observed alive
        // again on the next probe - the process never actually went away.
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at,
            runtime_epoch: None,
        });
        app.handle_internal_event(crate::events::AppEvent::AgentProcessDetected {
            pane_id,
            agent: Agent::Pi,
            observed_at: observed_at + std::time::Duration::from_secs(1),
            runtime_epoch: None,
        });

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.detected_agent,
            Some(Agent::Pi),
            "the agent process is still there"
        );

        let after = app.handle_agent_get(
            "req:after".into(),
            AgentTarget {
                target: "reviewer".into(),
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&after).is_ok(),
            "a live agent must stay reachable by its assigned name: {after}"
        );
    }

    #[tokio::test]
    async fn agent_prompt_sends_text_then_delays_enter() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 2,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let bracketed_started = std::time::Instant::now();
        let response_rx = start_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: public_pane_id,
                text: "A != B".into(),
                wait: None,
            },
        );
        // No "has it answered yet?" poll here: that is a timing window, and a loaded
        // runner loses it (this line failed on CI while passing locally every time).
        // The contract is CAUSAL - the response follows the delayed Enter - and the
        // elapsed assertion below proves it against the same clock the delay uses.
        let response = response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("agent prompt responds after submission");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, delivery } = success.result else {
            panic!("expected prompted response");
        };
        assert_eq!(agent.name.as_deref(), Some("reviewer"));
        assert_eq!(delivery, Some(AgentPromptDelivery::WrittenToPty));
        let first_attempt = agent
            .composer
            .attempt_id
            .as_deref()
            .expect("acknowledged prompt should expose its writer attempt");
        assert!(first_attempt.starts_with("cmp-"));
        assert_eq!(
            agent.composer.evidence.provenance,
            crate::api::schema::ComposerProvenance::AgentPrompt
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
        assert!(bracketed_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        app.lookup_runtime_sender(0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2004l");
        let raw_started = std::time::Instant::now();
        let raw = run_deferred_agent_prompt(
            &mut app,
            "req-raw",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let raw: SuccessResponse = serde_json::from_str(&raw).unwrap();
        assert!(matches!(&raw.result, ResponseResult::AgentPrompted { .. }));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B"));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
        assert!(raw_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);
        let ResponseResult::AgentPrompted { agent, .. } = raw.result else {
            panic!("expected prompted response");
        };
        assert_ne!(
            agent.composer.attempt_id.as_deref(),
            Some(first_attempt),
            "retry with identical text is still a fresh write attempt"
        );

        let rejected = run_deferred_agent_prompt(
            &mut app,
            "req-label",
            AgentPromptParams {
                target: "opencode".into(),
                text: "wrong target".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "agent_not_found");
        assert!(rx.try_recv().is_err());
    }

    /// #26: the submitting Enter is scheduled, not sent, when the identity check
    /// runs. If the pane's occupant changes inside the delay window, an
    /// unguarded delayed write delivers a bare Enter to whatever inherited the
    /// pane — submitting whatever sits in its line buffer — after the caller was
    /// already told the prompt was received.
    ///
    /// Asserts on what reached the PTY, not on a log line. Remove the guard
    /// argument from the scheduling call and this must fail.
    #[tokio::test]
    async fn delayed_enter_is_withheld_when_the_pane_occupant_changes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 1,
            );

        // Occupant is unchanged when the text is written, then changes before the
        // delayed Enter fires — exactly the #26 window.
        let still_hosting = Arc::new(AtomicBool::new(true));
        let guard_flag = Arc::clone(&still_hosting);
        let abandoned = Arc::new(crate::terminal::PromptSubmitWatch::default());

        let completion = runtime
            .queue_user_input_submission_guarded(
                Bytes::from_static(b"prompt text"),
                Bytes::from_static(b"\r"),
                AGENT_PROMPT_SUBMIT_DELAY,
                None,
                Some(crate::pty::actor::SubmissionGuard {
                    occupant_unchanged: Arc::new(move || guard_flag.load(Ordering::SeqCst)),
                    watch: Some(Arc::clone(&abandoned)),
                }),
            )
            .expect("the submission should queue");
        let text = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("text should reach the PTY")
            .expect("channel open");
        assert_eq!(text, Bytes::from_static(b"prompt text"));

        // The pane changes hands while the Enter is still pending.
        still_hosting.store(false, Ordering::SeqCst);

        // Nothing further may reach the PTY. Wait well past the delay so a
        // delivered Enter would have arrived.
        let late = tokio::time::timeout(
            AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(400),
            rx.recv(),
        )
        .await;
        assert!(
            late.is_err(),
            "a bare Enter reached a pane whose occupant had changed: {late:?}"
        );
        assert!(
            abandoned.abandoned.load(Ordering::SeqCst),
            "withholding the Enter must be observable, not silent"
        );
        let completion_error = completion
            .recv_timeout(Duration::from_secs(1))
            .expect("the withheld submission should answer")
            .expect_err("a withheld Enter must not report prompt success");
        assert_eq!(completion_error.kind(), std::io::ErrorKind::Interrupted);
    }

    /// The guard must not withhold the Enter when the occupant is unchanged —
    /// otherwise every prompt would strand, which is the bug the delay exists to
    /// fix (upstream bb29eedb).
    #[tokio::test]
    async fn delayed_enter_is_delivered_when_the_pane_is_unchanged() {
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 1,
            );
        let abandoned = Arc::new(crate::terminal::PromptSubmitWatch::default());
        let completion = runtime
            .queue_user_input_submission_guarded(
                Bytes::new(),
                Bytes::from_static(b"\r"),
                AGENT_PROMPT_SUBMIT_DELAY,
                None,
                Some(crate::pty::actor::SubmissionGuard {
                    occupant_unchanged: Arc::new(|| true),
                    watch: Some(Arc::clone(&abandoned)),
                }),
            )
            .expect("the submission should queue");
        let delivered = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("Enter should arrive")
            .expect("channel open");
        assert_eq!(delivered, Bytes::from_static(b"\r"));
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("the submission should answer")
            .expect("an unchanged occupant should receive the submission");
        assert!(!abandoned.abandoned.load(Ordering::SeqCst));
        assert!(
            abandoned.submitted.load(Ordering::SeqCst),
            "a delivered Enter must record that the prompt submitted"
        );
    }

    /// F3: a post-guard PTY write failure must raise abandonment, not pass
    /// silently. The receiver is dropped so the delayed send fails.
    #[tokio::test]
    async fn a_failed_delayed_write_raises_abandonment() {
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 1,
            );
        let watch = Arc::new(crate::terminal::PromptSubmitWatch::default());
        drop(rx); // the PTY side is gone, so the delayed write cannot land

        let completion = runtime
            .queue_user_input_submission_guarded(
                Bytes::new(),
                Bytes::from_static(b"\r"),
                AGENT_PROMPT_SUBMIT_DELAY,
                None,
                Some(crate::pty::actor::SubmissionGuard {
                    occupant_unchanged: Arc::new(|| true),
                    watch: Some(Arc::clone(&watch)),
                }),
            )
            .expect("the submission should queue");
        let _ = completion.recv_timeout(Duration::from_secs(1));

        assert!(
            watch.abandoned.load(Ordering::SeqCst),
            "a failed delayed write must be loud, not silent"
        );
        assert!(
            !watch.submitted.load(Ordering::SeqCst),
            "a write that failed must not report as submitted"
        );
    }

    /// #31 acceptance, driven through the PRODUCTION path.
    ///
    /// The previous version mutated terminal.turn directly and asserted
    /// retirement — manufacturing a state production cannot reach, because
    /// record_completed_turn_at increments the turn and destroys the watch in
    /// the same call. It passed while the incident still reproduced.
    ///
    /// This drives the real sequence: prompt, let the key land, then complete a
    /// turn the way the runtime does. Reverting resolve-at-clear kills it.
    #[tokio::test]
    async fn a_completed_turn_retires_the_submitted_prompts_claim() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 8,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        let target = app.public_pane_id(0, pane_id).unwrap();

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target,
                text: "api prompt".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, .. } = success.result else {
            panic!("expected prompted response");
        };
        let attempt = agent.composer.attempt_id.clone().expect("attempt id");
        assert_eq!(
            agent.composer.evidence.provenance,
            crate::api::schema::ComposerProvenance::AgentPrompt
        );

        // Let the delayed key land so the watch records submitted.
        let _ = rx.try_recv();
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("Enter should arrive")
            .expect("channel open");

        // Complete a turn exactly as the runtime does — this is the call that
        // both increments the turn and discards the watch.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .record_completed_turn(0, Default::default());

        let after = app.agent_info(0, pane_id).expect("agent info");
        assert_ne!(
            after.composer.attempt_id.as_deref(),
            Some(attempt.as_str()),
            "a submitted prompt whose turn completed must stop being the current attempt"
        );
        assert_ne!(
            after.composer.evidence.provenance,
            crate::api::schema::ComposerProvenance::AgentPrompt,
            "composer text must not inherit the last prompter's identity"
        );
        // F2: author derives from the same condemned source, so it must clear too.
        assert!(
            after.composer.author.is_none(),
            "author must not report api_client for text the prompt did not write"
        );
    }

    /// The axis the deleted turn_at_write test guarded, now covered through the
    /// production path: a retirement belongs to the prompt that earned it and
    /// must not be inherited by the next one.
    ///
    /// Taken from the reviewer's probe. Prompt A submits and its turn completes,
    /// so A retires — correct. Prompt B is then written and is still mid-flight,
    /// so B's claim must stand. With the flag left sticky, B is stripped of
    /// provenance, attempt_id and author, and `spent` has degenerated into
    /// "has this pane ever retired".
    #[tokio::test]
    async fn a_retirement_does_not_carry_over_to_the_next_prompt() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 8,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        let target = app.public_pane_id(0, pane_id).unwrap();

        // Prompt A: submits, its turn completes, so A's claim retires.
        let _ = run_deferred_agent_prompt(
            &mut app,
            "req-a",
            AgentPromptParams {
                target: target.clone(),
                text: "prompt a".into(),
                wait: None,
            },
        );
        let _ = rx.try_recv();
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("Enter should arrive")
            .expect("channel open");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .record_completed_turn(0, Default::default());

        // Prompt B: freshly written, Enter not yet landed.
        let response = run_deferred_agent_prompt(
            &mut app,
            "req-b",
            AgentPromptParams {
                target,
                text: "prompt b".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, .. } = success.result else {
            panic!("expected prompted response");
        };
        assert!(
            agent.composer.attempt_id.is_some(),
            "a fresh prompt's claim must not be retired by a previous prompt's retirement"
        );
        assert_eq!(
            agent.composer.evidence.provenance,
            crate::api::schema::ComposerProvenance::AgentPrompt,
            "a fresh mid-flight prompt keeps its draft attribution"
        );
    }

    /// Reviewer finding on #33: retiring on "the key was written" alone erases
    /// attribution from a genuinely STRANDED prompt — the herdr#18 case, where
    /// the Enter reached the PTY and the draft sat in the composer anyway.
    ///
    /// A stranded draft IS the prompt's, and saying "unknown" about it trades one
    /// wrong answer for another. Retirement now also requires a turn to have
    /// begun. This pins the stranded half; the submitted half is pinned by
    /// a_completed_turn_retires_the_submitted_prompts_claim.
    #[tokio::test]
    async fn a_stranded_prompt_keeps_owning_its_own_draft() {
        use std::sync::atomic::Ordering;

        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 8,
            );
        app.state.insert_test_runtime(pane_id, runtime);

        let target = app.public_pane_id(0, pane_id).unwrap();
        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target,
                text: "stranded text".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, .. } = success.result else {
            panic!("expected prompted response");
        };
        let attempt = agent.composer.attempt_id.clone().expect("attempt id");

        // The Enter is written — but no turn ever starts, which is what stranding
        // looks like from the server's side.
        let _ = rx.try_recv();
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("Enter should arrive")
            .expect("channel open");
        let watch = app
            .state
            .terminals
            .get(&terminal_id)
            .unwrap()
            .prompt_submit_abandoned
            .as_ref()
            .expect("submit watch")
            .clone();
        assert!(
            watch.submitted.load(Ordering::SeqCst),
            "the key was written"
        );

        let after = app.agent_info(0, pane_id).expect("agent info");
        assert_eq!(
            after.composer.attempt_id.as_deref(),
            Some(attempt.as_str()),
            "a stranded prompt must keep owning the draft it wrote"
        );
        assert_eq!(
            after.composer.evidence.provenance,
            crate::api::schema::ComposerProvenance::AgentPrompt,
            "attribution must not be erased when no turn started"
        );
    }

    #[tokio::test]
    async fn agent_prompt_waiter_spawn_failure_fails_before_submitting() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        crate::thread_spawn::test_hook::fail_next_spawns(1);
        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "hello".into(),
                wait: None,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_prompt_failed");
        std::thread::sleep(AGENT_PROMPT_SUBMIT_DELAY * 2);
        assert!(rx.try_recv().is_err(), "a failed prompt must not be sent");
    }

    #[tokio::test]
    async fn agent_prompt_validation_errors_win_over_waiter_spawn_failure() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        for (target, text, code) in [
            ("reviewer", "", "empty_agent_prompt"),
            ("missing", "hello", "agent_not_found"),
        ] {
            crate::thread_spawn::test_hook::fail_next_spawns(1);
            let response = run_deferred_agent_prompt(
                &mut app,
                "req",
                AgentPromptParams {
                    target: target.into(),
                    text: text.into(),
                    wait: None,
                },
            );

            let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(error.error.code, code);
            assert!(
                crate::thread_spawn::spawn_named("probe", || {}).is_err(),
                "a rejected prompt must not start a waiter thread"
            );
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_prompt_rejects_blocked_agent_without_writing() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::GithubCopilot), AgentState::Blocked);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "unrelated prompt".into(),
                wait: None,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_blocked");
        assert!(
            tokio::time::timeout(
                AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "blocked prompt wrote or scheduled terminal input"
        );
    }

    #[tokio::test]
    async fn agent_prompt_focuses_copilot_before_submitting() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::GithubCopilot), AgentState::Idle);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 3,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\x1b[I"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
    }

    #[tokio::test]
    async fn agent_send_keys_validates_every_key_before_writing() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let rejected = app.handle_agent_send_keys(
            "req-invalid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["enter".into(), "not-a-key".into()],
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert!(rx.try_recv().is_err());

        let sent = app.handle_agent_send_keys(
            "req-valid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["up".into(), "enter".into()],
            },
        );
        let success: SuccessResponse = serde_json::from_str(&sent).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\x1b[A\r"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_prompt_rejects_managed_agent_while_startup_is_pending() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        let now = std::time::Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::OpenCode,
            now,
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(10),
        );
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req-pending",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_not_ready");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn agent_focus_marks_already_focused_done_agent_seen() {
        let mut app = app_with_agent();
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_focus(
            "req".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
    }

    #[test]
    fn agent_get_exposes_live_turn_hints_before_first_completion() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);

        let response = app.handle_agent_get(
            "req".into(),
            AgentTarget {
                target: "reviewer".into(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.turn, Some(0));
        assert!(agent.turn_epoch.is_some());
        assert!(agent.last_completed_turn.is_none());
    }

    #[test]
    fn agent_rename_does_not_replace_the_pane_label() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_manual_label("shell-pane".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let target = app.public_pane_id(0, pane_id).unwrap();

        for name in [Some("reviewer".to_string()), None] {
            let response = app.handle_agent_rename(
                "req".into(),
                AgentRenameParams {
                    target: target.clone(),
                    name,
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
            assert_eq!(
                app.state.terminals[&terminal_id].manual_label.as_deref(),
                Some("shell-pane")
            );
        }
    }

    #[tokio::test]
    async fn agent_restart_arms_resume_plan_from_live_session() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name("reviewer".into());
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("sess-123").unwrap(),
            });
        }
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_restart(
            "req".into(),
            AgentRestartParams {
                target: "reviewer".into(),
                account: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));

        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        let plan = terminal
            .pending_agent_resume_plan
            .as_ref()
            .expect("restart arms a resume plan");
        assert_eq!(
            plan.argv,
            vec![
                "claude".to_string(),
                "--resume".to_string(),
                "sess-123".to_string()
            ]
        );
        // The imminent PaneDied must respawn the pane (with resume), not close it.
        assert!(terminal.respawn_shell_on_exit);
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// Characterize the identity/persistence boundary a same-pane replacement relies on.
    ///
    /// Arming a deliberate restart is not the ownership commit. Until the replacement
    /// reports its native session, the source session and all logical pane identity must
    /// remain authoritative so a daemon restart or launch failure can resume the source
    /// instead of forking or losing it.
    #[tokio::test]
    async fn armed_same_pane_replacement_keeps_source_ownership_and_identity() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).expect("public pane id");
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let source_session = crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("source-thread").unwrap(),
        };
        let original_cwd = app.state.terminals[&terminal_id].cwd.clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name("reviewer".into());
            terminal.set_manual_label("session-transfer".into());
            terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
            terminal.set_persisted_agent_session(source_session.clone());
            terminal.agent_account = Some("work".into());
        }
        app.loaded_accounts = vec![crate::config::AccountConfig {
            id: "work".into(),
            kind: "codex".into(),
            label: "Work".into(),
            config_dir: "/home/x/.codex-work".into(),
        }];
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_restart(
            "req".into(),
            AgentRestartParams {
                target: "reviewer".into(),
                account: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));

        let pane_after = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal survives");
        assert_eq!(
            pane_after, terminal_id,
            "the pane keeps its terminal identity"
        );
        assert_eq!(
            app.public_pane_id(0, pane_id).as_deref(),
            Some(public_pane_id.as_str())
        );
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(terminal.manual_label.as_deref(), Some("session-transfer"));
        assert_eq!(terminal.cwd, original_cwd);
        assert_eq!(terminal.agent_account.as_deref(), Some("work"));
        assert_eq!(
            terminal.persisted_agent_session.as_ref(),
            Some(&source_session)
        );
        assert!(terminal.pending_agent_resume_plan.is_some());
        assert!(terminal.respawn_shell_on_exit);
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    fn arm_ready_session_transfer(
        app: &mut App,
    ) -> (
        crate::terminal::TerminalId,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let terminal_id = crate::terminal::TerminalId::from_persisted(arm_resumable_claude(
            app,
            "reviewer",
            AgentState::Idle,
        ));
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let source_home = swap_temp_root("transfer-source");
        let target_home = swap_temp_root("transfer-target");
        let source_path = source_home.join("projects/source/sess-123.jsonl");
        let target_path = target_home.join("sessions/target-thread.jsonl");
        std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        std::fs::write(
            &source_path,
            concat!(
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\r\n",
                "{\"type\":\"progress\",\"provider\":\"claude\"}\r\n",
                "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"world\"}]}}\r\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &target_path,
            concat!(
                "{\"payload\":{\"id\":\"target-thread\"},\"type\":\"session_meta\"}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hello\",\"images\":[],\"local_images\":[]}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"content\":[{\"text\":\"hello\",\"type\":\"input_text\"}],\"role\":\"user\",\"type\":\"message\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"model\":\"test\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"world\"}]}}\n"
            ),
        )
        .unwrap();
        let source_fingerprint =
            crate::session_transfer::fingerprint_transcript(&source_home, &source_path).unwrap();
        let target_fingerprint =
            crate::session_transfer::fingerprint_transcript(&target_home, &target_path).unwrap();
        let source_session = crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("sess-123").unwrap(),
        };
        app.loaded_accounts = vec![
            crate::config::AccountConfig {
                id: "claude-source".into(),
                kind: "claude".into(),
                label: "Claude source".into(),
                config_dir: source_home.to_string_lossy().into_owned(),
            },
            crate::config::AccountConfig {
                id: "codex-target".into(),
                kind: "codex".into(),
                label: "Codex target".into(),
                config_dir: target_home.to_string_lossy().into_owned(),
            },
        ];
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.manual_label = Some("session-transfer".into());
        terminal.agent_account = Some("claude-source".into());
        terminal.set_persisted_agent_session(source_session.clone());
        terminal.session_transfer = Some(crate::session_transfer::RuntimeSessionTransfer {
            id: "transfer-1".into(),
            source_kind: crate::session_transfer::HarnessKind::Claude,
            source_session,
            source_account: Some("claude-source".into()),
            source_config_home: source_home.clone(),
            source_sessions_root: source_home.clone(),
            source_cursor: None,
            source_process_pid: None,
            target_kind: crate::session_transfer::HarnessKind::Codex,
            target_account: Some("codex-target".into()),
            target_config_home: target_home.clone(),
            target_sessions_root: target_home.clone(),
            phase: AgentSessionTransferPhase::Ready,
            message_count: 3,
            omissions: Default::default(),
            error: None,
            source_path: Some(source_path),
            source_fingerprint: Some(source_fingerprint),
            target_session_ref: crate::agent_resume::AgentSessionRef::id("target-thread"),
            target_cursor: None,
            target_transcript_path: Some(target_path),
            target_fingerprint: Some(target_fingerprint),
            target_deadline: None,
            target_process: None,
            source_rollback_process: None,
            verification_in_flight: None,
            verification_observation_deadline: None,
            awaiting_deferred_target_report: false,
            target_report_accepted: false,
        });
        (terminal_id, source_home, target_home)
    }

    fn confirm_transfer_params() -> AgentTransferSessionParams {
        AgentTransferSessionParams {
            target: "reviewer".into(),
            to: AgentSessionTransferHarness::Codex,
            account: Some("codex-target".into()),
            transfer_id: Some("transfer-1".into()),
            confirm: true,
        }
    }

    fn reverse_confirm_transfer_params() -> AgentTransferSessionParams {
        AgentTransferSessionParams {
            target: "reviewer".into(),
            to: AgentSessionTransferHarness::Claude,
            account: Some("claude-source".into()),
            transfer_id: Some("transfer-1".into()),
            confirm: true,
        }
    }

    fn reverse_ready_session_transfer(
        app: &mut App,
    ) -> (
        crate::terminal::TerminalId,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let (terminal_id, claude_home, codex_home) = arm_ready_session_transfer(app);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        let transfer = terminal.session_transfer.as_mut().unwrap();
        let claude_path = transfer.source_path.take().unwrap();
        let claude_fingerprint = transfer.source_fingerprint.take().unwrap();
        let codex_path = transfer.target_transcript_path.take().unwrap();
        let codex_fingerprint = transfer.target_fingerprint.take().unwrap();
        transfer.source_kind = crate::session_transfer::HarnessKind::Codex;
        transfer.source_session = crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("target-thread").unwrap(),
        };
        transfer.source_account = Some("codex-target".into());
        transfer.source_config_home = codex_home.clone();
        transfer.source_sessions_root = codex_home.clone();
        transfer.source_path = Some(codex_path);
        transfer.source_fingerprint = Some(codex_fingerprint);
        transfer.target_kind = crate::session_transfer::HarnessKind::Claude;
        transfer.target_account = Some("claude-source".into());
        transfer.target_config_home = claude_home.clone();
        transfer.target_sessions_root = claude_home.clone();
        transfer.target_session_ref = crate::agent_resume::AgentSessionRef::id("sess-123");
        transfer.target_transcript_path = Some(claude_path);
        transfer.target_fingerprint = Some(claude_fingerprint);
        let source_session = transfer.source_session.clone();
        terminal.hook_authority = None;
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_persisted_agent_session(source_session);
        terminal.agent_account = Some("codex-target".into());
        terminal.set_agent_name("reviewer".into());
        (terminal_id, claude_home, codex_home)
    }

    fn codex_resume_job(pid: u32, session_id: &str) -> crate::platform::ForegroundJob {
        crate::platform::ForegroundJob {
            process_group_id: pid,
            processes: vec![crate::platform::ForegroundProcess {
                pid,
                name: "codex".into(),
                argv0: Some("/usr/bin/codex".into()),
                argv: Some(vec![
                    "/usr/bin/codex".into(),
                    "resume".into(),
                    session_id.into(),
                ]),
                cmdline: Some(format!("/usr/bin/codex resume {session_id}")),
            }],
        }
    }

    fn confirm_and_finish_cutover_verification(
        app: &mut App,
        terminal_id: &crate::terminal::TerminalId,
    ) -> String {
        let response = app.handle_agent_transfer_session("req".into(), confirm_transfer_params());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = &success.result else {
            panic!("expected agent info");
        };
        assert_eq!(
            agent.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::VerifyingCutover
        );
        assert!(app.handle_agent_session_transfer_cutover_verified(
            terminal_id.clone(),
            "transfer-1".into(),
            Ok(())
        ));
        response
    }

    fn launch_ready_codex_transfer(
        app: &mut App,
    ) -> (
        crate::terminal::TerminalId,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(app);
        let response = confirm_and_finish_cutover_verification(app, &terminal_id);
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        app.mark_session_transfer_runtime_launched(&terminal_id, "codex");
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::AwaitingTarget
        );
        (terminal_id, source_home, target_home)
    }

    fn finish_codex_transfer_from_destination_proof(
        app: &mut App,
        terminal_id: &crate::terminal::TerminalId,
        pid: u32,
    ) {
        let observed_at = std::time::Instant::now();
        let job = codex_resume_job(pid, "target-thread");
        assert!(app.reconcile_codex_session_transfer_readiness_with_job(
            terminal_id,
            observed_at,
            Some(&job)
        ));
        assert_eq!(
            app.state.terminals[terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::AwaitingTarget,
            "process observation alone must not cut over before the settle point"
        );
        assert!(app.reconcile_codex_session_transfer_readiness_with_job(
            terminal_id,
            observed_at + crate::app::agents::AGENT_START_SETTLE_DELAY,
            Some(&job)
        ));
        assert_eq!(
            app.state.terminals[terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .verification_in_flight,
            Some(crate::session_transfer::RuntimeVerificationKind::Target)
        );
        assert!(app.handle_agent_session_transfer_runtime_verified_with_job(
            terminal_id.clone(),
            "transfer-1".into(),
            crate::session_transfer::RuntimeVerificationKind::Target,
            pid,
            Ok(()),
            Some(&job),
        ));
        finish_codex_target_blocker_observation(app, terminal_id, pid, &job);
    }

    fn finish_codex_target_blocker_observation(
        app: &mut App,
        terminal_id: &crate::terminal::TerminalId,
        pid: u32,
        job: &crate::platform::ForegroundJob,
    ) {
        let observation_deadline = app.state.terminals[terminal_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .verification_observation_deadline
            .expect("the first JSONL pass starts blocker observation");
        assert_eq!(
            app.state.terminals[terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::AwaitingTarget,
            "the first JSONL pass must not complete before blocker observation"
        );
        assert!(app.reconcile_codex_session_transfer_readiness_with_job(
            terminal_id,
            observation_deadline,
            Some(job),
        ));
        assert!(app.handle_agent_session_transfer_runtime_verified_with_job(
            terminal_id.clone(),
            "transfer-1".into(),
            crate::session_transfer::RuntimeVerificationKind::Target,
            pid,
            Ok(()),
            Some(job),
        ));
    }

    #[tokio::test]
    async fn codex_fallback_completes_from_exact_destination_and_process_proof() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);

        finish_codex_transfer_from_destination_proof(&mut app, &terminal_id, 4242);

        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::Completed);
        assert_eq!(transfer.target_process.unwrap().pid, 4242);
        assert!(transfer.awaiting_deferred_target_report);
        assert_eq!(terminal.agent_account.as_deref(), Some("codex-target"));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(terminal.manual_label.as_deref(), Some("session-transfer"));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| (session.agent.as_str(), session.session_ref.value.as_str())),
            Some(("codex", "target-thread"))
        );

        let reverse_while_deferred = app.handle_agent_transfer_session(
            "reverse".into(),
            AgentTransferSessionParams {
                target: "reviewer".into(),
                to: AgentSessionTransferHarness::Claude,
                account: Some("claude-source".into()),
                transfer_id: None,
                confirm: false,
            },
        );
        let error: crate::api::schema::ErrorResponse =
            serde_json::from_str(&reverse_while_deferred).unwrap();
        assert_eq!(error.error.code, "session_transfer_in_progress");

        app.reconcile_agent_session_transfer_report(
            pane_id,
            "herdr:codex",
            "codex",
            crate::agent_resume::AgentSessionRef::id("target-thread").as_ref(),
            None,
            None,
            true,
        );
        assert!(
            !app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .awaiting_deferred_target_report
        );

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn codex_fallback_proceeds_at_deadline_when_jsonl_and_exact_process_verify() {
        let mut app = app_with_agent();
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);
        let now = std::time::Instant::now();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .session_transfer
            .as_mut()
            .unwrap()
            .target_deadline = Some(now);
        let job = codex_resume_job(5151, "target-thread");

        assert!(app.reconcile_codex_session_transfer_readiness_with_job(
            &terminal_id,
            now,
            Some(&job)
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::AwaitingTarget,
            "the deadline starts the load-bearing JSONL check; it does not itself approve cutover"
        );
        assert!(app.handle_agent_session_transfer_runtime_verified_with_job(
            terminal_id.clone(),
            "transfer-1".into(),
            crate::session_transfer::RuntimeVerificationKind::Target,
            5151,
            Ok(()),
            Some(&job),
        ));
        finish_codex_target_blocker_observation(&mut app, &terminal_id, 5151, &job);
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::Completed,
            "verified content and process proof win after bounded blocker observation"
        );

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn codex_fallback_rolls_back_when_jsonl_differs_after_settle() {
        let mut app = app_with_agent();
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);
        let observed_at = std::time::Instant::now();
        let job = codex_resume_job(6161, "target-thread");
        assert!(app.reconcile_codex_session_transfer_readiness_with_job(
            &terminal_id,
            observed_at,
            Some(&job)
        ));
        let target_path = app.state.terminals[&terminal_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .target_transcript_path
            .clone()
            .unwrap();
        use std::io::Write as _;
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(target_path)
                .unwrap(),
            "{}",
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "extra"}]
                }
            })
        )
        .unwrap();

        assert!(app.reconcile_codex_session_transfer_readiness_with_job(
            &terminal_id,
            observed_at + crate::app::agents::AGENT_START_SETTLE_DELAY,
            Some(&job)
        ));
        let transfer = app.state.terminals[&terminal_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .clone();
        let verification = transfer.verified_visible_destination();
        assert!(verification.is_err());
        assert!(app.handle_agent_session_transfer_runtime_verified_with_job(
            terminal_id.clone(),
            "transfer-1".into(),
            crate::session_transfer::RuntimeVerificationKind::Target,
            6161,
            verification,
            Some(&job),
        ));
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::RollingBack
        );
        assert!(terminal
            .session_transfer
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("destination transcript did not verify"));
        assert_eq!(terminal.agent_account.as_deref(), Some("claude-source"));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.pending_agent_resume_plan.is_some());

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn codex_fallback_rolls_back_on_blocker_or_wrong_session_timeout() {
        let mut blocked = app_with_agent();
        let (blocked_id, source_home, target_home) = launch_ready_codex_transfer(&mut blocked);
        let observed_at = std::time::Instant::now();
        let job = codex_resume_job(7171, "target-thread");
        assert!(blocked.reconcile_codex_session_transfer_readiness_with_job(
            &blocked_id,
            observed_at,
            Some(&job)
        ));
        assert!(blocked.reconcile_codex_session_transfer_readiness_with_job(
            &blocked_id,
            observed_at + crate::app::agents::AGENT_START_SETTLE_DELAY,
            Some(&job)
        ));
        assert!(
            blocked.handle_agent_session_transfer_runtime_verified_with_job(
                blocked_id.clone(),
                "transfer-1".into(),
                crate::session_transfer::RuntimeVerificationKind::Target,
                7171,
                Ok(()),
                Some(&job),
            )
        );
        assert!(blocked.state.terminals[&blocked_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .verification_observation_deadline
            .is_some());
        blocked
            .state
            .terminals
            .get_mut(&blocked_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Blocked);
        assert!(blocked.reconcile_codex_session_transfer_readiness_with_job(
            &blocked_id,
            observed_at + crate::app::agents::AGENT_START_SETTLE_DELAY,
            Some(&job)
        ));
        assert_eq!(
            blocked.state.terminals[&blocked_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::RollingBack
        );
        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();

        let mut timed_out = app_with_agent();
        let (timed_out_id, source_home, target_home) = launch_ready_codex_transfer(&mut timed_out);
        let now = std::time::Instant::now();
        timed_out
            .state
            .terminals
            .get_mut(&timed_out_id)
            .unwrap()
            .session_transfer
            .as_mut()
            .unwrap()
            .target_deadline = Some(now);
        let wrong_job = codex_resume_job(8181, "different-thread");
        assert!(
            timed_out.reconcile_codex_session_transfer_readiness_with_job(
                &timed_out_id,
                now,
                Some(&wrong_job)
            )
        );
        let transfer = timed_out.state.terminals[&timed_out_id]
            .session_transfer
            .as_ref()
            .unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::RollingBack);
        assert!(transfer
            .error
            .as_deref()
            .unwrap()
            .contains("exact Codex resume process for session target-thread"));
        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn deferred_codex_report_mismatch_rolls_back_to_the_source() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);
        finish_codex_transfer_from_destination_proof(&mut app, &terminal_id, 9191);

        app.reconcile_agent_session_transfer_report(
            pane_id,
            "herdr:codex",
            "codex",
            crate::agent_resume::AgentSessionRef::id("wrong-thread").as_ref(),
            None,
            None,
            true,
        );
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::RollingBack
        );
        assert_eq!(terminal.agent_account.as_deref(), Some("claude-source"));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("sess-123")
        );

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn confirmed_session_transfer_arms_target_but_keeps_source_ownership() {
        let mut app = app_with_agent();
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);

        let response = confirm_and_finish_cutover_verification(&mut app, &terminal_id);
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info");
        };
        assert_eq!(
            agent
                .session_transfer
                .as_ref()
                .and_then(|transfer| transfer.target_account.as_deref()),
            Some("codex-target"),
            "clients must be able to reopen a prepared confirmation with the exact target account"
        );
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::LaunchingTarget);
        assert_eq!(terminal.agent_account.as_deref(), Some("codex-target"));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("sess-123"),
            "the source remains the durable owner until the target reports"
        );
        assert_eq!(
            terminal
                .pending_agent_resume_plan
                .as_ref()
                .map(|plan| plan.argv.as_slice()),
            Some(
                ["codex", "resume", "target-thread"]
                    .map(str::to_string)
                    .as_slice()
            )
        );
        assert_eq!(
            terminal.pending_launch_env,
            vec![(
                "CODEX_HOME".into(),
                target_home.to_string_lossy().into_owned()
            )]
        );

        assert!(
            app.begin_agent_session_transfer_rollback(&terminal_id, "forced target launch failure")
        );
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::RollingBack
        );
        assert_eq!(terminal.agent_account.as_deref(), Some("claude-source"));
        assert_eq!(
            terminal
                .pending_agent_resume_plan
                .as_ref()
                .map(|plan| plan.argv.as_slice()),
            Some(
                ["claude", "--resume", "sess-123"]
                    .map(str::to_string)
                    .as_slice()
            )
        );
        assert_eq!(
            terminal.pending_launch_env,
            vec![(
                "CLAUDE_CONFIG_DIR".into(),
                source_home.to_string_lossy().into_owned()
            )]
        );
        assert!(app.fail_agent_session_transfer_rollback_launch(
            &terminal_id,
            "forced source shell failure"
        ));
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::Failed
        );
        assert!(terminal.pending_agent_resume_plan.is_none());
        assert!(terminal.respawn_shell_on_exit);
        assert!(terminal
            .session_transfer
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("source rollback could not launch"));
        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn target_shell_exit_during_rollback_keeps_the_source_resume_armed() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);

        let response = confirm_and_finish_cutover_verification(&mut app, &terminal_id);
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        app.mark_session_transfer_runtime_launched(&terminal_id, "codex");
        assert!(app.begin_agent_session_transfer_rollback(
            &terminal_id,
            "target did not report the staged session"
        ));

        assert!(
            !app.session_transfer_process_exited(pane_id),
            "the target shell killed to begin rollback is not the resumed source exiting"
        );
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::RollingBack
        );
        assert_eq!(
            terminal
                .pending_agent_resume_plan
                .as_ref()
                .map(|plan| plan.argv.as_slice()),
            Some(
                ["claude", "--resume", "sess-123"]
                    .map(str::to_string)
                    .as_slice()
            ),
            "the old target PaneDied must leave the source resume plan intact"
        );

        app.mark_session_transfer_runtime_launched(&terminal_id, "claude");
        assert!(
            app.session_transfer_process_exited(pane_id),
            "after the source launch starts, its exit is a real rollback failure"
        );
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::Failed
        );
        assert!(terminal.pending_agent_resume_plan.is_none());

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn stale_target_pane_died_cannot_fail_a_launched_source_rollback() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);

        let response = confirm_and_finish_cutover_verification(&mut app, &terminal_id);
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        app.mark_session_transfer_runtime_launched(&terminal_id, "codex");
        assert!(app.begin_agent_session_transfer_rollback(
            &terminal_id,
            "target did not report the staged session"
        ));

        let (retired_target_runtime, _target_rx) =
            crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let retired_target_epoch = retired_target_runtime.epoch();
        let (source_runtime, _source_rx) =
            crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let source_epoch = source_runtime.epoch();
        assert_ne!(retired_target_epoch, source_epoch);

        app.handle_internal_event(crate::events::AppEvent::PaneDied {
            pane_id,
            runtime_epoch: Some(retired_target_epoch),
            exit_reason: crate::platform::ChildExitReason::Exited,
        });
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::RollingBack,
            "an unclaimed retired-runtime exit must be rejected during the no-runtime window"
        );

        app.terminal_runtimes
            .insert(terminal_id.clone(), source_runtime);
        app.mark_session_transfer_runtime_launched(&terminal_id, "claude");

        app.handle_internal_event(crate::events::AppEvent::PaneDied {
            pane_id,
            runtime_epoch: Some(retired_target_epoch),
            exit_reason: crate::platform::ChildExitReason::Exited,
        });

        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::RollingBack,
            "a delayed target exit must not be attributed to the live source runtime"
        );
        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .map(crate::terminal::TerminalRuntime::epoch),
            Some(source_epoch),
            "the source rollback runtime must remain installed"
        );

        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.session_transfer = None;
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.set_input_prompt_kind(None);
        }
        let observed_at = std::time::Instant::now();
        assert!(!app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::AgentProcessDetected {
                pane_id,
                runtime_epoch: Some(retired_target_epoch),
                agent: Agent::Codex,
                observed_at,
            }
        ));
        assert!(!app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::StateChanged {
                pane_id,
                runtime_epoch: Some(retired_target_epoch),
                agent: Some(Agent::Codex),
                state: AgentState::Blocked,
                visible_blocker: true,
                visible_working: false,
                process_exited: false,
                observed_at,
            }
        ));
        assert!(!app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::InputStateChanged {
                pane_id,
                runtime_epoch: Some(retired_target_epoch),
                kind: Some(crate::detect::InputPromptKind::Confirm),
            }
        ));
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.detected_agent, Some(Agent::Claude));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.input_prompt_kind, None);

        retired_target_runtime.shutdown();
        if let Some(runtime) = app.terminal_runtimes.remove(&terminal_id) {
            runtime.shutdown();
        }
        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn codex_source_rollback_uses_exact_process_and_native_jsonl_proof() {
        let mut app = app_with_agent();
        let (terminal_id, claude_home, codex_home) = reverse_ready_session_transfer(&mut app);
        let original_cwd = app.state.terminals[&terminal_id].cwd.clone();
        let response =
            app.handle_agent_transfer_session("req".into(), reverse_confirm_transfer_params());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert!(app.handle_agent_session_transfer_cutover_verified(
            terminal_id.clone(),
            "transfer-1".into(),
            Ok(()),
        ));
        app.mark_session_transfer_runtime_launched(&terminal_id, "claude");
        assert!(
            app.begin_agent_session_transfer_rollback(&terminal_id, "forced Claude target failure")
        );
        app.mark_session_transfer_runtime_launched(&terminal_id, "codex");

        let observed_at = std::time::Instant::now();
        let job = codex_resume_job(9292, "target-thread");
        assert!(app.reconcile_codex_session_transfer_rollback_with_job(
            &terminal_id,
            observed_at,
            Some(&job),
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::RollingBack,
            "the exact process observation alone must not approve rollback"
        );
        assert!(app.reconcile_codex_session_transfer_rollback_with_job(
            &terminal_id,
            observed_at + crate::app::agents::AGENT_START_SETTLE_DELAY,
            Some(&job),
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .verification_in_flight,
            Some(crate::session_transfer::RuntimeVerificationKind::SourceRollback)
        );
        assert!(app.handle_agent_session_transfer_runtime_verified_with_job(
            terminal_id.clone(),
            "transfer-1".into(),
            crate::session_transfer::RuntimeVerificationKind::SourceRollback,
            9292,
            Ok(()),
            Some(&job),
        ));
        let observation_deadline = app.state.terminals[&terminal_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .verification_observation_deadline
            .expect("the first source JSONL pass starts blocker observation");
        assert!(app.reconcile_codex_session_transfer_rollback_with_job(
            &terminal_id,
            observation_deadline,
            Some(&job),
        ));
        assert!(app.handle_agent_session_transfer_runtime_verified_with_job(
            terminal_id.clone(),
            "transfer-1".into(),
            crate::session_transfer::RuntimeVerificationKind::SourceRollback,
            9292,
            Ok(()),
            Some(&job),
        ));

        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::RolledBack);
        assert_eq!(transfer.source_rollback_process.unwrap().pid, 9292);
        assert_eq!(terminal.agent_account.as_deref(), Some("codex-target"));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(terminal.manual_label.as_deref(), Some("session-transfer"));
        assert_eq!(terminal.cwd, original_cwd);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| (session.agent.as_str(), session.session_ref.value.as_str())),
            Some(("codex", "target-thread"))
        );

        std::fs::remove_dir_all(claude_home).ok();
        std::fs::remove_dir_all(codex_home).ok();
    }

    #[tokio::test]
    async fn epoch_bound_pane_exit_starts_rollback_before_the_deadline() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);

        app.handle_internal_event_with_render_impact(crate::events::AppEvent::StateChanged {
            runtime_epoch: None,
            pane_id,
            agent: Some(Agent::Codex),
            state: AgentState::Unknown,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at: std::time::Instant::now(),
        });

        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::AwaitingTarget,
            "a detector exit without runtime identity is not transfer authority"
        );
        assert!(app.session_transfer_process_exited(pane_id));

        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::RollingBack);
        assert!(transfer
            .error
            .as_deref()
            .unwrap()
            .contains("Codex resume command exited"));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(terminal.manual_label.as_deref(), Some("session-transfer"));
        assert_eq!(terminal.agent_account.as_deref(), Some("claude-source"));
        assert_eq!(
            terminal
                .pending_agent_resume_plan
                .as_ref()
                .map(|plan| plan.argv.as_slice()),
            Some(
                ["claude", "--resume", "sess-123"]
                    .map(str::to_string)
                    .as_slice()
            )
        );

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn official_session_report_api_finishes_cutover_and_rollback() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);

        let response = confirm_and_finish_cutover_verification(&mut app, &terminal_id);
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        app.mark_session_transfer_runtime_launched(&terminal_id, "codex");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);
        let report = app.handle_pane_report_agent_session(
            "target-report".into(),
            PaneReportAgentSessionParams {
                pane_id: public_pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(1),
                agent_session_id: Some("target-thread".into()),
                agent_session_path: Some(
                    target_home
                        .join("sessions/target-thread.jsonl")
                        .to_string_lossy()
                        .into_owned(),
                ),
                agent_session_cursor: None,
                agent_process_pid: None,
                resume_argv: None,
                session_start_source: Some("resume".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&report).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::Completed,
            "the real pane.report_agent_session API path must finalize target ownership"
        );
        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();

        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);
        let response = confirm_and_finish_cutover_verification(&mut app, &terminal_id);
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        app.mark_session_transfer_runtime_launched(&terminal_id, "codex");
        assert!(app.begin_agent_session_transfer_rollback(&terminal_id, "forced target failure"));
        app.mark_session_transfer_runtime_launched(&terminal_id, "claude");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let report = app.handle_pane_report_agent_session(
            "source-report".into(),
            PaneReportAgentSessionParams {
                pane_id: public_pane_id,
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(2),
                agent_session_id: Some("sess-123".into()),
                agent_session_path: None,
                agent_session_cursor: None,
                agent_process_pid: None,
                resume_argv: None,
                session_start_source: Some("resume".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&report).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .session_transfer
                .as_ref()
                .unwrap()
                .phase,
            AgentSessionTransferPhase::RolledBack,
            "the source integration report must close the rollback transaction"
        );
        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn rejected_stale_session_report_cannot_reconcile_a_transfer() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);
        finish_codex_transfer_from_destination_proof(&mut app, &terminal_id, 9393);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                "herdr:codex".into(),
                "codex".into(),
                crate::agent_resume::AgentSessionRef::id("target-thread"),
                Some(100),
                Some("resume".into()),
            )
            .is_some());

        app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::AgentSessionReported {
                pane_id,
                source: "herdr:codex".into(),
                agent_label: "codex".into(),
                seq: Some(99),
                session_ref: crate::agent_resume::AgentSessionRef::id("wrong-stale-thread"),
                session_path: None,
                session_cursor: None,
                process_pid: None,
                session_start_source: Some("resume".into()),
            },
        );

        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::Completed);
        assert!(transfer.awaiting_deferred_target_report);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("target-thread")
        );

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn accepted_wrong_rollback_report_fails_and_restores_intended_authority() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);
        assert!(app.begin_agent_session_transfer_rollback(&terminal_id, "forced target failure"));
        app.mark_session_transfer_runtime_launched(&terminal_id, "claude");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);

        app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::AgentSessionReported {
                pane_id,
                source: "herdr:claude".into(),
                agent_label: "claude".into(),
                seq: Some(10_000),
                session_ref: crate::agent_resume::AgentSessionRef::id("wrong-source-session"),
                session_path: None,
                session_cursor: None,
                process_pid: None,
                session_start_source: Some("resume".into()),
            },
        );

        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::Failed);
        assert!(transfer
            .error
            .as_deref()
            .unwrap()
            .contains("source rollback reported a different native session"));
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("sess-123")
        );

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn confirmation_refuses_changed_destination_without_stopping_source() {
        let mut app = app_with_agent();
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);
        let target_path = app.state.terminals[&terminal_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .target_transcript_path
            .clone()
            .unwrap();
        std::fs::write(target_path, b"tampered after review\n").unwrap();

        let response = app.handle_agent_transfer_session("req".into(), confirm_transfer_params());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        let transfer = app.state.terminals[&terminal_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .clone();
        let verification = crate::session_transfer::verify_unchanged_transcripts(
            &transfer.source_config_home,
            transfer.source_path.as_ref().unwrap(),
            transfer.source_fingerprint.as_ref().unwrap(),
            &transfer.target_config_home,
            transfer.target_transcript_path.as_ref().unwrap(),
            transfer.target_fingerprint.as_ref().unwrap(),
        );
        assert!(verification.is_err());
        assert!(!app.handle_agent_session_transfer_cutover_verified(
            terminal_id.clone(),
            "transfer-1".into(),
            verification,
        ));
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.session_transfer.as_ref().unwrap().phase,
            AgentSessionTransferPhase::Failed
        );
        assert_eq!(terminal.agent_account.as_deref(), Some("claude-source"));
        assert!(terminal.pending_agent_resume_plan.is_none());
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());
        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn cutover_refuses_target_account_registry_drift_before_stopping_source() {
        let mut app = app_with_agent();
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);

        let response = app.handle_agent_transfer_session("req".into(), confirm_transfer_params());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        app.loaded_accounts
            .retain(|account| account.id != "codex-target");

        assert!(!app.handle_agent_session_transfer_cutover_verified(
            terminal_id.clone(),
            "transfer-1".into(),
            Ok(()),
        ));
        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::Failed);
        assert!(transfer
            .error
            .as_deref()
            .unwrap()
            .contains("target account routing is no longer available"));
        assert_eq!(terminal.agent_account.as_deref(), Some("claude-source"));
        assert!(terminal.pending_agent_resume_plan.is_none());
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn cutover_verification_timeout_fails_without_stopping_source() {
        let mut app = app_with_agent();
        let (terminal_id, source_home, target_home) = arm_ready_session_transfer(&mut app);
        let response = app.handle_agent_transfer_session("req".into(), confirm_transfer_params());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        let deadline = app.state.terminals[&terminal_id]
            .session_transfer
            .as_ref()
            .unwrap()
            .target_deadline
            .unwrap();

        assert!(app.expire_session_transfer_deadlines(deadline));
        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::Failed);
        assert!(transfer
            .error
            .as_deref()
            .unwrap()
            .contains("source stayed running"));
        assert_eq!(terminal.agent_account.as_deref(), Some("claude-source"));
        assert!(terminal.pending_agent_resume_plan.is_none());
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn rollback_refuses_source_account_registry_drift_without_false_recovery() {
        let mut app = app_with_agent();
        let (terminal_id, source_home, target_home) = launch_ready_codex_transfer(&mut app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .pending_agent_resume_plan = None;
        app.loaded_accounts
            .retain(|account| account.id != "claude-source");

        assert!(app.begin_agent_session_transfer_rollback(&terminal_id, "forced target failure"));
        let terminal = &app.state.terminals[&terminal_id];
        let transfer = terminal.session_transfer.as_ref().unwrap();
        assert_eq!(transfer.phase, AgentSessionTransferPhase::Failed);
        assert!(transfer
            .error
            .as_deref()
            .unwrap()
            .contains("source rollback account routing is no longer available"));
        assert_eq!(terminal.agent_account.as_deref(), Some("codex-target"));
        assert!(terminal.pending_agent_resume_plan.is_none());

        std::fs::remove_dir_all(source_home).ok();
        std::fs::remove_dir_all(target_home).ok();
    }

    #[tokio::test]
    async fn agent_restart_omp_uses_path_resume() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name("omp-seat".into());
            terminal.set_detected_state(Some(Agent::Omp), AgentState::Working);
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:omp".into(),
                agent: "omp".into(),
                session_ref: crate::agent_resume::AgentSessionRef::path(absolute_session_path(
                    "omp-sess.jsonl",
                ))
                .unwrap(),
            });
        }
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_restart(
            "req".into(),
            AgentRestartParams {
                target: "omp-seat".into(),
                account: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        let plan = app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .as_ref()
            .expect("restart arms a resume plan");
        assert_eq!(
            plan.argv,
            vec![
                "omp".to_string(),
                format!("--resume={}", absolute_session_path("omp-sess.jsonl"))
            ]
        );
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn agent_restart_errors_without_resumable_session() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name("noresume".into());
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
            // No hook-authority session ref and no persisted session → not resumable.
        }
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_restart(
            "req".into(),
            AgentRestartParams {
                target: "noresume".into(),
                account: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "no_resumable_session");
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    fn arm_codex_agent(app: &mut App, name: &str) -> crate::terminal::TerminalId {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name(name.into());
            terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("codex-sess").unwrap(),
            });
        }
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
        terminal_id
    }

    #[tokio::test]
    async fn agent_restart_with_account_threads_env_and_remembers_swap() {
        let mut app = app_with_agent();
        app.loaded_accounts = vec![crate::config::AccountConfig {
            id: "work".into(),
            kind: "codex".into(),
            label: "Work".into(),
            config_dir: "/home/x/.codex-work".into(),
        }];
        let terminal_id = arm_codex_agent(&mut app, "codexseat");

        // Explicit swap: the account env lands on pending_launch_env — exactly the
        // pairs the resume relaunch injects at its fresh-shell spawn.
        let response = app.handle_agent_restart(
            "req".into(),
            AgentRestartParams {
                target: "codexseat".into(),
                account: Some("work".into()),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        {
            let terminal = app.state.terminals.get(&terminal_id).unwrap();
            assert_eq!(
                terminal.pending_launch_env,
                vec![("CODEX_HOME".to_string(), "/home/x/.codex-work".to_string())]
            );
            assert_eq!(terminal.agent_account.as_deref(), Some("work"));
        }

        // Simulate the relaunch consuming the armed env, then a plain restart
        // (no account param) must resume under the remembered account.
        arm_codex_agent(&mut app, "codexseat");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .pending_launch_env
            .clear();
        let response = app.handle_agent_restart(
            "req".into(),
            AgentRestartParams {
                target: "codexseat".into(),
                account: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.pending_launch_env,
            vec![("CODEX_HOME".to_string(), "/home/x/.codex-work".to_string())],
            "a plain restart keeps the remembered account"
        );
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// THE POINT OF THE WHOLE DIAGNOSTIC: an agent says which account it is on.
    ///
    /// During the incident nothing did. The API answered `ok`, the pane count was right,
    /// and the only evidence that 11 agents had come back on the WRONG account lived in
    /// each child's `/proc/<pid>/environ`. This is that fact, reported by the daemon.
    #[tokio::test]
    async fn an_agent_reports_the_account_it_runs_under() {
        let mut app = app_with_agent();
        app.loaded_accounts = vec![crate::config::AccountConfig {
            id: "work".into(),
            kind: "codex".into(),
            label: "Work".into(),
            config_dir: "/home/x/.codex-work".into(),
        }];
        let terminal_id = arm_codex_agent(&mut app, "codexseat");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .agent_account = Some("work".to_string());

        let info = app
            .agent_info_for_target("codexseat")
            .expect("agent info for a live agent");
        assert_eq!(info.account.as_deref(), Some("work"));
        assert_eq!(
            info.account_config_dir.as_deref(),
            Some("/home/x/.codex-work"),
            "the config-home is the fact that decides which transcript is written"
        );
        assert!(!info.account_unresolved);
    }

    /// A recorded account that is GONE is an error state a person must see, not an agent
    /// that quietly looks idle. This pane will refuse to resume rather than come back on
    /// the default account, and the reason has to reach the surface.
    #[tokio::test]
    async fn an_account_that_no_longer_resolves_is_reported_as_an_error_state() {
        let mut app = app_with_agent();
        // Registry deliberately does NOT contain "retired".
        app.loaded_accounts = vec![crate::config::AccountConfig {
            id: "work".into(),
            kind: "codex".into(),
            label: "Work".into(),
            config_dir: "/home/x/.codex-work".into(),
        }];
        let terminal_id = arm_codex_agent(&mut app, "codexseat");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .agent_account = Some("retired".to_string());

        let info = app
            .agent_info_for_target("codexseat")
            .expect("agent info for a live agent");
        assert_eq!(info.account.as_deref(), Some("retired"));
        assert!(
            info.account_unresolved,
            "an unresolvable account must be surfaced; it is why the agent will not resume"
        );
        assert!(
            info.account_config_dir.is_none(),
            "there is no config-home to report for an account that is not registered"
        );
    }

    /// The NEGATIVE half: a pane with no account invents nothing. Without this, the two
    /// tests above would pass just as well against a builder that hard-coded a value.
    #[tokio::test]
    async fn an_agent_without_an_account_reports_none_and_no_error() {
        let mut app = app_with_agent();
        arm_codex_agent(&mut app, "codexseat");

        let info = app
            .agent_info_for_target("codexseat")
            .expect("agent info for a live agent");
        assert_eq!(info.account, None);
        assert_eq!(info.account_config_dir, None);
        assert!(!info.account_unresolved);
    }

    #[tokio::test]
    async fn agent_restart_unknown_account_errors() {
        let mut app = app_with_agent();
        arm_codex_agent(&mut app, "codexseat");

        let response = app.handle_agent_restart(
            "req".into(),
            AgentRestartParams {
                target: "codexseat".into(),
                account: Some("missing".into()),
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "unknown_account");
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// Arm a live, resumable Claude agent named `name` on the root pane and return
    /// its terminal id.
    fn arm_resumable_claude(app: &mut App, name: &str, state: AgentState) -> String {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name(name.into());
            terminal.set_detected_state(Some(Agent::Claude), state);
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("sess-123").unwrap(),
            });
        }
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
        terminal_id.to_string()
    }

    #[tokio::test]
    async fn agent_archive_moves_agent_into_store_and_persists() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);

        let response = app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: Some("parked".into()),
                by: Some("tester".into()),
                parked_work: vec![serde_json::json!({"pr": 42})],
                force: false,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info");
        };
        assert!(
            agent.archived.is_some(),
            "response marks the agent archived"
        );
        assert_eq!(agent.parked_work, vec![serde_json::json!({"pr": 42})]);

        assert_eq!(app.state.archived_agents.len(), 1);
        let record = &app.state.archived_agents[0];
        assert_eq!(record.name.as_deref(), Some("reviewer"));
        assert_eq!(record.kind, "claude");
        assert_eq!(record.terminal_id, terminal_id);
        assert_eq!(record.agent_session.source, "herdr:claude");
        assert_eq!(record.agent_session.value, "sess-123");
        assert_eq!(record.archived.by, "tester");
        assert_eq!(record.archived.reason.as_deref(), Some("parked"));
        assert!(!record.archived.at.is_empty());
        assert_eq!(record.parked_work, vec![serde_json::json!({"pr": 42})]);
        // The pane's terminal is released (this was the workspace's only pane).
        assert!(!app
            .state
            .terminals
            .contains_key(&crate::terminal::TerminalId::from_persisted(
                terminal_id.clone()
            )));
        assert!(app.state.session_dirty);
    }

    #[tokio::test]
    async fn agent_archive_rejects_working_agent_without_force() {
        let mut app = app_with_agent();
        arm_resumable_claude(&mut app, "reviewer", AgentState::Working);

        let response = app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: None,
                parked_work: Vec::new(),
                force: false,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_working");
        assert!(app.state.archived_agents.is_empty());

        // With force it archives anyway.
        let forced = app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: None,
                parked_work: Vec::new(),
                force: true,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&forced).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert_eq!(app.state.archived_agents.len(), 1);
    }

    #[tokio::test]
    async fn agent_archive_is_idempotent() {
        let mut app = app_with_agent();
        arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        let params = || AgentArchiveParams {
            target: "reviewer".into(),
            reason: None,
            by: Some("tester".into()),
            parked_work: Vec::new(),
            force: false,
        };
        let first = app.handle_agent_archive("req".into(), params());
        assert!(serde_json::from_str::<SuccessResponse>(&first).is_ok());
        // A second archive of the same (now archived) name is a no-op ok.
        let second = app.handle_agent_archive("req".into(), params());
        let success: SuccessResponse = serde_json::from_str(&second).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert_eq!(app.state.archived_agents.len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_resumes_the_session_and_removes_record() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );
        assert_eq!(app.state.archived_agents.len(), 1);

        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: false,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert!(app.state.archived_agents.is_empty());

        // Round-trip preserves the terminal identity and resumes the archived session.
        //
        // The plan is CONSUMED, not left pending: unarchive now spawns the runtime
        // itself instead of arming a plan for a render loop that a headless daemon never
        // runs. A still-pending plan here would mean a second resume fires later.
        let tid = crate::terminal::TerminalId::from_persisted(terminal_id.clone());
        let terminal = app
            .state
            .terminals
            .get(&tid)
            .expect("unarchive resumes into the same terminal id");
        assert!(
            terminal.pending_agent_resume_plan.is_none(),
            "the resume plan must be consumed by the spawn, or it resumes twice"
        );
        // The session that was resumed is the archived one — asserted through the
        // durable field the spawn does not clear, since the plan itself is gone.
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("sess-123")
        );
        // The argv that session produces (`claude --resume <id>`) is asserted where it is
        // built, in `agent_resume`'s own plan tests. Re-deriving it here would only test
        // this test's copy of the call, not the one unarchive made.
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        // The resumed agent lives on a real pane again.
        assert!(app
            .state
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .any(|pane| pane.attached_terminal_id == tid));
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// An unarchived pane must come back with a LIVE RUNTIME, not just live state.
    ///
    /// This is the assertion whose absence let the bug ship. The sibling test above
    /// checks the terminal, the plan, the name and that some pane references the
    /// terminal id — all of which were TRUE while the pane was unusable, because the
    /// PTY spawn was deferred to a render loop gated on view geometry a headless daemon
    /// never has. `agent.list` and `pane.list` read state and treat the runtime as
    /// optional, so they advertised the pane; `pane.read`, `pane.stream` and
    /// `agent.read` all require the runtime and answered `pane_not_found` for it.
    ///
    /// Asserting through the registry rather than a state field is the point: it is the
    /// thing every runtime-requiring API path actually consults.
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_leaves_the_pane_with_a_live_runtime() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );
        let tid = crate::terminal::TerminalId::from_persisted(terminal_id.clone());
        // Archiving really did release the runtime — otherwise a leftover one would let
        // this test pass without unarchive spawning anything.
        assert!(
            app.terminal_runtimes.get(&tid).is_none(),
            "archive must release the pane's runtime, or this test proves nothing"
        );

        app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: false,
            },
        );

        assert!(
            app.terminal_runtimes.get(&tid).is_some(),
            "unarchive advertised a pane with no runtime: every read/stream call on it \
             answers pane_not_found and a client retries forever"
        );
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// The `--fresh` escape hatch arms no resume plan, so the resume launcher declines —
    /// and the pane must still get a shell. A pane you cannot open is not an escape
    /// hatch. Kept separate because `fresh` takes the other branch entirely, and a test
    /// of the default path cannot reach it.
    // Unix-only, and NOT because the assertions are unix-specific: these restore a
    // pane, which spawns a real PTY child, and on a Windows runner they block
    // forever inside that spawn - 1360 s each, aborted by the job timeout. Four of
    // them saturate a 4-core runner, which is what stalled the whole suite at
    // 345/3419 while everything that ran passed.
    //
    // They ran on Windows for the first time in this merge, because the fork's
    // windows_check.ps1 ran a filtered list and upstream's runs everything. Whether
    // the daemon's own unarchive path blocks the same way on Windows, or only this
    // in-process harness does, is UNVERIFIED - it needs a Windows host to answer,
    // and it is filed rather than assumed benign.
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_fresh_also_leaves_the_pane_with_a_live_runtime() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );

        app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: true,
            },
        );

        let tid = crate::terminal::TerminalId::from_persisted(terminal_id);
        assert!(
            app.terminal_runtimes.get(&tid).is_some(),
            "a fresh unarchive left the pane with no runtime, so it cannot be opened"
        );
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// AN UNARCHIVED AGENT MUST COME BACK WHERE IT LEFT, WITH ITS LABEL.
    ///
    /// This is the assertion whose absence let the defect ship. The sibling round-trip
    /// test accepts a pane in ANY workspace, so restoring into a brand-new one passed it
    /// while, in production, every restored agent silently lost the pane LABEL that
    /// fleet tooling binds a role to — healthy-looking and unaddressable.
    /// `arm_resumable_claude` for a workspace other than the first.
    ///
    /// The duplicate-session tests need a SECOND agent holding the same session, and it
    /// has to exist BEFORE the archive: archiving the last agent in a workspace closes
    /// that workspace, so a stand-in armed afterwards has nowhere to live.
    fn arm_resumable_claude_in(app: &mut App, ws_idx: usize, name: &str) -> String {
        let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name(name.into());
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("sess-123").unwrap(),
            });
        }
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
        terminal_id.to_string()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_restores_the_original_workspace_tab_and_label() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        // A SECOND pane in the same tab, so the origin workspace SURVIVES the archive.
        // Without it, archiving the last agent closes the workspace and the origin is
        // gone — the test would then silently exercise the new-workspace fallback and
        // could not tell a real restore from it.
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let filler = crate::workspace::MovedPane {
            pane_id: crate::layout::PaneId::alloc(),
            pane_state: crate::pane::PaneState::new(crate::terminal::TerminalId::alloc()),
        };
        app.state.workspaces[0]
            .insert_moved_pane_into_tab(
                0,
                root,
                filler,
                ratatui::layout::Direction::Horizontal,
                0.5,
                false,
            )
            .unwrap_or_else(|_| panic!("second pane must insert, or the origin dies on archive"));
        // Give the pane a label, the thing that has to survive the round trip.
        let tid = crate::terminal::TerminalId::from_persisted(terminal_id.clone());
        app.state
            .terminals
            .get_mut(&tid)
            .expect("terminal")
            .set_manual_label("reviewer".into());
        let origin_ws = app.public_workspace_id(0);
        let origin_tab = app.public_tab_id(0, 0).expect("tab id");

        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );
        // The origin really was captured — otherwise the restore below could only be
        // passing by luck of there being one workspace.
        let record = app.state.archived_agents.first().expect("archived");
        assert_eq!(
            record.origin_workspace_id.as_deref(),
            Some(origin_ws.as_str())
        );
        assert_eq!(record.origin_tab_id.as_deref(), Some(origin_tab.as_str()));
        assert_eq!(record.pane_label.as_deref(), Some("reviewer"));

        app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: false,
            },
        );

        // Back in the SAME workspace AND the same tab — asserted by ID, not by count.
        // A count cannot discriminate here: the fallback also yields one workspace, so
        // an id-blind assertion would pass on the very behaviour this test exists to
        // reject.
        let (restored_ws, restored_tab) = app
            .state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, workspace)| {
                workspace
                    .tabs
                    .iter()
                    .enumerate()
                    .find_map(|(tab_idx, tab)| {
                        tab.panes
                            .values()
                            .any(|pane| pane.attached_terminal_id == tid)
                            .then_some((ws_idx, tab_idx))
                    })
            })
            .expect("the restored pane must exist somewhere");
        assert_eq!(
            app.public_workspace_id(restored_ws),
            origin_ws,
            "unarchive stranded the agent in a different workspace instead of its own"
        );
        assert_eq!(
            app.public_tab_id(restored_ws, restored_tab),
            Some(origin_tab),
            "unarchive restored into the right workspace but the wrong tab"
        );
        // And the label is back, which is the whole point.
        assert_eq!(
            app.state
                .terminals
                .get(&tid)
                .and_then(|terminal| terminal.manual_label.clone())
                .as_deref(),
            Some("reviewer"),
            "the restored pane lost its label, so a role bound to it cannot resolve"
        );
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// An archive can outlive its workspace. When the origin is gone the restore must
    /// still succeed in a new workspace — the old behaviour, kept as the last tier.
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_falls_back_to_a_new_workspace_when_the_origin_is_gone() {
        let mut app = app_with_agent();
        arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );
        // Destroy the origin between archive and unarchive.
        app.state.workspaces.clear();

        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: false,
            },
        );
        let success: SuccessResponse =
            serde_json::from_str(&response).expect("a lost origin must still restore, not error");
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert_eq!(
            app.state.workspaces.len(),
            1,
            "restored into a fresh workspace"
        );
        assert!(app.state.archived_agents.is_empty());
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// TWO PROCESSES MUST NEVER SHARE ONE SESSION.
    ///
    /// Reproduces the shape seen in production: an agent is archived, a replacement is
    /// started on the SAME session as a workaround, and then the original is unarchived.
    /// Without the guard that yields two harness processes appending to one transcript,
    /// and org actions that cannot be attributed to either.
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_refuses_a_session_a_live_agent_already_holds() {
        let mut app = app_with_agent();
        arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        // The stand-in holding the same session must exist BEFORE the archive —
        // archiving the last agent in a workspace closes that workspace.
        app.state.workspaces.push(Workspace::test_new("standin"));
        app.state.ensure_test_terminals();
        arm_resumable_claude_in(&mut app, 1, "reviewer-2");

        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );
        assert_eq!(app.state.archived_agents.len(), 1);

        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: false,
            },
        );
        let err: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response).expect("a duplicate resume must be refused");
        assert_eq!(err.error.code, "session_in_use");
        // Refusal must be recoverable: the record survives so it can be unarchived once
        // the other agent is retired.
        assert_eq!(
            app.state.archived_agents.len(),
            1,
            "a refused unarchive must leave the archive intact"
        );
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// The guard must not fire for `--fresh`, which resumes nothing and therefore cannot
    /// duplicate anything. A guard that blocked the escape hatch would be worse than no
    /// guard, because it would strand the operator with no way forward.
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_fresh_is_not_blocked_by_a_live_session_holder() {
        let mut app = app_with_agent();
        arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        app.state.workspaces.push(Workspace::test_new("standin"));
        app.state.ensure_test_terminals();
        arm_resumable_claude_in(&mut app, 1, "reviewer-2");

        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );

        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: true,
            },
        );
        let success: SuccessResponse =
            serde_json::from_str(&response).expect("--fresh must remain available");
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert!(app.state.archived_agents.is_empty());
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[test]
    fn agent_unarchive_missing_record_errors() {
        let mut app = app_with_agent();
        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "ghost".into(),
                fresh: false,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "archived_agent_not_found");
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// Push an archived record directly into the store, bypassing the archive
    /// flow, so a test can pin the exact session ref kind/value under test.
    fn push_archived(
        app: &mut App,
        name: &str,
        kind: &str,
        source: &str,
        agent: &str,
        ref_kind: crate::agent_resume::AgentSessionRefKind,
        value: &str,
        cwd: &str,
    ) {
        app.state
            .archived_agents
            .push(crate::persist::ArchivedAgentSnapshot {
                name: Some(name.into()),
                kind: kind.into(),
                terminal_id: format!("term-{name}"),
                agent_session: crate::persist::PaneAgentSessionSnapshot {
                    source: source.into(),
                    agent: agent.into(),
                    kind: ref_kind,
                    value: value.into(),
                },
                cwd: std::path::PathBuf::from(cwd),
                occupant_generation: 2,
                archived: crate::persist::ArchivedAgentMeta {
                    at: "2026-08-26T00:00:00Z".into(),
                    by: "tester".into(),
                    reason: None,
                },
                parked_work: Vec::new(),
                // Origin deliberately absent: these fixtures model a record written
                // BEFORE the origin was captured, which is the tier-3 fallback path.
                origin_workspace_id: None,
                origin_tab_id: None,
                pane_label: None,
            });
    }

    /// Names of the agents `agent.list` reports, split into (live, archived).
    fn listed_agent_names(app: &mut App) -> (Vec<String>, Vec<String>) {
        let success: SuccessResponse = serde_json::from_str(
            &app.handle_agent_list("list".into(), AgentListParams { local_only: true }),
        )
        .expect("agent.list response");
        let ResponseResult::AgentList { agents, .. } = success.result else {
            panic!("expected agent list");
        };
        let (archived, live): (Vec<_>, Vec<_>) = agents
            .into_iter()
            .partition(|agent| agent.archived.is_some());
        let names = |agents: Vec<crate::api::schema::AgentInfo>| {
            agents
                .into_iter()
                .filter_map(|agent| agent.name)
                .collect::<Vec<_>>()
        };
        (names(live), names(archived))
    }

    fn push_archived_claude(app: &mut App, name: &str) {
        push_archived(
            app,
            name,
            "claude",
            "herdr:claude",
            "claude",
            crate::agent_resume::AgentSessionRefKind::Id,
            &format!("sess-{name}"),
            "/tmp",
        );
    }

    fn forget(app: &mut App, target: &str) -> String {
        app.handle_agent_forget(
            "req".into(),
            AgentForgetParams {
                target: target.into(),
            },
        )
    }

    #[test]
    fn agent_forget_removes_only_the_matching_record() {
        let mut app = app_with_agent();
        push_archived_claude(&mut app, "alpha");
        push_archived_claude(&mut app, "beta");
        push_archived_claude(&mut app, "gamma");
        app.state.session_dirty = false;

        let success: SuccessResponse = serde_json::from_str(&forget(&mut app, "beta"))
            .expect("forgetting an archived agent succeeds");
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info");
        };
        // The response is the record exactly as `agent.list` rendered it.
        assert_eq!(agent.name.as_deref(), Some("beta"));
        assert_eq!(agent.terminal_id, "term-beta");
        assert_eq!(
            agent.archived.as_ref().map(|archived| archived.by.as_str()),
            Some("tester")
        );
        assert_eq!(
            agent
                .agent_session
                .as_ref()
                .map(|session| session.value.as_str()),
            Some("sess-beta")
        );

        let remaining: Vec<_> = app
            .state
            .archived_agents
            .iter()
            .map(|record| record.name.clone().unwrap())
            .collect();
        assert_eq!(remaining, ["alpha", "gamma"]);
        let (_, archived) = listed_agent_names(&mut app);
        assert_eq!(archived, ["alpha", "gamma"]);
        assert!(
            app.state.session_dirty,
            "forgetting must schedule a session save, or it is lost on restart"
        );

        // Terminal id addresses a record too, exactly like unarchive.
        let success: SuccessResponse = serde_json::from_str(&forget(&mut app, "term-gamma"))
            .expect("forgetting by terminal id succeeds");
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        let (_, archived) = listed_agent_names(&mut app);
        assert_eq!(archived, ["alpha"]);
    }

    #[test]
    fn agent_forget_stays_gone_after_persist_and_reload() {
        let mut app = app_with_agent();
        push_archived_claude(&mut app, "alpha");
        push_archived_claude(&mut app, "beta");
        forget(&mut app, "alpha");

        // Capture exactly what the session save writes, round-trip it through
        // JSON, and rehydrate a fresh app from it the way `App::new` does.
        let snapshot = crate::persist::capture(
            &app.state.workspaces,
            &app.state.terminals,
            &app.terminal_runtimes,
            app.state.active,
            app.state.selected,
            &app.state.archived_agents,
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        let reloaded: crate::persist::SessionSnapshot = serde_json::from_str(&json).unwrap();

        let mut restarted = app_with_agent();
        restarted.state.archived_agents = reloaded.archived_agents.clone();
        let (_, archived) = listed_agent_names(&mut restarted);
        assert_eq!(
            archived,
            ["beta"],
            "the forgotten record must not come back"
        );

        let error: crate::api::schema::ErrorResponse =
            serde_json::from_str(&forget(&mut restarted, "alpha")).unwrap();
        assert_eq!(error.error.code, "agent_not_found");
    }

    #[test]
    fn agent_forget_unknown_target_is_agent_not_found() {
        let mut app = app_with_agent();
        push_archived_claude(&mut app, "alpha");
        app.state.session_dirty = false;

        let error: crate::api::schema::ErrorResponse =
            serde_json::from_str(&forget(&mut app, "ghost")).unwrap();
        assert_eq!(error.error.code, "agent_not_found");
        assert_eq!(app.state.archived_agents.len(), 1);
        assert!(
            !app.state.session_dirty,
            "a miss must not touch the session"
        );
    }

    #[tokio::test]
    async fn agent_forget_refuses_a_live_agent() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        push_archived_claude(&mut app, "alpha");
        app.state.session_dirty = false;

        for target in ["reviewer", terminal_id.as_str()] {
            let error: crate::api::schema::ErrorResponse =
                serde_json::from_str(&forget(&mut app, target)).unwrap();
            assert_eq!(
                error.error.code, "archived_agent_not_found",
                "target {target}"
            );
            assert!(
                error.error.message.contains("live"),
                "the refusal must say why: {}",
                error.error.message
            );
        }

        // Still live, still not archived, and the unrelated record is untouched.
        let (live, archived) = listed_agent_names(&mut app);
        assert_eq!(live, ["reviewer"]);
        assert_eq!(archived, ["alpha"]);
        assert!(app.resolve_agent_target("reviewer").is_ok());
        assert!(!app.state.session_dirty);
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    /// Several records answering to one name: forget takes the first, the same
    /// record `agent.unarchive` would resume, and leaves the rest.
    #[test]
    fn agent_forget_with_duplicate_names_takes_the_record_unarchive_would() {
        let mut app = app_with_agent();
        push_archived_claude(&mut app, "twin");
        push_archived_claude(&mut app, "twin");
        app.state.archived_agents[1].terminal_id = "term-twin-2".into();

        let success: SuccessResponse = serde_json::from_str(&forget(&mut app, "twin")).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info");
        };
        assert_eq!(agent.terminal_id, "term-twin");
        assert_eq!(app.state.archived_agents.len(), 1);
        assert_eq!(app.state.archived_agents[0].terminal_id, "term-twin-2");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_fresh_starts_clean_and_removes_record() {
        let mut app = app_with_agent();
        let terminal_id = arm_resumable_claude(&mut app, "reviewer", AgentState::Idle);
        app.handle_agent_archive(
            "req".into(),
            AgentArchiveParams {
                target: "reviewer".into(),
                reason: None,
                by: Some("tester".into()),
                parked_work: Vec::new(),
                force: false,
            },
        );
        let cwd = app.state.archived_agents[0].cwd.clone();
        assert_eq!(app.state.archived_agents.len(), 1);

        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "reviewer".into(),
                fresh: true,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert!(app.state.archived_agents.is_empty());

        let tid = crate::terminal::TerminalId::from_persisted(terminal_id.clone());
        let terminal = app
            .state
            .terminals
            .get(&tid)
            .expect("fresh unarchive resumes into the same terminal id");
        // No resume plan is armed — the agent starts clean.
        assert!(
            terminal.pending_agent_resume_plan.is_none(),
            "fresh unarchive must not attach a pending resume plan"
        );
        // Identity and launch cwd are still preserved.
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(terminal.cwd, cwd);
        assert!(app
            .state
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .any(|pane| pane.attached_terminal_id == tid));
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_path_kind_missing_session_errors_then_fresh_succeeds() {
        let mut app = app_with_agent();
        let missing = "/nonexistent/herdr-h2-test/omp-session.json";
        assert!(
            !std::path::Path::new(missing).exists(),
            "test fixture path must not exist"
        );
        push_archived(
            &mut app,
            "omp-one",
            "omp",
            "herdr:omp",
            "omp",
            crate::agent_resume::AgentSessionRefKind::Path,
            missing,
            "/tmp/omp",
        );

        // Without --fresh the lost session file is fatal, and the record stays
        // archived so the operator can retry.
        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "omp-one".into(),
                fresh: false,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "session_lost");
        assert_eq!(app.state.archived_agents.len(), 1, "archive left intact");

        // With --fresh the probe is skipped and the agent starts clean.
        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "omp-one".into(),
                fresh: true,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert!(app.state.archived_agents.is_empty());
        let tid = crate::terminal::TerminalId::from_persisted("term-omp-one".to_string());
        let terminal = app
            .state
            .terminals
            .get(&tid)
            .expect("fresh unarchive brings the terminal back");
        assert!(terminal.pending_agent_resume_plan.is_none());
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_unarchive_id_kind_ignores_existence_probe_and_resumes() {
        // Regression guard: the existence probe is PATH-kind only. An ID-kind
        // ref whose value happens to look like a missing path must still resume.
        let mut app = app_with_agent();
        let path_shaped_id = "/nonexistent/herdr-h2-test/looks-like-a-path";
        assert!(!std::path::Path::new(path_shaped_id).exists());
        push_archived(
            &mut app,
            "claude-one",
            "claude",
            "herdr:claude",
            "claude",
            crate::agent_resume::AgentSessionRefKind::Id,
            path_shaped_id,
            "/tmp/claude",
        );

        let response = app.handle_agent_unarchive(
            "req".into(),
            AgentUnarchiveParams {
                target: "claude-one".into(),
                fresh: false,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert!(app.state.archived_agents.is_empty());

        let tid = crate::terminal::TerminalId::from_persisted("term-claude-one".to_string());
        let terminal = app
            .state
            .terminals
            .get(&tid)
            .expect("id-kind unarchive resumes into the same terminal id");
        // Resumed, not merely armed — unarchive spawns the runtime itself now, so the
        // plan is consumed. The point of this test is unchanged: an id-kind ref that
        // merely LOOKS like a path is not probed for existence, and still resumes.
        assert!(terminal.pending_agent_resume_plan.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some(path_shaped_id)
        );
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn collect_agent_infos_emits_archived_and_marks_active() {
        let mut app = app_with_agent();
        // One live agent on the pane.
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_agent_name("live-one".into());
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);

        // One archived agent in the store.
        app.state
            .archived_agents
            .push(crate::persist::ArchivedAgentSnapshot {
                name: Some("archived-one".into()),
                kind: "codex".into(),
                terminal_id: "term-archived".into(),
                agent_session: crate::persist::PaneAgentSessionSnapshot {
                    source: "herdr:codex".into(),
                    agent: "codex".into(),
                    kind: crate::agent_resume::AgentSessionRefKind::Id,
                    value: "sess-arch".into(),
                },
                cwd: std::path::PathBuf::from("/tmp/arch"),
                occupant_generation: 3,
                archived: crate::persist::ArchivedAgentMeta {
                    at: "2026-08-26T00:00:00Z".into(),
                    by: "tester".into(),
                    reason: None,
                },
                parked_work: vec![serde_json::json!({"task": "x"})],
                origin_workspace_id: None,
                origin_tab_id: None,
                pane_label: None,
            });

        let agents = app.collect_agent_infos();
        let live = agents
            .iter()
            .find(|a| a.name.as_deref() == Some("live-one"))
            .expect("live agent present");
        assert!(
            live.archived.is_none(),
            "active agents omit the archived block"
        );
        assert!(live.parked_work.is_empty());

        let archived = agents
            .iter()
            .find(|a| a.name.as_deref() == Some("archived-one"))
            .expect("archived agent present");
        let block = archived.archived.as_ref().expect("archived block present");
        assert_eq!(block.by, "tester");
        assert_eq!(archived.terminal_id, "term-archived");
        assert_eq!(archived.parked_work, vec![serde_json::json!({"task": "x"})]);
        // Paneless: no live pane ids.
        assert!(archived.pane_id.is_empty());
    }
}
