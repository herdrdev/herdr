//! `agent.transcript`: resolve the agent's omp session file on the app thread,
//! then read and page it on a worker thread, since a cold index of a long
//! session scans a file of over a hundred megabytes.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::agent_transcript::{self, PageRequest};
use crate::api::schema::{AgentTranscriptParams, ErrorBody, Method, Request, ResponseResult};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

/// Where an agent's conversation is recorded, and how the reply names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptSource {
    pub(crate) path: PathBuf,
    pub(crate) agent: String,
    pub(crate) pane_id: String,
}

impl App {
    /// The omp session file behind `target` (an agent name or pane id, as
    /// `agent.read` resolves it). Errors are `agent_not_found` (and the other
    /// target errors) or `transcript_unsupported` for an agent that is not
    /// omp or has not reported its session file.
    pub(crate) fn agent_transcript_source(
        &self,
        target: &str,
    ) -> Result<TranscriptSource, ErrorBody> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(|err| self.agent_target_error_body(err))?;
        let not_found = || ErrorBody {
            code: "agent_not_found".into(),
            message: format!("agent target {target} not found"),
        };
        let unsupported = |message: &str| ErrorBody {
            code: "transcript_unsupported".into(),
            message: message.into(),
        };
        let terminal = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
            .ok_or_else(not_found)?;
        let Some((source, agent, session_ref)) = Self::terminal_resume_source(terminal) else {
            return Err(unsupported("the agent has not reported a session file"));
        };
        if crate::detect::parse_agent_label(&agent) != Some(crate::detect::Agent::Omp) {
            return Err(unsupported("transcripts are read from omp agents only"));
        }
        let path = terminal
            .reported_agent_session_path_for(&source, &agent, &session_ref)
            .map(PathBuf::from)
            .or_else(|| {
                (session_ref.kind == crate::agent_resume::AgentSessionRefKind::Path)
                    .then(|| PathBuf::from(&session_ref.value))
            })
            .ok_or_else(|| unsupported("the agent has not reported a session file"))?;
        let info = self
            .agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(not_found)?;
        Ok(TranscriptSource {
            path,
            agent: info.name.clone().unwrap_or_else(|| agent.clone()),
            pane_id: info.pane_id,
        })
    }

    /// Answers `agent.transcript` through `respond_to` from a worker thread.
    /// Returns false for any other method.
    pub(crate) fn handle_deferred_transcript_request(
        &self,
        request: Request,
        respond_to: Sender<String>,
    ) -> bool {
        let Method::AgentTranscript(params) = request.method else {
            return false;
        };
        let id = request.id;
        let source = match self.agent_transcript_source(&params.target) {
            Ok(source) => source,
            Err(error) => {
                let _ = respond_to.send(encode_error_body(id, error));
                return true;
            }
        };
        let reply = respond_to.clone();
        let reply_id = id.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-agent-transcript", move || {
            let _ = reply.send(transcript_response(reply_id, &source, params));
        });
        if let Err(err) = spawned {
            tracing::warn!(err = %err, "failed to spawn agent transcript reader");
            let _ = respond_to.send(encode_error(
                id,
                "transcript_unavailable",
                format!("could not start the transcript reader: {err}"),
            ));
        }
        true
    }
}

fn transcript_response(
    id: String,
    source: &TranscriptSource,
    params: AgentTranscriptParams,
) -> String {
    let request = PageRequest {
        before: params.before,
        after: params.after,
        limit: params.limit,
        subagent: params.subagent,
    };
    match agent_transcript::read_page(&source.path, &request) {
        Ok(page) => encode_success(
            id,
            ResponseResult::AgentTranscript {
                agent: source.agent.clone(),
                pane_id: source.pane_id.clone(),
                session_id: page.session_id,
                entries: page.entries,
                before: page.before,
                after: page.after,
                pending_ask: page.pending_ask,
                working: page.working,
                model: page.model,
                plan: page.plan,
                recap: page.recap,
                subagents: page.subagents,
            },
        ),
        Err(error) => encode_error(id, error.code(), error.message()),
    }
}
