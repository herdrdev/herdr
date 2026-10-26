//! Normaliser, branch, paging and cursor rules over synthetic omp sessions.
//! Every fixture is made up here; none is copied from a real session.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use super::*;
use crate::api::schema::{
    AskQuestion, TranscriptAuthor, TranscriptNoteSource, TranscriptPlanItem, TranscriptStepStatus,
    TranscriptSubagentStatus, TranscriptTaskStatus, TranscriptTurnStatus,
};

const T0_MS: u64 = 1_792_749_600_000; // 2026-10-23T10:00:00Z

/// A synthetic omp session file, appended to the way omp appends: one JSON
/// entry per line, each parented to the previous one unless told otherwise.
pub(crate) struct Fixture {
    dir: PathBuf,
    pub(crate) path: PathBuf,
    next: u32,
    last: Option<String>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(crate) fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

pub(crate) fn thinking(text: &str) -> Value {
    json!({"type": "thinking", "thinking": text, "thinkingSignature": "sig"})
}

pub(crate) fn call(id: &str, name: &str, intent: Option<&str>, arguments: Value) -> Value {
    let mut part = json!({"type": "toolCall", "id": id, "name": name, "arguments": arguments});
    if let Some(intent) = intent {
        part["intent"] = json!(intent);
    }
    part
}

pub(crate) fn ask_call(id: &str, questions: Value) -> Value {
    call(
        id,
        "ask",
        Some("Asking which way to go"),
        json!({"questions": questions}),
    )
}

impl Fixture {
    pub(crate) fn new(tag: &str) -> Self {
        Self::create(tag, "")
    }

    /// A session file laid out the way omp stores it, with room for the
    /// `history.db` omp keeps beside `sessions/`.
    fn in_agent_dir(tag: &str) -> Self {
        Self::create(tag, "sessions/--tmp-project--")
    }

    fn create(tag: &str, subdir: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-transcript-test-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(subdir)).unwrap();
        let path = dir
            .join(subdir)
            .join("2026-10-23T10-00-00-000Z_sess-1.jsonl");
        let mut fixture = Self {
            dir,
            path,
            next: 0,
            last: None,
        };
        fixture.raw(&format!(
            "{}\n",
            json!({"type": "title", "v": 1, "title": "Synthetic", "updatedAt": "2026-10-23T10:00:00.000Z", "pad": "    "})
        ));
        fixture.raw(&format!(
            "{}\n",
            json!({"type": "session", "version": 3, "id": "sess-1", "timestamp": "2026-10-23T10:00:00.000Z", "cwd": "/tmp/project"})
        ));
        fixture
    }

    pub(crate) fn raw(&mut self, bytes: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
    }

    /// Append `entry` under `parent` (the previous entry when `None`).
    pub(crate) fn push_under(&mut self, mut entry: Value, parent: Option<&str>) -> String {
        self.next += 1;
        let id = format!("{:08x}", self.next);
        let parent = parent.map(str::to_owned).or_else(|| self.last.clone());
        entry["id"] = json!(id);
        entry["parentId"] = parent.map_or(Value::Null, Value::String);
        entry["timestamp"] = json!(self.iso(self.next));
        self.raw(&format!("{entry}\n"));
        self.last = Some(id.clone());
        id
    }

    pub(crate) fn push(&mut self, entry: Value) -> String {
        self.push_under(entry, None)
    }

    pub(crate) fn ms(&self, n: u32) -> u64 {
        T0_MS + u64::from(n) * 1000
    }

    fn iso(&self, n: u32) -> String {
        let minutes = n / 60;
        let seconds = n % 60;
        format!("2026-10-23T10:{minutes:02}:{seconds:02}.000Z")
    }

    pub(crate) fn user(&mut self, text: &str) -> String {
        let ms = self.ms(self.next + 1);
        self.push(json!({"type": "message", "message": {
            "role": "user", "content": [{"type": "text", "text": text}],
            "attribution": "user", "timestamp": ms,
        }}))
    }

    pub(crate) fn assistant(&mut self, content: Vec<Value>, stop: &str) -> String {
        let ms = self.ms(self.next + 1);
        self.push(json!({"type": "message", "message": {
            "role": "assistant", "content": content, "stopReason": stop,
            "timestamp": ms, "completedAt": ms + 500, "api": "x", "provider": "x", "model": "x",
            "usage": {"input": 1, "output": 1},
        }}))
    }

    pub(crate) fn failed(&mut self, stop: &str, error: &str) -> String {
        let ms = self.ms(self.next + 1);
        self.push(json!({"type": "message", "message": {
            "role": "assistant", "content": [], "stopReason": stop, "errorMessage": error,
            "timestamp": ms, "completedAt": ms + 500,
        }}))
    }

    pub(crate) fn result(
        &mut self,
        call: &str,
        tool: &str,
        is_error: bool,
        details: Value,
    ) -> String {
        let ms = self.ms(self.next + 1);
        self.push(json!({"type": "message", "message": {
            "role": "toolResult", "toolCallId": call, "toolName": tool, "isError": is_error,
            "content": [{"type": "text", "text": "output"}], "details": details, "timestamp": ms,
        }}))
    }

    pub(crate) fn note(
        &mut self,
        custom_type: &str,
        content: &str,
        display: bool,
        details: Value,
    ) -> String {
        self.push(
            json!({"type": "custom_message", "customType": custom_type, "content": content,
            "display": display, "details": details, "attribution": "agent"}),
        )
    }

    pub(crate) fn page(
        &self,
        before: Option<&str>,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Page {
        read_page(&self.path, &request(before, after, limit)).unwrap()
    }

    pub(crate) fn newest(&self) -> Page {
        self.page(None, None, None)
    }
}

fn request(before: Option<&str>, after: Option<&str>, limit: Option<u32>) -> PageRequest {
    PageRequest {
        before: before.map(str::to_owned),
        after: after.map(str::to_owned),
        limit,
        subagent: None,
    }
}

fn read_err(path: &Path, before: Option<&str>, after: Option<&str>) -> &'static str {
    read_page(path, &request(before, after, None))
        .unwrap_err()
        .code()
}

fn kinds(entries: &[TranscriptEntry]) -> Vec<&'static str> {
    entries
        .iter()
        .map(|entry| match entry {
            TranscriptEntry::User { .. } => "user",
            TranscriptEntry::Reply { .. } => "reply",
            TranscriptEntry::Thinking { .. } => "thinking",
            TranscriptEntry::Step { .. } => "step",
            TranscriptEntry::Note { .. } => "note",
            TranscriptEntry::Plan { .. } => "plan",
            TranscriptEntry::Ask { .. } => "ask",
            TranscriptEntry::TurnEnd { .. } => "turn_end",
            TranscriptEntry::Summary { .. } => "summary",
        })
        .collect()
}

