//! `agent.answer` (herdr#308): pick one option of an omp agent's pending `ask`
//! question by driving its on-screen selector, then confirm the pick from the
//! agent's session file.
//!
//! Runs on the API connection thread: every pane read and key write is a
//! normal app request, so the app loop is never blocked while the selector
//! settles or the answer is confirmed.

mod selector;
mod session;

pub(crate) use session::{pending_ask, AskQuestion, PendingAsk};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::agent_resume::AgentSessionRefKind;
use crate::api::schema::{
    AgentAnswerParams, AgentAnswerStatus, AgentInfo, ErrorBody, ErrorResponse, Method,
    PaneReadParams, PaneReadResult, PaneSendKeysParams, ReadFormat, ReadIntent, ReadSource,
    Request, ResponseResult, SuccessResponse,
};
use crate::api::server::{dispatch_to_app_with_timeout, APP_RESPONSE_TIMEOUT};
use crate::api::ApiRequestSender;

/// How long an answered (Enter sent) ask stays claimed, so a retry while omp
/// is still writing its tool result cannot press Enter a second time.
const CLAIM_TTL: Duration = Duration::from_secs(60);

/// Waits used while driving and confirming; tests shrink them.
#[derive(Debug, Clone, Copy)]
struct Timing {
    /// Between selector screen reads.
    selector_pause: Duration,
    /// Between session file looks for the answer.
    confirm_poll: Duration,
    /// How long to look for the answer after Enter.
    confirm_timeout: Duration,
}

const LIVE: Timing = Timing {
    selector_pause: Duration::from_millis(50),
    confirm_poll: Duration::from_millis(100),
    confirm_timeout: Duration::from_secs(5),
};

pub(super) fn answer_agent(
    request_id: String,
    params: AgentAnswerParams,
    api_tx: &ApiRequestSender,
) -> String {
    answer_response(request_id, &params, api_tx, LIVE)
}

fn answer_response(
    request_id: String,
    params: &AgentAnswerParams,
    api_tx: &ApiRequestSender,
    timing: Timing,
) -> String {
    match answer(&request_id, params, api_tx, timing) {
        Ok(status) => serde_json::to_string(&SuccessResponse {
            id: request_id,
            result: ResponseResult::AgentAnswer { status },
        }),
        Err(error) => serde_json::to_string(&ErrorResponse {
            id: request_id,
            error,
        }),
    }
    .expect("agent.answer response serializes")
}

fn answer(
    request_id: &str,
    params: &AgentAnswerParams,
    api_tx: &ApiRequestSender,
    timing: Timing,
) -> Result<AgentAnswerStatus, ErrorBody> {
    let agent = crate::api::wait::agent_get(request_id, &params.target, api_tx)
        .map_err(|response| response.error)?;
    let session_path = omp_session_path(&agent).ok_or_else(|| {
        error(
            "ask_unsupported",
            "agent.answer needs an omp agent with a session file",
        )
    })?;
    let mut claim = Claim::take(&agent.pane_id, &params.ask_id).ok_or_else(|| {
        error(
            "ask_not_pending",
            "this ask is already being answered or was just answered",
        )
    })?;
    let pending = session::pending_ask(&session_path)
        .map_err(|err| {
            error(
                "ask_not_pending",
                format!("the agent's session file cannot be read: {err}"),
            )
        })?
        .filter(|ask| ask.id == params.ask_id)
        .ok_or_else(|| error("ask_not_pending", "the agent is not waiting on this ask"))?;
    let question = pending.single_select().ok_or_else(|| {
        error(
            "ask_unsupported",
            "only a single single-select question can be answered",
        )
    })?;
    let target = question
        .options
        .iter()
        .position(|option| option == &params.option)
        .ok_or_else(|| error("invalid_request", "option is not one of the ask's labels"))?;
    let rows = selector::display_rows(
        &question.options,
        question
            .recommended
            .and_then(|index| usize::try_from(index).ok()),
    );
    let offset = session::len(&session_path).map_err(|err| {
        error(
            "ask_not_pending",
            format!("the agent's session file cannot be read: {err}"),
        )
    })?;

    let mut pane = AppPane {
        request_id,
        pane_id: &agent.pane_id,
        api_tx,
        pause: timing.selector_pause,
    };
    selector::select_and_submit(&mut pane, &rows, target)?;
    // Enter is out: from here on a retry must not press it again.
    claim.keep();
    Ok(confirm(
        &session_path,
        offset,
        &params.ask_id,
        &params.option,
        timing,
    ))
}

