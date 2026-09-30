use serde::Deserialize;
use std::sync::Arc;

pub(crate) const MAX_RUNS_PER_HOST: usize = 40;
pub(crate) const RECENT_FINISHED_PER_HOST: usize = 3;
pub(crate) const MAX_RUN_STATE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum State {
    Active,
    Blocked,
    Done,
    Failed,
    Empty,
    // Kept for states written by older ra-wrap versions.
    Waiting,
    Unknown,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Empty => "empty",
            Self::Waiting => "waiting",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RunParent {
    #[serde(default)]
    pub(crate) host: String,
    #[serde(default)]
    pub(crate) run_id: Option<String>,
    #[serde(default)]
    pub(crate) session: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
// Parse the producer contract even when the current projection does not display
// every field. Legacy metadata is optional because current ra-wrap states omit it.
#[allow(dead_code)]
pub(crate) struct RunState {
    pub(crate) schema: u32,
    pub(crate) run_id: String,
    pub(crate) host: String,
    pub(crate) agent: String,
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) label: String,
    pub(crate) task: String,
    #[serde(default)]
    pub(crate) cwd: String,
    pub(crate) repo: String,
    pub(crate) branch: String,
    pub(crate) pid: u32,
    pub(crate) started_at: String,
    pub(crate) last_heartbeat: String,
    #[serde(default)]
    pub(crate) progress_at: Option<String>,
    pub(crate) phase: String,
    pub(crate) state: State,
    #[serde(default)]
    pub(crate) blocked_reason: Option<String>,
    #[serde(default)]
    pub(crate) blocked_since: Option<String>,
    #[serde(default)]
    pub(crate) exit_code: Option<i32>,
    #[serde(default)]
    pub(crate) exit_reason: Option<String>,
    #[serde(default)]
    pub(crate) log_path: String,
    #[serde(default)]
    pub(crate) tokens_in: Option<u64>,
    #[serde(default)]
    pub(crate) tokens_out: Option<u64>,
    #[serde(default)]
    pub(crate) cost_usd: Option<f64>,
    #[serde(default)]
    pub(crate) tool_calls: Option<u64>,
    #[serde(default)]
    pub(crate) parent: RunParent,
}

#[derive(Debug, Clone)]
pub(crate) struct Observation {
    pub(crate) state: RunState,
    pub(crate) pid_alive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayState {
    Active,
    Blocked,
    Stale,
    Done,
    Failed,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    pub(crate) host: String,
    pub(crate) run_id: String,
    pub(crate) label: String,
    pub(crate) task: String,
    pub(crate) phase: String,
    pub(crate) started_at: String,
    pub(crate) started_at_unix_s: u64,
    pub(crate) heartbeat_age_s: Option<u64>,
    pub(crate) state: DisplayState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostProjection {
    pub(crate) name: String,
    pub(crate) active_count: usize,
    pub(crate) runs: Vec<Arc<Summary>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Projection {
    pub(crate) active_count: usize,
    pub(crate) hosts: Vec<HostProjection>,
}

pub(crate) fn project(snapshot: &crate::fleet::Snapshot) -> Projection {
    let mut projection = Projection::default();
    for host in snapshot.hosts.iter().filter(|host| {
        host.state != crate::fleet::HostState::Unreachable
            || host.entries.iter().any(|row| row.run_summary().is_some())
    }) {
        let mut active = Vec::new();
        let mut stale = Vec::new();
        let mut terminal = Vec::new();
        for summary in host.entries.iter().filter_map(|row| row.run_summary()) {
            match summary.state {
                DisplayState::Active | DisplayState::Blocked => active.push(Arc::clone(summary)),
                DisplayState::Stale => stale.push(Arc::clone(summary)),
                DisplayState::Done | DisplayState::Failed => {
                    terminal.push(Arc::clone(summary));
                }
                DisplayState::Empty => continue,
            }
        }
        active.sort_by_key(|summary| std::cmp::Reverse(summary.started_at_unix_s));
        stale.sort_by_key(|summary| std::cmp::Reverse(summary.started_at_unix_s));
        terminal.sort_by(|left, right| {
            left.heartbeat_age_s
                .unwrap_or(u64::MAX)
                .cmp(&right.heartbeat_age_s.unwrap_or(u64::MAX))
                .then_with(|| right.started_at_unix_s.cmp(&left.started_at_unix_s))
        });
        terminal.truncate(RECENT_FINISHED_PER_HOST);
        let active_count = active.len();
        projection.active_count += active_count;
        active.extend(stale);
        active.extend(terminal);
        projection.hosts.push(HostProjection {
            name: host.name.clone(),
            active_count,
            runs: active,
        });
    }
    projection
}

pub(crate) fn parse_state(bytes: &[u8], source: &str) -> Result<RunState, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid run state {source}: {error}"))?;
    let schema = value.get("schema").and_then(serde_json::Value::as_u64);
    if schema != Some(1) {
        return Err(format!(
            "rejected run state {source}: schema {} is unsupported; expected 1",
            schema.map_or_else(|| "missing".into(), |value| value.to_string())
        ));
    }
    let state: RunState = serde_json::from_value(value)
        .map_err(|error| format!("invalid run state {source}: {error}"))?;
    if state.run_id.len() > 64
        || state.run_id.is_empty()
        || !state.run_id.starts_with("ra-")
        || !state
            .run_id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(format!("invalid run state {source}: invalid run_id"));
    }
    for (field, timestamp) in [
        ("started_at", Some(state.started_at.as_str())),
        ("last_heartbeat", Some(state.last_heartbeat.as_str())),
        ("progress_at", state.progress_at.as_deref()),
        ("blocked_since", state.blocked_since.as_deref()),
    ] {
        if timestamp.is_some_and(|value| crate::fleet::parse_utc_timestamp(value).is_none()) {
            return Err(format!(
                "invalid run state {source}: {field} must be RFC3339 UTC with Z"
            ));
        }
    }
    Ok(state)
}

pub(crate) fn summarize(
    host: &str,
    observation: Observation,
    now_s: u64,
    heartbeat_stale_s: u64,
) -> Summary {
    let state = observation.state;
    let heartbeat_at = crate::fleet::parse_utc_timestamp(&state.last_heartbeat);
    let heartbeat_age_s = heartbeat_at.and_then(|timestamp| now_s.checked_sub(timestamp));
    let heartbeat_fresh = heartbeat_age_s.is_some_and(|age| age <= heartbeat_stale_s);
    let live = observation.pid_alive || heartbeat_fresh;
    let display_state = match state.state {
        State::Active | State::Waiting | State::Unknown if live => DisplayState::Active,
        State::Blocked if live => DisplayState::Blocked,
        State::Active | State::Blocked | State::Waiting | State::Unknown => DisplayState::Stale,
        State::Done => DisplayState::Done,
        State::Failed => DisplayState::Failed,
        State::Empty => DisplayState::Empty,
    };
    let started_at_unix_s =
        crate::fleet::parse_utc_timestamp(&state.started_at).unwrap_or_default();
    Summary {
        host: host.to_string(),
        run_id: state.run_id,
        label: state.label,
        task: state.task,
        phase: state.phase,
        started_at: state.started_at,
        started_at_unix_s,
        heartbeat_age_s,
        state: display_state,
    }
}

pub(crate) fn log_argv(
    host: &crate::config::FleetHostConfig,
    run_id: &str,
) -> Result<Vec<String>, String> {
    if run_id.is_empty()
        || !run_id.starts_with("ra-")
        || !run_id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err("invalid run id".to_string());
    }
    let relative = format!(".agents/runs/{run_id}");
    let script = format!(
        "run_dir=\"$HOME/{relative}\"; if [ -f \"$run_dir/out.log\" ]; then tail -n 200 -f -- \"$run_dir/out.log\"; else cat \"$run_dir/state.json\"; fi"
    );
    if host.local {
        return Ok(vec!["sh".to_string(), "-lc".to_string(), script]);
    }
    if host.target.trim().is_empty() || host.target.starts_with('-') {
        return Err(format!("{} has no valid SSH target", host.name));
    }
    Ok(vec![
        "ssh".to_string(),
        "-t".to_string(),
        host.target.clone(),
        format!("sh -lc {}", crate::fleet::shell_quote(&script)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(state: &str, heartbeat: &str) -> Vec<u8> {
        format!(
            r#"{{
  "schema": 1,
  "run_id": "ra-260917-sidebar-a1b2c3d",
  "host": "ub2",
  "agent": "codex",
  "model": "gpt-5.6-sol",
  "effort": "high",
  "label": "runs sidebar",
  "task": "Add active runs per machine",
  "repo": "matthias-scale/herdr",
  "branch": "feat/ra-runs-section",
  "pid": 4242,
  "started_at": "2026-09-17T08:00:00Z",
  "last_heartbeat": "{heartbeat}",
  "progress_at": "2026-09-17T08:04:00Z",
  "phase": "implementation",
  "state": "{state}",
  "exit_code": null,
  "exit_reason": null
}}"#
        )
        .into_bytes()
    }

    #[test]
    fn current_ra_state_shape_parses() {
        let state = parse_state(
            &fixture("active", "2026-09-17T08:05:00Z"),
            "fixture/state.json",
        )
        .expect("current ra state");

        assert_eq!(state.run_id, "ra-260917-sidebar-a1b2c3d");
        assert_eq!(state.progress_at.as_deref(), Some("2026-09-17T08:04:00Z"));
        assert_eq!(state.phase, "implementation");
        assert_eq!(state.blocked_reason, None);
        assert!(state.parent.host.is_empty());
        assert_eq!(state.parent.run_id, None);
    }

    #[test]
    fn active_requires_live_pid_or_fresh_heartbeat() {
        let now =
            crate::fleet::parse_utc_timestamp("2026-09-17T09:00:00Z").expect("valid timestamp");
        let stale = Observation {
            state: parse_state(
                &fixture("active", "2026-09-17T08:00:00Z"),
                "fixture/state.json",
            )
            .expect("state"),
            pid_alive: false,
        };
        let live_pid = Observation {
            state: stale.state.clone(),
            pid_alive: true,
        };
        let fresh_heartbeat = Observation {
            state: parse_state(
                &fixture("blocked", "2026-09-17T08:59:30Z"),
                "fixture/state.json",
            )
            .expect("state"),
            pid_alive: false,
        };

        assert_eq!(summarize("ub2", stale, now, 60).state, DisplayState::Stale);
        assert_eq!(
            summarize("ub2", live_pid, now, 60).state,
            DisplayState::Active
        );
        assert_eq!(
            summarize("ub2", fresh_heartbeat, now, 60).state,
            DisplayState::Blocked
        );
    }

    #[test]
    fn live_waiting_and_unknown_runs_are_not_stale() {
        let now =
            crate::fleet::parse_utc_timestamp("2026-09-17T09:00:00Z").expect("valid timestamp");
        for state in ["waiting", "unknown"] {
            let live = Observation {
                state: parse_state(
                    &fixture(state, "2026-09-17T08:59:30Z"),
                    "fixture/state.json",
                )
                .expect("state"),
                pid_alive: false,
            };
            let stale = Observation {
                state: parse_state(
                    &fixture(state, "2026-09-17T08:00:00Z"),
                    "fixture/state.json",
                )
                .expect("state"),
                pid_alive: false,
            };

            assert_eq!(
                summarize("ub2", live, now, 60).state,
                DisplayState::Active,
                "live {state} run"
            );
            assert_eq!(
                summarize("ub2", stale, now, 60).state,
                DisplayState::Stale,
                "stale {state} run"
            );
        }
    }

    #[test]
    fn log_command_derives_path_only_from_validated_run_id() {
        let host = crate::config::FleetHostConfig {
            name: "ub2".to_string(),
            target: "ub2".to_string(),
            local: false,
            ..crate::config::FleetHostConfig::default()
        };
        let argv = log_argv(&host, "ra-260917-sidebar-a1b2c3d").expect("safe command");
        assert_eq!(&argv[..3], ["ssh", "-t", "ub2"]);
        assert!(argv[3].contains(".agents/runs/ra-260917-sidebar-a1b2c3d"));
        assert!(argv[3].contains("out.log"));
        assert!(log_argv(&host, "ra-ok; touch /tmp/pwned").is_err());
        assert!(log_argv(&host, "../out.log").is_err());
    }

    fn summary(host: &str, run_id: &str, state: DisplayState, started: u64) -> Summary {
        Summary {
            host: host.to_string(),
            run_id: run_id.to_string(),
            label: run_id.to_string(),
            task: "task".to_string(),
            phase: "verify".to_string(),
            started_at: "2026-09-17T08:00:00Z".to_string(),
            started_at_unix_s: started,
            heartbeat_age_s: Some(1),
            state,
        }
    }

    #[test]
    fn projection_groups_hosts_and_caps_recent_finished_rows() {
        let entries = [
            summary("ub2", "ra-active", DisplayState::Active, 10),
            summary("ub2", "ra-done-1", DisplayState::Done, 9),
            summary("ub2", "ra-done-2", DisplayState::Done, 8),
            summary("ub2", "ra-done-3", DisplayState::Done, 7),
            summary("ub2", "ra-done-4", DisplayState::Done, 6),
            summary("ub2", "ra-empty", DisplayState::Empty, 11),
        ]
        .into_iter()
        .map(crate::fleet::FleetRow::test_run_summary_row)
        .collect();
        let snapshot = crate::fleet::Snapshot {
            polled: true,
            hosts: vec![crate::fleet::HostSnapshot {
                name: "ub2".to_string(),
                target: "ub2".to_string(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries,
            }],
            ..crate::fleet::Snapshot::default()
        };

        let projection = project(&snapshot);
        assert_eq!(projection.active_count, 1);
        assert_eq!(projection.hosts[0].active_count, 1);
        assert_eq!(projection.hosts[0].runs.len(), 4);
        assert_eq!(projection.hosts[0].runs[0].run_id, "ra-active");
        assert_eq!(projection.hosts[0].runs[3].run_id, "ra-done-3");
        assert!(projection.hosts[0]
            .runs
            .iter()
            .all(|summary| summary.state != DisplayState::Empty));
    }

    #[test]
    fn unreachable_hosts_are_silent() {
        let snapshot = crate::fleet::Snapshot {
            polled: true,
            hosts: vec![crate::fleet::HostSnapshot {
                name: "offline".to_string(),
                target: "offline".to_string(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Unreachable,
                version: None,
                protocol: None,
                error: Some("timeout".to_string()),
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: Vec::new(),
            }],
            ..crate::fleet::Snapshot::default()
        };

        assert!(project(&snapshot).hosts.is_empty());
    }

    #[test]
    fn readable_runs_survive_an_unreachable_herdr_api() {
        let snapshot = crate::fleet::Snapshot {
            polled: true,
            hosts: vec![crate::fleet::HostSnapshot {
                name: "ub2".to_string(),
                target: "ub2".to_string(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Unreachable,
                version: None,
                protocol: None,
                error: Some("Herdr socket unavailable".to_string()),
                remote_identity: None,
                sessions: None,
                reachable: false,
                last_seen_unix_ms: None,
                entries: vec![crate::fleet::FleetRow::test_run_summary_row(summary(
                    "ub2",
                    "ra-active",
                    DisplayState::Active,
                    10,
                ))],
            }],
            ..crate::fleet::Snapshot::default()
        };

        let projection = project(&snapshot);
        assert_eq!(projection.active_count, 1);
        assert_eq!(projection.hosts[0].runs[0].run_id, "ra-active");
    }
}
