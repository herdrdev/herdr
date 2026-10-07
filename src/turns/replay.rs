//! Offline replay uses transcript timestamps as its virtual receipt clock.
use super::{Turn, SETTLE};
use serde_json::Value;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn files(path: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_symlink() {
                files(&entry.path(), out)?;
            }
        }
    } else if path.extension().is_some_and(|e| e == "jsonl") {
        out.push(path.to_owned());
    }
    Ok(())
}
fn rows(path: &Path) -> io::Result<(Vec<Value>, usize)> {
    let mut rows = Vec::new();
    let mut skipped = 0;
    for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
        match serde_json::from_str(&line?) {
            Ok(row) => rows.push(row),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!("skipped {skipped} malformed line(s) in {}", path.display());
    }
    Ok((rows, skipped))
}
fn timestamp(row: &Value) -> Option<SystemTime> {
    row["timestamp"]
        .as_str()
        .or(row["ts"].as_str())
        .or(row["at"].as_str())
        .and_then(crate::agent_state::parse_rfc3339)
}
fn settle(turn: &mut Turn, now: SystemTime, session: &str, records: &mut Vec<Value>) {
    turn.state(now);
    if !turn.recorded {
        if let Some(mut record) = turn.record() {
            record["session_id"] = session.into();
            records.push(record);
            turn.recorded = true;
        }
    }
}
fn replay(input: &[Value], fallback_session: &str) -> Vec<Value> {
    let mut turn = Turn::default();
    let mut session = fallback_session.to_owned();
    let mut records = Vec::new();
    let mut previous = None;
    for row in input {
        let Some(at) = timestamp(row) else { continue };
        if let Some(prior) = previous {
            if at.duration_since(prior).is_ok_and(|gap| gap >= SETTLE) {
                settle(&mut turn, prior + SETTLE, &session, &mut records);
            }
        }
        if let Some(id) = row["sessionId"].as_str().or_else(|| {
            (row["type"] == "session_meta")
                .then(|| row["payload"]["id"].as_str())
                .flatten()
        }) {
            if session != id {
                turn = Turn::default();
                session = id.to_owned();
            }
        }
        turn.ingest(row, Some(at), at);
        if let Some(mut action) = turn.next_action.take() {
            action["session_id"] = session.clone().into();
            records.push(action);
        }
        previous = Some(at);
    }
    if let Some(at) = previous {
        settle(&mut turn, at + SETTLE, &session, &mut records);
    }
    records
}

