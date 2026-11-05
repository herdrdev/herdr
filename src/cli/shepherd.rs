use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::api::schema::Method;
use crate::cli::projection::ParseError;
use crate::cli::shepherd_bindings as bindings;
use crate::cli::shepherd_bindings::{LocalCommand, ShepherdOperation};
use shepherd_core::service::{OrchestrationService, ServiceConfig, SystemClock};
use shepherd_core::terminals::{connect_terminals, BackendKind, Terminals};

pub(crate) fn run_shepherd_command(args: &[String]) -> std::io::Result<i32> {
    let raw_args = if args.first().map(String::as_str) == Some("shepherd") {
        &args[1..]
    } else {
        args
    };

    let is_json = raw_args.iter().any(|a| a == "--json");
    let no_server = raw_args.iter().any(|a| a == "--no-server");

    let operation = match bindings::parse_operation(raw_args) {
        Ok(op) => op,
        Err(ParseError::Usage(msg)) => {
            if is_json {
                eprintln!("{}", serde_json::json!({ "error": msg }));
            } else {
                eprintln!("shepherd: {msg}");
            }
            return Ok(1);
        }
        Err(ParseError::Clap(err)) => {
            if is_json {
                eprintln!("{}", serde_json::json!({ "error": err.to_string().trim() }));
                return Ok(1);
            } else {
                return super::print_clap_error(&err);
            }
        }
        Err(ParseError::Io(err)) => return Err(err),
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let operation = match operation {
        ShepherdOperation::Api(mut request) => {
            fill_snapshot_root(&mut request.method, &cwd);
            ShepherdOperation::Api(request)
        }
        local => local,
    };
    let root = shepherd_core::cli::project_root(cwd);
    let config = shepherd_core::config::load_config(&root).unwrap_or_default();

    match operation {
        ShepherdOperation::Api(request) => {
            let active_session = crate::session::active_name();
            let session_name = active_session
                .as_deref()
                .or(Some(config.shepherd.session.as_str()));
            let socket_path = crate::shepherd::resolve_session_socket_path(session_name);

            let mut response_val: Option<Value> = None;

            if !no_server && socket_path.exists() {
                if let Ok(resp) = super::send_request(&request) {
                    response_val = Some(resp);
                }
            }

            let response = if let Some(val) = response_val {
                val
            } else {
                // In-process execution mode
                let in_mem = || {
                    Arc::new(shepherd_core::terminals::InMemoryTerminals::default())
                        as Arc<dyn Terminals>
                };
                let terminals: Arc<dyn Terminals> =
                    match shepherd_core::terminals::select_backend(&config) {
                        Ok(BackendKind::Hc) => in_mem(),
                        Ok(BackendKind::InProcess | BackendKind::Socket) | Err(_) => {
                            if let Some(in_proc) =
                                crate::shepherd::try_connect_local_in_process(session_name)
                            {
                                Arc::new(in_proc) as Arc<dyn Terminals>
                            } else {
                                connect_terminals(&config)
                                    .map(|b| Arc::from(b) as Arc<dyn Terminals>)
                                    .unwrap_or_else(|_| in_mem())
                            }
                        }
                    };
                let core = OrchestrationService::new(
                    ServiceConfig::default(),
                    config,
                    terminals,
                    Arc::new(SystemClock::new()),
                );
                let json_str = crate::shepherd::service::execute_shepherd_method(
                    &core,
                    &request.id,
                    request.method.clone(),
                );
                serde_json::from_str(&json_str)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
            };

            if let Some(err) = response.get("error") {
                let err_msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                if is_json {
                    eprintln!("{}", serde_json::to_string(&response).unwrap());
                } else {
                    eprintln!("shepherd: {err_msg}");
                }
                return Ok(1);
            }

            if is_json {
                println!("{}", serde_json::to_string_pretty(&response).unwrap());
            } else {
                let mut stdout = std::io::stdout();
                if let Err(e) =
                    print_human_api_response(&mut stdout, &request.method, &response["result"])
                {
                    eprintln!("shepherd: {e}");
                    return Ok(1);
                }
            }

            Ok(0)
        }
        ShepherdOperation::Local(local) => {
            let mut stdout = std::io::stdout();
            execute_local_command(local, root, &config, is_json, &mut stdout)
        }
    }
}

/// Snapshot methods act on the worktree the CLI runs in, not on the server's cwd.
fn fill_snapshot_root(method: &mut Method, cwd: &std::path::Path) {
    let root = match method {
        Method::ShepherdSnapshotCapture(p) => &mut p.root,
        Method::ShepherdSnapshotList(p) => &mut p.root,
        Method::ShepherdSnapshotShow(p) => &mut p.root,
        Method::ShepherdSnapshotDiff(p) => &mut p.root,
        Method::ShepherdSnapshotRestore(p) => &mut p.root,
        Method::ShepherdSnapshotDelete(p) => &mut p.root,
        Method::ShepherdSnapshotPrune(p) => &mut p.root,
        _ => return,
    };
    if root.is_none() {
        *root = Some(cwd.display().to_string());
    }
}

fn print_snapshot_response(
    writer: &mut dyn std::io::Write,
    method: &Method,
    result: &Value,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use shepherd_core::snapshot::render;
    fn field<T: serde::de::DeserializeOwned>(
        result: &Value,
        name: &str,
    ) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
        Ok(serde_json::from_value(result[name].clone())?)
    }
    let text = match method {
        Method::ShepherdSnapshotCapture(_) => render::capture(&field(result, "result")?),
        Method::ShepherdSnapshotList(_) => render::list(&field::<Vec<_>>(result, "snapshots")?),
        Method::ShepherdSnapshotShow(_) => render::show(&field(result, "result")?),
        Method::ShepherdSnapshotDiff(_) => render::diff(&field(result, "diff")?),
        Method::ShepherdSnapshotRestore(_) => render::restore(&field(result, "result")?),
        Method::ShepherdSnapshotDelete(_) => render::delete(&field(result, "result")?),
        Method::ShepherdSnapshotPrune(_) => render::prune(&field(result, "result")?),
        _ => return Ok(false),
    };
    write!(writer, "{text}")?;
    Ok(true)
}

