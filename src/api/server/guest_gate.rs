//! Guest principal gate: an explicit allowlist bound to one agent grant.
//! Everything else answers `guest_forbidden`. Prompts are labeled, the
//! terminal takes no input (a guest may watch it, read its scrollback and,
//! while watching, resize it for everyone viewing), and streams close with
//! `guest_paused` when the agent leaves the foreground or `guest_revoked` on
//! revoke. With `share_gram` the guest also reads the agent's Grams (see
//! `crate::guest::gram`), and any guest may register a phone for the agent's
//! push notifications (see `crate::guest::push`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{
    dispatch_to_app_with_timeout, error_response_json, finish_wait_response, handle_request,
    pane_output_stream, prompt_agent, write_text_line_allow_disconnect, ApiRequestSender,
    ConnectionPrincipal, EventHub,
};
use crate::api::schema::{
    AgentInfo, AgentTranscriptParams, ErrorResponse, GramGetFileChunkParams, GramGetFileParams,
    GramPostParams, GramRelayCall, GramRelayParams, GramRelayPostParams, GramUploadChunkParams,
    GuestAgentProbeParams, GuestAuditEvent, GuestAuditFile, GuestGrantInfo, Method,
    PaneHistoryParams, PanePtyLeaseReleaseParams, PaneReadParams, PaneSetPtySizeParams, PendingAsk,
    ReadIntent, ReadSource, Request, ResponseResult, SuccessResponse, TranscriptEntry,
};
use crate::api::transport::ApiStream;
use crate::guest::GuestPrincipal;
use crate::persist::gram::GramItem;

/// Prompt text cap, before the label is prefixed.
const PROMPT_MAX_BYTES: usize = 32 * 1024;
/// How often a guest stream re-checks revocation and the live-agent check.
pub(super) const WATCH_INTERVAL: Duration = Duration::from_millis(250);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Most scrollback lines one guest `agent.read` returns.
const READ_MAX_LINES: u32 = 1000;
/// A guest's reads and resizes are audited at most once per interval: the app
/// reads the scrollback again on every stream reseed and reconnect, and
/// resizes on every rotation or layout change.
const AUDIT_INTERVAL: Duration = Duration::from_secs(60);
/// Bounds of a guest's terminal size and width-lease TTL. The TTL is the
/// backstop when the guest vanishes without closing its stream.
const RESIZE_COLS: std::ops::RangeInclusive<u16> = 20..=500;
const RESIZE_ROWS: std::ops::RangeInclusive<u16> = 5..=300;
const RESIZE_TTL_MS: std::ops::RangeInclusive<u64> = 1_000..=60_000;
const RESIZE_DEFAULT_TTL_MS: u64 = 30_000;

/// When each throttled event was last audited, per key (a guest id, or a
/// guest id and Gram id for file opens).
static AUDITED: Mutex<Vec<(GuestAuditEvent, String, Instant)>> = Mutex::new(Vec::new());

/// Whether `event` for `key` should be audited now; records it if so.
fn audit_due(event: GuestAuditEvent, key: &str) -> bool {
    let now = Instant::now();
    let mut audited = AUDITED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    audited.retain(|(_, _, at)| now.duration_since(*at) < AUDIT_INTERVAL);
    if audited
        .iter()
        .any(|(seen, seen_key, _)| *seen == event && seen_key == key)
    {
        return false;
    }
    audited.push((event, key.to_string(), now));
    true
}

/// The viewer a guest's width lease and stream are tied to, whatever viewer
/// id the guest sent, so its stream closing drops its lease.
fn guest_viewer_id(guest_id: &str) -> String {
    format!("guest:{guest_id}")
}

/// Drop the guest's width lease now, for a resize that landed after a revoke:
/// its streams close on their own, but only at their next watch.
fn release_lease(api_tx: &ApiRequestSender, guest_id: &str) {
    dispatch_to_app_with_timeout(
        Request {
            id: "guest:release".into(),
            method: Method::PanePtyLeaseRelease(PanePtyLeaseReleaseParams {
                viewer_id: guest_viewer_id(guest_id),
            }),
        },
        api_tx,
        Some(PROBE_TIMEOUT),
    );
}

#[derive(Clone)]
struct GuestContext {
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    running: Arc<AtomicBool>,
}

static CONTEXT: Mutex<Option<GuestContext>> = Mutex::new(None);

/// Called once the API server is up, so `serve` can run guest connections.
pub(super) fn install_context(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    running: Arc<AtomicBool>,
) {
    *CONTEXT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(GuestContext {
        api_tx,
        event_hub,
        running,
    });
}

/// Run one guest API connection. The stream closes when this returns.
pub(crate) fn serve_guest_stream(
    principal: GuestPrincipal,
    stream: std::os::unix::net::UnixStream,
) {
    let context = CONTEXT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let Some(context) = context else {
        tracing::warn!("guest connection before the API server started; closing");
        return;
    };
    if let Err(err) = serve_guest_with(
        &context.api_tx,
        &context.event_hub,
        &context.running,
        principal,
        stream,
    ) {
        tracing::debug!(err = %err, "guest connection ended with an error");
    }
}

fn serve_guest_with(
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    principal: GuestPrincipal,
    stream: std::os::unix::net::UnixStream,
) -> std::io::Result<()> {
    let stream = ApiStream::Local(crate::ipc::LocalStream::from(
        interprocess::os::unix::uds_local_socket::Stream::from(stream),
    ));
    // Guests never drive outbound federation routing.
    super::handle_principal_connection(
        stream,
        api_tx,
        event_hub,
        running,
        None,
        None,
        ConnectionPrincipal::Guest(principal),
        &HashMap::new(),
        // Guests cannot register SSH agents for the server.
        None,
    )
}

/// What the app reports for one pane: its public id, the agent in it (if
/// any), and the `agent.prompt` live-agent check.
pub(super) struct Probe {
    pub pane_id: String,
    pub agent: Option<AgentInfo>,
    pub running: bool,
}

/// Probe by terminal id (any pane) or by agent target (agent panes only).
/// Errors return the app's error line.
pub(super) fn probe_target(
    api_tx: &ApiRequestSender,
    terminal_id: Option<&str>,
    target: Option<&str>,
) -> Result<Probe, String> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: "guest:probe".into(),
            method: Method::GuestAgentProbe(GuestAgentProbeParams {
                terminal_id: terminal_id.map(str::to_string),
                target: target.map(str::to_string),
            }),
        },
        api_tx,
        Some(PROBE_TIMEOUT),
    );
    match serde_json::from_str::<SuccessResponse>(&response) {
        Ok(SuccessResponse {
            result:
                ResponseResult::GuestAgentProbed {
                    pane_id,
                    agent,
                    running,
                },
            ..
        }) => Ok(Probe {
            pane_id,
            agent,
            running,
        }),
        _ => Err(response),
    }
}

/// The grant is the agent with this name and kind in this terminal, local,
/// not archived and not being transferred. The harness session may change:
/// a restarted agent keeps its guest.
fn grant_matches(grant: &GuestGrantInfo, agent: &AgentInfo) -> bool {
    agent.agent.as_deref().is_some_and(|kind| {
        crate::guest::grant_names(grant, &agent.terminal_id, agent.name.as_deref(), kind)
    }) && agent.machine_id.is_none()
        && agent.archived.is_none()
        && agent.session_transfer.is_none()
}

enum GrantState {
    /// The granted agent is running.
    Live(Box<AgentInfo>),
    /// The granted terminal exists but does not run the granted agent now.
    Paused { pane_id: String },
    /// The granted terminal is gone.
    Gone,
}

impl GrantState {
    fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }

    /// The pane the granted terminal is shown in, while it exists.
    fn pane_id(&self) -> Option<&str> {
        match self {
            Self::Live(agent) => Some(&agent.pane_id),
            Self::Paused { pane_id } => Some(pane_id),
            Self::Gone => None,
        }
    }
}

fn grant_state(guest: &GuestPrincipal, api_tx: &ApiRequestSender) -> GrantState {
    let Ok(probe) = probe_target(api_tx, Some(&guest.grant.terminal_id), None) else {
        return GrantState::Gone;
    };
    match probe.agent {
        Some(agent) if probe.running && grant_matches(&guest.grant, &agent) => {
            if let Some(session) = &agent.agent_session {
                note_resumed_session(guest, session);
            }
            GrantState::Live(Box::new(agent))
        }
        _ => GrantState::Paused {
            pane_id: probe.pane_id,
        },
    }
}

/// Follow the granted agent onto a new harness session, auditing `resumed`
/// once when the stored grant changes.
fn note_resumed_session(guest: &GuestPrincipal, session: &crate::api::schema::AgentSessionInfo) {
    if *session == guest.grant.agent_session {
        return;
    }
    match crate::guest::store::update_grant_session(&guest.dir, &guest.guest_id, session) {
        Ok(true) => guest.audit(GuestAuditEvent::Resumed, None, None, None),
        Ok(false) => {}
        Err(err) => tracing::warn!(err = %err, "guest grant session update failed"),
    }
}

/// A target names the grant only by its terminal id, agent name, or the
/// pane id the granted terminal currently occupies. Alias-qualified and
/// other panes' ids never match.
fn names_grant(target: &str, guest: &GuestPrincipal, pane_id: Option<&str>) -> bool {
    target == guest.grant.terminal_id
        || guest.grant.agent_name.as_deref() == Some(target)
        || pane_id == Some(target)
}

/// The only agent fields a guest sees. Titles, cwd, ids of other scopes,
/// session references, tokens and account details stay with the owner.
#[derive(serde::Serialize)]
struct GuestAgentView<'a> {
    terminal_id: &'a str,
    pane_id: &'a str,
    name: Option<&'a str>,
    agent: Option<&'a str>,
    display_agent: Option<&'a str>,
    agent_status: crate::api::schema::AgentStatus,
    guest_running: bool,
}

fn guest_view(agent: &AgentInfo, running: bool) -> serde_json::Value {
    serde_json::to_value(GuestAgentView {
        terminal_id: &agent.terminal_id,
        pane_id: &agent.pane_id,
        name: agent.name.as_deref(),
        agent: agent.agent.as_deref(),
        display_agent: agent.display_agent.as_deref(),
        agent_status: agent.agent_status,
        guest_running: running,
    })
    .unwrap_or_default()
}

/// The granted agent while paused, described from the grant alone.
fn paused_view(guest: &GuestPrincipal, pane_id: &str) -> serde_json::Value {
    serde_json::to_value(GuestAgentView {
        terminal_id: &guest.grant.terminal_id,
        pane_id,
        name: guest.grant.agent_name.as_deref(),
        agent: Some(guest.grant.kind()),
        display_agent: None,
        agent_status: crate::api::schema::AgentStatus::Unknown,
        guest_running: false,
    })
    .unwrap_or_default()
}

/// The guest view of the granted agent while its terminal exists.
fn state_view(guest: &GuestPrincipal, state: &GrantState) -> Option<serde_json::Value> {
    match state {
        GrantState::Live(agent) => Some(guest_view(agent, true)),
        GrantState::Paused { pane_id } => Some(paused_view(guest, pane_id)),
        GrantState::Gone => None,
    }
}

/// Revoked in this process (live registry) or in the store, which another
/// daemon sharing it may have written.
fn is_revoked(guest: &GuestPrincipal, live: &crate::guest::LiveSession) -> bool {
    live.revoked() || crate::guest::store::is_revoked(&guest.dir, &guest.guest_id)
}

fn guest_error(id: &str, code: &str) -> String {
    let message = match code {
        "guest_paused" => "the shared agent is not running",
        "guest_revoked" => "guest access was revoked",
        _ => "guests may not call this method",
    };
    error_response_json(id.to_string(), code, message.to_string())
}

fn success_value(id: &str, result: serde_json::Value) -> String {
    serde_json::json!({"id": id, "result": result}).to_string()
}

/// Rebuild an app reply for a guest. Errors keep only their code and
/// message; a success is rebuilt by `project`, and any other shape is
/// dropped rather than forwarded.
fn guest_reply(
    id: &str,
    response: &str,
    project: impl FnOnce(ResponseResult) -> Option<serde_json::Value>,
) -> String {
    if let Ok(success) = serde_json::from_str::<SuccessResponse>(response) {
        if let Some(result) = project(success.result) {
            return success_value(id, result);
        }
    } else if let Ok(error) = serde_json::from_str::<ErrorResponse>(response) {
        return error_response_json(id.to_string(), &error.error.code, error.error.message);
    }
    error_response_json(
        id.to_string(),
        "internal_error",
        "the host produced a reply guests cannot receive".into(),
    )
}

fn project_pong(result: ResponseResult) -> Option<serde_json::Value> {
    let ResponseResult::Pong {
        version, protocol, ..
    } = result
    else {
        return None;
    };
    // Only what this gate itself serves; the owner's capability set stays private.
    Some(serde_json::json!({
        "type": "pong",
        "version": version,
        "protocol": protocol,
        "capabilities": {"agent_transcript": true},
    }))
}

fn project_prompted(result: ResponseResult) -> Option<serde_json::Value> {
    let ResponseResult::AgentPrompted { agent, delivery } = result else {
        return None;
    };
    let mut value =
        serde_json::json!({"type": "agent_prompted", "agent": guest_view(&agent, true)});
    if let Some(delivery) = delivery {
        value["delivery"] = serde_json::to_value(delivery).ok()?;
    }
    Some(value)
}

fn project_ok(result: ResponseResult) -> Option<serde_json::Value> {
    matches!(result, ResponseResult::Ok {}).then(|| serde_json::json!({"type": "ok"}))
}

/// The rendered scrollback and the fields the app decodes; the workspace,
/// tab and revision stay with the owner.
fn project_read(result: ResponseResult) -> Option<serde_json::Value> {
    let ResponseResult::PaneRead { read } = result else {
        return None;
    };
    Some(serde_json::json!({
        "type": "pane_read",
        "read": {
            "pane_id": read.pane_id,
            "source": read.source,
            "format": read.format,
            "text": read.text,
            "truncated": read.truncated,
        },
    }))
}

/// A history page is the granted pane's own scrollback, like `pane.read`'s
/// text; every field is the wire shape the app decodes.
fn project_history(result: ResponseResult) -> Option<serde_json::Value> {
    let ResponseResult::PaneHistory {
        pane_id,
        history_b64,
        rows,
        cursor,
    } = result
    else {
        return None;
    };
    Some(serde_json::json!({
        "type": "pane_history",
        "pane_id": pane_id,
        "history_b64": history_b64,
        "rows": rows,
        "cursor": cursor,
    }))
}

/// The granted agent's conversation without the owner's private parts: no
/// notes (the owner's fleet traffic), no step details (commands and paths),
/// and asks only the owner may answer.
fn project_transcript(result: ResponseResult) -> Option<serde_json::Value> {
    let ResponseResult::AgentTranscript {
        agent,
        pane_id,
        session_id,
        entries,
        before,
        after,
        pending_ask,
        working,
        model,
        plan,
        recap: _,
    } = result
    else {
        return None;
    };
    let entries: Vec<TranscriptEntry> = entries
        .into_iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::Note { .. } => None,
            TranscriptEntry::Step {
                id,
                ts_ms,
                tool,
                intent,
                status,
                detail: _,
            } => Some(TranscriptEntry::Step {
                id,
                ts_ms,
                tool,
                intent,
                status,
                detail: None,
            }),
            TranscriptEntry::Plan {
                id,
                ts_ms,
                mut items,
            } => {
                for item in &mut items {
                    item.blocker = None;
                }
                Some(TranscriptEntry::Plan { id, ts_ms, items })
            }
            other => Some(other),
        })
        .collect();
    let pending_ask = pending_ask.map(|ask| PendingAsk {
        answerable: false,
        ..ask
    });
    let plan = plan.map(|mut items| {
        for item in &mut items {
            item.blocker = None;
        }
        items
    });
    Some(serde_json::json!({
        "type": "agent_transcript",
        "agent": agent,
        "pane_id": pane_id,
        "session_id": session_id,
        "entries": serde_json::to_value(entries).ok()?,
        "before": before,
        "after": after,
        "pending_ask": serde_json::to_value(pending_ask).ok()?,
        "working": serde_json::to_value(working).ok()?,
        "model": serde_json::to_value(model).ok()?,
        "plan": serde_json::to_value(plan).ok()?,
        // A recap can summarize owner-only notes or step details; do not expose it to guests.
        "recap": serde_json::Value::Null,
    }))
}

/// The size in effect, the same fields the owner's `pane.set_pty_size` returns.
fn project_pty_size(result: ResponseResult) -> Option<serde_json::Value> {
    let ResponseResult::PanePtySize {
        pane_id,
        cols,
        rows,
        locked,
    } = result
    else {
        return None;
    };
    Some(serde_json::json!({
        "type": "pane_pty_size",
        "pane_id": pane_id,
        "cols": cols,
        "rows": rows,
        "locked": locked,
    }))
}

fn project_gram_sent(result: ResponseResult) -> Option<serde_json::Value> {
    let ResponseResult::GramSent { message, .. } = result else {
        return None;
    };
    let mut view = serde_json::json!({
        "id": message.id,
        "from": message.from,
        "to": message.to,
        "text": message.text,
        "created_unix_ms": message.created_unix_ms,
    });
    if let Some(file) = message.file {
        view["file"] = serde_json::json!({
            "name": file.name,
            "size": file.size,
            "mime": file.mime,
            "sha256": file.sha256,
        });
    }
    Some(serde_json::json!({"type": "gram_sent", "message": view}))
}

