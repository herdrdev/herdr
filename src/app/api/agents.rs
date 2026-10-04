use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{
    AgentFocusParams, AgentFocusStatusParams, AgentPromptParams, AgentRenameParams,
    AgentReportParams, AgentSendKeysParams, AgentStartParams, AgentStateParams, AgentTarget,
    PaneReadResult, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

const AGENT_PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);
const MAX_AGENT_SEARCH_BYTES: u64 = 8 * 1024 * 1024;
type AgentFileSearchMatch = (String, u32, Vec<String>, Vec<String>);

impl App {
    pub(crate) fn tick_agent_finder(&mut self, now: std::time::Instant) -> bool {
        let Some(deadline) = self.state.agent_finder_deadline else {
            return false;
        };
        if now < deadline {
            return false;
        }
        self.state.agent_finder_deadline = None;
        let query = self.state.sidebar_work_filter.query.trim().to_owned();
        let query = query.to_lowercase();
        let generation = self.state.agent_finder_generation;
        self.state.agent_finder_results.clear();
        self.state.agent_finder_selected = 0;
        self.state.agent_finder_partial = false;

        let mut candidates = Vec::new();
        for agent in self.collect_agent_infos() {
            let title = agent
                .display_title
                .as_deref()
                .or(agent.title.as_deref())
                .or(agent.name.as_deref())
                .unwrap_or("agent")
                .to_owned();
            let title_match = agent_title_match(&agent, &query).map(str::to_owned);
            if query.is_empty() {
                self.state
                    .agent_finder_results
                    .push(crate::api::schema::AgentSearchHit {
                        target: agent.pane_id.clone(),
                        title,
                        source: crate::api::schema::AgentSearchSource::Title,
                        path: None,
                        line: None,
                        context: Vec::new(),
                        preview: Vec::new(),
                        agent,
                    });
                continue;
            }
            if let Some(matched) = title_match {
                self.state
                    .agent_finder_results
                    .push(crate::api::schema::AgentSearchHit {
                        target: agent.pane_id.clone(),
                        title,
                        source: crate::api::schema::AgentSearchSource::Title,
                        path: None,
                        line: None,
                        context: vec![matched],
                        preview: Vec::new(),
                        agent,
                    });
            } else if let Some(context) = self.search_agent_tail(&agent.pane_id, &query) {
                self.state
                    .agent_finder_results
                    .push(crate::api::schema::AgentSearchHit {
                        target: agent.pane_id.clone(),
                        title,
                        source: crate::api::schema::AgentSearchSource::Tail,
                        path: None,
                        line: None,
                        preview: context.clone(),
                        context,
                        agent,
                    });
            } else {
                candidates.push(agent);
            }
        }
        if !query.is_empty() && self.state.agent_finder_results.len() > 100 {
            self.state.agent_finder_results.truncate(100);
            self.state.agent_finder_partial = true;
        }
        for host in self.state.fleet_snapshot.hosts.iter().filter(|host| {
            !host.local && host.reachable && !self.state.sidebar_work_filter.only_this_machine
        }) {
            for row in &host.entries {
                let Some(agent) = row.agent_info() else {
                    continue;
                };
                let title = agent
                    .display_title
                    .as_deref()
                    .or(agent.title.as_deref())
                    .or(agent.name.as_deref())
                    .unwrap_or(&row.agent_ref.agent);
                let matched = [
                    Some(title),
                    agent.display_agent.as_deref(),
                    agent.agent.as_deref(),
                    Some(row.agent_ref.host.as_str()),
                ]
                .into_iter()
                .flatten()
                .find(|value| value.to_lowercase().contains(&query));
                let matched = match matched {
                    Some(matched) => matched,
                    None if query.is_empty() => title,
                    None => continue,
                };
                if query.is_empty() || self.state.agent_finder_results.len() < 100 {
                    self.state
                        .agent_finder_results
                        .push(crate::api::schema::AgentSearchHit {
                            target: agent.pane_id.clone(),
                            title: title.to_owned(),
                            source: crate::api::schema::AgentSearchSource::Title,
                            path: None,
                            line: None,
                            context: vec![matched.to_owned()],
                            preview: Vec::new(),
                            agent: agent.clone(),
                        });
                } else {
                    self.state.agent_finder_partial = true;
                }
            }
        }
        if query.is_empty() {
            self.state.agent_finder_results.sort_by(|left, right| {
                right
                    .agent
                    .last_turn_at
                    .as_deref()
                    .cmp(&left.agent.last_turn_at.as_deref())
            });
        }
        if self.state.agent_finder_results.len() >= 100 && !candidates.is_empty() {
            self.state.agent_finder_partial = true;
        } else if !candidates.is_empty() {
            let remaining = 100 - self.state.agent_finder_results.len();
            let event_tx = self.event_tx.clone();
            tokio::task::spawn_blocking(move || {
                let (hits, partial) = search_agent_session_candidates(
                    candidates
                        .into_iter()
                        .map(|agent| {
                            let title = agent
                                .display_title
                                .as_deref()
                                .or(agent.title.as_deref())
                                .or(agent.name.as_deref())
                                .unwrap_or("agent")
                                .to_owned();
                            (agent, title)
                        })
                        .collect(),
                    &query,
                    remaining,
                    MAX_AGENT_SEARCH_BYTES,
                    None,
                );
                let _ =
                    event_tx.blocking_send(crate::events::AppEvent::AgentFinderSearchCompleted {
                        generation,
                        query,
                        hits,
                        partial,
                    });
            });
        }
        true
    }

