//! Reusable CLI entrypoint for Shepherd (`shepherd <subcommand>` and `shepherd shepherd <subcommand>`).

use crate::{
    agents_json, archive_task,
    config::{self, available_vendors, load_config, resolve_all, validate},
    create_task, find_task, list_tasks, load_agent_registry, parse_plan, read_task_section,
    roles_list_json, save_task_status, schedule_subtasks,
    service::{OrchestrationService, ServiceConfig, SystemClock},
    spawn::{self, SpawnRequest},
    task_context, task_list_json, task_section_path, task_show_json,
    terminals::{connect_terminals, Terminals},
    touch_task, validate_plan, write_task_section, CreatedTask, NewTask,
};
use crate::shepherd::{self, ShepherdClient};
use serde_json::Value;
use std::{
    env,
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

/// Execute Shepherd CLI arguments using the current working directory and configured `Terminals` backend.
pub fn run_cli<I, S>(args: I) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    run_cli_with_terminals(args, None)
}

/// Execute Shepherd CLI arguments with an optional pre-connected `Terminals` adapter (such as `ShepherdInProcess`).
pub fn run_cli_with_terminals<I, S>(
    args: I,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let root = project_root(env::current_dir()?);
    run_cli_in_root(root, args, terminals)
}

/// Execute Shepherd CLI arguments rooted at `root` with an optional pre-connected `Terminals` adapter.
pub fn run_cli_in_root<I, S>(
    root: PathBuf,
    args: I,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    run_cli_in_root_with_client(root, args, terminals, None)
}

/// Execute Shepherd CLI arguments with an explicit or resolved `ShepherdClient`.
pub fn run_cli_in_root_with_client<I, S>(
    root: PathBuf,
    args: I,
    terminals: Option<Arc<dyn Terminals>>,
    client: Option<ShepherdClient>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut stdout = std::io::stdout();
    run_cli_in_root_with_output(root, args, terminals, client, &mut stdout)
}

/// Execute Shepherd CLI arguments directing output to `writer`.
pub fn run_cli_in_root_with_output<I, S>(
    root: PathBuf,
    args: I,
    terminals: Option<Arc<dyn Terminals>>,
    client: Option<ShepherdClient>,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let raw_args: Vec<String> = args.into_iter().map(Into::into).collect();
    let no_server = raw_args.iter().any(|a| a == "--no-server");
    let json = raw_args.iter().any(|a| a == "--json");
    let clean_args: Vec<String> = raw_args
        .into_iter()
        .filter(|a| a != "--no-server" && a != "--json")
        .collect();

    let server_client = if !no_server {
        if let Some(c) = client {
            if c.socket_exists() {
                Some(c)
            } else {
                None
            }
        } else {
            resolve_existing_client(&root)
        }
    } else {
        None
    };

    match dispatch_cli(root, clean_args, terminals, server_client.as_ref(), json, writer) {
        Ok(()) => Ok(()),
        Err(err) => {
            if json {
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::json!({ "error": err.to_string() })
                );
            }
            Err(err)
        }
    }
}

fn resolve_existing_client(_root: &Path) -> Option<ShepherdClient> {
    if let Some(path) = std::env::var_os("SHEPHERD_SOCKET_PATH").filter(|v| !v.is_empty()) {
        let client = ShepherdClient::new(PathBuf::from(path));
        if client.socket_exists() {
            return Some(client);
        }
    }
    if let Some(session) = std::env::var_os("SHEPHERD_SESSION").filter(|v| !v.is_empty()) {
        if let Ok(socket) = shepherd::resolve_socket(session.to_str()) {
            let client = ShepherdClient::new(socket);
            if client.socket_exists() {
                return Some(client);
            }
        }
    }
    None
}

fn dispatch_cli(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args_iter = args.into_iter();
    let Some(area) = args_iter.next() else {
        return usage();
    };
    if area == "attach" {
        return attach_to_shepherd(root, args_iter.collect(), terminals);
    }
    if area == "agents" {
        return agents_command(root, server, json, writer);
    }
    if area == "spawn" {
        return spawn_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "kill" {
        return kill_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "clean" {
        return clean_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "focus" {
        return focus_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "git" {
        return git_command(root, args_iter.collect(), server, json, writer);
    }
    if area == "messages" || area == "msg" {
        return Err("operation not available (service does not support msg/messages)".into());
    }
    let Some(command) = args_iter.next() else {
        if area == "tasks" {
            return list_task_command(root, Vec::new(), server, json, writer);
        }
        if area == "patterns" || area == "pattern" {
            return patterns_list_command(root, Vec::new(), server, json, writer);
        }
        return usage();
    };
    let rest: Vec<String> = args_iter.collect();

    match area.as_str() {
        "pattern" => match command.as_str() {
            "list" => patterns_list_command(root, rest, server, json, writer),
            "show" => pattern_show_command(root, rest, server, json, writer),
            "new" => pattern_new_command(root, rest, server, json, writer),
            "validate" => pattern_validate_command(root, rest, server, json, writer),
            "delete" => pattern_delete_command(root, rest, server, json, writer),
            "edit" => pattern_edit_command(root, rest, writer),
            _ => usage(),
        },
        "patterns" => {
            let mut list_args = vec![command];
            list_args.extend(rest);
            patterns_list_command(root, list_args, server, json, writer)
        }
        "task" => match command.as_str() {
            "new" => create_new_task(root, rest, server, json, writer),
            "list" => list_task_command(root, rest, server, json, writer),
            "show" => show_task_command(root, rest, json, writer),
            "plan" => plan_task_command(root, rest, json, writer),
            "edit" => edit_task_command(root, rest, writer),
            "path" => task_path_command(root, rest, json, writer),
            "archive" => archive_task_command(root, rest, server, json, writer),
            _ => usage(),
        },
        "tasks" => {
            let mut list_args = vec![command];
            list_args.extend(rest);
            list_task_command(root, list_args, server, json, writer)
        }
        "skills" if command == "list" && rest.is_empty() => {
            skills_list(root, rest, server, json, writer)
        }
        "runs" if command == "list" && rest.is_empty() => {
            runs_list(root, rest, server, json, writer)
        }
        "roles" if command == "list" => roles_list(root, rest, json, writer),
        "vendors" if command == "list" && rest.is_empty() => vendors_list(root, json, writer),
        "run" => run_command(root, command, rest, terminals, server, json, writer),
        "approve" if rest.len() == 1 => {
            approve_command(root, &command, &rest[0], server, json, writer)
        }
        "reject" if !rest.is_empty() => {
            reject_command(root, &command, rest, server, json, writer)
        }
        "mcp" if command == "serve" && rest.len() <= 1 => {
            let port = rest
                .first()
                .map(String::as_str)
                .unwrap_or("0")
                .parse::<u16>()?;
            crate::mcp::serve_with_terminals(port, terminals)
        }
        "config" if command == "path" && rest.is_empty() => config_paths(root, json, writer),
        "config" if command == "validate" && rest.is_empty() => {
            config_validate(root, json, writer)
        }
        _ => usage(),
    }
}

fn agents_command(
    _root: PathBuf,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call("agents", serde_json::json!({}))?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_agents_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let registry = load_agent_registry(config::state_dir().join("agents.json"))?;
    if json {
        writeln!(writer, "{}", agents_json(&registry)?)?;
        return Ok(());
    }
    print_human_agents(writer, &registry.agents)
}

fn print_human_agents_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let list = match data.as_array() {
        Some(arr) => arr,
        None => return Ok(()),
    };
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
    Ok(())
}

fn print_human_agents(
    writer: &mut dyn std::io::Write,
    agents: &[crate::Agent],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if agents.is_empty() {
        writeln!(writer, "no agents")?;
        return Ok(());
    }
    writeln!(writer, "ID\tSTATE\tROLE\tVENDOR\tMODEL\tTASK")?;
    for agent in agents {
        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}\t{}",
            agent.id,
            agent.state,
            agent.role,
            agent.vendor,
            if agent.model.is_empty() {
                "-"
            } else {
                &agent.model
            },
            agent.task_text
        )?;
    }
    Ok(())
}

fn spawn_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(role) = args.first() else {
        return usage();
    };
    let mut task = Vec::new();
    let mut task_id = None;
    let mut vendor = None;
    let mut model = None;
    let mut mcp_url = None;
    let mut i = 1;
    while i < args.len() {
        let value = args[i].clone();
        i += 1;
        let option = match value.as_str() {
            "--task-id" => Some(&mut task_id),
            "--vendor" => Some(&mut vendor),
            "--model" => Some(&mut model),
            "--mcp-url" => Some(&mut mcp_url),
            _ => None,
        };
        if let Some(destination) = option {
            let Some(argument) = args.get(i) else {
                return Err(format!("{value} needs a value").into());
            };
            *destination = Some(argument.clone());
            i += 1;
        } else if value.starts_with('-') {
            return Err(format!("unknown option {value}").into());
        } else {
            task.push(value);
        }
    }
    if let Some(client) = server {
        let spawn_req = serde_json::json!({
            "root": root.display().to_string(),
            "role": role,
            "task": task.join(" "),
            "task_id": task_id,
            "vendor": vendor,
            "model": model,
        });
        let data = client.shepherd_call("spawn", spawn_req)?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_spawn_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let req = SpawnRequest {
        root,
        role: role.clone(),
        task: task.join(" "),
        task_id,
        vendor,
        model,
        mcp_url,
    };
    let spawned = match terminals {
        Some(t) => spawn::spawn_with_terminals(req, t.as_ref())?,
        None => spawn::spawn(req)?,
    };
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&spawned.agent.public())?
        )?;
    } else {
        print_human_spawn(writer, &spawned.agent)?;
    }
    Ok(())
}

