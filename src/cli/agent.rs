use std::time::{Duration, Instant};

use crate::api::schema::{
    AgentArchiveParams, AgentForgetParams, AgentListParams, AgentPromptParams, AgentReadParams,
    AgentRenameParams, AgentRestartParams, AgentSendKeysParams, AgentStartParams, AgentTarget,
    AgentTransferSessionParams, AgentUnarchiveParams, AgentWaitParams, ErrorBody, ErrorResponse,
    Method, PaneProcessInfoParams, PaneTarget, Request,
};

const AGENT_START_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PANE_SHELL_READINESS_RETRY_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) mod bindings;
#[cfg(test)]
mod bindings_tests;

pub(crate) fn command() -> clap::Command {
    bindings::command()
}

pub(super) fn run_agent_command(args: &[String]) -> std::io::Result<i32> {
    use super::projection::ParseError;
    use bindings::Operation;

    // `explain` and `wait` historically treat a bare `help` operand after the target as help.
    let help_args;
    let args = match args.first().map(String::as_str) {
        Some(leaf @ ("explain" | "wait")) => {
            let operands = super::projection::operands(&command(), args);
            let skip = usize::from(leaf == "wait");
            if operands.iter().skip(skip).any(|operand| *operand == "help") {
                help_args = [leaf.to_owned(), "--help".to_owned()];
                &help_args[..]
            } else {
                args
            }
        }
        _ => args,
    };
    let operation = match super::projection::parse(
        command().bin_name("herdr agent"),
        args,
        bindings::from_matches,
    ) {
        Ok(operation) => operation,
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
    };
    match operation {
        Operation::List => agent_list(),
        Operation::FederatedList(params) => federated_agent_command(
            params.caller_pane,
            crate::api::reverse_agents::ReverseMethod::AgentList,
        ),
        Operation::FederatedPrompt(params) => federated_agent_command(
            params.caller_pane,
            crate::api::reverse_agents::ReverseMethod::AgentPrompt {
                target: params.target,
                text: params.text,
            },
        ),
        Operation::Get(params) => agent_get(params),
        Operation::Focus(params) => agent_focus(params),
        Operation::Read(params) => agent_read(params),
        Operation::SendKeys(params) => agent_send_keys(params),
        Operation::Rename(params) => agent_rename(params),
        Operation::Archive(params) => agent_archive(params),
        Operation::Unarchive(params) => agent_unarchive(params),
        Operation::Forget(params) => agent_forget(params),
        Operation::Wait(params) => agent_wait(params),
        Operation::Prompt(params) => agent_prompt(params),
        Operation::Start(params) => agent_start(params),
        Operation::Restart(params) => agent_restart(params),
        Operation::TransferSession(params) => agent_transfer_session(params),
        Operation::Attach(params) => agent_attach(params),
        Operation::Explain(params) => agent_explain(params),
    }
}

fn agent_explain(params: bindings::Explain) -> std::io::Result<i32> {
    let bindings::Explain {
        file,
        agent,
        json,
        verbose,
        target,
    } = params;

    let explain = if let Some(path) = file {
        if target.is_some() {
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        }
        let Some(agent_label) = agent else {
            eprintln!("herdr agent explain --file requires --agent LABEL");
            return Ok(2);
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                let response = ErrorResponse {
                    id: "cli:agent:explain".into(),
                    error: ErrorBody {
                        code: "agent_explain_file_read_failed".into(),
                        message: format!("failed to read agent explain file {path}: {err}"),
                    },
                };
                let response = serde_json::to_string(&response).map_err(std::io::Error::other)?;
                eprintln!("{response}");
                return Ok(1);
            }
        };
        crate::detect::manifest::explain_to_json_value(&crate::detect::manifest::explain_for_label(
            &agent_label,
            &content,
        ))
    } else {
        let Some(target) = target else {
            eprintln!("usage: herdr agent explain <target> [--json]");
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        };
        if agent.is_some() {
            eprintln!("--agent is only valid with --file");
            return Ok(2);
        }

        let response = super::send_request(&Request {
            id: "cli:agent:explain".into(),
            method: Method::AgentExplain(AgentTarget {
                target: target.to_owned(),
            }),
        })?;
        if response.get("error").is_some() {
            eprintln!("{}", serde_json::to_string(&response).unwrap());
            return Ok(1);
        }
        response["result"]["explain"].clone()
    };

    if json {
        println!("{explain}");
    } else {
        print_agent_explain_text(&explain, verbose);
    }
    Ok(0)
}