    pub(crate) fn start_deferred_agent_search(
        &mut self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
        active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) {
        let crate::api::schema::Method::AgentSearch(params) = request.method else {
            return;
        };
        let query = params.query.trim().to_lowercase();
        if query.is_empty() {
            let _ = respond_to.send(encode_success(
                request.id,
                ResponseResult::AgentSearch {
                    hits: Vec::new(),
                    partial: false,
                },
            ));
            return;
        }

        let limit = usize::from(params.limit.clamp(1, 200));
        let mut hits = Vec::new();
        let mut candidates = Vec::new();
        let mut partial = false;
        for agent in self.collect_agent_infos() {
            let title = agent
                .display_title
                .as_deref()
                .or(agent.title.as_deref())
                .or(agent.name.as_deref())
                .unwrap_or("agent")
                .to_owned();
            if let Some(value) = agent_title_match(&agent, &query).map(str::to_owned) {
                if hits.len() < limit {
                    hits.push(agent_search_hit(
                        agent,
                        title,
                        crate::api::schema::AgentSearchSource::Title,
                        None,
                        None,
                        vec![value],
                        Vec::new(),
                    ));
                } else {
                    partial = true;
                }
            } else if let Some(context) = self.search_agent_tail(&agent.pane_id, &query) {
                if hits.len() < limit {
                    hits.push(agent_search_hit(
                        agent,
                        title,
                        crate::api::schema::AgentSearchSource::Tail,
                        None,
                        None,
                        context.clone(),
                        context,
                    ));
                } else {
                    partial = true;
                }
            } else {
                candidates.push((agent, title));
            }
        }

        if hits.len() > limit {
            hits.truncate(limit);
            partial = true;
        }
        for host in self
            .state
            .fleet_snapshot
            .hosts
            .iter()
            .filter(|host| !host.local && host.reachable)
        {
            for row in &host.entries {
                let Some(agent) = row.agent_info() else {
                    continue;
                };
                let title = agent
                    .display_title
                    .as_deref()
                    .or(agent.title.as_deref())
                    .or(agent.name.as_deref())
                    .unwrap_or(&row.agent_ref.agent);
                let matched = [
                    Some(title),
                    agent.display_agent.as_deref(),
                    agent.agent.as_deref(),
                    Some(row.agent_ref.host.as_str()),
                ]
                .into_iter()
                .flatten()
                .find(|value| value.to_lowercase().contains(&query));
                if let Some(matched) = matched {
                    if hits.len() < limit {
                        hits.push(agent_search_hit(
                            agent.clone(),
                            title.to_owned(),
                            crate::api::schema::AgentSearchSource::Title,
                            None,
                            None,
                            vec![matched.to_owned()],
                            Vec::new(),
                        ));
                    } else {
                        partial = true;
                    }
                }
            }
        }

        let remaining = limit.saturating_sub(hits.len());
        if candidates.is_empty() || remaining == 0 {
            partial |= remaining == 0 && !candidates.is_empty();
            let _ = respond_to.send(encode_success(
                request.id,
                ResponseResult::AgentSearch { hits, partial },
            ));
            return;
        }

        let id = request.id;
        let event_tx = self.event_tx.clone();
        let active =
            active.unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)));
        defer_agent_search_response(event_tx, respond_to, active, move |active| {
            let (session_hits, session_partial) = search_agent_session_candidates(
                candidates,
                &query,
                limit.saturating_sub(hits.len()),
                MAX_AGENT_SEARCH_BYTES,
                Some(active),
            );
            hits.extend(session_hits);
            partial |= session_partial;
            encode_success(id, ResponseResult::AgentSearch { hits, partial })
        });
    }

    pub(super) fn handle_agent_search(
        &mut self,
        id: String,
        params: crate::api::schema::AgentSearchParams,
    ) -> String {
        let query = params.query.trim().to_lowercase();
        if query.is_empty() {
            return encode_success(
                id,
                ResponseResult::AgentSearch {
                    hits: Vec::new(),
                    partial: false,
                },
            );
        }

        let mut hits = Vec::new();
        let limit = usize::from(params.limit.clamp(1, 200));
        let mut partial = false;
        for agent in self.collect_agent_infos() {
            let title = agent
                .display_title
                .as_deref()
                .or(agent.title.as_deref())
                .or(agent.name.as_deref())
                .unwrap_or("agent")
                .to_owned();
            let title_match = agent_title_match(&agent, &query).map(str::to_owned);
            if let Some(value) = title_match {
                hits.push(crate::api::schema::AgentSearchHit {
                    target: agent.pane_id.clone(),
                    title,
                    source: crate::api::schema::AgentSearchSource::Title,
                    path: None,
                    line: None,
                    context: vec![value],
                    preview: Vec::new(),
                    agent,
                });
            } else if let Some(context) = self.search_agent_tail(&agent.pane_id, &query) {
                hits.push(crate::api::schema::AgentSearchHit {
                    target: agent.pane_id.clone(),
                    title,
                    source: crate::api::schema::AgentSearchSource::Tail,
                    path: None,
                    line: None,
                    preview: context.clone(),
                    context,
                    agent,
                });
            } else if params.include_session {
                let transcript = load_agent_transcript(&agent);
                if let Some(hit) = self.search_agent_session(agent, title, &query, transcript) {
                    hits.push(hit);
                }
            }
        }
        if hits.len() > limit {
            hits.truncate(limit);
            partial = true;
        }
        for host in self
            .state
            .fleet_snapshot
            .hosts
            .iter()
            .filter(|host| !host.local && host.reachable)
        {
            for row in &host.entries {
                let Some(agent) = row.agent_info() else {
                    continue;
                };
                let title = agent
                    .display_title
                    .as_deref()
                    .or(agent.title.as_deref())
                    .or(agent.name.as_deref())
                    .unwrap_or(&row.agent_ref.agent);
                let matched = [
                    Some(title),
                    agent.display_agent.as_deref(),
                    agent.agent.as_deref(),
                    Some(row.agent_ref.host.as_str()),
                ]
                .into_iter()
                .flatten()
                .find(|value| value.to_lowercase().contains(&query));
                let Some(matched) = matched else {
                    continue;
                };
                if hits.len() < limit {
                    hits.push(crate::api::schema::AgentSearchHit {
                        target: agent.pane_id.clone(),
                        title: title.to_owned(),
                        source: crate::api::schema::AgentSearchSource::Title,
                        path: None,
                        line: None,
                        context: vec![matched.to_owned()],
                        preview: Vec::new(),
                        agent: agent.clone(),
                    });
                } else {
                    partial = true;
                }
            }
        }
        encode_success(id, ResponseResult::AgentSearch { hits, partial })
    }

    fn search_agent_tail(&mut self, target: &str, query: &str) -> Option<Vec<String>> {
        let resolved = self.resolve_agent_target(target).ok()?;
        let (pane, _) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)?;
        let snapshot = crate::app::api_helpers::read_terminal_snapshot(
            pane,
            crate::api::schema::ReadSource::Detection,
            crate::api::schema::ReadFormat::Text,
            Some(300),
        );
        matching_context(&snapshot.text, query)
    }

    fn search_agent_session(
        &self,
        agent: crate::api::schema::AgentInfo,
        title: String,
        query: &str,
        transcript: Option<AgentTranscript>,
    ) -> Option<crate::api::schema::AgentSearchHit> {
        if let Some(transcript) = transcript {
            if let Some(context) = matching_context(&transcript.content, query) {
                return Some(crate::api::schema::AgentSearchHit {
                    target: agent.pane_id.clone(),
                    title,
                    source: crate::api::schema::AgentSearchSource::Session,
                    path: Some(transcript.path),
                    line: matching_line(&transcript.content, query),
                    context,
                    preview: trailing_lines(&transcript.content, 5),
                    agent,
                });
            }
        }

        let cwd = agent.cwd.as_deref().or(agent.foreground_cwd.as_deref())?;
        for folder in [".local", "out"] {
            let root = std::path::Path::new(cwd).join(folder);
            if let Some((path, line, context, preview)) = search_text_files(&root, query) {
                return Some(crate::api::schema::AgentSearchHit {
                    target: agent.pane_id.clone(),
                    title,
                    source: crate::api::schema::AgentSearchSource::Path,
                    path: Some(path),
                    line: Some(line),
                    context,
                    preview,
                    agent,
                });
            }
        }
        None
    }

    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        self.reconcile_managed_agent_target(&target.target);
        let agent = match self.agent_info_for_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_state(&mut self, id: String, params: AgentStateParams) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let status = self
            .pane_info(resolved.ws_idx, resolved.pane_id)
            .map(|pane| pane.agent_status)
            .unwrap_or(crate::api::schema::AgentStatus::Unknown);
        let state = self.state.agent_states.snapshot(resolved.pane_id, status);
        encode_success(id, ResponseResult::AgentState { state })
    }

    pub(super) fn handle_agent_report(&mut self, id: String, params: AgentReportParams) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        if let Err(message) = self.state.agent_states.report(
            resolved.pane_id,
            params.payload(),
            std::time::SystemTime::now(),
        ) {
            return encode_error(id, "invalid_agent_report", message);
        }
        self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
        let status = self
            .pane_info(resolved.ws_idx, resolved.pane_id)
            .map(|pane| pane.agent_status)
            .unwrap_or(crate::api::schema::AgentStatus::Unknown);
        let state = self.state.agent_states.snapshot(resolved.pane_id, status);
        encode_success(id, ResponseResult::AgentState { state })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus_params(
        &mut self,
        id: String,
        params: AgentFocusParams,
    ) -> String {
        match (params.target, params.agent_ref) {
            (Some(target), None) => self.handle_agent_focus(id, AgentTarget { target }),
            (None, Some(agent_ref)) if agent_ref.host == self.state.agent_host_name => self
                .handle_agent_focus(
                    id,
                    AgentTarget {
                        target: agent_ref.agent,
                    },
                ),
            (None, Some(agent_ref)) => {
                if !self
                    .remote_focus_transport
                    .accepts_remote_host(&agent_ref.host)
                {
                    return encode_error(
                        id,
                        "unknown_host",
                        format!("unknown remote focus host: {}", agent_ref.host),
                    );
                }
                if !self
                    .remote_focus_transport
                    .remote_host_ready(&agent_ref.host)
                {
                    return encode_error(
                        id,
                        "host_unreachable",
                        format!(
                            "remote host {} has no unambiguous identity from a successful fleet poll",
                            agent_ref.host
                        ),
                    );
                }
                let started = match self.start_remote_focus_operation(agent_ref) {
                    Ok(started) => started,
                    Err(error) => return encode_error_body(id, error),
                };
                encode_success(
                    id,
                    ResponseResult::AgentFocusStarted {
                        operation_id: started.operation_id,
                        agent_ref: started.agent_ref,
                        state: crate::api::schema::RemoteFocusState::Connecting,
                        proxy_pane_id: started.proxy_pane_id,
                    },
                )
            }
            (Some(_), Some(_)) | (None, None) => encode_error(
                id,
                "invalid_request",
                "agent.focus requires exactly one of target or agent_ref",
            ),
        }
    }

    pub(super) fn handle_agent_focus_status(
        &mut self,
        id: String,
        params: AgentFocusStatusParams,
    ) -> String {
        let status = match self.remote_focus_status(&params.operation_id) {
            Ok(status) => status,
            Err(error) => return encode_error_body(id, error),
        };
        encode_success(
            id,
            ResponseResult::AgentFocusStatus {
                operation_id: status.operation_id,
                agent_ref: status.agent_ref,
                state: status.state,
                proxy_pane_id: status.proxy_pane_id,
                context: status.context,
                error: status.error,
            },
        )
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let (agent, argv) = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(super) fn handle_agent_prompt(&mut self, id: String, params: AgentPromptParams) -> String {
        if params.text.is_empty() {
            return encode_error(id, "empty_agent_prompt", "agent prompt must not be empty");
        }
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
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return agent_not_found(id, &params.target);
        };
        let closing_block_hook = terminal.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::is_closing_block_source(&authority.source, &authority.agent_label)
        });
        if terminal.raw_agent_state() == crate::detect::AgentState::Blocked && !closing_block_hook {
            return encode_error(
                id,
                "agent_blocked",
                format!(
                    "agent {} is blocked and requires interactive input",
                    params.target
                ),
            );
        }
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return agent_not_ready(id, &params.target);
        };
        if terminal.managed_agent_launch_pending() {
            return agent_not_ready(id, &params.target);
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return encode_error(
                id,
                "agent_not_ready",
                format!(
                    "agent {} is no longer the pane foreground process",
                    params.target
                ),
            );
        }
        if expected_agent == crate::detect::Agent::GithubCopilot {
            // Copilot ignores synthetic Enter after focus loss until it receives focus gained.
            let focus = match crate::ghostty::encode_focus(crate::ghostty::FocusEvent::Gained) {
                Ok(focus) => focus,
                Err(err) => return encode_error(id, "agent_prompt_failed", err.to_string()),
            };
            if let Err(err) = runtime.try_send_bytes(Bytes::from(focus)) {
                return encode_error(id, "agent_prompt_failed", err.to_string());
            }
        }
        let (text, enter) =
            crate::app::api_helpers::encode_api_submission_parts(runtime, &params.text);
        if let Err(err) = runtime.try_send_bytes(Bytes::from(text)) {
            return encode_error(id, "agent_prompt_failed", err.to_string());
        }
        runtime.send_bytes_after(Bytes::from(enter), AGENT_PROMPT_SUBMIT_DELAY);
        self.retire_blocked_hook_authority_for_pane(resolved.pane_id, std::time::Instant::now());
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        encode_success(id, ResponseResult::AgentPrompted { agent })
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
                    terminal_id: resolved.terminal_id,
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    agent_ref: None,
                    agent_session: None,
                    source: params.source,
                    format: params.format,
                    text: snapshot.text,
                    revision: pane.content_revision(),
                    truncated: snapshot.truncated,
                    input_observation: None,
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
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
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
        if let Some(object) = value.as_object_mut() {
            let screen_state = crate::detect::manifest::agent_state_label(explain.state);
            let effective_state = terminal.raw_agent_state();
            let arbitration = terminal.effective_state_arbitration();
            object.insert("screen_state".into(), serde_json::json!(screen_state));
            object.insert(
                "state".into(),
                serde_json::json!(crate::detect::manifest::agent_state_label(effective_state)),
            );
            object.insert(
                "effective_state".into(),
                serde_json::json!(crate::detect::manifest::agent_state_label(effective_state)),
            );
            object.insert("arbitration".into(), serde_json::json!(arbitration));
            object.insert("screen_detection_skipped".into(), serde_json::json!(false));
            object.insert(
                "screen_detection_skip_reason".into(),
                serde_json::Value::Null,
            );
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
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = self
            .state
            .terminals
            .get(terminal_id)
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
        let has_bytes = !bytes.is_empty();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "agent_send_keys_failed", err.to_string());
        }
        if has_bytes {
            self.retire_blocked_hook_authority_for_pane(
                resolved.pane_id,
                std::time::Instant::now(),
            );
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn defer_agent_search_response<F>(
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    respond_to: std::sync::mpsc::Sender<String>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    search: F,
) where
    F: FnOnce(&std::sync::atomic::AtomicBool) -> String + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let response = search(&active);
        if !active.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let _ = event_tx.blocking_send(crate::events::AppEvent::AgentSearchFinished {
            response,
            respond_to,
            active,
        });
    });
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