fn print_human_spawn_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let id = data.get("id").and_then(Value::as_str).unwrap_or("-");
    let vendor = data.get("vendor").and_then(Value::as_str).unwrap_or("-");
    let model = data.get("model").and_then(Value::as_str).unwrap_or("");
    let model = if model.is_empty() { "default" } else { model };
    let pane = data.get("pane").and_then(Value::as_str).unwrap_or("-");
    writeln!(writer, "started {id} ({vendor}/{model}) in Shepherd pane {pane}")?;
    let worktree = data.get("worktree").and_then(Value::as_str).unwrap_or("");
    let branch = data.get("branch").and_then(Value::as_str).unwrap_or("");
    if !worktree.is_empty() {
        writeln!(writer, "worktree {worktree} ({branch})")?;
    }
    Ok(())
}

fn print_human_spawn(
    writer: &mut dyn std::io::Write,
    agent: &crate::Agent,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    writeln!(
        writer,
        "started {} ({}/{}) in Shepherd pane {}",
        agent.id,
        agent.vendor,
        if agent.model.is_empty() {
            "default"
        } else {
            &agent.model
        },
        agent.pane
    )?;
    if !agent.worktree.is_empty() {
        writeln!(writer, "worktree {} ({})", agent.worktree, agent.branch)?;
    }
    Ok(())
}

fn kill_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let id = args.into_iter().find(|arg| !arg.starts_with('-'));
    let Some(id) = id else {
        return Err("usage: shepherd kill <id> [--json]".into());
    };
    if let Some(client) = server {
        let data = client.shepherd_call("kill", serde_json::json!({ "id": id }))?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            let kid = data.get("id").and_then(Value::as_str).unwrap_or(id.as_str());
            let state = data.get("state").and_then(Value::as_str).unwrap_or("killed");
            writeln!(writer, "killed agent {kid} ({state})")?;
        }
        return Ok(());
    }
    let terminals = match terminals {
        Some(t) => t,
        None => Arc::from(connect_terminals(&load_config(&root).unwrap_or_default())?),
    };
    let svc = OrchestrationService::new(
        ServiceConfig::default(),
        load_config(&root).unwrap_or_default(),
        terminals,
        Arc::new(SystemClock::new()),
    );
    let agent = svc.kill(&id)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&agent.public())?)?;
    } else {
        writeln!(writer, "killed agent {} ({})", agent.id, agent.state)?;
    }
    Ok(())
}

fn clean_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ids = Vec::new();
    let mut workspace = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--workspace" {
            i += 1;
            workspace = args.get(i).cloned();
        } else if !arg.starts_with('-') {
            ids.push(arg.clone());
        }
        i += 1;
    }
    if let Some(client) = server {
        let data = client.shepherd_call(
            "clean",
            serde_json::json!({
                "ids": ids,
                "workspace": workspace,
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            let cleaned: Vec<String> = data
                .get("cleaned")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            writeln!(
                writer,
                "cleaned {} agent(s): {}",
                cleaned.len(),
                cleaned.join(", ")
            )?;
        }
        return Ok(());
    }
    let terminals = match terminals {
        Some(t) => t,
        None => Arc::from(connect_terminals(&load_config(&root).unwrap_or_default())?),
    };
    let svc = OrchestrationService::new(
        ServiceConfig::default(),
        load_config(&root).unwrap_or_default(),
        terminals,
        Arc::new(SystemClock::new()),
    );
    let cleaned = svc.clean(&ids, workspace.as_deref())?;
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "cleaned": cleaned }))?
        )?;
    } else {
        writeln!(
            writer,
            "cleaned {} agent(s): {}",
            cleaned.len(),
            cleaned.join(", ")
        )?;
    }
    Ok(())
}

fn focus_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let id = args.into_iter().find(|arg| !arg.starts_with('-'));
    let Some(id) = id else {
        return Err("usage: shepherd focus <id> [--json]".into());
    };
    if let Some(client) = server {
        let data = client.shepherd_call("focus", serde_json::json!({ "id": id }))?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            let fid = data.get("id").and_then(Value::as_str).unwrap_or(id.as_str());
            writeln!(
                writer,
                "focused agent {fid} in native shepherd terminal"
            )?;
        }
        return Ok(());
    }
    let terminals = match terminals {
        Some(t) => t,
        None => Arc::from(connect_terminals(&load_config(&root).unwrap_or_default())?),
    };
    let svc = OrchestrationService::new(
        ServiceConfig::default(),
        load_config(&root).unwrap_or_default(),
        terminals,
        Arc::new(SystemClock::new()),
    );
    let agent = svc.focus(&id)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&agent.public())?)?;
    } else {
        writeln!(
            writer,
            "focused agent {} in native shepherd terminal",
            agent.id
        )?;
    }
    Ok(())
}

fn run_command(
    root: PathBuf,
    reference: String,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut mcp_url = None;
    let mut auto_push = false;
    let mut wait = false;
    let mut i = 0;
    while i < args.len() {
        let option = &args[i];
        i += 1;
        if option == "--mcp-url" {
            let Some(value) = args.get(i) else {
                return Err("--mcp-url needs a value".into());
            };
            mcp_url = Some(value.clone());
            i += 1;
        } else if option == "--auto-push" {
            auto_push = true;
        } else if option == "--wait" {
            wait = true;
        } else {
            return Err(format!("unknown option {option}").into());
        }
    }
    if let Some(client) = server {
        let data = client.shepherd_call(
            "run",
            serde_json::json!({
                "root": root.display().to_string(),
                "task": reference,
                "mcp_url": mcp_url,
                "auto_push": auto_push,
            }),
        )?;
        if wait {
            let start = Instant::now();
            let max_timeout = Duration::from_secs(30 * 60);
            if !json {
                writeln!(writer, "waiting for run {reference} to complete...")?;
            }
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if start.elapsed() > max_timeout {
                    return Err(format!("run for task {reference} timed out after 30 minutes").into());
                }
                let runs_data = client.shepherd_call(
                    "runs",
                    serde_json::json!({ "root": root.display().to_string() }),
                )?;
                if let Some(runs) = runs_data.as_array() {
                    let r_opt = runs.iter().find(|item| {
                        item.get("task_id").and_then(Value::as_u64).map(|n| n.to_string()) == Some(reference.clone())
                            || item.get("task_id").and_then(Value::as_str) == Some(&reference)
                    });
                    if let Some(r) = r_opt {
                        let status = r.get("status").and_then(Value::as_str).unwrap_or("");
                        let pending_gate = r.get("pending_gate");
                        let has_gate = pending_gate.is_some() && !pending_gate.unwrap().is_null();
                        if has_gate || matches!(status, "done" | "completed" | "failed" | "cancelled") {
                            if json {
                                writeln!(writer, "{}", serde_json::to_string_pretty(&r)?)?;
                            } else if has_gate {
                                writeln!(writer, "run for task {reference} is waiting for human approval at a gate; inspect with `shepherd runs list` or approve with `shepherd approve`")?;
                            } else {
                                writeln!(writer, "run for task {reference} finished with status: {status}")?;
                            }
                            return Ok(());
                        }
                    }
                }
            }
        }
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_run(writer, &data)?;
        }
        return Ok(());
    }
    let mcp_url =
        mcp_url.ok_or("run requires --mcp-url while the Rust MCP server is started separately")?;
    let req = crate::workflow::RunRequest {
        root,
        task: reference,
        mcp_url,
    };
    let report = match terminals {
        Some(t) => crate::workflow::run_coders_with_terminals(req, t.as_ref(), None, None)?,
        None => crate::workflow::run_coders(req)?,
    };
    if json {
        let rep_json = serde_json::json!({
            "started": report.started,
            "completed_coding": report.completed_coding,
            "waiting_for_human": report.waiting_for_human,
            "failed": report.failed,
            "plan_waiting": report.plan_waiting,
            "auto_push": false,
            "pushed_prs": [],
        });
        writeln!(writer, "{}", serde_json::to_string_pretty(&rep_json)?)?;
        return Ok(());
    }
    if report.plan_waiting {
        writeln!(writer, "plan is saved and waiting for human approval; inspect plan.md, run shepherd approve <task> plan, then rerun shepherd run")?;
        return Ok(());
    }
    writeln!(
        writer,
        "started coders: {}",
        if report.started.is_empty() {
            "none".into()
        } else {
            report.started.join(", ")
        }
    )?;
    writeln!(
        writer,
        "results received for subtasks: {}",
        ids(&report.completed_coding)
    )?;
    writeln!(writer, "failed subtasks: {}", ids(&report.failed))?;
    writeln!(
        writer,
        "needs human/review stage: {}",
        ids(&report.waiting_for_human)
    )?;
    writeln!(
        writer,
        "Rust run currently stops before human approval, push, and PR creation."
    )?;
    Ok(())
}

fn print_human_run(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if data.get("accepted").and_then(Value::as_bool).unwrap_or(false) {
        let task = data.get("task").and_then(Value::as_str).unwrap_or("");
        let job = data.get("job").and_then(Value::as_str).unwrap_or("");
        writeln!(writer, "run started in background for task {task} (job: {job}); use --wait to wait for completion or inspect with `shepherd runs list`")?;
        return Ok(());
    }
    if data
        .get("plan_waiting")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        writeln!(writer, "plan is saved and waiting for human approval; inspect plan.md, run shepherd approve <task> plan, then rerun shepherd run")?;
        return Ok(());
    }
    let started = data
        .get("started")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "none".into());
    writeln!(
        writer,
        "started coders: {}",
        if started.is_empty() { "none" } else { &started }
    )?;
    let completed = format_json_u32_array(data.get("completed_coding"));
    writeln!(writer, "results received for subtasks: {completed}")?;
    let failed = format_json_u32_array(data.get("failed"));
    writeln!(writer, "failed subtasks: {failed}")?;
    let waiting = format_json_u32_array(data.get("waiting_for_human"));
    writeln!(writer, "needs human/review stage: {waiting}")?;
    writeln!(
        writer,
        "Rust run currently stops before human approval, push, and PR creation."
    )?;
    Ok(())
}