fn print_agent_explain_text(explain: &serde_json::Value, verbose: bool) {
    println!("agent: {}", explain["agent"].as_str().unwrap_or("unknown"));
    println!("state: {}", explain["state"].as_str().unwrap_or("unknown"));
    println!(
        "manifest: {} {}",
        explain["manifest_source"].as_str().unwrap_or("none"),
        explain["manifest_version"].as_str().unwrap_or("unknown")
    );
    if let Some(rule) = explain["matched_rule"].as_object() {
        let rule_id = rule
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        println!(
            "rule: {} (region={} priority={})",
            rule_id,
            rule.get("region")
                .and_then(|value| value.as_str())
                .unwrap_or("-"),
            rule.get("priority")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        if let Some(preview) = matched_rule_region_preview(explain, rule_id) {
            println!("evidence: {preview:?}");
        }
    } else {
        println!("rule: none");
    }
    if let Some(reason) = explain["fallback_reason"].as_str() {
        println!("fallback_reason: {reason}");
    }
    if let Some(reason) = explain["screen_detection_skip_reason"].as_str() {
        println!("screen_detection_skip_reason: {reason}");
    }
    if let Some(reason) = explain["skipped_update_reason"].as_str() {
        println!("skipped_update_reason: {reason}");
    }
    if let Some(warning) = explain["warning"].as_str() {
        println!("warning: {warning}");
    }

    if !verbose {
        return;
    }

    println!(
        "visible: idle={} blocker={} working={}",
        explain["visible_idle"].as_bool().unwrap_or(false),
        explain["visible_blocker"].as_bool().unwrap_or(false),
        explain["visible_working"].as_bool().unwrap_or(false)
    );
    println!(
        "cached_remote_version: {}",
        explain["cached_remote_version"].as_str().unwrap_or("none")
    );
    println!(
        "local_override_shadowing_remote: {}",
        explain["local_override_shadowing_remote"]
            .as_bool()
            .unwrap_or(false)
    );
    if let Some(status) = explain["remote_update_status"].as_str() {
        println!("remote_update_status: {status}");
    }
    if let Some(error) = explain["remote_update_error"].as_str() {
        println!("remote_update_error: {error}");
    }
    if let Some(evaluated_rules) = explain["evaluated_rules"]
        .as_array()
        .filter(|rules| !rules.is_empty())
    {
        println!("evaluated_rules:");
        for rule in evaluated_rules {
            println!(
                "  {} {} priority={} region={} state={}",
                if rule["matched"].as_bool().unwrap_or(false) {
                    "✓"
                } else {
                    "✗"
                },
                rule["id"].as_str().unwrap_or("-"),
                rule["priority"].as_i64().unwrap_or(0),
                rule["region"].as_str().unwrap_or("-"),
                rule["state"].as_str().unwrap_or("unknown")
            );
            let evidence = &rule["evidence"];
            println!(
                "    matchers: contains={:?} regex={:?} line_regex={:?} all={} any={} not={}",
                evidence["contains"],
                evidence["regex"],
                evidence["line_regex"],
                evidence["all_count"].as_u64().unwrap_or(0),
                evidence["any_count"].as_u64().unwrap_or(0),
                evidence["not_count"].as_u64().unwrap_or(0)
            );
            println!(
                "    region: bytes={} preview={:?}",
                evidence["region_bytes"].as_u64().unwrap_or(0),
                evidence["region_preview"].as_str().unwrap_or("")
            );
        }
    }
}

fn matched_rule_region_preview<'a>(
    explain: &'a serde_json::Value,
    rule_id: &str,
) -> Option<&'a str> {
    explain["evaluated_rules"]
        .as_array()?
        .iter()
        .find(|rule| rule["id"].as_str() == Some(rule_id))?["evidence"]["region_preview"]
        .as_str()
        .filter(|preview| !preview.is_empty())
}

