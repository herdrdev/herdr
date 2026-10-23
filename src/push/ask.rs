//! Actionable question pushes (herdr#309): a NeedsInput alert for an omp agent
//! waiting on an `ask` carries the question and its options as `herdr_ask`,
//! so the phone can answer from the notification through `agent.answer`.
//!
//! The session file is read on the push thread, never the app loop. Only the
//! owner's alerts carry the question; guest payloads are built elsewhere
//! (`guest::push`) from the title and body alone.

use std::cell::LazyCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::{PushKind, PushNotification};
use crate::api::schema::PendingAsk;

/// Notification actions the phone can show for one alert.
pub(crate) const MAX_OPTIONS: usize = 4;
/// The APNs alert category the phone registers the answer actions under.
pub(super) const CATEGORY: &str = "herdr.ask";
const QUESTION_MAX_CHARS: usize = 240;
const AGENT_MAX_CHARS: usize = 60;
/// Labels travel verbatim (each is an `agent.answer` option), so an ask whose
/// labels would crowd Apple's 4 KB payload gets a plain alert instead.
const OPTIONS_MAX_BYTES: usize = 1024;
/// The alert can fire as omp draws the dialog, a moment before the session
/// line holding the ask is flushed.
const LOOKUP_TRIES: usize = 3;
const LOOKUP_PAUSE: Duration = Duration::from_millis(250);

/// Where a NeedsInput alert's local omp agent keeps its session, so the push
/// thread can look for a pending ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskSource {
    pub session_path: PathBuf,
    /// The agent's display name (`<agent> asks`).
    pub agent: String,
}

/// The `herdr_ask` payload object. `pane_id` is this daemon's public pane id
/// (the `agent.answer` target) and `machine_id` this daemon's machine id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PushAsk {
    pub agent: String,
    pub pane_id: String,
    pub machine_id: String,
    pub ask_id: String,
    pub question: String,
    pub options: Vec<String>,
}

/// Fill `ask` on every NeedsInput alert whose agent has a pending ask that a
/// notification can answer.
pub(super) fn attach(notifications: &mut [PushNotification]) {
    attach_with(
        notifications,
        crate::persist::machine::get_or_create,
        LOOKUP_PAUSE,
    );
}

fn attach_with(
    notifications: &mut [PushNotification],
    machine_id: impl FnOnce() -> String,
    pause: Duration,
) {
    let machine_id = LazyCell::new(machine_id);
    for notification in notifications {
        let Some(source) = notification.ask_source.take() else {
            continue;
        };
        if notification.kind != PushKind::NeedsInput {
            continue;
        }
        let Some(pending) = lookup(&source.session_path, pause) else {
            continue;
        };
        notification.ask = push_ask(&pending, &source.agent, &notification.pane_id, &machine_id);
    }
}

fn lookup(path: &Path, pause: Duration) -> Option<PendingAsk> {
    for attempt in 0..LOOKUP_TRIES {
        if attempt > 0 {
            std::thread::sleep(pause);
        }
        match crate::agent_transcript::pending_ask(path) {
            Ok(Some(ask)) => return Some(ask),
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "push: session file unreadable; plain alert");
                return None;
            }
        }
    }
    None
}