fn format_json_u32_array(val: Option<&Value>) -> String {
    let Some(arr) = val.and_then(Value::as_array) else {
        return "none".into();
    };
    if arr.is_empty() {
        return "none".into();
    }
    arr.iter()
        .filter_map(Value::as_u64)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn ids(numbers: &[u32]) -> String {
    if numbers.is_empty() {
        "none".into()
    } else {
        numbers
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn approve_command(
    root: PathBuf,
    task: &str,
    target: &str,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call(
            "approve",
            serde_json::json!({
                "root": root.display().to_string(),
                "task": task,
                "target": target,
            }),
        )?;
        if json {
            let mut val = data.clone();
            if val.get("ok").is_none() {
                if let Some(map) = val.as_object_mut() {
                    map.insert("ok".into(), Value::Bool(true));
                }
            }
            writeln!(writer, "{}", serde_json::to_string_pretty(&val)?)?;
        } else if target == "plan" {
            writeln!(
                writer,
                "approved plan for task {task}; rerun shepherd run to start coders"
            )?;
        } else {
            let pr = data.get("pr").or_else(|| data.get("url")).and_then(Value::as_str).unwrap_or("");
            writeln!(writer, "created pull request: {pr}")?;
        }
        return Ok(());
    }
    if target == "plan" {
        crate::workflow::approve_plan(root, task)?;
        if json {
            writeln!(
                writer,
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "task": task,
                    "approved": "plan",
                }))?
            )?;
        } else {
            writeln!(
                writer,
                "approved plan for task {task}; rerun shepherd run to start coders"
            )?;
        }
        return Ok(());
    }
    let number = target
        .parse::<u32>()
        .map_err(|_| "subtask number must be an integer")?;
    let url = crate::workflow::approve_subtask(root, task, number)?;
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "task": task,
                "approved": number,
                "url": url,
                "pr": url,
            }))?
        )?;
    } else {
        writeln!(writer, "created pull request: {url}")?;
    }
    Ok(())
}

fn reject_command(
    root: PathBuf,
    task: &str,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut target = None;
    let mut reason = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--reason" {
            i += 1;
            let Some(val) = args.get(i) else {
                return Err("--reason requires a value".into());
            };
            reason = Some(val.clone());
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            return Err(format!("unexpected argument {arg}").into());
        }
        i += 1;
    }
    let target = target.ok_or("reject requires a target: 'plan' or a subtask number")?;
    if let Some(client) = server {
        let mut req = serde_json::json!({
            "root": root.display().to_string(),
            "task": task,
            "target": target,
        });
        if let Some(ref r) = reason {
            req["reason"] = serde_json::json!(r);
        }
        let data = client.shepherd_call("reject", req)?;
        if json {
            let mut val = data.clone();
            if val.get("ok").is_none() {
                if let Some(map) = val.as_object_mut() {
                    map.insert("ok".into(), Value::Bool(true));
                }
            }
            writeln!(writer, "{}", serde_json::to_string_pretty(&val)?)?;
        } else {
            writeln!(
                writer,
                "rejected {} for task {task}{}",
                target,
                reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()
            )?;
        }
        return Ok(());
    }
    if target == "plan" {
        crate::workflow::reject_plan(root, task, reason.as_deref())?;
        if json {
            writeln!(
                writer,
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "task": task,
                    "rejected": "plan",
                    "reason": reason,
                }))?
            )?;
        } else {
            writeln!(
                writer,
                "rejected plan for task {task}{}",
                reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()
            )?;
        }
        return Ok(());
    }
    let number = target
        .parse::<u32>()
        .map_err(|_| "subtask number must be an integer")?;
    crate::workflow::reject_subtask(root, task, number, reason.as_deref())?;
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "task": task,
                "rejected": number,
                "reason": reason,
            }))?
        )?;
    } else {
        writeln!(
            writer,
            "rejected subtask {number} for task {task}{}",
            reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()
        )?;
    }
    Ok(())
}

fn skills_list(
    root: PathBuf,
    _args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call(
            "skills",
            serde_json::json!({
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_skills_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let cat = crate::skills::Catalog::load(Some(&root), None);
    let all = cat.all();
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&all)?)?;
    } else {
        print_human_skills_entries(writer, &all)?;
    }
    Ok(())
}

fn print_human_skills_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let skills = data.as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    if skills.is_empty() {
        writeln!(writer, "no skills")?;
        return Ok(());
    }
    writeln!(writer, "NAME\tTIER\tDESCRIPTION")?;
    for s in skills {
        let name = s.get("name").and_then(Value::as_str).unwrap_or("-");
        let tier = s.get("tier").and_then(Value::as_str).unwrap_or("-");
        let desc = s.get("description").and_then(Value::as_str).unwrap_or("");
        writeln!(writer, "{name}\t{tier}\t{desc}")?;
    }
    Ok(())
}

fn print_human_skills_entries(
    writer: &mut dyn std::io::Write,
    skills: &[crate::skills::Skill],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if skills.is_empty() {
        writeln!(writer, "no skills")?;
        return Ok(());
    }
    writeln!(writer, "NAME\tTIER\tDESCRIPTION")?;
    for s in skills {
        writeln!(writer, "{}\t{}\t{}", s.name, s.scope, s.description)?;
    }
    Ok(())
}

fn runs_list(
    root: PathBuf,
    _args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call(
            "runs",
            serde_json::json!({
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_runs(writer, &data)?;
        }
        return Ok(());
    }
    let tasks = list_tasks(&root, false).unwrap_or_default();
    let runs: Vec<serde_json::Value> = tasks
        .into_iter()
        .map(|t| {
            let has_plan = t.directory.join("plan.md").is_file();
            let pending_gate = if t.status.waiting == "plan" {
                Some(serde_json::json!("plan"))
            } else if let Some(subtask) = t.status.subtasks.iter().find(|s| s.state == "awaiting-approval") {
                Some(serde_json::json!({
                    "kind": "push_pr",
                    "subtask": subtask.number,
                }))
            } else {
                None
            };
            let total = t.status.subtasks.len();
            let done_count = t.status.subtasks.iter().filter(|s| matches!(s.state.as_str(), "pr" | "done")).count();
            let subtasks: Vec<serde_json::Value> = t.status.subtasks.iter().map(|s| {
                serde_json::json!({
                    "n": s.number,
                    "title": s.title,
                    "state": s.state,
                    "branch": s.branch,
                    "pr": s.pr,
                    "note": s.note,
                    "coder": s.coder,
                    "reviewer": s.reviewer,
                })
            }).collect();
            serde_json::json!({
                "task_id": t.status.id,
                "task_title": t.status.title,
                "status": t.status.state,
                "current_stage": t.status.state,
                "has_saved_plan": has_plan,
                "pending_gate": pending_gate,
                "active_agents": t.status.agents,
                "subtasks_progress": [done_count, total],
                "subtasks": subtasks,
            })
        })
        .collect();
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&runs)?)?;
    } else {
        print_human_runs(writer, &serde_json::json!(runs))?;
    }
    Ok(())
}

fn print_human_runs(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runs = data.as_array().map(|a| a.as_slice()).unwrap_or(&[]);
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
    Ok(())
}

fn git_command(
    root: PathBuf,
    _args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call(
            "git_status",
            serde_json::json!({
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_git_status(writer, &data)?;
        }
        return Ok(());
    }
    let registry = load_agent_registry(config::state_dir().join("agents.json")).unwrap_or_default();
    let status = crate::git::collect_git_status(&root, &registry.agents)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&status)?)?;
    } else {
        let val = serde_json::to_value(&status)?;
        print_human_git_status(writer, &val)?;
    }
    Ok(())
}

fn print_human_git_status(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let worktrees = data
        .get("worktrees")
        .and_then(Value::as_array);
    let list = match worktrees {
        Some(arr) => arr,
        None => return Ok(()),
    };
    if list.is_empty() {
        writeln!(writer, "no worktrees found")?;
        return Ok(());
    }
    writeln!(writer, "BRANCH\tSTATUS\tSYNC\tAGENT\tPR\tPATH")?;
    for wt in list {
        let branch = wt.get("branch").and_then(Value::as_str).unwrap_or("-");
        let is_clean = wt.get("is_clean").and_then(Value::as_bool).unwrap_or(true);
        let dirty_count = wt.get("dirty_count").and_then(Value::as_u64).unwrap_or(0);
        let status = if is_clean {
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
                let state = p.get("state")?.as_str()?;
                Some(format!("#{num} {state}"))
            })
            .unwrap_or_else(|| "—".to_string());
        let path = wt.get("path").and_then(Value::as_str).unwrap_or("-");
        writeln!(writer, "{branch}\t{status}\t{sync}\t{agent}\t{pr}\t{path}")?;
    }
    if let Some(note) = data.get("gh_note").and_then(Value::as_str) {
        writeln!(writer, "\nNote: {note}")?;
    }
    Ok(())
}

fn roles_list(
    root: PathBuf,
    args: Vec<String>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !args.is_empty() {
        return usage();
    }
    let config = load_config(&root)?;
    let active = available_vendors(&config);
    if json {
        writeln!(writer, "{}", roles_list_json(&config, &active)?)?;
        return Ok(());
    }
    for assignment in resolve_all(&config, &active) {
        match assignment {
            Ok(role) => writeln!(
                writer,
                "{}\t{}/{}{}",
                role.role,
                role.vendor,
                if role.model.is_empty() {
                    "default"
                } else {
                    &role.model
                },
                if role.fallback { " (fallback)" } else { "" }
            )?,
            Err(error) => writeln!(writer, "unavailable\t{error}")?,
        }
    }
    Ok(())
}