fn print_human_api_response(
    writer: &mut dyn std::io::Write,
    method: &Method,
    result: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if print_snapshot_response(writer, method, result)? {
        return Ok(());
    }
    match method {
        Method::ShepherdAgentList(_) => {
            let agents = result.get("agents").and_then(Value::as_array);
            let list = agents.map(|a| a.as_slice()).unwrap_or(&[]);
            if list.is_empty() {
                writeln!(writer, "no agents")?;
                return Ok(());
            }
            writeln!(writer, "ID\tSTATE\tROLE\tVENDOR\tMODEL\tTASK")?;
            for agent in list {
                let id = agent.get("id").and_then(Value::as_str).unwrap_or("-");
                let state = agent.get("state").and_then(Value::as_str).unwrap_or("-");
                let role = agent.get("role").and_then(Value::as_str).unwrap_or("-");
                let vendor = agent.get("vendor").and_then(Value::as_str).unwrap_or("-");
                let model = agent.get("model").and_then(Value::as_str).unwrap_or("");
                let model = if model.is_empty() { "-" } else { model };
                let task = agent.get("task_text").and_then(Value::as_str).unwrap_or("");
                writeln!(writer, "{id}\t{state}\t{role}\t{vendor}\t{model}\t{task}")?;
            }
        }
        Method::ShepherdAgentSpawn(_) => {
            let agent = &result["agent"];
            let id = agent.get("id").and_then(Value::as_str).unwrap_or("-");
            let vendor = agent.get("vendor").and_then(Value::as_str).unwrap_or("-");
            let model = agent.get("model").and_then(Value::as_str).unwrap_or("");
            let model = if model.is_empty() { "default" } else { model };
            let pane = agent.get("pane").and_then(Value::as_str).unwrap_or("-");
            writeln!(
                writer,
                "started {id} ({vendor}/{model}) in Shepherd pane {pane}"
            )?;
            let worktree = agent.get("worktree").and_then(Value::as_str).unwrap_or("");
            let branch = agent.get("branch").and_then(Value::as_str).unwrap_or("");
            if !worktree.is_empty() {
                writeln!(writer, "worktree {worktree} ({branch})")?;
            }
        }
        Method::ShepherdAgentKill(_) => {
            let agent = &result["agent"];
            let id = agent.get("id").and_then(Value::as_str).unwrap_or("-");
            let state = agent.get("state").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "killed agent {id} ({state})")?;
        }
        Method::ShepherdAgentClean(_) => {
            let cleaned = result
                .get("cleaned")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            writeln!(
                writer,
                "cleaned {} agent(s): {}",
                cleaned.len(),
                cleaned.join(", ")
            )?;
        }
        Method::ShepherdAgentFocus(_) => {
            let id = result["agent"]
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            writeln!(writer, "focused agent {id} in native shepherd terminal")?;
        }
        Method::ShepherdTaskList(_) => {
            let tasks = result
                .get("tasks")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if tasks.is_empty() {
                writeln!(writer, "no tasks")?;
                return Ok(());
            }
            writeln!(writer, "ID\tSTATE\tTITLE\tISSUE\tPR")?;
            for task in tasks {
                let status = &task["status"];
                let id = status.get("id").and_then(Value::as_u64).unwrap_or(0);
                let state = status.get("state").and_then(Value::as_str).unwrap_or("-");
                let title = status.get("title").and_then(Value::as_str).unwrap_or("-");
                let issue = status
                    .get("issue")
                    .and_then(|i| i.get("number"))
                    .and_then(Value::as_u64)
                    .map(|n| format!("#{n}"))
                    .unwrap_or_else(|| "-".into());
                let pr = status.get("pr").and_then(Value::as_u64).unwrap_or(0);
                let pr_str = if pr == 0 {
                    "-".into()
                } else {
                    format!("#{pr}")
                };
                writeln!(writer, "{id}\t{state}\t{title}\t{issue}\t{pr_str}")?;
            }
        }
        Method::ShepherdTaskCreate(_) => {
            let task = &result["task"];
            let status = &task["status"];
            let id = status.get("id").and_then(Value::as_u64).unwrap_or(0);
            let slug = status.get("slug").and_then(Value::as_str).unwrap_or("");
            let dir = task.get("directory").and_then(Value::as_str).unwrap_or("");
            writeln!(writer, "created task {id}-{slug}")?;
            writeln!(writer, "{dir}")?;
        }
        Method::ShepherdTaskArchive(_) => {
            let path = result.get("path").and_then(Value::as_str).unwrap_or("");
            writeln!(writer, "archived {path}")?;
        }
        Method::ShepherdSkillList(_) => {
            let skills = result
                .get("skills")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if skills.is_empty() {
                writeln!(writer, "no skills")?;
                return Ok(());
            }
            writeln!(writer, "NAME\tTIER\tDESCRIPTION")?;
            for s in skills {
                let name = s.get("name").and_then(Value::as_str).unwrap_or("-");
                let scope = s.get("scope").and_then(Value::as_str).unwrap_or("-");
                let desc = s.get("description").and_then(Value::as_str).unwrap_or("-");
                writeln!(writer, "{name}\t{scope}\t{desc}")?;
            }
        }
        Method::ShepherdRunList(_) => {
            let runs = result
                .get("runs")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if runs.is_empty() {
                writeln!(writer, "no runs")?;
                return Ok(());
            }
            writeln!(writer, "TASK\tSTATUS\tGATE\tAGENTS\tTITLE")?;
            for r in runs {
                let task_id = r.get("task_id").and_then(Value::as_u64).unwrap_or(0);
                let status = r.get("status").and_then(Value::as_str).unwrap_or("-");
                let gate = match r.get("pending_gate") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Object(map)) => {
                        if let Some(subtask) = map.get("subtask").and_then(Value::as_u64) {
                            format!("push_pr({subtask})")
                        } else {
                            "-".into()
                        }
                    }
                    _ => "-".into(),
                };
                let agents = r
                    .get("active_agents")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_else(|| "-".into());
                let title = r.get("task_title").and_then(Value::as_str).unwrap_or("-");
                writeln!(writer, "{task_id}\t{status}\t{gate}\t{agents}\t{title}")?;
            }
        }
        Method::ShepherdRunStart(_) => {
            let job = result.get("job").and_then(Value::as_str).unwrap_or("-");
            let task = result.get("task").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "started workflow run {job} for task {task}")?;
        }
        Method::ShepherdRunApprove(_) => {
            let approved = result
                .get("approved")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let task = result.get("task").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "approved {approved} for task {task}")?;
        }
        Method::ShepherdRunReject(_) => {
            let rejected = result
                .get("rejected")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let task = result.get("task").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "rejected {rejected} for task {task}")?;
        }
        Method::ShepherdGitStatus(_) => {
            let status = &result["status"];
            let worktrees = status
                .get("worktrees")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if worktrees.is_empty() {
                writeln!(writer, "no worktrees found")?;
                return Ok(());
            }
            writeln!(writer, "BRANCH\tSTATUS\tSYNC\tAGENT\tPR\tPATH")?;
            for wt in worktrees {
                let branch = wt.get("branch").and_then(Value::as_str).unwrap_or("-");
                let is_clean = wt.get("is_clean").and_then(Value::as_bool).unwrap_or(true);
                let dirty_count = wt.get("dirty_count").and_then(Value::as_u64).unwrap_or(0);
                let status_str = if is_clean {
                    "clean".to_string()
                } else {
                    format!("dirty ({dirty_count})")
                };
                let ahead = wt.get("ahead").and_then(Value::as_u64).unwrap_or(0);
                let behind = wt.get("behind").and_then(Value::as_u64).unwrap_or(0);
                let sync = if ahead == 0 && behind == 0 {
                    "up to date".to_string()
                } else {
                    format!("+{ahead}/-{behind}")
                };
                let agent = wt.get("agent_id").and_then(Value::as_str).unwrap_or("-");
                let pr = wt
                    .get("pr")
                    .and_then(|p| {
                        let num = p.get("number")?.as_u64()?;
                        let st = p.get("state")?.as_str()?;
                        Some(format!("#{num} {st}"))
                    })
                    .unwrap_or_else(|| "—".to_string());
                let path = wt.get("path").and_then(Value::as_str).unwrap_or("-");
                writeln!(
                    writer,
                    "{branch}\t{status_str}\t{sync}\t{agent}\t{pr}\t{path}"
                )?;
            }
            if let Some(note) = status.get("gh_note").and_then(Value::as_str) {
                writeln!(writer, "\nNote: {note}")?;
            }
        }
        Method::ShepherdPatternList(_) => {
            let patterns = result
                .get("patterns")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if patterns.is_empty() {
                writeln!(writer, "no patterns found")?;
                return Ok(());
            }
            writeln!(
                writer,
                "NAME\tKIND\tSOURCE\tSHADOWED\tAGENTS\tTOPICS\tDESCRIPTION"
            )?;
            for p in patterns {
                let name = p.get("name").and_then(Value::as_str).unwrap_or("-");
                let kind = p.get("kind").and_then(Value::as_str).unwrap_or("-");
                let source = p.get("source").and_then(Value::as_str).unwrap_or("-");
                let shadowed = if p.get("shadowed").and_then(Value::as_bool).unwrap_or(false) {
                    "yes"
                } else {
                    "no"
                };
                let agents = p.get("agents_count").and_then(Value::as_u64).unwrap_or(0);
                let topics = p.get("topics_count").and_then(Value::as_u64).unwrap_or(0);
                let desc = p.get("description").and_then(Value::as_str).unwrap_or("-");
                writeln!(
                    writer,
                    "{name}\t{kind}\t{source}\t{shadowed}\t{agents}\t{topics}\t{desc}"
                )?;
            }
        }
        Method::ShepherdPatternGet(_) => {
            let pattern = &result["pattern"];
            let name = pattern.get("name").and_then(Value::as_str).unwrap_or("-");
            let kind = pattern.get("kind").and_then(Value::as_str).unwrap_or("-");
            let source = pattern.get("source").and_then(Value::as_str).unwrap_or("-");
            let desc = pattern
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("-");
            writeln!(writer, "Name:        {name}")?;
            writeln!(writer, "Kind:        {kind}")?;
            writeln!(writer, "Source:      {source}")?;
            writeln!(writer, "Description: {desc}")?;
            if let Some(path) = pattern.get("path").and_then(Value::as_str) {
                writeln!(writer, "Path:        {path}")?;
            }
            if let Some(agents) = pattern.get("agents").and_then(Value::as_array) {
                writeln!(writer, "\nAgents ({}):", agents.len())?;
                for a in agents {
                    let id = a.get("id").and_then(Value::as_str).unwrap_or("-");
                    let role = a.get("role").and_then(Value::as_str).unwrap_or("-");
                    writeln!(writer, "  - {id} ({role})")?;
                }
            }
            if let Some(topics) = pattern.get("topics").and_then(Value::as_array) {
                writeln!(writer, "\nTopics ({}):", topics.len())?;
                for t in topics {
                    let name = t.get("name").and_then(Value::as_str).unwrap_or("-");
                    let p = t.get("partitions").and_then(Value::as_u64).unwrap_or(1);
                    writeln!(writer, "  - {name} ({p} partitions)")?;
                }
            }
        }
        Method::ShepherdPatternSave(_) => {
            let name = result.get("name").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "saved pattern {name}")?;
        }
        Method::ShepherdPatternDelete(_) => {
            let name = result.get("name").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "deleted pattern {name}")?;
        }
        Method::ShepherdPatternValidate(_) => {
            let valid = result
                .get("valid")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if valid {
                writeln!(writer, "pattern is valid")?;
            } else {
                writeln!(writer, "pattern has validation problems:")?;
                if let Some(problems) = result.get("problems").and_then(Value::as_array) {
                    for p in problems {
                        let sev = p.get("severity").and_then(Value::as_str).unwrap_or("error");
                        let path = p.get("path").and_then(Value::as_str).unwrap_or("-");
                        let msg = p.get("message").and_then(Value::as_str).unwrap_or("");
                        writeln!(writer, "  [{sev}] {path}: {msg}")?;
                    }
                }
            }
        }
        Method::ShepherdPatternRunStart(_) => {
            let run = &result["run"];
            let id = run.get("id").and_then(Value::as_str).unwrap_or("-");
            let pattern = run.get("pattern").and_then(Value::as_str).unwrap_or("-");
            let state = run.get("state").and_then(Value::as_str).unwrap_or("-");
            let agents_count = run
                .get("agents")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            writeln!(
                writer,
                "started pattern run {id} for pattern {pattern} ({state}) with {agents_count} agents"
            )?;
        }
        Method::ShepherdPatternRunList(_) => {
            let runs = result
                .get("runs")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if runs.is_empty() {
                writeln!(writer, "no pattern runs found")?;
                return Ok(());
            }
            writeln!(
                writer,
                "ID\tPATTERN\tKIND\tSTATE\tAGENTS\tLIVE\tTOPICS\tCREATED"
            )?;
            for r in runs {
                let id = r.get("id").and_then(Value::as_str).unwrap_or("-");
                let pattern = r.get("pattern").and_then(Value::as_str).unwrap_or("-");
                let kind = r.get("kind").and_then(Value::as_str).unwrap_or("-");
                let state = r.get("state").and_then(Value::as_str).unwrap_or("-");
                let agents = r.get("agents_count").and_then(Value::as_u64).unwrap_or(0);
                let live = r.get("live_agents").and_then(Value::as_u64).unwrap_or(0);
                let topics = r.get("topics_count").and_then(Value::as_u64).unwrap_or(0);
                let created = r.get("created_at").and_then(Value::as_str).unwrap_or("-");
                writeln!(
                    writer,
                    "{id}\t{pattern}\t{kind}\t{state}\t{agents}\t{live}\t{topics}\t{created}"
                )?;
            }
        }
        Method::ShepherdPatternRunShow(_) => {
            let run = &result["run"];
            let id = run.get("id").and_then(Value::as_str).unwrap_or("-");
            let pattern = run.get("pattern").and_then(Value::as_str).unwrap_or("-");
            let kind = run.get("kind").and_then(Value::as_str).unwrap_or("-");
            let state = run.get("state").and_then(Value::as_str).unwrap_or("-");
            let task = run.get("task").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "Run ID:   {id}")?;
            writeln!(writer, "Pattern:  {pattern}")?;
            writeln!(writer, "Kind:     {kind}")?;
            writeln!(writer, "State:    {state}")?;
            if let Some(why) = run.get("stalled_reason").and_then(Value::as_str) {
                writeln!(writer, "Stalled:  {why}")?;
            }
            writeln!(writer, "Task:     {task}")?;
            if let Some(agents) = run.get("agents").and_then(Value::as_array) {
                writeln!(writer, "\nAgents ({}):", agents.len())?;
                for a in agents {
                    let aid = a.get("id").and_then(Value::as_str).unwrap_or("-");
                    let role = a.get("role").and_then(Value::as_str).unwrap_or("-");
                    let astate = a.get("state").and_then(Value::as_str).unwrap_or("-");
                    let starts = if a.get("start").and_then(Value::as_bool) == Some(true) {
                        " starts"
                    } else {
                        ""
                    };
                    writeln!(writer, "  - {aid} ({role}) [{astate}]{starts}")?;
                }
            }
        }
        Method::ShepherdPatternRunPause(_) => {
            let run = &result["run"];
            let id = run.get("id").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "paused pattern run {id}")?;
        }
        Method::ShepherdPatternRunResume(_) => {
            let run = &result["run"];
            let id = run.get("id").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "resumed pattern run {id}")?;
        }
        Method::ShepherdPatternRunStop(_) => {
            let run = &result["run"];
            let id = run.get("id").and_then(Value::as_str).unwrap_or("-");
            writeln!(writer, "stopped pattern run {id}")?;
        }
        Method::ShepherdTopicList(_) => {
            let topics = result
                .get("topics")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if topics.is_empty() {
                writeln!(writer, "no topics found")?;
                return Ok(());
            }
            writeln!(writer, "NAME\tPARTITIONS\tMESSAGES")?;
            for t in topics {
                let name = t.get("name").and_then(Value::as_str).unwrap_or("-");
                let p = t.get("partitions").and_then(Value::as_u64).unwrap_or(0);
                let m = t.get("messages_count").and_then(Value::as_u64).unwrap_or(0);
                writeln!(writer, "{name}\t{p}\t{m}")?;
            }
        }
        Method::ShepherdTopicCreate(_) => {
            let topic = &result["topic"];
            let name = topic.get("name").and_then(Value::as_str).unwrap_or("-");
            let p = topic.get("partitions").and_then(Value::as_u64).unwrap_or(1);
            writeln!(writer, "created topic {name} ({p} partitions)")?;
        }
        Method::ShepherdTopicPublish(_) => {
            let msg = &result["message"];
            let id = msg.get("id").and_then(Value::as_str).unwrap_or("-");
            let topic = msg.get("topic").and_then(Value::as_str).unwrap_or("-");
            let partition = msg.get("partition").and_then(Value::as_u64).unwrap_or(0);
            let offset = msg.get("offset").and_then(Value::as_u64).unwrap_or(0);
            writeln!(
                writer,
                "published message {id} to {topic}[{partition}] (offset {offset})"
            )?;
        }
        Method::ShepherdTopicTail(_) | Method::ShepherdTopicReplay(_) => {
            let messages = result
                .get("messages")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            for msg in messages {
                let ts = msg.get("ts").and_then(Value::as_str).unwrap_or("");
                let topic = msg.get("topic").and_then(Value::as_str).unwrap_or("-");
                let partition = msg.get("partition").and_then(Value::as_u64).unwrap_or(0);
                let offset = msg.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let from = msg.get("from").and_then(Value::as_str).unwrap_or("unknown");
                let content = msg.get("content").and_then(Value::as_str).unwrap_or("");
                writeln!(
                    writer,
                    "[{ts}] {topic}[{partition}]:#{offset} from {from}: {content}"
                )?;
            }
        }
        Method::ShepherdTopicGroups(_) => {
            let groups = result
                .get("groups")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            if groups.is_empty() {
                writeln!(writer, "no consumer groups")?;
                return Ok(());
            }
            writeln!(writer, "TOPIC\tGROUP\tMEMBERS\tCOMMITTED\tTOTAL\tLAG")?;
            for g in groups {
                let topic = g.get("topic").and_then(Value::as_str).unwrap_or("-");
                let group = g.get("group").and_then(Value::as_str).unwrap_or("-");
                let members = g
                    .get("members")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                let per_partition = |key: &str, only_nonzero: bool| {
                    let mut parts: Vec<(u64, u64)> = g
                        .get(key)
                        .and_then(Value::as_object)
                        .map(|o| {
                            o.iter()
                                .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_u64()?)))
                                .filter(|(_, v)| !only_nonzero || *v > 0)
                                .collect()
                        })
                        .unwrap_or_default();
                    parts.sort_unstable();
                    parts
                };
                let join = |parts: Vec<(u64, u64)>, sep: &str, empty: &str| {
                    if parts.is_empty() {
                        empty.to_owned()
                    } else {
                        parts
                            .iter()
                            .map(|(p, v)| format!("p{p}:{sep}{v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                };
                let committed = join(per_partition("committed_offsets", false), "#", "-");
                let lag = join(per_partition("lag_by_partition", true), "", "0");
                let total = g.get("total_messages").and_then(Value::as_u64).unwrap_or(0);
                writeln!(
                    writer,
                    "{topic}\t{group}\t{members}\t{committed}\t{total}\t{lag}"
                )?;
            }
        }
        _ => {
            writeln!(writer, "{}", serde_json::to_string_pretty(result)?)?;
        }
    }
    Ok(())
}