fn agent_start(params: AgentStartParams) -> std::io::Result<i32> {
    let AgentStartParams {
        name,
        kind,
        pane_id,
        args: agent_args,
        timeout_ms,
        account,
    } = params;
    let Some(expected_kind) = crate::detect::parse_agent_label(&kind) else {
        eprintln!("unsupported interactive agent kind: {kind}");
        return Ok(2);
    };
    let expected_kind = crate::detect::agent_label(expected_kind).to_string();
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));
    let retryable_timeout = timeout > crate::app::AGENT_START_SETTLE_DELAY
        && timeout <= crate::app::MAX_AGENT_START_TIMEOUT;
    let pinned_terminal_id = pane_terminal_id(&pane_id)?;
    let mut retry_deadline = None;
    let mut previous_busy_response = None;
    let mut response = loop {
        if let Some(previous_busy_response) = previous_busy_response.as_ref() {
            let retry_expired = retry_deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if retry_expired
                || pane_terminal_id(&pane_id)? != pinned_terminal_id
                || !pane_shell_is_initializing(&pane_id)?
            {
                return super::print_response(previous_busy_response);
            }
        }

        let response = super::send_request(&Request {
            id: "cli:agent:start".into(),
            method: Method::AgentStart(AgentStartParams {
                name: name.clone(),
                kind: kind.clone(),
                pane_id: pane_id.clone(),
                args: agent_args.clone(),
                timeout_ms,
                account: account.clone(),
            }),
        })?;
        if response.get("error").is_none() {
            break response;
        }
        if response["error"]["code"].as_str() != Some("agent_pane_busy")
            || !retryable_timeout
            || pinned_terminal_id.is_none()
            || pane_terminal_id(&pane_id)? != pinned_terminal_id
            || !pane_shell_is_initializing(&pane_id)?
        {
            return super::print_response(&response);
        }

        let deadline = *retry_deadline
            .get_or_insert_with(|| Instant::now() + PANE_SHELL_READINESS_RETRY_TIMEOUT);
        previous_busy_response = Some(response);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if let Some(previous_busy_response) = previous_busy_response.as_ref() {
                return super::print_response(previous_busy_response);
            }
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL.min(remaining));
    };

    let Some(expected_terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        return super::print_response(&cli_agent_error(
            "cli:agent:start",
            "agent_start_failed",
            "agent start response did not include terminal_id",
        ));
    };
    if pinned_terminal_id
        .as_deref()
        .is_some_and(|pinned| pinned != expected_terminal_id)
    {
        return super::print_response(&agent_name_lost_error("cli:agent:start", &name));
    }
    let waited = wait_for_named_agent(
        &name,
        &pane_id,
        timeout,
        &expected_kind,
        expected_terminal_id,
    );
    match waited {
        Ok(Ok(agent)) => {
            response["result"]["agent"] = agent;
            super::print_response(&response)
        }
        Ok(Err(error)) => super::print_response(&error),
        Err(err) => {
            print_agent_transport_error(err, "cli:agent:start", "agent_start_transport_failed")
        }
    }
}

fn agent_list() -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:list".into(),
        method: Method::AgentList(AgentListParams::default()),
    })?)
}

