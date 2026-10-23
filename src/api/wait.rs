use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use regex::Regex;

use crate::api::schema::{
    ErrorBody, ErrorResponse, EventData, EventEnvelope, EventKind, EventMatch, EventsWaitParams,
    Method, Request, ResponseResult, Subscription, SubscriptionEventData,
    SubscriptionEventEnvelope, SuccessResponse,
};
use crate::api::server::{
    dispatch_to_app_with_caller_timeout, dispatch_to_app_with_timeout, should_stop_connection,
    APP_RESPONSE_TIMEOUT, CONNECTION_POLL_INTERVAL,
};
use crate::api::subscriptions::ActiveSubscription;
use crate::api::subscriptions::{match_output, output_match_read_source};
use crate::api::{ApiRequestSender, ApiStream, EventHub};

const AGENT_PROMPT_EFFECT_TIMEOUT_MS: u64 = 5_000;

pub(super) fn wait_for_output(
    request_id: String,
    params: crate::api::schema::PaneWaitForOutputParams,
    stream: &mut ApiStream,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    crate::logging::api_wait_started(&request_id, &params.pane_id, params.timeout_ms);
    let deadline = params
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));

    let regex = match &params.r#match {
        crate::api::schema::OutputMatch::Regex { value } => match Regex::new(value) {
            Ok(regex) => Some(regex),
            Err(err) => {
                return Ok(Some(
                    serde_json::to_string(&ErrorResponse {
                        id: request_id,
                        error: ErrorBody {
                            code: "invalid_regex".into(),
                            message: err.to_string(),
                        },
                    })
                    .unwrap(),
                ));
            }
        },
        crate::api::schema::OutputMatch::Substring { .. } => None,
    };

    loop {
        if should_stop_connection(stream, running)? {
            crate::logging::api_wait_completed(&request_id, &params.pane_id, "client_disconnected");
            return Ok(None);
        }

        let read_request = Request {
            id: format!("{request_id}:read"),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: params.pane_id.clone(),
                source: output_match_read_source(&params.source),
                lines: params.lines,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: params.strip_ansi,
                intent: crate::api::schema::ReadIntent::Passive,
            }),
        };
        let response =
            dispatch_to_app_with_timeout(read_request, api_tx, Some(APP_RESPONSE_TIMEOUT));
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) else {
            return Ok(Some(response));
        };
        if value.get("error").is_some() {
            let mut value = value;
            value["id"] = serde_json::Value::String(request_id.clone());
            return Ok(Some(serde_json::to_string(&value).unwrap()));
        }

        let read_value = value["result"]["read"].clone();
        let Ok(read) = serde_json::from_value::<crate::api::schema::PaneReadResult>(read_value)
        else {
            return Ok(Some(
                serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "internal_error".into(),
                        message: "failed to decode pane read result".into(),
                    },
                })
                .unwrap(),
            ));
        };

        let matched_line = match_output(&read.text, &params.r#match, regex.as_ref());
        if matched_line.is_some() {
            let revision = read.revision;
            crate::logging::api_wait_completed(&request_id, &params.pane_id, "matched");
            return Ok(Some(
                serde_json::to_string(&SuccessResponse {
                    id: request_id,
                    result: ResponseResult::OutputMatched {
                        pane_id: read.pane_id.clone(),
                        revision,
                        matched_line,
                        read,
                    },
                })
                .unwrap(),
            ));
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            crate::logging::api_wait_timed_out(&request_id, &params.pane_id);
            return Ok(Some(
                serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "timeout".into(),
                        message: "timed out waiting for output match".into(),
                    },
                })
                .unwrap(),
            ));
        }

        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

