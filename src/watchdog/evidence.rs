//! Shared, deterministic evidence extraction for pane and worker watchdogs.
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static TIMERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\((?:[^()]*\d+\s*(?:ms|s|m|h)\b[^()]*|[^()]*esc to interrupt[^()]*)\)",
        r"\b\d+(?:\.\d+)?\s*[kKmM]?\s*tokens?\b",
        r"\b\d+h\s*\d+m\b|\b\d+m\s*\d+s\b|\b\d+(?:\.\d+)?\s*(?:ms|s)\b",
        r"\b\d{1,2}:\d{2}(?::\d{2})?\b",
        r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?",
        r"\b\d{1,3}(?:\.\d+)?\s*%",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static regex"))
    .collect()
});
static HEARTBEAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\[?(?:heartbeat|keepalive|still (?:running|working)|tick)\b")
        .expect("static regex")
});
static YESNO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\[(]\s*(?:y\s*/\s*n|yes\s*/\s*no)\s*[\])]").expect("static regex")
});
static OPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[❯›>▌│|]\s*)?(?:\d[.)]|\[\d\])\s+\S").expect("static regex"));
static DIALOG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:do you want to|allow (?:this|command|the)|approve|grant permission|permission required|requires approval|choose (?:a|an|the)|select (?:a|an|the))\b").expect("static regex")
});
static HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:esc to cancel|enter to (?:confirm|select)|press enter|tab to amend|\(esc\)|↑/↓)",
    )
    .expect("static regex")
});
static ACCOUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)you've hit your usage limit|usage limit reached|hit your usage limit|please (?:log ?in|run /login)|authentication failed|session expired|change (?:your )?account|upgrade your plan").expect("static regex")
});
static PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:new turn|continuing|i am continuing|i'm continuing|working on|running |reading |writing |editing |compil|step \d|resum)").expect("static regex")
});
static RETRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:retry(?:ing)?|reconnecting|backing off|backoff)\b[^\n]*?\bin\s+(\d+(?:\.\d+)?)\s*(ms|s|sec|secs|seconds?|m|min|minutes?)\b").expect("static regex")
});
static RELATIVE_AGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d+[dhms](?: \d+[hms])* ago\b").expect("static regex"));
static LOAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"🖥\d+(?:\.\d+)?").expect("static regex"));
static TOKEN_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d+(?:\.\d+)?k\b").expect("static regex"));
static QUOTA_CYCLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"↻\S+").expect("static regex"));
static QUOTA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d+h:\d+%").expect("static regex"));
static BACKGROUND_SHELLS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d+) shells?\b").expect("static regex"));
static BACKGROUND_TASKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d+) tasks?\b").expect("static regex"));
static NUMBERED_DECISION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*\d+[.)]\s+\*{0,2}(?:approve|decide)\b").expect("static regex")
});
static NEEDS_YOU: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*\*{0,2}needs you\s*\((\d+)\)\*{0,2}\s*$").expect("static regex")
});
static NEEDS_YOU_NOTHING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*\*{0,2}needs you\s*:\s*nothing\.?\s*\*{0,2}\s*$").expect("static regex")
});
static CLOSING_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*\*{0,2}(?:needs you\b|now:)\*{0,2}").expect("static regex")
});
static WAITING_ON_YOU: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*\*{0,2}now:\*{0,2}\s*waiting on you\b").expect("static regex")
});
static COMPOSER_PROMPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[❯›>]\s*(.*)$").expect("static regex"));
static USER_PROMPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*❯\s+\S").expect("static regex"));
static DONE_HERE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^done here\.?$").expect("static regex"));

fn divider(line: &str) -> bool {
    let s = line.trim();
    !s.is_empty() && s.chars().all(|c| c == '─')
}

fn composer_index(lines: &[&str]) -> Option<usize> {
    (1..lines.len().saturating_sub(1)).rev().find(|&i| {
        divider(lines[i - 1]) && COMPOSER_PROMPT.is_match(lines[i]) && divider(lines[i + 1])
    })
}