/// Explicit trusted-machine reverse request. The pane id is supplied by the
/// caller, not OS-attested; a malicious same-user process can spoof it.
fn federated_agent_command(
    caller_pane: Option<String>,
    method: crate::api::reverse_agents::ReverseMethod,
) -> std::io::Result<i32> {
    let coordinator = crate::config::Config::load()
        .config
        .federation
        .reverse_coordinator_machine_id;
    let Some(coordinator) = coordinator else {
        eprintln!("reverse federation disabled: set federation.reverse_coordinator_machine_id to the trusted coordinator's pinned install id");
        return Ok(1);
    };
    let caller = caller_pane.or_else(|| std::env::var("HERDR_PANE_ID").ok());
    let Some(caller) = caller.filter(|caller| !caller.is_empty()) else {
        eprintln!("HERDR_PANE_ID (or --caller-pane) is required; same-user processes can spoof opted-in pane identities");
        return Ok(2);
    };
    let roster = match crate::api::client::ApiClient::local().request(Request {
        id: "cli:agent:federated:caller".into(),
        method: Method::AgentList(AgentListParams { local_only: true }),
    }) {
        Ok(roster) => roster,
        Err(error) => {
            eprintln!("cannot verify live local caller: {error}");
            return Ok(1);
        }
    };
    let crate::api::schema::ResponseResult::AgentList { agents, .. } = roster.result else {
        eprintln!("local agent roster unavailable");
        return Ok(1);
    };
    let mut matches = agents.into_iter().filter(|agent| {
        (agent.pane_id == caller || agent.terminal_id == caller)
            && agent.machine_id.is_none()
            && agent.archived.is_none()
    });
    let Some(agent) = matches.next() else {
        eprintln!("caller is not a live local agent pane or terminal");
        return Ok(1);
    };
    if matches.next().is_some() {
        eprintln!("caller identity is ambiguous");
        return Ok(1);
    }
    let caller = agent.terminal_id;
    match crate::api::reverse_agents::request_remote(&coordinator, caller, method) {
        Ok(response) => super::print_response(&response),
        Err(error) => {
            eprintln!("reverse federation failed: {error}");
            Ok(1)
        }
    }
}

fn agent_get(params: AgentTarget) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:get".into(),
        method: Method::AgentGet(params),
    })?)
}

fn agent_focus(params: AgentTarget) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:focus".into(),
        method: Method::AgentFocus(params),
    })?)
}

fn agent_restart(params: AgentRestartParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:restart".into(),
        method: Method::AgentRestart(params),
    })?)
}

