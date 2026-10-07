//! Append-only agent status history.
//!
//! Every agent state transition appends one JSON line to a daily file under
//! `<state_dir>/status-log/YYYY-MM-DD.jsonl`, so blocked and done detection can
//! be tuned later from real history. Writers: the herdr server (source
//! `detector`) and external processes via `herdr status-log record`
//! (source `watchdog` by default).

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub(crate) const TAIL_MAX_LINES: usize = 40;
pub(crate) const TAIL_MAX_BYTES: usize = 8 * 1024;
pub(crate) const DAY_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const RETAINED_DAY_FILES: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Source {
    Detector,
    User,
    Watchdog,
}

impl Source {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "detector" => Some(Self::Detector),
            "user" => Some(Self::User),
            "watchdog" => Some(Self::Watchdog),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) ts: String,
    pub(crate) ts_ms: i64,
    pub(crate) pane: String,
    #[serde(default)]
    pub(crate) session: Option<String>,
    #[serde(default)]
    pub(crate) agent: Option<String>,
    #[serde(default)]
    pub(crate) host: Option<String>,
    pub(crate) from_state: String,
    pub(crate) to_state: String,
    pub(crate) source: Source,
    #[serde(default)]
    pub(crate) note: Option<String>,
    #[serde(default)]
    pub(crate) tail: String,
    #[serde(default)]
    pub(crate) tail_truncated: bool,
}

pub(crate) struct NewRecord<'a> {
    pub(crate) pane: &'a str,
    pub(crate) session: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) from_state: &'a str,
    pub(crate) to_state: &'a str,
    pub(crate) source: Source,
    pub(crate) note: Option<String>,
    pub(crate) tail: &'a str,
}

impl NewRecord<'_> {
    pub(crate) fn into_record(self, now: OffsetDateTime) -> Record {
        let (tail, tail_truncated) = bound_tail(self.tail);
        Record {
            ts: now.format(&Rfc3339).unwrap_or_default(),
            ts_ms: (now.unix_timestamp_nanos() / 1_000_000) as i64,
            pane: self.pane.to_string(),
            session: self.session,
            agent: self.agent,
            host: self.host,
            from_state: self.from_state.to_string(),
            to_state: self.to_state.to_string(),
            source: self.source,
            note: self.note,
            tail,
            tail_truncated,
        }
    }
}

/// Keep the last `TAIL_MAX_LINES` lines, then at most the last `TAIL_MAX_BYTES` bytes.
pub(crate) fn bound_tail(text: &str) -> (String, bool) {
    let trimmed = text.trim_end_matches(['\n', '\r', ' ']);
    let lines: Vec<&str> = trimmed.lines().collect();
    let mut truncated = lines.len() > TAIL_MAX_LINES;
    let start = lines.len().saturating_sub(TAIL_MAX_LINES);
    let mut tail = lines[start..].join("\n");
    if tail.len() > TAIL_MAX_BYTES {
        let mut cut = tail.len() - TAIL_MAX_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail[cut..].to_string();
        truncated = true;
    }
    (tail, truncated)
}

pub(crate) fn default_dir() -> PathBuf {
    crate::config::state_dir().join("status-log")
}

#[derive(Debug, Clone)]
pub(crate) struct StatusLog {
    dir: PathBuf,
    day_file_max_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AppendOutcome {
    Written(PathBuf),
    /// The day file reached its size cap; the record was dropped.
    Capped,
}

pub(crate) fn redact_credential_lines(stderr: &str, max_lines: usize) -> String {
    let mut safe = String::new();
    for line in stderr.lines().take(max_lines) {
        let lower = line.to_ascii_lowercase();
        if [
            "api_key",
            "access_token",
            "refresh_token",
            "authorization",
            "bearer ",
            "password",
            "secret",
            "sk-",
            "ghp_",
            "gho_",
            "github_pat_",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            safe.push_str("[redacted credential diagnostic]\n");
        } else {
            safe.push_str(line);
            safe.push('\n');
        }
    }
    safe
}

/// Bound and redact a persisted transcript tail using the diagnostic redactor.
pub(crate) fn redact_tail(text: &str) -> String {
    let tail = bound_tail(text).0;
    bound_tail(&redact_credential_lines(&tail, 40)).0
}

impl StatusLog {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            day_file_max_bytes: DAY_FILE_MAX_BYTES,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_cap(dir: PathBuf, day_file_max_bytes: u64) -> Self {
        Self {
            dir,
            day_file_max_bytes,
        }
    }

