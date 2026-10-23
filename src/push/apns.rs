//! APNs HTTP/2 delivery for one alert to one device token.
//!
//! Delivery shells out to `curl --http2` (mirroring `herdr update`) so Herdr
//! adds no in-process HTTP/TLS stack. The auth JWT (team-wide, ~60 min) and the
//! JSON payload are secrets-adjacent, so they are NOT placed on curl's argv
//! (which is world-readable via `/proc/<pid>/cmdline` to any same-user process,
//! including agent panes Herdr spawns). Instead they are passed through a curl
//! config document fed on stdin via `--config -`. The config/argv builders and
//! the status classifier are pure functions, unit-tested without spawning curl.
//!
//! Follow-up (not in this change): set `apns-collapse-id` to coalesce repeated
//! alerts for the same pane.

use std::io::{Read, Write};
use std::process::{Child, Stdio};

use super::{PushKind, PushNotification};

const PROD_HOST: &str = "https://api.push.apple.com";
const SANDBOX_HOST: &str = "https://api.sandbox.push.apple.com";

/// curl writes the numeric HTTP status after the (small) APNs JSON body, on its
/// own line, so the trailing line of stdout is the status and the line(s) before
/// it are the body used to surface the APNs `reason`.
const HTTP_CODE_WRITEOUT: &str = "\n%{http_code}";

/// Outcome of a single delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryOutcome {
    Delivered,
    /// APNs reported the token as permanently unregistered (410) — prune it.
    PruneToken,
    /// APNs rejected the auth token (403: InvalidProviderToken / clock skew /
    /// wrong key id or team id). The cached JWT should be cleared and re-minted.
    AuthExpired,
    /// Transient or unexpected failure — keep the token, log, and move on.
    Failed,
}

/// Map a Herdr agent transition to the APNs alert body. `pane_id`/`workspace_id`
/// are the public API ids so the mobile client can deep-link.
pub(super) fn payload_body(notification: &PushNotification) -> String {
    let mut payload = serde_json::json!({
        "aps": {
            "alert": {
                "title": notification.title,
                "body": notification.body,
            },
            "sound": "default",
        },
        "pane_id": notification.pane_id,
        "workspace_id": notification.workspace_id,
    });
    // A gram alert deep-links to the app's Gram page, not a pane. The `gram`
    // marker lets the client branch on tap; `pane_id`/`workspace_id` are empty
    // for these and must be ignored when `gram` is present.
    if notification.kind == PushKind::Gram {
        payload["gram"] = serde_json::Value::Bool(true);
    }
    // An answerable question (herdr#309): the phone shows the options as
    // notification actions and answers with `agent.answer`.
    if let Some(ask) = &notification.ask {
        payload["aps"]["alert"] = serde_json::json!({
            "title": format!("{} asks", ask.agent),
            "body": ask.question,
        });
        payload["aps"]["category"] = serde_json::json!(super::ask::CATEGORY);
        payload["aps"]["mutable-content"] = serde_json::json!(1);
        payload["herdr_ask"] = serde_json::json!(ask);
    }
    payload.to_string()
}

fn device_url(sandbox: bool, device_token: &str) -> String {
    let host = if sandbox { SANDBOX_HOST } else { PROD_HOST };
    format!("{host}/3/device/{device_token}")
}

/// The non-sensitive curl argv. The JWT, headers, url, and payload are delivered
/// out-of-band via the stdin config (`--config -`) so no secret lands on argv.
pub(super) fn build_curl_argv() -> Vec<String> {
    vec![
        "--http2".to_string(),
        "-s".to_string(),
        "--connect-timeout".to_string(),
        "10".to_string(),
        "--max-time".to_string(),
        "20".to_string(),
        "-w".to_string(),
        HTTP_CODE_WRITEOUT.to_string(),
        "--config".to_string(),
        "-".to_string(),
    ]
}

