//! Server-owned transcript turn lifecycle. Never consulted from render loops.
mod replay;
use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::detect::AgentState;

pub(crate) const SETTLE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Closing {
    pub needs_you: usize,
    pub item_kinds: Vec<String>,
    pub parse_ok: bool,
}

/// Uses the same bundled parser as the closing-block integration, off the UI thread.
fn parse_closing(text: &str) -> Closing {
    let program = concat!(
        include_str!("integration/assets/closing-block/closing_block.py"),
        "\nimport json, sys\nb = parse(sys.stdin.read())\n",
        "print(json.dumps({'needs_you': b.declared_blocking if b.declared_blocking is not None else b.blocking, 'item_kinds': [i.label for i in b.items], 'parse_ok': b.parse_status == 'ok'}))\n"
    );
    let result = (|| -> io::Result<Closing> {
        let mut child = Command::new("python3")
            .args(["-c", program])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut input) = child.stdin.take() {
            input.write_all(text.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(io::Error::other("closing parser failed"));
        }
        serde_json::from_slice(&output.stdout).map_err(io::Error::other)
    })();
    result.unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Turn {
    pub last_at: Option<SystemTime>,
    start: Option<SystemTime>,
    last_observed: Option<SystemTime>,
    final_reply: bool,
    ask_tools: HashSet<String>,
    pending_tools: HashSet<String>,
    background: HashSet<String>,
    background_calls: HashSet<String>,
    text: String,
    closing: Option<Closing>,
    stop_rejections: usize,
    settled: bool,
    blocked: bool,
    pub next_action: Option<Value>,
    pub settled_at: Option<SystemTime>,
    pub state_at_settle: Option<AgentState>,
    pub sequence: u64,
    pub recorded: bool,
    pub projected: Option<AgentState>,
}

pub(crate) fn message_text(value: &Value) -> String {
    let content = &value["message"]["content"];
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub(crate) fn real_user(value: &Value) -> bool {
    if value["type"] != "user" || value["isMeta"] == true || value["isSidechain"] == true {
        return false;
    }
    if value["message"]["content"]
        .as_array()
        .is_some_and(|items| items.iter().any(|i| i["type"] == "tool_result"))
    {
        return false;
    }
    let text = message_text(value);
    !text.starts_with('<')
        || ![
            "<task-notification",
            "<local-command",
            "<command-",
            "<system-reminder",
        ]
        .iter()
        .any(|p| text.starts_with(p))
}

impl Turn {
    /// Replay and runtime use the same state machine; `observed` is receipt time.
    pub(crate) fn ingest(&mut self, value: &Value, at: Option<SystemTime>, observed: SystemTime) {
        // Rollout events are normalized at this boundary so replay and runtime agree.
        if value["type"] == "response_item" && value["payload"]["role"] == "assistant" {
            if let Some(items) = value["payload"]["content"].as_array() {
                self.text = items
                    .iter()
                    .filter_map(|i| i["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            return;
        }
        if value["type"] == "event_msg" {
            let payload = &value["payload"];
            let row = match payload["type"].as_str() {
                Some("user_message") => {
                    serde_json::json!({"type":"user", "message":{"content":payload["message"]}})
                }
                Some("task_started") => {
                    serde_json::json!({"type":"user", "isMeta":true, "message":{"content":""}})
                }
                Some("task_complete") => {
                    serde_json::json!({"type":"assistant", "message":{"stop_reason":"end_turn", "content":payload["last_agent_message"].as_str().unwrap_or(&self.text)}})
                }
                _ => return,
            };
            self.ingest(&row, at, observed);
            return;
        }
        if value["isSidechain"] == true
            || !matches!(value["type"].as_str(), Some("assistant" | "user"))
        {
            return;
        }
        let at = at.unwrap_or(observed);
        let text = message_text(value);
        if value["type"] == "user"
            && ["<local-command", "<command-"]
                .iter()
                .any(|prefix| text.trim_start().starts_with(prefix))
        {
            return;
        }
        let question_answer = value["message"]["content"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["type"] == "tool_result"
                    && item["tool_use_id"]
                        .as_str()
                        .is_some_and(|id| self.ask_tools.contains(id))
            })
        });
        if real_user(value) || question_answer {
            let next = self.settled_at.map(|end| serde_json::json!({
                "record": "next_action", "turn_start": self.start.and_then(crate::agent_state::format_rfc3339),
                "at": crate::agent_state::format_rfc3339(at),
                "latency_seconds": at.duration_since(end).unwrap_or_default().as_secs_f64(),
                "answer_like": answer_like(&text),
                "state_at_next_action": if self.blocked { "blocked" } else { "idle" },
                "state_at_settle": crate::detect::manifest::agent_state_label(self.state_at_settle.unwrap_or(if self.blocked { AgentState::Blocked } else { AgentState::Idle })),
            }));
            let sequence = self.sequence.saturating_add(1);
            *self = Self {
                start: Some(at),
                sequence,
                next_action: next,
                ..Self::default()
            };
        }
        self.last_at = Some(at);
        self.last_observed = Some(observed);
        // Once an ask settled, incidental output cannot clear it. Only real_user does.
        if self.blocked {
            return;
        }
        self.final_reply = false;
        self.closing = None;
        self.settled = false;
        self.settled_at = None;
        self.recorded = false;
        if value["isMeta"] == true && text.contains("Stop hook feedback") {
            self.stop_rejections += 1;
        }
        if text.contains("<task-notification") {
            // Notifications identify tasks independently of tool-use IDs.
            if let Some(id) = text
                .split_once("<task-id>")
                .and_then(|(_, tail)| tail.split_once("</task-id>"))
                .map(|(id, _)| id.trim())
            {
                self.background.remove(id);
            }
        }
        if let Some(items) = value["message"]["content"].as_array() {
            for item in items {
                if item["type"] == "tool_use" {
                    let id = item["id"].as_str().unwrap_or_default().to_owned();
                    self.pending_tools.insert(id.clone());
                    if matches!(
                        item["name"].as_str(),
                        Some("AskUserQuestion" | "ExitPlanMode")
                    ) {
                        self.ask_tools.insert(id.clone());
                    }
                    if matches!(item["name"].as_str(), Some("Agent" | "Task" | "Bash"))
                        && item["input"]["run_in_background"] == true
                    {
                        self.background_calls.insert(id);
                    }
                }
                if item["type"] == "tool_result" {
                    let id = item["tool_use_id"].as_str().unwrap_or_default();
                    self.ask_tools.remove(id);
                    self.pending_tools.remove(id);
                    let result = &value["toolUseResult"];
                    if self.background_calls.remove(id)
                        || result["isAsync"] == true
                        || result["backgroundTaskId"].is_string()
                    {
                        if let Some(task_id) = result["backgroundTaskId"]
                            .as_str()
                            .or(result["taskId"].as_str())
                            .or(result["agentId"].as_str())
                        {
                            self.background.insert(task_id.to_owned());
                        } else if item["is_error"] != true {
                            // Unknown background identity is conservative: no premature done.
                            self.background_calls.insert(id.to_owned());
                        }
                    }
                }
            }
        }
        if value["type"] == "assistant" {
            if !text.is_empty() {
                self.text = text;
            }
            self.final_reply = value["message"]["stop_reason"] == "end_turn"
                && !value["message"]["content"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|i| i["type"] == "tool_use"));
        }
    }

    pub(crate) fn state(&mut self, now: SystemTime) -> Option<AgentState> {
        if self.last_at.is_none() {
            return Some(AgentState::Idle);
        }
        if self.blocked {
            return Some(AgentState::Blocked);
        }
        if self.settled {
            return Some(AgentState::Idle);
        }
        let quiet = self
            .last_observed
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age >= SETTLE);
        if quiet
            && (self.final_reply && self.pending_tools.is_empty() || !self.ask_tools.is_empty())
            && self.background.is_empty()
            && self.background_calls.is_empty()
        {
            let closing = parse_closing(&self.text);
            self.blocked = !self.ask_tools.is_empty() || closing.needs_you > 0;
            self.closing = Some(closing);
            self.settled = true;
            self.settled_at = Some(now);
            self.state_at_settle = Some(if self.blocked {
                AgentState::Blocked
            } else {
                AgentState::Idle
            });
            return Some(if self.blocked {
                AgentState::Blocked
            } else {
                AgentState::Idle
            });
        }
        Some(AgentState::Working)
    }

    pub(crate) fn record(&self) -> Option<Value> {
        self.settled_at?;
        Some(serde_json::json!({
            "record": "settle", "turn_start": self.start.and_then(crate::agent_state::format_rfc3339),
            "turn_end": self.last_at.and_then(crate::agent_state::format_rfc3339),
            "settled_at": self.settled_at.and_then(crate::agent_state::format_rfc3339),
            "detected_kind": if self.blocked { "ask" } else { "done" },
            "signal_source": "transcript", "closing_block": self.closing,
            "state_at_settle": if self.blocked { "blocked" } else { "idle" },
            "stop_hook_rejections": self.stop_rejections,
            "closing_tail": redact_tail(&bounded_tail(&self.text)),
        }))
    }
}

