//! Agent lifecycle, spawn, run, workflow gates, skills, runs, and git status CLI commands.

use super::usage;
use crate::{
    agents_json,
    config::{self, available_vendors, load_config, resolve_all, validate},
    list_tasks, load_agent_registry, roles_list_json,
    service::{OrchestrationService, ServiceConfig, SystemClock},
    shepherd::ShepherdClient,
    spawn::{self, SpawnRequest},
    terminals::{connect_terminals, Terminals},
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) fn agents_command(
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

pub(crate) fn spawn_command(
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
    writeln!(
        writer,
        "started {id} ({vendor}/{model}) in Shepherd pane {pane}"
    )?;
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

pub(crate) fn kill_command(
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
            let kid = data
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(id.as_str());
            let state = data
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("killed");
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

pub(crate) fn clean_command(
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

pub(crate) fn focus_command(
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
            let fid = data
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(id.as_str());
            writeln!(writer, "focused agent {fid} in native shepherd terminal")?;
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

pub(crate) fn run_command(
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
                    return Err(
                        format!("run for task {reference} timed out after 30 minutes").into(),
                    );
                }
                let runs_data = client.shepherd_call(
                    "runs",
                    serde_json::json!({ "root": root.display().to_string() }),
                )?;
                if let Some(runs) = runs_data.as_array() {
                    let r_opt = runs.iter().find(|item| {
                        item.get("task_id")
                            .and_then(Value::as_u64)
                            .map(|n| n.to_string())
                            == Some(reference.clone())
                            || item.get("task_id").and_then(Value::as_str) == Some(&reference)
                    });
                    if let Some(r) = r_opt {
                        let status = r.get("status").and_then(Value::as_str).unwrap_or("");
                        let pending_gate = r.get("pending_gate");
                        let has_gate = pending_gate.is_some() && !pending_gate.unwrap().is_null();
                        if has_gate
                            || matches!(status, "done" | "completed" | "failed" | "cancelled")
                        {
                            if json {
                                writeln!(writer, "{}", serde_json::to_string_pretty(&r)?)?;
                            } else if has_gate {
                                writeln!(writer, "run for task {reference} is waiting for human approval at a gate; inspect with `shepherd runs list` or approve with `shepherd approve`")?;
                            } else {
                                writeln!(
                                    writer,
                                    "run for task {reference} finished with status: {status}"
                                )?;
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
    if data
        .get("accepted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
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

pub(crate) fn approve_command(
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
            let pr = data
                .get("pr")
                .or_else(|| data.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("");
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

pub(crate) fn reject_command(
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
                reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
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
                reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
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
            reason
                .as_deref()
                .map(|r| format!(": {r}"))
                .unwrap_or_default()
        )?;
    }
    Ok(())
}

pub(crate) fn skills_list(
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

pub(crate) fn runs_list(
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
            } else {
                t.status
                    .subtasks
                    .iter()
                    .find(|s| s.state == "awaiting-approval")
                    .map(|subtask| {
                        serde_json::json!({
                            "kind": "push_pr",
                            "subtask": subtask.number,
                        })
                    })
            };
            let total = t.status.subtasks.len();
            let done_count = t
                .status
                .subtasks
                .iter()
                .filter(|s| matches!(s.state.as_str(), "pr" | "done"))
                .count();
            let subtasks: Vec<serde_json::Value> = t
                .status
                .subtasks
                .iter()
                .map(|s| {
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
                })
                .collect();
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

pub(crate) fn git_command(
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
    let worktrees = data.get("worktrees").and_then(Value::as_array);
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

pub fn roles_list(
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

pub fn vendors_list(
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

pub fn config_paths(
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
    writeln!(
        writer,
        "project\t{}",
        config::project_config_path(&root).display()
    )?;
    writeln!(writer, "state\t{}", config::state_dir().display())?;
    Ok(())
}

pub fn config_validate(
    root: PathBuf,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (config, deprecations) = crate::config::load_config_with_diagnostics(&root)?;
    let issues = validate(&config);
    for dep in &deprecations {
        eprintln!("- warning: {dep}");
    }
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

pub fn attach_to_shepherd(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (target, takeover) = match args.as_slice() {
        [] => (None, false),
        [target] => (Some(target.as_str()), false),
        [target, flag] if flag == "--takeover" => (Some(target.as_str()), true),
        _ => {
            return Err(
                "usage: shepherd attach [shepherd-agent-name-or-pane-id] [--takeover]".into(),
            )
        }
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