    /// The server-side writer. Tests never write to the real state directory.
    pub(crate) fn for_server() -> Option<Self> {
        if cfg!(test) || std::env::var_os("HERDR_STATUS_LOG_DISABLE").is_some() {
            return None;
        }
        Some(Self::new(default_dir()))
    }

    pub(crate) fn append(&self, record: &Record) -> io::Result<AppendOutcome> {
        fs::create_dir_all(&self.dir)?;
        let day = record.ts.get(..10).unwrap_or("unknown");
        let path = self.dir.join(format!("{day}.jsonl"));
        let mut line = serde_json::to_string(record).map_err(io::Error::other)?;
        line.push('\n');
        let (size, existed) = match fs::metadata(&path) {
            Ok(meta) => (meta.len(), true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (0, false),
            Err(error) => return Err(error),
        };
        if size.saturating_add(line.len() as u64) > self.day_file_max_bytes {
            return Ok(AppendOutcome::Capped);
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        // One append write per record keeps concurrent appends line-atomic in practice.
        file.write_all(line.as_bytes())?;
        if !existed {
            self.prune();
        }
        Ok(AppendOutcome::Written(path))
    }

    fn day_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_day_file(path))
            .collect();
        files.sort();
        files
    }

    fn prune(&self) {
        let files = self.day_files();
        let excess = files.len().saturating_sub(RETAINED_DAY_FILES);
        for path in &files[..excess] {
            let _ = fs::remove_file(path);
        }
    }

    /// Records in file order, filtered by `since_ms` and pane id. Malformed lines are skipped.
    pub(crate) fn read(&self, filter: &ReadFilter) -> Vec<Record> {
        let since_day = filter.since_ms.and_then(|ms| {
            OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
                .ok()
                .and_then(|dt| dt.format(&Rfc3339).ok())
                .and_then(|ts| ts.get(..10).map(str::to_string))
        });
        let mut out = Vec::new();
        for path in self.day_files() {
            if let (Some(since_day), Some(stem)) =
                (&since_day, path.file_stem().and_then(|s| s.to_str()))
            {
                if stem < since_day.as_str() {
                    continue;
                }
            }
            let Ok(file) = fs::File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                let Ok(record) = serde_json::from_str::<Record>(&line) else {
                    continue;
                };
                if filter.since_ms.is_some_and(|since| record.ts_ms < since) {
                    continue;
                }
                if filter
                    .pane
                    .as_deref()
                    .is_some_and(|pane| record.pane != pane)
                {
                    continue;
                }
                out.push(record);
            }
        }
        if let Some(limit) = filter.limit {
            let skip = out.len().saturating_sub(limit);
            out = out.into_iter().skip(skip).collect();
        }
        out
    }
}