fn vendors_list(
    root: PathBuf,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = load_config(&root)?;
    let active = available_vendors(&config);
    let mut names: Vec<_> = config.vendors.keys().collect();
    names.sort();
    if json {
        let vendors_json: Vec<_> = names
            .iter()
            .map(|name| {
                let vendor = &config.vendors[*name];
                let state = if vendor.disabled {
                    "disabled"
                } else if active.contains(*name) {
                    "available"
                } else {
                    "not installed"
                };
                serde_json::json!({
                    "name": name,
                    "command": vendor.command,
                    "state": state,
                })
            })
            .collect();
        writeln!(writer, "{}", serde_json::to_string_pretty(&vendors_json)?)?;
        return Ok(());
    }
    for name in names {
        let vendor = &config.vendors[name];
        let state = if vendor.disabled {
            "disabled"
        } else if active.contains(name) {
            "available"
        } else {
            "not installed"
        };
        writeln!(writer, "{name}\t{}\t{state}", vendor.command)?;
    }
    Ok(())
}

fn config_paths(
    root: PathBuf,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "global": config::global_config_path().display().to_string(),
                "project": config::project_config_path(&root).display().to_string(),
                "state": config::state_dir().display().to_string(),
            }))?
        )?;
        return Ok(());
    }
    writeln!(writer, "global\t{}", config::global_config_path().display())?;
    writeln!(writer, "project\t{}", config::project_config_path(&root).display())?;
    writeln!(writer, "state\t{}", config::state_dir().display())?;
    Ok(())
}

fn config_validate(
    root: PathBuf,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = load_config(&root)?;
    let issues = validate(&config);
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "valid": issues.is_empty(),
                "issues": issues,
            }))?
        )?;
        return Ok(());
    }
    if issues.is_empty() {
        writeln!(writer, "configuration is valid")?;
        return Ok(());
    }
    for issue in issues {
        eprintln!("- {issue}");
    }
    Err("configuration validation failed".into())
}

fn attach_to_shepherd(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (target, takeover) = match args.as_slice() {
        [] => (None, false),
        [target] => (Some(target.as_str()), false),
        [target, flag] if flag == "--takeover" => (Some(target.as_str()), true),
        _ => return Err("usage: shepherd attach [shepherd-agent-name-or-pane-id] [--takeover]".into()),
    };
    match terminals {
        Some(t) => t.attach(target, takeover)?,
        None => {
            let config = load_config(root).unwrap_or_default();
            let terminals = connect_terminals(&config)?;
            terminals.attach(target, takeover)?;
        }
    }
    Ok(())
}

fn create_new_task(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(title) = args.first() else {
        return usage();
    };
    let mut input = NewTask {
        id: 0,
        title: title.clone(),
        description: String::new(),
        criteria: Vec::new(),
        issue: None,
        issue_markdown: None,
        links: Vec::new(),
    };
    let mut i = 1;
    while i < args.len() {
        let option = &args[i];
        i += 1;
        let value = args
            .get(i)
            .ok_or_else(|| format!("{option} needs a value"))?;
        i += 1;
        match option.as_str() {
            "--description" => input.description = value.clone(),
            "--criteria" => input.criteria.push(value.clone()),
            "--id" => input.id = value.parse()?,
            _ => return Err(format!("unknown option {option}").into()),
        }
    }
    if let Some(client) = server {
        let data = client.shepherd_call(
            "task_create",
            serde_json::json!({
                "root": root.display().to_string(),
                "title": input.title,
                "description": input.description,
                "criteria": input.criteria,
                "attachments": input.links,
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            let id = data
                .get("status")
                .and_then(|s| s.get("id"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let slug = data
                .get("status")
                .and_then(|s| s.get("slug"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let dir = data
                .get("directory")
                .and_then(Value::as_str)
                .unwrap_or("");
            writeln!(writer, "created task {id}-{slug}")?;
            writeln!(writer, "{dir}")?;
        }
        return Ok(());
    }
    let task = create_task(&root, input)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&task)?)?;
    } else {
        writeln!(writer, "created task {}-{}", task.status.id, task.status.slug)?;
        writeln!(writer, "{}", task.directory.display())?;
    }
    Ok(())
}

fn list_task_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut archived = false;
    for arg in &args {
        if arg == "--archived" {
            archived = true;
        } else {
            return usage();
        }
    }
    if let Some(client) = server {
        let data = client.shepherd_call(
            "tasks",
            serde_json::json!({
                "root": root.display().to_string(),
                "archived": archived,
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_tasks_from_json(writer, &data, archived)?;
        }
        return Ok(());
    }
    let tasks = list_tasks(&root, archived)?;
    if json {
        writeln!(writer, "{}", task_list_json(&tasks)?)?;
        return Ok(());
    }
    print_human_tasks(writer, &tasks, archived)
}

fn print_human_tasks(
    writer: &mut dyn std::io::Write,
    tasks: &[CreatedTask],
    archived: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if tasks.is_empty() {
        writeln!(writer, "no {}tasks", if archived { "archived " } else { "" })?;
        return Ok(());
    }
    writeln!(writer, "ID\tSTATE\tTITLE\tISSUE\tPR")?;
    for task in tasks {
        let issue = task
            .status
            .issue
            .as_ref()
            .map(|i| format!("#{}", i.number))
            .unwrap_or_else(|| "-".into());
        let pr = if task.status.pr == 0 {
            "-".into()
        } else {
            format!("#{}", task.status.pr)
        };
        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}",
            task.status.id, task.status.state, task.status.title, issue, pr
        )?;
    }
    Ok(())
}

fn print_human_tasks_from_json(
    writer: &mut dyn std::io::Write,
    data: &Value,
    archived: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let arr = data.as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    if arr.is_empty() {
        writeln!(writer, "no {}tasks", if archived { "archived " } else { "" })?;
        return Ok(());
    }
    writeln!(writer, "ID\tSTATE\tTITLE\tISSUE\tPR")?;
    for item in arr {
        let status = item.get("status");
        let id = status
            .and_then(|s| s.get("id"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let state = status
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("-");
        let title = status
            .and_then(|s| s.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("-");
        let issue = status
            .and_then(|s| s.get("issue"))
            .and_then(|i| i.get("number"))
            .and_then(Value::as_u64)
            .map(|n| format!("#{n}"))
            .unwrap_or_else(|| "-".into());
        let pr = status
            .and_then(|s| s.get("pr"))
            .and_then(Value::as_u64)
            .filter(|&n| n != 0)
            .map(|n| format!("#{n}"))
            .unwrap_or_else(|| "-".into());
        writeln!(writer, "{id}\t{state}\t{title}\t{issue}\t{pr}")?;
    }
    Ok(())
}

fn show_task_command(
    root: PathBuf,
    args: Vec<String>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !(1..=2).contains(&args.len()) {
        return usage();
    }
    let reference = &args[0];
    let task = find_task(&root, reference)?;
    if json {
        writeln!(
            writer,
            "{}",
            task_show_json(&task, args.get(1).map(String::as_str))?
        )?;
        return Ok(());
    }
    if let Some(section) = args.get(1) {
        write!(writer, "{}", read_task_section(&task, section)?)?;
    } else {
        write!(writer, "{}", task_context(&task, 0))?;
    }
    Ok(())
}

fn plan_task_command(
    root: PathBuf,
    args: Vec<String>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reference, save) = match args.as_slice() {
        [reference] => (reference.as_str(), false),
        [reference, flag] if flag == "--save" => (reference.as_str(), true),
        _ => return usage(),
    };
    let mut task = find_task(&root, reference)?;
    let plan = read_task_section(&task, "plan")?;
    if plan.trim().is_empty() || plan.trim() == "# Plan" {
        return Err("the task has no plan yet; write one to its plan section first".into());
    }
    let subtasks = parse_plan(&plan, &task.status.title);
    validate_plan(&subtasks)?;
    let mut scheduled = subtasks.clone();
    let round = schedule_subtasks(&mut scheduled, &[], usize::MAX);
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": task.status.id,
                "subtasks": subtasks,
                "ready": round.ready,
                "saved": save,
            }))?
        )?;
        if save {
            task.status.subtasks = subtasks;
            task.status.state = "planning".into();
            save_task_status(task.directory.join("status.json"), &task.status)?;
        }
        return Ok(());
    }
    writeln!(
        writer,
        "task {}: {} subtask(s), initial ready: {}",
        task.status.id,
        subtasks.len(),
        round
            .ready
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    for subtask in &subtasks {
        let deps = if subtask.depends_on.is_empty() {
            "-".to_owned()
        } else {
            subtask
                .depends_on
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        writeln!(
            writer,
            "{}\t{}\tdepends: {}\tverify: {}",
            subtask.number,
            subtask.title,
            deps,
            subtask.verify.join(" && ")
        )?;
    }
    if save {
        task.status.subtasks = subtasks;
        task.status.state = "planning".into();
        save_task_status(task.directory.join("status.json"), &task.status)?;
        writeln!(writer, "saved validated plan to status.json")?;
    }
    Ok(())
}

fn edit_task_command(
    root: PathBuf,
    args: Vec<String>,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !(1..=2).contains(&args.len()) {
        return usage();
    }
    let task = find_task(&root, &args[0])?;
    let section = args.get(1).map(String::as_str).unwrap_or("spec");
    let path = task_section_path(&task, section)?;
    if !path.exists() {
        write_task_section(&task, section, &format!("# {section}\n\n"))?;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        writeln!(writer, "{}", path.display())?;
        return Ok(());
    }
    let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(editor).arg(&path).status()?;
    if !status.success() {
        return Err(format!("editor exited with {status}").into());
    }
    touch_task(&task)?;
    Ok(())
}

fn task_path_command(
    root: PathBuf,
    args: Vec<String>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let [reference] = args.as_slice() else {
        return usage();
    };
    let task = find_task(&root, reference)?;
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": task.directory.display().to_string(),
            }))?
        )?;
    } else {
        writeln!(writer, "{}", task.directory.display())?;
    }
    Ok(())
}