pub(crate) fn reply_text(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    composer_index(&lines).map_or_else(|| text.to_owned(), |i| lines[..i - 1].join("\n"))
}

pub(crate) fn composer_is_empty(text: &str) -> bool {
    let lines = text.lines().collect::<Vec<_>>();
    composer_index(&lines)
        .and_then(|i| COMPOSER_PROMPT.captures(lines[i]))
        .and_then(|c| c.get(1))
        .is_some_and(|s| s.as_str().trim().is_empty())
}

pub(crate) fn composer_text(text: &str) -> Option<String> {
    let lines = text.lines().collect::<Vec<_>>();
    composer_index(&lines)
        .and_then(|i| COMPOSER_PROMPT.captures(lines[i]))
        .and_then(|c| c.get(1))
        .map(|s| s.as_str().trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub(crate) fn promised_work(text: &str) -> Option<String> {
    let reply = reply_text(text);
    let mut now = None;
    for line in reply.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed
            .strip_prefix("Now:")
            .or_else(|| trimmed.strip_prefix("**Now:**"))
        {
            let value = value.trim().trim_matches('*').trim();
            if value.is_empty()
                || value.eq_ignore_ascii_case("waiting on you")
                || value.eq_ignore_ascii_case("done here.")
                || value.to_ascii_lowercase().starts_with("needs you")
            {
                now = None;
            } else {
                now = Some(value.to_owned());
            }
        }
        if NEEDS_YOU_NOTHING.is_match(trimmed) || DONE_HERE.is_match(trimmed) {
            now = None;
        }
    }
    let work = now?;
    let lower = reply.to_ascii_lowercase();
    let dead_background = lower.contains("background shell command didn't finish")
        || lower.contains("background shell command did not finish");
    (dead_background || !work.is_empty()).then_some(work)
}

pub(crate) fn expected_to_continue(text: &str) -> bool {
    if closing_block_open(text)
        || background_shell_count(text) > 0
        || background_agent_count(text) > 0
        || background_task_count(text) > 0
    {
        return false;
    }
    let reply = reply_text(text);
    reply
        .lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            let now = trimmed
                .strip_prefix("Now:")
                .or_else(|| trimmed.strip_prefix("**Now:**"))?;
            let work = now.trim().trim_matches('*').trim();
            Some(
                !work.is_empty()
                    && !work.eq_ignore_ascii_case("done here.")
                    && !work.to_ascii_lowercase().starts_with("waiting on you")
                    && !work.to_ascii_lowercase().starts_with("stopped —")
                    && !work.to_ascii_lowercase().starts_with("stopped -"),
            )
        })
        .unwrap_or(false)
}

pub(crate) fn background_task_count(text: &str) -> usize {
    let lines = text.lines().collect::<Vec<_>>();
    let Some(i) = composer_index(&lines) else {
        return 0;
    };
    lines[i + 2..]
        .iter()
        .flat_map(|line| BACKGROUND_TASKS.captures_iter(line))
        .filter_map(|c| c[1].parse::<usize>().ok())
        .max()
        .unwrap_or(0)
}

pub(crate) fn background_shell_count(text: &str) -> usize {
    let lines = text.lines().collect::<Vec<_>>();
    let Some(i) = composer_index(&lines) else {
        return 0;
    };
    lines[i + 2..]
        .iter()
        .flat_map(|line| BACKGROUND_SHELLS.captures_iter(line))
        .filter_map(|c| c[1].parse::<usize>().ok())
        .max()
        .unwrap_or(0)
}

pub(crate) fn background_agent_count(text: &str) -> usize {
    let lines = text.lines().collect::<Vec<_>>();
    let Some(i) = composer_index(&lines) else {
        return 0;
    };
    let mut in_agents_panel = false;
    let mut count = 0;
    for line in &lines[i + 2..] {
        let trimmed = line.trim_start();
        if trimmed.starts_with("● main") {
            in_agents_panel = true;
        } else if in_agents_panel && trimmed.starts_with("◯ ") {
            count += 1;
        }
    }
    count
}

