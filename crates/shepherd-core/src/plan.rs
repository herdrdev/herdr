//! Plan parsing, validation, scheduling, and acceptance criteria review.

use crate::{now_rfc3339, Subtask, TaskStatus};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScheduleRound {
    pub ready: Vec<u32>,
    pub dependency_failures: Vec<(u32, u32)>,
    pub stalled: bool,
}

/// Choose the next dependency-safe subtasks, matching the Go workflow engine's runAll policy.
/// `running` contains subtask numbers already assigned to workers.
pub fn schedule_subtasks(
    subtasks: &mut [Subtask],
    running: &[u32],
    max_parallel: usize,
) -> ScheduleRound {
    let states: HashMap<u32, &str> = subtasks
        .iter()
        .map(|s| (s.number, s.state.as_str()))
        .collect();
    let running: HashSet<u32> = running.iter().copied().collect();
    let mut round = ScheduleRound::default();
    let mut pending_left = false;

    for subtask in subtasks.iter() {
        if subtask.state != "pending" || running.contains(&subtask.number) {
            continue;
        }
        pending_left = true;
        let mut failed_dependency = None;
        let mut waiting = false;
        for dependency in &subtask.depends_on {
            match states.get(dependency).copied().unwrap_or("") {
                "pr" | "done" => {}
                "needs-human" | "failed" => failed_dependency = Some(*dependency),
                _ => waiting = true,
            }
        }
        if let Some(dependency) = failed_dependency {
            round.dependency_failures.push((subtask.number, dependency));
        } else if !waiting {
            round.ready.push(subtask.number);
        }
    }

    for (number, dependency) in &round.dependency_failures {
        if let Some(subtask) = subtasks.iter_mut().find(|s| s.number == *number) {
            subtask.state = "failed".into();
            subtask.note = Some(format!("dependency {dependency} did not finish"));
        }
    }

    let capacity = max_parallel.max(1).saturating_sub(running.len());
    round.ready.truncate(capacity);
    if running.is_empty() && round.ready.is_empty() && pending_left {
        for subtask in subtasks.iter_mut().filter(|s| s.state == "pending") {
            subtask.state = "failed".into();
            subtask.note = Some("dependencies cannot make progress".into());
        }
        round.stalled = true;
    }
    round
}

pub fn aggregate_task_state(subtasks: &[Subtask], previous: &str) -> String {
    if subtasks.is_empty() {
        return previous.to_owned();
    }
    let has = |states: &[&str]| subtasks.iter().any(|s| states.contains(&s.state.as_str()));
    if has(&["coding", "fixing"]) {
        return "coding".into();
    }
    if has(&["reviewing", "checks"]) {
        return "reviewing".into();
    }
    if has(&["awaiting-approval"]) {
        return "pr".into();
    }
    if has(&["pending"]) {
        return "coding".into();
    }
    if subtasks
        .iter()
        .all(|s| matches!(s.state.as_str(), "pr" | "done"))
    {
        return "done".into();
    }
    previous.to_owned()
}

/// Prepare a persisted workflow for resume after daemon interruption.
/// Completed PRs remain complete; every other subtask returns to pending.
pub fn prepare_resume(subtasks: &mut [Subtask]) -> usize {
    let mut kept = 0;
    for subtask in subtasks {
        if matches!(subtask.state.as_str(), "pr" | "done") {
            kept += 1;
        } else {
            subtask.state = "pending".into();
            subtask.note = Some("resumed after an interruption".into());
        }
    }
    kept
}

/// Reset an interrupted persisted workflow for a future run while retaining completed work.
/// The returned count is the number of subtasks already completed as PRs or marked done.
pub fn prepare_task_resume(status: &mut TaskStatus) -> usize {
    status.waiting.clear();
    let kept = prepare_resume(&mut status.subtasks);
    status.state = aggregate_task_state(&status.subtasks, "coding");
    status.updated = now_rfc3339();
    kept
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Criterion {
    pub met: bool,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verdict {
    pub approve: bool,
    pub found: bool,
    pub criteria: Vec<Criterion>,
    pub downgraded: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    DuplicateSubtask(u32),
    DependencyCycle(u32),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSubtask(n) => write!(f, "duplicate subtask number {n}"),
            Self::DependencyCycle(n) => write!(f, "dependency cycle includes subtask {n}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// Parse the plan format used by Shepherd's plan-tasks skill.
/// A free-form plan becomes one subtask, matching the Go implementation.
pub fn parse_plan(plan: &str, fallback_title: &str) -> Vec<Subtask> {
    let lines: Vec<&str> = plan.lines().collect();
    let heads: Vec<(usize, u32, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| parse_subtask_heading(line).map(|(n, title)| (i, n, title)))
        .collect();

    if heads.is_empty() {
        return vec![Subtask::pending(1, fallback_title, plan.trim())];
    }

    let mut out = Vec::with_capacity(heads.len());
    for (index, (start, number, title)) in heads.iter().enumerate() {
        let end = heads.get(index + 1).map_or(lines.len(), |h| h.0);
        let body = lines[*start + 1..end].join("\n").trim().to_owned();
        let mut subtask = Subtask::pending(*number, title, body.clone());
        for line in body.lines() {
            if let Some(value) = field_value(line, "verify") {
                let command = strip_backticks(value.trim());
                if !command.is_empty() {
                    subtask.verify.push(command.to_owned());
                }
            }
            if let Some(value) = field_value(line, "depends on") {
                if !value.to_ascii_lowercase().contains("none") {
                    for number in digit_runs(value) {
                        if number != subtask.number {
                            subtask.depends_on.push(number);
                        }
                    }
                }
            }
        }
        out.push(subtask);
    }

    let have: HashSet<u32> = out.iter().map(|s| s.number).collect();
    for subtask in &mut out {
        subtask.depends_on.retain(|n| have.contains(n));
    }
    out
}

fn parse_subtask_heading(line: &str) -> Option<(u32, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    if !(2..=4).contains(&hashes) {
        return None;
    }
    let mut rest = trimmed[hashes..].trim_start();
    if let Some(after) = rest.strip_prefix("Subtask ") {
        rest = after;
    }
    let digit_count = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }
    let number = rest[..digit_count].parse().ok()?;
    let rest = rest[digit_count..].trim_start();
    if !matches!(rest.chars().next()?, '.' | ')' | ':') {
        return None;
    }
    let rest = rest[1..].trim_start();
    let title = rest.trim_end();
    if title.is_empty() {
        None
    } else {
        Some((number, title.to_owned()))
    }
}

pub(crate) fn field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let (key, value) = line.split_once(':')?;
    let key = key
        .trim()
        .trim_start_matches(|c: char| c == '-' || c == '*' || c == '_' || c.is_whitespace())
        .trim_end_matches(|c: char| c == '*' || c == '_' || c.is_whitespace());
    if !key.eq_ignore_ascii_case(field) {
        return None;
    }
    let mut value = value.trim();
    while let Some(stripped) = value.strip_prefix('*').or_else(|| value.strip_prefix('_')) {
        value = stripped.trim_start();
    }
    while let Some(stripped) = value
        .strip_suffix("**")
        .or_else(|| value.strip_suffix("__"))
    {
        value = stripped.trim_end();
    }
    Some(value)
}

fn strip_backticks(value: &str) -> &str {
    value
        .strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .unwrap_or(value)
        .trim()
}

fn digit_runs(value: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    for ch in value.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(n) = current.parse() {
                numbers.push(n);
            }
            current.clear();
        }
    }
    numbers
}

