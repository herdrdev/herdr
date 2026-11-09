//! Everything the panels ask the server to do. The server is the only writer of agents, runs and the bus;
//! the app never edits their files. Calls that carry a workspace go through `shepherd.call`, because the
//! typed `shepherd.pattern.run.start` and `shepherd.agent.spawn` have a frozen shape without one.

use crate::api::call;
use serde_json::{json, Value};
use std::path::Path;

pub struct Launched {
    pub run: Value,
    /// The pattern needs more agents than the concurrency limit: the server's confirmation was given.
    pub over_limit: bool,
}

/// Starts a pattern run in `workspace_id`. Going over `max_concurrent_agents` is a deliberate request
/// here, so it is confirmed once and reported instead of failing.
pub fn start_pattern_run(
    api: &Path,
    pattern: &str,
    objective: Option<&str>,
    root: Option<&str>,
    workspace_id: Option<&str>,
) -> Result<Launched, String> {
    let mut args = json!({ "pattern": pattern, "task": objective, "root": root, "workspace_id": workspace_id });
    let first = call(
        api,
        "shepherd.call",
        json!({ "op": "pattern_run_start", "args": args.clone() }),
    );
    match first {
        Ok(out) => Ok(Launched {
            run: out["data"].clone(),
            over_limit: false,
        }),
        Err(e) if e.contains("confirmation_required: max_concurrent_agents") => {
            args["confirm_over_limit"] = json!(true);
            let out = call(
                api,
                "shepherd.call",
                json!({ "op": "pattern_run_start", "args": args }),
            )?;
            Ok(Launched {
                run: out["data"].clone(),
                over_limit: true,
            })
        }
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunControl {
    Pause,
    Resume,
    Stop,
}

pub fn control_run(
    api: &Path,
    id: &str,
    control: RunControl,
    root: Option<&str>,
) -> Result<(), String> {
    let (method, params) = match control {
        RunControl::Pause => (
            "shepherd.pattern.run.pause",
            json!({ "id": id, "root": root }),
        ),
        RunControl::Resume => (
            "shepherd.pattern.run.resume",
            json!({ "id": id, "root": root }),
        ),
        RunControl::Stop => (
            "shepherd.pattern.run.stop",
            json!({ "id": id, "root": root, "confirm": true }),
        ),
    };
    call(api, method, params).map(|_| ())
}

/// Forgets one run, whatever its state. A live run is stopped first (which kills its agents), and the server
/// only does that with `confirm`. The agents the run leaves behind are cleaned too; returns how many.
pub fn delete_run(
    api: &Path,
    id: &str,
    root: Option<&str>,
    confirm: bool,
) -> Result<usize, String> {
    let reply = call(
        api,
        "shepherd.call",
        json!({ "op": "run_delete", "args": { "id": id, "root": root, "confirm": confirm } }),
    )?;
    let ids = run_agent_ids(&reply["data"]);
    if ids.is_empty() {
        return Ok(0);
    }
    // the run is already gone; a failure here only leaves old agents in the list
    Ok(call(api, "shepherd.agent.clean", json!({ "ids": ids }))
        .map(|r| r["cleaned"].as_array().map_or(0, Vec::len))
        .unwrap_or(0))
}

/// The ids of the agents of a run, as the server describes it.
fn run_agent_ids(run: &Value) -> Vec<String> {
    run["agents"]
        .as_array()
        .map(|agents| {
            agents
                .iter()
                .filter_map(|a| a["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Forgets finished runs and cleans the agents that are already gone; returns (runs, agents).
pub fn clean_runs(api: &Path, root: Option<&str>) -> Result<(u64, usize), String> {
    let runs = call(
        api,
        "shepherd.call",
        json!({ "op": "runs_clean", "args": { "root": root } }),
    )?["data"]["removed"]
        .as_u64()
        .unwrap_or(0);
    let agents = call(api, "shepherd.agent.clean", json!({ "ids": [] }))?["cleaned"]
        .as_array()
        .map_or(0, Vec::len);
    Ok((runs, agents))
}

/// Empties the message bus; with live agents the server asks for `force`.
pub fn clean_bus(api: &Path, force: bool) -> Result<u64, String> {
    Ok(call(
        api,
        "shepherd.call",
        json!({ "op": "bus_clean", "args": { "force": force } }),
    )?["data"]["removed"]
        .as_u64()
        .unwrap_or(0))
}

/// Creates a task; returns its number.
pub fn create_task(
    api: &Path,
    root: &str,
    title: &str,
    description: &str,
    criteria: &[String],
) -> Result<u32, String> {
    let reply = call(
        api,
        "shepherd.call",
        json!({ "op": "task_create", "args": {
            "root": root, "title": title, "description": description, "criteria": criteria } }),
    )?;
    Ok(reply["data"]["status"]["id"].as_u64().unwrap_or(0) as u32)
}

/// Changes a task in place (`spec` is left alone when `None`).
pub fn update_task(
    api: &Path,
    root: &str,
    id: u32,
    title: &str,
    description: &str,
    spec: Option<&str>,
    criteria: &[String],
) -> Result<(), String> {
    let mut args = json!({
        "root": root, "id": id.to_string(), "title": title,
        "description": description, "criteria": criteria });
    if let Some(spec) = spec {
        args["spec"] = json!(spec);
    }
    call(
        api,
        "shepherd.call",
        json!({ "op": "task_update", "args": args }),
    )
    .map(|_| ())
}

/// Deletes a task for good (the server refuses while live agents work on it).
pub fn delete_task(api: &Path, id: u32, root: &str) -> Result<(), String> {
    call(
        api,
        "shepherd.call",
        json!({ "op": "task_delete", "args": { "root": root, "id": id.to_string(), "confirm": true } }),
    )
    .map(|_| ())
}

pub fn archive_task(api: &Path, id: u32, root: Option<&str>) -> Result<(), String> {
    call(
        api,
        "shepherd.task.archive",
        json!({ "id": id.to_string(), "root": root }),
    )
    .map(|_| ())
}

/// Spawns a single agent with a Shepherd role. Returns the agent data from the server.
pub fn spawn_agent(
    api: &Path,
    role: &str,
    task: &str,
    root: Option<&str>,
    workspace_id: Option<&str>,
    vendor: Option<&str>,
    model: Option<&str>,
    worktree: bool,
    task_id: Option<u32>,
) -> Result<Value, String> {
    let mut args = json!({
        "role": role,
        "task": task,
        "worktree": worktree,
        "root": root,
        "workspace_id": workspace_id,
    });
    if let Some(id) = task_id {
        // links the agent to the task: its result closes a direct task
        args["task_id"] = json!(id.to_string());
    }
    if let Some(v) = vendor {
        args["vendor"] = json!(v);
    }
    if let Some(m) = model {
        args["model"] = json!(m);
    }
    call(api, "shepherd.call", json!({ "op": "spawn", "args": args }))
        .map(|out| out["data"].clone())
}

/// Kills an agent (if not already in a terminal state) and cleans it from the registry.
pub fn kill_and_clean_agent(api: &Path, id: &str) -> Result<(), String> {
    // Kill if still alive
    let _ = call(api, "shepherd.agent.kill", json!({ "id": id }));
    // Clean from registry
    call(api, "shepherd.agent.clean", json!({ "ids": [id] })).map(|_| ())
}

/// Resumes all idle agents.
pub fn resume_agents(api: &Path) -> Result<(), String> {
    call(api, "shepherd.agent.resume", json!({})).map(|_| ())
}

/// Assigns an agent to a project task (or detaches it). Writes the agent registry file.
pub fn assign_agent_task(
    api: &Path,
    agent_id: &str,
    task_id: Option<u32>,
    root: &str,
) -> Result<(), String> {
    // The server has no dedicated method; we edit the registry via shepherd.call
    let params = json!({
        "op": "assign_agent_task",
        "args": {
            "agentId": agent_id,
            "task": task_id.map(|id| id.to_string()),
            "root": root,
        },
    });
    call(api, "shepherd.call", params).map(|_| ())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_agents_of_a_deleted_run_are_found_in_what_the_server_returns() {
        let run = json!({"id":"run-duo-1","agents":[{"id":"specialist-1"},{"id":"specialist-2","role":"x"},{"role":"no id"}]});
        assert_eq!(run_agent_ids(&run), ["specialist-1", "specialist-2"]);
        assert!(run_agent_ids(&Value::Null).is_empty());
    }

    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    /// A server that answers each connection with the next canned reply and records the requests.
    fn fake_server(
        replies: Vec<&'static str>,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<Vec<Value>>) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("native-actions-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                seen.push(serde_json::from_str(&line).unwrap());
                writeln!(stream, "{reply}").unwrap();
            }
            seen
        });
        (path, handle)
    }

    #[test]
    fn a_pattern_over_the_agent_limit_is_confirmed_once_and_reported() {
        let (path, server) = fake_server(vec![
            r#"{"id":"1","error":{"code":"shepherd_error","message":"confirmation_required: max_concurrent_agents (4) reached (0 active + 7 new)"}}"#,
            r#"{"id":"2","result":{"type":"shepherd_call","data":{"id":"run-big-1","agents":[]}}}"#,
        ]);
        let launched =
            start_pattern_run(&path, "big", Some("Task #1: x"), Some("/p"), Some("w2")).unwrap();
        assert!(launched.over_limit);
        assert_eq!(launched.run["id"], "run-big-1");
        let seen = server.join().unwrap();
        assert_eq!(seen[0]["params"]["args"]["workspace_id"], "w2");
        assert!(seen[0]["params"]["args"]
            .get("confirm_over_limit")
            .is_none());
        assert_eq!(seen[1]["params"]["args"]["confirm_over_limit"], true);
        assert_eq!(seen[1]["params"]["args"]["pattern"], "big");
    }

    #[test]
    fn other_errors_are_not_retried() {
        let (path, server) = fake_server(vec![
            r#"{"id":"1","error":{"code":"x","message":"pattern_not_found"}}"#,
        ]);
        assert_eq!(
            start_pattern_run(&path, "nope", None, None, None)
                .err()
                .as_deref(),
            Some("pattern_not_found")
        );
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn stopping_a_run_carries_the_confirmation() {
        let (path, server) = fake_server(vec![r#"{"id":"1","result":{"type":"ok"}}"#]);
        control_run(&path, "run-1", RunControl::Stop, Some("/p")).unwrap();
        let seen = server.join().unwrap();
        assert_eq!(seen[0]["method"], "shepherd.pattern.run.stop");
        assert_eq!(seen[0]["params"]["confirm"], true);
    }
}