pub(crate) fn closing_block_waiting(text: &str) -> bool {
    let reply = reply_text(text);
    if !composer_is_empty(text) {
        return false;
    }
    closing_block_state(&reply)
}

pub(crate) fn closing_block_open(text: &str) -> bool {
    closing_block_state(text)
}

fn closing_block_state(text: &str) -> bool {
    let mut latest = false;
    let mut block: Option<Vec<&str>> = None;
    for line in text.lines() {
        if USER_PROMPT.is_match(line) {
            if latest || block.is_some() {
                latest = false;
                block = None;
            }
            continue;
        }
        if NEEDS_YOU.is_match(line) || NEEDS_YOU_NOTHING.is_match(line) {
            block = Some(vec![line]);
            continue;
        }
        if WAITING_ON_YOU.is_match(line) && block.is_none() {
            latest = true;
            continue;
        }
        if line
            .trim()
            .eq_ignore_ascii_case("Reply 1a / 1b. Silence holds.")
            && block.is_none()
        {
            latest = true;
            continue;
        }
        if let Some(lines) = block.as_mut() {
            lines.push(line);
            if CLOSING_MARKER.is_match(line) || DONE_HERE.is_match(line.trim()) {
                latest = closing_block_lines_waiting(lines);
                block = None;
            }
        }
    }
    if let Some(lines) = block {
        latest = closing_block_lines_waiting(&lines);
    }
    latest
}

fn closing_block_lines_waiting(lines: &[&str]) -> bool {
    if lines.iter().any(|line| WAITING_ON_YOU.is_match(line))
        || lines.iter().any(|line| {
            line.trim()
                .eq_ignore_ascii_case("Reply 1a / 1b. Silence holds.")
        })
    {
        return true;
    }
    lines.iter().enumerate().any(|(i, line)| {
        NEEDS_YOU.captures(line).is_some_and(|c| {
            c[1].parse::<u32>().is_ok_and(|n| n >= 1)
                && lines[i + 1..]
                    .iter()
                    .any(|item| NUMBERED_DECISION.is_match(item))
        })
    })
}

pub(crate) fn finished_reply(text: &str) -> bool {
    if !composer_is_empty(text) {
        return false;
    }
    let reply = reply_text(text);
    let last = reply.lines().rev().find(|line| !line.trim().is_empty());
    last.is_some_and(|line| DONE_HERE.is_match(line.trim()))
        && !reply.lines().rev().skip(1).take(2).any(spinner_line)
}

fn spinner_line(line: &str) -> bool {
    line.chars().any(spinner) || PROGRESS.is_match(line.trim())
}