fn answer_like(text: &str) -> bool {
    let word = text.trim().trim_end_matches('.');
    word.as_bytes()
        .split_last()
        .is_some_and(|(letter, digits)| {
            letter.is_ascii_alphabetic()
                && !digits.is_empty()
                && digits.iter().all(u8::is_ascii_digit)
        })
}

fn bounded_tail(text: &str) -> String {
    let tail = text
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let mut start = tail.len().saturating_sub(4096);
    while !tail.is_char_boundary(start) {
        start += 1;
    }
    tail[start..].to_owned()
}

fn redact_tail(text: &str) -> String {
    // Reuse the CLI diagnostic redactor before persisting any assistant text.
    crate::status_log::redact_tail(text)
}

pub(crate) fn ledger_path() -> PathBuf {
    crate::config::state_dir().join("turn-ledger.jsonl")
}

pub(crate) fn append(mut record: Value, pane: &str, session: &str, agent: &str) {
    record["pane"] = pane.into();
    record["session_id"] = session.into();
    record["agent"] = agent.into();
    record["host"] = serde_json::json!(crate::platform::hostname());
    if let Err(error) = append_record(&ledger_path(), &record) {
        tracing::warn!(%error, "cannot append turn ledger");
    }
}

fn append_record(path: &std::path::Path, record: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut lock = options.open(path.with_extension("lock"))?;
    lock.lock()?;
    let date = time::OffsetDateTime::now_utc().date();
    let today = date.to_string();
    let mut previous = String::new();
    lock.read_to_string(&mut previous)?;
    if previous != today {
        if !previous.is_empty() && parse_day(&previous).is_some() && path.exists() {
            std::fs::rename(
                path,
                path.with_file_name(format!("turn-ledger.{previous}.jsonl")),
            )?;
        }
        if let Some(parent) = path.parent() {
            for entry in std::fs::read_dir(parent)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(day) = name
                    .strip_prefix("turn-ledger.")
                    .and_then(|s| s.strip_suffix(".jsonl"))
                    .and_then(parse_day)
                {
                    if date - day >= time::Duration::days(30) {
                        std::fs::remove_file(entry.path())?;
                    }
                }
            }
        }
        lock.set_len(0)?;
        use std::io::Seek;
        lock.rewind()?;
        lock.write_all(today.as_bytes())?;
    }
    options.append(true);
    let mut file = options.open(path)?;
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    file.write_all(&line)
}

