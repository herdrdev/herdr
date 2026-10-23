use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::*;
use crate::api::schema::AgentSessionInfo;

const FAST: Timing = Timing {
    selector_pause: Duration::ZERO,
    confirm_poll: Duration::from_millis(5),
    confirm_timeout: Duration::from_millis(150),
};

/// An omp pane behind the app: a boxed ask dialog that arrows move through
/// (clamped at the ends, like omp 18.10.4) and Enter submits by appending the
/// ask's tool result to the session file.
struct FakeOmp {
    options: Vec<String>,
    rows: Vec<String>,
    highlighted: usize,
    session: PathBuf,
    ask_id: String,
    arrows_land: bool,
    records_answer: bool,
    keys: Vec<String>,
}

impl FakeOmp {
    fn screen(&self) -> String {
        let mut screen = String::from("╭─ Ask ─────────────────────────────────────╮\n");
        screen.push_str("│ Which deploy path should I take?          │\n");
        for (index, row) in self.rows.iter().enumerate() {
            let cursor = if index == self.highlighted { "❯" } else { " " };
            screen.push_str(&format!("│ {cursor} ○ {row:<38}│\n"));
        }
        screen.push_str("╰───────────────────────────────────────────╯\n");
        screen
    }

    fn press(&mut self, key: &str) {
        self.keys.push(key.to_owned());
        match key {
            "up" if self.arrows_land => self.highlighted = self.highlighted.saturating_sub(1),
            "down" if self.arrows_land => {
                self.highlighted = (self.highlighted + 1).min(self.rows.len() - 1);
            }
            "enter" if self.records_answer => {
                let line = json!({
                    "type": "message",
                    "message": {
                        "role": "toolResult",
                        "toolCallId": self.ask_id,
                        "toolName": "ask",
                        "details": {"selectedOptions": [self.options[self.highlighted]]},
                    },
                });
                let mut text = std::fs::read_to_string(&self.session).unwrap();
                text.push_str(&format!("{line}\n"));
                std::fs::write(&self.session, text).unwrap();
            }
            _ => {}
        }
    }
}

struct Harness {
    omp: Arc<Mutex<FakeOmp>>,
    agent: AgentInfo,
    pane_id: String,
    session: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.session);
    }
}

fn ask_line(id: &str, questions: Value) -> String {
    json!({
        "type": "message",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "toolCall",
                "id": id,
                "name": "ask",
                "arguments": {"questions": questions},
            }],
        },
    })
    .to_string()
}

fn deploy_question() -> Value {
    json!([{
        "id": "deploy",
        "question": "Which deploy path should I take?",
        "options": [
            {"label": "Ship it now"},
            {"label": "Wait for review"},
            {"label": "Abort the change"},
        ],
        "recommended": 1,
    }])
}

/// A harness whose pane and ask ids are unique, so the process-wide claim set
/// never couples two tests.
fn harness(questions: Value) -> Harness {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let pane_id = format!("w1:p{n}");
    let ask_id = format!("call_{n}");
    let session = std::env::temp_dir().join(format!(
        "herdr-answer-{}-{n}.jsonl",
        std::process::id()
    ));
    std::fs::write(&session, ask_line(&ask_id, questions.clone()) + "\n").unwrap();
    let question = &questions[0];
    let options: Vec<String> = question["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["label"].as_str().unwrap().to_owned())
        .collect();
    let recommended = question["recommended"].as_u64().map(|index| index as usize);
    let rows = selector::display_rows(&options, recommended);
    let omp = FakeOmp {
        options,
        rows,
        highlighted: recommended.unwrap_or(0),
        session: session.clone(),
        ask_id,
        arrows_land: true,
        records_answer: true,
        keys: Vec::new(),
    };
    let mut agent: AgentInfo = serde_json::from_value(json!({
        "terminal_id": "term_1",
        "agent": "omp",
        "agent_status": "blocked",
        "input_pending": true,
        "composer": {},
        "screen_detection_skipped": false,
        "state_labels": {},
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "pane_id": pane_id,
        "focused": true,
        "launch_pending": false,
        "interactive_ready": true,
        "state_change_seq": 1,
        "revision": 1,
    }))
    .expect("agent info");
    agent.agent_session = Some(AgentSessionInfo {
        source: "omp".into(),
        agent: "omp".into(),
        kind: AgentSessionRefKind::Path,
        value: session.display().to_string(),
    });
    Harness {
        omp: Arc::new(Mutex::new(omp)),
        agent,
        pane_id,
        session,
    }
}

impl Harness {
    fn ask_id(&self) -> String {
        self.omp.lock().unwrap().ask_id.clone()
    }

    fn keys(&self) -> Vec<String> {
        self.omp.lock().unwrap().keys.clone()
    }

    fn params(&self, option: &str) -> AgentAnswerParams {
        AgentAnswerParams {
            target: self.pane_id.clone(),
            ask_id: self.ask_id(),
            option: option.into(),
        }
    }

    /// Run `agent.answer` against an app that serves `agent.get` with this
    /// harness's agent and pane reads/keys from the fake omp.
    fn answer(&self, params: &AgentAnswerParams) -> Value {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<crate::api::ApiRequestMessage>();
        let omp = Arc::clone(&self.omp);
        let agent = self.agent.clone();
        let app = std::thread::spawn(move || {
            while let Some(message) = api_rx.blocking_recv() {
                let id = message.request.id;
                let result = match message.request.method {
                    Method::AgentGet(_) => json!({"type": "agent_info", "agent": agent}),
                    Method::PaneRead(read) => {
                        let text = omp.lock().unwrap().screen();
                        json!({"type": "pane_read", "read": {
                            "pane_id": read.pane_id, "workspace_id": "w1", "tab_id": "w1:t1",
                            "source": "visible", "format": "text", "text": text,
                            "revision": 1, "truncated": false,
                        }})
                    }
                    Method::PaneSendKeys(keys) => {
                        let mut omp = omp.lock().unwrap();
                        for key in &keys.keys {
                            omp.press(key);
                        }
                        json!({"type": "ok"})
                    }
                    other => panic!("unexpected app request: {other:?}"),
                };
                let _ = message
                    .respond_to
                    .send(json!({"id": id, "result": result}).to_string());
            }
        });
        let response = answer_response("req".into(), params, &api_tx, FAST);
        drop(api_tx);
        app.join().unwrap();
        serde_json::from_str(&response).unwrap()
    }
}

fn error_code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or_else(|| {
        panic!("expected an error, got {response}");
    })
}