fn is_day_file(path: &Path) -> bool {
    if path.extension().is_none_or(|ext| ext != "jsonl") {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let Ok(format) = time::format_description::parse_borrowed::<3>("[year]-[month]-[day]") else {
        return false;
    };
    time::Date::parse(stem, &format).is_ok()
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ReadFilter {
    pub(crate) since_ms: Option<i64>,
    pub(crate) pane: Option<String>,
    pub(crate) limit: Option<usize>,
}

/// Parse `--since`: RFC 3339, unix seconds, or a relative age like `90s`, `30m`, `2h`, `7d`.
pub(crate) fn parse_since(value: &str, now: OffsetDateTime) -> Result<i64, String> {
    if let Ok(dt) = OffsetDateTime::parse(value, &Rfc3339) {
        return i64::try_from(dt.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| format!("invalid --since value: {value}"));
    }
    if let Ok(secs) = value.parse::<i64>() {
        return secs
            .checked_mul(1000)
            .ok_or_else(|| format!("invalid --since value: {value}"));
    }
    let invalid = || format!("invalid --since value: {value}");
    if value.len() < 2 {
        return Err(invalid());
    }
    let (digits, unit) = value.split_at(value.len() - 1);
    let amount: i64 = digits.parse().map_err(|_| invalid())?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(invalid()),
    };
    let age_ms = amount
        .checked_mul(multiplier)
        .and_then(|secs| secs.checked_mul(1000))
        .ok_or_else(invalid)?;
    let now_ms = i64::try_from(now.unix_timestamp_nanos() / 1_000_000).map_err(|_| invalid())?;
    now_ms.checked_sub(age_ms).ok_or_else(invalid)
}

pub(crate) fn state_label(state: crate::detect::AgentState) -> &'static str {
    match state {
        crate::detect::AgentState::Idle => "idle",
        crate::detect::AgentState::Working => "working",
        crate::detect::AgentState::Blocked => "blocked",
        crate::detect::AgentState::Unknown => "unknown",
    }
}

/// A transition observed by the server, queued until its pane tail can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingTransition {
    pub(crate) pane_id: crate::layout::PaneId,
    pub(crate) agent: Option<crate::detect::Agent>,
    pub(crate) from_state: crate::detect::AgentState,
    pub(crate) to_state: crate::detect::AgentState,
}