/// Escape a value for a curl config double-quoted string. curl un-escapes `\\`
/// and `\"` inside quotes, so backslashes MUST be escaped before double-quotes
/// (order matters — escaping quotes first would then double-escape the added
/// backslashes). The JSON payload contains `"`, so this has to be exact.
pub(super) fn quote_config_value(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Build the curl config document (one directive per line) that carries the url, method,
/// headers (including the bearer JWT + `apns-push-type` / `apns-priority`), and JSON body.
/// Pure: no I/O, so the escaping and header set can be asserted directly. An alert uses
/// `"alert"` / `"10"`; a Live Activity UPDATE uses `"liveactivity"` / `"5"`.
pub(super) fn build_curl_config_typed(
    url: &str,
    jwt: &str,
    topic: &str,
    push_type: &str,
    priority: &str,
    payload: &str,
) -> String {
    let mut config = String::new();
    config.push_str(&format!("url = {}\n", quote_config_value(url)));
    config.push_str("request = \"POST\"\n");
    config.push_str(&format!(
        "header = {}\n",
        quote_config_value(&format!("authorization: bearer {jwt}"))
    ));
    config.push_str(&format!(
        "header = {}\n",
        quote_config_value(&format!("apns-topic: {topic}"))
    ));
    config.push_str(&format!(
        "header = {}\n",
        quote_config_value(&format!("apns-push-type: {push_type}"))
    ));
    config.push_str(&format!(
        "header = {}\n",
        quote_config_value(&format!("apns-priority: {priority}"))
    ));
    config.push_str(&format!("data = {}\n", quote_config_value(payload)));
    config
}

/// The APNs payload for a Live Activity UPDATE push. `content_state` is the widget's
/// dynamic ContentState — its keys must match the app's `AgentActivityAttributes.State`
/// (headline / status / needsYouCount / workingCount / totalCount). `timestamp` is Unix
/// seconds; Apple uses it to order updates and drop stale ones.
pub(super) fn live_activity_payload(content_state: &serde_json::Value, timestamp: u64) -> String {
    serde_json::json!({
        "aps": {
            "timestamp": timestamp,
            "event": "update",
            "content-state": content_state,
        }
    })
    .to_string()
}

/// Split curl's combined stdout into `(body, status)`. `stdout` is
/// `<body>\n<http_code>`.
pub(super) fn split_body_status(stdout: &str) -> (&str, &str) {
    let stdout = stdout.trim_end();
    match stdout.rsplit_once('\n') {
        Some((body, status)) => (body, status.trim()),
        None => ("", stdout.trim()),
    }
}

/// Extract the APNs `reason` string from a `{"reason":"..."}` error body.
fn reason_from_body(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn classify_status(status: &str) -> DeliveryOutcome {
    match status {
        "200" => DeliveryOutcome::Delivered,
        // 410 Unregistered is Apple's one documented permanent "token is dead"
        // signal. A 400 BadDeviceToken can also mean the token was minted for
        // the OTHER environment (prod vs sandbox), so pruning on it would delete
        // every token when `push.sandbox` is misconfigured — treat it as Failed.
        "410" => DeliveryOutcome::PruneToken,
        "403" => DeliveryOutcome::AuthExpired,
        _ => DeliveryOutcome::Failed,
    }
}

/// Classify curl's combined output. Test-only convenience that mirrors what
/// `deliver_one` does (split then classify) so the end-to-end parsing is
/// unit-tested; production splits once to also surface the reason.
#[cfg(test)]
fn classify(stdout: &str) -> DeliveryOutcome {
    let (_, status) = split_body_status(stdout);
    classify_status(status)
}

/// Deliver one alert to one device token by spawning curl and feeding the
/// sensitive directives on stdin. Best-effort: any spawn/IO failure is logged
/// and treated as a transient failure (token kept).
pub(super) fn deliver_one(
    device_token: &str,
    jwt: &str,
    topic: &str,
    sandbox: bool,
    payload: &str,
) -> DeliveryOutcome {
    deliver_one_typed(device_token, jwt, topic, sandbox, "alert", "10", payload)
}

/// Like [`deliver_one`] but with an explicit `apns-push-type` + `apns-priority`, so a
/// Live Activity update (`"liveactivity"` / `"5"`) reuses the exact same curl/stdin
/// delivery + status classification as an alert.
pub(super) fn deliver_one_typed(
    device_token: &str,
    jwt: &str,
    topic: &str,
    sandbox: bool,
    push_type: &str,
    priority: &str,
    payload: &str,
) -> DeliveryOutcome {
    let url = device_url(sandbox, device_token);
    let config = build_curl_config_typed(&url, jwt, topic, push_type, priority, payload);

    let Some(stdout) = run_curl_with_stdin_config(&config) else {
        return DeliveryOutcome::Failed;
    };
    let (body, status) = split_body_status(&stdout);
    let outcome = classify_status(status);
    if matches!(
        outcome,
        DeliveryOutcome::Failed | DeliveryOutcome::AuthExpired
    ) {
        // Status and APNs reason are diagnostics, not secrets.
        tracing::warn!(
            status = %status,
            reason = %reason_from_body(body).unwrap_or_else(|| "unknown".to_string()),
            "apns push delivery rejected"
        );
    }
    outcome
}

/// Upper bound on curl's stdout. A response is a small JSON body plus the status
/// line; the relay URL is configurable, so a faulty or hostile endpoint must not
/// be able to grow the daemon's memory without limit.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Spawn curl with the fixed [`build_curl_argv`] and feed `config` on stdin, so
/// url, headers and body never reach argv. Returns curl's stdout
/// (`<body>\n<http_code>`), or `None` after logging a spawn/IO failure or an
/// oversized response. stderr goes to null, so curl never blocks on it.
pub(super) fn run_curl_with_stdin_config(config: &str) -> Option<String> {
    let mut child = match crate::noninteractive_process::curl_command()
        .args(build_curl_argv())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(error = %err, "push curl failed to spawn");
            return None;
        }
    };

    // Write the config to stdin and close it (EOF) before draining stdout. The
    // config is tiny and curl reads it fully before issuing the request, so this
    // cannot deadlock.
    {
        let Some(mut stdin) = child.stdin.take() else {
            tracing::warn!("push curl child stdin was unavailable");
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        if let Err(err) = stdin.write_all(config.as_bytes()) {
            tracing::warn!(error = %err, "failed to write curl config to stdin");
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    }

    collect_capped_stdout(child, MAX_RESPONSE_BYTES)
}

/// Read at most `cap` bytes of `child`'s stdout, then reap it. Output past the
/// cap kills the child and yields `None`; none of the remote bytes are logged.
fn collect_capped_stdout(mut child: Child, cap: u64) -> Option<String> {
    let Some(stdout) = child.stdout.take() else {
        tracing::warn!("push curl child stdout was unavailable");
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    match read_capped(stdout, cap) {
        Ok(Some(bytes)) => {
            if let Err(err) = child.wait() {
                tracing::warn!(error = %err, "failed to reap curl");
            }
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        Ok(None) => {
            tracing::warn!(
                cap_bytes = cap,
                "push response exceeded the size cap; dropped"
            );
            let _ = child.kill();
            let _ = child.wait();
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to collect curl output");
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// All of `reader` when it ends within `cap` bytes, or `None` once it exceeds
/// the cap. Reads at most `cap + 1` bytes either way.
fn read_capped(reader: impl Read, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader.take(cap + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= cap).then_some(bytes))
}

#[cfg(test)]
mod tests {
    use super::super::PushKind;
    use super::*;

    fn sample_notification() -> PushNotification {
        PushNotification {
            title: "claude finished".to_string(),
            body: "herdr · 1".to_string(),
            pane_id: "w1-1".to_string(),
            workspace_id: "w_1".to_string(),
            kind: PushKind::Finished,
            #[cfg(unix)]
            guest_scope: None,
            ask_source: None,
            ask: None,
        }
    }

    #[test]
    fn payload_body_carries_alert_sound_and_public_ids() {
        let payload: serde_json::Value =
            serde_json::from_str(&payload_body(&sample_notification())).unwrap();
        assert_eq!(payload["aps"]["alert"]["title"], "claude finished");
        assert_eq!(payload["aps"]["alert"]["body"], "herdr · 1");
        assert_eq!(payload["aps"]["sound"], "default");
        assert_eq!(payload["pane_id"], "w1-1");
        assert_eq!(payload["workspace_id"], "w_1");
        // Agent-transition alerts carry no gram marker.
        assert!(payload.get("gram").is_none());
        // Nor, without a pending ask, any question.
        assert!(payload.get("herdr_ask").is_none());
        assert!(payload["aps"].get("category").is_none());
        assert!(payload["aps"].get("mutable-content").is_none());
    }

    #[test]
    fn payload_body_marks_gram_alerts() {
        let mut notification = sample_notification();
        notification.kind = PushKind::Gram;
        notification.pane_id = String::new();
        notification.workspace_id = String::new();
        let payload: serde_json::Value =
            serde_json::from_str(&payload_body(&notification)).unwrap();
        assert_eq!(payload["gram"], serde_json::Value::Bool(true));
    }

    #[test]
    fn payload_body_turns_a_pending_ask_into_an_actionable_question() {
        let mut notification = sample_notification();
        notification.kind = PushKind::NeedsInput;
        notification.ask = Some(super::super::PushAsk {
            agent: "jarvis".into(),
            pane_id: "w1-1".into(),
            machine_id: "machine-1".into(),
            ask_id: "call_7".into(),
            question: "Ship it?".into(),
            options: vec!["Yes".into(), "No".into()],
        });
        let payload: serde_json::Value =
            serde_json::from_str(&payload_body(&notification)).unwrap();
        assert_eq!(
            payload["aps"],
            serde_json::json!({
                "alert": {"title": "jarvis asks", "body": "Ship it?"},
                "sound": "default",
                "category": "herdr.ask",
                "mutable-content": 1,
            })
        );
        assert_eq!(
            payload["herdr_ask"],
            serde_json::json!({
                "agent": "jarvis",
                "pane_id": "w1-1",
                "machine_id": "machine-1",
                "ask_id": "call_7",
                "question": "Ship it?",
                "options": ["Yes", "No"],
            })
        );
        assert_eq!(payload["pane_id"], "w1-1");
    }

    #[test]
    fn live_activity_payload_carries_event_timestamp_and_content_state() {
        let content_state = serde_json::json!({ "headline": "claude", "status": "working" });
        let payload: serde_json::Value =
            serde_json::from_str(&live_activity_payload(&content_state, 1_700_000_000)).unwrap();
        assert_eq!(payload["aps"]["event"], "update");
        assert_eq!(payload["aps"]["timestamp"], 1_700_000_000u64);
        assert_eq!(payload["aps"]["content-state"]["headline"], "claude");
        assert_eq!(payload["aps"]["content-state"]["status"], "working");
    }

    #[test]
    fn build_curl_config_typed_sets_live_activity_headers() {
        let config = build_curl_config_typed(
            "https://api.push.apple.com/3/device/tok",
            "jwt",
            "com.example.herdr.push-type.liveactivity",
            "liveactivity",
            "5",
            "{}",
        );
        assert!(config.contains("header = \"apns-push-type: liveactivity\""));
        assert!(config.contains("header = \"apns-priority: 5\""));
        assert!(
            config.contains("header = \"apns-topic: com.example.herdr.push-type.liveactivity\"")
        );
    }

    #[test]
    fn quote_config_value_escapes_backslash_before_quote() {
        // Backslash must be escaped first; escaping the quote first would then
        // double-escape the inserted backslash.
        assert_eq!(quote_config_value(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn curl_config_carries_secrets_and_argv_does_not() {
        let payload = payload_body(&sample_notification());
        let url = device_url(false, "dev-token");
        let config = build_curl_config_typed(
            &url,
            "jwt-abc",
            "com.example.herdr",
            "alert",
            "10",
            &payload,
        );

        assert!(config.contains(&format!("url = \"{url}\"")));
        assert!(config.contains("request = \"POST\""));
        assert!(config.contains("header = \"authorization: bearer jwt-abc\""));
        assert!(config.contains("header = \"apns-topic: com.example.herdr\""));
        assert!(config.contains("header = \"apns-push-type: alert\""));
        assert!(config.contains("header = \"apns-priority: 10\""));
        assert!(config.contains("data = "));
        // The JSON payload's double-quotes are escaped for the config format.
        assert!(config.contains("\\\""));

        // The secret JWT and the payload must never appear on the argv.
        let argv = build_curl_argv();
        assert!(argv.iter().all(|arg| !arg.contains("jwt-abc")));
        assert!(!argv.contains(&payload));
        assert!(argv.contains(&"--config".to_string()));
        assert!(argv.contains(&"-".to_string()));
        assert!(argv.contains(&"--http2".to_string()));
        assert_eq!(
            argv[argv.iter().position(|arg| arg == "-w").unwrap() + 1],
            HTTP_CODE_WRITEOUT
        );
    }

    #[test]
    fn device_url_selects_host_by_environment() {
        assert_eq!(
            device_url(false, "tok"),
            "https://api.push.apple.com/3/device/tok"
        );
        assert_eq!(
            device_url(true, "tok"),
            "https://api.sandbox.push.apple.com/3/device/tok"
        );
    }

    #[test]
    fn classify_prunes_only_on_410_and_flags_403() {
        assert_eq!(classify("\n200"), DeliveryOutcome::Delivered);
        assert_eq!(
            classify("{\"reason\":\"Unregistered\"}\n410"),
            DeliveryOutcome::PruneToken
        );
        assert_eq!(
            classify("{\"reason\":\"InvalidProviderToken\"}\n403"),
            DeliveryOutcome::AuthExpired
        );
        // 400 BadDeviceToken is NOT a prune: a prod token hitting the sandbox
        // host (or vice-versa) returns this, and pruning would wipe every token.
        assert_eq!(
            classify("{\"reason\":\"BadDeviceToken\"}\n400"),
            DeliveryOutcome::Failed
        );
        assert_eq!(classify("\n500"), DeliveryOutcome::Failed);
        assert_eq!(classify(""), DeliveryOutcome::Failed);
    }

    #[test]
    fn reason_from_body_extracts_apns_reason() {
        assert_eq!(
            reason_from_body("{\"reason\":\"BadDeviceToken\"}").as_deref(),
            Some("BadDeviceToken")
        );
        assert_eq!(reason_from_body("").as_deref(), None);
        assert_eq!(reason_from_body("not json").as_deref(), None);
    }

    #[test]
    fn read_capped_stops_one_byte_past_the_cap() {
        assert_eq!(read_capped(&b"abc"[..], 3).unwrap(), Some(b"abc".to_vec()));
        assert_eq!(read_capped(&b"abcd"[..], 3).unwrap(), None);

        // An endless reader stops after cap + 1 bytes instead of buffering forever.
        let mut endless = std::io::repeat(b'x');
        assert_eq!(read_capped(&mut endless, 1024).unwrap(), None);
        let mut counted = CountingReader::default();
        assert_eq!(read_capped(&mut counted, 1024).unwrap(), None);
        assert_eq!(counted.read, 1025);
    }

    #[derive(Default)]
    struct CountingReader {
        read: u64,
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            buf.fill(b'x');
            self.read += buf.len() as u64;
            Ok(buf.len())
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn oversized_child_output_is_dropped_and_the_child_reaped() {
        // `yes` writes forever; without the cap this would never return.
        let child = std::process::Command::new("yes")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert_eq!(collect_capped_stdout(child, MAX_RESPONSE_BYTES), None);
        // Reaped: no zombie is left behind under this pid.
        assert!(!is_zombie(pid));
    }

    #[cfg(unix)]
    #[test]
    fn small_child_output_is_returned() {
        let child = std::process::Command::new("printf")
            .arg("{}\\n200")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = collect_capped_stdout(child, MAX_RESPONSE_BYTES).unwrap();
        assert_eq!(split_body_status(&stdout), ("{}", "200"));
    }

    /// True while `pid` exists as an unreaped zombie. A reaped pid has no /proc entry.
    #[cfg(target_os = "linux")]
    fn is_zombie(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|stat| {
                stat.rsplit_once(')')
                    .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
            })
            .unwrap_or(false)
    }
}
