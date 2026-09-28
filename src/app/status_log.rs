use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Instant, SystemTime};

#[cfg(not(test))]
use std::sync::mpsc::{self, SyncSender};
use std::sync::OnceLock;

use serde::Serialize;

use crate::detect::{manifest::agent_state_label, AgentState};
use crate::terminal::TerminalId;

const STATUS_CHANGE_LOG_FILE: &str = "status-changes.jsonl";
#[cfg(not(test))]
const STATUS_CHANGE_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
#[cfg(not(test))]
const STATUS_CHANGE_LOG_CHANNEL_CAPACITY: usize = 1024;
const STATUS_CHANGE_OBSERVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Serialize)]
pub(crate) struct StatusChangeRecord {
    ts: String,
    host: Option<String>,
    pane_id: String,
    terminal_id: String,
    agent: Option<String>,
    session_id: Option<String>,
    from_state: &'static str,
    to_state: &'static str,
    reason: &'static str,
    completion: Option<&'static str>,
    parse: Option<&'static str>,
    workers_unknown: Option<bool>,
    idle: Option<bool>,
}

#[derive(Debug, Default)]
pub(crate) struct StatusLogSink {
    #[cfg(not(test))]
    sender: Option<SyncSender<StatusChangeRecord>>,
}

impl StatusLogSink {
    fn try_send(&mut self, record: StatusChangeRecord) {
        #[cfg(test)]
        {
            let _ = record;
        }

        #[cfg(not(test))]
        {
            if self.sender.is_none() {
                self.sender = spawn_writer();
            }
            if let Some(sender) = self.sender.as_ref() {
                let _ = sender.try_send(record);
            }
        }
    }
}

#[cfg(not(test))]
fn spawn_writer() -> Option<SyncSender<StatusChangeRecord>> {
    let (sender, receiver) = mpsc::sync_channel(STATUS_CHANGE_LOG_CHANNEL_CAPACITY);
    let path = crate::config::state_dir().join(STATUS_CHANGE_LOG_FILE);
    let worker = std::thread::Builder::new()
        .name("herdr-status-change-log".into())
        .spawn(move || {
            for record in receiver {
                let line = status_change_line(&record);
                if let Err(error) = append_with_rotation(&path, &line, STATUS_CHANGE_LOG_MAX_BYTES)
                {
                    tracing::debug!(%error, "could not append status change log record");
                }
            }
        });
    worker.ok().map(|_| sender)
}

fn cached_hostname() -> Option<String> {
    static HOSTNAME: OnceLock<Option<String>> = OnceLock::new();
    HOSTNAME.get_or_init(crate::platform::hostname).clone()
}

fn rfc3339_now() -> String {
    crate::agent_state::format_rfc3339(SystemTime::now())
        .or_else(|| crate::agent_state::format_rfc3339(std::time::UNIX_EPOCH))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

pub(crate) fn status_change_line(record: &StatusChangeRecord) -> String {
    serde_json::to_string(record).unwrap_or_else(|_| "{}".to_string())
}

pub(crate) fn append_with_rotation(path: &Path, line: &str, max_bytes: u64) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let current_size = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    if current_size > max_bytes {
        let file_name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "status log path has no file name",
            )
        })?;
        let rotated = path.with_file_name(format!("{}.1", file_name.to_string_lossy()));
        match fs::remove_file(&rotated) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(path, rotated)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateChange {
    terminal_id: TerminalId,
    from: AgentState,
    to: AgentState,
}

fn diff_states(
    previous: &mut HashMap<TerminalId, AgentState>,
    current: impl Iterator<Item = (TerminalId, AgentState)>,
) -> Vec<StateChange> {
    let mut present = HashSet::new();
    let mut changes = Vec::new();
    for (terminal_id, state) in current {
        present.insert(terminal_id.clone());
        if let Some(from) = previous.insert(terminal_id.clone(), state) {
            if from != state {
                changes.push(StateChange {
                    terminal_id,
                    from,
                    to: state,
                });
            }
        }
    }
    previous.retain(|terminal_id, _| present.contains(terminal_id));
    changes
}

struct PaneMetadata {
    pane_id: Option<String>,
    agent: Option<String>,
    session_id: Option<String>,
    reason: &'static str,
    completion: Option<&'static str>,
    parse: Option<&'static str>,
    workers_unknown: Option<bool>,
    idle: Option<bool>,
}

struct PaneObservation {
    terminal_id: TerminalId,
    state: AgentState,
    metadata: PaneMetadata,
}