pub(crate) fn stable_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
fn spinner(c: char) -> bool {
    matches!(
        c,
        '\u{2800}'
            ..='\u{28ff}'
                | '✻'
                | '✶'
                | '✳'
                | '✢'
                | '✽'
                | '·'
                | '◐'
                | '◓'
                | '◑'
                | '◒'
                | '⏺'
                | '●'
                | '○'
                | '◌'
    )
}
pub(crate) fn normalize_line(line: &str) -> String {
    let mut s: String = line.chars().filter(|c| !spinner(*c)).collect();
    for p in [
        &*RELATIVE_AGE,
        &*LOAD,
        &*TOKEN_COUNT,
        &*QUOTA_CYCLE,
        &*QUOTA,
    ] {
        s = p.replace_all(&s, "").into_owned();
    }
    for p in TIMERS.iter() {
        s = p.replace_all(&s, "").into_owned();
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if HEARTBEAT.is_match(&s) {
        return String::new();
    }
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '|' | '/' | '-' | '\\' | '.' | '…'))
        .to_owned()
}
pub(crate) fn semantic_lines(text: &str) -> Vec<String> {
    let lines = text.lines().collect::<Vec<_>>();
    let content = composer_index(&lines).map_or(text.to_owned(), |i| lines[..i - 1].join("\n"));
    let v: Vec<_> = content
        .lines()
        .map(normalize_line)
        .filter(|s| !s.is_empty())
        .collect();
    v[v.len().saturating_sub(40)..].to_vec()
}
pub(crate) fn semantic_hash(text: &str) -> u64 {
    stable_hash(&semantic_lines(text).join("\n"))
}
fn bottom(text: &str, n: usize) -> Vec<String> {
    let v: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    v[v.len().saturating_sub(n)..].to_vec()
}
fn quoted(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('>')
        || s.starts_with("//")
        || ['"', '\'', '`']
            .iter()
            .any(|q| s.starts_with(*q) && s.ends_with(*q))
        || (s.starts_with('‘') && s.ends_with('’'))
        || (s.starts_with('“') && s.ends_with('”'))
        || (s.contains(": \"") && s.ends_with('"'))
        || (s.contains(": ‘") && s.ends_with('’'))
        || (s.contains(": “") && s.ends_with('”'))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptKind {
    YesNo,
    Dialog,
    AccountAction,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivePrompt {
    pub kind: PromptKind,
    pub line: String,
}
pub(crate) fn active_prompt(text: &str) -> Option<ActivePrompt> {
    let reply = reply_text(text);
    let w = bottom(&reply, 6);
    if w.is_empty() {
        return None;
    }
    let last = w.len() - 1;
    for (i, l) in w.iter().enumerate().rev() {
        if !quoted(l)
            && i + 2 > last
            && YESNO.is_match(l)
            && !w[i + 1..].iter().any(|line| PROGRESS.is_match(line))
        {
            return Some(ActivePrompt {
                kind: PromptKind::YesNo,
                line: l.clone(),
            });
        }
    }
    for (i, l) in w.iter().enumerate() {
        if quoted(l) || !DIALOG.is_match(l) {
            continue;
        }
        let rest = &w[i + 1..];
        if rest.iter().filter(|s| OPTION.is_match(s)).count() >= 2
            && rest
                .iter()
                .all(|s| OPTION.is_match(s) || HINT.is_match(s) || s.len() < 3)
        {
            return Some(ActivePrompt {
                kind: PromptKind::Dialog,
                line: l.clone(),
            });
        }
    }
    for (i, l) in w.iter().enumerate() {
        if !quoted(l) && ACCOUNT.is_match(l) && !w[i + 1..].iter().any(|s| PROGRESS.is_match(s)) {
            return Some(ActivePrompt {
                kind: PromptKind::AccountAction,
                line: l.clone(),
            });
        }
    }
    None
}
pub(crate) fn prose_question(text: &str) -> Option<String> {
    let reply = reply_text(text);
    bottom(&reply, 2).into_iter().rev().find(|l| {
        !quoted(l)
            && (l.ends_with('?')
                || [
                    "let me know",
                    "please confirm",
                    "which option",
                    "would you like",
                ]
                .iter()
                .any(|m| l.to_ascii_lowercase().contains(m)))
    })
}
pub(crate) fn scheduled_retry_secs(text: &str) -> Option<u64> {
    let reply = reply_text(text);
    bottom(&reply, 3)
        .iter()
        .rev()
        .filter(|l| !quoted(l))
        .find_map(|l| {
            let c = RETRY.captures(l)?;
            let n = c[1].parse::<f64>().ok()?;
            let unit = c[2].to_ascii_lowercase();
            Some(
                (if unit == "ms" {
                    n / 1000.
                } else if unit.starts_with('m') {
                    n * 60.
                } else {
                    n
                })
                .ceil()
                .max(1.) as u64,
            )
        })
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcSample {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub state: char,
    pub elapsed_secs: u64,
    pub cpu_ms: u64,
    pub name: String,
}
#[cfg(unix)]
fn duration(s: &str) -> Option<u64> {
    let (d, c) = match s.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, s),
    };
    let p: Vec<_> = c.split(':').collect();
    let (h, m, t): (u64, u64, &str) = match p.as_slice() {
        [m, s] => (0, m.parse().ok()?, *s),
        [h, m, s] => (h.parse().ok()?, m.parse().ok()?, *s),
        _ => return None,
    };
    Some(((d * 24 + h) * 3600 + m * 60) * 1000 + (t.parse::<f64>().ok()? * 1000.).round() as u64)
}
#[cfg(unix)]
pub(crate) fn parse_ps_rows(text: &str) -> Vec<ProcSample> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            let ppid = f.next()?.parse().ok()?;
            let pgid = f.next()?.parse().ok()?;
            let state = f.next()?.chars().next()?;
            let elapsed_secs = duration(f.next()?)? / 1000;
            // procps can wrap etime for a just-started process into an impossible duration.
            let elapsed_secs = if elapsed_secs > u32::MAX as u64 {
                0
            } else {
                elapsed_secs
            };
            let cpu_ms = duration(f.next()?)?;
            let name = f.collect::<Vec<_>>().join(" ");
            (!name.is_empty()).then_some(ProcSample {
                pid,
                ppid,
                pgid,
                state,
                elapsed_secs,
                cpu_ms,
                name,
            })
        })
        .collect()
}