pub(super) fn run(args: &[String]) -> io::Result<i32> {
    let option = |name: &str| {
        args.windows(2)
            .find(|w| w[0] == name)
            .map(|w| w[1].clone())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("missing {name}")))
    };
    let mut paths = Vec::new();
    files(Path::new(&option("--transcripts")?), &mut paths)?;
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no JSONL transcripts",
        ));
    }
    let (mut history, mut skipped) = rows(Path::new(&option("--status-log")?))?;
    history.sort_by_key(timestamp);
    let mut counts = [(0usize, 0usize, 0usize); 2];
    let mut records = Vec::new();
    let mut sessions = std::collections::HashSet::new();
    paths.sort();
    for path in paths {
        let fallback = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        sessions.insert(fallback.to_owned());
        let (input, file_skipped) = rows(&path)?;
        skipped += file_skipped;
        for row in &input {
            if let Some(id) = row["sessionId"].as_str().or_else(|| {
                (row["type"] == "session_meta")
                    .then(|| row["payload"]["id"].as_str())
                    .flatten()
            }) {
                sessions.insert(id.to_owned());
            }
        }
        records.extend(replay(&input, fallback));
    }
    for row in &history {
        if row["session"]
            .as_str()
            .or(row["session_id"].as_str())
            .is_some_and(|id| sessions.contains(id))
        {
            if let Some(kind @ ("ask" | "done")) = row["expected_kind"].as_str() {
                counts[usize::from(kind == "done")].1 += 1;
            }
        }
    }
    let mut scored = std::collections::HashSet::new();
    crate::platform::begin_cli_output();
    println!("session\tturn-end\treplayed\tshown\texpected");
    for row in records.iter().filter(|r| r["record"] == "settle") {
        let end = row["settled_at"]
            .as_str()
            .and_then(crate::agent_state::parse_rfc3339);
        let start = row["turn_start"]
            .as_str()
            .and_then(crate::agent_state::parse_rfc3339);
        let in_turn = |h: &Value| {
            h["session"].as_str().or(h["session_id"].as_str()) == row["session_id"].as_str()
                && timestamp(h)
                    .zip(end)
                    .is_some_and(|(at, end)| at <= end && start.is_none_or(|start| at >= start))
        };
        let matched = history.iter().rev().find(|h| in_turn(h));
        let label = history.iter().enumerate().rev().find(|(_, h)| {
            in_turn(h) && matches!(h["expected_kind"].as_str(), Some("ask" | "done"))
        });
        let shown = matched
            .and_then(|h| h["to_state"].as_str())
            .unwrap_or("unobserved");
        // Labels are independent; unmatched labels remain misses in the denominator.
        let expected = label
            .and_then(|(_, h)| h["expected_kind"].as_str())
            .unwrap_or("unlabelled");
        let kind = row["detected_kind"].as_str().unwrap_or("unknown");
        counts[usize::from(kind == "done")].0 += 1;
        if let Some((index, _)) = label {
            if scored.insert(index) {
                counts[usize::from(expected == "done")].2 += usize::from(kind == expected);
            }
        }
        println!(
            "{}\t{}\t{kind}\t{shown}\t{expected}",
            row["session_id"].as_str().unwrap_or_default(),
            row["turn_end"].as_str().unwrap_or_default()
        );
    }
    for (kind, (total, labelled, correct)) in ["ask", "done"].into_iter().zip(counts) {
        let accuracy = if labelled == 0 {
            "unverified".to_owned()
        } else {
            format!("{:.2}%", correct as f64 * 100.0 / labelled as f64)
        };
        println!("{kind}: turns={total} labelled={labelled} correct={correct} accuracy={accuracy}");
    }
    println!("skipped {skipped} malformed line(s) total");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn malformed_middle_and_truncated_final_lines_preserve_settle_records() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/turns/codex-malformed.jsonl");
        let (input, skipped) = rows(&path).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(input.len(), 3);
        let records = replay(&input, "codex-done");
        let valid = include_str!("../../tests/fixtures/turns/codex-done.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect::<Vec<Value>>();
        assert_eq!(records, replay(&valid, "codex-done"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["record"], "settle");
        assert_eq!(records[0]["detected_kind"], "done");
    }
    #[test]
    fn fixture_replays_codex_and_clear_without_phantom_turns() {
        let input = |text: &str| {
            text.lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect::<Vec<Value>>()
        };
        let done = replay(
            &input(include_str!("../../tests/fixtures/turns/codex-done.jsonl")),
            "codex-done",
        );
        assert_eq!(done.len(), 1);
        assert_eq!(done[0]["detected_kind"], "done");
        let ask = replay(
            &input(include_str!("../../tests/fixtures/turns/codex-ask.jsonl")),
            "codex-ask",
        );
        assert_eq!(ask.len(), 2);
        assert_eq!(ask[0]["detected_kind"], "ask");
        assert_eq!(ask[1]["state_at_settle"], "blocked");
        let clear = replay(
            &input(include_str!(
                "../../tests/fixtures/turns/claude-clear.jsonl"
            )),
            "old",
        );
        assert_eq!(clear.len(), 1);
        assert_eq!(clear[0]["session_id"], "old");
    }
    #[test]
    fn virtual_quiet_window_cancels_stop_rejection_and_preserves_next_action_state() {
        let rows = vec![
            json!({"timestamp":"2026-10-07T00:00:00Z","type":"user","message":{"content":"work"}}),
            json!({"timestamp":"2026-10-07T00:00:01Z","type":"assistant","message":{"stop_reason":"end_turn","content":"Done"}}),
            json!({"timestamp":"2026-10-07T00:00:02Z","type":"user","isMeta":true,"message":{"content":"Stop hook feedback: continue"}}),
            json!({"timestamp":"2026-10-07T00:00:03Z","type":"assistant","message":{"stop_reason":"end_turn","content":"**Needs you (1):**\n1. **Decide** Pick.\n   a) A\n   b) B\nReply 1a / 1b. Silence holds."}}),
            json!({"timestamp":"2026-10-07T00:00:10Z","type":"user","message":{"content":"1a"}}),
        ];
        let records = replay(&rows, "session");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["detected_kind"], "ask");
        assert_eq!(records[0]["stop_hook_rejections"], 1);
        assert_eq!(records[1]["state_at_settle"], "blocked");
    }
}