/// The session file of an omp agent whose session herdr tracks by path.
fn omp_session_path(agent: &AgentInfo) -> Option<PathBuf> {
    if agent.agent.as_deref() != Some("omp") {
        return None;
    }
    let session = agent.agent_session.as_ref()?;
    (session.kind == AgentSessionRefKind::Path && !session.value.is_empty())
        .then(|| PathBuf::from(&session.value))
}

/// Look for the ask's tool result written after `offset`: `answered` once it
/// records exactly `option`, `sent_unconfirmed` if it records something else
/// or does not show up in time.
fn confirm(
    path: &Path,
    offset: u64,
    ask_id: &str,
    option: &str,
    timing: Timing,
) -> AgentAnswerStatus {
    let deadline = Instant::now() + timing.confirm_timeout;
    loop {
        if let Ok(Some(selected)) = session::answer_since(path, offset, ask_id) {
            return if selected == [option] {
                AgentAnswerStatus::Answered
            } else {
                AgentAnswerStatus::SentUnconfirmed
            };
        }
        if Instant::now() >= deadline {
            return AgentAnswerStatus::SentUnconfirmed;
        }
        std::thread::sleep(timing.confirm_poll);
    }
}

fn error(code: &str, message: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code: code.to_owned(),
        message: message.into(),
    }
}

/// `(pane id, ask id)` pairs being answered, or answered less than
/// [`CLAIM_TTL`] ago.
static CLAIMS: LazyLock<Mutex<HashMap<(String, String), Instant>>> =
    LazyLock::new(Default::default);

/// One caller's hold on an ask. Dropped without [`Claim::keep`], it is
/// released at once so a failed attempt can be retried; kept, it lasts until
/// [`CLAIM_TTL`] after it was taken.
struct Claim {
    key: (String, String),
    keep: bool,
}

impl Claim {
    fn take(pane_id: &str, ask_id: &str) -> Option<Self> {
        let mut claims = CLAIMS.lock().unwrap_or_else(|err| err.into_inner());
        let now = Instant::now();
        claims.retain(|_, taken| now.duration_since(*taken) < CLAIM_TTL);
        let key = (pane_id.to_owned(), ask_id.to_owned());
        if claims.contains_key(&key) {
            return None;
        }
        claims.insert(key.clone(), now);
        Some(Self { key, keep: false })
    }

    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.keep {
            CLAIMS
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .remove(&self.key);
        }
    }
}

/// The agent's pane, read and typed into through the app.
struct AppPane<'a> {
    request_id: &'a str,
    pane_id: &'a str,
    api_tx: &'a ApiRequestSender,
    pause: Duration,
}

impl AppPane<'_> {
    fn call(&self, suffix: &str, method: Method) -> Result<serde_json::Value, ErrorBody> {
        let response = dispatch_to_app_with_timeout(
            Request {
                id: format!("{}:{suffix}", self.request_id),
                method,
            },
            self.api_tx,
            Some(APP_RESPONSE_TIMEOUT),
        );
        let mut value: serde_json::Value = serde_json::from_str(&response)
            .map_err(|_| error("internal_error", "failed to decode app response"))?;
        if let Some(body) = value.get_mut("error") {
            return Err(serde_json::from_value(body.take())
                .unwrap_or_else(|_| error("internal_error", "failed to decode app error")));
        }
        Ok(value)
    }
}

impl selector::SelectorPane for AppPane<'_> {
    fn read_screen(&mut self) -> Result<String, ErrorBody> {
        let mut value = self.call(
            "read",
            Method::PaneRead(PaneReadParams {
                pane_id: self.pane_id.to_owned(),
                source: ReadSource::Visible,
                lines: None,
                format: ReadFormat::Text,
                strip_ansi: true,
                intent: ReadIntent::Passive,
            }),
        )?;
        serde_json::from_value::<PaneReadResult>(value["result"]["read"].take())
            .map(|read| read.text)
            .map_err(|_| error("internal_error", "failed to decode pane read result"))
    }

    fn send_keys(&mut self, keys: &[&str]) -> Result<(), ErrorBody> {
        self.call(
            "keys",
            Method::PaneSendKeys(PaneSendKeysParams {
                pane_id: self.pane_id.to_owned(),
                keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            }),
        )
        .map(drop)
    }

    fn pause(&mut self) {
        std::thread::sleep(self.pause);
    }
}

#[cfg(test)]
mod tests;
