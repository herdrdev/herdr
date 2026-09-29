use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::status_log::{AppendOutcome, NewRecord, ReadFilter, Source, StatusLog, TAIL_MAX_BYTES};

const USAGE: &str = "usage: herdr status-log <read|record> [options]";

pub(super) fn run_status_log_command(args: &[String]) -> io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("read") => run_read(&args[1..]),
        Some("record") => run_record(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_status_log_help();
            Ok(0)
        }
        _ => {
            print_status_log_help();
            Ok(2)
        }
    }
}

struct ReadArgs {
    filter: ReadFilter,
    dir: Option<PathBuf>,
}

fn parse_read_args(args: &[String], now: time::OffsetDateTime) -> Result<ReadArgs, String> {
    let values = parse_options(args, &["since", "pane", "limit", "dir"])?;
    let since_ms = values
        .get("since")
        .map(|value| crate::status_log::parse_since(value, now))
        .transpose()?;
    let limit = values
        .get("limit")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| format!("invalid --limit value: {value}"))
        })
        .transpose()?;
    Ok(ReadArgs {
        filter: ReadFilter {
            since_ms,
            pane: values.get("pane").cloned(),
            limit,
        },
        dir: values.get("dir").map(|value| PathBuf::from(value.as_str())),
    })
}

fn run_read(args: &[String]) -> io::Result<i32> {
    let parsed = match parse_read_args(args, time::OffsetDateTime::now_utc()) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: herdr status-log read [--since RFC3339|UNIX-SECS|AGE] [--pane ID] [--limit N] [--dir PATH]");
            return Ok(2);
        }
    };
    let log = StatusLog::new(parsed.dir.unwrap_or_else(crate::status_log::default_dir));
    let records = log.read(&parsed.filter);
    println!("{}", serde_json::to_string(&records)?);
    Ok(0)
}

#[derive(Debug, PartialEq, Eq)]
struct RecordArgs {
    pane: String,
    from_state: Option<String>,
    to_state: String,
    source: Source,
    agent: Option<String>,
    session: Option<String>,
    note: Option<String>,
    tail_file: Option<PathBuf>,
    dir: Option<PathBuf>,
}

fn parse_record_args(args: &[String]) -> Result<RecordArgs, String> {
    let values = parse_options(
        args,
        &[
            "pane",
            "from",
            "to",
            "source",
            "agent",
            "session",
            "note",
            "tail-file",
            "dir",
        ],
    )?;
    let pane = values
        .get("pane")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing required --pane ID".to_string())?
        .clone();
    let to_state = values
        .get("to")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing required --to STATE".to_string())?
        .clone();
    let source_name = values.get("source").map_or("watchdog", String::as_str);
    let source = Source::parse(source_name)
        .ok_or_else(|| format!("invalid --source value: {source_name}"))?;
    Ok(RecordArgs {
        pane,
        from_state: values.get("from").cloned(),
        to_state,
        source,
        agent: values.get("agent").cloned(),
        session: values.get("session").cloned(),
        note: values.get("note").cloned(),
        tail_file: values
            .get("tail-file")
            .map(|value| PathBuf::from(value.as_str())),
        dir: values.get("dir").map(|value| PathBuf::from(value.as_str())),
    })
}

fn run_record(args: &[String]) -> io::Result<i32> {
    let parsed = match parse_record_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: herdr status-log record --pane ID --to STATE [--from STATE] [--source watchdog|user|detector] [--agent KIND] [--session NAME] [--note TEXT] [--tail-file PATH|-] [--dir PATH]");
            return Ok(2);
        }
    };
    let (tail, input_truncated) = match parsed.tail_file.as_deref() {
        Some(path) if path == Path::new("-") => read_tail_from(&mut io::stdin().lock())?,
        Some(path) => read_tail_from(&mut File::open(path)?)?,
        None => (String::new(), false),
    };
    let session = parsed.session.or_else(|| {
        Some(
            crate::session::active_name()
                .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string()),
        )
    });
    let mut record = NewRecord {
        pane: &parsed.pane,
        session,
        agent: parsed.agent,
        host: crate::platform::hostname(),
        from_state: parsed.from_state.as_deref().unwrap_or("unknown"),
        to_state: &parsed.to_state,
        source: parsed.source,
        note: parsed.note,
        tail: &tail,
    }
    .into_record(time::OffsetDateTime::now_utc());
    record.tail_truncated |= input_truncated;
    let log = StatusLog::new(parsed.dir.unwrap_or_else(crate::status_log::default_dir));
    match log.append(&record)? {
        AppendOutcome::Written(_) => {
            println!("{}", serde_json::to_string(&record)?);
            Ok(0)
        }
        AppendOutcome::Capped => {
            eprintln!("status-log day file reached its size cap; record was not written");
            Ok(1)
        }
    }
}