fn agent_transfer_session(parsed: bindings::TransferSession) -> std::io::Result<i32> {
    if let Some(transfer_id) = parsed.confirm {
        let response = super::send_request(&Request {
            id: "cli:agent:transfer-session:confirm".into(),
            method: Method::AgentTransferSession(AgentTransferSessionParams {
                target: parsed.target.clone(),
                to: parsed.to,
                account: parsed.account,
                transfer_id: Some(transfer_id),
                confirm: true,
            }),
        })?;
        return wait_for_session_transfer_outcome(&parsed.target, response);
    }

    let mut response = super::send_request(&Request {
        id: "cli:agent:transfer-session:prepare".into(),
        method: Method::AgentTransferSession(AgentTransferSessionParams {
            target: parsed.target.clone(),
            to: parsed.to,
            account: parsed.account.clone(),
            transfer_id: None,
            confirm: false,
        }),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    let poll_target = session_transfer_poll_target(&response, &parsed.target);

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let transfer = response
            .pointer("/result/agent/session_transfer")
            .and_then(serde_json::Value::as_object);
        let phase = transfer
            .and_then(|transfer| transfer.get("phase"))
            .and_then(serde_json::Value::as_str);
        match phase {
            Some("ready") => {
                let message_count = transfer
                    .and_then(|transfer| transfer.get("message_count"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let transfer_id = transfer
                    .and_then(|transfer| transfer.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                // History left behind, if the transcript was larger than the
                // transfer window. Reported separately from the record-class
                // omissions because it is the only one that costs the user
                // something they might not accept.
                let dropped = windowed_records_of(transfer);
                match staged_transfer_decision(parsed.yes, dropped) {
                    StagedDecision::ReportAndWait { lossy } => {
                        eprintln!(
                            "staged {message_count} visible messages; source is still running. Re-run with --confirm <transfer-id> to cut over"
                        );
                        if lossy {
                            eprintln!(
                                "LOSSY: the transcript exceeded the transfer window, so the {dropped} OLDEST records are NOT being transferred. Only the most recent history moves."
                            );
                        }
                        return super::print_response(&response);
                    }
                    StagedDecision::RefuseLossyYes => {
                        eprintln!(
                            "refusing --yes: this transfer would drop the {dropped} oldest records (the transcript exceeds the transfer window). Re-run without --yes to see what is lost, then --confirm <transfer-id> to accept it."
                        );
                        return super::print_response(&response).map(|_| 1);
                    }
                    StagedDecision::Confirm => {}
                }
                let Some(transfer_id) = transfer_id else {
                    eprintln!("session transfer became ready without a transfer id");
                    return Ok(1);
                };
                let response = super::send_request(&Request {
                    id: "cli:agent:transfer-session:confirm".into(),
                    method: Method::AgentTransferSession(AgentTransferSessionParams {
                        target: parsed.target.clone(),
                        to: parsed.to,
                        account: parsed.account,
                        transfer_id: Some(transfer_id),
                        confirm: true,
                    }),
                })?;
                return wait_for_session_transfer_outcome(&poll_target, response);
            }
            Some("failed" | "rolled_back") => {
                println!("{response}");
                return Ok(1);
            }
            Some("completed") => return super::print_response(&response),
            _ => {}
        }
        if Instant::now() >= deadline {
            eprintln!("timed out waiting for the staged session transfer");
            println!("{response}");
            return Ok(1);
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL);
        response = super::send_request(&Request {
            id: "cli:agent:transfer-session:poll".into(),
            method: Method::AgentGet(AgentTarget {
                target: poll_target.clone(),
            }),
        })?;
        if response.get("error").is_some() && !session_transfer_poll_may_retry(&response) {
            return super::print_response(&response);
        }
    }
}

fn session_transfer_poll_target(response: &serde_json::Value, fallback: &str) -> String {
    response
        .pointer("/result/agent/pane_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

/// What to do with a staged transfer, given `--yes` and how much history it drops.
///
/// EXTRACTED SO THE REFUSAL IS TESTABLE. The previous test exercised only the field
/// lookup, so deleting the lossy `--yes` refusal, inverting it, or dropping the warning
/// all left it green — review round 3, and the fourth test in this PR that could not
/// reach what it claimed to protect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedDecision {
    /// Print the staging summary and stop, so a person decides. `lossy` adds the
    /// warning naming what is being left behind.
    ReportAndWait { lossy: bool },
    /// `--yes` was passed but the transfer would discard history. Refuse: the flag
    /// exists to skip a confirmation nobody needed to read, and this is one they do.
    RefuseLossyYes,
    /// `--yes` on a lossless transfer — confirm immediately, as asked.
    Confirm,
}

fn staged_transfer_decision(yes: bool, dropped_records: u64) -> StagedDecision {
    match (yes, dropped_records > 0) {
        (false, lossy) => StagedDecision::ReportAndWait { lossy },
        (true, true) => StagedDecision::RefuseLossyYes,
        (true, false) => StagedDecision::Confirm,
    }
}

/// How many records a staged transfer is leaving behind.
///
/// Extracted so the FIELD PATH is testable. A typo anywhere in it makes this
/// silently return 0, which reads as "nothing was dropped" — so `--yes` would wave
/// a lossy transfer through and staging would print no warning, with nothing
/// anywhere reporting a fault. The failure is invisible in exactly the direction
/// that costs the user history.
fn windowed_records_of(transfer: Option<&serde_json::Map<String, serde_json::Value>>) -> u64 {
    transfer
        .and_then(|transfer| transfer.get("omissions"))
        .and_then(|omissions| omissions.get("windowed_records"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

fn wait_for_session_transfer_outcome(
    target: &str,
    mut response: serde_json::Value,
) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    let target = session_transfer_poll_target(&response, target);
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let phase = response
            .pointer("/result/agent/session_transfer/phase")
            .and_then(serde_json::Value::as_str);
        match phase {
            Some("completed") => return super::print_response(&response),
            Some("failed" | "rolled_back") => {
                println!("{response}");
                return Ok(1);
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            eprintln!("timed out waiting for the target harness to verify its session");
            println!("{response}");
            return Ok(1);
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL);
        response = super::send_request(&Request {
            id: "cli:agent:transfer-session:cutover-poll".into(),
            method: Method::AgentGet(AgentTarget {
                target: target.clone(),
            }),
        })?;
        if response.get("error").is_some() && !session_transfer_poll_may_retry(&response) {
            return super::print_response(&response);
        }
    }
}

fn session_transfer_poll_may_retry(response: &serde_json::Value) -> bool {
    response
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)
        == Some("agent_not_found")
}

fn agent_attach(params: bindings::Attach) -> std::io::Result<i32> {
    let bindings::Attach { target, takeover } = params;

    let response = resolve_agent_target(&target, "cli:agent:attach:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    let Some(terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        eprintln!("agent attach failed: response did not include terminal_id");
        return Ok(1);
    };
    crate::client::run_terminal_attach(terminal_id.to_owned(), takeover)?;
    Ok(0)
}

fn agent_wait(params: AgentWaitParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:wait".into(),
        method: Method::AgentWait(params),
    })?)
}

fn wait_for_named_agent(
    name: &str,
    fallback_pane_id: &str,
    timeout: Duration,
    expected_kind: &str,
    expected_terminal_id: &str,
) -> std::io::Result<Result<serde_json::Value, serde_json::Value>> {
    let deadline = Instant::now().checked_add(timeout);
    let mut first_poll = true;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            // Let the server reconcile its matching startup deadline before
            // returning so the pending name is immediately reusable.
            let _ = resolve_agent_target_unchecked(name, "cli:agent:start:timeout");
            return Ok(Err(agent_wait_timeout()));
        }
        let poll_id = "cli:agent:start";
        let mut response = if first_poll {
            first_poll = false;
            resolve_agent_target(name, poll_id)?
        } else {
            resolve_agent_target_unchecked(name, poll_id)?
        };
        if response.get("error").is_some() {
            response = resolve_agent_target_unchecked(fallback_pane_id, poll_id)?;
            if response.get("error").is_some() {
                std::thread::sleep(AGENT_START_POLL_INTERVAL);
                continue;
            }
        }
        let agent = &response["result"]["agent"];
        let outcome = if agent["terminal_id"].as_str() != Some(expected_terminal_id) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else if let Some(actual) = agent["agent"]
            .as_str()
            .filter(|actual| *actual != expected_kind)
        {
            Some(Err(cli_agent_error(
                "cli:agent:start",
                "agent_kind_mismatch",
                format!("expected {expected_kind}, detected {actual}"),
            )))
        } else if agent["name"].as_str() != Some(name) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else {
            match agent["agent_status"].as_str() {
                Some("blocked") => Some(Err(cli_agent_error(
                    "cli:agent:start",
                    "agent_not_ready",
                    format!("agent {name} is blocked during startup and is not ready for prompts"),
                ))),
                Some("unknown")
                    if expected_kind == "codex"
                        && agent["interactive_ready"].as_bool() == Some(true) =>
                {
                    Some(Ok(agent.clone()))
                }
                Some("working" | "unknown") => None,
                Some("idle" | "done") if agent["interactive_ready"].as_bool() == Some(true) => {
                    Some(Ok(agent.clone()))
                }
                Some("idle" | "done") if !agent["launch_pending"].as_bool().unwrap_or(false) => {
                    Some(Err(cli_agent_error(
                        "cli:agent:start",
                        "agent_start_failed",
                        "agent process exited before becoming interactive",
                    )))
                }
                _ => None,
            }
        };
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL);
    }
}