fn agent_title_match<'a>(agent: &'a crate::api::schema::AgentInfo, query: &str) -> Option<&'a str> {
    [
        agent.display_title.as_deref(),
        agent.title.as_deref(),
        agent.name.as_deref(),
        agent.display_agent.as_deref(),
        agent.agent.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find(|value| value.to_lowercase().contains(query))
}

fn search_agent_session_candidates(
    candidates: Vec<(crate::api::schema::AgentInfo, String)>,
    query: &str,
    limit: usize,
    mut bytes_remaining: u64,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> (Vec<crate::api::schema::AgentSearchHit>, bool) {
    let mut hits = Vec::new();
    let mut partial = false;
    for (index, (agent, title)) in candidates.iter().cloned().enumerate() {
        if cancelled.is_some_and(|active| !active.load(std::sync::atomic::Ordering::Acquire)) {
            break;
        }
        if hits.len() >= limit || bytes_remaining == 0 {
            partial = true;
            break;
        }
        if let Some(hit) = search_agent_session_files_bounded(
            agent,
            title,
            query,
            &mut bytes_remaining,
            &mut partial,
            cancelled,
        ) {
            hits.push(hit);
        }
        if bytes_remaining == 0 && index + 1 < candidates.len() {
            partial = true;
            break;
        }
    }
    (hits, partial)
}

fn search_agent_session_files_bounded(
    agent: crate::api::schema::AgentInfo,
    title: String,
    query: &str,
    bytes_remaining: &mut u64,
    partial: &mut bool,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Option<crate::api::schema::AgentSearchHit> {
    if cancelled.is_some_and(|active| !active.load(std::sync::atomic::Ordering::Acquire)) {
        return None;
    }
    let transcript = load_agent_transcript_bounded(&agent, *bytes_remaining);
    if let Some((transcript, bytes_read)) = transcript {
        *partial |= bytes_read >= *bytes_remaining;
        *bytes_remaining = bytes_remaining.saturating_sub(bytes_read);
        if let Some(context) = matching_context(&transcript.content, query) {
            return Some(crate::api::schema::AgentSearchHit {
                target: agent.pane_id.clone(),
                title,
                source: crate::api::schema::AgentSearchSource::Session,
                path: Some(transcript.path),
                line: matching_line(&transcript.content, query),
                context,
                preview: trailing_lines(&transcript.content, 5),
                agent,
            });
        }
    }

    let cwd = agent.cwd.as_deref().or(agent.foreground_cwd.as_deref())?;
    for folder in [".local", "out"] {
        if *bytes_remaining == 0 {
            *partial = true;
            break;
        }
        let root = std::path::Path::new(cwd).join(folder);
        let (found, bytes_read, budget_exhausted) =
            search_text_files_bounded(&root, query, *bytes_remaining, cancelled);
        *partial |= budget_exhausted;
        *bytes_remaining = bytes_remaining.saturating_sub(bytes_read);
        if let Some((path, line, context, preview)) = found {
            return Some(crate::api::schema::AgentSearchHit {
                target: agent.pane_id.clone(),
                title,
                source: crate::api::schema::AgentSearchSource::Path,
                path: Some(path),
                line: Some(line),
                context,
                preview,
                agent,
            });
        }
    }
    None
}

fn agent_search_hit(
    agent: crate::api::schema::AgentInfo,
    title: String,
    source: crate::api::schema::AgentSearchSource,
    path: Option<String>,
    line: Option<u32>,
    context: Vec<String>,
    preview: Vec<String>,
) -> crate::api::schema::AgentSearchHit {
    crate::api::schema::AgentSearchHit {
        target: agent.pane_id.clone(),
        title,
        source,
        path,
        line,
        context,
        preview,
        agent,
    }
}

fn load_agent_transcript_bounded(
    agent: &crate::api::schema::AgentInfo,
    max_bytes: u64,
) -> Option<(AgentTranscript, u64)> {
    use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind, PersistedAgentSession};
    let session = agent.agent_session.as_ref()?;
    let limit = max_bytes.min(8 * 1024 * 1024);
    if limit == 0 {
        return None;
    }
    if session.kind == AgentSessionRefKind::Path {
        let path = std::path::PathBuf::from(&session.value);
        let metadata = std::fs::metadata(&path).ok()?;
        let mut file = std::fs::File::open(&path).ok()?;
        use std::io::{Read, Seek, SeekFrom};
        let count = metadata.len().min(limit);
        file.seek(SeekFrom::Start(metadata.len().saturating_sub(count)))
            .ok()?;
        let mut bytes = Vec::with_capacity(count.min(usize::MAX as u64) as usize);
        file.take(count).read_to_end(&mut bytes).ok()?;
        let bytes_read = bytes.len() as u64;
        return Some((
            AgentTranscript {
                path: path.display().to_string(),
                content: String::from_utf8_lossy(&bytes).into_owned(),
            },
            bytes_read,
        ));
    }
    let cwd = agent.cwd.as_deref().or(agent.foreground_cwd.as_deref())?;
    let persisted = PersistedAgentSession {
        source: session.source.clone(),
        agent: session.agent.clone(),
        session_ref: AgentSessionRef {
            kind: session.kind,
            value: session.value.clone(),
        },
    };
    let (transcript, bytes_read) = crate::app::settled_view::load_transcript_limited(
        &persisted,
        std::path::Path::new(cwd),
        limit,
    )?;
    let content = transcript
        .turns
        .into_iter()
        .map(|(role, text)| format!("{role}: {text}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some((
        AgentTranscript {
            path: transcript.source,
            content,
        },
        bytes_read,
    ))
}

fn matching_context(text: &str, query: &str) -> Option<Vec<String>> {
    let lines = text.lines().collect::<Vec<_>>();
    let matched = lines
        .iter()
        .position(|line| line.to_lowercase().contains(query))?;
    let start = matched.saturating_sub(1);
    let end = (matched + 2).min(lines.len());
    Some(
        lines[start..end]
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
    )
}

fn matching_line(text: &str, query: &str) -> Option<u32> {
    text.lines()
        .position(|line| line.to_lowercase().contains(query))
        .and_then(|line| u32::try_from(line + 1).ok())
}

fn trailing_lines(text: &str, count: usize) -> Vec<String> {
    let lines = text.lines().collect::<Vec<_>>();
    lines
        .iter()
        .skip(lines.len().saturating_sub(count))
        .map(|line| (*line).to_owned())
        .collect()
}

struct AgentTranscript {
    path: String,
    content: String,
}

fn load_agent_transcript(agent: &crate::api::schema::AgentInfo) -> Option<AgentTranscript> {
    use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind, PersistedAgentSession};
    const MAX_TRANSCRIPT_BYTES: u64 = 8 * 1024 * 1024;

    let session = agent.agent_session.as_ref()?;
    if session.kind == AgentSessionRefKind::Path {
        let path = std::path::PathBuf::from(&session.value);
        let metadata = std::fs::metadata(&path).ok()?;
        let mut file = std::fs::File::open(&path).ok()?;
        use std::io::{Read, Seek, SeekFrom};
        let start = metadata.len().saturating_sub(MAX_TRANSCRIPT_BYTES);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_TRANSCRIPT_BYTES)
            .read_to_end(&mut bytes)
            .ok()?;
        return Some(AgentTranscript {
            path: path.display().to_string(),
            content: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    let cwd = agent.cwd.as_deref().or(agent.foreground_cwd.as_deref())?;
    let session = PersistedAgentSession {
        source: session.source.clone(),
        agent: session.agent.clone(),
        session_ref: AgentSessionRef {
            kind: session.kind,
            value: session.value.clone(),
        },
    };
    let transcript =
        crate::app::settled_view::load_transcript(&session, std::path::Path::new(cwd))?;
    let content = transcript
        .turns
        .into_iter()
        .map(|(role, text)| format!("{role}: {text}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(AgentTranscript {
        path: transcript.source,
        content,
    })
}

fn search_text_files(
    root: &std::path::Path,
    query: &str,
) -> Option<(String, u32, Vec<String>, Vec<String>)> {
    search_text_files_bounded(root, query, 4_000_000, None).0
}

fn search_text_files_bounded(
    root: &std::path::Path,
    query: &str,
    max_total_bytes: u64,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> (Option<AgentFileSearchMatch>, u64, bool) {
    const MAX_FILES: usize = 200;
    const MAX_FILE_BYTES: u64 = 512_000;
    const MAX_DIRECTORY_ENTRIES: usize = 4_096;
    const MAX_DEPTH: usize = 8;
    let max_total_bytes = max_total_bytes.min(4_000_000);
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    let mut entries_seen = 0usize;
    let mut bytes_read = 0u64;
    let mut budget_exhausted = false;
    while let Some((path, depth)) = pending.pop() {
        if bytes_read >= max_total_bytes
            || cancelled.is_some_and(|active| !active.load(std::sync::atomic::Ordering::Acquire))
        {
            budget_exhausted |= bytes_read >= max_total_bytes && !pending.is_empty();
            break;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if depth >= MAX_DEPTH || entries_seen >= MAX_DIRECTORY_ENTRIES {
                budget_exhausted = true;
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            let remaining_entries = MAX_DIRECTORY_ENTRIES - entries_seen;
            let mut entries = entries;
            for entry in (&mut entries).take(remaining_entries).flatten() {
                if cancelled
                    .is_some_and(|active| !active.load(std::sync::atomic::Ordering::Acquire))
                {
                    break;
                }
                entries_seen += 1;
                pending.push((entry.path(), depth + 1));
            }
            if entries_seen >= MAX_DIRECTORY_ENTRIES && entries.next().is_some() {
                budget_exhausted = true;
            }
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > MAX_FILE_BYTES || visited >= MAX_FILES {
            budget_exhausted = true;
            continue;
        }
        visited += 1;
        use std::io::Read;
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut bytes = Vec::new();
        let allowance = MAX_FILE_BYTES.min(max_total_bytes.saturating_sub(bytes_read));
        if file.take(allowance).read_to_end(&mut bytes).is_err() {
            continue;
        }
        bytes_read = bytes_read.saturating_add(bytes.len() as u64);
        budget_exhausted |= (bytes.len() as u64) < allowance && metadata.len() > bytes.len() as u64;
        let content = String::from_utf8_lossy(&bytes);
        let Some(line) = matching_line(&content, query) else {
            continue;
        };
        let Some(context) = matching_context(&content, query) else {
            continue;
        };
        return (
            Some((
                path.display().to_string(),
                line,
                context,
                trailing_lines(&content, 5),
            )),
            bytes_read,
            budget_exhausted,
        );
    }
    (None, bytes_read, budget_exhausted)
}

#[cfg(test)]
mod agent_search_tests {
    use super::*;

    fn session_candidates(
        root: &std::path::Path,
        count: usize,
    ) -> Vec<(crate::api::schema::AgentInfo, String)> {
        (0..count)
            .map(|index| {
                let cwd = root.join(format!("agent-{index}"));
                std::fs::create_dir_all(cwd.join(".local")).expect("create candidate folder");
                if index + 1 == count {
                    std::fs::write(cwd.join(".local/notes.md"), "laterneedle appears here")
                        .expect("write later candidate match");
                }
                let pane_id = format!("pane-{index}");
                let agent: crate::api::schema::AgentInfo =
                    serde_json::from_value(serde_json::json!({
                        "terminal_id": format!("terminal-{index}"),
                        "agent_status": "working",
                        "workspace_id": "workspace",
                        "tab_id": "tab",
                        "pane_id": pane_id,
                        "focused": false,
                        "revision": 1,
                        "display_title": format!("unrelated task {index}"),
                        "cwd": cwd,
                    }))
                    .expect("agent fixture deserializes");
                (agent, format!("unrelated task {index}"))
            })
            .collect()
    }

    fn session_fixture_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn api_session_search_finds_a_match_after_twenty_candidates() {
        let root = session_fixture_root("agent-api-search");
        std::fs::create_dir_all(&root).expect("create API search fixture");
        let (hits, partial) = search_agent_session_candidates(
            session_candidates(&root, 121),
            "laterneedle",
            100,
            MAX_AGENT_SEARCH_BYTES,
            None,
        );
        assert!(!partial);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "pane-120");
        std::fs::remove_dir_all(root).expect("remove API search fixture");
    }

    #[test]
    fn finder_session_search_finds_a_match_after_twenty_candidates() {
        let root = session_fixture_root("agent-finder-search");
        std::fs::create_dir_all(&root).expect("create finder search fixture");
        let (hits, partial) = search_agent_session_candidates(
            session_candidates(&root, 21),
            "laterneedle",
            100,
            MAX_AGENT_SEARCH_BYTES,
            None,
        );
        assert!(!partial);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "pane-20");
        std::fs::remove_dir_all(root).expect("remove finder search fixture");
    }

    #[test]
    fn session_search_marks_exhausted_file_budget_as_partial() {
        let root = session_fixture_root("agent-partial-search");
        std::fs::create_dir_all(&root).expect("create partial search fixture");
        let (hits, partial) = search_agent_session_candidates(
            session_candidates(&root, 21),
            "laterneedle",
            100,
            1,
            None,
        );
        assert!(hits.is_empty());
        assert!(partial);
        std::fs::remove_dir_all(root).expect("remove partial search fixture");
    }

    #[test]
    fn matching_context_returns_two_or_three_neighboring_lines() {
        assert_eq!(
            matching_context("one\ntwo\nNeedle here\nfour\nfive", "needle"),
            Some(vec!["two".into(), "Needle here".into(), "four".into()])
        );
        assert_eq!(
            matching_context("Needle\ntwo", "needle"),
            Some(vec!["Needle".into(), "two".into()])
        );
        assert_eq!(matching_context("one\ntwo", "missing"), None);
    }

    #[tokio::test]
    async fn app_processes_an_api_request_while_agent_session_search_is_pending() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        defer_agent_search_response(
            app.event_tx.clone(),
            respond_to,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            move |_| {
                started_tx.send(()).expect("signal slow search start");
                release_rx.recv().expect("release slow search");
                "deferred search response".into()
            },
        );
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("session search worker started");

        let other_response = app.handle_api_request(crate::api::schema::Request {
            id: "while-search-pending".into(),
            method: crate::api::schema::Method::AgentList(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let other_success: crate::api::schema::SuccessResponse =
            serde_json::from_str(&other_response).expect("concurrent API response is valid");
        assert!(matches!(
            other_success.result,
            crate::api::schema::ResponseResult::AgentList { .. }
        ));
        assert!(response_rx.try_recv().is_err());

        release_tx.send(()).expect("release session search");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), app.event_rx.recv())
            .await
            .expect("search completion event arrives")
            .expect("app event channel remains open");
        app.handle_internal_event_with_render_impact(event);
        assert_eq!(
            response_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("deferred API response"),
            "deferred search response"
        );
    }

    #[tokio::test]
    async fn cancelled_agent_search_drops_its_deferred_response() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let worker_active = active.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        defer_agent_search_response(app.event_tx.clone(), respond_to, active, move |_| {
            started_tx.send(()).expect("signal slow search start");
            release_rx.recv().expect("release slow search");
            done_tx.send(()).expect("signal search work complete");
            "stale response".into()
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("session search worker started");
        worker_active.store(false, std::sync::atomic::Ordering::Release);
        release_tx.send(()).expect("release session search");
        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("session search worker exits");
        tokio::task::yield_now().await;
        assert!(app.event_rx.try_recv().is_err());
        assert!(response_rx.try_recv().is_err());
    }

    #[test]
    fn search_text_files_reports_path_and_one_based_line() {
        let root = std::env::temp_dir().join(format!(
            "herdr-agent-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create search fixture");
        let path = root.join("notes.md");
        std::fs::write(&path, "heading\nfound token\ntrailing").expect("write search fixture");

        let found = search_text_files(&root, "token").expect("match fixture");
        assert_eq!(found.0, path.display().to_string());
        assert_eq!(found.1, 2);
        assert_eq!(found.2, ["heading", "found token", "trailing"]);
        assert_eq!(found.3, ["heading", "found token", "trailing"]);

        std::fs::remove_dir_all(root).expect("remove search fixture");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{
            AgentFocusParams, AgentFocusStatusParams, AgentRef, AgentStatus, ErrorBody,
            ErrorResponse, Method, PaneMoveDestination, PaneMoveParams, RemoteControlContext,
            RemoteFocusState, RemoteForegroundProcess, Request, ResponseResult, SplitDirection,
            SuccessResponse,
        },
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app
    }

    #[derive(Debug)]
    struct FakeRemoteFocusTransport;

    impl crate::app::remote_focus::RemoteFocusTransport for FakeRemoteFocusTransport {
        fn accepts_remote_host(&self, _host: &str) -> bool {
            true
        }

        fn remote_host_ready(&self, _host: &str) -> bool {
            true
        }

        fn start(
            &mut self,
            operation_id: &str,
            agent_ref: &AgentRef,
            _proxy_pane_id: &str,
            _channels: crate::pane::RemoteProxyChannels,
            event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        ) -> Result<(), ErrorBody> {
            let transition = if agent_ref.agent.ends_with("p3") {
                crate::app::remote_focus::RemoteFocusTransition::Active(Box::new(remote_context()))
            } else {
                crate::app::remote_focus::RemoteFocusTransition::Failed(ErrorBody {
                    code: "connection_lost".into(),
                    message: "fake transport ended".into(),
                })
            };
            event_tx
                .try_send(crate::events::AppEvent::RemoteFocusTransition {
                    operation_id: operation_id.to_string(),
                    transition: Box::new(transition),
                })
                .map_err(|_| ErrorBody {
                    code: "connection_lost".into(),
                    message: "fake event queue closed".into(),
                })
        }
    }

    #[derive(Debug)]
    struct IdentityUnavailableTransport;

    impl crate::app::remote_focus::RemoteFocusTransport for IdentityUnavailableTransport {
        fn accepts_remote_host(&self, _host: &str) -> bool {
            true
        }

        fn remote_host_ready(&self, _host: &str) -> bool {
            false
        }

        fn start(
            &mut self,
            _operation_id: &str,
            _agent_ref: &AgentRef,
            _proxy_pane_id: &str,
            _channels: crate::pane::RemoteProxyChannels,
            _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        ) -> Result<(), ErrorBody> {
            Err(ErrorBody {
                code: "connection_lost".into(),
                message: "identity preflight was bypassed".into(),
            })
        }
    }

    fn remote_context() -> RemoteControlContext {
        RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: "w1".into(),
            tab_id: "t2".into(),
            pane_id: "w1:p3".into(),
            terminal_id: "terminal-id".into(),
            cwd: "/work/repo".into(),
            foreground_cwd: "/work/repo".into(),
            tty: "/dev/pts/4".into(),
            foreground_process: RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into(), "run".into()],
                cwd: "/work/repo".into(),
            },
            detected_agent: "claude".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 9,
            revision: 41,
            context_epoch: 12,
        }
    }

    fn mark_agent(app: &mut App, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let terminal_id = app.state.workspaces[ws_idx].tabs[app.state.workspaces[ws_idx]
            .find_tab_index_for_pane(pane_id)
            .expect("agent tab")]
        .panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("agent terminal")
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);
    }

    fn assert_list_display_title_matches_sidebar(
        app: &mut App,
        pane_id: crate::layout::PaneId,
        expected: &str,
    ) {
        let response = app.handle_agent_list("titles".into());
        let success: SuccessResponse = serde_json::from_str(&response).expect("agent list");
        let ResponseResult::AgentList { agents } = success.result else {
            panic!("expected agent list response");
        };
        let agent = agents.into_iter().next().expect("one agent");
        let sidebar_title = crate::ui::sidebar_thread_entries(&app.state)
            .into_iter()
            .find(|entry| {
                entry
                    .local_target()
                    .is_some_and(|target| target.pane_id == pane_id)
            })
            .and_then(|entry| entry.primary_tab_label.clone())
            .expect("sidebar title");
        assert_eq!(agent.display_title.as_deref(), Some(expected));
        assert_eq!(agent.display_title.as_deref(), Some(sidebar_title.as_str()));
    }

    #[test]
    fn agent_list_display_title_uses_manual_pane_label() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        mark_agent(&mut app, 0, pane_id);
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_manual_label("host pane label".into());
        terminal.set_terminal_title(Some("ignored terminal title".into()));
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                work_title: Some("ignored work title".into()),
                ..Default::default()
            })
            .unwrap();

        assert_list_display_title_matches_sidebar(&mut app, pane_id, "host pane label");
    }

    #[test]
    fn agent_list_display_title_filters_tilde_cwd_with_host_home() {
        let _env_lock = crate::integration::integration_env_lock();
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        mark_agent(&mut app, 0, pane_id);
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .expect("test host HOME or USERPROFILE");
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = std::path::PathBuf::from(home).join("fleet-title-project");
        terminal.set_terminal_title(Some("~/fleet-title-project".into()));
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                work_title: Some("host work title".into()),
                ..Default::default()
            })
            .unwrap();

        assert_list_display_title_matches_sidebar(&mut app, pane_id, "host work title");
    }

    #[test]
    fn agent_list_display_title_uses_work_title_fallback() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        mark_agent(&mut app, 0, pane_id);
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("agent terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                work_title: Some("prompt-derived host title".into()),
                ..Default::default()
            })
            .unwrap();

        assert_list_display_title_matches_sidebar(&mut app, pane_id, "prompt-derived host title");
    }

    fn assert_agent_list_refs(app: &mut App, expected: &[(usize, crate::layout::PaneId)]) {
        let mut expected = expected
            .iter()
            .map(|(ws_idx, pane_id)| {
                (
                    app.state.agent_host_name.clone(),
                    app.public_pane_id(*ws_idx, *pane_id)
                        .expect("expected public pane id"),
                )
            })
            .collect::<Vec<_>>();
        expected.sort();

        let response = app.handle_agent_list("list".into());
        let success: SuccessResponse = serde_json::from_str(&response).expect("agent list");
        let ResponseResult::AgentList { agents } = success.result else {
            panic!("expected agent list response");
        };
        let mut actual = agents
            .into_iter()
            .map(|agent| {
                let agent_ref = agent.agent_ref.expect("local agent ref");
                (agent_ref.host, agent_ref.agent)
            })
            .collect::<Vec<_>>();
        actual.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn agent_list_uses_the_server_self_name_and_public_pane_id() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".to_string();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);
        let expected_pane_id = app.public_pane_id(0, pane_id).expect("public pane id");

        let response = app.handle_agent_list("req".to_string());
        let success: SuccessResponse = serde_json::from_str(&response).expect("agent list");
        let ResponseResult::AgentList { agents } = success.result else {
            panic!("expected agent list response");
        };
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0].agent_ref.as_ref(),
            Some(
                &crate::api::schema::AgentRef::new("laptop", expected_pane_id)
                    .expect("valid expected agent reference")
            )
        );
    }

    #[test]
    fn agent_list_rejects_stale_identity_after_source_workspace_removal() {
        let mut app = app_with_agent();
        app.state.workspaces.push(Workspace::test_new("target"));
        app.state.ensure_test_terminals();
        app.state.agent_host_name = "laptop".into();
        let source_pane = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0].panes[&source_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&source_terminal)
            .expect("source terminal")
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);
        app.state.refresh_local_agent_panel_identities();
        let stale_ref = app.state.local_agent_panel_identities[&source_pane]
            .agent_ref
            .clone();
        let source_id = app.public_pane_id(0, source_pane).expect("source pane id");
        let target_pane = app.state.workspaces[1].tabs[0].root_pane;
        let target_tab = app.public_tab_id(1, 0).expect("target tab id");
        let target_id = app.public_pane_id(1, target_pane).expect("target pane id");

        let response = app.handle_pane_move(
            "move".into(),
            PaneMoveParams {
                pane_id: source_id,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab,
                    target_pane_id: Some(target_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).expect("pane move");
        assert!(matches!(success.result, ResponseResult::PaneMove { .. }));
        assert_eq!(app.state.workspaces.len(), 1);
        let current_ref = crate::api::schema::AgentRef::new(
            "laptop",
            app.public_pane_id(0, source_pane).expect("moved pane id"),
        )
        .expect("current agent ref");
        assert_ne!(stale_ref, current_ref);

        let response = app.handle_agent_list("list".into());
        let success: SuccessResponse = serde_json::from_str(&response).expect("agent list");
        let ResponseResult::AgentList { agents } = success.result else {
            panic!("expected agent list response");
        };
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_ref.as_ref(), Some(&current_ref));
    }

    #[test]
    fn agent_list_rejects_stale_public_pane_id_in_the_same_workspace() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".into();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        mark_agent(&mut app, 0, pane_id);
        app.state.refresh_local_agent_panel_identities();
        let stale_ref = app.state.local_agent_panel_identities[&pane_id]
            .agent_ref
            .clone();

        app.state.workspaces[0]
            .public_pane_numbers
            .insert(pane_id, 2);
        let current_ref = crate::api::schema::AgentRef::new(
            "laptop",
            app.public_pane_id(0, pane_id).expect("current pane id"),
        )
        .expect("current agent ref");
        assert_ne!(stale_ref, current_ref);

        assert_agent_list_refs(&mut app, &[(0, pane_id)]);
    }

    #[test]
    fn agent_list_identity_cache_handles_pane_create_and_close() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".into();
        let original = app.state.workspaces[0].tabs[0].root_pane;
        mark_agent(&mut app, 0, original);
        app.state.refresh_local_agent_panel_identities();

        let created = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.state.ensure_test_terminals();
        mark_agent(&mut app, 0, created);
        assert_agent_list_refs(&mut app, &[(0, original), (0, created)]);

        assert!(!app.state.workspaces[0].remove_pane(original));
        assert_agent_list_refs(&mut app, &[(0, created)]);
    }

    #[test]
    fn agent_list_identity_cache_handles_pane_move_between_tabs_and_tab_reorder() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".into();
        let moved = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.ensure_test_terminals();
        mark_agent(&mut app, 0, moved);
        app.state.refresh_local_agent_panel_identities();

        let taken = app.state.workspaces[0]
            .take_pane_for_move(moved)
            .expect("movable pane");
        let target_tab = app.state.workspaces[0]
            .find_tab_index_for_pane(target)
            .expect("target tab after source removal");
        assert!(app.state.workspaces[0]
            .insert_moved_pane_into_tab(
                target_tab,
                target,
                taken.moved,
                ratatui::layout::Direction::Vertical,
                0.5,
                false,
                false,
            )
            .is_ok());
        assert_agent_list_refs(&mut app, &[(0, moved)]);

        app.state.workspaces[0].test_add_tab(Some("later"));
        assert!(app.state.workspaces[0].move_tab(0, 2));
        assert_agent_list_refs(&mut app, &[(0, moved)]);
    }

    #[test]
    fn agent_list_identity_cache_handles_cross_workspace_move_reorder_and_removal() {
        let mut app = app_with_agent();
        app.state.workspaces.push(Workspace::test_new("target"));
        app.state.agent_host_name = "laptop".into();
        let moved = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        let target = app.state.workspaces[1].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        mark_agent(&mut app, 0, moved);
        app.state.refresh_local_agent_panel_identities();

        let taken = app.state.workspaces[0]
            .take_pane_for_move(moved)
            .expect("movable pane");
        app.state.workspaces[0].unregister_moved_pane(moved);
        assert!(app.state.workspaces[1]
            .insert_moved_pane_into_tab(
                0,
                target,
                taken.moved,
                ratatui::layout::Direction::Vertical,
                0.5,
                false,
                false,
            )
            .is_ok());
        assert_agent_list_refs(&mut app, &[(1, moved)]);

        assert!(app.state.move_workspace(1, 0));
        assert_agent_list_refs(&mut app, &[(0, moved)]);

        app.state.workspaces.remove(1);
        assert_agent_list_refs(&mut app, &[(0, moved)]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn visible_blocker_overrides_fresh_hook_authority_in_explain_api() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let screen_observed_at = std::time::Instant::now();
        let hook_reported_at = screen_observed_at + std::time::Duration::from_secs(1);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Blocked,
            true,
            false,
            false,
            false,
            false,
            screen_observed_at,
        );
        terminal.apply_closing_task_report(
            None,
            None,
            Some(crate::api::schema::ClosingParseStatus::Ok),
            Some(false),
            hook_reported_at,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            hook_reported_at,
        );
        let screen = include_bytes!(
            "../../../tests/fixtures/agent-detection/claude-native-bash-permission-20260825.txt"
        );
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, screen);
        app.state.insert_test_runtime(pane_id, runtime);
        let target = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_agent_explain("explain".into(), AgentTarget { target });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentExplain { explain } = success.result else {
            panic!("expected agent explain response");
        };
        assert_eq!(explain["screen_detection_skipped"], false);
        assert_eq!(explain["matched_rule"]["id"], "bash_permission_prompt");
        assert_eq!(explain["screen_state"], "blocked");
        assert_eq!(explain["effective_state"], "working");
        assert_eq!(explain["arbitration"], "closing_block_report");
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            AgentState::Working
        );
        assert_eq!(
            app.agent_info(0, pane_id).unwrap().agent_status,
            AgentStatus::Working
        );

        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Blocked,
            visible_blocker: true,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: hook_reported_at + std::time::Duration::from_secs(1),
        });
        let response = app.handle_agent_explain(
            "newer-blocker".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentExplain { explain } = success.result else {
            panic!("expected newer blocker explain response");
        };
        assert_eq!(explain["effective_state"], "blocked");
        assert_eq!(explain["arbitration"], "visible_blocker_over_hook");
        assert_eq!(
            app.agent_info(0, pane_id).unwrap().agent_status,
            AgentStatus::Blocked
        );

        let mut cleared_prompt = b"\x1b[3J\x1b[2J\x1b[H".to_vec();
        cleared_prompt.extend_from_slice(include_bytes!(
            "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
        ));
        app.state.workspaces[0]
            .test_runtimes
            .get(&pane_id)
            .unwrap()
            .test_process_pty_bytes(&cleared_prompt);
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: hook_reported_at + std::time::Duration::from_secs(2),
        });
        let response = app.handle_agent_explain(
            "panel-removed".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentExplain { explain } = success.result else {
            panic!("expected panel-removed explain response");
        };
        assert_eq!(explain["screen_state"], "idle");
        assert_eq!(explain["effective_state"], "unknown");
        assert_eq!(explain["arbitration"], "closing_task_uncertain");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explain_api_reports_screen_and_foreground_working_owners() {
        for (case, visible_working, foreground_process, expected_owner) in [
            ("visible working", true, false, "screen"),
            ("detected working", false, false, "screen"),
            ("foreground process", false, true, "foreground_process"),
        ] {
            let mut app = app_with_agent();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let now = std::time::Instant::now();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.set_hook_authority_at(
                "herdr:claude".into(),
                "claude".into(),
                AgentState::Idle,
                None,
                None,
                None,
                now,
            );
            if foreground_process {
                terminal.set_foreground_process(
                    Some("cargo".into()),
                    true,
                    now + std::time::Duration::from_millis(1),
                );
            } else {
                terminal.set_detected_state_with_screen_signals_at(
                    Some(Agent::Claude),
                    AgentState::Working,
                    false,
                    false,
                    visible_working,
                    false,
                    false,
                    now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
                );
            }
            let screen = include_bytes!(
                "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
            );
            app.state.insert_test_runtime(
                pane_id,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, screen),
            );

            let response = app.handle_agent_explain(
                case.into(),
                AgentTarget {
                    target: app.public_pane_id(0, pane_id).unwrap(),
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::AgentExplain { explain } = success.result else {
                panic!("expected {case} explain response");
            };
            assert_eq!(explain["effective_state"], "working", "{case}");
            assert_eq!(explain["arbitration"], expected_owner, "{case}");
            assert_eq!(
                app.agent_info(0, pane_id).unwrap().agent_status,
                AgentStatus::Working,
                "{case}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unmatched_output_retires_hook_authority_rebaselines_same_state_report() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        for seq in [1, 2] {
            assert_eq!(
                app.handle_internal_event(crate::events::AppEvent::HookStateReported {
                    pane_id,
                    source: "herdr:claude-closing-block".into(),
                    agent_label: "claude".into(),
                    state: AgentState::Blocked,
                    message: None,
                    seq: Some(seq),
                    wait: None,
                    eta_s: None,
                    reported_at: None,
                    session_ref: None,
                    closing_block: None,
                }),
                Some(true)
            );
            if seq == 1 {
                app.terminal_runtimes
                    .get(&terminal_id)
                    .unwrap()
                    .test_mark_detection_content_changed();
            }
        }
        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .hook_authority_output_baseline_for_test(),
            1,
            "an accepted same-state report rearms from current content"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn blocked_closing_report_requests_a_baseline_for_a_no_input_wake() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let screen = include_bytes!(
            "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
        );
        let (runtime, mut detection_events) =
            crate::terminal::TerminalRuntime::test_with_live_detection_screen_bytes(
                pane_id,
                Agent::Claude,
                AgentState::Idle,
                true,
                false,
                false,
                screen,
            );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        assert_eq!(
            app.handle_internal_event(crate::events::AppEvent::HookStateReported {
                pane_id,
                source: "herdr:claude-closing-block".into(),
                agent_label: "claude".into(),
                state: AgentState::Blocked,
                message: None,
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                session_ref: None,
                closing_block: None,
            }),
            Some(true)
        );
        let rescanned = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = detection_events.recv().await.expect("detector event");
                if matches!(event, crate::events::AppEvent::StateChanged { .. }) {
                    break event;
                }
            }
        })
        .await
        .expect("blocked closing report requests a screen rescan");
        let baseline_at = match &rescanned {
            crate::events::AppEvent::StateChanged {
                state: AgentState::Idle,
                observed_at,
                ..
            } => *observed_at,
            _ => panic!("expected the forced idle baseline"),
        };
        let _ = app.handle_internal_event(rescanned);

        let reported_at = app.state.terminals[&terminal_id]
            .status_reported_at()
            .expect("blocked report timestamp");
        assert!(baseline_at >= reported_at);
        let turn_started_at = reported_at + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH;
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: true,
            usage_limited: false,
            process_exited: false,
            observed_at: turn_started_at,
        });

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.raw_agent_state(),
            AgentState::Blocked,
            "a visible Working signal cannot answer a gate without pane input"
        );
        assert_eq!(terminal.hook_authority.as_ref().unwrap().retired_at, None);
        assert_eq!(
            terminal.agent_status_watchdog_deadline(app.state.agent_stale_after),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn closing_block_authority_is_limited_to_live_blocked_gates_quiet_report_wakes_screen() {
        for report_state in [AgentState::Working, AgentState::Idle, AgentState::Unknown] {
            let mut app = app_with_agent();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_detected_state(Some(Agent::Claude), AgentState::Idle);
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .apply_closing_task_report(
                    None,
                    None,
                    Some(crate::api::schema::ClosingParseStatus::Ok),
                    Some(false),
                    std::time::Instant::now(),
                );
            let screen = include_bytes!(
                "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
            );
            let (runtime, mut detection_events) =
                crate::terminal::TerminalRuntime::test_with_live_detection_screen_bytes(
                    pane_id,
                    Agent::Claude,
                    AgentState::Idle,
                    true,
                    false,
                    false,
                    screen,
                );
            app.terminal_runtimes.insert(terminal_id, runtime);
            app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane_id)
                .unwrap()
                .seen = false;
            app.state.active = None;

            assert_eq!(
                app.handle_internal_event(crate::events::AppEvent::HookStateReported {
                    pane_id,
                    source: "herdr:claude-closing-block".into(),
                    agent_label: "claude".into(),
                    state: report_state,
                    message: None,
                    seq: Some(1),
                    wait: None,
                    eta_s: None,
                    reported_at: None,
                    session_ref: None,
                    closing_block: None,
                }),
                Some(true)
            );

            let event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    let event = detection_events.recv().await.expect("detector event");
                    if matches!(event, crate::events::AppEvent::StateChanged { .. }) {
                        break event;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("quiet {report_state:?} closing report must force a real screen scan")
            });
            let crate::events::AppEvent::StateChanged { state, .. } = &event else {
                unreachable!()
            };
            assert_eq!(*state, AgentState::Idle, "no synthetic transition");
            app.handle_internal_event(event);
            assert_eq!(
                app.agent_info(0, pane_id).unwrap().agent_status,
                AgentStatus::Unknown,
                "screen idle cannot complete a task report with missing completion evidence"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closing_block_authority_is_limited_to_live_blocked_gates_explain_matches_effective_state(
    ) {
        for (report_state, expected_status, expected_label, expected_arbitration) in [
            (
                AgentState::Working,
                AgentStatus::Working,
                "working",
                "closing_block_report",
            ),
            (
                AgentState::Idle,
                AgentStatus::Unknown,
                "unknown",
                "closing_task_uncertain",
            ),
            (
                AgentState::Unknown,
                AgentStatus::Unknown,
                "unknown",
                "closing_block_report",
            ),
        ] {
            let mut app = app_with_agent();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_detected_state(Some(Agent::Claude), AgentState::Idle);
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .apply_closing_task_report(
                    None,
                    None,
                    Some(crate::api::schema::ClosingParseStatus::Ok),
                    Some(false),
                    std::time::Instant::now(),
                );
            let screen = include_bytes!(
                "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
            );
            app.terminal_runtimes.insert(
                terminal_id,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, screen),
            );
            app.handle_internal_event(crate::events::AppEvent::HookStateReported {
                pane_id,
                source: "herdr:claude-closing-block".into(),
                agent_label: "claude".into(),
                state: report_state,
                message: None,
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                session_ref: None,
                closing_block: None,
            });
            let target = app.public_pane_id(0, pane_id).unwrap();

            let info = app.agent_info(0, pane_id).unwrap();
            let response = app.handle_agent_explain(
                "before-refresh".into(),
                AgentTarget {
                    target: target.clone(),
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::AgentExplain { explain } = success.result else {
                panic!("expected agent explain response");
            };
            assert_eq!(info.agent_status, expected_status, "{report_state:?}");
            assert_eq!(explain["effective_state"], expected_label);
            assert_eq!(explain["arbitration"], expected_arbitration);

            app.handle_internal_event(crate::events::AppEvent::StateChanged {
                pane_id,
                agent: Some(Agent::Claude),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                usage_limited: false,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });
            let info = app.agent_info(0, pane_id).unwrap();
            let response = app.handle_agent_explain("after-refresh".into(), AgentTarget { target });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::AgentExplain { explain } = success.result else {
                panic!("expected refreshed agent explain response");
            };
            assert_eq!(info.agent_status, AgentStatus::Unknown);
            assert_eq!(explain["effective_state"], "unknown");
            assert_eq!(explain["arbitration"], "closing_task_uncertain");
        }
    }

    #[test]
    fn fresh_hook_state_wins_over_non_blocker_screen_done_projection() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("done-projection").unwrap();
        terminal.set_agent_session_ref_for_session_start(
            "herdr:kimi".into(),
            "kimi".into(),
            Some(session_ref.clone()),
            Some(1),
            Some("startup".into()),
        );
        let reported_at = std::time::Instant::now();
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Idle,
            None,
            Some(session_ref),
            Some(2),
            reported_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            reported_at + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;

        let info = app.agent_info(0, pane_id).unwrap();
        assert_eq!(info.agent_status, AgentStatus::Done);
        assert!(!info.screen_detection_skipped);
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
                80, 24, 0, b"", 1,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let bracketed_started = std::time::Instant::now();
        let response = app.handle_agent_prompt(
            "req".into(),
            AgentPromptParams {
                target: public_pane_id,
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, .. } = success.result else {
            panic!("expected prompted response");
        };
        assert_eq!(agent.name.as_deref(), Some("reviewer"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap(),
            Bytes::from_static(b"\r")
        );
        assert!(bracketed_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        app.lookup_runtime_sender(0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2004l");
        let raw_started = std::time::Instant::now();
        let raw = app.handle_agent_prompt(
            "req-raw".into(),
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let raw: SuccessResponse = serde_json::from_str(&raw).unwrap();
        assert!(matches!(raw.result, ResponseResult::AgentPrompted { .. }));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B"));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap(),
            Bytes::from_static(b"\r")
        );
        assert!(raw_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        let rejected = app.handle_agent_prompt(
            "req-label".into(),
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

        let response = app.handle_agent_prompt(
            "req".into(),
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

        let response = app.handle_agent_prompt(
            "req".into(),
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
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap(),
            Bytes::from_static(b"\r")
        );
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

    #[tokio::test(flavor = "current_thread")]
    async fn agent_prompt_retires_blocked_hook_without_completing_background_agent() {
        let mut app = app_with_agent();
        app.state.active = None;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast_config.delay_seconds = 1;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            Some(1),
        );
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
        let event_start = app.event_hub.current_sequence();

        let response = app.handle_agent_prompt(
            "req".into(),
            AgentPromptParams {
                target: "reviewer".into(),
                text: "continue".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        assert!(rx.try_recv().is_ok());
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            AgentState::Idle
        );
        assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
        assert!(!app.state.pending_agent_notifications.contains_key(&pane_id));
        assert!(!matches!(
            app.state.toast.as_ref().map(|toast| toast.kind),
            Some(crate::app::state::ToastKind::Finished)
        ));
        assert_eq!(
            app.agent_info(0, pane_id).expect("agent info").agent_status,
            AgentStatus::Idle
        );
        let statuses = app
            .event_hub
            .events_after(event_start)
            .into_iter()
            .filter_map(|(_, event)| match event.data {
                crate::api::schema::EventData::PaneAgentStatusChanged { agent_status, .. } => {
                    Some(agent_status)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(statuses, vec![AgentStatus::Idle]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_send_keys_retires_blocked_hook_authority_after_forwarding() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            Some(1),
        );
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_send_keys(
            "req".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["enter".into()],
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(rx.try_recv().is_ok());
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            AgentState::Idle
        );
        assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
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

        let response = app.handle_agent_prompt(
            "req-pending".into(),
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
    fn self_host_agent_ref_matches_local_focus_from_fresh_state() {
        fn fresh_app() -> (App, String, crate::layout::PaneId) {
            let mut app = app_with_agent();
            app.state.agent_host_name = "laptop".into();
            let root_pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let pane_id =
                app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
            app.state.ensure_test_terminals();
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("test terminal")
                .set_detected_state(Some(Agent::Pi), AgentState::Idle);
            app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane_id)
                .expect("test pane")
                .seen = false;
            app.state.workspaces[0].tabs[0]
                .layout
                .focus_pane(root_pane_id);
            app.state.active = None;
            app.state.set_server_mode(Mode::Navigate);
            assert_eq!(
                app.state.workspaces[0].focused_pane_id(),
                Some(root_pane_id)
            );
            let target = app.public_pane_id(0, pane_id).expect("public pane id");
            (app, target, pane_id)
        }

        fn response_without_instance_ids(response: &str) -> serde_json::Value {
            let mut response: serde_json::Value =
                serde_json::from_str(response).expect("focus response");
            let agent = response
                .pointer_mut("/result/agent")
                .and_then(serde_json::Value::as_object_mut)
                .expect("agent info result");
            for field in [
                "agent_ref",
                "terminal_id",
                "workspace_id",
                "tab_id",
                "pane_id",
            ] {
                let _ = agent.remove(field);
            }
            response
        }

        let (mut target_app, target, target_pane_id) = fresh_app();
        let target_response = target_app.handle_api_request(Request {
            id: "same-id".into(),
            method: Method::AgentFocus(AgentFocusParams {
                target: Some(target),
                agent_ref: None,
            }),
        });
        let (mut agent_ref_app, target, agent_ref_pane_id) = fresh_app();
        let agent_ref_response = agent_ref_app.handle_api_request(Request {
            id: "same-id".into(),
            method: Method::AgentFocus(AgentFocusParams {
                target: None,
                agent_ref: Some(AgentRef::new("laptop", target).expect("valid agent reference")),
            }),
        });

        assert_eq!(
            response_without_instance_ids(&agent_ref_response),
            response_without_instance_ids(&target_response)
        );
        for (app, pane_id) in [
            (&target_app, target_pane_id),
            (&agent_ref_app, agent_ref_pane_id),
        ] {
            assert_eq!(app.state.active, Some(0));
            assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));
            assert!(app.state.workspaces[0].tabs[0].panes[&pane_id].seen);
            assert_eq!(app.state.server_mode(), Mode::Navigate);
            assert_eq!(app.state.effective_interaction_mode(), Mode::Terminal);
            assert_eq!(app.remote_focus_operations.len(), 0);
        }
        let parsed: SuccessResponse = serde_json::from_str(&agent_ref_response).expect("response");
        assert!(matches!(parsed.result, ResponseResult::AgentInfo { .. }));
    }

    #[test]
    fn agent_focus_rejects_missing_and_conflicting_target_forms() {
        for params in [
            AgentFocusParams {
                target: None,
                agent_ref: None,
            },
            AgentFocusParams {
                target: Some("w1:p1".into()),
                agent_ref: Some(AgentRef::new("laptop", "w1:p1").expect("valid agent reference")),
            },
        ] {
            let mut app = app_with_agent();
            let response = app.handle_api_request(Request {
                id: "invalid-focus".into(),
                method: Method::AgentFocus(params),
            });
            let error: ErrorResponse = serde_json::from_str(&response).expect("error response");
            assert_eq!(error.error.code, "invalid_request");
            assert!(error.error.message.contains("exactly one"));
        }
    }

    #[test]
    fn remote_focus_rejects_unknown_hosts() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".into();
        let response = app.handle_api_request(Request {
            id: "unknown-host".into(),
            method: Method::AgentFocus(AgentFocusParams {
                target: None,
                agent_ref: Some(AgentRef::new("missing", "w1:p3").expect("valid agent reference")),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).expect("error response");
        assert_eq!(error.error.code, "unknown_host");
        assert_eq!(app.remote_focus_operations.len(), 0);
    }

    #[test]
    fn production_remote_focus_rejects_hosts_absent_from_transport_snapshot() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".into();
        let response = app.handle_api_request(Request {
            id: "remote-focus".into(),
            method: Method::AgentFocus(AgentFocusParams {
                target: None,
                agent_ref: Some(AgentRef::new("buildbox", "w1:p3").expect("valid agent reference")),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).expect("error response");
        assert_eq!(error.error.code, "unknown_host");
        assert_eq!(app.remote_focus_operations.len(), 0);
    }

    #[test]
    fn remote_focus_rejects_an_unidentified_configured_host_before_opening_a_proxy() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".into();
        app.remote_focus_transport = Box::new(IdentityUnavailableTransport);
        let response = app.handle_api_request(Request {
            id: "identity-unavailable".into(),
            method: Method::AgentFocus(AgentFocusParams {
                target: None,
                agent_ref: Some(AgentRef::new("buildbox", "w1:p3").expect("valid agent reference")),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).expect("error response");
        assert_eq!(error.error.code, "host_unreachable");
        assert!(error.error.message.contains("no unambiguous identity"));
        assert_eq!(app.remote_focus_operations.len(), 0);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn remote_focus_status_rejects_unknown_operation_ids() {
        let mut app = app_with_agent();
        let response = app.handle_api_request(Request {
            id: "unknown-operation".into(),
            method: Method::AgentFocusStatus(AgentFocusStatusParams {
                operation_id: "missing-operation".into(),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).expect("error response");
        assert_eq!(error.error.code, "unknown_operation");
    }

    #[test]
    fn injected_transport_can_drive_remote_focus_active_closed_and_failed() {
        let mut app = app_with_agent();
        app.state.agent_host_name = "laptop".into();
        app.remote_focus_transport = Box::new(FakeRemoteFocusTransport);

        let started = app.handle_api_request(Request {
            id: "fake-active".into(),
            method: Method::AgentFocus(AgentFocusParams {
                target: None,
                agent_ref: Some(AgentRef::new("buildbox", "w1:p3").expect("valid agent reference")),
            }),
        });
        let started: serde_json::Value = serde_json::from_str(&started).expect("started response");
        let operation_id = started["result"]["operation_id"]
            .as_str()
            .expect("operation id")
            .to_string();
        let status = app.handle_api_request(Request {
            id: "fake-active-status".into(),
            method: Method::AgentFocusStatus(AgentFocusStatusParams {
                operation_id: operation_id.clone(),
            }),
        });
        let status: serde_json::Value = serde_json::from_str(&status).expect("active status");
        assert_eq!(status["result"]["state"], "active");
        assert!(status["result"].get("context").is_some());

        app.apply_remote_focus_transition(
            &operation_id,
            crate::app::remote_focus::RemoteFocusTransition::Closed,
        );
        let status = app
            .remote_focus_status(&operation_id)
            .expect("closed status");
        assert_eq!(status.state, RemoteFocusState::Closed);
        assert!(status.context.is_none());

        let failed = app.handle_api_request(Request {
            id: "fake-failed".into(),
            method: Method::AgentFocus(AgentFocusParams {
                target: None,
                agent_ref: Some(AgentRef::new("buildbox", "w1:p4").expect("valid agent reference")),
            }),
        });
        let failed: serde_json::Value = serde_json::from_str(&failed).expect("started response");
        let failed_id = failed["result"]["operation_id"]
            .as_str()
            .expect("operation id")
            .to_string();
        let _ = app.handle_api_request(Request {
            id: "fake-failed-status".into(),
            method: Method::AgentFocusStatus(AgentFocusStatusParams {
                operation_id: failed_id.clone(),
            }),
        });
        app.apply_remote_focus_transition(
            &failed_id,
            crate::app::remote_focus::RemoteFocusTransition::Active(Box::new(remote_context())),
        );
        let status = app.remote_focus_status(&failed_id).expect("failed status");
        assert_eq!(status.state, RemoteFocusState::Failed);
        assert_eq!(
            status.error.as_ref().map(|error| error.code.as_str()),
            Some("connection_lost")
        );
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

    #[test]
    fn agent_rename_keeps_single_token_aliases_out_of_the_agent_session() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-agent-rename-session-name-{}-{nonce}",
            std::process::id(),
        ));
        let transcript_dir = root.join("projects/-tmp-repro");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let transcript = transcript_dir.join("claude-session.jsonl");
        std::fs::write(&transcript, b"{\"type\":\"user\"}\n").unwrap();
        let before = std::fs::read_to_string(&transcript).unwrap();

        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_session_name_write_target(Some(
            crate::work_title::SessionNameWriteTarget::new(
                crate::work_title::WorkTitleProvider::Claude,
                "claude-session".into(),
                transcript.clone(),
            ),
        ));

        let response = app.handle_agent_rename(
            "rename".into(),
            AgentRenameParams {
                target: public_pane_id,
                name: Some("reviewer".into()),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert_eq!(std::fs::read_to_string(&transcript).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_rename_writes_a_trimmed_multiword_session_name() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-agent-rename-write-session-name-{}-{nonce}",
            std::process::id(),
        ));
        let transcript_dir = root.join("projects/-tmp-repro");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let transcript = transcript_dir.join("claude-session.jsonl");
        std::fs::write(&transcript, b"{\"type\":\"user\"}\n").unwrap();

        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_session_name_write_target(Some(
            crate::work_title::SessionNameWriteTarget::new(
                crate::work_title::WorkTitleProvider::Claude,
                "claude-session".into(),
                transcript.clone(),
            ),
        ));

        let response = app.handle_agent_rename(
            "rename".into(),
            AgentRenameParams {
                target: public_pane_id,
                name: Some("  Review billing retries  ".into()),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        assert_eq!(
            app.state.terminals[&terminal_id].agent_name.as_deref(),
            Some("Review billing retries")
        );
        let written = std::fs::read_to_string(&transcript).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(written.lines().last().unwrap()).unwrap();
        assert_eq!(record["customTitle"], "Review billing retries");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_rename_writes_codex_names_and_survives_write_failure() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-agent-rename-codex-session-name-{}-{nonce}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let index = root.join("session_index.jsonl");
        std::fs::write(
            &index,
            b"{\"id\":\"codex-thread\",\"thread_name\":\"Original thread\"}\n",
        )
        .unwrap();

        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-thread").unwrap(),
        });
        terminal.set_session_name_write_target(Some(
            crate::work_title::SessionNameWriteTarget::new(
                crate::work_title::WorkTitleProvider::Codex,
                "codex-thread".into(),
                index.clone(),
            ),
        ));

        let response = app.handle_agent_rename(
            "rename".into(),
            AgentRenameParams {
                target: public_pane_id.clone(),
                name: Some("Review Codex session names".into()),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
        let written = std::fs::read_to_string(&index).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(written.lines().last().unwrap()).unwrap();
        assert_eq!(record["thread_name"], "Review Codex session names");

        std::fs::remove_file(&index).unwrap();
        let failure_response = app.handle_agent_rename(
            "write-failure".into(),
            AgentRenameParams {
                target: public_pane_id,
                name: Some("Keep local Codex name".into()),
            },
        );
        let failure_success: SuccessResponse = serde_json::from_str(&failure_response).unwrap();
        assert!(matches!(
            failure_success.result,
            ResponseResult::AgentInfo { .. }
        ));
        assert_eq!(
            app.state.terminals[&terminal_id].agent_name.as_deref(),
            Some("Keep local Codex name")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