fn archive_task_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let [reference] = args.as_slice() else {
        return usage();
    };
    if let Some(client) = server {
        let data = client.shepherd_call(
            "task_archive",
            serde_json::json!({
                "root": root.display().to_string(),
                "id": reference,
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            let path = data.get("path").and_then(Value::as_str).unwrap_or("");
            writeln!(writer, "archived {path}")?;
        }
        return Ok(());
    }
    let path = archive_task(&root, reference)?;
    if json {
        writeln!(
            writer,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "archived": true,
                "path": path.display().to_string(),
            }))?
        )?;
    } else {
        writeln!(writer, "archived {}", path.display())?;
    }
    Ok(())
}

fn patterns_list_command(
    root: PathBuf,
    _args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call(
            "patterns",
            serde_json::json!({
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_patterns_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let list = crate::patterns::list_patterns(&root, None)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&list)?)?;
    } else {
        print_human_patterns_list(writer, &list)?;
    }
    Ok(())
}

fn print_human_patterns_list(
    writer: &mut dyn std::io::Write,
    list: &[crate::patterns::PatternSummary],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if list.is_empty() {
        writeln!(writer, "no patterns found")?;
        return Ok(());
    }
    writeln!(
        writer,
        "NAME\tKIND\tSOURCE\tSHADOWED\tAGENTS\tTOPICS\tDESCRIPTION"
    )?;
    for p in list {
        let shadowed_str = if p.shadowed { "yes" } else { "no" };
        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            p.name,
            p.kind,
            p.source,
            shadowed_str,
            p.agents_count,
            p.topics_count,
            p.description
        )?;
    }
    Ok(())
}

fn print_human_patterns_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let patterns = data.as_array().map(|a| a.as_slice()).unwrap_or(&[]);
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
        let shadowed = p
            .get("shadowed")
            .and_then(Value::as_bool)
            .map(|b| if b { "yes" } else { "no" })
            .unwrap_or("no");
        let agents = p.get("agents_count").and_then(Value::as_u64).unwrap_or(0);
        let topics = p.get("topics_count").and_then(Value::as_u64).unwrap_or(0);
        let desc = p.get("description").and_then(Value::as_str).unwrap_or("");
        writeln!(
            writer,
            "{name}\t{kind}\t{source}\t{shadowed}\t{agents}\t{topics}\t{desc}"
        )?;
    }
    Ok(())
}

fn pattern_show_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = args.first().ok_or("missing pattern name for 'show'")?;
    if let Some(client) = server {
        let data = client.shepherd_call(
            "pattern_get",
            serde_json::json!({
                "name": name,
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_pattern_detail(writer, &data)?;
        }
        return Ok(());
    }
    let (pattern, source, path) = crate::patterns::load_pattern(name, &root, None)?;
    let config = load_config(&root).unwrap_or_default();
    let problems = crate::patterns::validate(&pattern, &config);
    let detail = crate::patterns::PatternDetail {
        pattern,
        source,
        path: path.map(|p| p.display().to_string()),
        problems,
    };
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&detail)?)?;
    } else {
        let val = serde_json::to_value(&detail)?;
        print_human_pattern_detail(writer, &val)?;
    }
    Ok(())
}

fn print_human_pattern_detail(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = data.get("name").and_then(Value::as_str).unwrap_or("-");
    let kind = data.get("kind").and_then(Value::as_str).unwrap_or("-");
    let source = data.get("source").and_then(Value::as_str).unwrap_or("-");
    let path = data
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("(built-in template)");
    let desc = data.get("description").and_then(Value::as_str).unwrap_or("");

    writeln!(writer, "Pattern: {name} ({kind})")?;
    writeln!(writer, "Source:  {source} ({path})")?;
    writeln!(writer, "Purpose: {desc}")?;
    writeln!(writer)?;

    if let Some(topics) = data.get("topics").and_then(Value::as_array) {
        writeln!(writer, "Topics ({}):", topics.len())?;
        for t in topics {
            let tname = t.get("name").and_then(Value::as_str).unwrap_or("-");
            let partitions = t.get("partitions").and_then(Value::as_u64).unwrap_or(1);
            let mut meta = vec![format!("partitions: {partitions}")];
            if let Some(k) = t.get("key").and_then(Value::as_str) {
                meta.push(format!("key: {k}"));
            }
            if let Some(f) = t.get("filter").and_then(Value::as_str) {
                meta.push(format!("filter: {f}"));
            }
            writeln!(writer, "  • {tname:<20} ({})", meta.join(", "))?;
        }
        writeln!(writer)?;
    }

    if let Some(agents) = data.get("agents").and_then(Value::as_array) {
        writeln!(writer, "Agents ({}):", agents.len())?;
        for a in agents {
            let aid = a.get("id").and_then(Value::as_str).unwrap_or("-");
            let role = a.get("role").and_then(Value::as_str).unwrap_or("-");
            let count = a.get("count").and_then(Value::as_u64).unwrap_or(1);
            let mut badges = vec![format!("role: {role}"), format!("count: {count}")];
            if let Some(grp) = a.get("group").and_then(Value::as_str) {
                badges.push(format!("group: {grp}"));
            }
            if let Some(lvl) = a.get("level").and_then(Value::as_u64) {
                badges.push(format!("level: {lvl}"));
            }
            if let Some(vendor) = a.get("vendor").and_then(Value::as_str) {
                badges.push(format!("vendor: {vendor}"));
            }
            if let Some(model) = a.get("model").and_then(Value::as_str) {
                badges.push(format!("model: {model}"));
            }
            writeln!(writer, "  • {aid:<20} [{}]", badges.join(" | "))?;

            let pub_list: Vec<&str> = a
                .get("publishes")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let con_list: Vec<&str> = a
                .get("consumes")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();

            if !pub_list.is_empty() {
                writeln!(writer, "      publishes -> {}", pub_list.join(", "))?;
            }
            if !con_list.is_empty() {
                writeln!(writer, "      consumes  <- {}", con_list.join(", "))?;
            }
            if let Some(prompt) = a.get("prompt").and_then(Value::as_str) {
                let preview = if prompt.len() > 120 {
                    format!("{}...", &prompt[..117])
                } else {
                    prompt.to_string()
                };
                writeln!(writer, "      persona: \"{preview}\"")?;
            }
        }
        writeln!(writer)?;
    }

    if let Some(problems) = data.get("problems").and_then(Value::as_array) {
        if !problems.is_empty() {
            writeln!(writer, "Validation Diagnostics:")?;
            for prob in problems {
                let sev = prob.get("severity").and_then(Value::as_str).unwrap_or("warning");
                let path = prob.get("path").and_then(Value::as_str).unwrap_or("-");
                let msg = prob.get("message").and_then(Value::as_str).unwrap_or("");
                writeln!(writer, "  [{sev}] {path}: {msg}")?;
            }
        }
    }
    Ok(())
}

fn pattern_new_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut name = None;
    let mut from = None;
    let mut is_global = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" | "-f" => {
                i += 1;
                if i < args.len() {
                    from = Some(&args[i]);
                }
            }
            "--global" | "-g" => {
                is_global = true;
            }
            other if !other.starts_with('-') && name.is_none() => {
                name = Some(other);
            }
            _ => {}
        }
        i += 1;
    }

    let name = name.ok_or("missing pattern name for 'new'")?;
    let from = from.ok_or("missing '--from <template|name>' for 'new'")?;

    if let Some(client) = server {
        let get_data = client.shepherd_call(
            "pattern_get",
            serde_json::json!({
                "name": from,
                "root": root.display().to_string(),
            }),
        )?;
        let mut pattern_val = get_data;
        pattern_val["name"] = serde_json::json!(name);
        let save_res = client.shepherd_call(
            "pattern_save",
            serde_json::json!({
                "pattern": pattern_val,
                "global": is_global,
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&save_res)?)?;
        } else {
            let path_str = save_res.get("path").and_then(Value::as_str).unwrap_or("");
            writeln!(writer, "created pattern '{name}' at {path_str}")?;
        }
        return Ok(());
    }

    let (pattern, path) = crate::patterns::duplicate_pattern(name, from, is_global, &root, None)?;
    let config = load_config(&root).unwrap_or_default();
    let problems = crate::patterns::validate(&pattern, &config);
    if json {
        let out = serde_json::json!({
            "ok": true,
            "name": name,
            "path": path.display().to_string(),
            "source": if is_global { "global" } else { "project" },
            "problems": problems,
        });
        writeln!(writer, "{}", serde_json::to_string_pretty(&out)?)?;
    } else {
        writeln!(writer, "created pattern '{name}' at {}", path.display())?;
    }
    Ok(())
}

fn pattern_validate_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut file_path = None;
    let mut name = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                i += 1;
                if i < args.len() {
                    file_path = Some(PathBuf::from(&args[i]));
                }
            }
            other if !other.starts_with('-') && name.is_none() => {
                name = Some(other);
            }
            _ => {}
        }
        i += 1;
    }

    if file_path.is_none() && name.is_none() {
        return Err("missing pattern name or '--file <path>' to validate".into());
    }

    let (pattern, pattern_label) = if let Some(path) = file_path {
        let pat = crate::patterns::load_pattern_from_file(&path)?;
        let label = path.display().to_string();
        (pat, label)
    } else {
        let n = name.unwrap();
        if let Some(client) = server {
            let res = client.shepherd_call(
                "pattern_validate",
                serde_json::json!({
                    "name": n,
                    "root": root.display().to_string(),
                }),
            )?;
            if json {
                writeln!(writer, "{}", serde_json::to_string_pretty(&res)?)?;
                return Ok(());
            }
            let valid = res.get("valid").and_then(Value::as_bool).unwrap_or(false);
            print_human_validation_problems(writer, n, &res)?;
            if !valid {
                return Err("pattern validation failed".into());
            }
            return Ok(());
        }
        let (pat, _, _) = crate::patterns::load_pattern(n, &root, None)?;
        (pat, n.to_string())
    };

    let config = load_config(&root).unwrap_or_default();
    let problems = crate::patterns::validate(&pattern, &config);
    let valid = !problems
        .iter()
        .any(|p| p.severity == crate::patterns::Severity::Error);

    if json {
        let out = serde_json::json!({
            "valid": valid,
            "problems": problems,
        });
        writeln!(writer, "{}", serde_json::to_string_pretty(&out)?)?;
        return Ok(());
    }

    let val = serde_json::json!({ "valid": valid, "problems": problems });
    print_human_validation_problems(writer, &pattern_label, &val)?;

    if !valid {
        return Err("pattern validation failed".into());
    }
    Ok(())
}