pub(super) fn wait_for_agent(
    request_id: String,
    params: crate::api::schema::AgentWaitParams,
    stream: &mut ApiStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let last_event_sequence = event_hub.current_sequence();
    let initial = match agent_get(&request_id, &params.target, api_tx) {
        Ok(agent) => agent,
        Err(response) => {
            return serde_json::to_string(&response)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    };
    let until = agent_wait_statuses(params.until);
    if agent_wait_matches(&initial, &until, None) {
        return agent_wait_success(request_id, initial).map(Some);
    }

    match wait_for_resolved_agent(
        request_id.clone(),
        ResolvedAgentWait {
            target: params.target,
            until,
            timeout_ms: params.timeout_ms,
            initial,
            last_event_sequence,
            after_state_change_seq: None,
            accept_transient_status: true,
            timeout_kind: AgentWaitTimeoutKind::Status,
        },
        stream,
        api_tx,
        event_hub,
        running,
    )? {
        Some(AgentWaitOutcome::Matched(agent)) => agent_wait_success(request_id, *agent).map(Some),
        Some(AgentWaitOutcome::Response(response)) => Ok(Some(response)),
        None => Ok(None),
    }
}

pub(super) fn prompt_agent(
    request_id: String,
    params: crate::api::schema::AgentPromptParams,
    stream: &mut ApiStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    prompt_agent_with_effect_timeout(
        request_id,
        params,
        stream,
        api_tx,
        event_hub,
        running,
        AGENT_PROMPT_EFFECT_TIMEOUT_MS,
    )
}

fn prompt_agent_with_effect_timeout(
    request_id: String,
    mut params: crate::api::schema::AgentPromptParams,
    stream: &mut ApiStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    effect_timeout_cap_ms: u64,
) -> std::io::Result<Option<String>> {
    let Some(wait) = params.wait.clone() else {
        return Ok(Some(dispatch_to_app_with_timeout(
            Request {
                id: request_id,
                method: Method::AgentPrompt(params),
            },
            api_tx,
            None,
        )));
    };

    let wait_started = std::time::Instant::now();
    let before_prompt = match agent_get_for_prompt(
        &request_id,
        &params.target,
        api_tx,
        wait.timeout_ms,
        wait_started,
    ) {
        Ok(agent) => agent,
        Err(response) => {
            return serde_json::to_string(&response)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    };
    let prompt_started_working =
        before_prompt.agent_status == crate::api::schema::AgentStatus::Working;
    let target = params.target.clone();
    if let Some(prompt_wait) = params.wait.as_mut() {
        prompt_wait.submission_deadline = wait
            .timeout_ms
            .map(|timeout_ms| wait_started + std::time::Duration::from_millis(timeout_ms));
    }
    let last_event_sequence = event_hub.current_sequence();
    let prompt_request = Request {
        id: request_id.clone(),
        method: Method::AgentPrompt(params),
    };
    #[cfg(windows)]
    let prompt_response = dispatch_to_app_with_caller_timeout(
        prompt_request,
        api_tx,
        remaining_timeout_ms(wait.timeout_ms, wait_started).map(std::time::Duration::from_millis),
    );
    #[cfg(not(windows))]
    let prompt_response = dispatch_to_app_with_timeout(prompt_request, api_tx, None);
    // Only the app's explicit PTY-write receipt authorizes a later
    // WrittenToPty result. A generic agent-shaped success is not evidence.
    let prompted = match serde_json::from_str::<SuccessResponse>(&prompt_response) {
        Ok(SuccessResponse {
            id,
            result:
                ResponseResult::AgentPrompted {
                    agent,
                    delivery: Some(crate::api::schema::AgentPromptDelivery::WrittenToPty),
                },
        }) if id == request_id => agent,
        _ => return Ok(Some(prompt_response)),
    };
    if !agent_wait_identity_matches(
        &prompted,
        &before_prompt.terminal_id,
        before_prompt.name.as_deref().filter(|name| *name == target),
        before_prompt.agent.as_deref(),
    ) {
        return agent_prompt_success(
            request_id,
            prompted,
            crate::api::schema::AgentPromptDelivery::WrittenToPty,
        )
        .map(Some);
    }
    let composer_attempt_id = prompted.composer.attempt_id.clone();
    let prompt_state_change_seq = prompted.state_change_seq;
    let until = agent_wait_statuses(wait.until);
    let effect_timeout_ms = wait.timeout_ms.map_or(effect_timeout_cap_ms, |timeout_ms| {
        timeout_ms.min(effect_timeout_cap_ms)
    });
    let Some(effect) = observe_prompt_effect(
        &request_id,
        &target,
        &before_prompt,
        prompted,
        prompt_state_change_seq,
        prompt_started_working,
        composer_attempt_id.as_deref(),
        effect_timeout_ms,
        stream,
        api_tx,
        running,
    )?
    else {
        return Ok(None);
    };
    let (mut initial, delivery, composer_submission_observed) = match effect {
        PromptEffectOutcome::Submitted(agent, composer_cleared) => (
            agent,
            crate::api::schema::AgentPromptDelivery::Submitted,
            composer_cleared,
        ),
        PromptEffectOutcome::WrittenToPty(agent) => (
            agent,
            crate::api::schema::AgentPromptDelivery::WrittenToPty,
            false,
        ),
        PromptEffectOutcome::Response(response) => return Ok(Some(response)),
    };
    // A PTY write is a receipt, not proof of submission. If submission could
    // not be verified, do not turn the subsequent status wait into a failure
    // (or imply that resending the prompt is safe).
    if delivery == crate::api::schema::AgentPromptDelivery::WrittenToPty {
        return agent_prompt_success(request_id, initial, delivery).map(Some);
    }
    // The submission observation itself may capture a fast settled transition.
    // A new lifecycle sequence or an observed same-attempt composer clear proves
    // this status belongs to a post-write sample, not a pre-prompt idle frame.
    if agent_wait_matches(&initial, &until, Some(prompt_state_change_seq))
        || (composer_submission_observed && until.contains(&initial.agent_status))
    {
        return agent_prompt_success(request_id, initial, delivery).map(Some);
    }
    let prompt_activity_observed = prompt_started_working
        || matches!(
            initial.agent_status,
            crate::api::schema::AgentStatus::Working | crate::api::schema::AgentStatus::Blocked
        );
    if !prompt_activity_observed {
        let remaining_timeout_ms = remaining_timeout_ms(wait.timeout_ms, wait_started);
        let stall_timeout_ms = remaining_timeout_ms
            .filter(|ms| *ms <= AGENT_PROMPT_EFFECT_TIMEOUT_MS)
            .unwrap_or(AGENT_PROMPT_EFFECT_TIMEOUT_MS);
        // A fast turn can go directly to done/idle without a sampled working
        // frame. Its new lifecycle sequence is enough to satisfy a requested
        // status; stale pre-prompt status is excluded below.
        let mut activity_statuses = prompt_activity_statuses();
        for status in &until {
            if !activity_statuses.contains(status) {
                activity_statuses.push(*status);
            }
        }
        let Some(outcome) = wait_for_resolved_agent(
            request_id.clone(),
            ResolvedAgentWait {
                target: target.clone(),
                until: activity_statuses,
                timeout_ms: Some(stall_timeout_ms),
                initial,
                last_event_sequence,
                after_state_change_seq: Some(prompt_state_change_seq),
                accept_transient_status: true,
                timeout_kind: AgentWaitTimeoutKind::AfterSubmitted,
            },
            stream,
            api_tx,
            event_hub,
            running,
        )?
        else {
            return Ok(None);
        };
        initial = match outcome {
            AgentWaitOutcome::Matched(agent) => *agent,
            AgentWaitOutcome::Response(response) => return Ok(Some(response)),
        };
    }
    if agent_wait_matches(&initial, &until, None) {
        return agent_prompt_success(request_id, initial, delivery).map(Some);
    }

    let Some(outcome) = wait_for_resolved_agent(
        request_id.clone(),
        ResolvedAgentWait {
            target,
            until,
            timeout_ms: remaining_timeout_ms(wait.timeout_ms, wait_started),
            initial,
            last_event_sequence,
            after_state_change_seq: None,
            accept_transient_status: false,
            timeout_kind: AgentWaitTimeoutKind::AfterSubmitted,
        },
        stream,
        api_tx,
        event_hub,
        running,
    )?
    else {
        return Ok(None);
    };
    let agent = match outcome {
        AgentWaitOutcome::Matched(agent) => *agent,
        AgentWaitOutcome::Response(response) => return Ok(Some(response)),
    };
    agent_prompt_success(request_id, agent, delivery).map(Some)
}

fn remaining_timeout_ms(total_ms: Option<u64>, started: std::time::Instant) -> Option<u64> {
    total_ms.map(|total_ms| {
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        total_ms.saturating_sub(elapsed_ms)
    })
}

fn agent_prompt_success(
    request_id: String,
    agent: crate::api::schema::AgentInfo,
    delivery: crate::api::schema::AgentPromptDelivery,
) -> std::io::Result<String> {
    serde_json::to_string(&SuccessResponse {
        id: request_id,
        result: ResponseResult::AgentPrompted {
            agent,
            delivery: Some(delivery),
        },
    })
    .map_err(std::io::Error::other)
}

enum PromptEffectOutcome {
    Submitted(crate::api::schema::AgentInfo, bool),
    WrittenToPty(crate::api::schema::AgentInfo),
    Response(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptObservationVerdict {
    Submitted,
    WrittenToPty,
    Unsubmitted,
}

/// Classifies only evidence attributable to this PTY write. A missing composer
/// observation is not proof of non-submission, nor is a deadline.
fn classify_prompt_observation(
    initially_working: bool,
    baseline: u64,
    current_sequence: u64,
    composer_clear_observed: bool,
    composer_matches: bool,
    timed_out: bool,
) -> Option<PromptObservationVerdict> {
    if composer_clear_observed || (!initially_working && current_sequence > baseline) {
        return Some(PromptObservationVerdict::Submitted);
    }
    if !timed_out {
        return None;
    }
    if composer_matches {
        return Some(PromptObservationVerdict::Unsubmitted);
    }
    Some(PromptObservationVerdict::WrittenToPty)
}

// The observation boundary keeps identity, evidence, timeout, and transport
// inputs explicit so prompt attribution cannot accidentally reuse wait state.
#[allow(clippy::too_many_arguments)]
fn observe_prompt_effect(
    request_id: &str,
    target: &str,
    before_prompt: &crate::api::schema::AgentInfo,
    mut current: crate::api::schema::AgentInfo,
    baseline: u64,
    initially_working: bool,
    composer_attempt_id: Option<&str>,
    timeout_ms: u64,
    stream: &mut ApiStream,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<PromptEffectOutcome>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let expected_name = before_prompt.name.as_deref().filter(|name| *name == target);
    let mut composer_observed = false;

    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }
        if !agent_wait_identity_matches(
            &current,
            &before_prompt.terminal_id,
            expected_name,
            before_prompt.agent.as_deref(),
        ) {
            return Ok(Some(PromptEffectOutcome::WrittenToPty(
                before_prompt.clone(),
            )));
        }

        let mut composer_clear_observed = false;
        let same_attempt = composer_attempt_id.is_some()
            && current.composer.attempt_id.as_deref() == composer_attempt_id;
        let composer_matches = same_attempt
            && current.composer.state == crate::api::schema::ComposerState::DraftPresent;
        if composer_matches {
            composer_observed = true;
        }
        let stable_empty_region = current.composer.evidence.frame_stable
            && current.composer.evidence.region
                == crate::api::schema::ComposerRegionEvidence::Empty
            && current.composer.evidence.cursor
                != crate::api::schema::ComposerCursorEvidence::Conflict
            && current.composer.evidence.style
                != crate::api::schema::ComposerStyleEvidence::Conflict;
        if same_attempt && stable_empty_region && composer_observed {
            composer_clear_observed = true;
        }

        match classify_prompt_observation(
            initially_working,
            baseline,
            current.state_change_seq,
            composer_clear_observed,
            composer_matches,
            std::time::Instant::now() >= deadline,
        ) {
            Some(PromptObservationVerdict::Submitted) => {
                return Ok(Some(PromptEffectOutcome::Submitted(
                    current,
                    composer_clear_observed,
                )));
            }
            Some(PromptObservationVerdict::WrittenToPty) => {
                return Ok(Some(PromptEffectOutcome::WrittenToPty(current)));
            }
            Some(PromptObservationVerdict::Unsubmitted) => {
                return agent_prompt_observation_error(
                    request_id,
                    "agent_prompt_unsubmitted",
                    "agent prompt remains visible in the live composer after the PTY write",
                )
                .map(PromptEffectOutcome::Response)
                .map(Some);
            }
            None if current.composer.evidence.region
                == crate::api::schema::ComposerRegionEvidence::Unavailable
                && current
                    .agent
                    .as_deref()
                    .or(before_prompt.agent.as_deref())
                    .and_then(crate::detect::parse_agent_label)
                    .is_some_and(|agent| {
                        !crate::detect::manifest::submission_verification_supported(agent)
                    }) =>
            {
                // The active manifest expressly cannot observe the composer.
                // Submission proof above still wins, but waiting for an
                // instrument we do not have cannot improve this receipt.
                return Ok(Some(PromptEffectOutcome::WrittenToPty(current)));
            }
            None => {}
        }

        std::thread::sleep(CONNECTION_POLL_INTERVAL);
        current = match agent_get(request_id, target, api_tx) {
            Ok(agent) => agent,
            Err(_) => {
                // The write has already been acknowledged. A failed probe
                // cannot revoke it or establish non-submission.
                return Ok(Some(PromptEffectOutcome::WrittenToPty(current)));
            }
        };
    }
}

fn agent_prompt_observation_error(
    request_id: &str,
    code: &str,
    message: &str,
) -> std::io::Result<String> {
    serde_json::to_string(&ErrorResponse {
        id: request_id.to_string(),
        error: ErrorBody {
            code: code.to_string(),
            message: message.to_string(),
        },
    })
    .map_err(std::io::Error::other)
}

struct ResolvedAgentWait {
    target: String,
    until: Vec<crate::api::schema::AgentStatus>,
    timeout_ms: Option<u64>,
    initial: crate::api::schema::AgentInfo,
    last_event_sequence: u64,
    after_state_change_seq: Option<u64>,
    accept_transient_status: bool,
    timeout_kind: AgentWaitTimeoutKind,
}

#[derive(Clone, Copy)]
enum AgentWaitTimeoutKind {
    Status,
    AfterSubmitted,
}

enum AgentWaitOutcome {
    Matched(Box<crate::api::schema::AgentInfo>),
    Response(String),
}

fn wait_for_resolved_agent(
    request_id: String,
    wait: ResolvedAgentWait,
    stream: &mut ApiStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<AgentWaitOutcome>> {
    let deadline = wait
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
    let expected_terminal_id = wait.initial.terminal_id.clone();
    let expected_name = wait
        .initial
        .name
        .as_ref()
        .filter(|name| name.as_str() == wait.target)
        .cloned();
    let expected_agent = wait.initial.agent.clone();
    let pane_id = wait.initial.pane_id.clone();
    let mut last_event_sequence = wait.last_event_sequence;

    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }

        let mut should_probe = false;
        let mut matched_event_status = None;
        for (sequence, event) in event_hub.events_after(last_event_sequence) {
            last_event_sequence = sequence;
            match event.data {
                EventData::PaneAgentDetected {
                    pane_id: event_pane,
                    agent,
                    released,
                    final_status,
                    ..
                } if event_pane == pane_id => {
                    if released {
                        if let Some(status) = final_status
                            .filter(|status| wait.until.contains(status))
                            .or(matched_event_status)
                        {
                            let mut matched = wait.initial.clone();
                            matched.agent_status = status;
                            return Ok(Some(AgentWaitOutcome::Matched(Box::new(matched))));
                        }
                        return agent_wait_not_running(request_id)
                            .map(AgentWaitOutcome::Response)
                            .map(Some);
                    }
                    if agent.is_some() && expected_agent.is_some() && agent != expected_agent {
                        return agent_wait_not_running(request_id)
                            .map(AgentWaitOutcome::Response)
                            .map(Some);
                    }
                    should_probe = true;
                }
                EventData::PaneAgentStatusChanged {
                    pane_id: event_pane,
                    agent_status,
                    ..
                } if event_pane == pane_id => {
                    if wait.accept_transient_status && wait.until.contains(&agent_status) {
                        matched_event_status = Some(agent_status);
                    }
                    should_probe = true;
                }
                EventData::PaneUpdated { pane } if pane.pane_id == pane_id => should_probe = true,
                EventData::PaneMoved {
                    previous_pane_id, ..
                } if previous_pane_id == pane_id => {
                    return agent_wait_not_running(request_id)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
                EventData::PaneClosed {
                    pane_id: event_pane,
                    ..
                }
                | EventData::PaneExited {
                    pane_id: event_pane,
                    ..
                } if event_pane == pane_id => {
                    return agent_wait_not_running(request_id)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
                _ => {}
            }
        }

        if should_probe {
            let current = match agent_get(&request_id, &wait.target, api_tx) {
                Ok(agent) => agent,
                Err(response) => {
                    return agent_wait_probe_error(response)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
            };
            if !agent_wait_identity_matches(
                &current,
                &expected_terminal_id,
                expected_name.as_deref(),
                expected_agent.as_deref(),
            ) {
                return agent_wait_not_running(request_id)
                    .map(AgentWaitOutcome::Response)
                    .map(Some);
            }
            if let Some(status) = matched_event_status {
                let mut matched = current;
                matched.agent_status = status;
                return Ok(Some(AgentWaitOutcome::Matched(Box::new(matched))));
            }
            if agent_wait_matches(&current, &wait.until, wait.after_state_change_seq) {
                return Ok(Some(AgentWaitOutcome::Matched(Box::new(current))));
            }
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            let current = match agent_get(&request_id, &wait.target, api_tx) {
                Ok(agent) => agent,
                Err(response) => {
                    return agent_wait_probe_error(response)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
            };
            if !agent_wait_identity_matches(
                &current,
                &expected_terminal_id,
                expected_name.as_deref(),
                expected_agent.as_deref(),
            ) {
                return agent_wait_not_running(request_id)
                    .map(AgentWaitOutcome::Response)
                    .map(Some);
            }
            if agent_wait_matches(&current, &wait.until, wait.after_state_change_seq) {
                return Ok(Some(AgentWaitOutcome::Matched(Box::new(current))));
            }
            return agent_wait_timeout(request_id, wait.timeout_kind, &current)
                .map(AgentWaitOutcome::Response)
                .map(Some);
        }
        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

fn prompt_activity_statuses() -> Vec<crate::api::schema::AgentStatus> {
    vec![
        crate::api::schema::AgentStatus::Working,
        crate::api::schema::AgentStatus::Blocked,
    ]
}
fn agent_wait_statuses(
    until: Vec<crate::api::schema::AgentStatus>,
) -> Vec<crate::api::schema::AgentStatus> {
    if until.is_empty() {
        vec![
            crate::api::schema::AgentStatus::Idle,
            crate::api::schema::AgentStatus::Done,
            crate::api::schema::AgentStatus::Blocked,
        ]
    } else {
        until
    }
}

fn agent_wait_identity_matches(
    agent: &crate::api::schema::AgentInfo,
    expected_terminal_id: &str,
    expected_name: Option<&str>,
    expected_agent: Option<&str>,
) -> bool {
    agent.terminal_id == expected_terminal_id
        && expected_name.is_none_or(|name| agent.name.as_deref() == Some(name))
        && match (expected_agent, agent.agent.as_deref()) {
            (Some(expected), Some(current)) => expected == current,
            (Some(_), None) => agent.name.is_some(),
            (None, _) => true,
        }
}

fn agent_wait_matches(
    agent: &crate::api::schema::AgentInfo,
    until: &[crate::api::schema::AgentStatus],
    after_state_change_seq: Option<u64>,
) -> bool {
    until.contains(&agent.agent_status)
        && after_state_change_seq.is_none_or(|baseline| agent.state_change_seq > baseline)
}

pub(super) fn agent_get(
    request_id: &str,
    target: &str,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::AgentInfo, ErrorResponse> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: format!("{request_id}:agent"),
            method: Method::AgentGet(crate::api::schema::AgentTarget {
                target: target.to_string(),
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
    agent_from_response(request_id, &response)
}

fn agent_get_for_prompt(
    request_id: &str,
    target: &str,
    api_tx: &ApiRequestSender,
    total_timeout_ms: Option<u64>,
    started: std::time::Instant,
) -> Result<crate::api::schema::AgentInfo, ErrorResponse> {
    let request = Request {
        id: format!("{request_id}:agent"),
        method: Method::AgentGet(crate::api::schema::AgentTarget {
            target: target.to_string(),
        }),
    };
    let remaining_ms = remaining_timeout_ms(total_timeout_ms, started);
    let response = match remaining_ms {
        Some(timeout_ms) if timeout_ms <= APP_RESPONSE_TIMEOUT.as_millis() as u64 => {
            dispatch_to_app_with_caller_timeout(
                request,
                api_tx,
                Some(std::time::Duration::from_millis(timeout_ms)),
            )
        }
        _ => dispatch_to_app_with_timeout(request, api_tx, Some(APP_RESPONSE_TIMEOUT)),
    };
    agent_from_response(request_id, &response)
}

fn agent_from_response(
    request_id: &str,
    response: &str,
) -> Result<crate::api::schema::AgentInfo, ErrorResponse> {
    let value: serde_json::Value = serde_json::from_str(response).map_err(|_| ErrorResponse {
        id: request_id.into(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode agent response".into(),
        },
    })?;
    if value.get("error").is_some() {
        let error = serde_json::from_value(value["error"].clone()).map_err(|_| ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode agent error".into(),
            },
        })?;
        return Err(ErrorResponse {
            id: request_id.into(),
            error,
        });
    }
    serde_json::from_value(value["result"]["agent"].clone()).map_err(|_| ErrorResponse {
        id: request_id.into(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode agent result".into(),
        },
    })
}

fn agent_wait_success(
    request_id: String,
    agent: crate::api::schema::AgentInfo,
) -> std::io::Result<String> {
    serde_json::to_string(&SuccessResponse {
        id: request_id,
        result: ResponseResult::AgentInfo { agent },
    })
    .map_err(std::io::Error::other)
}

fn agent_wait_timeout(
    request_id: String,
    kind: AgentWaitTimeoutKind,
    current: &crate::api::schema::AgentInfo,
) -> std::io::Result<String> {
    let (code, message) = match kind {
        AgentWaitTimeoutKind::Status => {
            ("timeout", "timed out waiting for agent status".to_string())
        }
        AgentWaitTimeoutKind::AfterSubmitted => {
            let status = format!("{:?}", current.agent_status).to_ascii_lowercase();
            (
                "agent_status_unobserved_after_submit",
                format!(
                    "prompt submission was confirmed, but the requested agent status was not observed before the wait deadline; current status is {status}"
                ),
            )
        }
    };
    serde_json::to_string(&ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: code.into(),
            message,
        },
    })
    .map_err(std::io::Error::other)
}

fn agent_wait_not_running(request_id: String) -> std::io::Result<String> {
    serde_json::to_string(&ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "agent_not_running".into(),
            message: "agent is no longer running in the target pane".into(),
        },
    })
    .map_err(std::io::Error::other)
}

fn agent_wait_probe_error(response: ErrorResponse) -> std::io::Result<String> {
    if response.error.code == "agent_not_found" {
        return agent_wait_not_running(response.id);
    }
    serde_json::to_string(&response).map_err(std::io::Error::other)
}

pub(super) fn wait_for_event(
    request_id: String,
    params: EventsWaitParams,
    stream: &mut ApiStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let deadline = params
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));

    let subscription = match event_match_subscription(&request_id, params.match_event) {
        Ok(subscription) => subscription,
        Err(response) => return Ok(Some(serde_json::to_string(&response).unwrap())),
    };
    let mut cursor = event_hub.current_sequence();
    let mut active =
        match ActiveSubscription::new(subscription, &request_id, 0, api_tx, event_hub, cursor) {
            Ok(active) => active,
            Err(response) => return Ok(Some(serde_json::to_string(&response).unwrap())),
        };

    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }

        let batch = event_hub.read_after(cursor);
        cursor = batch.head;
        match active.poll_for_wait(api_tx, event_hub, &batch) {
            Ok(Some(event)) => return Ok(Some(wait_matched_response(&request_id, event))),
            Ok(None) => {}
            Err(mut response) if response.error.code == "pane_not_found" => {
                response.id = request_id;
                return serde_json::to_string(&response)
                    .map(Some)
                    .map_err(std::io::Error::other);
            }
            Err(_) => {}
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Ok(Some(
                serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "timeout".into(),
                        message: "timed out waiting for event match".into(),
                    },
                })
                .unwrap(),
            ));
        }

        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