fn push_ask(pending: &PendingAsk, agent: &str, pane_id: &str, machine_id: &str) -> Option<PushAsk> {
    let question = crate::api::answer::drivable_question(pending)?;
    let options_bytes: usize = question.options.iter().map(String::len).sum();
    if question.options.len() > MAX_OPTIONS || options_bytes > OPTIONS_MAX_BYTES {
        return None;
    }
    Some(PushAsk {
        agent: super::sanitized_notification_text(agent, AGENT_MAX_CHARS)?,
        pane_id: pane_id.to_owned(),
        machine_id: machine_id.to_owned(),
        ask_id: pending.id.clone(),
        question: super::sanitized_notification_text(&question.question, QUESTION_MAX_CHARS)?,
        options: question.options.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TempSession(PathBuf);

    impl Drop for TempSession {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn session(questions: serde_json::Value, answered: bool) -> TempSession {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "herdr-push-ask-{}-{}.jsonl",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut text = json!({
            "type": "message",
            "id": "e1",
            "message": {"role": "assistant", "content": [{
                "type": "toolCall", "id": "call_7", "name": "ask",
                "arguments": {"questions": questions},
            }]},
        })
        .to_string()
            + "\n";
        if answered {
            text += &json!({
                "type": "message", "id": "e2", "parentId": "e1",
                "message": {"role": "toolResult", "toolCallId": "call_7", "toolName": "ask",
                    "details": {"selectedOptions": ["A"]}},
            })
            .to_string();
            text += "\n";
        }
        std::fs::write(&path, text).unwrap();
        TempSession(path)
    }

    fn labels(count: usize) -> serde_json::Value {
        (0..count)
            .map(|index| json!({"label": format!("Option {index}")}))
            .collect()
    }

    fn alert(kind: PushKind, source: Option<&TempSession>) -> PushNotification {
        PushNotification {
            title: "jarvis needs attention".into(),
            body: "herdr · 1".into(),
            pane_id: "w1-2".into(),
            workspace_id: "w_1".into(),
            kind,
            #[cfg(unix)]
            guest_scope: None,
            ask_source: source.map(|session| AskSource {
                session_path: session.0.clone(),
                agent: "jarvis".into(),
            }),
            ask: None,
        }
    }

    fn attached(notification: PushNotification) -> Option<PushAsk> {
        let mut batch = [notification];
        attach_with(&mut batch, || "machine-1".into(), Duration::ZERO);
        assert!(batch[0].ask_source.is_none(), "the source is consumed");
        batch[0].ask.take()
    }

    #[test]
    fn pending_single_select_ask_rides_the_needs_input_alert() {
        let file = session(
            json!([{"question": "Ship\nnow?", "options": labels(4)}]),
            false,
        );
        assert_eq!(
            attached(alert(PushKind::NeedsInput, Some(&file))),
            Some(PushAsk {
                agent: "jarvis".into(),
                pane_id: "w1-2".into(),
                machine_id: "machine-1".into(),
                ask_id: "call_7".into(),
                question: "Ship now?".into(),
                options: vec![
                    "Option 0".into(),
                    "Option 1".into(),
                    "Option 2".into(),
                    "Option 3".into(),
                ],
            })
        );
    }

    #[test]
    fn asks_a_notification_cannot_answer_get_a_plain_alert() {
        let five = session(json!([{"question": "Q", "options": labels(5)}]), false);
        assert_eq!(attached(alert(PushKind::NeedsInput, Some(&five))), None);
        let multi = session(
            json!([{"question": "Q", "options": labels(2), "multi": true}]),
            false,
        );
        assert_eq!(attached(alert(PushKind::NeedsInput, Some(&multi))), None);
        let long = session(
            json!([{"question": "Q", "options": [{"label": "x".repeat(OPTIONS_MAX_BYTES + 1)}]}]),
            false,
        );
        assert_eq!(attached(alert(PushKind::NeedsInput, Some(&long))), None);
        let answered = session(json!([{"question": "Q", "options": labels(2)}]), true);
        assert_eq!(attached(alert(PushKind::NeedsInput, Some(&answered))), None);
    }

    #[test]
    fn only_needs_input_alerts_with_a_source_look_for_an_ask() {
        let file = session(json!([{"question": "Q", "options": labels(2)}]), false);
        assert_eq!(attached(alert(PushKind::Finished, Some(&file))), None);
        let mut batch = [alert(PushKind::NeedsInput, None)];
        attach_with(
            &mut batch,
            || panic!("no ask, so no machine id lookup"),
            Duration::ZERO,
        );
        assert_eq!(batch[0].ask, None);
    }
}