fn print_human_validation_problems(
    writer: &mut dyn std::io::Write,
    label: &str,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let valid = data.get("valid").and_then(Value::as_bool).unwrap_or(false);
    let problems = data.get("problems").and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[]);

    if valid && problems.is_empty() {
        writeln!(writer, "OK: pattern '{label}' is valid (0 errors, 0 warnings)")?;
        return Ok(());
    }

    if valid {
        writeln!(writer, "OK: pattern '{label}' is valid (with warnings):")?;
    } else {
        writeln!(writer, "FAILED: pattern '{label}' has validation errors:")?;
    }

    for p in problems {
        let sev = p.get("severity").and_then(Value::as_str).unwrap_or("warning");
        let path = p.get("path").and_then(Value::as_str).unwrap_or("-");
        let msg = p.get("message").and_then(Value::as_str).unwrap_or("");
        writeln!(writer, "  [{sev}] {path}: {msg}")?;
    }
    Ok(())
}

fn pattern_delete_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = args.first().ok_or("missing pattern name for 'delete'")?;

    if let Some(client) = server {
        let data = client.shepherd_call(
            "pattern_delete",
            serde_json::json!({
                "name": name,
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            let src = data
                .get("deleted_source")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            writeln!(writer, "deleted pattern '{name}' from {src}")?;
        }
        return Ok(());
    }

    let source = crate::patterns::delete_pattern(name, &root, None)?;
    if json {
        let out = serde_json::json!({
            "ok": true,
            "name": name,
            "deleted_source": source.as_str(),
        });
        writeln!(writer, "{}", serde_json::to_string_pretty(&out)?)?;
    } else {
        writeln!(writer, "deleted pattern '{name}' from {source}")?;
    }
    Ok(())
}

fn pattern_edit_command(
    root: PathBuf,
    args: Vec<String>,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = args.first().ok_or("missing pattern name for 'edit'")?;
    let (_, _source, path) = crate::patterns::load_pattern(name, &root, None)?;
    let Some(path) = path else {
        return Err(format!(
            "cannot edit built-in pattern template '{name}' in place; duplicate it first with `shepherd pattern new <name> --from {name}`"
        )
        .into());
    };
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        writeln!(writer, "{}", path.display())?;
        return Ok(());
    }
    let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(editor).arg(&path).status()?;
    if !status.success() {
        return Err(format!("editor exited with {status}").into());
    }
    Ok(())
}

pub fn project_root(mut path: PathBuf) -> PathBuf {
    loop {
        if path.join(".git").exists() || path.join(".shepherd").exists() {
            return path;
        }
        if !path.pop() {
            return env::current_dir().unwrap_or_default();
        }
    }
}