fn pane_terminal_id(pane_id: &str) -> std::io::Result<Option<String>> {
    let response = super::send_request(&Request {
        id: "cli:agent:start:pane".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_owned(),
        }),
    })?;
    Ok(response["result"]["pane"]["terminal_id"]
        .as_str()
        .map(str::to_owned))
}

fn pane_shell_is_initializing(pane_id: &str) -> std::io::Result<bool> {
    let response = super::send_request(&Request {
        id: "cli:agent:start:process_info".into(),
        method: Method::PaneProcessInfo(PaneProcessInfoParams {
            pane_id: Some(pane_id.to_owned()),
        }),
    })?;
    Ok(process_info_shows_shell_initialization(
        &response["result"]["process_info"],
    ))
}

#[cfg(unix)]
fn process_info_shows_shell_initialization(process_info: &serde_json::Value) -> bool {
    let Some(shell_pid) = process_info["shell_pid"].as_u64() else {
        return false;
    };
    if process_info["foreground_process_group_id"].as_u64() != Some(shell_pid) {
        return false;
    }
    process_info["foreground_processes"]
        .as_array()
        .is_some_and(|processes| {
            processes.iter().any(|process| {
                process["pid"].as_u64() == Some(shell_pid)
                    && (process["name"]
                        .as_str()
                        .is_some_and(crate::platform::is_pane_shell_process_name)
                        || process["argv"]
                            .as_array()
                            .and_then(|argv| argv.first())
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(crate::platform::is_pane_shell_process_name))
            })
        })
}