#[test]
fn answers_by_moving_from_the_recommended_row_and_confirms_from_the_session() {
    let harness = harness(deploy_question());
    let response = harness.answer(&harness.params("Ship it now"));
    assert_eq!(
        response["result"],
        json!({"type": "agent_answer", "status": "answered"})
    );
    assert_eq!(harness.keys(), ["up", "enter"]);
    assert_eq!(
        session::answer_since(&harness.session, 0, &harness.ask_id()).unwrap(),
        Some(vec!["Ship it now".to_owned()])
    );
}

#[test]
fn recommended_option_is_submitted_without_arrows() {
    let harness = harness(deploy_question());
    let response = harness.answer(&harness.params("Wait for review"));
    assert_eq!(response["result"]["status"], "answered");
    assert_eq!(harness.keys(), ["enter"]);
}

#[test]
fn highlight_that_does_not_land_never_sends_enter() {
    let harness = harness(deploy_question());
    harness.omp.lock().unwrap().arrows_land = false;
    let response = harness.answer(&harness.params("Abort the change"));
    assert_eq!(error_code(&response), "selection_mismatch");
    assert_eq!(harness.keys(), ["down"]);
    // Nothing was sent, so the claim is released and a retry is allowed.
    harness.omp.lock().unwrap().arrows_land = true;
    let retry = harness.answer(&harness.params("Abort the change"));
    assert_eq!(retry["result"]["status"], "answered");
}

#[test]
fn second_answer_to_the_same_ask_presses_nothing() {
    let harness = harness(deploy_question());
    // omp has not written its tool result yet: the ask still looks pending.
    harness.omp.lock().unwrap().records_answer = false;
    let first = harness.answer(&harness.params("Ship it now"));
    assert_eq!(first["result"]["status"], "sent_unconfirmed");
    assert_eq!(harness.keys(), ["up", "enter"]);
    let second = harness.answer(&harness.params("Abort the change"));
    assert_eq!(error_code(&second), "ask_not_pending");
    assert_eq!(harness.keys(), ["up", "enter"], "no key after the first Enter");
}

#[test]
fn answered_ask_is_not_pending() {
    let harness = harness(deploy_question());
    let first = harness.answer(&harness.params("Ship it now"));
    assert_eq!(first["result"]["status"], "answered");
    super::CLAIMS.lock().unwrap().clear();
    let again = harness.answer(&harness.params("Ship it now"));
    assert_eq!(error_code(&again), "ask_not_pending");
    assert_eq!(harness.keys(), ["up", "enter"]);
}

#[test]
fn stale_ask_id_is_not_pending() {
    let harness = harness(deploy_question());
    let mut params = harness.params("Ship it now");
    params.ask_id = "call_older".into();
    assert_eq!(error_code(&harness.answer(&params)), "ask_not_pending");
    assert!(harness.keys().is_empty());
}

#[test]
fn option_that_is_not_a_label_is_invalid() {
    let harness = harness(deploy_question());
    for option in ["Wait for review (Recommended)", "Other (type your own)", "ship it now"] {
        let response = harness.answer(&harness.params(option));
        assert_eq!(error_code(&response), "invalid_request", "{option}");
    }
    assert!(harness.keys().is_empty());
}

#[test]
fn multi_select_and_non_omp_agents_are_unsupported() {
    let multi = harness(json!([{
        "id": "q", "question": "Which?", "multi": true,
        "options": [{"label": "A"}, {"label": "B"}],
    }]));
    assert_eq!(
        error_code(&multi.answer(&multi.params("A"))),
        "ask_unsupported"
    );
    assert!(multi.keys().is_empty());

    let mut claude = harness(deploy_question());
    claude.agent.agent = Some("claude".into());
    assert_eq!(
        error_code(&claude.answer(&claude.params("Ship it now"))),
        "ask_unsupported"
    );
    let mut by_id = harness(deploy_question());
    by_id.agent.agent_session.as_mut().unwrap().kind = AgentSessionRefKind::Id;
    assert_eq!(
        error_code(&by_id.answer(&by_id.params("Ship it now"))),
        "ask_unsupported"
    );
    assert!(claude.keys().is_empty() && by_id.keys().is_empty());
}