fn event_match_subscription(
    request_id: &str,
    match_event: EventMatch,
) -> Result<Subscription, ErrorResponse> {
    match match_event {
        EventMatch::PaneAgentStatusChanged {
            pane_id,
            agent_status,
        } => Ok(Subscription::PaneAgentStatusChanged {
            pane_id: Some(pane_id),
            agent_status: Some(agent_status),
        }),
        _ => Err(ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "unsupported_event_wait_match".into(),
                message: "events.wait currently supports pane agent status matches".into(),
            },
        }),
    }
}

fn wait_matched_response(request_id: &str, event: serde_json::Value) -> String {
    let Ok(event) = serde_json::from_value::<SubscriptionEventEnvelope>(event) else {
        return serde_json::to_string(&ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode matched event".into(),
            },
        })
        .unwrap();
    };

    let SubscriptionEventData::PaneAgentStatusChanged(data) = event.data else {
        return serde_json::to_string(&ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "unsupported_event_wait_match".into(),
                message: "events.wait currently supports pane agent status matches".into(),
            },
        })
        .unwrap();
    };

    serde_json::to_string(&SuccessResponse {
        id: request_id.into(),
        result: ResponseResult::WaitMatched {
            event: EventEnvelope {
                event: EventKind::PaneAgentStatusChanged,
                data: EventData::PaneAgentStatusChanged {
                    pane_id: data.pane_id,
                    workspace_id: data.workspace_id,
                    agent_status: data.agent_status,
                    input_pending: data.input_pending,
                    input_prompt_kind: data.input_prompt_kind,
                    agent: data.agent,
                    title: data.title,
                    display_agent: data.display_agent,
                    state_labels: data.state_labels,
                    turn: data.turn,
                    turn_epoch: data.turn_epoch,
                },
            },
        },
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::LocalStream;
    use interprocess::local_socket::traits::Listener as _;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::mpsc;

    fn test_agent(
        status: crate::api::schema::AgentStatus,
        state_change_seq: u64,
    ) -> crate::api::schema::AgentInfo {
        crate::api::schema::AgentInfo {
            account: None,
            account_config_dir: None,
            account_unresolved: false,
            terminal_id: "term_1".into(),
            name: Some("reviewer".into()),
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: status,
            input_pending: false,
            input_prompt_kind: None,
            composer: Default::default(),
            screen_detection_skipped: false,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            last_completed_turn: None,
            turn: Some(1),
            turn_epoch: Some(9),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            pane_id: "pane_1".into(),
            focused: true,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq,
            status_since_unix_ms: None,
            completion_seq: None,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
            machine_id: None,
            machine_profile_id: None,
            machine_label: None,
            origin_machine_id: None,
            reachability: None,
            last_known_status: None,
            archived: None,
            parked_work: Vec::new(),
            session_transfer: None,
        }
    }

    fn with_composer(
        mut agent: crate::api::schema::AgentInfo,
        state: crate::api::schema::ComposerState,
        attempt_id: Option<&str>,
    ) -> crate::api::schema::AgentInfo {
        agent.composer = crate::api::schema::ComposerInfo {
            submit_abandoned: false,
            author: None,
            state,
            attempt_id: attempt_id.map(str::to_string),
            evidence: crate::api::schema::ComposerEvidence {
                provenance: if attempt_id.is_some() {
                    crate::api::schema::ComposerProvenance::AgentPrompt
                } else {
                    crate::api::schema::ComposerProvenance::None
                },
                region: match state {
                    crate::api::schema::ComposerState::Empty => {
                        crate::api::schema::ComposerRegionEvidence::Empty
                    }
                    crate::api::schema::ComposerState::DraftPresent => {
                        crate::api::schema::ComposerRegionEvidence::Text
                    }
                    crate::api::schema::ComposerState::Unknown => {
                        crate::api::schema::ComposerRegionEvidence::Unavailable
                    }
                },
                cursor: crate::api::schema::ComposerCursorEvidence::Unavailable,
                style: crate::api::schema::ComposerStyleEvidence::Unavailable,
                frame_stable: state != crate::api::schema::ComposerState::Unknown,
            },
        };
        agent
    }

    fn with_composer_region(
        agent: crate::api::schema::AgentInfo,
        state: crate::api::schema::ComposerState,
        attempt_id: Option<&str>,
        region: crate::api::schema::ComposerRegionEvidence,
    ) -> crate::api::schema::AgentInfo {
        let mut agent = with_composer(agent, state, attempt_id);
        agent.composer.evidence.region = region;
        agent.composer.evidence.frame_stable = true;
        agent
    }

    fn success_agent_response(
        id: String,
        agent: crate::api::schema::AgentInfo,
        prompted: bool,
    ) -> String {
        serde_json::to_string(&SuccessResponse {
            id,
            result: if prompted {
                ResponseResult::AgentPrompted {
                    agent,
                    delivery: Some(crate::api::schema::AgentPromptDelivery::WrittenToPty),
                }
            } else {
                ResponseResult::AgentInfo { agent }
            },
        })
        .expect("serialize agent response")
    }

    fn local_stream_pair() -> (LocalStream, LocalStream, PathBuf) {
        static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

        let file_name = format!(
            "hpw-{:x}-{:x}.sock",
            std::process::id(),
            NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed)
        );
        #[cfg(unix)]
        let path = PathBuf::from("/tmp").join(file_name);
        #[cfg(windows)]
        let path = std::env::temp_dir().join(file_name);
        #[cfg(unix)]
        assert!(
            path.as_os_str().as_encoded_bytes().len() < 104,
            "test socket path must fit macOS sockaddr_un.sun_path"
        );
        let listener = crate::ipc::bind_local_listener(&path).expect("bind local listener");
        let client = crate::ipc::connect_local_stream(&path).expect("connect local stream");
        let server = listener.accept().expect("accept local stream");
        (client, server, path)
    }

    struct PromptHarness {
        agents: VecDeque<crate::api::schema::AgentInfo>,
        prompted: crate::api::schema::AgentInfo,
        prompt_error: Option<ErrorBody>,
    }

    fn spawn_prompt_responder(
        mut harness: PromptHarness,
    ) -> (ApiRequestSender, std::thread::JoinHandle<()>) {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<crate::api::ApiRequestMessage>();
        let responder = std::thread::spawn(move || {
            while let Some(message) = api_rx.blocking_recv() {
                let id = message.request.id;
                let response = match message.request.method {
                    Method::AgentGet(_) => success_agent_response(
                        id,
                        harness
                            .agents
                            .pop_front()
                            .unwrap_or_else(|| harness.prompted.clone()),
                        false,
                    ),
                    Method::AgentPrompt(_) => match harness.prompt_error.clone() {
                        Some(error) => serde_json::to_string(&ErrorResponse { id, error })
                            .expect("serialize prompt error"),
                        None => success_agent_response(id, harness.prompted.clone(), true),
                    },
                    other => panic!("unexpected prompt observation request: {other:?}"),
                };
                message
                    .respond_to
                    .send(response)
                    .expect("prompt observer still receiving");
            }
        });
        (api_tx, responder)
    }

    fn run_prompt_harness(
        name: &str,
        text: &str,
        until: crate::api::schema::AgentStatus,
        effect_timeout_cap_ms: u64,
        harness: PromptHarness,
    ) -> serde_json::Value {
        let (api_tx, responder) = spawn_prompt_responder(harness);
        let (client, _server, path) = local_stream_pair();
        let mut client = ApiStream::Local(client);
        let response = prompt_agent_with_effect_timeout(
            name.into(),
            crate::api::schema::AgentPromptParams {
                target: "reviewer".into(),
                text: text.into(),
                wait: Some(crate::api::schema::AgentPromptWaitOptions {
                    until: vec![until],
                    timeout_ms: Some(10_000),
                    submission_deadline: None,
                }),
            },
            &mut client,
            &api_tx,
            &EventHub::default(),
            &Arc::new(AtomicBool::new(true)),
            effect_timeout_cap_ms,
        )
        .expect("prompt wait succeeds")
        .expect("connection remains active");
        drop(api_tx);
        responder.join().expect("prompt responder joins");
        drop(client);
        let _ = std::fs::remove_file(path);
        serde_json::from_str(&response).expect("decode prompt response")
    }

    #[test]
    fn prompt_observation_verdicts_preserve_the_evidence_boundaries() {
        assert_eq!(
            classify_prompt_observation(false, 10, 11, false, false, false),
            Some(PromptObservationVerdict::Submitted),
            "a new lifecycle sequence is submission evidence"
        );
        assert_eq!(
            classify_prompt_observation(true, 10, 11, true, false, false),
            Some(PromptObservationVerdict::Submitted),
            "an already-working pane needs same-attempt composer clearance"
        );
        assert_eq!(
            classify_prompt_observation(false, 10, 10, false, true, true),
            Some(PromptObservationVerdict::Unsubmitted),
            "a same-attempt draft remains a real error, even at the caller deadline"
        );
        assert_eq!(
            classify_prompt_observation(false, 10, 10, false, false, false),
            None,
            "an inconclusive write may still gather evidence"
        );
        assert_eq!(
            classify_prompt_observation(false, 10, 10, false, false, true),
            Some(PromptObservationVerdict::WrittenToPty),
            "observation expiry cannot erase an acknowledged PTY write"
        );
        assert_eq!(
            classify_prompt_observation(true, 10, 11, false, false, true),
            Some(PromptObservationVerdict::WrittenToPty),
            "already-working lifecycle changes cannot prove this submission"
        );
    }

    #[test]
    fn prompt_agent_reports_submitted_from_a_new_lifecycle_sequence() {
        let response = run_prompt_harness(
            "lifecycle-submitted",
            "review the diff",
            crate::api::schema::AgentStatus::Working,
            2_000,
            PromptHarness {
                agents: VecDeque::from([
                    test_agent(crate::api::schema::AgentStatus::Idle, 10),
                    test_agent(crate::api::schema::AgentStatus::Working, 11),
                ]),
                prompted: test_agent(crate::api::schema::AgentStatus::Idle, 10),
                prompt_error: None,
            },
        );

        assert_eq!(
            response["result"]["type"], "agent_prompted",
            "response: {response}"
        );
        assert_eq!(response["result"]["delivery"], "submitted");
    }

    #[test]
    fn prompt_agent_uses_attempt_provenance_for_bracketed_paste_without_rendered_token() {
        let prompted = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-paste"),
        );
        let response = run_prompt_harness(
            "paste-provenance",
            "multiline input whose rendered form is intentionally irrelevant",
            crate::api::schema::AgentStatus::Idle,
            0,
            PromptHarness {
                agents: VecDeque::from([test_agent(crate::api::schema::AgentStatus::Idle, 10)]),
                prompted,
                prompt_error: None,
            },
        );
        assert_eq!(response["error"]["code"], "agent_prompt_unsubmitted");
    }

    #[test]
    fn prompt_agent_accepts_only_the_same_attempt_clearing() {
        let prompted = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-clear"),
        );
        let cleared = with_composer_region(
            test_agent(crate::api::schema::AgentStatus::Working, 10),
            crate::api::schema::ComposerState::Unknown,
            Some("attempt-clear"),
            crate::api::schema::ComposerRegionEvidence::Empty,
        );
        let response = run_prompt_harness(
            "composer-submitted",
            "composer evidence",
            crate::api::schema::AgentStatus::Working,
            2_000,
            PromptHarness {
                agents: VecDeque::from([
                    test_agent(crate::api::schema::AgentStatus::Idle, 10),
                    cleared,
                ]),
                prompted,
                prompt_error: None,
            },
        );
        assert_eq!(response["result"]["delivery"], "submitted");
    }

    #[test]
    fn prompt_agent_accepts_fast_done_after_confirmed_submission() {
        let prompted = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-fast"),
        );
        let cleared = with_composer_region(
            test_agent(crate::api::schema::AgentStatus::Done, 11),
            crate::api::schema::ComposerState::Unknown,
            Some("attempt-fast"),
            crate::api::schema::ComposerRegionEvidence::Empty,
        );
        let response = run_prompt_harness(
            "fast-done",
            "quick turn",
            crate::api::schema::AgentStatus::Done,
            2_000,
            PromptHarness {
                agents: VecDeque::from([
                    test_agent(crate::api::schema::AgentStatus::Idle, 10),
                    cleared,
                ]),
                prompted,
                prompt_error: None,
            },
        );
        assert_eq!(response["result"]["delivery"], "submitted", "{response}");
        assert_eq!(response["result"]["agent"]["agent_status"], "done");
    }

    #[test]
    fn prompt_agent_reports_status_timeout_after_confirmed_submission() {
        let prompted = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-no-state"),
        );
        let cleared = with_composer_region(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::Unknown,
            Some("attempt-no-state"),
            crate::api::schema::ComposerRegionEvidence::Empty,
        );
        let response = run_prompt_harness(
            "submitted-status-unobserved",
            "still waiting for status",
            crate::api::schema::AgentStatus::Done,
            500,
            PromptHarness {
                agents: VecDeque::from([
                    test_agent(crate::api::schema::AgentStatus::Idle, 10),
                    cleared,
                ]),
                prompted,
                prompt_error: None,
            },
        );
        assert_eq!(
            response["error"]["code"], "agent_status_unobserved_after_submit",
            "{response}"
        );
    }

    #[test]
    fn prompt_agent_accepts_post_write_composer_clear_in_requested_idle() {
        let prompted = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-idle-clear"),
        );
        let cleared = with_composer_region(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::Unknown,
            Some("attempt-idle-clear"),
            crate::api::schema::ComposerRegionEvidence::Empty,
        );
        let response = run_prompt_harness(
            "idle-clear",
            "fast completed turn",
            crate::api::schema::AgentStatus::Idle,
            2_000,
            PromptHarness {
                agents: VecDeque::from([
                    test_agent(crate::api::schema::AgentStatus::Idle, 10),
                    cleared,
                ]),
                prompted,
                prompt_error: None,
            },
        );
        assert_eq!(response["result"]["delivery"], "submitted", "{response}");
        assert_eq!(response["result"]["agent"]["agent_status"], "idle");
    }

    #[test]
    fn prompt_agent_does_not_attribute_another_attempt_clearing_our_draft() {
        let prompted = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-ours"),
        );
        let other_cleared = with_composer_region(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::Unknown,
            Some("attempt-other"),
            crate::api::schema::ComposerRegionEvidence::Empty,
        );
        // Keep the other attempt's observation in the queue throughout the
        // deadline. The harness otherwise repeats `prompted` after exhausting
        // the queue, which would expose our original draft again and correctly
        // produce an unsubmitted verdict instead of testing cross-attribution.
        let window_ms = 3 * CONNECTION_POLL_INTERVAL.as_millis() as u64;
        let response = run_prompt_harness(
            "different-clear-attempt",
            "our delivery",
            crate::api::schema::AgentStatus::Idle,
            window_ms,
            PromptHarness {
                agents: std::iter::once(test_agent(crate::api::schema::AgentStatus::Idle, 10))
                    .chain(std::iter::repeat_n(other_cleared, 6))
                    .collect(),
                prompted,
                prompt_error: None,
            },
        );
        assert_eq!(
            response["result"]["delivery"], "written_to_pty",
            "{response}"
        );
    }

    #[test]
    fn prompt_agent_does_not_use_another_attempts_draft_to_clear_ours() {
        // Another attempt's draft must not count as evidence for this one.
        let prompted = with_composer_region(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::Unknown,
            Some("attempt-ours"),
            crate::api::schema::ComposerRegionEvidence::Empty,
        );
        let other = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-other"),
        );
        let ours_cleared = with_composer_region(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::Unknown,
            Some("attempt-ours"),
            crate::api::schema::ComposerRegionEvidence::Empty,
        );
        let response = run_prompt_harness(
            "different-draft-attempt",
            "our delivery",
            crate::api::schema::AgentStatus::Idle,
            250,
            PromptHarness {
                agents: VecDeque::from([
                    test_agent(crate::api::schema::AgentStatus::Idle, 10),
                    other,
                    ours_cleared,
                ]),
                prompted,
                prompt_error: None,
            },
        );
        assert_eq!(response["result"]["delivery"], "written_to_pty");
    }

    #[test]
    fn prompt_agent_reports_unsubmitted_without_leaking_prompt_text() {
        let text = "secret removal-sensitive prompt";
        let prompted = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::DraftPresent,
            Some("attempt-secret"),
        );
        let response = run_prompt_harness(
            "unsubmitted",
            text,
            crate::api::schema::AgentStatus::Idle,
            0,
            PromptHarness {
                agents: VecDeque::from([test_agent(crate::api::schema::AgentStatus::Idle, 10)]),
                prompted,
                prompt_error: None,
            },
        );

        assert_eq!(response["error"]["code"], "agent_prompt_unsubmitted");
        assert!(
            !response.to_string().contains(text),
            "prompt text must not leak into observation errors"
        );
    }

    /// An observable but inconclusive composer must retain the PTY receipt.
    #[test]
    fn prompt_agent_reports_written_to_pty_when_observation_stalls() {
        let observable = with_composer(
            test_agent(crate::api::schema::AgentStatus::Idle, 10),
            crate::api::schema::ComposerState::Empty,
            None,
        );
        let response = run_prompt_harness(
            "stalled",
            "review the diff",
            crate::api::schema::AgentStatus::Idle,
            0,
            PromptHarness {
                agents: VecDeque::from([observable.clone()]),
                prompted: observable,
                prompt_error: None,
            },
        );

        assert_eq!(response["result"]["type"], "agent_prompted");
        assert_eq!(response["result"]["delivery"], "written_to_pty");
    }

    /// Missing composer coverage cannot turn a completed PTY write into an
    /// API error, even when the caller requested a status wait.
    #[test]
    fn prompt_agent_reports_written_to_pty_without_composer_observation() {
        let unobservable = test_agent(crate::api::schema::AgentStatus::Idle, 10);
        assert_eq!(
            unobservable.composer.evidence.region,
            crate::api::schema::ComposerRegionEvidence::Unavailable,
            "fixture must have no composer to observe, or this pins nothing"
        );
        let response = run_prompt_harness(
            "unverifiable",
            "review the diff",
            crate::api::schema::AgentStatus::Idle,
            0,
            PromptHarness {
                agents: VecDeque::from([unobservable.clone()]),
                prompted: unobservable,
                prompt_error: None,
            },
        );

        assert_eq!(response["result"]["type"], "agent_prompted");
        assert_eq!(response["result"]["delivery"], "written_to_pty");
    }

    #[test]
    fn prompt_agent_does_not_attribute_an_already_working_sequence_advance() {
        let response = run_prompt_harness(
            "already-working",
            "follow-up prompt",
            crate::api::schema::AgentStatus::Working,
            0,
            PromptHarness {
                agents: VecDeque::from([test_agent(crate::api::schema::AgentStatus::Working, 10)]),
                prompted: test_agent(crate::api::schema::AgentStatus::Working, 11),
                prompt_error: None,
            },
        );

        assert_eq!(response["result"]["type"], "agent_prompted");
        assert_eq!(response["result"]["delivery"], "written_to_pty");
    }

    #[test]
    fn prompt_agent_preserves_the_not_received_verdict() {
        let response = run_prompt_harness(
            "write-failure",
            "review the diff",
            crate::api::schema::AgentStatus::Idle,
            0,
            PromptHarness {
                agents: VecDeque::from([test_agent(crate::api::schema::AgentStatus::Idle, 10)]),
                prompted: test_agent(crate::api::schema::AgentStatus::Idle, 10),
                prompt_error: Some(ErrorBody {
                    code: "agent_prompt_not_received".into(),
                    message: "agent prompt was not fully written to the pane PTY".into(),
                }),
            },
        );

        assert_eq!(response["error"]["code"], "agent_prompt_not_received");
    }

    #[test]
    fn agent_wait_probe_only_translates_agent_disappearance() {
        let disappeared = agent_wait_probe_error(ErrorResponse {
            id: "wait".into(),
            error: ErrorBody {
                code: "agent_not_found".into(),
                message: "missing".into(),
            },
        })
        .unwrap();
        let disappeared: ErrorResponse = serde_json::from_str(&disappeared).unwrap();
        assert_eq!(disappeared.id, "wait");
        assert_eq!(disappeared.error.code, "agent_not_running");

        let unavailable = agent_wait_probe_error(ErrorResponse {
            id: "wait".into(),
            error: ErrorBody {
                code: "server_unavailable".into(),
                message: "timed out waiting for app response".into(),
            },
        })
        .unwrap();
        let unavailable: ErrorResponse = serde_json::from_str(&unavailable).unwrap();
        assert_eq!(unavailable.id, "wait");
        assert_eq!(unavailable.error.code, "server_unavailable");
    }
}