pub(super) fn serve_request(
    mut stream: ApiStream,
    request: Request,
    guest: &GuestPrincipal,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    let id = request.id.clone();
    let method = crate::api::api_method_name(&request.method);
    let live = crate::guest::register_live(&guest.guest_id);
    if is_revoked(guest, &live) {
        return write_text_line_allow_disconnect(&mut stream, &guest_error(&id, "guest_revoked"));
    }
    let forbidden = |stream: &mut ApiStream| {
        guest.audit(GuestAuditEvent::Denied, Some(method), None, None);
        write_text_line_allow_disconnect(stream, &guest_error(&id, "guest_forbidden"))
    };
    let paused = |stream: &mut ApiStream| {
        guest.audit(GuestAuditEvent::Paused, Some(method), None, None);
        write_text_line_allow_disconnect(stream, &guest_error(&id, "guest_paused"))
    };
    match request.method {
        Method::Ping(params) => {
            let response = handle_request(
                Request {
                    id: id.clone(),
                    method: Method::Ping(params),
                },
                api_tx,
                None,
                None,
                None,
                None,
            );
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_pong),
            )
        }
        Method::AgentList(_) => {
            let state = grant_state(guest, api_tx);
            let agents: Vec<_> = state_view(guest, &state).into_iter().collect();
            let result = serde_json::json!({"type": "agent_list", "agents": agents});
            write_text_line_allow_disconnect(&mut stream, &success_value(&id, result))
        }
        Method::AgentGet(target) => {
            let state = grant_state(guest, api_tx);
            if !names_grant(&target.target, guest, state.pane_id()) {
                return forbidden(&mut stream);
            }
            let Some(agent) = state_view(guest, &state) else {
                return paused(&mut stream);
            };
            let result = serde_json::json!({"type": "agent_info", "agent": agent});
            write_text_line_allow_disconnect(&mut stream, &success_value(&id, result))
        }
        Method::AgentRead(params) => {
            let state = grant_state(guest, api_tx);
            if !names_grant(&params.target, guest, state.pane_id())
                || !matches!(params.source, ReadSource::Recent | ReadSource::Visible)
            {
                return forbidden(&mut stream);
            }
            let GrantState::Live(agent) = state else {
                return paused(&mut stream);
            };
            // A passive pane read of the grant: the snapshot only, never the
            // idle alternate-screen capture that scrolls the agent's TUI.
            let request = Request {
                id: id.clone(),
                method: Method::PaneRead(PaneReadParams {
                    pane_id: agent.pane_id,
                    source: params.source,
                    lines: params.lines.map(|lines| lines.min(READ_MAX_LINES)),
                    format: params.format,
                    strip_ansi: params.strip_ansi,
                    intent: ReadIntent::Passive,
                }),
            };
            let response = dispatch_to_app_with_timeout(request, api_tx, None);
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_read),
            )
        }
        Method::PaneHistory(params) => {
            let state = grant_state(guest, api_tx);
            if !names_grant(&params.pane_id, guest, state.pane_id()) {
                return forbidden(&mut stream);
            }
            let GrantState::Live(agent) = state else {
                return paused(&mut stream);
            };
            // Read-only scrollback of the grant, like `agent.read`: the cursor
            // only resolves against the granted pane it was issued for.
            let request = Request {
                id: id.clone(),
                method: Method::PaneHistory(PaneHistoryParams {
                    pane_id: agent.pane_id,
                    ..params
                }),
            };
            let response = dispatch_to_app_with_timeout(request, api_tx, None);
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_history),
            )
        }
        Method::AgentTranscript(params) => {
            let state = grant_state(guest, api_tx);
            if !names_grant(&params.target, guest, state.pane_id()) {
                return forbidden(&mut stream);
            }
            let GrantState::Live(agent) = state else {
                return paused(&mut stream);
            };
            // The granted agent's own session file; cursors only resolve
            // against the file they were issued for.
            let request = Request {
                id: id.clone(),
                method: Method::AgentTranscript(AgentTranscriptParams {
                    target: agent.pane_id,
                    ..params
                }),
            };
            let response = dispatch_to_app_with_timeout(request, api_tx, None);
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_transcript),
            )
        }
        Method::PaneSetPtySize(params) => {
            let state = grant_state(guest, api_tx);
            let target = params.pane_id.as_deref().unwrap_or_default();
            if !names_grant(target, guest, state.pane_id()) {
                return forbidden(&mut stream);
            }
            let GrantState::Live(agent) = state else {
                return paused(&mut stream);
            };
            // Only while one of the guest's streams watches the grant: the lease
            // then ends with the guest's last stream, and a revoke closes those.
            let request = Request {
                id: id.clone(),
                method: Method::PaneSetPtySize(PaneSetPtySizeParams {
                    pane_id: Some(agent.pane_id),
                    cols: params.cols.clamp(*RESIZE_COLS.start(), *RESIZE_COLS.end()),
                    rows: params.rows.clamp(*RESIZE_ROWS.start(), *RESIZE_ROWS.end()),
                    viewer_id: Some(guest_viewer_id(&guest.guest_id)),
                    ttl_ms: Some(
                        params
                            .ttl_ms
                            .unwrap_or(RESIZE_DEFAULT_TTL_MS)
                            .clamp(*RESIZE_TTL_MS.start(), *RESIZE_TTL_MS.end()),
                    ),
                    require_stream: true,
                    ..params
                }),
            };
            let response = dispatch_to_app_with_timeout(request, api_tx, None);
            // Revoked while the resize was in flight: take its lease back now.
            if is_revoked(guest, &live) {
                release_lease(api_tx, &guest.guest_id);
                return write_text_line_allow_disconnect(
                    &mut stream,
                    &guest_error(&id, "guest_revoked"),
                );
            }
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_pty_size),
            )
        }
        Method::PaneStream(mut params) => {
            let state = grant_state(guest, api_tx);
            if !names_grant(&params.pane_id, guest, state.pane_id()) {
                return forbidden(&mut stream);
            }
            let GrantState::Live(agent) = state else {
                return paused(&mut stream);
            };
            // The guest's own viewer, so this stream closing (paused, revoked or
            // gone) drops the width lease its `pane.set_pty_size` took.
            params.pane_id = agent.pane_id;
            params.viewer_id = Some(guest_viewer_id(&guest.guest_id));
            // Asked before every frame: the live revoke flag and a fresh grant
            // probe each time; the store at most every WATCH_INTERVAL.
            let mut last_store_check = Instant::now();
            let mut watch = |closed: bool| -> Option<String> {
                let store_due = last_store_check.elapsed() >= WATCH_INTERVAL;
                if store_due {
                    last_store_check = Instant::now();
                }
                if live.revoked() || (store_due && is_revoked(guest, &live)) {
                    return Some(guest_error(&id, "guest_revoked"));
                }
                if closed || !grant_state(guest, api_tx).is_live() {
                    guest.audit(GuestAuditEvent::Paused, Some(method), None, None);
                    return Some(guest_error(&id, "guest_paused"));
                }
                None
            };
            pane_output_stream::serve_watched(
                stream,
                id.clone(),
                params,
                api_tx,
                running,
                Some(&mut watch),
            )
        }
        Method::AgentPrompt(mut params) => {
            if params.text.len() > PROMPT_MAX_BYTES {
                return write_text_line_allow_disconnect(
                    &mut stream,
                    &error_response_json(
                        id,
                        "invalid_params",
                        "guest prompts are limited to 32 KiB".into(),
                    ),
                );
            }
            let state = grant_state(guest, api_tx);
            if !names_grant(&params.target, guest, state.pane_id()) {
                return forbidden(&mut stream);
            }
            let GrantState::Live(agent) = state else {
                return paused(&mut stream);
            };
            guest.audit(
                GuestAuditEvent::Prompt,
                Some(method),
                Some(params.text.clone()),
                None,
            );
            params.target = agent.pane_id;
            params.text = format!("{}{}", guest.label(), params.text);
            let response =
                prompt_agent(id.clone(), params, &mut stream, api_tx, event_hub, running)?
                    .map(|response| guest_reply(&id, &response, project_prompted));
            finish_wait_response(&mut stream, response, &id, method, false)
        }
        Method::GramUploadChunk(params) => {
            // The file limits are enforced by the store; this bounds the decode.
            let max_encoded = crate::persist::gram_files::MAX_CHUNK_BYTES.div_ceil(3) * 4;
            if params.data_base64.len() > max_encoded {
                return write_text_line_allow_disconnect(
                    &mut stream,
                    &error_response_json(id, "invalid_params", "upload chunk is too large".into()),
                );
            }
            if !grant_state(guest, api_tx).is_live() {
                return paused(&mut stream);
            }
            let request = Request {
                id: id.clone(),
                method: Method::GramUploadChunk(GramUploadChunkParams {
                    upload_id: guest_upload_id(guest, &params.upload_id),
                    ..params
                }),
            };
            // On a Gram-relay remote the post goes to the coordinator, where
            // the agent reads its Gram, so its upload is staged there too.
            let response = crate::api::reverse::forward_relay(&request)
                .unwrap_or_else(|| dispatch_to_app_with_timeout(request, api_tx, None));
            write_text_line_allow_disconnect(&mut stream, &guest_reply(&id, &response, project_ok))
        }
        Method::GramPost(params) => {
            let GrantState::Live(agent) = grant_state(guest, api_tx) else {
                return paused(&mut stream);
            };
            if params.to.is_some() && params.to != agent.name {
                return forbidden(&mut stream);
            }
            let text = params.text.clone();
            let file = params.file.map(|mut file| {
                file.upload_id = guest_upload_id(guest, &file.upload_id);
                file
            });
            // On a Gram-relay remote the agent reads the coordinator's Gram:
            // post there, and keep the guest's own copy here.
            let relayed = Request {
                id: id.clone(),
                method: Method::GramRelay(GramRelayParams {
                    peer_alias: String::new(),
                    call: GramRelayCall::Post(GramRelayPostParams {
                        text: params.text.clone(),
                        to: agent.name.clone().unwrap_or_default(),
                        guest: guest.name.clone(),
                        file: file.clone(),
                    }),
                }),
            };
            let response = match crate::api::reverse::forward_relay(&relayed) {
                Some(response) => {
                    let local = agent.name.as_deref().unwrap_or_default();
                    mirror_relayed(&guest.dir, &response, &agent.pane_id, local, None);
                    localize_sent(&response, local)
                }
                None => {
                    let request = Request {
                        id: id.clone(),
                        method: Method::GramPost(GramPostParams {
                            text: params.text,
                            to: agent.name,
                            file,
                            from: Some(guest.post_from()),
                        }),
                    };
                    dispatch_to_app_with_timeout(request, api_tx, None)
                }
            };
            audit_post(guest, method, text, &response);
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_gram_sent),
            )
        }
        Method::GramList(params) if guest.shares_gram() => {
            reconcile_copies(guest, api_tx);
            let items = crate::guest::gram::items(guest);
            let reply = match crate::guest::gram::list(
                guest,
                &items,
                params.limit,
                params.before_id.as_deref(),
            ) {
                Ok((messages, has_more)) => success_value(
                    &id,
                    serde_json::json!({
                        "type": "guest_gram_list",
                        "messages": messages,
                        "has_more": has_more,
                    }),
                ),
                Err(message) => error_response_json(id, "invalid_params", message.into()),
            };
            write_text_line_allow_disconnect(&mut stream, &reply)
        }
        Method::GramMarkRead(params) if guest.shares_gram() => {
            let ids: Vec<String> = params.targets().map(str::to_string).collect();
            if ids.is_empty() {
                return write_text_line_allow_disconnect(
                    &mut stream,
                    &error_response_json(id, "invalid_params", "pass id or ids".into()),
                );
            }
            let items = crate::guest::gram::items(guest);
            let visible: HashSet<&str> = items
                .iter()
                .filter(|item| crate::guest::gram::visible(guest, item))
                .map(|item| item.id.as_str())
                .collect();
            if !ids.iter().all(|id| visible.contains(id.as_str())) {
                return forbidden(&mut stream);
            }
            let reply =
                match crate::guest::gram::mark_read(&guest.dir, &guest.guest_id, &ids, &visible) {
                    Ok(()) => success_value(&id, serde_json::json!({"type": "ok"})),
                    Err(err) => error_response_json(id, "guest_store_failed", err.to_string()),
                };
            write_text_line_allow_disconnect(&mut stream, &reply)
        }
        Method::GramGetFile(params) if guest.shares_gram() => {
            let Some(item) = may_fetch(guest, method, &params.id) else {
                return forbidden(&mut stream);
            };
            if let Some(reply) = mirrored_file(guest, &id, &item, None) {
                return write_text_line_allow_disconnect(&mut stream, &reply);
            }
            let request = Request {
                id: id.clone(),
                method: Method::GramGetFile(GramGetFileParams {
                    id: params.id,
                    caller_pane_id: None,
                }),
            };
            let response = dispatch_to_app_with_timeout(request, api_tx, None);
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_file),
            )
        }
        Method::GramGetFileChunk(params) if guest.shares_gram() => {
            let Some(item) = may_fetch(guest, method, &params.id) else {
                return forbidden(&mut stream);
            };
            if let Some(reply) = mirrored_file(guest, &id, &item, Some(params.offset)) {
                return write_text_line_allow_disconnect(&mut stream, &reply);
            }
            let request = Request {
                id: id.clone(),
                method: Method::GramGetFileChunk(GramGetFileChunkParams {
                    caller_pane_id: None,
                    ..params
                }),
            };
            let response = dispatch_to_app_with_timeout(request, api_tx, None);
            write_text_line_allow_disconnect(
                &mut stream,
                &guest_reply(&id, &response, project_file),
            )
        }
        Method::NotificationsRegisterDevice(params) => {
            let reply = match crate::app::registered_device(params) {
                Err(message) => error_response_json(id.clone(), "invalid_params", message.into()),
                Ok(device) => {
                    match crate::guest::push::register(&guest.dir, &guest.guest_id, device) {
                        Ok(()) => success_value(&id, serde_json::json!({"type": "ok"})),
                        Err(err) => error_response_json(
                            id.clone(),
                            "device_registry_save_failed",
                            err.to_string(),
                        ),
                    }
                }
            };
            // Revoked while registering: the revoke already removed this
            // guest's devices, so remove the one that landed after it.
            if is_revoked(guest, &live) {
                if let Err(err) = crate::guest::push::remove_guest(&guest.dir, &guest.guest_id) {
                    tracing::warn!(err = %err, "guest push devices removal failed");
                }
                return write_text_line_allow_disconnect(
                    &mut stream,
                    &guest_error(&id, "guest_revoked"),
                );
            }
            write_text_line_allow_disconnect(&mut stream, &reply)
        }
        Method::NotificationsUnregisterDevice(params) => {
            let reply = match crate::guest::push::unregister(
                &guest.dir,
                &guest.guest_id,
                params.device_token.trim(),
            ) {
                Ok(_) => success_value(&id, serde_json::json!({"type": "ok"})),
                Err(err) => error_response_json(id, "device_registry_save_failed", err.to_string()),
            };
            write_text_line_allow_disconnect(&mut stream, &reply)
        }
        _ => forbidden(&mut stream),
    }
}

/// Message `message_id` when it is in the guest's shared Gram. The first
/// fetch of its file, at whatever offset, is audited as the guest opening it;
/// further fetches of the same file within [`AUDIT_INTERVAL`] are not.
fn may_fetch(guest: &GuestPrincipal, method: &str, message_id: &str) -> Option<GramItem> {
    let item = crate::guest::gram::items(guest)
        .into_iter()
        .find(|item| item.id == message_id)
        .filter(|item| crate::guest::gram::visible(guest, item))?;
    let key = format!("{}\0{message_id}", guest.guest_id);
    if let Some(file) = item
        .file
        .clone()
        .filter(|_| audit_due(GuestAuditEvent::GramFile, &key))
    {
        guest.audit(
            GuestAuditEvent::GramFile,
            Some(method),
            None,
            Some(GuestAuditFile {
                name: file.name,
                size: file.size,
                sha256: file.sha256,
            }),
        );
    }
    Some(item)
}

/// The reply for a file kept as a guest copy (see `crate::guest::mirror`):
/// the whole file, or one chunk from `offset`. `None` for a message of this
/// machine's own store.
fn mirrored_file(
    guest: &GuestPrincipal,
    id: &str,
    item: &GramItem,
    offset: Option<u64>,
) -> Option<String> {
    use base64::Engine as _;
    let mirrored = crate::guest::mirror::get(&guest.dir, &item.id)?;
    let Some(file) = mirrored.file else {
        return Some(error_response_json(
            id.to_string(),
            "no_file",
            "that message has no attached file".into(),
        ));
    };
    let (start, len) = match offset {
        Some(offset) if offset > file.size => {
            return Some(error_response_json(
                id.to_string(),
                "invalid_params",
                "file offset exceeds size".into(),
            ))
        }
        Some(offset) => (offset, crate::persist::gram_files::MAX_CHUNK_BYTES as u64),
        None => (0, file.size),
    };
    let bytes = match crate::guest::mirror::read_file(&guest.dir, &item.id, start, len) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Some(error_response_json(
                id.to_string(),
                "gram_file_error",
                format!("failed to read file: {err}"),
            ))
        }
    };
    let data_base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let result = match offset {
        Some(offset) => ResponseResult::GramFileChunk {
            name: file.name,
            mime: file.mime,
            size: file.size,
            sha256: file.sha256,
            offset,
            data_base64,
        },
        None => ResponseResult::GramFileContent {
            name: file.name,
            mime: file.mime,
            size: file.size,
            data_base64,
        },
    };
    Some(success_value(id, serde_json::to_value(result).ok()?))
}

/// On a Gram-relay remote, after the coordinator accepted a local agent's
/// `gram.send`, keep a guest copy of it when a sharing guest's grant names
/// the sending pane's agent, and notify those guests. The owner's copy and
/// the owner's notifications stay with the coordinator.
///
/// A relayed `gram.delete` that the coordinator accepted drops the copy too.
pub(super) fn after_relayed_gram(request: &Request, response: &str, api_tx: &ApiRequestSender) {
    // No guest was ever invited here: leave the guest directory alone.
    let dir = crate::guest::store::guest_dir();
    if !dir.join("guests.json").exists() {
        return;
    }
    let params = match &request.method {
        Method::GramSend(params) => params,
        Method::GramDelete(delete) => {
            let deleted = matches!(
                serde_json::from_str::<SuccessResponse>(response),
                Ok(SuccessResponse {
                    result: ResponseResult::Ok {},
                    ..
                })
            );
            if deleted {
                let removed = crate::guest::mirror::remove(&dir, &delete.id).and_then(|()| {
                    crate::guest::mirror::forget_witnessed(&dir, |item| item.id != delete.id)
                });
                if let Err(err) = removed {
                    tracing::warn!(err = %err, "guest gram copy removal failed");
                }
            }
            return;
        }
        _ => return,
    };
    let Some(pane) = params.caller_pane_id.as_deref() else {
        return;
    };
    let Ok(Probe {
        agent: Some(agent), ..
    }) = probe_target(api_tx, None, Some(pane))
    else {
        return;
    };
    let Some(local) = agent.name else {
        return;
    };
    let sender = crate::persist::gram::GramSender {
        terminal_id: agent.terminal_id,
        agent: agent.agent,
    };
    if let Some(item) = mirror_relayed(&dir, response, pane, &local, Some(sender)) {
        let cfg = crate::config::Config::load().config.push;
        // The guest's copy names the agent as this machine does: no machine label.
        crate::push::dispatch_guests(cfg, vec![crate::app::gram_push_notification(&item, None)]);
    }
}

/// Most pages of the agent's coordinator Gram one check reads.
const RECONCILE_MAX_PAGES: usize = 20;
const RECONCILE_PAGE: usize = 500;
/// A guest's copies are checked against the coordinator at most this often.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
static RECONCILED: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// Most witnessed Grams one check brings back; the rest follow at the next.
const RECONCILE_MAX_IMPORTS: usize = 50;

/// On a Gram-relay remote, bring the guest's copies in line with the
/// coordinator: drop copies of Grams it no longer has (deleted there, by the
/// owner or the agent), and bring back the guest's Grams it has but this
/// machine kept no copy of (sent or posted while the guest did not share the
/// Gram, or dropped when sharing was turned off).
///
/// It reads the shared agent's own view of the coordinator's Gram, which
/// holds the Grams it sent and those addressed to it, page by page. Anything
/// short of the whole view (the agent not running, the relay down, the page
/// bound hit) changes nothing.
///
/// Only Grams this machine witnessed relaying (see
/// `crate::guest::mirror::witnessed`) are brought back. The coordinator's
/// view cannot tell this machine's agent apart: its records carry a bare
/// `from` name, which an agent on another machine or on the coordinator may
/// share, and no sender binding. The witnessed record carries the binding
/// taken from the sending pane when it was relayed, so what comes back meets
/// the same rule as a live copy (`grant_sent`); the coordinator only
/// confirms the Gram still exists unchanged and serves its file, which is
/// checked against the witnessed size and SHA-256.
fn reconcile_copies(guest: &GuestPrincipal, api_tx: &ApiRequestSender) {
    let Ok(copies) = crate::guest::mirror::load_verified(&guest.dir) else {
        return;
    };
    let witnessed = crate::guest::mirror::witnessed(&guest.dir).unwrap_or_else(|err| {
        tracing::warn!(err = %err, "witnessed guest grams unavailable");
        Vec::new()
    });
    // A copy whose file is gone or damaged counts as not copied: it is
    // fetched again, from its witnessed record or else its own.
    let copied: HashSet<&str> = copies
        .iter()
        .filter(|(_, intact)| *intact)
        .map(|(item, _)| item.id.as_str())
        .collect();
    let witnessed_ids: HashSet<&str> = witnessed.iter().map(|item| item.id.as_str()).collect();
    let missing: Vec<&GramItem> = witnessed
        .iter()
        .chain(
            copies
                .iter()
                .filter(|(item, intact)| !intact && !witnessed_ids.contains(item.id.as_str()))
                .map(|(item, _)| item),
        )
        .filter(|item| {
            crate::guest::gram::visible(guest, item) && !copied.contains(item.id.as_str())
        })
        .collect();
    // No copy to check and nothing to bring back: nothing to ask.
    if missing.is_empty()
        && !copies
            .iter()
            .any(|(item, _)| crate::guest::gram::visible(guest, item))
    {
        return;
    }
    {
        let now = Instant::now();
        let mut reconciled = RECONCILED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reconciled.retain(|(_, at)| now.duration_since(*at) < RECONCILE_INTERVAL);
        if reconciled.iter().any(|(id, _)| *id == guest.guest_id) {
            return;
        }
        reconciled.push((guest.guest_id.clone(), now));
    }
    let GrantState::Live(agent) = grant_state(guest, api_tx) else {
        return;
    };
    let mut present: HashMap<String, crate::api::schema::GramMessageInfo> = HashMap::new();
    let mut before_id = None;
    for _ in 0..RECONCILE_MAX_PAGES {
        let request = Request {
            id: "guest:reconcile".into(),
            method: Method::GramList(crate::api::schema::GramListParams {
                caller_pane_id: Some(agent.pane_id.clone()),
                limit: Some(RECONCILE_PAGE),
                before_id: before_id.take(),
                ..Default::default()
            }),
        };
        let Some(response) = crate::api::reverse::forward_relay(&request) else {
            return;
        };
        let Ok(SuccessResponse {
            result: ResponseResult::GramList {
                messages, has_more, ..
            },
            ..
        }) = serde_json::from_str::<SuccessResponse>(&response)
        else {
            return;
        };
        before_id = messages.last().map(|message| message.id.clone());
        present.extend(
            messages
                .into_iter()
                .map(|message| (message.id.clone(), message)),
        );
        if !has_more {
            apply_reconcile(guest, &agent.pane_id, &present, &missing);
            return;
        }
        if before_id.is_none() {
            return;
        }
    }
}