// Windows exposes no foreground process group, so shell initialization is not
// observable and a busy `agent.start` is not retried there.
#[cfg(not(unix))]
fn process_info_shows_shell_initialization(_process_info: &serde_json::Value) -> bool {
    false
}

fn agent_name_lost_error(request_id: &str, expected_name: &str) -> serde_json::Value {
    cli_agent_error(
        request_id,
        "agent_name_not_found",
        format!("named agent {expected_name} no longer owns the target terminal"),
    )
}

fn print_agent_transport_error(
    err: std::io::Error,
    request_id: &str,
    code: &str,
) -> std::io::Result<i32> {
    if super::protocol_mismatch_was_reported(&err) {
        return Ok(1);
    }
    // A dead-server marker reaches here from `send_request` in the agent
    // startup path; surface its deferred response exactly once instead of
    // printing a second, generic transport-error line.
    if let Some(response) = super::server_not_running_reported_response(&err) {
        let value = serde_json::to_value(response).map_err(std::io::Error::other)?;
        return super::print_response(&value);
    }
    super::print_response(&cli_agent_error(request_id, code, err.to_string()))
}

fn agent_wait_timeout() -> serde_json::Value {
    cli_agent_error(
        "cli:agent:start",
        "timeout",
        "timed out waiting for agent startup",
    )
}

fn cli_agent_error(id: &str, code: &str, message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn resolve_agent_target(target: &str, request_id: &str) -> std::io::Result<serde_json::Value> {
    super::send_request(&agent_get_request(target, request_id))
}

fn resolve_agent_target_unchecked(
    target: &str,
    request_id: &str,
) -> std::io::Result<serde_json::Value> {
    super::send_request_unchecked(&agent_get_request(target, request_id))
}

fn agent_get_request(target: &str, request_id: &str) -> Request {
    Request {
        id: request_id.into(),
        method: Method::AgentGet(AgentTarget {
            target: target.to_owned(),
        }),
    }
}

fn agent_rename(params: AgentRenameParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:rename".into(),
        method: Method::AgentRename(params),
    })?)
}

fn agent_archive(params: AgentArchiveParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:archive".into(),
        method: Method::AgentArchive(params),
    })?)
}

fn agent_unarchive(params: AgentUnarchiveParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:unarchive".into(),
        method: Method::AgentUnarchive(params),
    })?)
}

fn agent_forget(params: AgentForgetParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:forget".into(),
        method: Method::AgentForget(params),
    })?)
}