fn parse_options(args: &[String], allowed: &[&str]) -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    let mut index = 0;
    while index < args.len() {
        let raw = &args[index];
        let option = raw
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument: {raw}"))?;
        let (name, inline_value) = option
            .split_once('=')
            .map_or((option, None), |(name, value)| {
                (name, Some(value.to_string()))
            });
        if !allowed.contains(&name) {
            return Err(format!("unknown option: --{name}"));
        }
        if values.contains_key(name) {
            return Err(format!("option supplied more than once: --{name}"));
        }
        let value = match inline_value {
            Some(value) => value,
            None => {
                index += 1;
                args.get(index)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| format!("missing value for --{name}"))?
                    .clone()
            }
        };
        values.insert(name.to_string(), value);
        index += 1;
    }
    Ok(values)
}

fn read_tail_from(reader: &mut impl Read) -> io::Result<(String, bool)> {
    let mut tail = Vec::with_capacity(TAIL_MAX_BYTES);
    let mut chunk = [0; 4096];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let overflow = tail
            .len()
            .saturating_add(read)
            .saturating_sub(TAIL_MAX_BYTES);
        if overflow > 0 {
            tail.drain(..overflow.min(tail.len()));
            truncated = true;
        }
        tail.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8_lossy(&tail);
    let (tail, bounded_truncated) = crate::status_log::bound_tail(&text);
    Ok((tail, truncated || bounded_truncated))
}

fn print_status_log_help() {
    eprintln!("{USAGE}");
    eprintln!("  read    Print matching local status records as a JSON array");
    eprintln!("  record  Append one local status record and print it as JSON");
    eprintln!("Run 'herdr status-log <read|record> --help' for command options.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status_log::{ReadFilter, StatusLog};

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn status_log_read_argument_parser_accepts_filters_and_rejects_bad_values() {
        let now = time::OffsetDateTime::parse(
            "2026-09-28T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let args = strings(&[
            "--since",
            "90s",
            "--pane",
            "w1:p2",
            "--limit",
            "20",
            "--dir",
            "/tmp/status-log",
        ]);
        let parsed = parse_read_args(&args, now).unwrap();
        assert_eq!(parsed.filter.pane.as_deref(), Some("w1:p2"));
        assert_eq!(parsed.filter.limit, Some(20));
        assert_eq!(parsed.dir, Some(PathBuf::from("/tmp/status-log")));
        assert!(parse_read_args(&strings(&["--limit", "-1"]), now).is_err());
        assert!(parse_read_args(&strings(&["--unknown", "value"]), now).is_err());
    }

    #[test]
    fn status_log_record_argument_parser_validates_required_fields_and_source() {
        let parsed = parse_record_args(&strings(&[
            "--pane",
            "w1:p2",
            "--to",
            "blocked",
            "--source",
            "user",
            "--note",
            "needs input",
        ]))
        .unwrap();
        assert_eq!(parsed.pane, "w1:p2");
        assert_eq!(parsed.to_state, "blocked");
        assert_eq!(parsed.source, Source::User);
        assert_eq!(parsed.note.as_deref(), Some("needs input"));
        assert_eq!(
            parse_record_args(&strings(&["--pane", "p1", "--to", "working"]))
                .unwrap()
                .source,
            Source::Watchdog
        );
        assert_eq!(
            parse_record_args(&strings(&["--pane", "p1"])).unwrap_err(),
            "missing required --to STATE"
        );
        assert!(parse_record_args(&strings(&[
            "--pane", "p1", "--to", "blocked", "--source", "other",
        ]))
        .is_err());
    }

    #[test]
    fn status_log_cli_records_and_reads_using_dir_override() {
        let dir = crate::status_log::test_tempdir();
        let record_args = strings(&[
            "record",
            "--pane",
            "w1:p3",
            "--from",
            "working",
            "--to",
            "blocked",
            "--agent",
            "codex",
            "--session",
            "test-session",
            "--note",
            "approval needed",
            "--dir",
        ]);
        let mut args = record_args;
        args.push(dir.to_string_lossy().into_owned());
        assert_eq!(run_status_log_command(&args).unwrap(), 0);

        let read_args = strings(&["read", "--pane", "w1:p3", "--dir"]);
        let mut args = read_args;
        args.push(dir.to_string_lossy().into_owned());
        assert_eq!(run_status_log_command(&args).unwrap(), 0);
        let records = StatusLog::new(dir).read(&ReadFilter {
            pane: Some("w1:p3".into()),
            ..Default::default()
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].from_state, "working");
        assert_eq!(records[0].to_state, "blocked");
        assert_eq!(records[0].session.as_deref(), Some("test-session"));
        assert_eq!(records[0].note.as_deref(), Some("approval needed"));
    }

    #[test]
    fn status_log_tail_reader_keeps_bounded_suffix_and_marks_truncation() {
        let input = "x".repeat(TAIL_MAX_BYTES + 100);
        let (tail, truncated) = read_tail_from(&mut input.as_bytes()).unwrap();
        assert!(truncated);
        assert_eq!(tail.len(), TAIL_MAX_BYTES);
    }
}