/// With the agent's whole coordinator view in `present`: drop the stale
/// copies and witnessed records, and bring back the `missing` ones.
fn apply_reconcile(
    guest: &GuestPrincipal,
    pane: &str,
    present: &HashMap<String, crate::api::schema::GramMessageInfo>,
    missing: &[&GramItem],
) {
    let visible = |item: &GramItem| crate::guest::gram::visible(guest, item);
    let pruned = crate::guest::mirror::prune(&guest.dir, |item| {
        !visible(item) || present.contains_key(&item.id)
    })
    .and_then(|()| {
        crate::guest::mirror::forget_witnessed(&guest.dir, |item| {
            !visible(item) || present.contains_key(&item.id)
        })
    });
    if let Err(err) = pruned {
        tracing::warn!(err = %err, "guest gram copies prune failed");
    }
    let unchanged = |item: &GramItem, message: &crate::api::schema::GramMessageInfo| {
        let same = |coordinator: &str, local: &str| {
            coordinator == local || names_local_agent(coordinator, local)
        };
        same(&message.from, &item.from)
            && match (&message.to, &item.to) {
                (Some(coordinator), Some(local)) => same(coordinator, local),
                (None, None) => true,
                _ => false,
            }
            && message.text == item.text
            // The same attachment, whether it is still there or expired since on either
            // side: an expired file is the same message, just without bytes.
            && message
                .file
                .as_ref()
                .or(message.expired_file.as_ref())
                .map(|file| (&file.name, file.size, &file.sha256))
                == item
                    .file
                    .as_ref()
                    .or(item.expired_file.as_ref())
                    .map(|file| (&file.name, file.size, &file.sha256))
    };
    let back = missing.iter().filter(|item| {
        present
            .get(&item.id)
            .is_some_and(|message| unchanged(item, message))
    });
    for item in back.take(RECONCILE_MAX_IMPORTS) {
        let mut item = (*item).clone();
        // Expired on the coordinator since it was witnessed: there are no bytes to pull,
        // so the copy comes back with the attachment's metadata only.
        if present
            .get(&item.id)
            .is_some_and(|message| message.file.is_none() && message.expired_file.is_some())
        {
            item.expired_file = item.file.take().or(item.expired_file);
        }
        let bytes = match &item.file {
            Some(file) => match pull_relayed_file(&item.id, file.size, pane) {
                Some(bytes) => Some(bytes),
                None => continue,
            },
            None => None,
        };
        if let Err(err) = crate::guest::mirror::add(&guest.dir, item, bytes.as_deref()) {
            tracing::warn!(err = %err, "guest gram copy failed");
        }
    }
}

/// Whether `coordinator`, a name in the coordinator's Gram, is this
/// machine's agent `local`. The coordinator names a relaying machine's agents
/// as its federation roster does, `<alias>/<name>`, where agent names never
/// contain `/`; the alias is the coordinator's own for this machine, which
/// this machine does not know, and it only ever talks to that coordinator.
fn names_local_agent(coordinator: &str, local: &str) -> bool {
    !local.is_empty()
        && coordinator
            .split_once('/')
            .is_some_and(|(alias, name)| !alias.is_empty() && name == local)
}

/// A relayed `gram.post` reply with the addressee named as this machine
/// names it, `local`, rather than as the coordinator does.
fn localize_sent(response: &str, local: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(response) else {
        return response.to_string();
    };
    let to = &mut value["result"]["message"]["to"];
    if to.as_str().is_some_and(|to| names_local_agent(to, local)) {
        *to = serde_json::Value::String(local.to_string());
        return value.to_string();
    }
    response.to_string()
}

/// Record the Gram the coordinator answered with `response` as witnessed when
/// an active guest's grant covers it, and keep a guest copy of it when an
/// active sharing guest can see it, pulling its file back through
/// the relay as the agent in `pane`, named `local` on this machine. `sender`
/// binds an agent's own Gram to the pane that sent it. The copy names the
/// agent as this machine does, not as the coordinator (see
/// [`names_local_agent`]). Returns the copy kept.
fn mirror_relayed(
    dir: &std::path::Path,
    response: &str,
    pane: &str,
    local: &str,
    sender: Option<crate::persist::gram::GramSender>,
) -> Option<GramItem> {
    let Ok(SuccessResponse {
        result: ResponseResult::GramSent { message, .. },
        ..
    }) = serde_json::from_str::<SuccessResponse>(response)
    else {
        return None;
    };
    let direction = match message.direction {
        crate::api::schema::GramDirection::AgentToOwner => {
            crate::persist::gram::GramDirection::AgentToOwner
        }
        crate::api::schema::GramDirection::OwnerToAgent => {
            crate::persist::gram::GramDirection::OwnerToAgent
        }
    };
    // The coordinator answered for this machine's agent; it must name it.
    let (from, to) = match direction {
        crate::persist::gram::GramDirection::AgentToOwner => {
            if !names_local_agent(&message.from, local) {
                return None;
            }
            (local.to_string(), None)
        }
        crate::persist::gram::GramDirection::OwnerToAgent => {
            if !message
                .to
                .as_deref()
                .is_some_and(|to| names_local_agent(to, local))
            {
                return None;
            }
            (message.from, Some(local.to_string()))
        }
    };
    let item = GramItem {
        expired_file: message
            .expired_file
            .map(|file| crate::persist::gram::GramFile {
                name: file.name,
                size: file.size,
                mime: file.mime,
                sha256: file.sha256,
            }),
        id: message.id,
        direction,
        from,
        to,
        text: message.text,
        grabbed_by: None,
        grabbed_unix_ms: None,
        created_unix_ms: message.created_unix_ms,
        read_by_owner: false,
        file: message.file.map(|file| crate::persist::gram::GramFile {
            name: file.name,
            size: file.size,
            mime: file.mime,
            sha256: file.sha256,
        }),
        origin_id: String::new(),
        sender,
    };
    let guests = crate::guest::store::load_store(dir).ok()?.guests;
    // Witnessed for any guest the Gram is meant for, sharing or not, so a
    // guest who turns sharing on later gets it back (see `reconcile_copies`).
    if !crate::guest::gram::any_guest_may_see(&guests, &item) {
        return None;
    }
    if let Err(err) = crate::guest::mirror::witness(dir, item.clone()) {
        tracing::warn!(err = %err, "witnessed guest gram record failed");
    }
    if !crate::guest::gram::any_guest_sees(&guests, &item) {
        return None;
    }
    let bytes = match &item.file {
        Some(file) => Some(pull_relayed_file(&item.id, file.size, pane)?),
        None => None,
    };
    match crate::guest::mirror::add(dir, item.clone(), bytes.as_deref()) {
        Ok(()) => Some(item),
        Err(err) => {
            tracing::warn!(err = %err, "guest gram copy failed");
            None
        }
    }
}