fn execute_local_command(
    cmd: LocalCommand,
    root: PathBuf,
    _config: &shepherd_core::config::Config,
    is_json: bool,
    writer: &mut dyn std::io::Write,
) -> std::io::Result<i32> {
    match cmd {
        LocalCommand::McpServe { port } => {
            let terminals: Option<Arc<dyn Terminals>> = None;
            if let Err(e) = shepherd_core::mcp::serve_with_terminals(port, terminals) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::Attach { target, takeover } => {
            let mut attach_args = Vec::new();
            if let Some(t) = target {
                attach_args.push(t);
            }
            if takeover {
                attach_args.push("--takeover".into());
            }
            if let Err(e) = shepherd_core::cli::attach_to_shepherd(root, attach_args, None) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::ConfigPath => {
            if let Err(e) = shepherd_core::cli::config_paths(root, is_json, writer) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::ConfigValidate => {
            if let Err(e) = shepherd_core::cli::config_validate(root, is_json, writer) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::RolesList => {
            if let Err(e) = shepherd_core::cli::roles_list(root, Vec::new(), is_json, writer) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::VendorsList => {
            if let Err(e) = shepherd_core::cli::vendors_list(root, is_json, writer) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::TaskShow { reference, section } => {
            let mut show_args = vec![reference];
            if let Some(s) = section {
                show_args.push(s);
            }
            if let Err(e) = shepherd_core::cli::show_task_command(root, show_args, is_json, writer)
            {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::TaskPlan { reference, save } => {
            let mut plan_args = vec![reference];
            if save {
                plan_args.push("--save".into());
            }
            if let Err(e) = shepherd_core::cli::plan_task_command(root, plan_args, is_json, writer)
            {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::TaskEdit { reference, section } => {
            let mut edit_args = vec![reference];
            if let Some(s) = section {
                edit_args.push(s);
            }
            if let Err(e) = shepherd_core::cli::edit_task_command(root, edit_args, writer) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::TaskPath { reference } => {
            if let Err(e) =
                shepherd_core::cli::task_path_command(root, vec![reference], is_json, writer)
            {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::PatternNew { name, from, global } => {
            let mut new_args = vec![name, "--from".into(), from];
            if global {
                new_args.push("--global".into());
            }
            if let Err(e) =
                shepherd_core::cli::pattern_new_command(root, new_args, None, is_json, writer)
            {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
        LocalCommand::PatternEdit { name } => {
            if let Err(e) = shepherd_core::cli::pattern_edit_command(root, vec![name], writer) {
                eprintln!("shepherd: {e}");
                return Ok(1);
            }
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_core::bus::MessageBus;

    /// Regression (F8f): `topic groups` printed `0 0 0 0` for real groups because the
    /// formatter read fields the serialized `ConsumerGroupSummary` does not have.
    #[test]
    fn topic_groups_table_shows_real_members_committed_total_and_lag() {
        let dir = std::env::temp_dir().join(format!("shep-f8f-groups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bus = MessageBus::open(&dir.join("messages.jsonl")).unwrap();
        bus.topic_create("results", 1).unwrap();
        bus.subscribe("coder-1", "results", None).unwrap();
        for _ in 0..3 {
            bus.publish("results", "m", None, Some(0), "brain-1")
                .unwrap();
        }
        bus.ack("coder-1", "results", Some(0), Some(0)).unwrap();
        let groups = bus.consumer_groups(Some("results")).unwrap();
        let result = serde_json::json!({ "groups": groups });

        let mut out = Vec::new();
        let method = Method::ShepherdTopicGroups(Default::default());
        print_human_api_response(&mut out, &method, &result).unwrap();
        let text = String::from_utf8(out).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            text,
            "TOPIC\tGROUP\tMEMBERS\tCOMMITTED\tTOTAL\tLAG\nresults\tcoder-1\t1\tp0:#0\t3\tp0:2\n"
        );
    }
}