/// Reject duplicate identifiers and dependency cycles before a plan is run.
pub fn validate_plan(subtasks: &[Subtask]) -> Result<(), PlanError> {
    let mut by_id = HashMap::with_capacity(subtasks.len());
    for subtask in subtasks {
        if by_id.insert(subtask.number, subtask).is_some() {
            return Err(PlanError::DuplicateSubtask(subtask.number));
        }
    }
    fn visit(
        n: u32,
        by_id: &HashMap<u32, &Subtask>,
        marks: &mut HashMap<u32, u8>,
    ) -> Result<(), PlanError> {
        match marks.get(&n).copied().unwrap_or(0) {
            1 => return Err(PlanError::DependencyCycle(n)),
            2 => return Ok(()),
            _ => {}
        }
        marks.insert(n, 1);
        if let Some(subtask) = by_id.get(&n) {
            for dep in &subtask.depends_on {
                if by_id.contains_key(dep) {
                    visit(*dep, by_id, marks)?;
                }
            }
        }
        marks.insert(n, 2);
        Ok(())
    }
    let mut marks = HashMap::new();
    for n in by_id.keys() {
        visit(*n, &by_id, &mut marks)?;
    }
    Ok(())
}

/// Count acceptance checklist items, excluding the empty-task template line.
pub fn count_criteria(acceptance: &str) -> usize {
    acceptance
        .lines()
        .filter_map(parse_checklist)
        .filter(|(_, text)| !text.starts_with("(add checkable criteria"))
        .count()
}

/// Parse reviewer output and downgrade APPROVE unless every requested criterion is answered and met.
pub fn parse_review(text: &str, want_criteria: usize) -> Verdict {
    let mut verdict = Verdict {
        approve: false,
        found: false,
        criteria: Vec::new(),
        downgraded: None,
        text: text.trim().to_owned(),
    };
    let Some(value) = text.lines().find_map(|line| field_value(line, "verdict")) else {
        return verdict;
    };
    let value = value.trim_matches('*').trim().to_ascii_lowercase();
    if !(value.starts_with("approve")
        || value.starts_with("request_changes")
        || value.starts_with("request changes"))
    {
        return verdict;
    }
    verdict.found = true;
    verdict.approve = value.starts_with("approve");

    let upper = text.to_ascii_uppercase();
    let section_start = upper.find("CRITERIA").unwrap_or(0);
    let mut section_end = text.len();
    for marker in ["COMMENTS", "SUMMARY"] {
        if let Some(pos) = upper[section_start..].find(marker) {
            if pos > 0 {
                section_end = section_end.min(section_start + pos);
            }
        }
    }
    for line in text[section_start..section_end].lines() {
        if let Some((met, text)) = parse_checklist(line) {
            verdict.criteria.push(Criterion {
                met,
                text: text.to_owned(),
            });
        }
    }
    if verdict.approve && want_criteria > 0 {
        let met = verdict.criteria.iter().filter(|c| c.met).count();
        if verdict.criteria.len() < want_criteria {
            verdict.approve = false;
            verdict.downgraded = Some(format!(
                "the review answered {} of {} acceptance criteria",
                verdict.criteria.len(),
                want_criteria
            ));
        } else if met < verdict.criteria.len() {
            verdict.approve = false;
            verdict.downgraded = Some("approved with unmet criteria".into());
        }
    }
    verdict
}

fn parse_checklist(line: &str) -> Option<(bool, &str)> {
    let line = line.trim_start();
    let line = line
        .strip_prefix('-')
        .or_else(|| line.strip_prefix('*'))?
        .trim_start();
    let line = line.strip_prefix('[')?;
    let (mark, rest) = line.split_once(']')?;
    if mark.len() != 1 || !matches!(mark.as_bytes()[0], b' ' | b'x' | b'X') {
        return None;
    }
    let text = rest.trim();
    (!text.is_empty()).then_some((mark != " ", text))
}