fn usage<T>() -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    Err("usage: shepherd attach [shepherd-agent-name-or-pane-id] [--takeover] | shepherd mcp serve [port] | shepherd run <task-id> [--mcp-url <url>] [--auto-push] [--wait] [--json] | shepherd approve <task-id> plan|<subtask-number> [--json] | shepherd reject <task-id> plan|<subtask-number> [--reason <text>] [--json] | shepherd spawn <role> <task...> [--task-id <id>] [--vendor <name>] [--model <name>] [--mcp-url <url>] [--json] | shepherd agents [--json] | shepherd skills list [--json] | shepherd runs list [--json] | shepherd roles list [--json] | shepherd vendors list [--json] | shepherd config path|validate [--json] | shepherd task new <title> [--description <text>] [--criteria <text>]... [--id <number>] [--json] | task list [--archived] [--json] | task show <id-or-name> [section] [--json] | task plan <id-or-name> [--save] [--json] | task edit <id-or-name> [section] | task path <id-or-name> [--json] | task archive <id-or-name> [--json] | shepherd pattern list [--json] | pattern show <name> [--json] | pattern new <name> --from <template|name> [--global] [--json] | pattern validate <name|--file path> [--json] | pattern delete <name> [--json] | pattern edit <name>".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminals::{shepherd_attach_args, InMemoryTerminals};

    static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TestEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        orig_xdg: Option<std::ffi::OsString>,
        orig_xdg_config: Option<std::ffi::OsString>,
        orig_home: Option<std::ffi::OsString>,
        orig_editor: Option<std::ffi::OsString>,
        dir: PathBuf,
    }

    impl TestEnvGuard {
        fn new() -> Self {
            let lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let orig_xdg = std::env::var_os("XDG_STATE_HOME");
            let orig_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
            let orig_home = std::env::var_os("HOME");
            let orig_editor = std::env::var_os("EDITOR");
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("shepherd-test-env-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(dir.join("shepherd")).unwrap();
            std::env::set_var("XDG_STATE_HOME", &dir);
            std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg"));
            std::env::set_var("HOME", &dir);
            std::env::set_var("EDITOR", "true");
            Self {
                _lock: lock,
                orig_xdg,
                orig_xdg_config,
                orig_home,
                orig_editor,
                dir,
            }
        }
    }

    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            match &self.orig_xdg {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
            match &self.orig_xdg_config {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match &self.orig_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.orig_editor {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn attach_delegates_to_the_selected_shepherd_session_and_agent() {
        assert_eq!(
            shepherd_attach_args("shepherd", Some("coder-1"), true),
            ["--session", "shepherd", "agent", "attach", "coder-1", "--takeover"]
        );
        assert_eq!(
            shepherd_attach_args("-", Some("w1:p2"), false),
            ["agent", "attach", "w1:p2"]
        );
        assert_eq!(
            shepherd_attach_args("shepherd", None, false),
            ["session", "attach", "shepherd"]
        );
        assert!(shepherd_attach_args("-", None, false).is_empty());
    }

    #[test]
    fn plan_command_validates_and_persists_subtasks_in_go_compatible_status() {
        let _env = TestEnvGuard::new();
        use crate::{create_task, find_task, write_task_section, NewTask};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-plan-command-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let task = create_task(
            &root,
            NewTask {
                id: 1,
                title: "Plan test".into(),
                description: String::new(),
                criteria: Vec::new(),
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();
        write_task_section(
            &task,
            "plan",
            "# Plan\n\n## Subtask 1: Base\nVerify: cargo test\n\n## Subtask 2: Follow-up\nDepends on: 1\n",
        )
        .unwrap();
        run_with_buf(&root, &["task", "plan", "1", "--save"], None, None).unwrap();
        let saved = find_task(&root, "1").unwrap();
        assert_eq!(saved.status.subtasks.len(), 2);
        assert_eq!(saved.status.subtasks[1].depends_on, [1]);
        assert_eq!(saved.status.subtasks[0].verify, ["cargo test"]);
        assert_eq!(saved.status.state, "planning");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_json_flags_and_injected_terminals_are_accepted() {
        let _env = TestEnvGuard::new();
        use crate::{create_task, NewTask};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-json-command-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        create_task(
            &root,
            NewTask {
                id: 1,
                title: "JSON CLI test".into(),
                description: "Desc".into(),
                criteria: vec!["Check".into()],
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();

        run_with_buf(&root, &["agents", "--json"], None, None).unwrap();
        run_with_buf(&root, &["task", "list", "--json"], None, None).unwrap();
        run_with_buf(
            &root,
            &["task", "list", "--archived", "--json"],
            None,
            None,
        )
        .unwrap();
        run_with_buf(&root, &["task", "show", "1", "--json"], None, None).unwrap();
        run_with_buf(
            &root,
            &["task", "show", "1", "spec", "--json"],
            None,
            None,
        )
        .unwrap();
        run_with_buf(&root, &["roles", "list", "--json"], None, None).unwrap();

        let fake = Arc::new(InMemoryTerminals::new());
        let terminals_dyn: Arc<dyn Terminals> = Arc::clone(&fake) as Arc<dyn Terminals>;
        run_with_buf(
            &root,
            &["clean", "--json"],
            Some(Arc::clone(&terminals_dyn)),
            None,
        )
        .unwrap();
        run_with_buf(
            &root,
            &["attach", "coder-1", "--takeover"],
            Some(terminals_dyn),
            None,
        )
        .unwrap();

        std::fs::remove_dir_all(root).unwrap();
    }

    fn run_with_buf(
        root: &Path,
        args: &[&str],
        terminals: Option<Arc<dyn Terminals>>,
        client: Option<ShepherdClient>,
    ) -> Result<String, (String, Box<dyn std::error::Error + Send + Sync>)> {
        let mut buf = Vec::new();
        let string_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match run_cli_in_root_with_output(root.to_path_buf(), string_args, terminals, client, &mut buf) {
            Ok(()) => Ok(String::from_utf8_lossy(&buf).to_string()),
            Err(e) => Err((String::from_utf8_lossy(&buf).to_string(), e)),
        }
    }

    #[test]
    fn test_flag_parsing_no_server_and_json_position() {
        let _env = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-flag-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        // 1. Mock server client that tracks whether it was invoked
        let invoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let invoked_clone = Arc::clone(&invoked);
        let client = ShepherdClient::in_process(move |method, params| {
            assert_eq!(method, "shepherd.call");
            invoked_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(params["op"], "agents");
            Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": []
            }))
        });

        // 2. Normal execution without --no-server uses the server
        let out = run_with_buf(&root, &["agents", "--json"], None, Some(client)).unwrap();
        assert!(invoked.load(std::sync::atomic::Ordering::SeqCst));
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_array());

        // 3. Execution with --no-server (placed anywhere) bypasses the server
        let invoked_no_srv = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let invoked_no_srv_clone = Arc::clone(&invoked_no_srv);
        let client_no_srv = ShepherdClient::in_process(move |_method, _params| {
            invoked_no_srv_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!({}))
        });
        let out2 = run_with_buf(
            &root,
            &["--no-server", "agents", "--json"],
            None,
            Some(client_no_srv),
        )
        .unwrap();
        assert!(!invoked_no_srv.load(std::sync::atomic::Ordering::SeqCst));
        let parsed2: Value = serde_json::from_str(&out2).unwrap();
        assert!(parsed2.is_array());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_fallback_to_in_process_when_no_server() {
        let _env = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-fallback-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        // Pass a client pointing to a nonexistent socket
        let client = ShepherdClient::new(PathBuf::from("/tmp/nonexistent-shepherd-socket-12345.sock"));
        let out = run_with_buf(&root, &["agents", "--json"], None, Some(client)).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_array());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_json_error_output() {
        let _env = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-json-err-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        // 1. Task not found error with --json
        let (out, err) = run_with_buf(&root, &["task", "show", "9999", "--json"], None, None).unwrap_err();
        assert!(err.to_string().contains("9999"));
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.get("error").is_some());
        assert!(parsed["error"].as_str().unwrap().contains("9999"));

        // 2. Kill missing id with --json
        let (out_kill, _) = run_with_buf(&root, &["kill", "--json"], None, None).unwrap_err();
        let parsed_kill: Value = serde_json::from_str(&out_kill).unwrap();
        assert!(parsed_kill["error"].as_str().unwrap().contains("usage"));

        // 3. Msg / messages not available error with --json
        let (out_msg, _) = run_with_buf(&root, &["messages", "--json"], None, None).unwrap_err();
        let parsed_msg: Value = serde_json::from_str(&out_msg).unwrap();
        assert!(parsed_msg["error"].as_str().unwrap().contains("operation not available"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_all_subcommands_json_output() {
        let _env = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-all-json-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let fake_terminals = Arc::new(InMemoryTerminals::new());
        let terminals_dyn: Arc<dyn Terminals> = Arc::clone(&fake_terminals) as Arc<dyn Terminals>;

        // 1. task new --json
        let out = run_with_buf(
            &root,
            &[
                "task", "new", "Feature X",
                "--description", "My feature",
                "--criteria", "- [ ] Must work",
                "--id", "1",
                "--json",
            ],
            None,
            None,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"]["id"], 1);
        assert_eq!(v["status"]["title"], "Feature X");
        assert!(v.get("directory").is_some());

        // 2. task list --json
        let out = run_with_buf(&root, &["task", "list", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["id"], 1);

        // 3. task list --archived --json
        let out = run_with_buf(&root, &["task", "list", "--archived", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());

        // 4. task show 1 --json
        let out = run_with_buf(&root, &["task", "show", "1", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], 1);
        assert!(v.get("context").is_some());

        // 5. task show 1 spec --json
        let out = run_with_buf(&root, &["task", "show", "1", "spec", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], 1);
        assert_eq!(v["section"], "spec");
        assert!(v.get("content").is_some());

        // 6. task plan 1 --json
        let task = find_task(&root, "1").unwrap();
        write_task_section(
            &task,
            "plan",
            "# Plan\n\n## Subtask 1: Base\nVerify: true\n",
        )
        .unwrap();
        let out = run_with_buf(&root, &["task", "plan", "1", "--save", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], 1);
        assert!(v.get("subtasks").is_some());
        assert_eq!(v["saved"], true);

        // 7. task path 1 --json
        let out = run_with_buf(&root, &["task", "path", "1", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("path").is_some());

        // 8. roles list --json
        let out = run_with_buf(&root, &["roles", "list", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());

        // 9. vendors list --json
        let out = run_with_buf(&root, &["vendors", "list", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());
        assert!(v[0].get("name").is_some());
        assert!(v[0].get("command").is_some());

        // 10. config path --json
        let out = run_with_buf(&root, &["config", "path", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("global").is_some());
        assert!(v.get("project").is_some());
        assert!(v.get("state").is_some());

        // 11. config validate --json
        let out = run_with_buf(&root, &["config", "validate", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("valid").is_some());

        // 12. skills list --json
        let out = run_with_buf(&root, &["skills", "list", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());

        // 13. runs list --json
        let out = run_with_buf(&root, &["runs", "list", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["task_id"], 1);

        // 14. agents --json
        let out = run_with_buf(&root, &["agents", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());

        // 15. clean --json
        let out = run_with_buf(&root, &["clean", "--json"], Some(Arc::clone(&terminals_dyn)), None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("cleaned").is_some());

        // 16. approve 1 plan --json
        let mut status = task.status.clone();
        status.waiting = "plan".into();
        save_task_status(task.directory.join("status.json"), &status).unwrap();
        let out = run_with_buf(&root, &["approve", "1", "plan", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["task"], "1");
        assert_eq!(v["approved"], "plan");

        // 16b. reject 1 plan --reason "reformulate" --json
        let mut status = task.status.clone();
        status.waiting = "plan".into();
        save_task_status(task.directory.join("status.json"), &status).unwrap();
        let out = run_with_buf(&root, &["reject", "1", "plan", "--reason", "reformulate", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["task"], "1");
        assert_eq!(v["rejected"], "plan");
        assert_eq!(v["reason"], "reformulate");

        // 17. task archive 1 --json
        let out = run_with_buf(&root, &["task", "archive", "1", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["archived"], true);
        assert!(v.get("path").is_some());

        // 18. git --json
        let _ = std::process::Command::new("git").args(["init", "-b", "master"]).current_dir(&root).output();
        let out = run_with_buf(&root, &["git", "--json"], None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("worktrees").is_some());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_server_mode_delegation_via_in_process_client() {
        let _env = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-server-delegation-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let ops_called = Arc::new(std::sync::Mutex::new(Vec::new()));
        let ops_clone = Arc::clone(&ops_called);

        let client = ShepherdClient::in_process(move |method, params| {
            assert_eq!(method, "shepherd.call");
            let op = params["op"].as_str().unwrap().to_string();
            ops_clone.lock().unwrap().push((op.clone(), params["args"].clone()));
            match op.as_str() {
                "agents" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": [
                        { "id": "coder-1", "state": "idle", "role": "coder", "vendor": "gemini", "model": "", "task_text": "demo" }
                    ]
                })),
                "spawn" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "id": "coder-2", "role": "coder", "vendor": "gemini", "model": "", "state": "starting", "pane": "w1:p2", "worktree": "", "branch": "" }
                })),
                "kill" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "id": "coder-1", "state": "killed" }
                })),
                "clean" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "cleaned": ["coder-1"] }
                })),
                "focus" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "id": "coder-1" }
                })),
                "skills" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": [
                        { "name": "test-skill", "tier": "default", "description": "A skill" }
                    ]
                })),
                "runs" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": [
                        { "task_id": 1, "task_title": "Run demo", "status": "active", "pending_gate": null, "active_agents": [] }
                    ]
                })),
                "tasks" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": [
                        { "status": { "id": 1, "state": "planning", "title": "T1", "slug": "t1", "issue": null, "pr": 0 }, "directory": "/tmp/t1" }
                    ]
                })),
                "task_create" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "status": { "id": 2, "state": "draft", "title": "T2", "slug": "t2" }, "directory": "/tmp/t2" }
                })),
                "task_archive" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "archived": true, "path": "/tmp/archive/1" }
                })),
                "run" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "accepted": true, "job": "run-1", "task": "1" }
                })),
                "approve" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "task": "1", "approved": "plan" }
                })),
                "reject" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": { "ok": true, "task": "1", "rejected": "plan" }
                })),
                "git_status" => Ok(serde_json::json!({
                    "type": "shepherd_call",
                    "data": {
                        "root": "/tmp/repo",
                        "worktrees": [
                            {
                                "path": "/tmp/repo",
                                "branch": "master",
                                "is_main": true,
                                "is_clean": true,
                                "dirty_count": 0,
                                "ahead": 0,
                                "behind": 0,
                                "upstream": null,
                                "agent_id": null,
                                "pr": null
                            }
                        ],
                        "gh_available": false,
                        "gh_note": "gh no disponible",
                        "error": null
                    }
                })),
                other => Err(crate::shepherd::ShepherdError::Api {
                    code: "unknown_op".into(),
                    message: format!("unknown op {other}"),
                }),
            }
        });

        // 1. agents in server mode (human and JSON)
        let out_json = run_with_buf(&root, &["agents", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out_json).unwrap();
        assert_eq!(parsed[0]["id"], "coder-1");

        let out_human = run_with_buf(&root, &["agents"], None, Some(client.clone())).unwrap();
        assert!(out_human.contains("coder-1"));

        // 2. spawn in server mode
        let out = run_with_buf(&root, &["spawn", "coder", "do task", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["id"], "coder-2");

        // 3. kill in server mode
        let out = run_with_buf(&root, &["kill", "coder-1", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["state"], "killed");

        // 4. clean in server mode
        let out = run_with_buf(&root, &["clean", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["cleaned"][0], "coder-1");

        // 5. focus in server mode
        let out = run_with_buf(&root, &["focus", "coder-1", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["id"], "coder-1");

        // 6. skills list in server mode
        let out = run_with_buf(&root, &["skills", "list", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["name"], "test-skill");

        // 7. runs list in server mode
        let out = run_with_buf(&root, &["runs", "list", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["task_id"], 1);

        // 8. tasks in server mode
        let out = run_with_buf(&root, &["task", "list", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["status"]["id"], 1);

        // 9. task new in server mode
        let out = run_with_buf(&root, &["task", "new", "T2", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"]["id"], 2);

        // 10. task archive in server mode
        let out = run_with_buf(&root, &["task", "archive", "1", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["archived"], true);

        // 11. run in server mode
        let out = run_with_buf(&root, &["run", "1", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["accepted"], true);
        assert_eq!(parsed["job"], "run-1");

        // 12. approve in server mode
        let out = run_with_buf(&root, &["approve", "1", "plan", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["approved"], "plan");

        // 12b. reject in server mode
        let out = run_with_buf(&root, &["reject", "1", "plan", "--reason", "too complex", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["rejected"], "plan");

        // 13. git in server mode
        let out = run_with_buf(&root, &["git", "--json"], None, Some(client.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.get("worktrees").is_some());

        // Verify that all 13 alive-state ops were dispatched to shepherd.call!
        let calls = ops_called.lock().unwrap();
        let dispatched_ops: Vec<String> = calls.iter().map(|(op, _)| op.clone()).collect();
        assert!(dispatched_ops.contains(&"agents".to_string()));
        assert!(dispatched_ops.contains(&"spawn".to_string()));
        assert!(dispatched_ops.contains(&"kill".to_string()));
        assert!(dispatched_ops.contains(&"clean".to_string()));
        assert!(dispatched_ops.contains(&"focus".to_string()));
        assert!(dispatched_ops.contains(&"skills".to_string()));
        assert!(dispatched_ops.contains(&"runs".to_string()));
        assert!(dispatched_ops.contains(&"tasks".to_string()));
        assert!(dispatched_ops.contains(&"task_create".to_string()));
        assert!(dispatched_ops.contains(&"task_archive".to_string()));
        assert!(dispatched_ops.contains(&"run".to_string()));
        assert!(dispatched_ops.contains(&"approve".to_string()));
        assert!(dispatched_ops.contains(&"reject".to_string()));
        assert!(dispatched_ops.contains(&"git_status".to_string()));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_server_failure_does_not_fallback_to_in_process() {
        let _env = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-server-fail-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        // Client whose call fails (e.g. server busy, timeout, or internal error)
        let failing_client = ShepherdClient::in_process(|_method, _params| {
            Err(crate::shepherd::ShepherdError::Protocol("server timeout or internal error".into()))
        });

        // 1. With --json: should exit with error and json output containing {"error": ...}, NOT silent []
        let (out, err) = run_with_buf(
            &root,
            &["agents", "--json"],
            None,
            Some(failing_client.clone()),
        )
        .unwrap_err();

        assert!(err.to_string().contains("server timeout or internal error"));
        let parsed: Value = serde_json::from_str(&out).expect("must output valid error json");
        assert!(parsed.get("error").is_some());
        assert!(parsed["error"].as_str().unwrap().contains("server timeout or internal error"));
        // Ensure it did not output silent empty array "[]"
        assert_ne!(out.trim(), "[]");

        // 2. Without --json: should also return Err
        let (_out_human, err_human) = run_with_buf(
            &root,
            &["agents"],
            None,
            Some(failing_client),
        )
        .unwrap_err();
        assert!(err_human.to_string().contains("server timeout or internal error"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_no_server_or_nonexistent_socket_falls_back_to_in_process() {
        let _env = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("shepherd-no-server-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        // 1. Client with a nonexistent socket falls back to in-process mode
        let nonexistent_sock = root.join("nonexistent.sock");
        let client_nonexistent = ShepherdClient::new(nonexistent_sock);
        let out = run_with_buf(
            &root,
            &["agents", "--json"],
            None,
            Some(client_nonexistent),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, serde_json::json!([]));

        // 2. Client provided, but --no-server is passed: falls back to in-process mode
        let failing_client = ShepherdClient::in_process(|_method, _params| {
            Err(crate::shepherd::ShepherdError::Protocol("should never be called".into()))
        });
        let out_no_server = run_with_buf(
            &root,
            &["agents", "--no-server", "--json"],
            None,
            Some(failing_client),
        )
        .unwrap();
        let parsed_no_server: Value = serde_json::from_str(&out_no_server).unwrap();
        assert_eq!(parsed_no_server, serde_json::json!([]));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_pattern_cli_lifecycle_and_json() {
        let _guard = TestEnvGuard::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("shepherd-test-cli-pat-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(root.join(".shepherd")).unwrap();

        // 1. pattern list --json
        let out_list = run_with_buf(&root, &["pattern", "list", "--json"], None, None).unwrap();
        let list_json: Value = serde_json::from_str(&out_list).expect("valid json list");
        let list_arr = list_json.as_array().expect("array of patterns");
        assert_eq!(list_arr.len(), 4);

        // Human list format check
        let out_human_list = run_with_buf(&root, &["pattern", "list"], None, None).unwrap();
        assert!(out_human_list.contains("orchestrator-worker"));
        assert!(out_human_list.contains("hierarchical"));
        assert!(out_human_list.contains("blackboard"));
        assert!(out_human_list.contains("market"));

        // 2. pattern show orchestrator-worker --json
        let out_show = run_with_buf(
            &root,
            &["pattern", "show", "orchestrator-worker", "--json"],
            None,
            None,
        )
        .unwrap();
        let show_json: Value = serde_json::from_str(&out_show).expect("valid json show");
        assert_eq!(show_json["name"], "orchestrator-worker");
        assert_eq!(show_json["kind"], "orchestrator-worker");

        // Human show format check
        let out_human_show = run_with_buf(
            &root,
            &["pattern", "show", "orchestrator-worker"],
            None,
            None,
        )
        .unwrap();
        assert!(out_human_show.contains("Pattern: orchestrator-worker"));
        assert!(out_human_show.contains("orchestrator"));

        // 3. pattern new demo --from orchestrator-worker --json
        let out_new = run_with_buf(
            &root,
            &["pattern", "new", "demo", "--from", "orchestrator-worker", "--json"],
            None,
            None,
        )
        .unwrap();
        let new_json: Value = serde_json::from_str(&out_new).expect("valid json new");
        assert_eq!(new_json["ok"], true);
        assert_eq!(new_json["name"], "demo");

        let demo_path = root.join(".shepherd/patterns/demo.yaml");
        assert!(demo_path.exists());

        // 4. pattern validate demo --json
        let out_valid = run_with_buf(
            &root,
            &["pattern", "validate", "demo", "--json"],
            None,
            None,
        )
        .unwrap();
        let valid_json: Value = serde_json::from_str(&out_valid).expect("valid json validate");
        assert_eq!(valid_json["valid"], true);

        // 5. pattern validate --file <path> --json
        let out_valid_file = run_with_buf(
            &root,
            &["pattern", "validate", "--file", demo_path.to_str().unwrap(), "--json"],
            None,
            None,
        )
        .unwrap();
        let valid_file_json: Value = serde_json::from_str(&out_valid_file).expect("valid json validate file");
        assert_eq!(valid_file_json["valid"], true);

        // 6. Break demo.yaml and verify validate catches and explains it
        let mut broken_content = std::fs::read_to_string(&demo_path).unwrap();
        broken_content = broken_content.replace("- orders", "- broken-unknown-orders-topic");
        std::fs::write(&demo_path, broken_content).unwrap();

        // 6a. --json returns valid: false with explanation
        let out_broken = run_with_buf(
            &root,
            &["pattern", "validate", "demo", "--json"],
            None,
            None,
        )
        .unwrap();
        let broken_json: Value = serde_json::from_str(&out_broken).expect("valid json explanation");
        assert_eq!(broken_json["valid"], false);
        let problems = broken_json["problems"].as_array().unwrap();
        assert!(problems.iter().any(|p| p["message"]
            .as_str()
            .map_or(false, |m| m.contains("broken-unknown-orders-topic"))));

        // 6b. human output returns Err and prints diagnostic
        let (_, human_err) = run_with_buf(
            &root,
            &["pattern", "validate", "demo"],
            None,
            None,
        )
        .unwrap_err();
        assert!(human_err.to_string().contains("pattern validation failed"));

        // 7. pattern delete demo --json
        let out_del = run_with_buf(
            &root,
            &["pattern", "delete", "demo", "--json"],
            None,
            None,
        )
        .unwrap();
        let del_json: Value = serde_json::from_str(&out_del).expect("valid json delete");
        assert_eq!(del_json["ok"], true);
        assert_eq!(del_json["deleted_source"], "project");
        assert!(!demo_path.exists());

        // 8. Delete builtin template fails and outputs json error
        let (out_del_err, err_del) = run_with_buf(
            &root,
            &["pattern", "delete", "orchestrator-worker", "--json"],
            None,
            None,
        )
        .unwrap_err();
        assert!(err_del.to_string().contains("cannot delete built-in"));
        let del_err_json: Value = serde_json::from_str(&out_del_err).expect("valid error json");
        assert!(del_err_json["error"].as_str().unwrap().contains("cannot delete built-in"));

        // 9. Pattern edit on project file succeeds (with EDITOR=true)
        let _ = run_with_buf(
            &root,
            &["pattern", "new", "editdemo", "--from", "blackboard"],
            None,
            None,
        )
        .unwrap();
        assert!(run_with_buf(&root, &["pattern", "edit", "editdemo"], None, None).is_ok());

        // 10. Pattern edit on builtin template fails with actionable instruction
        let (_, err_edit_builtin) = run_with_buf(
            &root,
            &["pattern", "edit", "orchestrator-worker"],
            None,
            None,
        )
        .unwrap_err();
        assert!(err_edit_builtin.to_string().contains("cannot edit built-in"));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