fn agent_prompt(params: AgentPromptParams) -> std::io::Result<i32> {
    let response = super::send_request(&Request {
        id: "cli:agent:prompt".into(),
        method: Method::AgentPrompt(params),
    })?;
    super::print_response(&response)
}

fn agent_send_keys(params: AgentSendKeysParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:send-keys".into(),
        method: Method::AgentSendKeys(params),
    })?)
}

fn agent_read(params: AgentReadParams) -> std::io::Result<i32> {
    let response = super::send_request(&Request {
        id: "cli:agent:read".into(),
        method: Method::AgentRead(params),
    })?;
    super::print_read_response(&response)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AXIS — ROUND-3 FINDING 4: BOTH arms of the lossy `--yes` refusal.
    ///
    /// The previous test called only `windowed_records_of`, so deleting the refusal,
    /// inverting it, or dropping the warning all left it green. This asserts the
    /// decision itself, including the lossless arm — without which "refuses" could
    /// quietly mean "--yes is broken for everyone".
    #[test]
    fn a_lossy_yes_is_refused_and_a_lossless_one_confirms() {
        // --yes on a transfer that discards history: refuse, do not confirm.
        assert_eq!(
            staged_transfer_decision(true, 874),
            StagedDecision::RefuseLossyYes
        );
        // --yes on a lossless transfer: confirm, as asked. THE MIRROR.
        assert_eq!(staged_transfer_decision(true, 0), StagedDecision::Confirm);
        // Without --yes: always report and wait, with the warning only when lossy.
        assert_eq!(
            staged_transfer_decision(false, 874),
            StagedDecision::ReportAndWait { lossy: true }
        );
        assert_eq!(
            staged_transfer_decision(false, 0),
            StagedDecision::ReportAndWait { lossy: false }
        );
    }

    /// AXIS: the field path reaches the real value, and a shape that lacks it
    /// reads as zero rather than as a wrong number.
    ///
    /// This guards the silent direction: a mistyped path returns 0, which means
    /// "nothing dropped" — staging would print no warning and `--yes` would wave a
    /// lossy transfer through, with no error anywhere. The fixture mirrors the
    /// live `agent.list` shape, taken from a real staged transfer.
    #[test]
    fn windowed_records_are_read_from_the_staged_transfer() {
        let staged: serde_json::Value = serde_json::from_str(
            r#"{"id":"t-1","phase":"ready","source":"claude","target":"codex",
                "message_count":12,
                "omissions":{"tool_records":3,"reasoning_records":0,"system_records":0,
                             "attachment_records":0,"metadata_records":0,
                             "unsupported_blocks":0,"sidechain_records":0,
                             "windowed_records":874}}"#,
        )
        .unwrap();
        assert_eq!(windowed_records_of(staged.as_object()), 874);

        // A transfer that dropped nothing, and one from a build without the field:
        // both must read 0 — the value that means "not lossy".
        let clean: serde_json::Value =
            serde_json::from_str(r#"{"omissions":{"windowed_records":0}}"#).unwrap();
        assert_eq!(windowed_records_of(clean.as_object()), 0);
        let legacy: serde_json::Value = serde_json::from_str(r#"{"omissions":{}}"#).unwrap();
        assert_eq!(windowed_records_of(legacy.as_object()), 0);
        assert_eq!(windowed_records_of(None), 0);
    }

    #[test]
    fn transfer_session_polling_prefers_the_stable_pane_id() {
        let response = serde_json::json!({
            "result": {
                "agent": {
                    "pane_id": "w2:p7",
                    "name": "jarvis"
                }
            }
        });

        assert_eq!(session_transfer_poll_target(&response, "jarvis"), "w2:p7");
        assert_eq!(
            session_transfer_poll_target(&serde_json::json!({}), "jarvis"),
            "jarvis"
        );
        assert!(session_transfer_poll_may_retry(&serde_json::json!({
            "error": { "code": "agent_not_found" }
        })));
        assert!(!session_transfer_poll_may_retry(&serde_json::json!({
            "error": { "code": "agent_blocked" }
        })));
    }
}