pub(crate) fn report(args: &[String]) -> io::Result<i32> {
    if args.first().is_some_and(|a| a == "replay") {
        return replay::run(&args[1..]);
    }
    if args.first().map(String::as_str) != Some("report") {
        eprintln!("usage: herdr turns report [--since YYYY-MM-DD]");
        return Ok(2);
    }
    let since = match &args[1..] {
        [] => "",
        [flag, value] if flag == "--since" => value,
        _ => {
            return Err(io::Error::other(
                "usage: herdr turns report [--since YYYY-MM-DD]",
            ))
        }
    };
    let path = ledger_path();
    let mut counts = std::collections::BTreeMap::<(String, String, String), usize>::new();
    let mut rows = Vec::new();
    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "turn-ledger.jsonl"
                    || name.starts_with("turn-ledger.") && name.ends_with(".jsonl")
                {
                    let file = std::fs::File::open(entry.path())?;
                    use std::io::BufRead;
                    rows.extend(
                        std::io::BufReader::new(file)
                            .lines()
                            .map_while(Result::ok)
                            .filter_map(|l| serde_json::from_str::<Value>(&l).ok()),
                    );
                }
            }
        }
    }
    let actions: std::collections::HashMap<_, _> = rows
        .iter()
        .filter(|r| r["record"] == "next_action")
        .map(|r| {
            (
                (
                    r["pane"].to_string(),
                    r["session_id"].to_string(),
                    r["turn_start"].to_string(),
                ),
                r["answer_like"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    for row in &rows {
        if row["record"] != "settle" || row["turn_end"].as_str().unwrap_or_default() < since {
            continue;
        }
        let answer = actions
            .get(&(
                row["pane"].to_string(),
                row["session_id"].to_string(),
                row["turn_start"].to_string(),
            ))
            .map(|answer| if *answer { "answer-like" } else { "prompt" })
            .unwrap_or("unobserved");
        *counts
            .entry((
                row["detected_kind"].as_str().unwrap_or("unknown").into(),
                row["state_at_settle"].as_str().unwrap_or("unknown").into(),
                answer.into(),
            ))
            .or_default() += 1;
    }
    crate::platform::begin_cli_output();
    println!("detected\tshown\tnext-action\tturns\tmisses");
    for ((kind, shown, answer), count) in counts {
        let miss = matches!(kind.as_str(), "ask" | "permission") && shown != "blocked"
            || kind == "done" && !matches!(shown.as_str(), "done" | "idle")
            || kind == "done" && answer == "answer-like";
        println!(
            "{kind}\t{shown}\t{answer}\t{count}\t{}",
            if miss { count } else { 0 }
        );
    }
    Ok(0)
}

fn parse_day(value: &str) -> Option<time::Date> {
    time::Date::parse(
        value,
        &time::format_description::well_known::Iso8601::DEFAULT,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const BASE: SystemTime = SystemTime::UNIX_EPOCH;
    fn feed(turn: &mut Turn, row: Value, second: u64) {
        turn.ingest(
            &row,
            Some(BASE + Duration::from_secs(second)),
            BASE + Duration::from_secs(second),
        );
    }
    fn user(text: &str) -> Value {
        json!({"type":"user","message":{"content":text}})
    }
    fn final_reply(text: &str) -> Value {
        json!({"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":text}]}})
    }
    fn state(turn: &mut Turn, second: u64) -> Option<AgentState> {
        turn.state(BASE + Duration::from_secs(second))
    }
    #[test]
    fn local_commands_leave_a_new_session_idle() {
        let mut turn = Turn::default();
        feed(&mut turn, user("<command-name>/clear</command-name>"), 0);
        feed(
            &mut turn,
            user("<local-command-stdout>Cleared</local-command-stdout>"),
            1,
        );
        assert_eq!(state(&mut turn, 200), Some(AgentState::Idle));
        assert!(turn.record().is_none());
    }
    #[test]
    fn codex_completion_uses_quiet_window_and_closing_latch() {
        for (text, expected) in [("Done", AgentState::Idle), ("**Needs you (1):**\n1. **Decide** Pick.\n   a) A\n   b) B\nReply 1a / 1b. Silence holds.", AgentState::Blocked)] {
            let mut turn = Turn::default();
            feed(&mut turn, json!({"type":"event_msg","payload":{"type":"user_message","message":"work"}}), 0);
            feed(&mut turn, json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":text}}), 1);
            assert_eq!(state(&mut turn, 3), Some(AgentState::Working));
            assert_eq!(state(&mut turn, 4), Some(expected));
            assert_eq!(state(&mut turn, 60), Some(expected));
            feed(&mut turn, json!({"type":"event_msg","payload":{"type":"user_message","message":"1a"}}), 61);
            assert_eq!(state(&mut turn, 62), Some(AgentState::Working));
            assert_eq!(turn.next_action.as_ref().unwrap()["state_at_settle"], if expected == AgentState::Blocked { "blocked" } else { "idle" });
        }
    }
    #[test]
    fn plain_done_settles_once_after_three_seconds() {
        let mut turn = Turn::default();
        feed(&mut turn, user("hello"), 0);
        feed(&mut turn, final_reply("Done."), 1);
        assert_eq!(state(&mut turn, 3), Some(AgentState::Working));
        assert_eq!(state(&mut turn, 4), Some(AgentState::Idle));
        let at = turn.settled_at;
        assert_eq!(state(&mut turn, 90), Some(AgentState::Idle));
        assert_eq!(at, turn.settled_at);
    }
    #[test]
    fn closing_ask_latches_across_spinner_and_releases_on_prompt() {
        let mut turn = Turn::default();
        feed(&mut turn, user("plan"), 0);
        feed(&mut turn, final_reply("**Needs you (1)**\n1. **Decide** Choose one.\nReply 1a / 1b. Silence holds.\n**Now:** Waiting."), 1);
        assert_eq!(state(&mut turn, 4), Some(AgentState::Blocked));
        feed(
            &mut turn,
            json!({"type":"assistant","message":{"content":[]}}),
            5,
        );
        assert_eq!(state(&mut turn, 20), Some(AgentState::Blocked));
        feed(&mut turn, user("1a"), 21);
        assert_eq!(turn.next_action.as_ref().unwrap()["answer_like"], true);
        assert_eq!(state(&mut turn, 25), Some(AgentState::Working));
    }
    #[test]
    fn stop_rejection_cancels_settle() {
        let mut turn = Turn::default();
        feed(&mut turn, user("work"), 0);
        feed(&mut turn, final_reply("Done"), 1);
        let mut feedback = user("Stop hook feedback: continue");
        feedback["isMeta"] = true.into();
        feed(&mut turn, feedback, 2);
        assert_eq!(state(&mut turn, 9), Some(AgentState::Working));
        assert!(turn.record().is_none());
        feed(&mut turn, final_reply("Actually done"), 10);
        assert_eq!(state(&mut turn, 13), Some(AgentState::Idle));
        assert_eq!(turn.record().unwrap()["stop_hook_rejections"], 1);
    }
    #[test]
    fn pending_question_and_interrupted_turn() {
        let mut turn = Turn::default();
        feed(&mut turn, user("choose"), 0);
        feed(
            &mut turn,
            json!({"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"ask1","name":"AskUserQuestion"}]}}),
            1,
        );
        assert_eq!(state(&mut turn, 4), Some(AgentState::Blocked));
        feed(&mut turn, user("new prompt"), 10);
        assert_eq!(state(&mut turn, 100), Some(AgentState::Working));
        assert!(turn.record().is_none());
    }
    #[test]
    fn background_agent_and_bash_hold_done_until_notification() {
        for name in ["Agent", "Bash"] {
            let mut turn = Turn::default();
            feed(&mut turn, user("work"), 0);
            feed(
                &mut turn,
                json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"launch1","name":name,"input":{"run_in_background":true}}]}}),
                1,
            );
            feed(
                &mut turn,
                json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"launch1","content":"started"}]},"toolUseResult":{"taskId":"background1"}}),
                2,
            );
            feed(&mut turn, final_reply("Launched."), 3);
            assert_eq!(state(&mut turn, 9), Some(AgentState::Working));
            feed(
                &mut turn,
                user("<task-notification><task-id>background1</task-id>done</task-notification>"),
                10,
            );
            feed(&mut turn, final_reply("Finished."), 11);
            assert_eq!(state(&mut turn, 14), Some(AgentState::Idle));
        }
    }
    #[test]
    fn question_tool_answer_is_a_real_user_action_and_background_ids_match_exactly() {
        let mut turn = Turn::default();
        feed(&mut turn, user("choose"), 0);
        feed(
            &mut turn,
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"ask1","name":"AskUserQuestion"}]}}),
            1,
        );
        assert_eq!(state(&mut turn, 4), Some(AgentState::Blocked));
        feed(
            &mut turn,
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"ask1","content":"option one"}]}}),
            5,
        );
        assert_eq!(state(&mut turn, 9), Some(AgentState::Working));
        assert!(turn.next_action.is_some());
        turn.background.insert("abc".into());
        feed(
            &mut turn,
            user("<task-notification><task-id>abcdef</task-id>abc output</task-notification>"),
            10,
        );
        feed(&mut turn, final_reply("Done"), 11);
        assert_eq!(state(&mut turn, 20), Some(AgentState::Working));
    }

    #[test]
    fn sidechain_does_not_cancel_final_and_new_session_has_no_latch() {
        let mut turn = Turn::default();
        feed(&mut turn, user("work"), 0);
        feed(&mut turn, final_reply("Done"), 1);
        let mut side = user("worker");
        side["isSidechain"] = true.into();
        feed(&mut turn, side, 3);
        assert_eq!(state(&mut turn, 4), Some(AgentState::Idle));
        turn = Turn::default();
        assert_eq!(state(&mut turn, 100), Some(AgentState::Idle));
        assert!(turn.last_at.is_none());
    }
    #[test]
    fn ledger_serializes_concurrent_writers_and_removes_expired_archives() {
        let dir = crate::status_log::test_tempdir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("turn-ledger.jsonl");
        let old = (time::OffsetDateTime::now_utc().date() - time::Duration::days(31)).to_string();
        std::fs::write(path.with_extension("lock"), &old).unwrap();
        std::fs::write(&path, "{\"record\":\"old\"}\n").unwrap();
        let workers = (0..12)
            .map(|id| {
                let path = path.clone();
                std::thread::spawn(move || {
                    append_record(&path, &json!({"record":"settle", "id":id})).unwrap()
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let records = content
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 12);
        assert!(!path
            .with_file_name(format!("turn-ledger.{old}.jsonl"))
            .exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_jsonl_fixtures() {
        for (fixture, expected) in [
            (
                include_str!("../tests/fixtures/transcript-turns/plain-done.jsonl"),
                AgentState::Idle,
            ),
            (
                include_str!("../tests/fixtures/transcript-turns/closing-ask.jsonl"),
                AgentState::Blocked,
            ),
            (
                include_str!("../tests/fixtures/transcript-turns/stop-rejection.jsonl"),
                AgentState::Working,
            ),
        ] {
            let mut turn = Turn::default();
            let mut last = BASE;
            for line in fixture.lines() {
                let row: Value = serde_json::from_str(line).unwrap();
                let seconds =
                    crate::fleet::parse_utc_timestamp(row["timestamp"].as_str().unwrap()).unwrap();
                last = BASE + Duration::from_secs(seconds);
                turn.ingest(&row, Some(last), last);
            }
            assert_eq!(turn.state(last + SETTLE), Some(expected));
        }
    }

    #[test]
    fn tail_is_bounded_and_redacted() {
        let text = format!(
            "{}\nAuthorization: Bearer private-value",
            "ü\n".repeat(8000)
        );
        let tail = redact_tail(&bounded_tail(&text));
        assert!(tail.len() <= 4096);
        assert!(tail.lines().count() <= 40);
        assert!(!tail.contains("private-value"));
    }
}