fn ids(entries: &[TranscriptEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            serde_json::to_value(entry).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn user_texts(entries: &[TranscriptEntry]) -> Vec<&str> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::User { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn steps(
    entries: &[TranscriptEntry],
) -> Vec<(String, String, TranscriptStepStatus, Option<String>)> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::Step {
                tool,
                intent,
                status,
                detail,
                ..
            } => Some((tool.clone(), intent.clone(), *status, detail.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_turn_maps_to_user_thinking_reply_and_turn_end() {
    let mut session = Fixture::new("basic");
    let user = session.user("Fix the flaky test");
    let answer = session.assistant(
        vec![
            thinking("The race is in the setup."),
            text("Fixed it."),
            text("Tests pass now."),
        ],
        "stop",
    );
    let page = session.newest();
    assert_eq!(page.session_id, "sess-1");
    assert_eq!(
        page.entries,
        vec![
            TranscriptEntry::User {
                id: user,
                ts_ms: session.ms(1),
                text: "Fix the flaky test".into(),
                author: TranscriptAuthor::Owner,
                author_name: None,
            },
            TranscriptEntry::Thinking {
                id: format!("{answer}:thinking"),
                ts_ms: session.ms(2),
                text: "The race is in the setup.".into(),
            },
            TranscriptEntry::Reply {
                id: format!("{answer}:reply"),
                ts_ms: session.ms(2),
                text: "Fixed it.\n\nTests pass now.".into(),
                is_final: true,
            },
            TranscriptEntry::TurnEnd {
                id: format!("{answer}:end"),
                ts_ms: session.ms(2) + 500,
                status: TranscriptTurnStatus::Done,
                error: None,
            },
        ]
    );
    assert_eq!(page.before, None, "the whole session fits one page");
    assert_eq!(page.pending_ask, None);
    assert_eq!(page.working, None);
    let wire = serde_json::to_value(&page.entries[2]).unwrap();
    assert_eq!(wire["kind"], "reply");
    assert_eq!(wire["final"], true);
}

#[test]
fn a_guest_prompt_is_attributed_and_its_prefix_stripped() {
    let mut session = Fixture::new("guest");
    session.user("Ana (via HerdrUp): can you check the logs?");
    session.user("Note: (via HerdrUp): is only a prefix at the start of a line\nlater");
    session.user("plain owner prompt");
    let page = session.newest();
    let users: Vec<_> = page
        .entries
        .iter()
        .map(|entry| match entry {
            TranscriptEntry::User {
                text,
                author,
                author_name,
                ..
            } => (text.as_str(), *author, author_name.as_deref()),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        users,
        vec![
            (
                "can you check the logs?",
                TranscriptAuthor::Guest,
                Some("Ana")
            ),
            (
                "is only a prefix at the start of a line\nlater",
                TranscriptAuthor::Guest,
                Some("Note:")
            ),
            ("plain owner prompt", TranscriptAuthor::Owner, None),
        ]
    );
    let wire = serde_json::to_value(&page.entries[0]).unwrap();
    assert_eq!(wire["author"], "guest");
    assert_eq!(wire["author_name"], "Ana");
}

#[test]
fn text_before_a_tool_call_is_a_reply_and_steps_follow_their_results() {
    let mut session = Fixture::new("steps");
    session.user("Ship it");
    let long_command = "x".repeat(400);
    let first = session.assistant(
        vec![
            text("Let me look first."),
            call(
                "c1",
                "bash",
                Some("Running the tests"),
                json!({"command": "cargo test", "i": "ignored"}),
            ),
            call(
                "c2",
                "read",
                None,
                json!({"path": "src/main.rs", "i": "Reading main"}),
            ),
            call("c3", "grep", None, json!({"pattern": long_command})),
        ],
        "toolUse",
    );
    session.result("c1", "bash", false, json!({}));
    session.result("c2", "read", true, json!({}));
    let page = session.newest();
    assert_eq!(
        kinds(&page.entries),
        vec!["user", "reply", "step", "step", "step"]
    );
    assert!(matches!(
        &page.entries[1],
        TranscriptEntry::Reply { is_final: false, text, .. } if text == "Let me look first."
    ));
    let steps = steps(&page.entries);
    assert_eq!(
        steps[0],
        (
            "bash".into(),
            "Running the tests".into(),
            TranscriptStepStatus::Ok,
            Some("cargo test".into())
        )
    );
    assert_eq!(
        steps[1],
        (
            "read".into(),
            "Reading main".into(),
            TranscriptStepStatus::Error,
            Some("src/main.rs".into())
        )
    );
    let (tool, intent, status, detail) = &steps[2];
    assert_eq!(
        (tool.as_str(), intent.as_str()),
        ("grep", "grep"),
        "falls back to the tool name"
    );
    assert_eq!(*status, TranscriptStepStatus::Running);
    let detail = detail.as_deref().unwrap();
    assert_eq!(detail.chars().count(), 300);
    assert!(detail.ends_with('…'));
    assert_eq!(ids(&page.entries)[2], format!("{first}:c1"));
    assert_eq!(
        page.working,
        Some(TranscriptWorking {
            intent: "grep".into(),
            tool: "grep".into(),
            since_ms: session.ms(2) + 500,
        })
    );
    assert_eq!(page.pending_ask, None);
}

#[test]
fn a_call_left_without_a_result_by_later_output_or_an_exit_is_an_error() {
    let mut session = Fixture::new("lost");
    session.user("go");
    session.assistant(
        vec![call("c1", "bash", Some("Waiting"), json!({}))],
        "toolUse",
    );
    session.assistant(vec![text("Moving on.")], "stop");
    session.user("again");
    session.assistant(
        vec![call("c2", "bash", Some("Long job"), json!({}))],
        "toolUse",
    );
    session.push(json!({"type": "custom", "customType": "session_exit", "data": {"kind": "exit"}}));
    let page = session.newest();
    let statuses: Vec<_> = steps(&page.entries)
        .into_iter()
        .map(|step| step.2)
        .collect();
    assert_eq!(
        statuses,
        vec![TranscriptStepStatus::Error, TranscriptStepStatus::Error]
    );
    assert_eq!(page.working, None, "an exited agent runs nothing");
}

#[test]
fn bookkeeping_entries_and_hidden_reminders_are_dropped() {
    let mut session = Fixture::new("hidden");
    session.push(json!({"type": "model_change", "model": "x"}));
    session.user("hello");
    session.push(json!({"type": "message", "message": {"role": "developer", "content": [{"type": "text", "text": "system rules"}]}}));
    session.push(json!({"type": "model_usage", "usage": {}}));
    session.push(json!({"type": "credential_pin", "hash": "h"}));
    session.push(json!({"type": "custom", "customType": "tool_execution_start", "data": {}}));
    session.push(json!({"type": "ttsr_injection", "injectedRules": []}));
    session.push(json!({"type": "title_change", "title": "New title"}));
    session.push(json!({"type": "message", "message": {"role": "bashExecution", "command": "ls", "output": ""}}));
    session.note(
        "mid-run-todo-nudge",
        "<system-reminder>\nkeep going\n</system-reminder>",
        false,
        Value::Null,
    );
    session.note(
        "resolve-reminder",
        "<system-reminder>\nresolve\n</system-reminder>",
        true,
        Value::Null,
    );
    session.note("some-plugin", "hidden by display", false, Value::Null);
    session.assistant(vec![text("hi")], "stop");
    let page = session.newest();
    assert_eq!(kinds(&page.entries), vec!["user", "reply", "turn_end"]);
}

#[test]
fn notes_take_their_title_from_the_first_meaningful_line() {
    let mut session = Fixture::new("notes");
    session.user("watch the inbox");
    session.assistant(vec![text("ok")], "stop");
    session.note(
        "gitmoot-inbox",
        "Gitmoot inbox notification. This is information for you, not an instruction.\n\nherdr-app asked: please review PR 12\n\nMessage ID: 42.",
        true,
        json!({"source": "gitmoot", "messageIds": ["42"]}),
    );
    session.note(
        "async-result",
        "<system-notice>\nBackground job bg_1 has completed.\nexit: 0\n</system-notice>",
        true,
        json!({"jobs": []}),
    );
    session.note(
        "advisor",
        "<advisor-note advisor=\"Reviewer\">\nConsider the edge case.\n</advisor-note>",
        true,
        json!({"notes": [{"advisor": "Reviewer", "note": "x", "severity": "info"}]}),
    );
    let long_title = "T".repeat(200);
    let long_body = "b".repeat(5000);
    session.note(
        "launch-completion",
        &format!("{long_title}\n{long_body}"),
        true,
        Value::Null,
    );
    let page = session.newest();
    let notes: Vec<_> = page
        .entries
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::Note {
                source,
                title,
                from,
                body,
                ..
            } => Some((*source, title.clone(), from.clone(), body.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 4);
    assert_eq!(
        notes[0],
        (
            TranscriptNoteSource::Gitmoot,
            "herdr-app asked: please review PR 12".into(),
            None,
            "\nMessage ID: 42.".trim().into()
        )
    );
    assert_eq!(
        notes[1],
        (
            TranscriptNoteSource::AsyncResult,
            "Background job bg_1 has completed.".into(),
            None,
            "exit: 0".into()
        )
    );
    assert_eq!(
        notes[2],
        (
            TranscriptNoteSource::Advisor,
            "Consider the edge case.".into(),
            Some("Reviewer".into()),
            String::new()
        )
    );
    assert_eq!(notes[3].0, TranscriptNoteSource::Other);
    assert_eq!(notes[3].1.chars().count(), 120);
    assert_eq!(notes[3].3.chars().count(), 4000);
    // A note waking the idle agent closes the turn before it.
    assert_eq!(kinds(&page.entries)[..3], ["user", "reply", "turn_end"]);
}

#[test]
fn the_plan_is_the_turns_newest_todo_state_and_todo_calls_are_not_steps() {
    let mut session = Fixture::new("plan");
    let user = session.user("do three things");
    session.assistant(
        vec![call("t1", "todo", Some("Planning"), json!({"op": "init"}))],
        "toolUse",
    );
    session.result(
        "t1",
        "todo",
        false,
        json!({"phases": [{"name": "Work", "tasks": [
            {"content": "one", "status": "in_progress"},
            {"content": "two", "status": "pending"},
            {"content": "dropped", "status": "abandoned"},
        ]}]}),
    );
    session.assistant(
        vec![call(
            "b1",
            "bash",
            Some("Doing one"),
            json!({"command": "true"}),
        )],
        "toolUse",
    );
    session.result("b1", "bash", false, json!({}));
    session.assistant(
        vec![call("t2", "todo", Some("Updating"), json!({"op": "done"}))],
        "toolUse",
    );
    session.result(
        "t2",
        "todo",
        false,
        json!({"phases": [{"name": "Work", "tasks": [
            {"content": "one", "status": "completed"},
            {"content": "two", "status": "in_progress"},
        ]}]}),
    );
    let page = session.newest();
    assert_eq!(kinds(&page.entries), vec!["user", "plan", "step"]);
    assert_eq!(
        page.entries[1],
        TranscriptEntry::Plan {
            id: format!("{user}:plan"),
            ts_ms: session.ms(7),
            items: vec![
                TranscriptPlanItem {
                    text: "one".into(),
                    done: true,
                    status: TranscriptTaskStatus::Completed,
                    phase: Some("Work".into()),
                    blocker: None,
                },
                TranscriptPlanItem {
                    text: "two".into(),
                    done: false,
                    status: TranscriptTaskStatus::InProgress,
                    phase: Some("Work".into()),
                    blocker: None,
                },
            ],
        }
    );
}

#[test]
fn current_model_tracks_selection_role_branch_and_clear() {
    let mut session = Fixture::new("model-metadata");
    let root = session.push(json!({"type": "model_change", "model": "provider/primary"}));
    session.user("Work");
    session.push(json!({"type": "model_change", "role": "smol", "model": "provider/helper"}));
    assert_eq!(session.newest().model.unwrap().id, "primary");
    session.push(json!({"type": "model_change", "role": "default", "model": "other/selected"}));
    assert_eq!(
        session.newest().model,
        Some(TranscriptModel {
            provider: "other".into(),
            id: "selected".into(),
            thinking_level: None,
            thinking_auto: false,
        })
    );
    session.push_under(
        json!({"type": "message", "message": {
            "role": "user", "content": "Rewound branch"
        }}),
        Some(&root),
    );
    assert_eq!(session.newest().model.unwrap().id, "primary");
    session.push(json!({"type": "reset_boundary"}));
    assert_eq!(session.newest().model.unwrap().id, "primary");
}

#[test]
fn latest_plan_preserves_phases_states_and_blockers_outside_the_page() {
    let mut session = Fixture::new("plan-metadata");
    session.user("Work");
    session.assistant(vec![call("tasks", "todo", None, json!({}))], "toolUse");
    session.result(
        "tasks",
        "todo",
        false,
        json!({"phases": [
            {"name": "Build", "tasks": [
                {"content": "Ready", "status": "completed"},
                {"content": "Running", "status": "in_progress"}]},
            {"name": "Ship", "tasks": [
                {"content": "Review", "status": "blocked", "blocker": "Awaiting owner context"},
                {"content": "Deploy", "status": "pending"},
                {"content": "Discarded", "status": "abandoned"}]}
        ]}),
    );
    session.user("Next turn");
    let page = read_page(
        &session.path,
        &PageRequest {
            limit: Some(1),
            ..PageRequest::default()
        },
    )
    .unwrap();
    assert!(!page
        .entries
        .iter()
        .any(|entry| matches!(entry, TranscriptEntry::Plan { .. })));
    let plan = page.plan.unwrap();
    assert_eq!(
        plan.iter().map(|task| task.status).collect::<Vec<_>>(),
        [
            TranscriptTaskStatus::Completed,
            TranscriptTaskStatus::InProgress,
            TranscriptTaskStatus::Blocked,
            TranscriptTaskStatus::Pending,
        ]
    );
    assert_eq!(
        plan.iter()
            .map(|task| task.phase.as_deref())
            .collect::<Vec<_>>(),
        [Some("Build"), Some("Build"), Some("Ship"), Some("Ship")]
    );
    assert_eq!(plan[2].blocker.as_deref(), Some("Awaiting owner context"));
    session.push(json!({"type": "reset_boundary"}));
    assert!(session.newest().plan.is_none());
}

#[test]
fn an_ask_is_pending_until_its_result_records_the_answer() {
    let mut session = Fixture::new("ask");
    session.user("deploy?");
    session.assistant(
        vec![
            text("Two options."),
            ask_call("ask-1", json!([{"id": "q", "question": "Deploy now?", "options": [{"label": "Yes"}, {"label": "Later"}], "recommended": 0}])),
        ],
        "toolUse",
    );
    let pending = PendingAsk {
        id: "ask-1".into(),
        questions: vec![AskQuestion {
            question: "Deploy now?".into(),
            options: vec!["Yes".into(), "Later".into()],
            multi: false,
            recommended: Some(0),
        }],
        answerable: true,
    };
    let page = session.newest();
    assert_eq!(page.pending_ask, Some(pending.clone()));
    assert_eq!(page.working, None, "waiting on the user is not working");
    assert_eq!(kinds(&page.entries), vec!["user", "reply", "ask"]);
    assert!(
        matches!(&page.entries[2], TranscriptEntry::Ask { id, answer: None, .. } if id == "ask-1")
    );
    assert_eq!(pending_ask(&session.path).unwrap(), Some(pending));
    assert_eq!(ask_result(&session.path, "ask-1").unwrap(), None);

    session.result("ask-1", "ask", false, json!({"question": "Deploy now?", "options": ["Yes", "Later"], "multi": false, "selectedOptions": ["Later"]}));
    let page = session.newest();
    assert_eq!(page.pending_ask, None);
    assert!(matches!(
        &page.entries[2],
        TranscriptEntry::Ask { answer: Some(answer), .. } if answer == &vec!["Later".to_string()]
    ));
    assert_eq!(pending_ask(&session.path).unwrap(), None);
    assert_eq!(
        ask_result(&session.path, "ask-1").unwrap(),
        Some(vec!["Later".into()])
    );
}

#[test]
fn only_one_single_select_question_with_options_is_answerable() {
    for (questions, answerable) in [
        (
            json!([{"question": "Pick", "options": [{"label": "A"}], "multi": true}]),
            false,
        ),
        (json!([{"question": "Pick", "options": []}]), false),
        (
            json!([{"question": "One", "options": [{"label": "A"}]}, {"question": "Two", "options": [{"label": "B"}]}]),
            false,
        ),
        (
            json!([{"question": "Pick", "options": [{"label": "A"}]}]),
            true,
        ),
    ] {
        let mut session = Fixture::new("answerable");
        session.user("q");
        session.assistant(vec![ask_call("a", questions.clone())], "toolUse");
        let ask = pending_ask(&session.path).unwrap().expect("pending");
        assert_eq!(ask.answerable, answerable, "{questions}");
    }
}

#[test]
fn a_prompt_after_an_ask_abandons_it() {
    let mut session = Fixture::new("abandon");
    session.user("q");
    session.assistant(
        vec![ask_call(
            "a",
            json!([{"question": "Pick", "options": [{"label": "A"}]}]),
        )],
        "toolUse",
    );
    assert!(pending_ask(&session.path).unwrap().is_some());
    session.user("never mind, do B");
    assert_eq!(pending_ask(&session.path).unwrap(), None);
}

#[test]
fn a_multi_question_ask_collects_each_answer() {
    let mut session = Fixture::new("multi");
    session.user("q");
    session.assistant(vec![ask_call("a", json!([{"question": "One", "options": ["A"]}, {"question": "Two", "options": ["B", "C"], "multi": true}]))], "toolUse");
    session.result("a", "ask", false, json!({"results": [
        {"id": "1", "question": "One", "options": ["A"], "multi": false, "selectedOptions": ["A"]},
        {"id": "2", "question": "Two", "options": ["B", "C"], "multi": true, "selectedOptions": ["B", "C"], "customInput": "and D"},
    ]}));
    assert_eq!(
        ask_result(&session.path, "a").unwrap(),
        Some(vec!["A".into(), "B".into(), "C".into(), "and D".into()])
    );
    let page = session.newest();
    let TranscriptEntry::Ask { questions, .. } = &page.entries[1] else {
        panic!("{:?}", page.entries);
    };
    assert_eq!(
        questions[0].options,
        vec!["A".to_string()],
        "plain-string options"
    );
    assert!(questions[1].multi);
}

#[test]
fn a_turn_ends_only_where_omp_did_not_continue() {
    let mut session = Fixture::new("continuation");
    session.user("first");
    let retried = session.failed("error", "overloaded");
    // An automatic retry (or a todo nudge) continues the same turn.
    session.note(
        "mid-run-todo-nudge",
        "<system-reminder>\ncontinue\n</system-reminder>",
        false,
        Value::Null,
    );
    let stopped_early = session.assistant(vec![text("Partial answer.")], "stop");
    session.assistant(
        vec![call("c", "bash", Some("More work"), json!({}))],
        "toolUse",
    );
    session.result("c", "bash", false, json!({}));
    let done = session.assistant(vec![text("All done.")], "stop");
    session.user("second");
    let failed = session.failed("error", "rate limited");
    session.user("third");
    let aborted = session.failed("aborted", "Request was aborted");
    let page = session.newest();
    let ends: Vec<_> = page
        .entries
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::TurnEnd {
                id, status, error, ..
            } => Some((id.clone(), *status, error.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends,
        vec![
            (format!("{done}:end"), TranscriptTurnStatus::Done, None),
            (
                format!("{failed}:end"),
                TranscriptTurnStatus::Error,
                Some("rate limited".into())
            ),
            (
                format!("{aborted}:end"),
                TranscriptTurnStatus::Aborted,
                None
            ),
        ]
    );
    let entry_ids = ids(&page.entries);
    assert!(!entry_ids.contains(&format!("{retried}:end")));
    assert!(!entry_ids.contains(&format!("{stopped_early}:end")));
    assert!(
        entry_ids.contains(&format!("{stopped_early}:reply")),
        "the early answer still shows"
    );
}

#[test]
fn only_the_active_branch_is_shown_after_a_rewind() {
    let mut session = Fixture::new("branch");
    session.user("one");
    let kept = session.assistant(vec![text("answer one")], "stop");
    session.user("two (abandoned)");
    session.assistant(vec![text("abandoned answer")], "stop");
    let before_rewind = session.newest();
    // The user rewinds to after the first answer and asks again.
    session.push_under(
        json!({"type": "message", "message": {"role": "user", "content": [{"type": "text", "text": "two, again"}], "timestamp": 0}}),
        Some(&kept),
    );
    session.assistant(vec![text("new answer")], "stop");
    let page = session.newest();
    assert_eq!(user_texts(&page.entries), vec!["one", "two, again"]);
    assert!(!ids(&page.entries)
        .iter()
        .any(|id| id.starts_with("00000004")));
    // A client that saw the abandoned branch reloads.
    assert_eq!(
        read_err(&session.path, None, Some(&before_rewind.after)),
        "cursor_expired"
    );
}

#[test]
fn a_compaction_is_a_summary_and_older_turns_stay_pageable() {
    let mut session = Fixture::new("compaction");
    session.user("old question");
    session.assistant(vec![text("old answer")], "stop");
    let compaction = session.push(json!({"type": "compaction", "summary": "private summary text", "shortSummary": "s", "firstKeptEntryId": "00000001", "tokensBefore": 1, "tokensAfter": 1}));
    session.user("new question");
    session.assistant(vec![text("new answer")], "stop");
    let page = session.newest();
    assert_eq!(
        kinds(&page.entries),
        vec!["user", "reply", "turn_end", "summary", "user", "reply", "turn_end"]
    );
    assert_eq!(
        page.entries[3],
        TranscriptEntry::Summary {
            id: compaction,
            ts_ms: session.ms(3),
            text: "Earlier messages were summarised".into(),
        }
    );
    let newest = session.page(None, None, Some(1));
    assert_eq!(user_texts(&newest.entries), vec!["new question"]);
    let older = session.page(newest.before.as_deref(), None, Some(1));
    assert_eq!(
        kinds(&older.entries),
        vec!["user", "reply", "turn_end", "summary"]
    );
}

#[test]
fn a_clear_hides_what_came_before_it() {
    let mut session = Fixture::new("clear");
    session.user("before clear");
    session.assistant(vec![text("x")], "stop");
    session.push(json!({"type": "reset_boundary"}));
    session.user("after clear");
    let page = session.newest();
    assert_eq!(user_texts(&page.entries), vec!["after clear"]);
    assert_eq!(page.before, None);
}

#[test]
fn malformed_and_unfinished_lines_are_skipped() {
    let mut session = Fixture::new("malformed");
    session.user("one");
    session.raw("{not json\n");
    session.raw("\n");
    session.raw("{\"type\":\"message\",\"message\":{\"role\":\"user\"}}\n"); // no id
    let leaf = session.assistant(vec![text("fine")], "stop");
    // omp is mid-write: the last line has no newline yet.
    session.raw(&format!(
        "{{\"type\":\"message\",\"id\":\"partial\",\"parentId\":\"{leaf}\",\"message\":{{\"role\":\"user\",\"content\":\"ha"
    ));
    let page = session.newest();
    assert_eq!(kinds(&page.entries), vec!["user", "reply", "turn_end"]);
    let index = cached_index(&session.path);
    assert_eq!(index.lock().unwrap().malformed, 2);
    // The line completes: the next read picks it up.
    session.raw(&format!("lf\",\"timestamp\":{T0_MS}}}}}\n"));
    let page = session.page(None, Some(&page.after), None);
    assert_eq!(user_texts(&page.entries), vec!["half"]);
}

fn numbered_session(tag: &str, turns: usize) -> Fixture {
    let mut session = Fixture::new(tag);
    for turn in 1..=turns {
        session.user(&format!("question {turn}"));
        session.assistant(
            vec![call(
                &format!("c{turn}"),
                "bash",
                Some("Working"),
                json!({}),
            )],
            "toolUse",
        );
        session.result(&format!("c{turn}"), "bash", false, json!({}));
        session.assistant(vec![text(&format!("answer {turn}"))], "stop");
    }
    session
}

#[test]
fn pages_walk_back_by_turns_without_gaps_or_overlap() {
    let session = numbered_session("paging", 5);
    let newest = session.page(None, None, Some(2));
    assert_eq!(
        user_texts(&newest.entries),
        vec!["question 4", "question 5"]
    );
    let middle = session.page(newest.before.as_deref(), None, Some(2));
    assert_eq!(
        user_texts(&middle.entries),
        vec!["question 2", "question 3"]
    );
    let oldest = session.page(middle.before.as_deref(), None, Some(2));
    assert_eq!(user_texts(&oldest.entries), vec!["question 1"]);
    assert_eq!(oldest.before, None);
    let mut all = oldest.entries.clone();
    all.extend(middle.entries.clone());
    all.extend(newest.entries.clone());
    assert_eq!(all, session.page(None, None, Some(50)).entries);
    assert_eq!(
        kinds(&newest.entries),
        vec!["user", "step", "reply", "turn_end", "user", "step", "reply", "turn_end"]
    );
}

#[test]
fn the_limit_defaults_to_twenty_turns_and_is_clamped() {
    let session = numbered_session("limits", 60);
    assert_eq!(
        user_texts(&session.page(None, None, None).entries).len(),
        20
    );
    assert_eq!(
        user_texts(&session.page(None, None, Some(0)).entries).len(),
        1
    );
    assert_eq!(
        user_texts(&session.page(None, None, Some(500)).entries).len(),
        50
    );
}

#[test]
fn a_turn_longer_than_the_page_budget_is_split_mid_turn() {
    let mut session = Fixture::new("budget");
    session.user("marathon");
    for step in 0..2000 {
        session.assistant(
            vec![call(&format!("c{step}"), "bash", Some("Step"), json!({}))],
            "toolUse",
        );
        session.result(&format!("c{step}"), "bash", false, json!({}));
    }
    session.assistant(vec![text("finished")], "stop");
    let mut page = session.newest();
    assert!(
        page.before.is_some(),
        "the rest of the turn is an older page"
    );
    let mut all = Vec::new();
    let mut pages = 0;
    loop {
        assert!(page.entries.len() <= PAGE_RECORD_BUDGET + 1);
        pages += 1;
        let mut older = page.entries.clone();
        older.extend(all);
        all = older;
        let Some(before) = page.before.clone() else {
            break;
        };
        page = session.page(Some(&before), None, None);
    }
    assert_eq!(pages, 2, "1500 steps a page");
    assert_eq!(kinds(&all)[0], "user");
    assert_eq!(kinds(&all).last(), Some(&"turn_end"));
    assert_eq!(steps(&all).len(), 2000);
    let mut unique = ids(&all);
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), all.len(), "no entry appears on both pages");
}

#[test]
fn after_returns_appended_entries_and_resends_changed_ones() {
    let mut session = Fixture::new("after");
    session.user("go");
    let message = session.assistant(
        vec![call(
            "c1",
            "bash",
            Some("Building"),
            json!({"command": "make"}),
        )],
        "toolUse",
    );
    let first = session.newest();
    assert_eq!(steps(&first.entries)[0].2, TranscriptStepStatus::Running);
    assert_eq!(first.working.as_ref().unwrap().intent, "Building");

    // Nothing new: an empty page with the same position.
    let idle = session.page(None, Some(&first.after), None);
    assert!(idle.entries.is_empty());
    assert_eq!(idle.after, first.after);
    assert_eq!(idle.before, None, "after pages never page backwards");

    session.result("c1", "bash", false, json!({}));
    session.assistant(vec![call("t", "todo", None, json!({}))], "toolUse");
    session.result(
        "t",
        "todo",
        false,
        json!({"phases": [{"name": "p", "tasks": [{"content": "a", "status": "pending"}]}]}),
    );
    let update = session.page(None, Some(&first.after), None);
    assert_eq!(kinds(&update.entries), vec!["step", "plan"]);
    assert_eq!(
        ids(&update.entries)[0],
        format!("{message}:c1"),
        "same id, new status"
    );
    assert_eq!(steps(&update.entries)[0].2, TranscriptStepStatus::Ok);
    assert_eq!(update.working, None);

    session.assistant(vec![call("t2", "todo", None, json!({}))], "toolUse");
    session.result(
        "t2",
        "todo",
        false,
        json!({"phases": [{"name": "p", "tasks": [{"content": "a", "status": "completed"}]}]}),
    );
    session.assistant(vec![text("done")], "stop");
    let last = session.page(None, Some(&update.after), None);
    assert_eq!(kinds(&last.entries), vec!["plan", "reply", "turn_end"]);
    assert_eq!(
        ids(&last.entries)[0],
        ids(&update.entries)[1],
        "the plan keeps its id"
    );
    assert!(session
        .page(None, Some(&last.after), None)
        .entries
        .is_empty());
}

#[test]
fn an_after_page_over_budget_continues_from_where_it_stopped() {
    let mut session = Fixture::new("after-budget");
    session.user("start");
    let first = session.newest();
    for step in 0..2000 {
        session.assistant(
            vec![call(&format!("c{step}"), "bash", Some("Step"), json!({}))],
            "toolUse",
        );
        session.result(&format!("c{step}"), "bash", false, json!({}));
    }
    let one = session.page(None, Some(&first.after), None);
    let two = session.page(None, Some(&one.after), None);
    let three = session.page(None, Some(&two.after), None);
    assert!(three.entries.is_empty());
    assert_eq!(steps(&one.entries).len() + steps(&two.entries).len(), 2000);
    assert!(!two.entries.is_empty());
}

#[test]
fn foreign_garbled_or_rewritten_cursors_expire() {
    let session = numbered_session("expire", 3);
    let page = session.page(None, None, Some(1));
    let before = page.before.clone().unwrap();
    assert_eq!(
        read_err(&session.path, Some("garbage!"), None),
        "cursor_expired"
    );
    assert_eq!(
        read_err(&session.path, Some(&page.after), None),
        "cursor_expired"
    );
    assert_eq!(
        read_err(&session.path, Some(&before), Some(&page.after)),
        "invalid_request"
    );

    // Another session's cursor does not resolve here.
    let other = numbered_session("expire-other", 3);
    std::fs::write(
        &other.path,
        std::fs::read_to_string(&other.path)
            .unwrap()
            .replace("\"sess-1\"", "\"sess-2\""),
    )
    .unwrap();
    let foreign = other.page(None, None, Some(1));
    assert_eq!(
        read_err(&session.path, foreign.before.as_deref(), None),
        "cursor_expired"
    );

    // The file is rewritten in place (same path): old cursors expire.
    let mut rewritten = Fixture::new("expire-rewrite");
    rewritten.user("different");
    rewritten.assistant(vec![text("history")], "stop");
    std::fs::copy(&rewritten.path, &session.path).unwrap();
    assert_eq!(
        read_err(&session.path, Some(&before), None),
        "cursor_expired"
    );
    assert_eq!(
        read_err(&session.path, None, Some(&page.after)),
        "cursor_expired"
    );
    assert_eq!(user_texts(&session.newest().entries), vec!["different"]);
}

#[test]
fn a_missing_file_is_unavailable() {
    let session = Fixture::new("missing");
    std::fs::remove_file(&session.path).unwrap();
    assert_eq!(
        read_err(&session.path, None, None),
        "transcript_unavailable"
    );
    assert!(pending_ask(&session.path).is_err());
}

#[test]
fn appends_extend_the_index_instead_of_rescanning() {
    let mut session = numbered_session("incremental", 2);
    session.newest();
    let index = cached_index(&session.path);
    let first_offset = index.lock().unwrap().records[0].offset;
    session.user("question 3");
    let page = session.newest();
    assert_eq!(user_texts(&page.entries).last(), Some(&"question 3"));
    let index = index.lock().unwrap();
    assert_eq!(index.records[0].offset, first_offset);
    assert_eq!(index.records.len(), 9);
}

#[test]
fn iso_timestamps_parse_to_epoch_milliseconds() {
    assert_eq!(
        normalize::parse_iso8601_ms("2026-10-23T10:00:00.000Z"),
        Some(T0_MS)
    );
    assert_eq!(
        normalize::parse_iso8601_ms("2026-10-23T10:00:01.5Z"),
        Some(T0_MS + 1500)
    );
    assert_eq!(normalize::parse_iso8601_ms("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        normalize::parse_iso8601_ms("2024-02-29T00:00:00.000Z"),
        Some(1_709_164_800_000)
    );
    assert_eq!(normalize::parse_iso8601_ms("yesterday"), None);
}

/// `cargo test --release --bin herdr agent_transcript::tests::large_session_pages -- --ignored --nocapture`
/// Generates a ~150 MB synthetic session (long tool outputs, many turns) and
/// times the cold first page (full index scan) and warm pages.
#[test]
#[ignore = "generates a 150 MB file; run explicitly for timings"]
fn large_session_pages() {
    let mut session = Fixture::new("large");
    let output = "o".repeat(12_000);
    let mut turn = 0;
    while std::fs::metadata(&session.path).map_or(0, |meta| meta.len()) < 150_000_000 {
        turn += 1;
        session.user(&format!("question {turn}"));
        for step in 0..8 {
            let id = format!("c{turn}-{step}");
            session.assistant(
                vec![
                    thinking("Considering the next step carefully."),
                    call(&id, "bash", Some("Running"), json!({"command": "make"})),
                ],
                "toolUse",
            );
            session.push(json!({"type": "message", "message": {"role": "toolResult", "toolCallId": id, "toolName": "bash", "isError": false, "content": [{"type": "text", "text": output}], "details": {}}}));
        }
        session.assistant(vec![text("done")], "stop");
    }
    let size = std::fs::metadata(&session.path).unwrap().len();
    let started = std::time::Instant::now();
    let first = session.newest();
    let cold = started.elapsed();
    let started = std::time::Instant::now();
    let again = session.newest();
    let warm_newest = started.elapsed();
    let started = std::time::Instant::now();
    let older = session.page(first.before.as_deref(), None, None);
    let warm_older = started.elapsed();
    let started = std::time::Instant::now();
    let poll = session.page(None, Some(&first.after), None);
    let warm_after = started.elapsed();
    assert_eq!(first, again);
    assert_eq!(user_texts(&first.entries).len(), 20);
    assert_eq!(user_texts(&older.entries).len(), 20);
    assert!(poll.entries.is_empty());
    println!(
        "size={size} turns={turn} cold_first_page={cold:?} warm_newest={warm_newest:?} warm_before={warm_older:?} warm_after={warm_after:?}"
    );
}

fn recap_record(turn_id: &str) -> Value {
    json!({"type": "custom", "customType": "completion-recap", "data": {
        "version": 1, "sessionId": "sess-1", "turnId": turn_id,
        "state": "available", "text": "Parser fixed; review is next."
    }})
}

#[test]
fn recap_is_current_metadata_not_a_conversation_entry_and_can_be_cleared() {
    let mut session = Fixture::new("recap-current");
    session.user("Fix the parser");
    let turn = session.assistant(vec![text("Fixed")], "stop");
    let before = session.newest();
    session.push(recap_record(&turn));
    let page = session.newest();
    assert_eq!(page.entries, before.entries);
    assert_eq!(
        page.recap,
        Some(TranscriptRecap {
            turn_id: turn.clone(),
            text: "Parser fixed; review is next.".into(),
        })
    );
    let delta = session.page(None, Some(&before.after), None);
    assert_eq!(delta.recap, page.recap);
    let mut cleared = recap_record(&turn);
    cleared["data"]["state"] = json!("cleared");
    session.push(cleared);
    assert_eq!(session.newest().recap, None);
}

#[test]
fn late_recap_cannot_cross_new_work_or_a_reset() {
    for work in [
        json!({"type": "message", "message": {"role": "user", "content": "New request"}}),
        json!({"type": "message", "message": {"role": "assistant", "content": [], "stopReason": "stop"}}),
        json!({"type": "message", "message": {"role": "toolResult", "toolName": "read", "toolCallId": "x", "content": []}}),
        json!({"type": "compaction", "summary": "Not a completion recap"}),
        json!({"type": "reset_boundary"}),
    ] {
        let mut session = Fixture::new("recap-stale");
        session.user("Old request");
        let turn = session.assistant(vec![text("Old answer")], "stop");
        session.push(recap_record(&turn));
        session.push(work.clone());
        assert_eq!(session.newest().recap, None, "new work: {work}");
        session.push(recap_record(&turn));
        assert_eq!(session.newest().recap, None, "late result after: {work}");
    }
}

#[test]
fn recap_rejects_wrong_identity_unknown_version_and_empty_text() {
    for (field, value) in [
        ("version", json!(2)),
        ("sessionId", json!("another-session")),
        ("turnId", json!("older-turn")),
        ("state", json!("pending")),
        ("text", json!("  ")),
    ] {
        let mut session = Fixture::new("recap-invalid");
        session.user("Work");
        let turn = session.assistant(vec![text("Done")], "stop");
        let mut record = recap_record(&turn);
        record["data"][field] = value;
        session.push(record);
        assert_eq!(session.newest().recap, None, "{field}");
    }
}

/// Seconds since the epoch of fixture entry `n` (entries are one second apart).
fn entry_second(n: u64) -> i64 {
    i64::try_from(T0_MS / 1000 + n).unwrap()
}

/// omp's `history.db` beside the fixture's `sessions/`, with omp's own schema.
fn write_history(session: &Fixture, rows: &[(&str, &str, i64)]) {
    let db = rusqlite::Connection::open(session.dir.join("history.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_recaps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            cwd TEXT NOT NULL,
            recap TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER))
        );",
    )
    .unwrap();
    for (session_id, recap, created_at) in rows {
        db.execute(
            "INSERT INTO session_recaps (session_id, cwd, recap, created_at) VALUES (?1, '/tmp/project', ?2, ?3)",
            rusqlite::params![session_id, recap, created_at],
        )
        .unwrap();
    }
}

#[test]
fn history_recap_is_shown_only_when_written_after_the_newest_reply() {
    let mut session = Fixture::in_agent_dir("history-recap");
    session.user("Fix the parser");
    let turn = session.assistant(vec![text("Fixed")], "stop");
    // Written in the reply's own second: whole-second rows cannot prove it came after.
    write_history(&session, &[("sess-1", "Same second.", entry_second(2))]);
    assert_eq!(session.newest().recap, None);
    write_history(
        &session,
        &[("sess-1", " Parser fixed; review is next. ", entry_second(3))],
    );
    assert_eq!(
        session.newest().recap,
        Some(TranscriptRecap {
            turn_id: turn,
            text: "Parser fixed; review is next.".into(),
        })
    );
}

#[test]
fn history_recap_is_hidden_by_newer_work() {
    let mut session = Fixture::in_agent_dir("history-stale");
    session.user("Fix the parser");
    session.assistant(vec![text("Fixed")], "stop");
    write_history(&session, &[("sess-1", "Parser fixed.", entry_second(3))]);
    assert!(session.newest().recap.is_some());
    session.user("Now add a test");
    assert_eq!(session.newest().recap, None, "pending user request");
    session.assistant(vec![text("Test added")], "stop");
    assert_eq!(session.newest().recap, None, "reply newer than the recap");
}

#[test]
fn history_recap_uses_this_session_newest_row() {
    let mut session = Fixture::in_agent_dir("history-identity");
    session.user("Work");
    session.assistant(vec![text("Done")], "stop");
    write_history(
        &session,
        &[
            ("sess-1", "Older recap.", entry_second(3)),
            ("sess-1", "Newest recap.", entry_second(5)),
            ("another-session", "Not this session.", entry_second(9)),
        ],
    );
    assert_eq!(session.newest().recap.unwrap().text, "Newest recap.");
}

#[test]
fn turn_bound_records_take_precedence_over_history() {
    let mut session = Fixture::in_agent_dir("history-precedence");
    session.user("Work");
    let turn = session.assistant(vec![text("Done")], "stop");
    session.push(recap_record(&turn));
    write_history(&session, &[("sess-1", "From history.", entry_second(9))]);
    assert_eq!(
        session.newest().recap.unwrap().text,
        "Parser fixed; review is next."
    );
    let mut cleared = recap_record(&turn);
    cleared["data"]["state"] = json!("cleared");
    session.push(cleared);
    assert_eq!(
        session.newest().recap,
        None,
        "a cleared record is not replaced by history"
    );
}

#[test]
fn unreadable_history_means_no_recap() {
    let mut session = Fixture::in_agent_dir("history-unreadable");
    session.user("Work");
    session.assistant(vec![text("Done")], "stop");
    assert_eq!(session.newest().recap, None, "no history.db");
    std::fs::write(session.dir.join("history.db"), b"not a database").unwrap();
    assert_eq!(session.newest().recap, None, "corrupt history.db");
}

#[test]
fn current_model_carries_the_newest_thinking_level() {
    let mut session = Fixture::new("thinking-level");
    session.push(
        json!({"type": "thinking_level_change", "thinkingLevel": "high", "configured": "high"}),
    );
    let selected =
        session.push(json!({"type": "model_change", "model": "anthropic/claude-opus-5-5"}));
    session.user("Work");
    let effort = |session: &Fixture| {
        let model = session.newest().model.unwrap();
        assert_eq!(model.id, "claude-opus-5-5");
        (model.thinking_level, model.thinking_auto)
    };
    assert_eq!(effort(&session), (Some("high".into()), false));
    session.push(
        json!({"type": "thinking_level_change", "thinkingLevel": "xhigh", "configured": "auto"}),
    );
    assert_eq!(
        effort(&session),
        (Some("xhigh".into()), true),
        "auto, resolved"
    );
    session.push(
        json!({"type": "thinking_level_change", "thinkingLevel": null, "configured": "auto"}),
    );
    assert_eq!(effort(&session), (None, true), "auto, not classified yet");
    session.push(
        json!({"type": "thinking_level_change", "thinkingLevel": "off", "configured": "off"}),
    );
    assert_eq!(effort(&session), (None, false), "off is no effort");
    session.push(json!({"type": "thinking_level_change", "thinkingLevel": "inherit"}));
    assert_eq!(effort(&session), (None, false));
    session.push(
        json!({"type": "thinking_level_change", "thinkingLevel": "turbo", "configured": "turbo"}),
    );
    assert_eq!(
        effort(&session),
        (None, false),
        "unknown levels are dropped"
    );
    session.push(json!({"type": "thinking_level_change", "thinkingLevel": "medium"}));
    assert_eq!(
        effort(&session),
        (Some("medium".into()), false),
        "an entry from before `configured` existed"
    );
    // A rewind to before the later changes restores the branch's own level.
    session.push_under(
        json!({"type": "message", "message": {"role": "user", "content": "Rewound"}}),
        Some(&selected),
    );
    assert_eq!(effort(&session), (Some("high".into()), false));
    session.push(json!({"type": "reset_boundary"}));
    assert_eq!(
        effort(&session),
        (Some("high".into()), false),
        "survives /clear"
    );

    let plain = serde_json::to_value(TranscriptModel {
        provider: "p".into(),
        id: "m".into(),
        thinking_level: None,
        thinking_auto: false,
    })
    .unwrap();
    assert_eq!(
        plain,
        json!({"provider": "p", "id": "m"}),
        "old apps see no new keys"
    );
}

/// A `task` tool result as omp writes it when it spawns background
/// subagents: `(id, assignment, description)` per spawn.
pub(crate) fn spawn_details(spawns: &[(&str, &str, Option<&str>)]) -> Value {
    let progress: Vec<Value> = spawns
        .iter()
        .enumerate()
        .map(|(index, (id, task, description))| {
            let mut item = json!({
                "index": index, "id": id, "agent": "reviewer", "agentSource": "bundled",
                "modelRole": "slow", "status": "pending",
                "task": format!("Complete assignment thoroughly:\n\n{task}"),
                "assignment": task, "recentTools": [], "recentOutput": [], "toolCount": 0,
                "requests": 0, "tokens": 0, "cost": 0, "durationMs": 0,
            });
            if let Some(description) = description {
                item["description"] = json!(description);
            }
            item
        })
        .collect();
    json!({
        "projectAgentsDir": null, "results": [], "totalDurationMs": 1, "progress": progress,
        "async": {"state": "running", "jobId": spawns[0].0, "type": "task"},
    })
}

/// The `async-result` note omp writes when background subagent `id` settles.
fn task_settled(session: &mut Fixture, id: &str, status: &str, duration_ms: u64) -> String {
    session.note(
        "async-result",
        &format!(
            "<system-notice>\nBackground job {id} has completed.\n\n<task-result id=\"{id}\" agent=\"reviewer\" status=\"{status}\" duration=\"1m0s\">\n<output>\nLGTM\n</output>\n</task-result>\n\nRead the transcript at history://{id}\n</system-notice>"
        ),
        true,
        json!({"jobs": [{"jobId": id, "type": "task", "label": id, "durationMs": duration_ms}]}),
    )
}

/// Subagent `id`'s own session file beside `parent`, as omp lays it out,
/// with a header naming `parent_session`.
pub(crate) fn child_file(parent: &Path, id: &str, parent_session: &str) -> PathBuf {
    let dir = parent.with_file_name(parent.file_stem().unwrap());
    let path = dir.join(format!("{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let header = json!({
        "type": "session", "version": 3, "id": format!("child-{id}"),
        "timestamp": "2026-10-23T10:00:00.000Z", "cwd": "/tmp/project",
        "parentSession": parent_session,
    });
    std::fs::write(&path, format!("{header}\n")).unwrap();
    path
}

fn append(path: &Path, lines: &[Value]) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

fn subagent_states(page: &Page) -> Vec<(String, TranscriptSubagentStatus)> {
    page.subagents
        .iter()
        .flatten()
        .map(|subagent| (subagent.id.clone(), subagent.status))
        .collect()
}

#[test]
fn a_background_task_is_a_running_subagent_until_its_async_result() {
    let mut session = Fixture::new("subagents");
    session.user("Review both heads");
    session.assistant(
        vec![call(
            "t1",
            "task",
            Some("Dispatching final reviews"),
            json!({"context": "# Goal\nA long shared context", "tasks": [
                {"name": "AppReview", "agent": "reviewer", "task": "Review the app head"},
                {"name": "DaemonReview", "agent": "reviewer", "task": "Review the daemon head"},
            ]}),
        )],
        "toolUse",
    );
    session.result(
        "t1",
        "task",
        false,
        spawn_details(&[
            ("AppReview", "Review the app head", Some("#12 app review")),
            ("DaemonReview", "Review the daemon head", None),
        ]),
    );
    session.assistant(vec![text("Both reviews are running.")], "stop");
    let page = session.newest();
    let spawned_ms = session.ms(3);
    assert_eq!(
        page.subagents,
        Some(vec![
            TranscriptSubagent {
                id: "AppReview".into(),
                agent: "reviewer".into(),
                label: Some("#12 app review".into()),
                task: Some("Review the app head".into()),
                status: TranscriptSubagentStatus::Running,
                started_ms: spawned_ms,
                finished_ms: None,
                duration_ms: None,
                last_intent: None,
                last_ms: None,
                has_transcript: false,
            },
            TranscriptSubagent {
                id: "DaemonReview".into(),
                agent: "reviewer".into(),
                label: Some("DaemonReview".into()),
                task: Some("Review the daemon head".into()),
                status: TranscriptSubagentStatus::Running,
                started_ms: spawned_ms,
                finished_ms: None,
                duration_ms: None,
                last_intent: None,
                last_ms: None,
                has_transcript: false,
            },
        ])
    );
    // The step names what it spawned, not the call's JSON.
    assert_eq!(
        steps(&page.entries)[0].3.as_deref(),
        Some("AppReview, DaemonReview")
    );

    // A background bash job's result settles nothing, even under the same id.
    session.note(
        "async-result",
        "<system-notice>\nBackground job AppReview has completed.\nexit: 0\n</system-notice>",
        true,
        json!({"jobs": [{"jobId": "AppReview", "type": "bash", "label": "cargo test", "durationMs": 5000}]}),
    );
    assert_eq!(
        subagent_states(&session.newest()),
        vec![
            ("AppReview".into(), TranscriptSubagentStatus::Running),
            ("DaemonReview".into(), TranscriptSubagentStatus::Running),
        ]
    );

    let settled = task_settled(&mut session, "AppReview", "completed", 60_000);
    let settled_ms = session.ms(session.next);
    let page = session.newest();
    assert_eq!(
        subagent_states(&page),
        vec![
            ("DaemonReview".into(), TranscriptSubagentStatus::Running),
            ("AppReview".into(), TranscriptSubagentStatus::Completed),
        ],
        "running first"
    );
    let app = &page.subagents.as_ref().unwrap()[1];
    assert_eq!(app.finished_ms, Some(settled_ms));
    assert_eq!(app.duration_ms, Some(60_000), "omp's own duration");
    // The async result still renders as a note.
    assert!(ids(&page.entries).contains(&settled));

    task_settled(&mut session, "DaemonReview", "failed (exit 1)", 90_000);
    assert_eq!(
        subagent_states(&session.newest()),
        vec![
            ("AppReview".into(), TranscriptSubagentStatus::Completed),
            ("DaemonReview".into(), TranscriptSubagentStatus::Failed),
        ]
    );

    // Finished subagents belong to their turn.
    session.user("Thanks, next thing");
    assert_eq!(session.newest().subagents, None);
}

#[test]
fn an_exit_or_a_clear_settles_running_subagents() {
    let mut session = Fixture::new("subagents-exit");
    session.user("Start the long one");
    session.assistant(
        vec![call(
            "t1",
            "task",
            None,
            json!({"name": "Old", "agent": "task", "task": "Run for hours"}),
        )],
        "toolUse",
    );
    session.result(
        "t1",
        "task",
        false,
        spawn_details(&[("Old", "Run for hours", None)]),
    );
    session.user("Meanwhile, another");
    assert_eq!(
        subagent_states(&session.newest()),
        vec![("Old".into(), TranscriptSubagentStatus::Running)],
        "a running subagent stays listed in later turns"
    );
    session.assistant(
        vec![call(
            "t2",
            "task",
            None,
            json!({"name": "New", "agent": "task", "task": "Quick one"}),
        )],
        "toolUse",
    );
    session.result(
        "t2",
        "task",
        false,
        spawn_details(&[("New", "Quick one", None)]),
    );
    session.push(json!({"type": "custom", "customType": "session_exit", "data": {"reason": "dispose", "kind": "normal"}}));
    let exit_ms = session.ms(session.next);
    let page = session.newest();
    assert_eq!(
        subagent_states(&page),
        vec![("New".into(), TranscriptSubagentStatus::Aborted)],
        "the exit aborts both; the older turn's is no longer listed"
    );
    assert_eq!(
        page.subagents.as_ref().unwrap()[0].finished_ms,
        Some(exit_ms)
    );

    session.user("Resume");
    session.assistant(
        vec![call(
            "t3",
            "task",
            None,
            json!({"name": "Third", "agent": "task", "task": "Again"}),
        )],
        "toolUse",
    );
    session.result(
        "t3",
        "task",
        false,
        spawn_details(&[("Third", "Again", None)]),
    );
    assert_eq!(
        subagent_states(&session.newest()),
        vec![("Third".into(), TranscriptSubagentStatus::Running)]
    );
    session.push(json!({"type": "reset_boundary"}));
    assert_eq!(session.newest().subagents, None, "/clear forgets them");
}

#[test]
fn a_subagent_reports_its_own_files_newest_intent_and_end() {
    let mut session = Fixture::new("subagents-child");
    session.user("Review it");
    session.assistant(
        vec![call(
            "t1",
            "task",
            None,
            json!({"name": "Rev", "agent": "reviewer", "task": "Review"}),
        )],
        "toolUse",
    );
    session.result(
        "t1",
        "task",
        false,
        spawn_details(&[("Rev", "Review", None), ("NoFile", "Review", None)]),
    );
    let parent = session.path.to_string_lossy().into_owned();
    let child = child_file(&session.path, "Rev", &parent);
    append(
        &child,
        &[
            json!({"type": "message", "id": "c1", "parentId": null, "timestamp": "2026-10-23T10:01:00.000Z", "message": {"role": "user", "content": [{"type": "text", "text": "Review"}], "timestamp": 1}}),
            json!({"type": "message", "id": "c2", "parentId": "c1", "timestamp": "2026-10-23T10:01:05.000Z", "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "k1", "name": "read", "arguments": {"path": "src/main.rs"}}], "stopReason": "toolUse", "timestamp": 2}}),
            json!({"type": "custom", "customType": "tool_execution_start", "id": "c3", "parentId": "c2", "timestamp": "2026-10-23T10:01:06.000Z", "data": {"toolCallId": "k1", "toolName": "read", "startedAt": "2026-10-23T10:01:06.000Z", "intent": "Reading the diff"}}),
        ],
    );
    let page = session.newest();
    let rev = &page.subagents.as_ref().unwrap()[0];
    assert_eq!(rev.id, "Rev");
    assert_eq!(rev.status, TranscriptSubagentStatus::Running);
    assert_eq!(rev.last_intent.as_deref(), Some("Reading the diff"));
    assert_eq!(rev.last_ms, Some(T0_MS + 66_000));
    assert!(rev.has_transcript);
    let no_file = &page.subagents.as_ref().unwrap()[1];
    assert_eq!(
        (no_file.id.as_str(), no_file.has_transcript),
        ("NoFile", false)
    );

    // `hub wait` consumed the async result: the child's own exit after its
    // yield completes it.
    append(
        &child,
        &[
            json!({"type": "message", "id": "c4", "parentId": "c3", "timestamp": "2026-10-23T10:01:07.000Z", "message": {"role": "toolResult", "toolCallId": "k1", "toolName": "read", "isError": false, "content": [], "timestamp": 3}}),
            json!({"type": "message", "id": "c5", "parentId": "c4", "timestamp": "2026-10-23T10:01:08.000Z", "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "k2", "name": "yield", "arguments": {"data": {}}}], "stopReason": "toolUse", "timestamp": 4}}),
            json!({"type": "message", "id": "c6", "parentId": "c5", "timestamp": "2026-10-23T10:01:09.000Z", "message": {"role": "toolResult", "toolCallId": "k2", "toolName": "yield", "isError": false, "content": [{"type": "text", "text": "Result submitted."}], "details": {"status": "success"}, "timestamp": 5}}),
            json!({"type": "custom", "customType": "session_exit", "id": "c7", "parentId": "c6", "timestamp": "2026-10-23T10:01:10.000Z", "data": {"reason": "dispose", "kind": "normal"}}),
        ],
    );
    let page = session.newest();
    let rev = page
        .subagents
        .iter()
        .flatten()
        .find(|subagent| subagent.id == "Rev")
        .unwrap();
    assert_eq!(rev.status, TranscriptSubagentStatus::Completed);
    assert_eq!(rev.finished_ms, Some(T0_MS + 70_000));
    assert_eq!(rev.last_intent.as_deref(), Some("Reading the diff"));
    assert!(rev.has_transcript);
}

#[test]
fn the_subagent_param_pages_only_a_real_child_of_the_session() {
    let mut session = Fixture::new("subagents-page");
    session.user("Review it");
    let parent = session.path.to_string_lossy().into_owned();
    let child = child_file(&session.path, "Rev", &parent);
    append(
        &child,
        &[
            json!({"type": "message", "id": "c1", "parentId": null, "timestamp": "2026-10-23T10:01:00.000Z", "message": {"role": "user", "content": [{"type": "text", "text": "Review the head"}], "timestamp": 1}}),
            json!({"type": "message", "id": "c2", "parentId": "c1", "timestamp": "2026-10-23T10:01:05.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "Looks good."}], "stopReason": "stop", "timestamp": 2}}),
        ],
    );
    let subagent = |id: &str| PageRequest {
        subagent: Some(id.into()),
        ..PageRequest::default()
    };
    let page = read_page(&session.path, &subagent("Rev")).unwrap();
    assert_eq!(page.session_id, "child-Rev");
    assert_eq!(kinds(&page.entries), vec!["user", "reply", "turn_end"]);
    // Its cursors page the child, not the parent.
    let after = read_page(
        &session.path,
        &PageRequest {
            after: Some(page.after.clone()),
            ..subagent("Rev")
        },
    )
    .unwrap();
    assert!(after.entries.is_empty());

    // Valid session files that name the parent, but outside its directory.
    child_file(&session.path, "../Escape", &parent);
    child_file(&session.path, "a/b", &parent);
    // In the directory, but another session's.
    child_file(&session.path, "Stray", "/elsewhere/session.jsonl");
    for id in [
        "../Escape",
        "a/b",
        "..",
        "",
        "Missing",
        "Stray",
        "Rev.jsonl",
    ] {
        let error = read_page(&session.path, &subagent(id)).unwrap_err();
        assert_eq!(error.code(), "subagent_not_found", "{id:?}");
    }
}

#[test]
fn task_step_details_and_results_name_the_subagents() {
    let names = |arguments: Value| normalize::step_detail("task", Some(&arguments));
    assert_eq!(
        names(json!({"agent": "explore", "name": "Scout", "task": "Find it"})).as_deref(),
        Some("Scout")
    );
    assert_eq!(
        names(json!({"agent": "explore", "task": "\n  Find the bug\nin detail"})).as_deref(),
        Some("Find the bug"),
        "an unnamed spawn shows its assignment's first line"
    );
    assert_eq!(
        names(json!({"context": "{\"secret\": 1}", "tasks": []})),
        None,
        "never the raw arguments"
    );
    let status = |text: &str| normalize::task_result_status(text, "Rev");
    for (envelope, expected) in [
        ("completed", Some(TranscriptSubagentStatus::Completed)),
        ("failed (exit 1)", Some(TranscriptSubagentStatus::Failed)),
        ("merge failed", Some(TranscriptSubagentStatus::Failed)),
        ("cancelled", Some(TranscriptSubagentStatus::Aborted)),
        ("parked", Some(TranscriptSubagentStatus::Unknown)),
    ] {
        let text = format!(
            "<task-result id=\"Other\" agent=\"a\" status=\"completed\">x</task-result>\n<task-result id=\"Rev\" agent=\"reviewer\" status=\"{envelope}\" duration=\"3s\">"
        );
        assert_eq!(status(&text), expected, "{envelope}");
    }
    assert_eq!(status("Background job Rev has completed."), None);
}