/// Parse one `ps -A -o pid=,args=` snapshot for Claude parent-session liveness.
#[cfg(unix)]
pub(crate) fn parse_process_args(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|line| {
            let (pid, args) = line.trim().split_once(char::is_whitespace)?;
            Some((pid.trim().parse().ok()?, args.trim().to_owned()))
        })
        .filter(|(_, args)| {
            let executable = args.split_whitespace().next().unwrap_or_default();
            executable
                .rsplit('/')
                .next()
                .unwrap_or(executable)
                .trim_start_matches('-')
                == "claude"
        })
        .collect()
}

/// Return the latest unresolved Claude tool use, if any.
pub(crate) fn claude_pending_tool(transcript_tail: &str) -> Option<String> {
    let mut pending = std::collections::BTreeMap::<String, String>::new();
    for line in transcript_tail.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(blocks) = record
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
        {
            for block in blocks {
                if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use") {
                    if let (Some(id), Some(name)) = (
                        block.get("id").and_then(serde_json::Value::as_str),
                        block.get("name").and_then(serde_json::Value::as_str),
                    ) {
                        pending.insert(id.to_owned(), name.to_owned());
                    }
                }
            }
        }
        if let Some(blocks) = record
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
        {
            for block in blocks {
                if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result") {
                    if let Some(id) = block.get("tool_use_id").and_then(serde_json::Value::as_str) {
                        pending.remove(id);
                    }
                }
            }
        }
    }
    pending.into_values().next_back()
}
pub(crate) fn descendants(table: &[ProcSample], root: u32) -> Vec<ProcSample> {
    let mut out = Vec::new();
    let mut todo = vec![root];
    while let Some(parent) = todo.pop() {
        for p in table.iter().filter(|p| p.ppid == parent) {
            if p.pid != root && !out.iter().any(|x: &ProcSample| x.pid == p.pid) {
                todo.push(p.pid);
                out.push(p.clone())
            }
        }
    }
    out
}
fn base(name: &str) -> String {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .to_ascii_lowercase()
}
pub(crate) fn current_tool_processes(
    ps: &[ProcSample],
    exclude: Option<u32>,
    progress_age_secs: u64,
) -> Vec<&ProcSample> {
    ps.iter()
        .filter(|p| {
            Some(p.pid) != exclude
                && p.state != 'Z'
                && !matches!(
                    base(&p.name).as_str(),
                    "claude" | "codex" | "gemini" | "opencode" | "amp" | "droid"
                )
                && (!matches!(
                    base(&p.name).as_str(),
                    "sh" | "bash" | "zsh" | "dash" | "fish" | "login"
                ) || p.state == 'T')
                && p.elapsed_secs <= progress_age_secs.saturating_add(120)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalization_ignores_animation_counters_and_heartbeat() {
        assert_eq!(
            semantic_hash("Compiling A\n⠋ Working… (12s · 1.2k tokens)\nheartbeat 10s"),
            semantic_hash("Compiling A\n⠙ Working… (47s · 3.9k tokens)\nheartbeat 20s")
        );
    }
    #[test]
    fn prompt_and_history_detection() {
        assert_eq!(
            active_prompt("Running tests\nOverwrite? [y/n]")
                .unwrap()
                .kind,
            PromptKind::YesNo
        );
        assert!(active_prompt("> Do you want to continue? [y/n]\nNew turn started").is_none());
        assert_eq!(
            active_prompt("Do you want to proceed?\n❯ 1. Yes\n2. No\n(esc)")
                .unwrap()
                .kind,
            PromptKind::Dialog
        );
        assert_eq!(
            active_prompt("Usage limit reached; change account")
                .unwrap()
                .kind,
            PromptKind::AccountAction
        );
    }
    #[cfg(unix)]
    #[test]
    fn retries_and_ps_formats() {
        assert_eq!(scheduled_retry_secs("retry in 1500ms"), Some(2));
        assert_eq!(scheduled_retry_secs("backoff in 2 min"), Some(120));
        let p = parse_ps_rows(
            "101 1 101 Ss 01-02:03:04 00:00:01 bash\n202 101 101 R+ 00:07 0:00.52 cargo test",
        );
        assert_eq!(p[0].elapsed_secs, 93784);
        assert_eq!(p[1].cpu_ms, 520);
        let wrapped = parse_ps_rows("303 1 303 R 441077234-00:18:40 00:00.01 worker");
        assert_eq!(wrapped[0].elapsed_secs, 0);
    }

    #[test]
    fn quoted_questions_and_old_prompts_are_not_active() {
        assert!(active_prompt("> overwrite? [y/n]\nNew turn started").is_none());
        assert!(active_prompt("‘Should I deploy?’\nContinuing with tests").is_none());
        assert!(active_prompt("Usage limit reached\nRunning tests").is_none());
        assert_eq!(
            active_prompt("Overwrite? [Y/n]").map(|p| p.kind),
            Some(PromptKind::YesNo)
        );
    }

    #[cfg(unix)]
    #[test]
    fn tool_process_selection_excludes_agents_shells_and_old_helpers() {
        let samples = parse_ps_rows(
            "1 0 1 R 00:05 0:00.01 codex\n2 1 1 S 00:04 0:00.01 bash\n3 1 1 R 00:03 0:00.52 cargo test\n4 1 1 S 10:00 0:00.00 mcp-server\n5 1 1 Z 00:01 0:00.00 rustc",
        );
        let tools = current_tool_processes(&samples, Some(1), 10);
        assert_eq!(tools.iter().map(|p| p.pid).collect::<Vec<_>>(), [3]);
    }

    fn claude_screen(reply: &str, composer: &str, footer: &str) -> String {
        format!(
            "{reply}\n────────────────────────\n❯ {composer}\n────────────────────────\n{footer}"
        )
    }

    #[test]
    fn semantic_hash_ignores_claude_footer_churn_and_composer_input() {
        let a = claude_screen(
            "Finished the analysis.",
            "typed question one",
            "2h 48m ago │ 🖥7.47 │ 132k ↻5d08h@20:00 │ 5h:94%",
        );
        let b = claude_screen(
            "Finished the analysis.",
            "typed question two",
            "3m ago │ 🖥8.1 │ 141.2k ↻5d09h@20:00 │ 6h:91%",
        );
        assert_eq!(semantic_hash(&a), semantic_hash(&b));
    }

    #[test]
    fn closing_block_detection_uses_structure_not_heading_copy() {
        let waiting = |reply: &str| closing_block_waiting(&claude_screen(reply, "", "0 shells"));
        assert!(waiting(
            "Now: waiting on you — about 10 dashboard spot-checks"
        ));
        assert!(waiting(
            "**Now:** waiting on you — about 10 dashboard spot-checks"
        ));
        assert!(waiting(
            "**Needs you (2)**\n1. Approve release\n2. Decide on rollout"
        ));
        assert!(waiting("**Review notes**\nReply 1a / 1b. Silence holds."));
        assert!(!waiting("**Needs you: nothing.**"));
        assert!(!closing_block_waiting("Now: waiting on you"));
    }

    #[test]
    fn closing_block_stays_open_while_later_activity_continues() {
        let block = "**Needs you (1)**\n1. **Approve** — Merge X?\nReply 1a / 1b. Silence holds.\n**Now:** Codex — fixing Y";
        assert!(closing_block_waiting(&claude_screen(block, "", "0 shells")));

        let activity = (0..40)
            .map(|i| format!("tool output / notification {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(closing_block_waiting(&claude_screen(
            &format!("{block}\n{activity}"),
            "",
            "0 shells"
        )));

        assert!(!closing_block_waiting(&claude_screen(
            &format!("{block}\n❯ 1a"),
            "",
            "0 shells"
        )));
        assert!(!closing_block_waiting(&claude_screen(
            &format!("{block}\n**Needs you: nothing.**\n**Now:** Codex — reviewing"),
            "",
            "0 shells"
        )));
    }

    #[test]
    fn canonical_numbered_approval_with_now_marker_is_waiting() {
        assert!(closing_block_open(
            "**Needs you (1)**\n1. **Approve** — X?\n**Now:** Codex — Y"
        ));
    }

    #[test]
    fn latest_closing_block_supersedes_old_approval_and_user_turn() {
        let old = "**Needs you (1)**\n1. Approve — deploy\nReply 1a / 1b. Silence holds.\nNow: waiting on you";
        assert!(!closing_block_waiting(&claude_screen(
            &format!("{old}\n**Needs you: nothing.**\n**Now:** Opus sub-agent — reviewing"),
            "",
            "0 shells"
        )));
        assert!(!closing_block_waiting(&claude_screen(
            &format!("{old}\n❯ continue\nI am working on the fix"),
            "",
            "0 shells"
        )));
    }

    #[test]
    fn footer_counts_shells_and_running_agents_without_hash_churn() {
        let a = claude_screen("Waiting", "", "-- INSERT -- ⏵⏵ bypass permissions on · 1 shell · ← for agents\n● main\n◯ fork  Watching CI  5m 19s · ↓ 142.4k tokens\n◯ general-purpose  Running check");
        let b = claude_screen("Waiting", "", "-- INSERT -- ⏵⏵ bypass permissions on · 1 shell · ← for agents\n● main\n◯ fork  Watching CI  6m 20s · ↓ 150k tokens\n◯ general-purpose  Running check");
        assert_eq!(background_shell_count(&a), 1);
        assert_eq!(background_agent_count(&a), 2);
        assert_eq!(semantic_hash(&a), semantic_hash(&b));

        let hint_only = claude_screen(
            "Waiting",
            "",
            "-- INSERT -- ⏵⏵ bypass permissions on · ← 1 agent",
        );
        assert_eq!(background_agent_count(&hint_only), 0);
    }

    #[test]
    fn finished_reply_requires_an_empty_composer_and_no_progress_spinner() {
        assert!(finished_reply(&claude_screen("Done here.", "", "0 shells")));
        assert!(!finished_reply(&claude_screen(
            "Done here.",
            "follow-up",
            "0 shells"
        )));
        assert!(!finished_reply(&claude_screen(
            "Now: wait — event",
            "",
            "0 shells"
        )));
        assert_eq!(
            background_shell_count(&claude_screen("Waiting", "", "1 shell · 2 shells")),
            2
        );
    }

    #[test]
    fn claude_pending_tool_requires_matching_result() {
        let pending =
            r#"{"message":{"content":[{"type":"tool_use","id":"tool-1","name":"Bash"}]}}"#;
        assert_eq!(claude_pending_tool(pending).as_deref(), Some("Bash"));
        let completed = format!(
            "{pending}\n{}",
            r#"{"message":{"content":[{"type":"tool_result","tool_use_id":"tool-1"}]}}"#
        );
        assert_eq!(claude_pending_tool(&completed), None);
    }

    #[cfg(unix)]
    #[test]
    fn process_args_snapshot_matches_claude_session_ids() {
        let rows = parse_process_args(
            "120 claude --session-id session-live\n121 cargo test\n122 /usr/bin/claude --resume session-old",
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[0].1.contains("session-live"));
        assert!(rows[1].1.contains("session-old"));
    }
}