#[cfg(test)]
pub(crate) fn test_tempdir() -> PathBuf {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "herdr-status-log-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ts: &str) -> OffsetDateTime {
        OffsetDateTime::parse(ts, &Rfc3339).unwrap()
    }

    fn record(pane: &str, ts: &str, tail: &str) -> Record {
        NewRecord {
            pane,
            session: Some("default".into()),
            agent: Some("claude".into()),
            host: Some("mac".into()),
            from_state: "working",
            to_state: "blocked",
            source: Source::Detector,
            note: None,
            tail,
        }
        .into_record(at(ts))
    }

    #[test]
    fn status_log_append_writes_daily_jsonl_and_read_filters() {
        let dir = test_tempdir();
        let log = StatusLog::new(dir.clone());
        log.append(&record("w1:p1", "2026-09-27T23:59:00Z", "a"))
            .unwrap();
        log.append(&record("w1:p2", "2026-09-28T10:00:00Z", "b"))
            .unwrap();
        log.append(&record("w1:p1", "2026-09-28T11:00:00Z", "c"))
            .unwrap();
        assert!(dir.join("2026-09-27.jsonl").exists());
        assert!(dir.join("2026-09-28.jsonl").exists());

        assert_eq!(log.read(&ReadFilter::default()).len(), 3);
        let pane = log.read(&ReadFilter {
            pane: Some("w1:p1".into()),
            ..Default::default()
        });
        let tails: Vec<&str> = pane.iter().map(|r| r.tail.as_str()).collect();
        assert_eq!(tails, ["a", "c"]);
        let since = parse_since("2026-09-28T10:30:00Z", at("2026-09-28T12:00:00Z")).unwrap();
        let since = log.read(&ReadFilter {
            since_ms: Some(since),
            ..Default::default()
        });
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].tail, "c");
        let limited = log.read(&ReadFilter {
            limit: Some(1),
            ..Default::default()
        });
        assert_eq!(limited[0].tail, "c");
    }

    #[test]
    fn status_log_day_file_cap_drops_records() {
        let log = StatusLog::with_cap(test_tempdir(), 400);
        let first = log
            .append(&record("p", "2026-09-28T10:00:00Z", "x"))
            .unwrap();
        assert!(matches!(first, AppendOutcome::Written(_)));
        let big = "y".repeat(500);
        assert_eq!(
            log.append(&record("p", "2026-09-28T10:00:01Z", &big))
                .unwrap(),
            AppendOutcome::Capped
        );
        assert_eq!(log.read(&ReadFilter::default()).len(), 1);
    }

    #[test]
    fn status_log_tail_is_bounded_by_lines_and_bytes() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let (tail, truncated) = bound_tail(&text);
        assert!(truncated);
        assert_eq!(tail.lines().count(), TAIL_MAX_LINES);
        assert!(tail.ends_with("line 99"));
        let wide = "é".repeat(TAIL_MAX_BYTES);
        let (tail, truncated) = bound_tail(&wide);
        assert!(truncated);
        assert!(tail.len() <= TAIL_MAX_BYTES);
        assert_eq!(bound_tail("short\n"), ("short".into(), false));
    }

    #[test]
    fn status_log_retention_prunes_oldest_day_files() {
        let dir = test_tempdir();
        let log = StatusLog::new(dir.clone());
        let start = at("2026-07-01T00:00:00Z");
        for day in 0..(RETAINED_DAY_FILES as i64 + 2) {
            let ts = (start + time::Duration::days(day))
                .format(&Rfc3339)
                .unwrap();
            log.append(&record("p", &ts, "t")).unwrap();
        }
        assert_eq!(log.day_files().len(), RETAINED_DAY_FILES);
        assert!(!dir.join("2026-07-01.jsonl").exists());
    }

    #[test]
    fn status_log_retention_ignores_non_day_jsonl_files() {
        let dir = test_tempdir();
        let log = StatusLog::new(dir.clone());
        for name in ["notes.jsonl", "2026-99-99.jsonl"] {
            fs::write(dir.join(name), "keep this file").unwrap();
        }
        for index in 0..RETAINED_DAY_FILES {
            fs::write(
                dir.join(format!("unrelated-{index}.jsonl")),
                "keep this file",
            )
            .unwrap();
        }

        log.append(&record("p", "2026-09-28T10:00:00Z", "t"))
            .unwrap();

        for name in ["notes.jsonl", "2026-99-99.jsonl"] {
            assert_eq!(
                fs::read_to_string(dir.join(name)).unwrap(),
                "keep this file"
            );
        }
        for index in 0..RETAINED_DAY_FILES {
            assert_eq!(
                fs::read_to_string(dir.join(format!("unrelated-{index}.jsonl"))).unwrap(),
                "keep this file"
            );
        }
    }

    #[test]
    fn status_log_malformed_lines_are_skipped() {
        let dir = test_tempdir();
        let log = StatusLog::new(dir.clone());
        log.append(&record("p", "2026-09-28T10:00:00Z", "ok"))
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join("2026-09-28.jsonl"))
            .unwrap();
        file.write_all(b"{not json\n").unwrap();
        assert_eq!(log.read(&ReadFilter::default()).len(), 1);
    }

    #[test]
    fn status_log_parse_since_accepts_relative_and_absolute() {
        let now = at("2026-09-28T12:00:00Z");
        let now_ms = now.unix_timestamp() * 1000;
        assert_eq!(parse_since("2h", now).unwrap(), now_ms - 7_200_000);
        assert_eq!(parse_since("30m", now).unwrap(), now_ms - 1_800_000);
        assert_eq!(parse_since("1700000000", now).unwrap(), 1_700_000_000_000);
        assert!(parse_since("soon", now).is_err());
        assert!(parse_since("h", now).is_err());
        assert!(parse_since("9223372036854775807d", now).is_err());
    }

    #[test]
    fn status_log_server_writer_is_disabled_in_tests() {
        assert!(StatusLog::for_server().is_none());
    }
}