/// A relayed Gram's file bytes, fetched chunk by chunk from the coordinator
/// as the agent in `pane`, which can see it.
fn pull_relayed_file(message_id: &str, size: u64, pane: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < size {
        let request = Request {
            id: "guest:mirror".into(),
            method: Method::GramGetFileChunk(GramGetFileChunkParams {
                id: message_id.to_string(),
                offset: bytes.len() as u64,
                caller_pane_id: Some(pane.to_string()),
            }),
        };
        let response = crate::api::reverse::forward_relay(&request)?;
        let Ok(SuccessResponse {
            result: ResponseResult::GramFileChunk { data_base64, .. },
            ..
        }) = serde_json::from_str::<SuccessResponse>(&response)
        else {
            tracing::warn!("guest gram copy: the coordinator refused the file");
            return None;
        };
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .ok()?;
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}

/// File bytes and the metadata the owner's reply carries.
fn project_file(result: ResponseResult) -> Option<serde_json::Value> {
    match result {
        ResponseResult::GramFileContent { .. } | ResponseResult::GramFileChunk { .. } => {
            serde_json::to_value(result).ok()
        }
        _ => None,
    }
}

/// Guests stage uploads in their own namespace, so they cannot append to or
/// attach an owner's staged upload.
fn guest_upload_id(guest: &GuestPrincipal, upload_id: &str) -> String {
    format!("guest-{}-{upload_id}", guest.guest_id)
}

fn audit_post(guest: &GuestPrincipal, method: &str, text: String, response: &str) {
    let Ok(SuccessResponse {
        result: ResponseResult::GramSent { message, .. },
        ..
    }) = serde_json::from_str::<SuccessResponse>(response)
    else {
        return;
    };
    let text = (!text.trim().is_empty()).then_some(text);
    match message.file {
        Some(file) => guest.audit(
            GuestAuditEvent::Upload,
            Some(method),
            text,
            Some(GuestAuditFile {
                name: file.name,
                size: file.size,
                sha256: file.sha256,
            }),
        ),
        None => guest.audit(GuestAuditEvent::Prompt, Some(method), text, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc as std_mpsc;
    use std::thread::JoinHandle;

    use serde_json::{json, Value};

    use crate::api::ApiRequestMessage;
    use crate::app::App;
    use crate::guest::link::host::{Admission as LinkAdmission, GuestHost, HostInfo, LinkState};
    use crate::guest::link::tests::{Device, StubRelay};
    use crate::guest::link::GuestLink;
    use crate::guest::store::tests::TempDir;
    use crate::guest::store::{now_ms, RevokeTarget};
    use crate::guest::{admit_in, Admission};

    type Control = Box<dyn FnOnce(&mut App) + Send>;
    type PtyView = ((u16, u16), Vec<(String, u16, u16, Duration)>);

    /// A real `App` on its own thread with two named agents: the granted
    /// `llm-opt` and `other-agent` on another pane.
    struct Harness {
        api_tx: ApiRequestSender,
        event_hub: EventHub,
        running: Arc<AtomicBool>,
        control: std_mpsc::Sender<Control>,
        pty: std_mpsc::Receiver<(usize, bytes::Bytes)>,
        pane_ids: Vec<String>,
        dir: TempDir,
        /// Runs once on the app thread just before it applies the next
        /// `pane.set_pty_size`.
        before_resize: Arc<Mutex<Option<Control>>>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn start(tag: &str) -> Harness {
        let (api_tx, mut api_rx) = tokio::sync::mpsc::unbounded_channel::<ApiRequestMessage>();
        let (control_tx, control_rx) = std_mpsc::channel::<Control>();
        let (pty_tx, pty_rx) = std_mpsc::channel();
        let (ids_tx, ids_rx) = std_mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let app_running = Arc::clone(&running);
        let before_resize: Arc<Mutex<Option<Control>>> = Arc::default();
        let app_before_resize = Arc::clone(&before_resize);
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let _guard = runtime.enter();
            let (_unused, app_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                app_rx,
                EventHub::default(),
            );
            app.state.workspaces = vec![
                crate::workspace::Workspace::test_new("agent"),
                crate::workspace::Workspace::test_new("other"),
            ];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            let mut ptys = Vec::new();
            let mut ids = Vec::new();
            for (ws_idx, name) in [(0, "llm-opt"), (1, "other-agent")] {
                let pane = app.state.workspaces[ws_idx].tabs[0].root_pane;
                let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane]
                    .attached_terminal_id
                    .clone();
                let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
                terminal.set_agent_name(name.into());
                terminal.set_detected_state(
                    Some(crate::detect::Agent::Pi),
                    crate::detect::AgentState::Idle,
                );
                terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                    source: "herdr".into(),
                    agent: "pi".into(),
                    session_ref: crate::agent_resume::AgentSessionRef::id(format!(
                        "session-{ws_idx}"
                    ))
                    .unwrap(),
                });
                let (runtime, rx) =
                    crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                        80,
                        24,
                        1024 * 1024,
                        &[],
                        4,
                    );
                app.state.insert_test_runtime(pane, runtime);
                ptys.push(rx);
                ids.push(app.public_pane_id(ws_idx, pane).unwrap());
            }
            ids_tx.send(ids).unwrap();
            while app_running.load(Ordering::Acquire) {
                while let Ok(control) = control_rx.try_recv() {
                    control(&mut app);
                }
                for (index, rx) in ptys.iter_mut().enumerate() {
                    while let Ok(bytes) = rx.try_recv() {
                        let _ = pty_tx.send((index, bytes));
                    }
                }
                match api_rx.try_recv() {
                    Ok(message) if matches!(message.request.method, Method::AgentPrompt(_)) => {
                        app.handle_deferred_agent_api_request(message.request, message.respond_to);
                    }
                    Ok(message) if matches!(message.request.method, Method::AgentTranscript(_)) => {
                        app.handle_deferred_transcript_request(message.request, message.respond_to);
                    }
                    Ok(message) => {
                        if matches!(message.request.method, Method::PaneSetPtySize(_)) {
                            let hook = app_before_resize
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .take();
                            if let Some(hook) = hook {
                                hook(&mut app);
                            }
                        }
                        let response = app.handle_api_request(message.request);
                        let _ = message.respond_to.send(response);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        let pane_ids = ids_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Harness {
            api_tx,
            event_hub: EventHub::default(),
            running,
            control: control_tx,
            pty: pty_rx,
            pane_ids,
            dir: TempDir::new(tag),
            before_resize,
            thread: Some(thread),
        }
    }

    impl Harness {
        /// An invite to agent `index`, created through the store.
        fn invite(&self, index: usize, name: &str) -> crate::guest::store::NewInvite {
            self.invite_with(index, name, false)
        }

        fn invite_with(
            &self,
            index: usize,
            name: &str,
            share_gram: bool,
        ) -> crate::guest::store::NewInvite {
            let probe = probe_target(&self.api_tx, None, Some(&self.pane_ids[index]))
                .expect("probe the granted agent");
            assert!(
                probe.running,
                "the granted agent passes the live-agent check"
            );
            let agent = probe.agent.expect("an agent");
            let grant = GuestGrantInfo {
                terminal_id: agent.terminal_id,
                agent_name: agent.name,
                agent_kind: agent.agent,
                agent_session: agent.agent_session.expect("session"),
            };
            crate::guest::store::create_invite(
                &self.dir.0,
                name,
                grant,
                "Jerry",
                "Jerry's Mac Studio",
                3600,
                share_gram,
                now_ms(),
            )
            .unwrap()
        }

        /// Invite through the store and accept it through `admit`, as the
        /// relay link would.
        fn admit(&self) -> GuestPrincipal {
            self.admit_to(0)
        }

        /// Accept a new invite for agent `index` from the same device.
        fn admit_to(&self, index: usize) -> GuestPrincipal {
            self.admit_with(index, false)
        }

        /// Accept a new invite for agent `index` with Gram sharing on or off.
        fn admit_with(&self, index: usize, share_gram: bool) -> GuestPrincipal {
            let invite = self.invite_with(index, "plotarmordev", share_gram);
            let hello = json!({"v": 1, "invite_id": invite.record.invite_id, "secret": invite.secret, "device": "iPhone"});
            match admit_in(self.dir.0.clone(), [4; 32], &hello) {
                Admission::Admitted { principal, .. } => principal,
                Admission::Refused(error) => panic!("refused: {error}"),
            }
        }

        fn open(
            &self,
            guest: &GuestPrincipal,
            request: Value,
        ) -> (BufReader<UnixStream>, JoinHandle<()>) {
            let (mut client, server) = UnixStream::pair().unwrap();
            let (api_tx, event_hub, running, guest) = (
                self.api_tx.clone(),
                self.event_hub.clone(),
                Arc::clone(&self.running),
                guest.clone(),
            );
            let handle = std::thread::spawn(move || {
                let _ = serve_guest_with(&api_tx, &event_hub, &running, guest, server);
            });
            writeln!(client, "{request}").unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            (BufReader::new(client), handle)
        }

        /// One request; every response line until the server closes.
        fn call(&self, guest: &GuestPrincipal, request: Value) -> Vec<Value> {
            let (reader, handle) = self.open(guest, request);
            let lines = reader
                .lines()
                .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
                .collect();
            handle.join().unwrap();
            lines
        }

        fn pty_text(&self, index: usize, wait: Duration) -> String {
            let deadline = Instant::now() + wait;
            let mut text = Vec::new();
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match self.pty.recv_timeout(left) {
                    Ok((pane, bytes)) if pane == index => text.extend_from_slice(&bytes),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            String::from_utf8_lossy(&text).into_owned()
        }

        /// Change the granted terminal's agent on the app thread.
        fn with_granted_terminal(
            &self,
            change: impl FnOnce(&mut crate::terminal::TerminalState) + Send + 'static,
        ) {
            let (done_tx, done_rx) = std_mpsc::channel();
            self.control
                .send(Box::new(move |app: &mut App| {
                    let pane = app.state.workspaces[0].tabs[0].root_pane;
                    let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane]
                        .attached_terminal_id
                        .clone();
                    change(app.state.terminals.get_mut(&terminal_id).unwrap());
                    let _ = done_tx.send(());
                }))
                .unwrap();
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        /// The agent comes back in the same terminal under a new session.
        fn agent_restarts(&self, kind: crate::detect::Agent, name: &str, session: &str) {
            let (name, session) = (name.to_string(), session.to_string());
            self.with_granted_terminal(move |terminal| {
                terminal.set_agent_name(name);
                terminal.set_detected_state(Some(kind), crate::detect::AgentState::Idle);
                terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                    source: "herdr".into(),
                    agent: kind_label(kind).into(),
                    session_ref: crate::agent_resume::AgentSessionRef::id(session).unwrap(),
                });
            });
        }

        /// Print `count` numbered lines (`<prefix>-0` ...) into agent `index`'s
        /// terminal.
        fn print_lines(&self, index: usize, prefix: &str, count: usize) {
            let text: String = (0..count).map(|n| format!("{prefix}-{n}\r\n")).collect();
            let (done_tx, done_rx) = std_mpsc::channel();
            self.control
                .send(Box::new(move |app: &mut App| {
                    let workspace = &app.state.workspaces[index];
                    let pane = workspace.tabs[0].root_pane;
                    workspace.test_runtimes[&pane].test_process_pty_bytes(text.as_bytes());
                    let _ = done_tx.send(());
                }))
                .unwrap();
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        fn agent_exits(&self) {
            self.control
                .send(Box::new(|app: &mut App| {
                    let pane = app.state.workspaces[0].tabs[0].root_pane;
                    let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane]
                        .attached_terminal_id
                        .clone();
                    app.state
                        .terminals
                        .get_mut(&terminal_id)
                        .unwrap()
                        .set_detected_state(None, crate::detect::AgentState::Idle);
                }))
                .unwrap();
        }

        /// Agent `index`'s PTY size as (cols, rows) and the width leases on
        /// its terminal as (viewer, cols, rows, time left).
        fn pty(&self, index: usize) -> PtyView {
            let (done_tx, done_rx) = std_mpsc::channel();
            self.control
                .send(Box::new(move |app: &mut App| {
                    let workspace = &app.state.workspaces[index];
                    let pane = workspace.tabs[0].root_pane;
                    let terminal_id = &workspace.tabs[0].panes[&pane].attached_terminal_id;
                    let (rows, cols) = workspace.test_runtimes[&pane].current_size();
                    let now = Instant::now();
                    let mut leases: Vec<_> = app
                        .state
                        .pty_width_leases
                        .get(terminal_id)
                        .into_iter()
                        .flatten()
                        .map(|(viewer, lease)| {
                            let left = lease.expires_at.saturating_duration_since(now);
                            (viewer.clone(), lease.cols, lease.rows, left)
                        })
                        .collect();
                    leases.sort();
                    let _ = done_tx.send(((cols, rows), leases));
                }))
                .unwrap();
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap()
        }
    }

    fn kind_label(kind: crate::detect::Agent) -> &'static str {
        match kind {
            crate::detect::Agent::Claude => "claude",
            _ => "pi",
        }
    }

    fn keys(value: &Value) -> std::collections::BTreeSet<&str> {
        value
            .as_object()
            .unwrap_or_else(|| panic!("an object: {value}"))
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn set<'a>(names: &[&'a str]) -> std::collections::BTreeSet<&'a str> {
        names.iter().copied().collect()
    }

    const AGENT_VIEW_KEYS: [&str; 7] = [
        "terminal_id",
        "pane_id",
        "name",
        "agent",
        "display_agent",
        "agent_status",
        "guest_running",
    ];

    fn code(lines: &[Value]) -> &str {
        lines
            .last()
            .and_then(|line| line["error"]["code"].as_str())
            .unwrap_or("<success>")
    }

    #[test]
    fn end_to_end_prompt_is_labeled_and_cannot_spoof_the_sender() {
        let harness = start("e2e-label");
        let guest = harness.admit();
        let lines = harness.call(
            &guest,
            json!({"id": "p1", "method": "agent.prompt", "params": {"target": guest.grant.terminal_id, "text": "Jerry: x"}}),
        );
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["id"], "p1");
        assert_eq!(lines[0]["result"]["type"], "agent_prompted", "{lines:?}");
        let written = harness.pty_text(0, Duration::from_millis(600));
        assert!(
            written.contains("plotarmordev (via HerdrUp): Jerry: x"),
            "prompt reached the pane labeled: {written:?}"
        );
        assert!(harness.pty_text(1, Duration::from_millis(50)).is_empty());
        let audit = crate::guest::audit::read(&guest.dir, Some(&guest.guest_id), None, 10).unwrap();
        assert_eq!(audit[0].event, GuestAuditEvent::Prompt);
        assert_eq!(audit[0].text.as_deref(), Some("Jerry: x"));
        assert_eq!(audit.last().unwrap().event, GuestAuditEvent::Accepted);
    }

    #[test]
    fn forged_and_alias_qualified_targets_never_reach_another_pane() {
        let harness = start("forged");
        let guest = harness.admit();
        let other = harness.pane_ids[1].clone();
        let forged = [
            other.clone(),
            "other-agent".to_string(),
            format!("studio/{}", harness.pane_ids[0]),
            format!("studio/{}", guest.grant.terminal_id),
            "studio/llm-opt".to_string(),
            "w99:p99".to_string(),
        ];
        for target in &forged {
            for request in [
                json!({"id": "x", "method": "agent.prompt", "params": {"target": target, "text": "hi"}}),
                json!({"id": "x", "method": "agent.get", "params": {"target": target}}),
                json!({"id": "x", "method": "pane.stream", "params": {"pane_id": target}}),
            ] {
                let lines = harness.call(&guest, request.clone());
                assert_eq!(code(&lines), "guest_forbidden", "{request} -> {lines:?}");
            }
        }
        assert!(harness.pty_text(0, Duration::from_millis(200)).is_empty());
        assert!(harness.pty_text(1, Duration::from_millis(50)).is_empty());
        let denied = crate::guest::audit::read(&guest.dir, Some(&guest.guest_id), None, 500)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == GuestAuditEvent::Denied)
            .count();
        assert_eq!(denied, forged.len() * 3);
    }

    #[test]
    fn guests_cannot_type_resize_or_reach_gram_listing_files_or_guest_rpcs() {
        let harness = start("denied");
        let guest = harness.admit();
        let pane = &harness.pane_ids[0];
        let terminal = &guest.grant.terminal_id;
        for request in [
            json!({"id": "d", "method": "agent.forget", "params": {"target": terminal}}),
            json!({"id": "d", "method": "agent.send_keys", "params": {"target": terminal, "keys": ["enter"]}}),
            json!({"id": "d", "method": "agent.answer", "params": {"target": terminal, "ask_id": "call_1", "option": "Yes"}}),
            json!({"id": "d", "method": "pane.send_text", "params": {"pane_id": pane, "text": "rm -rf /"}}),
            json!({"id": "d", "method": "pane.send_keys", "params": {"pane_id": pane, "keys": ["enter"]}}),
            json!({"id": "d", "method": "pane.send_input", "params": {"pane_id": pane, "text": "x"}}),
            json!({"id": "d", "method": "pane.resize", "params": {"pane_id": pane, "direction": "right"}}),
            json!({"id": "d", "method": "pane.input.stream", "params": {"pane_id": pane}}),
            json!({"id": "d", "method": "gram.list", "params": {}}),
            json!({"id": "d", "method": "gram.get_file", "params": {"id": "gram-1"}}),
            json!({"id": "d", "method": "guest.list", "params": {}}),
            json!({"id": "d", "method": "guest.invite.create", "params": {"target": terminal, "name": "friend", "owner_name": "Jerry", "machine_label": "Mac"}}),
            json!({"id": "d", "method": "guest.revoke", "params": {"guest_id": guest.guest_id}}),
            json!({"id": "d", "method": "guest.audit", "params": {}}),
            json!({"id": "d", "method": "server.stop", "params": {}}),
        ] {
            let lines = harness.call(&guest, request.clone());
            assert_eq!(code(&lines), "guest_forbidden", "{request} -> {lines:?}");
        }
        assert!(harness.pty_text(0, Duration::from_millis(200)).is_empty());
        assert!(!crate::guest::store::is_revoked(
            &guest.dir,
            &guest.guest_id
        ));
    }

    #[test]
    fn agent_list_shows_only_the_grant_and_pauses_when_the_agent_exits() {
        let harness = start("list");
        let guest = harness.admit();
        let list = harness.call(
            &guest,
            json!({"id": "l", "method": "agent.list", "params": {}}),
        );
        let agents = list[0]["result"]["agents"].as_array().unwrap().clone();
        assert_eq!(agents.len(), 1, "{list:?}");
        assert_eq!(agents[0]["terminal_id"], guest.grant.terminal_id.as_str());
        assert_eq!(agents[0]["guest_running"], true);
        assert_eq!(
            harness.call(
                &guest,
                json!({"id": "g", "method": "agent.get", "params": {"target": "llm-opt"}})
            )[0]["result"]["agent"]["guest_running"],
            true
        );

        harness.agent_exits();
        std::thread::sleep(Duration::from_millis(50));
        let list = harness.call(
            &guest,
            json!({"id": "l", "method": "agent.list", "params": {}}),
        );
        let agents = list[0]["result"]["agents"].as_array().unwrap();
        assert_eq!(agents[0]["guest_running"], false, "{list:?}");
        let prompt = harness.call(
            &guest,
            json!({"id": "p", "method": "agent.prompt", "params": {"target": "llm-opt", "text": "hi"}}),
        );
        assert_eq!(code(&prompt), "guest_paused");
        let stream = harness.call(
            &guest,
            json!({"id": "s", "method": "pane.stream", "params": {"pane_id": guest.grant.terminal_id}}),
        );
        assert_eq!(code(&stream), "guest_paused");
        assert!(harness.pty_text(0, Duration::from_millis(200)).is_empty());
    }

    #[test]
    fn oversized_prompts_are_refused_before_delivery() {
        let harness = start("cap");
        let guest = harness.admit();
        let lines = harness.call(
            &guest,
            json!({"id": "p", "method": "agent.prompt", "params": {"target": "llm-opt", "text": "x".repeat(PROMPT_MAX_BYTES + 1)}}),
        );
        assert_eq!(code(&lines), "invalid_params");
        assert!(harness.pty_text(0, Duration::from_millis(200)).is_empty());
    }

    /// Open the granted stream and read up to its `stream_started` ack.
    fn open_stream(
        harness: &Harness,
        guest: &GuestPrincipal,
    ) -> (BufReader<UnixStream>, JoinHandle<()>) {
        let (mut reader, handle) = harness.open(
            guest,
            json!({"id": "s", "method": "pane.stream", "params": {"pane_id": guest.grant.terminal_id, "viewer_id": "v"}}),
        );
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let first: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(first["result"]["type"], "stream_started", "{first}");
        (reader, handle)
    }

    /// Read frames until the error line that ends the stream, then EOF.
    fn stream_end(reader: &mut BufReader<UnixStream>) -> Value {
        loop {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "stream ended without an error line"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value.get("error").is_some() {
                let mut rest = String::new();
                assert_eq!(
                    reader.read_line(&mut rest).unwrap(),
                    0,
                    "EOF after the error line"
                );
                return value;
            }
        }
    }

    #[test]
    fn a_live_stream_pauses_within_a_second_of_the_agent_exiting() {
        let harness = start("pause");
        let guest = harness.admit();
        let (mut reader, handle) = open_stream(&harness, &guest);
        let exited_at = Instant::now();
        harness.agent_exits();
        let end = stream_end(&mut reader);
        assert_eq!(end["error"]["code"], "guest_paused");
        assert_eq!(end["id"], "s");
        assert!(
            exited_at.elapsed() < Duration::from_secs(1),
            "{:?}",
            exited_at.elapsed()
        );
        handle.join().unwrap();
        let audit = crate::guest::audit::read(&guest.dir, Some(&guest.guest_id), None, 5).unwrap();
        assert_eq!(audit[0].event, GuestAuditEvent::Paused);
    }

    #[test]
    fn revoke_closes_a_live_stream_and_refuses_later_requests() {
        let harness = start("revoke");
        let guest = harness.admit();
        let (mut reader, handle) = open_stream(&harness, &guest);
        let closed =
            crate::guest::revoke_at(harness.dir.0.clone(), RevokeTarget::Guest(&guest.guest_id))
                .unwrap();
        assert_eq!(closed, Some(1));
        let end = stream_end(&mut reader);
        assert_eq!(end["error"]["code"], "guest_revoked");
        handle.join().unwrap();
        let ping = harness.call(&guest, json!({"id": "p", "method": "ping", "params": {}}));
        assert_eq!(code(&ping), "guest_revoked");
    }

    #[test]
    fn guest_agent_views_expose_only_the_safe_fields() {
        let harness = start("projection");
        let guest = harness.admit();
        let allowed: std::collections::BTreeSet<&str> = [
            "terminal_id",
            "pane_id",
            "name",
            "agent",
            "display_agent",
            "agent_status",
            "guest_running",
        ]
        .into();
        let list = harness.call(
            &guest,
            json!({"id": "l", "method": "agent.list", "params": {}}),
        );
        let get = harness.call(
            &guest,
            json!({"id": "g", "method": "agent.get", "params": {"target": "llm-opt"}}),
        );
        for agent in [&list[0]["result"]["agents"][0], &get[0]["result"]["agent"]] {
            let keys: std::collections::BTreeSet<&str> = agent
                .as_object()
                .unwrap_or_else(|| panic!("agent object: {list:?} {get:?}"))
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, allowed, "{agent}");
            assert_eq!(agent["name"], "llm-opt");
            assert_eq!(agent["pane_id"], harness.pane_ids[0].as_str());
        }
    }

    fn read(target: &str, lines: u32) -> Value {
        json!({"id": "r", "method": "agent.read", "params": {"target": target, "source": "recent", "format": "ansi", "lines": lines}})
    }

    #[test]
    fn a_guest_reads_the_granted_scrollback_with_only_the_rendered_fields() {
        let harness = start("read");
        let guest = harness.admit();
        harness.print_lines(0, "history", 60);
        harness.print_lines(1, "owner-secret", 5);
        for target in [
            "llm-opt",
            guest.grant.terminal_id.as_str(),
            harness.pane_ids[0].as_str(),
        ] {
            let lines = harness.call(&guest, read(target, 1000));
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(keys(&lines[0]), set(&["id", "result"]), "{lines:?}");
            let result = &lines[0]["result"];
            assert_eq!(keys(result), set(&["type", "read"]));
            assert_eq!(result["type"], "pane_read");
            let read = &result["read"];
            assert_eq!(
                keys(read),
                set(&["pane_id", "source", "format", "text", "truncated"])
            );
            assert_eq!(read["pane_id"], harness.pane_ids[0].as_str());
            assert_eq!(read["source"], "recent");
            assert_eq!(read["format"], "ansi");
            let text = read["text"].as_str().unwrap();
            // Scrolled past the 24-row screen: the oldest line is history.
            assert!(text.contains("history-0\r\n"), "{text:?}");
            assert!(text.contains("history-59"), "{text:?}");
            assert!(!text.contains("owner-secret"), "{text:?}");
        }
        let visible = harness.call(
            &guest,
            json!({"id": "v", "method": "agent.read", "params": {"target": "llm-opt", "source": "visible", "format": "text"}}),
        );
        let text = visible[0]["result"]["read"]["text"].as_str().unwrap();
        assert!(text.contains("history-59"), "{visible:?}");
        assert!(
            !text.lines().any(|line| line.trim_end() == "history-0"),
            "{visible:?}"
        );
    }

    #[test]
    fn agent_read_is_bound_to_the_grant() {
        let harness = start("read-forged");
        let guest = harness.admit();
        harness.print_lines(1, "owner-secret", 5);
        assert_eq!(
            code(&harness.call(&guest, read("llm-opt", 80))),
            "<success>"
        );
        let other = harness.pane_ids[1].clone();
        for target in [
            other.clone(),
            "other-agent".to_string(),
            format!("studio/{}", harness.pane_ids[0]),
            format!("studio/{}", guest.grant.terminal_id),
            "studio/llm-opt".to_string(),
            "w99:p99".to_string(),
        ] {
            let lines = harness.call(&guest, read(&target, 80));
            assert_eq!(code(&lines), "guest_forbidden", "{target} -> {lines:?}");
        }
        // Only the rendered sources a terminal view needs.
        for source in ["recent_unwrapped", "detection"] {
            let lines = harness.call(
                &guest,
                json!({"id": "r", "method": "agent.read", "params": {"target": "llm-opt", "source": source}}),
            );
            assert_eq!(code(&lines), "guest_forbidden", "{source} -> {lines:?}");
        }
    }

    #[test]
    fn agent_read_returns_at_most_a_thousand_lines() {
        let harness = start("read-clamp");
        let guest = harness.admit();
        harness.print_lines(0, "row", 1500);
        let lines = harness.call(
            &guest,
            json!({"id": "r", "method": "agent.read", "params": {"target": "llm-opt", "source": "recent", "format": "text", "lines": 5000}}),
        );
        let text = lines[0]["result"]["read"]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("a read: {lines:?}"));
        assert!(
            text.lines().count() <= 1000,
            "{} lines",
            text.lines().count()
        );
        assert!(text.contains("row-1499"), "the newest line is kept");
        let rows: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert!(!rows.contains(&"row-400"), "older lines are dropped");
        assert!(rows.contains(&"row-600"), "a full 1000 lines: {}", rows[0]);
    }

    #[test]
    fn agent_read_needs_a_running_agent_and_an_active_grant() {
        let harness = start("read-state");
        let guest = harness.admit();
        assert_eq!(
            code(&harness.call(&guest, read("llm-opt", 80))),
            "<success>"
        );
        harness.agent_exits();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            code(&harness.call(&guest, read("llm-opt", 80))),
            "guest_paused"
        );
        crate::guest::revoke_at(harness.dir.0.clone(), RevokeTarget::Guest(&guest.guest_id))
            .unwrap();
        assert_eq!(
            code(&harness.call(&guest, read("llm-opt", 80))),
            "guest_revoked"
        );
    }

    /// Makes both agents omp agents recording to their own synthetic session
    /// file, before any invite is made for them.
    fn omp_sessions(harness: &Harness) -> Vec<crate::agent_transcript::tests::Fixture> {
        use crate::agent_transcript::tests::{ask_call, call, text, Fixture};

        let mut fixtures = Vec::new();
        for index in 0..2 {
            let mut session = Fixture::new(&format!("guest-gate-{index}"));
            session.user(&format!("owner prompt {index}"));
            session.user("Ana (via HerdrUp): a guest prompt");
            session.assistant(
                vec![
                    text("Working on it."),
                    call(
                        "c1",
                        "bash",
                        Some("Running the tests"),
                        json!({"command": "cargo test --secret-flag"}),
                    ),
                ],
                "toolUse",
            );
            session.result("c1", "bash", false, json!({}));
            session.note(
                "gitmoot-inbox",
                "owner fleet traffic: deploy keys rotated",
                true,
                Value::Null,
            );
            session.assistant(
                vec![ask_call("ask-1", json!([{"question": "Deploy?", "options": [{"label": "Yes"}, {"label": "No"}]}]))],
                "toolUse",
            );
            let path = session.path.to_string_lossy().into_owned();
            let (done_tx, done_rx) = std_mpsc::channel();
            harness
                .control
                .send(Box::new(move |app: &mut App| {
                    let pane = app.state.workspaces[index].tabs[0].root_pane;
                    let terminal_id = app.state.workspaces[index].tabs[0].panes[&pane]
                        .attached_terminal_id
                        .clone();
                    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
                    // Only the recorded session changes, so the grant still
                    // names the agent; the session says omp wrote it.
                    terminal.set_persisted_agent_session(
                        crate::agent_resume::PersistedAgentSession {
                            source: "herdr".into(),
                            agent: "omp".into(),
                            session_ref: crate::agent_resume::AgentSessionRef::path(path).unwrap(),
                        },
                    );
                    let _ = done_tx.send(());
                }))
                .unwrap();
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            fixtures.push(session);
        }
        fixtures
    }

    fn transcript(target: &str) -> Value {
        json!({"id": "t", "method": "agent.transcript", "params": {"target": target}})
    }

    #[test]
    fn a_guest_reads_the_granted_transcript_without_the_owners_private_parts() {
        let harness = start("transcript");
        let _sessions = omp_sessions(&harness);
        let guest = harness.admit();

        // The owner's own view of the same agent has every part.
        let owner = dispatch_to_app_with_timeout(
            Request {
                id: "o".into(),
                method: Method::AgentTranscript(AgentTranscriptParams {
                    target: harness.pane_ids[0].clone(),
                    before: None,
                    after: None,
                    limit: None,
                }),
            },
            &harness.api_tx,
            None,
        );
        let owner: Value = serde_json::from_str(&owner).unwrap();
        let owner_kinds: Vec<&str> = owner["result"]["entries"]
            .as_array()
            .unwrap_or_else(|| panic!("{owner}"))
            .iter()
            .map(|entry| entry["kind"].as_str().unwrap())
            .collect();
        assert!(owner_kinds.contains(&"note"), "{owner}");
        assert_eq!(owner["result"]["pending_ask"]["answerable"], true);
        assert!(owner.to_string().contains("--secret-flag"));

        for target in ["llm-opt", harness.pane_ids[0].as_str()] {
            let lines = harness.call(&guest, transcript(target));
            let result = &lines[0]["result"];
            assert_eq!(result["type"], "agent_transcript", "{lines:?}");
            assert_eq!(result["agent"], owner["result"]["agent"]);
            assert_eq!(result["pane_id"], harness.pane_ids[0]);
            let entries = result["entries"].as_array().unwrap();
            let kinds: Vec<&str> = entries
                .iter()
                .map(|entry| entry["kind"].as_str().unwrap())
                .collect();
            assert_eq!(
                kinds,
                vec!["user", "user", "reply", "step", "ask"],
                "no notes"
            );
            assert_eq!(
                entries[0]["text"], "owner prompt 0",
                "the granted agent's file"
            );
            assert_eq!(entries[1]["author"], "guest");
            assert_eq!(entries[1]["author_name"], "Ana");
            assert_eq!(entries[3]["intent"], "Running the tests");
            assert!(entries[3].get("detail").is_none(), "{}", entries[3]);
            assert!(!lines[0].to_string().contains("--secret-flag"));
            assert!(!lines[0].to_string().contains("deploy keys"));
            assert_eq!(result["pending_ask"]["id"], "ask-1");
            assert_eq!(result["pending_ask"]["answerable"], false);
            // The cursor works for the guest's next poll.
            let after = result["after"].as_str().unwrap();
            let poll = harness.call(
                &guest,
                json!({"id": "t", "method": "agent.transcript", "params": {"target": target, "after": after}}),
            );
            assert_eq!(poll[0]["result"]["entries"], json!([]), "{poll:?}");
        }
    }

    #[test]
    fn guest_transcript_metadata_preserves_progress_without_private_blockers() {
        use crate::agent_transcript::tests::call;
        let harness = start("transcript-metadata");
        let mut sessions = omp_sessions(&harness);
        sessions[0].assistant(
            vec![call("todo-metadata", "todo", None, json!({}))],
            "toolUse",
        );
        sessions[0].result(
            "todo-metadata",
            "todo",
            false,
            json!({"phases": [
                {"name": "Release", "tasks": [
                    {"content": "Review", "status": "blocked", "blocker": "private-review-context"}
                ]}
            ]}),
        );
        let recap_turn = sessions[0].assistant(vec![], "stop");
        sessions[0].push(json!({"type": "model_change", "model": "provider/selected"}));
        sessions[0].push(
            json!({"type": "custom", "customType": "completion-recap", "data": {
                "version": 1, "sessionId": "sess-1", "turnId": recap_turn,
                "state": "available", "text": "private-recap-context"
            }}),
        );
        let owner = dispatch_to_app_with_timeout(
            serde_json::from_value(transcript("llm-opt")).unwrap(),
            &harness.api_tx,
            None,
        );
        assert!(owner.contains("private-review-context"), "{owner}");
        assert!(owner.contains("private-recap-context"), "{owner}");
        let guest = harness.admit();
        let lines = harness.call(&guest, transcript("llm-opt"));
        let result = &lines[0]["result"];
        assert_eq!(result["plan"][0]["status"], "blocked");
        assert_eq!(result["model"]["id"], "selected");
        assert_eq!(result["model"]["provider"], "provider");
        assert_eq!(result["plan"][0]["phase"], "Release");
        assert!(result["plan"][0].get("blocker").is_none());
        assert!(!lines[0].to_string().contains("private-review-context"));
        assert!(result["recap"].is_null());
        assert!(!lines[0].to_string().contains("private-recap-context"));
    }

    #[test]
    fn agent_transcript_is_bound_to_the_grant() {
        let harness = start("transcript-forged");
        let _sessions = omp_sessions(&harness);
        let guest = harness.admit();
        let other = harness.pane_ids[1].clone();
        for target in [
            other,
            "other-agent".to_string(),
            format!("studio/{}", harness.pane_ids[0]),
            "studio/llm-opt".to_string(),
            "w99:p99".to_string(),
        ] {
            let lines = harness.call(&guest, transcript(&target));
            assert_eq!(code(&lines), "guest_forbidden", "{target} -> {lines:?}");
        }
    }

    #[test]
    fn a_guest_pages_only_the_granted_scrollback() {
        use base64::Engine as _;

        let harness = start("history");
        let guest = harness.admit();
        harness.print_lines(0, "history", 60);
        let (mut reader, _handle) = harness.open(
            &guest,
            json!({"id": "s", "method": "pane.stream", "params": {"pane_id": "llm-opt", "viewer_id": "v", "scrollback_lines": 10}}),
        );
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let seed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(seed["frame"], "reset", "{seed}");
        let cursor = seed["history_cursor"]
            .as_str()
            .unwrap_or_else(|| panic!("older rows exist above the keyframe: {seed}"))
            .to_string();
        let page = |target: &str| {
            harness.call(
                &guest,
                json!({"id": "h", "method": "pane.history", "params": {"pane_id": target, "cursor": cursor}}),
            )
        };

        let lines = page("llm-opt");
        assert_eq!(lines.len(), 1, "{lines:?}");
        let result = &lines[0]["result"];
        assert_eq!(
            keys(result),
            set(&["type", "pane_id", "history_b64", "rows", "cursor"])
        );
        assert_eq!(result["type"], "pane_history");
        assert_eq!(result["pane_id"], harness.pane_ids[0].as_str());
        assert_eq!(result["cursor"], Value::Null, "the page reached line 0");
        let history = base64::engine::general_purpose::STANDARD
            .decode(result["history_b64"].as_str().unwrap())
            .unwrap();
        let history = String::from_utf8(history).unwrap();
        assert!(history.contains("history-0\r\n"), "{history:?}");

        for target in [
            harness.pane_ids[1].clone(),
            format!("studio/{}", harness.pane_ids[0]),
        ] {
            assert_eq!(code(&page(&target)), "guest_forbidden", "{target}");
        }
    }

    /// Open the granted stream and read past the ack and the reset seed, so
    /// the next line is whatever the stream sends after that.
    fn open_stream_past_seed(
        harness: &Harness,
        guest: &GuestPrincipal,
    ) -> (BufReader<UnixStream>, JoinHandle<()>) {
        let (mut reader, handle) = open_stream(harness, guest);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let seed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(seed["frame"], "reset", "{seed}");
        (reader, handle)
    }

    /// The very next line is the closing error, then EOF: no frame first.
    fn next_is_close(reader: &mut BufReader<UnixStream>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert!(value.get("error").is_some(), "a frame followed: {value}");
        let mut rest = String::new();
        assert_eq!(
            reader.read_line(&mut rest).unwrap(),
            0,
            "EOF after the error"
        );
        value
    }

    fn write_output(harness: &Harness) {
        crate::api::output_registry::lookup(&harness.pane_ids[0])
            .expect("the granted pane has a live output ring")
            .append(b"owner-only output\r\n");
    }

    fn set_pty_size(target: &str, cols: u16, rows: u16, extra: Value) -> Value {
        let mut params = json!({"pane_id": target, "cols": cols, "rows": rows, "lock": true});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"id": "z", "method": "pane.set_pty_size", "params": params})
    }

    #[test]
    fn a_guest_resizes_the_granted_terminal_under_its_own_clamped_lease() {
        let harness = start("resize");
        let guest = harness.admit();
        let viewer = format!("guest:{}", guest.guest_id);
        let pane = harness.pane_ids[0].as_str();
        let _stream = open_stream(&harness, &guest);

        let lines = harness.call(
            &guest,
            set_pty_size(
                "llm-opt",
                1000,
                1,
                json!({"viewer_id": "owner-mac", "ttl_ms": 999_999_999u64, "cell_width_px": 9, "cell_height_px": 18}),
            ),
        );
        assert_eq!(
            lines,
            vec![
                json!({"id": "z", "result": {"type": "pane_pty_size", "pane_id": pane, "cols": 500, "rows": 5, "locked": true}})
            ]
        );
        let ((cols, rows), leases) = harness.pty(0);
        assert_eq!((cols, rows), (500, 5));
        assert_eq!(leases.len(), 1, "{leases:?}");
        let (lease_viewer, lease_cols, lease_rows, left) = &leases[0];
        assert_eq!(
            lease_viewer, &viewer,
            "the guest's viewer, not the one it sent"
        );
        assert_eq!((*lease_cols, *lease_rows), (500, 5));
        assert!(
            *left <= Duration::from_secs(60) && *left > Duration::from_secs(50),
            "{left:?}"
        );

        // Every target naming the grant; the lower clamps and the TTL floor.
        for target in [guest.grant.terminal_id.as_str(), pane] {
            let lines = harness.call(&guest, set_pty_size(target, 3, 1000, json!({"ttl_ms": 1})));
            assert_eq!(lines[0]["result"]["cols"], 20, "{lines:?}");
            assert_eq!(lines[0]["result"]["rows"], 300, "{lines:?}");
            let (size, leases) = harness.pty(0);
            assert_eq!(size, (20, 300));
            assert_eq!(leases.len(), 1, "one lease, replaced: {leases:?}");
            assert_eq!((leases[0].1, leases[0].2), (20, 300));
            assert!(
                leases[0].3 <= Duration::from_secs(1) && leases[0].3 > Duration::from_millis(500),
                "{leases:?}"
            );
        }

        // No TTL: the default.
        harness.call(&guest, set_pty_size("llm-opt", 100, 30, json!({})));
        let (_, leases) = harness.pty(0);
        assert!(
            leases[0].3 <= Duration::from_secs(30) && leases[0].3 > Duration::from_secs(25),
            "{leases:?}"
        );

        // Release: the guest's lease goes, whatever viewer it names.
        let lines = harness.call(
            &guest,
            set_pty_size(
                "llm-opt",
                90,
                28,
                json!({"lock": false, "viewer_id": "owner-mac"}),
            ),
        );
        assert_eq!(lines[0]["result"]["locked"], false, "{lines:?}");
        assert_eq!(harness.pty(0), ((90, 28), Vec::new()));
        assert_eq!(harness.pty(1), ((80, 24), Vec::new()));
    }

    #[test]
    fn pane_set_pty_size_is_bound_to_a_live_grant() {
        let harness = start("resize-forged");
        let guest = harness.admit();
        let other = harness.pane_ids[1].clone();
        for target in [
            other.clone(),
            "other-agent".to_string(),
            format!("studio/{}", harness.pane_ids[0]),
            format!("studio/{}", guest.grant.terminal_id),
            "studio/llm-opt".to_string(),
            "w99:p99".to_string(),
        ] {
            let lines = harness.call(&guest, set_pty_size(&target, 40, 10, json!({})));
            assert_eq!(code(&lines), "guest_forbidden", "{target} -> {lines:?}");
        }
        let no_target = harness.call(
            &guest,
            json!({"id": "z", "method": "pane.set_pty_size", "params": {"cols": 40, "rows": 10, "lock": true}}),
        );
        assert_eq!(code(&no_target), "guest_forbidden", "{no_target:?}");
        assert_eq!(harness.pty(0), ((80, 24), Vec::new()));
        assert_eq!(harness.pty(1), ((80, 24), Vec::new()));

        harness.agent_exits();
        for lock in [true, false] {
            let lines = harness.call(
                &guest,
                set_pty_size("llm-opt", 40, 10, json!({"lock": lock})),
            );
            assert_eq!(code(&lines), "guest_paused", "lock {lock}: {lines:?}");
        }
        assert_eq!(harness.pty(0), ((80, 24), Vec::new()));
    }

    /// Poll until agent `index` holds no width lease.
    fn wait_for_no_lease(harness: &Harness, index: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !harness.pty(index).1.is_empty() {
            assert!(Instant::now() < deadline, "{:?}", harness.pty(index));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_guests_lease_ends_with_its_stream_or_its_revoke() {
        let harness = start("resize-lease");
        let guest = harness.admit();
        let resize = set_pty_size(
            "llm-opt",
            120,
            40,
            json!({"viewer_id": "v", "ttl_ms": 60_000}),
        );

        // The app closing its stream (it went away) drops the lease.
        let (reader, handle) = open_stream(&harness, &guest);
        assert_eq!(code(&harness.call(&guest, resize.clone())), "<success>");
        assert_eq!(harness.pty(0).1.len(), 1);
        drop(reader);
        handle.join().unwrap();
        wait_for_no_lease(&harness, 0);

        // A revoke closes the stream, which drops the lease.
        let (mut reader, handle) = open_stream(&harness, &guest);
        assert_eq!(code(&harness.call(&guest, resize)), "<success>");
        assert_eq!(harness.pty(0).1.len(), 1);
        crate::guest::revoke_at(harness.dir.0.clone(), RevokeTarget::Guest(&guest.guest_id))
            .unwrap();
        assert_eq!(stream_end(&mut reader)["error"]["code"], "guest_revoked");
        handle.join().unwrap();
        assert!(harness.pty(0).1.is_empty(), "{:?}", harness.pty(0));
    }

    #[test]
    fn a_resize_needs_one_of_the_guests_streams_open() {
        let harness = start("resize-stream");
        let guest = harness.admit();
        let resize = set_pty_size("llm-opt", 120, 40, json!({}));
        let lines = harness.call(&guest, resize.clone());
        assert_eq!(code(&lines), "guest_no_stream", "{lines:?}");
        assert_eq!(harness.pty(0), ((80, 24), Vec::new()));
        let release = harness.call(
            &guest,
            set_pty_size("llm-opt", 80, 24, json!({"lock": false})),
        );
        assert_eq!(code(&release), "<success>", "{release:?}");

        let (reader, handle) = open_stream(&harness, &guest);
        assert_eq!(code(&harness.call(&guest, resize.clone())), "<success>");
        drop(reader);
        handle.join().unwrap();
        wait_for_no_lease(&harness, 0);
        let lines = harness.call(&guest, resize);
        assert_eq!(code(&lines), "guest_no_stream", "{lines:?}");
        assert!(harness.pty(0).1.is_empty(), "{:?}", harness.pty(0));
    }

    #[test]
    fn the_lease_lasts_until_the_guests_last_stream_closes() {
        let harness = start("resize-streams");
        let guest = harness.admit();
        let (old_reader, old_handle) = open_stream(&harness, &guest);
        let (new_reader, new_handle) = open_stream(&harness, &guest);
        let resize = set_pty_size("llm-opt", 120, 40, json!({}));
        assert_eq!(code(&harness.call(&guest, resize)), "<success>");
        // The app remounted its view: the old stream closes, the new one watches.
        drop(old_reader);
        old_handle.join().unwrap();
        let (size, leases) = harness.pty(0);
        assert_eq!((size, leases.len()), ((120, 40), 1), "{leases:?}");
        drop(new_reader);
        new_handle.join().unwrap();
        assert!(harness.pty(0).1.is_empty(), "{:?}", harness.pty(0));
    }

    #[test]
    fn a_resize_that_lands_after_a_revoke_is_taken_back() {
        let harness = start("resize-revoked");
        let guest = harness.admit();
        let (mut reader, handle) = open_stream(&harness, &guest);
        let (dir, guest_id) = (harness.dir.0.clone(), guest.guest_id.clone());
        *harness
            .before_resize
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(Box::new(move |_: &mut App| {
                crate::guest::revoke_at(dir, RevokeTarget::Guest(&guest_id)).unwrap();
            }));
        let lines = harness.call(&guest, set_pty_size("llm-opt", 120, 40, json!({})));
        assert_eq!(code(&lines), "guest_revoked", "{lines:?}");
        assert!(harness.pty(0).1.is_empty(), "{:?}", harness.pty(0));
        assert_eq!(stream_end(&mut reader)["error"]["code"], "guest_revoked");
        handle.join().unwrap();
    }

    #[test]
    fn viewing_the_agent_writes_nothing_to_the_owners_log() {
        // Reading scrollback and resizing are what a watching guest does
        // continuously; logging them buried the owner's activity log.
        let harness = start("view-audit");
        let guest = harness.admit();
        let _stream = open_stream(&harness, &guest);
        for cols in [100, 110, 120] {
            let lines = harness.call(&guest, set_pty_size("llm-opt", cols, 30, json!({})));
            assert_eq!(code(&lines), "<success>", "{lines:?}");
        }
        assert_eq!(
            code(&harness.call(&guest, read("llm-opt", 80))),
            "<success>"
        );
        let audit = crate::guest::audit::read(&guest.dir, Some(&guest.guest_id), None, 50).unwrap();
        let methods: Vec<_> = audit
            .iter()
            .filter_map(|entry| entry.method.as_deref())
            .collect();
        assert!(
            !methods
                .iter()
                .any(|m| *m == "agent.read" || *m == "pane.set_pty_size"),
            "{audit:?}"
        );
    }

    #[test]
    fn no_frame_follows_a_pause_even_with_output_pending() {
        let harness = start("pause-frame");
        let guest = harness.admit();
        let (mut reader, handle) = open_stream_past_seed(&harness, &guest);
        harness.agent_exits();
        write_output(&harness);
        assert_eq!(next_is_close(&mut reader)["error"]["code"], "guest_paused");
        handle.join().unwrap();
    }

    #[test]
    fn no_frame_follows_a_revoke_even_with_output_pending() {
        let harness = start("revoke-frame");
        let guest = harness.admit();
        let (mut reader, handle) = open_stream_past_seed(&harness, &guest);
        crate::guest::revoke_at(harness.dir.0.clone(), RevokeTarget::Guest(&guest.guest_id))
            .unwrap();
        write_output(&harness);
        assert_eq!(next_is_close(&mut reader)["error"]["code"], "guest_revoked");
        handle.join().unwrap();
    }

    #[test]
    fn a_new_invite_for_the_same_terminal_closes_the_old_grants_stream() {
        let harness = start("replace-stream");
        let first = harness.admit_to(0);
        let (mut reader, handle) = open_stream(&harness, &first);
        let second = harness.admit_to(0);
        assert_ne!(first.guest_id, second.guest_id);
        assert_eq!(stream_end(&mut reader)["error"]["code"], "guest_revoked");
        handle.join().unwrap();
        let ping = harness.call(&first, json!({"id": "p", "method": "ping", "params": {}}));
        assert_eq!(code(&ping), "guest_revoked");
    }

    #[test]
    fn uploads_and_posts_need_a_running_agent_and_an_active_grant() {
        let harness = start("gram-state");
        let guest = harness.admit();
        let upload = json!({"id": "u", "method": "gram.upload_chunk", "params": {"upload_id": "up-1", "offset": 0, "data_base64": "aGk="}});
        let post =
            json!({"id": "g", "method": "gram.post", "params": {"text": "hello", "to": "llm-opt"}});
        harness.agent_exits();
        std::thread::sleep(Duration::from_millis(50));
        for request in [&upload, &post] {
            let lines = harness.call(&guest, request.clone());
            assert_eq!(code(&lines), "guest_paused", "{request} -> {lines:?}");
        }
        crate::guest::revoke_at(harness.dir.0.clone(), RevokeTarget::Guest(&guest.guest_id))
            .unwrap();
        for request in [&upload, &post] {
            let lines = harness.call(&guest, request.clone());
            assert_eq!(code(&lines), "guest_revoked", "{request} -> {lines:?}");
        }
    }

    /// Points the Gram store at a scratch config dir for one test.
    struct ConfigHome {
        previous: Option<std::ffi::OsString>,
        dir: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl ConfigHome {
        fn new(tag: &str) -> Self {
            let lock = crate::config::test_config_env_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "herdr-guest-cfg-{tag}-{}-{}",
                std::process::id(),
                now_ms()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", &dir);
            Self {
                previous,
                dir,
                _lock: lock,
            }
        }
    }

    impl Drop for ConfigHome {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn every_guest_success_reply_carries_only_guest_fields() {
        let _config = ConfigHome::new("replies");
        let harness = start("replies");
        let guest = harness.admit();
        let agent_keys = set(&AGENT_VIEW_KEYS);
        let error_keys = set(&["code", "message"]);

        let pong = harness.call(&guest, json!({"id": "p", "method": "ping", "params": {}}));
        assert_eq!(keys(&pong[0]), set(&["id", "result"]));
        assert_eq!(
            keys(&pong[0]["result"]),
            set(&["type", "version", "protocol", "capabilities"])
        );
        assert_eq!(
            pong[0]["result"]["capabilities"],
            json!({"agent_transcript": true}),
            "only what the gate serves"
        );

        let prompt = harness.call(
            &guest,
            json!({"id": "a", "method": "agent.prompt", "params": {"target": "llm-opt", "text": "hi"}}),
        );
        assert_eq!(keys(&prompt[0]), set(&["id", "result"]), "{prompt:?}");
        assert_eq!(
            keys(&prompt[0]["result"]),
            set(&["type", "agent", "delivery"])
        );
        assert_eq!(keys(&prompt[0]["result"]["agent"]), agent_keys);

        // The waiting form ends in a projected success or a bare error.
        let waited = harness.call(
            &guest,
            json!({"id": "w", "method": "agent.prompt", "params": {"target": "llm-opt", "text": "again", "wait": {"until": ["idle", "working", "blocked", "done", "unknown"], "timeout_ms": 400}}}),
        );
        let reply = waited.last().unwrap();
        match reply.get("result") {
            Some(result) => {
                assert_eq!(keys(result), set(&["type", "agent", "delivery"]), "{reply}");
                assert_eq!(keys(&result["agent"]), agent_keys);
            }
            None => assert_eq!(keys(&reply["error"]), error_keys, "{reply}"),
        }

        let upload = harness.call(
            &guest,
            json!({"id": "u", "method": "gram.upload_chunk", "params": {"upload_id": "up-1", "offset": 0, "data_base64": "aGk="}}),
        );
        assert_eq!(upload[0], json!({"id": "u", "result": {"type": "ok"}}));

        let post = harness.call(
            &guest,
            json!({"id": "g", "method": "gram.post", "params": {"text": "notes", "file": {"upload_id": "up-1", "name": "a.txt", "mime": "text/plain"}}}),
        );
        assert_eq!(keys(&post[0]), set(&["id", "result"]), "{post:?}");
        assert_eq!(keys(&post[0]["result"]), set(&["type", "message"]));
        let message = &post[0]["result"]["message"];
        assert_eq!(
            keys(message),
            set(&["id", "from", "to", "text", "created_unix_ms", "file"])
        );
        assert_eq!(
            keys(&message["file"]),
            set(&["name", "size", "mime", "sha256"])
        );
        assert_eq!(message["from"], "plotarmordev (via HerdrUp)");
        assert_eq!(message["to"], "llm-opt");

        let (mut reader, handle) = open_stream(&harness, &guest);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let seed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            keys(&seed),
            set(&["stream", "frame", "seq", "epoch", "cols", "rows", "data_b64"])
        );
        harness.agent_exits();
        let end = stream_end(&mut reader);
        assert_eq!(keys(&end), set(&["id", "error"]));
        assert_eq!(keys(&end["error"]), error_keys);
        handle.join().unwrap();
    }

    #[test]
    fn a_paused_agent_is_still_listed_from_the_grant() {
        let harness = start("paused-view");
        let guest = harness.admit();
        harness.agent_exits();
        let list = harness.call(
            &guest,
            json!({"id": "l", "method": "agent.list", "params": {}}),
        );
        let get = harness.call(
            &guest,
            json!({"id": "g", "method": "agent.get", "params": {"target": guest.grant.terminal_id}}),
        );
        let agents = list[0]["result"]["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1, "{list:?}");
        for agent in [&agents[0], &get[0]["result"]["agent"]] {
            assert_eq!(keys(agent), set(&AGENT_VIEW_KEYS), "{agent}");
            assert_eq!(agent["guest_running"], false);
            assert_eq!(agent["agent_status"], "unknown");
            assert_eq!(agent["name"], "llm-opt");
            assert_eq!(agent["terminal_id"], guest.grant.terminal_id.as_str());
            assert_eq!(agent["pane_id"], harness.pane_ids[0].as_str());
        }
    }

    #[test]
    fn a_restarted_agent_regains_access_but_a_renamed_or_other_kind_does_not() {
        let harness = start("restart");
        let guest = harness.admit();
        let prompt = |target: &str, text: &str| {
            harness.call(
                &guest,
                json!({"id": "p", "method": "agent.prompt", "params": {"target": target, "text": text}}),
            )
        };
        harness.agent_exits();
        assert_eq!(code(&prompt("llm-opt", "while away")), "guest_paused");

        harness.agent_restarts(crate::detect::Agent::Pi, "llm-opt", "session-restarted");
        let lines = prompt("llm-opt", "welcome back");
        assert_eq!(lines[0]["result"]["type"], "agent_prompted", "{lines:?}");
        let written = harness.pty_text(0, Duration::from_millis(600));
        assert!(
            written.contains("plotarmordev (via HerdrUp): welcome back"),
            "{written:?}"
        );
        let stored = crate::guest::store::load_store(&guest.dir).unwrap();
        let record = stored
            .guests
            .iter()
            .find(|record| record.guest_id == guest.guest_id)
            .unwrap();
        assert_eq!(record.grant.agent_session.value, "session-restarted");
        let list = harness.call(
            &guest,
            json!({"id": "l", "method": "agent.list", "params": {}}),
        );
        assert_eq!(list[0]["result"]["agents"][0]["guest_running"], true);
        let resumed = crate::guest::audit::read(&guest.dir, Some(&guest.guest_id), None, 50)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == GuestAuditEvent::Resumed)
            .count();
        assert_eq!(resumed, 1, "resumed is audited once per new session");

        harness.agent_restarts(crate::detect::Agent::Pi, "renamed", "session-renamed");
        assert_eq!(code(&prompt("llm-opt", "x")), "guest_paused");
        assert_eq!(code(&prompt(&guest.grant.terminal_id, "x")), "guest_paused");
        assert_eq!(code(&prompt("renamed", "x")), "guest_forbidden");

        harness.agent_restarts(crate::detect::Agent::Claude, "llm-opt", "session-claude");
        assert_eq!(code(&prompt("llm-opt", "x")), "guest_paused");
        assert!(harness.pty_text(0, Duration::from_millis(200)).is_empty());
    }

    #[test]
    fn grant_matching_follows_name_and_kind_not_the_session() {
        let grant = crate::guest::store::tests::grant();
        let agent: AgentInfo = serde_json::from_value(json!({
            "terminal_id": grant.terminal_id, "name": "llm-opt", "agent": "pi",
            "agent_status": "idle",
            "agent_session": {"source": "herdr", "agent": "pi", "kind": "id", "value": "new"},
            "workspace_id": "w", "tab_id": "t", "pane_id": "w1:p1",
            "focused": false, "revision": 1
        }))
        .unwrap();
        assert!(
            grant_matches(&grant, &agent),
            "a new session keeps the grant"
        );
        let mut other = agent.clone();
        other.name = Some("renamed".into());
        assert!(!grant_matches(&grant, &other));
        let mut other = agent.clone();
        other.agent = Some("claude".into());
        assert!(!grant_matches(&grant, &other));
        let mut other = agent.clone();
        other.agent = None;
        assert!(!grant_matches(&grant, &other));
        let mut other = agent.clone();
        other.terminal_id = "term_2".into();
        assert!(!grant_matches(&grant, &other));
        let mut other = agent.clone();
        other.machine_id = Some("studio".into());
        assert!(!grant_matches(&grant, &other));
        let mut other = agent.clone();
        other.archived = Some(crate::api::schema::AgentArchivedInfo {
            at: "2026-09-28T00:00:00Z".into(),
            by: "owner".into(),
            reason: None,
        });
        assert!(!grant_matches(&grant, &other));
        let mut kinded = grant.clone();
        kinded.agent_kind = Some("claude".into());
        assert!(
            !grant_matches(&kinded, &agent),
            "agent_kind wins over the session"
        );
    }

    #[test]
    fn ping_is_allowed() {
        let harness = start("ping");
        let guest = harness.admit();
        let lines = harness.call(&guest, json!({"id": "p", "method": "ping", "params": {}}));
        assert_eq!(lines[0]["result"]["type"], "pong", "{lines:?}");
    }

    /// The guest store in the harness's directory and its real `App`, behind
    /// the real relay link. Only the directory and API context differ from
    /// the daemon's `DaemonGuestHost`.
    struct StoreHost {
        dir: std::path::PathBuf,
        relay_url: String,
        api_tx: ApiRequestSender,
        event_hub: EventHub,
        running: Arc<AtomicBool>,
    }

    impl GuestHost for StoreHost {
        type Principal = GuestPrincipal;

        fn host_info(&self) -> std::io::Result<HostInfo> {
            let (host, node_secret) = crate::guest::store::load_host(&self.dir)?;
            Ok(HostInfo {
                host_id: host.host_id,
                relay_secret: host.relay_secret,
                node_secret,
                relay_url: self.relay_url.clone(),
            })
        }

        fn link_wanted(&self) -> bool {
            crate::guest::store::link_wanted_in(&self.dir, now_ms())
        }

        fn admit(&self, device_pub: [u8; 32], hello: &Value) -> LinkAdmission<GuestPrincipal> {
            admit_in(self.dir.clone(), device_pub, hello).into()
        }

        fn serve(&self, principal: GuestPrincipal, stream: UnixStream) {
            let _ = serve_guest_with(
                &self.api_tx,
                &self.event_hub,
                &self.running,
                principal,
                stream,
            );
        }

        fn set_link_status(&self, _: LinkState, _: Option<String>) {}

        fn subscribe_changes(&self) -> std_mpsc::Receiver<()> {
            crate::guest::subscribe_changes()
        }
    }

    #[test]
    fn guest_through_the_relay_link_prompts_the_agent_until_revoked() {
        let harness = start("guest-link-e2e");
        let dir = harness.dir.0.clone();
        let (identity, node_secret) = crate::guest::store::load_host(&dir).unwrap();
        let invite = harness.invite(0, "plotarmordev");
        // Another pending invite keeps the link wanted after the revoke.
        harness.invite(0, "second-guest");

        let stub = StubRelay::new();
        let host = Arc::new(StoreHost {
            dir: dir.clone(),
            relay_url: stub.url.clone(),
            api_tx: harness.api_tx.clone(),
            event_hub: harness.event_hub.clone(),
            running: Arc::clone(&harness.running),
        });
        let _link = GuestLink::start(host).unwrap();
        let mut relay = stub.accept_host(&identity.host_id, &identity.relay_secret);

        let device = Device {
            secret: [0x17; 32],
            host_pub: crate::guest::store::x25519_public(&node_secret),
            host_id: identity.host_id.clone(),
        };
        let accept = json!({"v": 1, "invite_id": invite.record.invite_id, "secret": invite.secret, "device": "iPhone"});
        let (reply, guest) = device.connect(&mut relay, 41, &accept);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["name"], "plotarmordev");
        assert_eq!(reply["agent"]["name"], "llm-opt");
        let guest_id = reply["guest_id"].as_str().unwrap().to_owned();

        // One request line in, the response line out, then CLOSE.
        let mut guest = guest.unwrap();
        let prompt = json!({"id": "p1", "method": "agent.prompt", "params": {"target": "llm-opt", "text": "ship it"}});
        guest.send(&mut relay, &format!("{prompt}\n"));
        let (line, _) = guest.recv_line(&mut relay);
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["type"], "agent_prompted", "{response}");
        assert_eq!(relay.closed(41), "");
        let written = harness.pty_text(0, Duration::from_secs(2));
        assert!(
            written.contains("plotarmordev (via HerdrUp): ship it"),
            "the labeled prompt reached the agent's PTY: {written:?}"
        );

        // A returning guest streams the pane until revoked; the link then
        // closes the session.
        let (reply, guest) = device.connect(&mut relay, 42, &json!({"v": 1}));
        assert_eq!(reply["guest_id"], guest_id.as_str(), "{reply}");
        let mut guest = guest.unwrap();
        let stream = json!({"id": "s", "method": "pane.stream", "params": {"pane_id": "llm-opt"}});
        guest.send(&mut relay, &format!("{stream}\n"));
        let (line, _) = guest.recv_line(&mut relay);
        let started: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(started["result"]["type"], "stream_started", "{started}");

        crate::guest::revoke_at(dir, RevokeTarget::Guest(&guest_id)).unwrap();
        let end = loop {
            let (line, _) = guest.recv_line(&mut relay);
            let value: Value = serde_json::from_str(&line).unwrap();
            if value.get("error").is_some() {
                break value;
            }
        };
        assert_eq!(end["error"]["code"], "guest_revoked", "{end}");
        assert_eq!(relay.closed(42), "");

        // The revoked device is refused at the handshake.
        let (reply, guest) = device.connect(&mut relay, 43, &json!({"v": 1}));
        assert_eq!(reply, json!({"ok": false, "error": "revoked"}));
        assert!(guest.is_none());
        assert_eq!(relay.closed(43), "revoked");
        assert!(harness.pty_text(0, Duration::from_millis(200)).is_empty());
    }

    /// An owner request straight to the app.
    fn owner(harness: &Harness, request: Value) -> Value {
        let request: Request = serde_json::from_value(request).unwrap();
        serde_json::from_str(&dispatch_to_app_with_timeout(
            request,
            &harness.api_tx,
            None,
        ))
        .unwrap()
    }

    /// The pane an agent of the harness runs in, by name.
    fn pane_of(harness: &Harness, agent: &str) -> Option<String> {
        let index = ["llm-opt", "other-agent"]
            .iter()
            .position(|name| *name == agent)?;
        Some(harness.pane_ids[index].clone())
    }

    fn sent_id(sent: &Value) -> String {
        sent["result"]["message"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("sent: {sent}"))
            .to_string()
    }

    /// A Gram to the owner labeled `from`, sent from `pane` as `herdr gram send`
    /// in that pane would (no pane: sent from outside any pane); returns its id.
    fn send_gram_as(harness: &Harness, pane: Option<String>, from: &str, text: &str) -> String {
        sent_id(&owner(
            harness,
            json!({"id": "s", "method": "gram.send", "params": {"text": text, "from": from, "caller_pane_id": pane}}),
        ))
    }

    /// An agent's Gram to the owner from its own pane; returns its id.
    fn send_gram(harness: &Harness, from: &str, text: &str) -> String {
        send_gram_as(harness, pane_of(harness, from), from, text)
    }

    /// An agent's Gram from its pane carrying a file holding `hello`.
    fn send_file(harness: &Harness, from: &str, name: &str) -> String {
        let upload = format!("up-{name}");
        let staged = owner(
            harness,
            json!({"id": "u", "method": "gram.upload_chunk", "params": {"upload_id": upload, "offset": 0, "data_base64": "aGVsbG8="}}),
        );
        assert_eq!(staged["result"]["type"], "ok", "{staged}");
        sent_id(&owner(
            harness,
            json!({"id": "s", "method": "gram.send", "params": {"text": "", "from": from, "caller_pane_id": pane_of(harness, from), "file": {"upload_id": upload, "name": name, "mime": "text/plain"}}}),
        ))
    }

    fn guest_list(harness: &Harness, guest: &GuestPrincipal, params: Value) -> Value {
        let lines = harness.call(
            guest,
            json!({"id": "l", "method": "gram.list", "params": params}),
        );
        lines[0]["result"].clone()
    }

    fn texts(result: &Value) -> Vec<String> {
        result["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("messages: {result}"))
            .iter()
            .map(|message| message["text"].as_str().unwrap().to_string())
            .collect()
    }

    fn read_marks(result: &Value) -> Vec<(String, bool)> {
        result["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| {
                (
                    message["id"].as_str().unwrap().to_string(),
                    message["read"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn grams_named_like_the_agent_but_sent_by_another_are_hidden() {
        let _config = ConfigHome::new("gram-impostor");
        let harness = start("gram-impostor");
        let guest = harness.admit_with(0, true);
        let other_pane = pane_of(&harness, "other-agent");
        let elsewhere = send_gram_as(&harness, other_pane, "llm-opt", "same name, other pane");
        let no_pane = send_gram_as(&harness, None, "llm-opt", "same name, no pane");
        // What a Gram relayed from another machine's `llm-opt` stores.
        crate::persist::gram::append(crate::persist::gram::GramItem {
            expired_file: None,
            id: "relay-remote".into(),
            direction: crate::persist::gram::GramDirection::AgentToOwner,
            from: "llm-opt".into(),
            to: None,
            text: "same name, relayed".into(),
            grabbed_by: None,
            grabbed_unix_ms: None,
            created_unix_ms: now_ms(),
            read_by_owner: false,
            file: None,
            origin_id: String::new(),
            sender: None,
        })
        .unwrap();
        send_gram(&harness, "llm-opt", "the granted agent");

        let result = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&result), ["the granted agent"]);
        for id in [elsewhere.as_str(), no_pane.as_str(), "relay-remote"] {
            let lines = harness.call(
                &guest,
                json!({"id": "m", "method": "gram.mark_read", "params": {"ids": [id]}}),
            );
            assert_eq!(code(&lines), "guest_forbidden", "{id}");
        }
    }

    #[test]
    fn a_shared_guest_sees_the_agents_grams_since_its_grant_and_its_own_posts() {
        let _config = ConfigHome::new("gram-visible");
        let harness = start("gram-visible");
        send_gram(&harness, "llm-opt", "before the grant");
        std::thread::sleep(Duration::from_millis(5));
        let guest = harness.admit_with(0, true);
        send_gram(&harness, "llm-opt", "after the grant");
        send_gram(&harness, "other-agent", "another agent");
        let posted = owner(
            &harness,
            json!({"id": "o", "method": "gram.post", "params": {"text": "from the owner", "to": "llm-opt"}}),
        );
        assert!(posted.get("result").is_some(), "{posted}");
        let own = harness.call(
            &guest,
            json!({"id": "g", "method": "gram.post", "params": {"text": "from the guest"}}),
        );
        let own_id = own[0]["result"]["message"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let result = guest_list(&harness, &guest, json!({}));
        assert_eq!(result["type"], "guest_gram_list");
        assert_eq!(texts(&result), ["from the guest", "after the grant"]);
        assert_eq!(result["has_more"], false);
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages[0]["direction"], "owner_to_agent");
        assert_eq!(messages[0]["from"], "plotarmordev (via HerdrUp)");
        assert_eq!(messages[0]["read"], true);
        assert_eq!(messages[1]["direction"], "agent_to_owner");
        assert_eq!(messages[1]["from"], "llm-opt");
        assert_eq!(messages[1]["read"], false);
        assert_eq!(
            keys(&messages[1]),
            set(&["id", "direction", "from", "text", "created_unix_ms", "read"])
        );

        let first = guest_list(&harness, &guest, json!({"limit": 1}));
        assert_eq!(texts(&first), ["from the guest"]);
        assert_eq!(first["has_more"], true);
        let next = guest_list(&harness, &guest, json!({"limit": 1, "before_id": own_id}));
        assert_eq!(texts(&next), ["after the grant"]);
        assert_eq!(next["has_more"], false);
        let stale = harness.call(
            &guest,
            json!({"id": "l", "method": "gram.list", "params": {"before_id": "gram-missing"}}),
        );
        assert_eq!(code(&stale), "invalid_params");
    }

    #[test]
    fn gram_methods_need_share_gram() {
        let _config = ConfigHome::new("gram-share");
        let harness = start("gram-share");
        let guest = harness.admit();
        let id = send_file(&harness, "llm-opt", "notes.txt");
        let requests = [
            json!({"id": "d", "method": "gram.list", "params": {}}),
            json!({"id": "d", "method": "gram.mark_read", "params": {"ids": [id]}}),
            json!({"id": "d", "method": "gram.get_file", "params": {"id": id}}),
            json!({"id": "d", "method": "gram.get_file_chunk", "params": {"id": id, "offset": 0}}),
        ];
        for request in &requests {
            let lines = harness.call(&guest, request.clone());
            assert_eq!(code(&lines), "guest_forbidden", "{request} -> {lines:?}");
        }
        // The owner's change applies to the session already admitted.
        crate::guest::update_at(&guest.dir, &guest.guest_id, true)
            .unwrap()
            .unwrap();
        for request in &requests {
            let lines = harness.call(&guest, request.clone());
            assert_eq!(code(&lines), "<success>", "{request} -> {lines:?}");
        }
        crate::guest::update_at(&guest.dir, &guest.guest_id, false)
            .unwrap()
            .unwrap();
        for request in &requests {
            let lines = harness.call(&guest, request.clone());
            assert_eq!(code(&lines), "guest_forbidden", "{request} -> {lines:?}");
        }
    }

    #[test]
    fn a_guest_fetches_only_visible_files_and_each_open_is_audited() {
        let _config = ConfigHome::new("gram-files");
        let harness = start("gram-files");
        let before = send_file(&harness, "llm-opt", "old.txt");
        std::thread::sleep(Duration::from_millis(5));
        let guest = harness.admit_with(0, true);
        let visible = send_file(&harness, "llm-opt", "notes.txt");
        let second = send_file(&harness, "llm-opt", "second.txt");
        let other = send_file(&harness, "other-agent", "other.txt");

        // A download resumed mid-file is still an open.
        let resumed = harness.call(
            &guest,
            json!({"id": "c", "method": "gram.get_file_chunk", "params": {"id": second, "offset": 3}}),
        );
        assert_eq!(resumed[0]["result"]["data_base64"], "bG8=", "{resumed:?}");

        let chunk = harness.call(
            &guest,
            json!({"id": "c", "method": "gram.get_file_chunk", "params": {"id": visible, "offset": 0}}),
        );
        let result = &chunk[0]["result"];
        assert_eq!(result["type"], "gram_file_chunk", "{chunk:?}");
        assert_eq!(result["name"], "notes.txt");
        assert_eq!(result["data_base64"], "aGVsbG8=");
        let rest = harness.call(
            &guest,
            json!({"id": "c", "method": "gram.get_file_chunk", "params": {"id": visible, "offset": 3}}),
        );
        assert_eq!(rest[0]["result"]["data_base64"], "bG8=", "{rest:?}");
        let whole = harness.call(
            &guest,
            json!({"id": "f", "method": "gram.get_file", "params": {"id": visible}}),
        );
        assert_eq!(whole[0]["result"]["type"], "gram_file_content", "{whole:?}");

        for id in [before.as_str(), other.as_str(), "gram-missing"] {
            for request in [
                json!({"id": "f", "method": "gram.get_file", "params": {"id": id}}),
                json!({"id": "c", "method": "gram.get_file_chunk", "params": {"id": id, "offset": 0}}),
            ] {
                let lines = harness.call(&guest, request.clone());
                assert_eq!(code(&lines), "guest_forbidden", "{request} -> {lines:?}");
            }
        }
        let mut opened: Vec<String> =
            crate::guest::audit::read(&guest.dir, Some(&guest.guest_id), None, 100)
                .unwrap()
                .into_iter()
                .filter(|entry| entry.event == GuestAuditEvent::GramFile)
                .map(|entry| entry.file.unwrap().name)
                .collect();
        opened.sort();
        assert_eq!(opened, ["notes.txt", "second.txt"], "one per file opened");
    }

    #[test]
    fn guest_read_marks_need_visible_ids_and_never_touch_the_owners() {
        let _config = ConfigHome::new("gram-read");
        let harness = start("gram-read");
        let guest = harness.admit_with(0, true);
        let first = send_gram(&harness, "llm-opt", "first");
        let second = send_gram(&harness, "llm-opt", "second");
        let other = send_gram(&harness, "other-agent", "other");

        let mixed = harness.call(
            &guest,
            json!({"id": "m", "method": "gram.mark_read", "params": {"ids": [first, other]}}),
        );
        assert_eq!(code(&mixed), "guest_forbidden");
        let unread = guest_list(&harness, &guest, json!({}));
        assert_eq!(
            read_marks(&unread),
            [(second.clone(), false), (first.clone(), false)]
        );

        let marked = harness.call(
            &guest,
            json!({"id": "m", "method": "gram.mark_read", "params": {"ids": [first]}}),
        );
        assert_eq!(marked[0], json!({"id": "m", "result": {"type": "ok"}}));
        let owner_view = owner(
            &harness,
            json!({"id": "l", "method": "gram.list", "params": {}}),
        );
        let owner_read = owner_view["result"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["id"] == first.as_str())
            .unwrap()["read_by_owner"]
            .clone();
        assert_eq!(owner_read, false);

        let owner_marked = owner(
            &harness,
            json!({"id": "r", "method": "gram.mark_read", "params": {"id": second}}),
        );
        assert_eq!(owner_marked["result"]["type"], "ok", "{owner_marked}");
        let after = guest_list(&harness, &guest, json!({}));
        assert_eq!(read_marks(&after), [(second, false), (first, true)]);
    }

    fn register(token: &str, prefs: Value) -> Value {
        let mut params =
            json!({"device_token": token, "platform": "ios", "relay_capability": "hpr1.guest_cap"});
        for (key, value) in prefs.as_object().unwrap() {
            params[key] = value.clone();
        }
        json!({"id": "n", "method": "notifications.register_device", "params": params})
    }

    fn guest_devices(guest: &GuestPrincipal) -> Vec<(String, String, bool)> {
        crate::guest::push::devices(&guest.dir)
            .unwrap()
            .into_iter()
            .map(|registered| {
                (
                    registered.guest_id,
                    registered.device.device_token,
                    registered.device.notify_needs_input,
                )
            })
            .collect()
    }

    #[test]
    fn a_guest_device_registers_under_the_guest_apart_from_the_owners() {
        let _config = ConfigHome::new("guest-device");
        let harness = start("guest-device");
        let guest = harness.admit();
        let token = "ab".repeat(32);
        let registered = harness.call(
            &guest,
            register(&token, json!({"notify_needs_input": true})),
        );
        assert_eq!(registered[0], json!({"id": "n", "result": {"type": "ok"}}));
        assert_eq!(
            guest_devices(&guest),
            [(guest.guest_id.clone(), token.clone(), true)]
        );
        harness.call(
            &guest,
            register(&token, json!({"notify_needs_input": false})),
        );
        assert_eq!(
            guest_devices(&guest),
            [(guest.guest_id.clone(), token.clone(), false)]
        );
        assert!(crate::persist::devices::load().is_empty());

        let invalid = harness.call(&guest, register("zz", json!({})));
        assert_eq!(code(&invalid), "invalid_params");
        let removed = harness.call(
            &guest,
            json!({"id": "u", "method": "notifications.unregister_device", "params": {"device_token": token}}),
        );
        assert_eq!(removed[0], json!({"id": "u", "result": {"type": "ok"}}));
        assert!(guest_devices(&guest).is_empty());
    }

    /// Relay mode without an APNs key, as on a host with only the HerdrUp relay.
    fn relay_only() -> crate::config::PushConfig {
        crate::config::PushConfig {
            mode: crate::config::PushMode::Auto,
            relay_url: "https://push.example".into(),
            ..Default::default()
        }
    }

    /// Both agents need input and send a Gram from their panes, and the other
    /// agent sends one more labeled `llm-opt`; every alert the app dispatches.
    fn both_agents_alert(harness: &Harness) -> Vec<crate::push::PushNotification> {
        let (done_tx, done_rx) = std_mpsc::channel();
        let panes = harness.pane_ids.clone();
        harness
            .control
            .send(Box::new(move |app: &mut App| {
                let capture = crate::push::test_sink::Capture::install();
                app.state.push_config = relay_only();
                for index in 0..2 {
                    let pane_id = app.state.workspaces[index].tabs[0].root_pane;
                    app.handle_internal_event(crate::events::AppEvent::StateChanged {
                        pane_id,
                        runtime_epoch: None,
                        agent: Some(crate::detect::Agent::Pi),
                        state: crate::detect::AgentState::Blocked,
                        visible_blocker: true,
                        visible_working: false,
                        process_exited: false,
                        observed_at: Instant::now(),
                    });
                }
                for (pane, from) in [
                    (&panes[0], "llm-opt"),
                    (&panes[1], "other-agent"),
                    (&panes[1], "llm-opt"),
                ] {
                    let request: Request = serde_json::from_value(
                        json!({"id": "s", "method": "gram.send", "params": {"text": "done", "from": from, "caller_pane_id": pane}}),
                    )
                    .unwrap();
                    app.handle_api_request(request);
                }
                let _ = done_tx.send(capture.take().alerts);
            }))
            .unwrap();
        done_rx.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    /// What the guest's devices would receive: (route, payload) per send.
    fn guest_sends(
        guest: &GuestPrincipal,
        alerts: &[crate::push::PushNotification],
    ) -> Vec<(&'static str, Value)> {
        let guests = crate::guest::store::load_store(&guest.dir).unwrap().guests;
        let devices = crate::guest::push::devices(&guest.dir).unwrap();
        let plan = crate::guest::push::plan(&relay_only(), alerts, &guests, &devices, "host-1");
        let payload = |index: usize| serde_json::from_str::<Value>(&plan.payloads[index]).unwrap();
        plan.direct
            .iter()
            .map(|(index, _)| ("direct", payload(*index)))
            .chain(
                plan.relayed
                    .iter()
                    .map(|(index, _)| ("relay", payload(*index))),
            )
            .collect()
    }

    #[test]
    fn one_phone_receives_pushes_for_each_independent_grant() {
        let _config = ConfigHome::new("multi-guest-push");
        let harness = start("multi-guest-push");
        let first = harness.admit_with(0, true);
        let second = harness.admit_with(1, false);
        let token = "ef".repeat(32);
        for guest in [&first, &second] {
            assert_eq!(
                code(&harness.call(
                    guest,
                    register(
                        &token,
                        json!({"notify_needs_input": true, "notify_gram": true})
                    )
                )),
                "<success>"
            );
        }
        let alerts = both_agents_alert(&harness);
        let routes = |guest: &GuestPrincipal| {
            let mut routes: Vec<_> = guest_sends(guest, &alerts)
                .into_iter()
                .map(|(_, payload)| {
                    (
                        payload["herdr_guest"]["guest_id"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                        payload["herdr_guest"]["kind"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            routes.sort();
            routes
        };
        let mut expected = vec![
            (first.guest_id.clone(), "status".to_owned()),
            (first.guest_id.clone(), "gram".to_owned()),
            (second.guest_id.clone(), "status".to_owned()),
        ];
        expected.sort();
        assert_eq!(
            routes(&first),
            expected,
            "registering the second grant must preserve the first"
        );
        assert_eq!(
            code(&harness.call(
                &first,
                json!({
                    "id": "u", "method": "notifications.unregister_device",
                    "params": {"device_token": token}
                })
            )),
            "<success>"
        );
        assert_eq!(
            routes(&second),
            vec![(second.guest_id.clone(), "status".to_owned())]
        );
    }

    #[test]
    fn the_granted_agents_status_and_grams_reach_the_guests_phone_through_the_relay() {
        let _config = ConfigHome::new("guest-push");
        let harness = start("guest-push");
        let guest = harness.admit_with(0, true);
        let all = json!({"notify_needs_input": true, "notify_finishes": true, "notify_dies": true, "notify_gram": true});
        let token = "cd".repeat(32);
        harness.call(&guest, register(&token, all));

        let alerts = both_agents_alert(&harness);
        let sends = guest_sends(&guest, &alerts);
        assert_eq!(sends.len(), 2, "{sends:?} from {alerts:?}");
        let (route, status) = &sends[0];
        assert_eq!(*route, "relay");
        assert_eq!(
            status["herdr_guest"],
            json!({"host_id": "host-1", "guest_id": guest.guest_id, "kind": "status"})
        );
        assert!(status["aps"]["alert"]["title"]
            .as_str()
            .unwrap()
            .starts_with("llm-opt"));
        assert_eq!(status["aps"]["alert"]["body"], "Jerry's Mac Studio");
        assert_eq!(keys(status), set(&["aps", "herdr_guest"]));
        // An owner alert that carries a question (herdr#309) reaches the guest
        // as the same plain status alert: no question, options or category.
        let with_ask: Vec<_> = alerts
            .iter()
            .cloned()
            .map(|mut alert| {
                alert.ask = Some(crate::push::PushAsk {
                    agent: "llm-opt".into(),
                    pane_id: alert.pane_id.clone(),
                    machine_id: "machine-1".into(),
                    ask_id: "call_1".into(),
                    question: "Ship it?".into(),
                    options: vec!["Yes".into(), "No".into()],
                });
                alert
            })
            .collect();
        assert_eq!(guest_sends(&guest, &with_ask), sends);
        let (route, gram) = &sends[1];
        assert_eq!(*route, "relay");
        assert_eq!(gram["herdr_guest"]["kind"], "gram");
        assert_eq!(gram["aps"]["alert"]["title"], "llm-opt");
        let gram_id = gram["herdr_guest"]["gram_id"].as_str().unwrap();
        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(listed["messages"][0]["id"], gram_id);

        // Without Gram sharing the status still arrives, the Gram does not.
        crate::guest::update_at(&guest.dir, &guest.guest_id, false)
            .unwrap()
            .unwrap();
        let sends = guest_sends(&guest, &alerts);
        assert_eq!(sends.len(), 1, "{sends:?}");
        assert_eq!(sends[0].1["herdr_guest"]["kind"], "status");

        // The device's own preferences decide.
        crate::guest::update_at(&guest.dir, &guest.guest_id, true)
            .unwrap()
            .unwrap();
        harness.call(&guest, register(&token, json!({"notify_finishes": true})));
        assert!(guest_sends(&guest, &alerts).is_empty());
    }

    /// What a coordinator holds for one relaying remote.
    #[derive(Default)]
    struct CoordinatorState {
        uploads: HashMap<String, Vec<u8>>,
        /// Each stored message with its file bytes.
        messages: Vec<(Value, Vec<u8>)>,
        posts: Vec<Value>,
        /// Relayed `gram.list` calls answered.
        lists: usize,
    }

    /// A coordinator behind the production gateway (`serve_one`) that answers
    /// the relay calls in memory, naming the remote's two panes `llm-opt` and
    /// `other-agent` as a real coordinator does, qualified with the relay
    /// alias (`mac-studio/llm-opt`): the name its federation roster gives them
    /// and so the name its Gram stores. It serves files 3 bytes per chunk.
    struct Coordinator {
        state: Arc<Mutex<CoordinatorState>>,
        _socket: RelaySocket,
    }

    /// This process as a Gram-relay remote of the socket at `path`, until
    /// dropped: then the variable, the relay policy and the socket file are
    /// back as they were. The path is short, since macOS limits a socket path
    /// to 104 bytes and its temp directory alone is about 50.
    struct RelaySocket {
        path: std::path::PathBuf,
        previous: Option<std::ffi::OsString>,
    }

    impl RelaySocket {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let path = std::path::PathBuf::from(format!(
                "/tmp/hg-{:x}-{:x}.sock",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_file(&path);
            Self {
                path,
                previous: std::env::var_os(crate::api::gram_relay::SOCKET_ENV),
            }
        }

        /// Point this daemon's Gram at the socket.
        fn enable(&self) {
            std::env::set_var(crate::api::gram_relay::SOCKET_ENV, &self.path);
            crate::api::gram_relay::apply_config(&crate::config::Config::default().gram_relay);
            assert!(crate::api::gram_relay::policy().remote_socket().is_some());
        }
    }

    impl Drop for RelaySocket {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(crate::api::gram_relay::SOCKET_ENV, value),
                None => std::env::remove_var(crate::api::gram_relay::SOCKET_ENV),
            }
            crate::api::gram_relay::apply_config(&crate::config::Config::default().gram_relay);
            let _ = std::fs::remove_file(&self.path);
        }
    }

    impl Coordinator {
        /// Makes this daemon (the harness) a Gram-relay remote of it.
        fn start(harness: &Harness) -> Self {
            use base64::Engine as _;
            let socket = RelaySocket::new();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ApiRequestMessage>();
            crate::api::reverse::serve_test_gateway(&socket.path, "mac-studio", tx);
            socket.enable();

            let state: Arc<Mutex<CoordinatorState>> = Arc::default();
            let names: HashMap<String, &str> = [
                (harness.pane_ids[0].clone(), "mac-studio/llm-opt"),
                (harness.pane_ids[1].clone(), "mac-studio/other-agent"),
            ]
            .into_iter()
            .collect();
            let handled = Arc::clone(&state);
            std::thread::spawn(move || {
                let b64 = base64::engine::general_purpose::STANDARD;
                while let Some(message) = rx.blocking_recv() {
                    let id = message.request.id.clone();
                    let Method::GramRelay(relay) = message.request.method else {
                        panic!("the gateway sends relay envelopes");
                    };
                    assert_eq!(relay.peer_alias, "mac-studio");
                    let mut state = handled.lock().unwrap();
                    let caller = |pane: &Option<String>| {
                        pane.as_deref().and_then(|pane| names.get(pane).copied())
                    };
                    let file_record =
                        |file: Option<crate::api::schema::GramFileUpload>,
                         state: &mut CoordinatorState| {
                            let Some(file) = file else {
                                return (Value::Null, Vec::new());
                            };
                            let bytes = state.uploads.remove(&file.upload_id).unwrap_or_default();
                            let sha: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes)
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect();
                            (
                                json!({"name": file.name, "size": bytes.len(), "mime": file.mime, "sha256": sha}),
                                bytes,
                            )
                        };
                    let result = match relay.call {
                        GramRelayCall::UploadChunk(chunk) => {
                            let bytes = b64.decode(chunk.data_base64).unwrap();
                            state
                                .uploads
                                .entry(chunk.upload_id)
                                .or_default()
                                .extend(bytes);
                            json!({"type": "ok"})
                        }
                        GramRelayCall::Send(send) => {
                            let Some(from) = caller(&send.caller_pane_id) else {
                                let _ = message.respond_to.send(
                                    json!({"id": id, "error": {"code": "unknown_caller", "message": "?"}}).to_string(),
                                );
                                continue;
                            };
                            let (file, bytes) = file_record(send.file, &mut state);
                            let mut stored = json!({
                                "id": format!("relay-{}", state.messages.len()),
                                "direction": "agent_to_owner", "from": from, "text": send.text,
                                "created_unix_ms": now_ms(), "read_by_owner": false,
                            });
                            if !file.is_null() {
                                stored["file"] = file;
                            }
                            state.messages.push((stored.clone(), bytes));
                            json!({"type": "gram_sent", "message": stored, "store_id": "coordinator"})
                        }
                        GramRelayCall::Post(post) => {
                            state.posts.push(serde_json::to_value(&post).unwrap());
                            let (file, bytes) = file_record(post.file, &mut state);
                            let mut stored = json!({
                                "id": format!("gram-{}", state.messages.len()),
                                "direction": "owner_to_agent",
                                "from": crate::guest::post_from(&post.guest),
                                "to": format!("mac-studio/{}", post.to), "text": post.text,
                                "created_unix_ms": now_ms(), "read_by_owner": true,
                            });
                            if !file.is_null() {
                                stored["file"] = file;
                            }
                            state.messages.push((stored.clone(), bytes));
                            json!({"type": "gram_sent", "message": stored, "store_id": "coordinator"})
                        }
                        GramRelayCall::GetFileChunk(fetch) => {
                            let identity = caller(&fetch.caller_pane_id);
                            let found = state.messages.iter().find(|(stored, _)| {
                                stored["id"] == fetch.id.as_str()
                                    && identity.is_some_and(|identity| {
                                        stored["from"] == identity || stored["to"] == identity
                                    })
                            });
                            match found {
                                Some((stored, bytes)) => {
                                    let start = (fetch.offset as usize).min(bytes.len());
                                    let end = (start + 3).min(bytes.len());
                                    let file = &stored["file"];
                                    json!({"type": "gram_file_chunk", "name": file["name"], "mime": file["mime"],
                                        "size": file["size"], "sha256": file["sha256"], "offset": fetch.offset,
                                        "data_base64": b64.encode(&bytes[start..end])})
                                }
                                None => {
                                    let _ = message.respond_to.send(
                                        json!({"id": id, "error": {"code": "forbidden", "message": "?"}}).to_string(),
                                    );
                                    continue;
                                }
                            }
                        }
                        GramRelayCall::List(list) => {
                            let Some(identity) = caller(&list.caller_pane_id) else {
                                let _ = message.respond_to.send(
                                    json!({"id": id, "error": {"code": "unknown_caller", "message": "?"}}).to_string(),
                                );
                                continue;
                            };
                            state.lists += 1;
                            let view: Vec<&Value> = state
                                .messages
                                .iter()
                                .rev()
                                .map(|(stored, _)| stored)
                                .filter(|stored| {
                                    stored["from"] == identity || stored["to"] == identity
                                })
                                .collect();
                            let start = list.before_id.as_deref().map_or(0, |before| {
                                view.iter()
                                    .position(|stored| stored["id"] == before)
                                    .map_or(view.len(), |at| at + 1)
                            });
                            let limit = list.limit.unwrap_or(usize::MAX);
                            let page: Vec<&Value> =
                                view.iter().skip(start).take(limit).copied().collect();
                            let has_more = start + page.len() < view.len();
                            json!({"type": "gram_list", "messages": page, "store_id": "coordinator",
                                "digest": "", "has_more": has_more, "unread_count": 0})
                        }
                        GramRelayCall::Delete(delete) => {
                            let identity = caller(&delete.caller_pane_id);
                            let before = state.messages.len();
                            state.messages.retain(|(stored, _)| {
                                stored["id"] != delete.id.as_str()
                                    || !identity.is_some_and(|identity| {
                                        stored["from"] == identity || stored["to"] == identity
                                    })
                            });
                            if state.messages.len() == before {
                                let _ = message.respond_to.send(
                                    json!({"id": id, "error": {"code": "not_found", "message": "?"}}).to_string(),
                                );
                                continue;
                            }
                            json!({"type": "ok"})
                        }
                    };
                    let _ = message
                        .respond_to
                        .send(json!({"id": id, "result": result}).to_string());
                }
            });
            Self {
                state,
                _socket: socket,
            }
        }
    }

    impl Coordinator {
        /// The owner deletes a Gram in the coordinator's own store.
        /// A Gram labeled `llm-opt` this machine never relayed, as another
        /// machine's agent of that name would send.
        fn foreign_gram(&self, text: &str) {
            let mut state = self.state.lock().unwrap();
            let stored = json!({
                "id": "relay-foreign", "direction": "agent_to_owner", "from": "mac-studio/llm-opt",
                "text": text, "created_unix_ms": now_ms(), "read_by_owner": false,
            });
            state.messages.push((stored, Vec::new()));
        }

        fn owner_deletes(&self, id: &str) {
            let mut state = self.state.lock().unwrap();
            let before = state.messages.len();
            state.messages.retain(|(stored, _)| stored["id"] != id);
            assert_eq!(state.messages.len() + 1, before, "{id} was stored");
        }

        /// The coordinator's attachment budget expired `id`'s file: its bytes are gone
        /// and the message carries the metadata as `expired_file` instead of `file`.
        fn expires_file(&self, id: &str) {
            let mut state = self.state.lock().unwrap();
            let (stored, bytes) = state
                .messages
                .iter_mut()
                .find(|(stored, _)| stored["id"] == id)
                .unwrap_or_else(|| panic!("{id} was stored"));
            let file = stored["file"].take();
            assert!(file.is_object(), "{id} had a file");
            stored["expired_file"] = file;
            stored.as_object_mut().unwrap().remove("file");
            bytes.clear();
        }
    }

    /// One request on this machine's own API, as a local client (`herdr` in
    /// an agent's pane) sends it. Runs on the calling thread, so a push
    /// capture installed there sees what it dispatches.
    fn local_call(harness: &Harness, request: Value) -> Value {
        let (mut client, server) = UnixStream::pair().unwrap();
        writeln!(client, "{request}").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let stream = ApiStream::Local(crate::ipc::LocalStream::from(
            interprocess::os::unix::uds_local_socket::Stream::from(server),
        ));
        super::super::handle_principal_connection(
            stream,
            &harness.api_tx,
            &harness.event_hub,
            &harness.running,
            None,
            None,
            ConnectionPrincipal::Owner,
            &HashMap::new(),
            None,
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// A harness whose guest store is this daemon's real one.
    fn relay_remote(tag: &str) -> (ConfigHome, Harness, Coordinator) {
        let config = ConfigHome::new(tag);
        let mut harness = start(tag);
        harness.dir = TempDir(crate::guest::store::guest_dir());
        let coordinator = Coordinator::start(&harness);
        (config, harness, coordinator)
    }

    #[test]
    fn on_a_gram_relay_remote_the_agents_grams_reach_its_sharing_guest() {
        let (_config, harness, _coordinator) = relay_remote("relay-send");
        let guest = harness.admit_with(0, true);
        let token = "ef".repeat(32);
        harness.call(&guest, register(&token, json!({"notify_gram": true})));
        let pane = &harness.pane_ids[0];

        let staged = local_call(
            &harness,
            json!({"id": "u", "method": "gram.upload_chunk", "params": {"upload_id": "up-1", "offset": 0, "data_base64": "aGVsbG8gd29ybGQ="}}),
        );
        assert_eq!(staged["result"]["type"], "ok", "{staged}");
        let capture = crate::push::test_sink::Capture::install();
        let sent = local_call(
            &harness,
            json!({"id": "s", "method": "gram.send", "params": {"text": "report", "caller_pane_id": pane, "file": {"upload_id": "up-1", "name": "report.txt", "mime": "text/plain"}}}),
        );
        let id = sent["result"]["message"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(id.starts_with("relay-"), "{sent}");
        let sent_alerts = capture.take();
        drop(capture);
        // The coordinator keeps the owner's copy and notifies the owner.
        assert!(crate::persist::gram::load().is_empty());
        assert!(sent_alerts.alerts.is_empty());

        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["report"], "{listed}");
        assert_eq!(listed["messages"][0]["id"], id.as_str());
        assert_eq!(listed["messages"][0]["from"], "llm-opt");
        let chunk = harness.call(
            &guest,
            json!({"id": "c", "method": "gram.get_file_chunk", "params": {"id": id, "offset": 0}}),
        );
        assert_eq!(
            chunk[0]["result"]["data_base64"], "aGVsbG8gd29ybGQ=",
            "{chunk:?}"
        );
        assert_eq!(chunk[0]["result"]["size"], 11);

        let sends = guest_sends(&guest, &sent_alerts.guest_alerts);
        assert_eq!(sends.len(), 1, "{sends:?}");
        assert_eq!(sends[0].0, "relay");
        assert_eq!(sends[0].1["herdr_guest"]["kind"], "gram");
        assert_eq!(sends[0].1["herdr_guest"]["gram_id"], id.as_str());

        // Another pane's Gram is not the shared agent's.
        let capture = crate::push::test_sink::Capture::install();
        local_call(
            &harness,
            json!({"id": "s", "method": "gram.send", "params": {"text": "elsewhere", "caller_pane_id": harness.pane_ids[1]}}),
        );
        assert!(capture.take().guest_alerts.is_empty());
        assert_eq!(texts(&guest_list(&harness, &guest, json!({}))), ["report"]);

        // Revoking the only sharing guest drops its copies.
        crate::guest::revoke_at(guest.dir.clone(), RevokeTarget::Guest(&guest.guest_id)).unwrap();
        assert!(crate::guest::mirror::load(&guest.dir).unwrap().is_empty());
    }

    /// The file of Gram `id` as the agent in `pane` downloads it through this
    /// machine's API, chunk by chunk.
    fn agent_download(harness: &Harness, pane: &str, id: &str) -> Vec<u8> {
        use base64::Engine as _;
        let mut bytes = Vec::new();
        loop {
            let chunk = local_call(
                harness,
                json!({"id": "c", "method": "gram.get_file_chunk", "params": {"id": id, "offset": bytes.len(), "caller_pane_id": pane}}),
            );
            let data = chunk["result"]["data_base64"]
                .as_str()
                .unwrap_or_else(|| panic!("chunk: {chunk}"));
            let data = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            if data.is_empty() {
                return bytes;
            }
            bytes.extend(data);
        }
    }

    #[test]
    fn on_a_gram_relay_remote_a_guests_post_reaches_the_agent_where_it_reads() {
        let (_config, harness, coordinator) = relay_remote("relay-post");
        let guest = harness.admit_with(0, true);
        let staged = harness.call(
            &guest,
            json!({"id": "u", "method": "gram.upload_chunk", "params": {"upload_id": "g-up", "offset": 0, "data_base64": "aGVsbG8="}}),
        );
        assert_eq!(staged[0]["result"]["type"], "ok", "{staged:?}");
        let posted = harness.call(
            &guest,
            json!({"id": "p", "method": "gram.post", "params": {"text": "see file", "file": {"upload_id": "g-up", "name": "a.txt", "mime": "text/plain"}}}),
        );
        let id = posted[0]["result"]["message"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("posted: {posted:?}"))
            .to_string();
        // The guest sees the agent under the name it knows.
        assert_eq!(posted[0]["result"]["message"]["to"], "llm-opt");
        assert!(
            crate::persist::gram::load().is_empty(),
            "nothing stays local"
        );
        let relayed = coordinator.state.lock().unwrap().posts.clone();
        assert_eq!(relayed.len(), 1);
        assert_eq!(relayed[0]["to"], "llm-opt");
        assert_eq!(relayed[0]["guest"], "plotarmordev");

        // The agent finds the attachment where it reads its Gram.
        assert_eq!(
            agent_download(&harness, &harness.pane_ids[0], &id),
            b"hello"
        );

        // The guest still sees its own post and file.
        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["see file"], "{listed}");
        assert_eq!(listed["messages"][0]["from"], "plotarmordev (via HerdrUp)");
        let whole = harness.call(
            &guest,
            json!({"id": "f", "method": "gram.get_file", "params": {"id": id}}),
        );
        assert_eq!(whole[0]["result"]["data_base64"], "aGVsbG8=", "{whole:?}");
    }

    fn copy_ids(guest: &GuestPrincipal) -> Vec<String> {
        crate::guest::mirror::load(&guest.dir)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect()
    }

    fn agent_sends(harness: &Harness, pane: usize, text: &str) -> String {
        let sent = local_call(
            harness,
            json!({"id": "s", "method": "gram.send", "params": {"text": text, "caller_pane_id": harness.pane_ids[pane]}}),
        );
        sent["result"]["message"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("sent: {sent}"))
            .to_string()
    }

    #[test]
    fn an_agents_relayed_delete_drops_the_guest_copy() {
        let (_config, harness, _coordinator) = relay_remote("relay-delete");
        let guest = harness.admit_with(0, true);
        let kept = agent_sends(&harness, 0, "kept");
        let deleted = agent_sends(&harness, 0, "deleted");
        assert_eq!(copy_ids(&guest), [kept.clone(), deleted.clone()]);

        let answer = local_call(
            &harness,
            json!({"id": "d", "method": "gram.delete", "params": {"id": deleted, "caller_pane_id": harness.pane_ids[0]}}),
        );
        assert_eq!(answer["result"]["type"], "ok", "{answer}");
        assert_eq!(copy_ids(&guest), std::slice::from_ref(&kept));

        // A delete the coordinator refuses keeps the copy.
        let refused = local_call(
            &harness,
            json!({"id": "d", "method": "gram.delete", "params": {"id": kept, "caller_pane_id": harness.pane_ids[1]}}),
        );
        assert_eq!(refused["error"]["code"], "not_found", "{refused}");
        assert_eq!(copy_ids(&guest), [kept]);
    }

    #[test]
    fn copies_of_grams_the_owner_deleted_on_the_coordinator_are_dropped() {
        let (_config, harness, coordinator) = relay_remote("relay-owner-delete");
        let guest = harness.admit_with(0, true);
        let kept = agent_sends(&harness, 0, "kept");
        let gone = agent_sends(&harness, 0, "gone");
        let own = harness.call(
            &guest,
            json!({"id": "p", "method": "gram.post", "params": {"text": "mine"}}),
        );
        let own = own[0]["result"]["message"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        coordinator.owner_deletes(&gone);
        coordinator.owner_deletes(&own);

        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["kept"], "{listed}");
        assert_eq!(copy_ids(&guest), std::slice::from_ref(&kept));
        let fetch = harness.call(
            &guest,
            json!({"id": "m", "method": "gram.mark_read", "params": {"ids": [gone]}}),
        );
        assert_eq!(code(&fetch), "guest_forbidden");

        // Checked at most once per interval, not on every poll.
        guest_list(&harness, &guest, json!({}));
        assert_eq!(coordinator.state.lock().unwrap().lists, 1);
    }

    #[test]
    fn an_unreachable_view_drops_no_copy() {
        let (_config, harness, _coordinator) = relay_remote("relay-paused");
        let guest = harness.admit_with(0, true);
        agent_sends(&harness, 0, "kept");
        // The agent is not running: its coordinator view cannot be read.
        harness.agent_exits();
        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["kept"], "{listed}");
    }

    #[test]
    fn the_relay_fixture_leaves_no_relay_behind() {
        let previous = std::env::var_os(crate::api::gram_relay::SOCKET_ENV);
        let (config, harness, coordinator) = relay_remote("relay-scope");
        drop(coordinator);
        drop(harness);
        drop(config);
        assert_eq!(
            std::env::var_os(crate::api::gram_relay::SOCKET_ENV),
            previous
        );
        assert_eq!(
            crate::api::gram_relay::policy().remote_socket().is_some(),
            previous.is_some()
        );
    }

    fn agent_sends_file(harness: &Harness, text: &str, name: &str) -> String {
        let upload = format!("up-{name}");
        let staged = local_call(
            harness,
            json!({"id": "u", "method": "gram.upload_chunk", "params": {"upload_id": upload, "offset": 0, "data_base64": "aGVsbG8gd29ybGQ="}}),
        );
        assert_eq!(staged["result"]["type"], "ok", "{staged}");
        let sent = local_call(
            harness,
            json!({"id": "s", "method": "gram.send", "params": {"text": text, "caller_pane_id": harness.pane_ids[0], "file": {"upload_id": upload, "name": name, "mime": "text/plain"}}}),
        );
        sent["result"]["message"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("sent: {sent}"))
            .to_string()
    }

    fn guest_file(harness: &Harness, guest: &GuestPrincipal, id: &str) -> Value {
        harness.call(
            guest,
            json!({"id": "f", "method": "gram.get_file", "params": {"id": id}}),
        )[0]["result"]["data_base64"]
            .clone()
    }

    #[test]
    fn turning_sharing_on_brings_back_what_was_relayed_meanwhile() {
        let (_config, harness, coordinator) = relay_remote("relay-backfill");
        let guest = harness.admit_with(0, false);
        let sent = agent_sends_file(&harness, "while off", "report.txt");
        let staged = harness.call(
            &guest,
            json!({"id": "u", "method": "gram.upload_chunk", "params": {"upload_id": "g-up", "offset": 0, "data_base64": "aGVsbG8="}}),
        );
        assert_eq!(staged[0]["result"]["type"], "ok", "{staged:?}");
        let posted = harness.call(
            &guest,
            json!({"id": "p", "method": "gram.post", "params": {"text": "my post", "file": {"upload_id": "g-up", "name": "a.txt", "mime": "text/plain"}}}),
        );
        let own = posted[0]["result"]["message"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("posted: {posted:?}"))
            .to_string();
        // Named like the shared agent, but never relayed from its pane here.
        coordinator.foreign_gram("impostor");
        assert!(copy_ids(&guest).is_empty(), "no copy while not sharing");

        crate::guest::update_at(&guest.dir, &guest.guest_id, true)
            .unwrap()
            .unwrap();
        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["my post", "while off"], "{listed}");
        assert_eq!(guest_file(&harness, &guest, &sent), "aGVsbG8gd29ybGQ=");
        assert_eq!(guest_file(&harness, &guest, &own), "aGVsbG8=");
    }

    #[test]
    fn copies_dropped_by_turning_sharing_off_come_back_when_it_is_on_again() {
        let (_config, harness, _coordinator) = relay_remote("relay-reshare");
        let guest = harness.admit_with(0, true);
        let sent = agent_sends_file(&harness, "shared", "report.txt");
        assert_eq!(copy_ids(&guest), std::slice::from_ref(&sent));
        crate::guest::update_at(&guest.dir, &guest.guest_id, false)
            .unwrap()
            .unwrap();
        assert!(copy_ids(&guest).is_empty());

        crate::guest::update_at(&guest.dir, &guest.guest_id, true)
            .unwrap()
            .unwrap();
        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["shared"], "{listed}");
        assert_eq!(guest_file(&harness, &guest, &sent), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn a_copy_whose_file_went_missing_is_fetched_again() {
        let (_config, harness, _coordinator) = relay_remote("relay-repair");
        let guest = harness.admit_with(0, true);
        let sent = agent_sends_file(&harness, "report", "report.txt");
        let file = crate::guest::mirror::file_of(&guest.dir, &sent).expect("a kept file");
        std::fs::remove_file(&file).unwrap();

        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["report"], "{listed}");
        assert_eq!(guest_file(&harness, &guest, &sent), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn a_copy_whose_file_expired_on_the_coordinator_comes_back_without_bytes() {
        // The coordinator's attachment budget expires old files but keeps their messages
        // (herdr#298). A guest copy dropped meanwhile must come back as the same message,
        // with the file's metadata as `expired_file`, not be lost for want of bytes.
        let (_config, harness, coordinator) = relay_remote("relay-expired");
        let guest = harness.admit_with(0, true);
        let sent = agent_sends_file(&harness, "old report", "report.txt");
        crate::guest::update_at(&guest.dir, &guest.guest_id, false)
            .unwrap()
            .unwrap();
        assert!(copy_ids(&guest).is_empty());
        coordinator.expires_file(&sent);

        crate::guest::update_at(&guest.dir, &guest.guest_id, true)
            .unwrap()
            .unwrap();
        let listed = guest_list(&harness, &guest, json!({}));
        assert_eq!(texts(&listed), ["old report"], "{listed}");
        let message = &listed["messages"][0];
        assert!(message.get("file").is_none(), "{message}");
        assert_eq!(message["expired_file"]["name"], "report.txt", "{message}");
    }
}