impl super::App {
    pub(crate) fn observe_status_changes(&mut self, now: Instant) {
        if self.last_status_observed.is_some_and(|last| {
            now.checked_duration_since(last)
                .is_some_and(|elapsed| elapsed < STATUS_CHANGE_OBSERVE_INTERVAL)
        }) {
            return;
        }
        self.last_status_observed = Some(now);

        let mut observations = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let terminal_id = pane.attached_terminal_id.clone();
                    let Some(terminal) = self.state.terminals.get(&terminal_id) else {
                        continue;
                    };
                    let projection = pane.agent_projection(terminal);
                    let has_pending_human_input = terminal.has_pending_human_input();
                    let tokens = terminal.closing_log_tokens();
                    observations.push(PaneObservation {
                        terminal_id,
                        state: projection.state,
                        metadata: PaneMetadata {
                            pane_id: self.public_pane_id(ws_idx, *pane_id),
                            agent: terminal.effective_agent_label().map(str::to_owned),
                            session_id: terminal.current_agent_session_id().map(str::to_owned),
                            reason: if pane.settled_at.is_some() {
                                "settled"
                            } else {
                                terminal.sidebar_projection_reason(has_pending_human_input)
                            },
                            completion: tokens.completion,
                            parse: tokens.parse,
                            workers_unknown: tokens.workers_unknown,
                            idle: tokens.idle,
                        },
                    });
                }
            }
        }

        let changes = diff_states(
            &mut self.last_status_states,
            observations
                .iter()
                .map(|observation| (observation.terminal_id.clone(), observation.state)),
        );
        let current: HashMap<_, _> = observations
            .into_iter()
            .map(|observation| (observation.terminal_id.clone(), observation))
            .collect();
        for change in changes {
            let Some(observation) = current.get(&change.terminal_id) else {
                continue;
            };
            let Some(pane_id) = observation.metadata.pane_id.as_ref() else {
                continue;
            };
            self.status_log_sink.try_send(StatusChangeRecord {
                ts: rfc3339_now(),
                host: cached_hostname(),
                pane_id: pane_id.clone(),
                terminal_id: change.terminal_id.to_string(),
                agent: observation.metadata.agent.clone(),
                session_id: observation.metadata.session_id.clone(),
                from_state: agent_state_label(change.from),
                to_state: agent_state_label(change.to),
                reason: observation.metadata.reason,
                completion: observation.metadata.completion,
                parse: observation.metadata.parse,
                workers_unknown: observation.metadata.workers_unknown,
                idle: observation.metadata.idle,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_change_line_contains_all_fields_and_an_rfc3339_timestamp() {
        let record = StatusChangeRecord {
            ts: rfc3339_now(),
            host: Some("host-a".into()),
            pane_id: "ws:p1".into(),
            terminal_id: "term_123".into(),
            agent: Some("claude".into()),
            session_id: Some("session-1".into()),
            from_state: "unknown",
            to_state: "idle",
            reason: "lifecycle",
            completion: Some("incomplete"),
            parse: Some("ok"),
            workers_unknown: Some(true),
            idle: Some(false),
        };
        let line = status_change_line(&record);
        let value: serde_json::Value = serde_json::from_str(&line).expect("JSON line");

        assert_eq!(value["host"], "host-a");
        assert_eq!(value["pane_id"], "ws:p1");
        assert_eq!(value["terminal_id"], "term_123");
        assert_eq!(value["agent"], "claude");
        assert_eq!(value["session_id"], "session-1");
        assert_eq!(value["from_state"], "unknown");
        assert_eq!(value["to_state"], "idle");
        assert_eq!(value["reason"], "lifecycle");
        assert_eq!(value["completion"], "incomplete");
        assert_eq!(value["parse"], "ok");
        assert_eq!(value["workers_unknown"].as_bool(), Some(true));
        assert_eq!(value["idle"].as_bool(), Some(false));
        let timestamp = value["ts"].as_str().expect("timestamp string");
        assert!(timestamp.contains('T'));
        assert!(timestamp.ends_with('Z'));
        assert!(timestamp.len() >= 20);
        assert!(time::OffsetDateTime::parse(
            timestamp,
            &time::format_description::well_known::Rfc3339,
        )
        .is_ok());
    }

    #[test]
    fn append_rotates_a_file_that_exceeded_the_limit_before_write() {
        let unique = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time after epoch")
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("herdr-status-log-{}-{unique}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp directory");
        let path = directory.join(STATUS_CHANGE_LOG_FILE);
        let rotated = directory.join("status-changes.jsonl.1");
        fs::write(&path, "old-current-content").expect("current log");
        fs::write(&rotated, "old-rotated-content").expect("previous rotation");

        append_with_rotation(&path, "new-line", 4).expect("append with rotation");

        assert_eq!(
            fs::read_to_string(&rotated).expect("rotated log"),
            "old-current-content"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("new current log"),
            "new-line\n"
        );
        fs::remove_dir_all(directory).expect("remove temp directory");
    }

    #[test]
    fn diff_states_ignores_first_sightings_and_reports_only_changes() {
        let first = TerminalId::alloc();
        let second = TerminalId::alloc();
        let mut previous = HashMap::new();

        let initial = diff_states(
            &mut previous,
            vec![
                (first.clone(), AgentState::Idle),
                (second.clone(), AgentState::Unknown),
            ]
            .into_iter(),
        );
        assert!(initial.is_empty());

        let changes = diff_states(
            &mut previous,
            vec![
                (first.clone(), AgentState::Working),
                (second.clone(), AgentState::Unknown),
            ]
            .into_iter(),
        );
        assert_eq!(
            changes,
            vec![StateChange {
                terminal_id: first.clone(),
                from: AgentState::Idle,
                to: AgentState::Working,
            }]
        );

        let _ = diff_states(&mut previous, std::iter::empty());
        assert!(previous.is_empty());
    }
}
