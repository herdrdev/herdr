//! Reusable CLI entrypoint for Shepherd (`shepherd <subcommand>` and `herdr shepherd <subcommand>`).

use crate::{
    agents_json, archive_task,
    config::{self, available_vendors, load_config, resolve_all, validate},
    create_task, find_task, list_tasks, load_agent_registry, parse_plan, read_task_section,
    roles_list_json, save_task_status, schedule_subtasks,
    service::{OrchestrationService, ServiceConfig, SystemClock},
    spawn::{self, SpawnRequest},
    task_context, task_list_json, task_section_path, task_show_json,
    terminals::{connect_terminals, Terminals},
    touch_task, validate_plan, write_task_section, NewTask,
};
use std::{env, io::IsTerminal, path::PathBuf, sync::Arc};

/// Execute Shepherd CLI arguments using the current working directory and configured `Terminals` backend.
pub fn run_cli<I, S>(args: I) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    run_cli_with_terminals(args, None)
}

/// Execute Shepherd CLI arguments with an optional pre-connected `Terminals` adapter (such as `HerdrInProcess`).
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
    let mut args = args.into_iter().map(Into::into);
    let Some(area) = args.next() else {
        return usage();
    };
    if area == "attach" {
        return attach_to_herdr(root, args.collect(), terminals);
    }
    if area == "agents" {
        return agents_list(args.collect());
    }
    if area == "spawn" {
        return spawn_command(root, args.collect(), terminals);
    }
    if area == "kill" {
        return kill_command(root, args.collect(), terminals);
    }
    if area == "clean" {
        return clean_command(root, args.collect(), terminals);
    }
    if area == "focus" {
        return focus_command(root, args.collect(), terminals);
    }
    let Some(command) = args.next() else {
        return usage();
    };
    let rest: Vec<String> = args.collect();

    match area.as_str() {
        "task" => match command.as_str() {
            "new" => create_new_task(root, rest),
            "list" => list_task_command(root, rest),
            "show" => show_task_command(root, rest),
            "plan" => plan_task_command(root, rest),
            "edit" => edit_task_command(root, rest),
            "path" => task_path_command(root, rest),
            "archive" => archive_task_command(root, rest),
            _ => usage(),
        },
        "roles" if command == "list" => roles_list(root, rest),
        "vendors" if command == "list" && rest.is_empty() => vendors_list(root),
        "run" => run_command(root, command, rest, terminals),
        "approve" if rest.len() == 1 && rest[0] == "plan" => {
            crate::workflow::approve_plan(root, &command)?;
            println!("approved plan for task {command}; rerun shepherd run to start coders");
            Ok(())
        }
        "approve" if rest.len() == 1 => approve_command(root, &command, &rest[0]),
        "mcp" if command == "serve" && rest.len() <= 1 => {
            let port = rest
                .first()
                .map(String::as_str)
                .unwrap_or("0")
                .parse::<u16>()?;
            crate::mcp::serve_with_terminals(port, terminals)
        }
        "config" if command == "path" && rest.is_empty() => config_paths(root),
        "config" if command == "validate" && rest.is_empty() => config_validate(root),
        _ => usage(),
    }
}

fn agents_list(args: Vec<String>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json = match args.as_slice() {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return usage(),
    };
    let registry = load_agent_registry(config::state_dir().join("agents.json"))?;
    if json {
        println!("{}", agents_json(&registry)?);
        return Ok(());
    }
    if registry.agents.is_empty() {
        println!("no agents");
        return Ok(());
    }
    println!("ID\tSTATE\tROLE\tVENDOR\tMODEL\tTASK");
    for agent in registry.agents {
        println!(
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
        );
    }
    Ok(())
}

fn spawn_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
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
    println!(
        "started {} ({}/{}) in Herdr pane {}",
        spawned.agent.id,
        spawned.agent.vendor,
        if spawned.agent.model.is_empty() {
            "default"
        } else {
            &spawned.agent.model
        },
        spawned.agent.pane
    );
    if !spawned.agent.worktree.is_empty() {
        println!(
            "worktree {} ({})",
            spawned.agent.worktree, spawned.agent.branch
        );
    }
    Ok(())
}

fn kill_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut id = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if !arg.starts_with('-') && id.is_none() {
            id = Some(arg);
        }
    }
    let Some(id) = id else {
        return Err("usage: shepherd kill <id> [--json]".into());
    };
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
        println!("{}", serde_json::to_string_pretty(&agent.public())?);
    } else {
        println!("killed agent {} ({})", agent.id, agent.state);
    }
    Ok(())
}

fn clean_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ids = Vec::new();
    let mut workspace = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--json" {
            json = true;
        } else if arg == "--workspace" {
            i += 1;
            workspace = args.get(i).cloned();
        } else if !arg.starts_with('-') {
            ids.push(arg.clone());
        }
        i += 1;
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
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "cleaned": cleaned }))?);
    } else {
        println!("cleaned {} agent(s): {}", cleaned.len(), cleaned.join(", "));
    }
    Ok(())
}

fn focus_command(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut id = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if !arg.starts_with('-') && id.is_none() {
            id = Some(arg);
        }
    }
    let Some(id) = id else {
        return Err("usage: shepherd focus <id> [--json]".into());
    };
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
        println!("{}", serde_json::to_string_pretty(&agent.public())?);
    } else {
        println!("focused agent {} in native herdr terminal", agent.id);
    }
    Ok(())
}

fn run_command(
    root: PathBuf,
    reference: String,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut mcp_url = None;
    let mut i = 0;
    while i < args.len() {
        let option = &args[i];
        i += 1;
        if option != "--mcp-url" {
            return Err(format!("unknown option {option}").into());
        }
        let Some(value) = args.get(i) else {
            return Err("--mcp-url needs a value".into());
        };
        mcp_url = Some(value.clone());
        i += 1;
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
    if report.plan_waiting {
        println!("plan is saved and waiting for human approval; inspect plan.md, run shepherd approve <task> plan, then rerun shepherd run");
        return Ok(());
    }
    println!(
        "started coders: {}",
        if report.started.is_empty() {
            "none".into()
        } else {
            report.started.join(", ")
        }
    );
    println!(
        "results received for subtasks: {}",
        ids(&report.completed_coding)
    );
    println!("failed subtasks: {}", ids(&report.failed));
    println!(
        "needs human/review stage: {}",
        ids(&report.waiting_for_human)
    );
    println!("Rust run currently stops before human approval, push, and PR creation.");
    Ok(())
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
    subtask: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let number = subtask
        .parse::<u32>()
        .map_err(|_| "subtask number must be an integer")?;
    let url = crate::workflow::approve_subtask(root, task, number)?;
    println!("created pull request: {url}");
    Ok(())
}

fn roles_list(
    root: PathBuf,
    args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json = match args.as_slice() {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return usage(),
    };
    let config = load_config(root)?;
    let active = available_vendors(&config);
    if json {
        println!("{}", roles_list_json(&config, &active)?);
        return Ok(());
    }
    for assignment in resolve_all(&config, &active) {
        match assignment {
            Ok(role) => println!(
                "{}\t{}/{}{}",
                role.role,
                role.vendor,
                if role.model.is_empty() {
                    "default"
                } else {
                    &role.model
                },
                if role.fallback { " (fallback)" } else { "" }
            ),
            Err(error) => println!("unavailable\t{error}"),
        }
    }
    Ok(())
}

fn vendors_list(root: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = load_config(root)?;
    let active = available_vendors(&config);
    let mut names: Vec<_> = config.vendors.keys().collect();
    names.sort();
    for name in names {
        let vendor = &config.vendors[name];
        let state = if vendor.disabled {
            "disabled"
        } else if active.contains(name) {
            "available"
        } else {
            "not installed"
        };
        println!("{name}\t{}\t{state}", vendor.command);
    }
    Ok(())
}

fn config_paths(root: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("global\t{}", config::global_config_path().display());
    println!("project\t{}", config::project_config_path(&root).display());
    println!("state\t{}", config::state_dir().display());
    Ok(())
}

fn config_validate(root: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = load_config(root)?;
    let issues = validate(&config);
    if issues.is_empty() {
        println!("configuration is valid");
        return Ok(());
    }
    for issue in issues {
        eprintln!("- {issue}");
    }
    Err("configuration validation failed".into())
}

fn attach_to_herdr(
    root: PathBuf,
    args: Vec<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (target, takeover) = match args.as_slice() {
        [] => (None, false),
        [target] => (Some(target.as_str()), false),
        [target, flag] if flag == "--takeover" => (Some(target.as_str()), true),
        _ => return Err("usage: shepherd attach [herdr-agent-name-or-pane-id] [--takeover]".into()),
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
    let task = create_task(root, input)?;
    println!("created task {}-{}", task.status.id, task.status.slug);
    println!("{}", task.directory.display());
    Ok(())
}

fn list_task_command(
    root: PathBuf,
    args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut archived = false;
    let mut json = false;
    for arg in &args {
        match arg.as_str() {
            "--archived" => archived = true,
            "--json" => json = true,
            _ => return usage(),
        }
    }
    let tasks = list_tasks(root, archived)?;
    if json {
        println!("{}", task_list_json(&tasks)?);
        return Ok(());
    }
    if tasks.is_empty() {
        println!("no {}tasks", if archived { "archived " } else { "" });
        return Ok(());
    }
    println!("ID\tSTATE\tTITLE\tISSUE\tPR");
    for task in tasks {
        let issue = task
            .status
            .issue
            .map(|i| format!("#{}", i.number))
            .unwrap_or_else(|| "-".into());
        let pr = if task.status.pr == 0 {
            "-".into()
        } else {
            format!("#{}", task.status.pr)
        };
        println!(
            "{}\t{}\t{}\t{}\t{}",
            task.status.id, task.status.state, task.status.title, issue, pr
        );
    }
    Ok(())
}

fn show_task_command(
    root: PathBuf,
    args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if arg.starts_with('-') {
            return usage();
        } else {
            positional.push(arg);
        }
    }
    if !(1..=2).contains(&positional.len()) {
        return usage();
    }
    let reference = &positional[0];
    let task = find_task(root, reference)?;
    if json {
        println!(
            "{}",
            task_show_json(&task, positional.get(1).map(String::as_str))?
        );
        return Ok(());
    }
    if let Some(section) = positional.get(1) {
        print!("{}", read_task_section(&task, section)?);
    } else {
        print!("{}", task_context(&task, 0));
    }
    Ok(())
}

fn plan_task_command(
    root: PathBuf,
    args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reference, save) = match args.as_slice() {
        [reference] => (reference.as_str(), false),
        [reference, flag] if flag == "--save" => (reference.as_str(), true),
        _ => return usage(),
    };
    let mut task = find_task(root, reference)?;
    let plan = read_task_section(&task, "plan")?;
    if plan.trim().is_empty() || plan.trim() == "# Plan" {
        return Err("the task has no plan yet; write one to its plan section first".into());
    }
    let subtasks = parse_plan(&plan, &task.status.title);
    validate_plan(&subtasks)?;
    let mut scheduled = subtasks.clone();
    let round = schedule_subtasks(&mut scheduled, &[], usize::MAX);
    println!(
        "task {}: {} subtask(s), initial ready: {}",
        task.status.id,
        subtasks.len(),
        round
            .ready
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
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
        println!(
            "{}\t{}\tdepends: {}\tverify: {}",
            subtask.number,
            subtask.title,
            deps,
            subtask.verify.join(" && ")
        );
    }
    if save {
        task.status.subtasks = subtasks;
        task.status.state = "planning".into();
        save_task_status(task.directory.join("status.json"), &task.status)?;
        println!("saved validated plan to status.json");
    }
    Ok(())
}

fn edit_task_command(
    root: PathBuf,
    args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !(1..=2).contains(&args.len()) {
        return usage();
    }
    let task = find_task(root, &args[0])?;
    let section = args.get(1).map(String::as_str).unwrap_or("spec");
    let path = task_section_path(&task, section)?;
    if !path.exists() {
        write_task_section(&task, section, &format!("# {section}\n\n"))?;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        println!("{}", path.display());
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let [reference] = args.as_slice() else {
        return usage();
    };
    let task = find_task(root, reference)?;
    println!("{}", task.directory.display());
    Ok(())
}

fn archive_task_command(
    root: PathBuf,
    args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let [reference] = args.as_slice() else {
        return usage();
    };
    let path = archive_task(root, reference)?;
    println!("archived {}", path.display());
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
    Err("usage: shepherd attach [herdr-agent-name-or-pane-id] [--takeover] | shepherd mcp serve [port] | shepherd run <task-id> --mcp-url <http://127.0.0.1:PORT/mcp> | shepherd approve <task-id> plan|<subtask-number> | shepherd spawn <role> <task...> [--task-id <id>] [--vendor <name>] [--model <name>] [--mcp-url <http://127.0.0.1:PORT/mcp>] | shepherd agents [--json] | shepherd roles list [--json] | shepherd vendors list | shepherd config path|validate | shepherd task new <title> [--description <text>] [--criteria <text>]... [--id <number>] | task list [--archived] [--json] | task show <id-or-name> [section] [--json] | task plan <id-or-name> [--save] | task edit <id-or-name> [section] | task path <id-or-name> | task archive <id-or-name>".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminals::{herdr_attach_args, InMemoryTerminals};

    #[test]
    fn attach_delegates_to_the_selected_herdr_session_and_agent() {
        assert_eq!(
            herdr_attach_args("shepherd", Some("coder-1"), true),
            ["--session", "shepherd", "agent", "attach", "coder-1", "--takeover"]
        );
        assert_eq!(
            herdr_attach_args("-", Some("w1:p2"), false),
            ["agent", "attach", "w1:p2"]
        );
        assert_eq!(
            herdr_attach_args("shepherd", None, false),
            ["session", "attach", "shepherd"]
        );
        assert!(herdr_attach_args("-", None, false).is_empty());
    }

    #[test]
    fn plan_command_validates_and_persists_subtasks_in_go_compatible_status() {
        use crate::{create_task, find_task, write_task_section, NewTask};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("shepherd-plan-command-{}-{nonce}", std::process::id()));
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
        run_cli_in_root(root.clone(), ["task", "plan", "1", "--save"], None).unwrap();
        let saved = find_task(&root, "1").unwrap();
        assert_eq!(saved.status.subtasks.len(), 2);
        assert_eq!(saved.status.subtasks[1].depends_on, [1]);
        assert_eq!(saved.status.subtasks[0].verify, ["cargo test"]);
        assert_eq!(saved.status.state, "planning");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_json_flags_and_injected_terminals_are_accepted() {
        use crate::{create_task, NewTask};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("shepherd-json-command-{}-{nonce}", std::process::id()));
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

        run_cli_in_root(root.clone(), ["agents", "--json"], None).unwrap();
        run_cli_in_root(root.clone(), ["task", "list", "--json"], None).unwrap();
        run_cli_in_root(
            root.clone(),
            ["task", "list", "--archived", "--json"],
            None,
        )
        .unwrap();
        run_cli_in_root(root.clone(), ["task", "show", "1", "--json"], None).unwrap();
        run_cli_in_root(
            root.clone(),
            ["task", "show", "1", "spec", "--json"],
            None,
        )
        .unwrap();
        run_cli_in_root(root.clone(), ["roles", "list", "--json"], None).unwrap();

        let fake = Arc::new(InMemoryTerminals::new());
        let terminals_dyn: Arc<dyn Terminals> = Arc::clone(&fake) as Arc<dyn Terminals>;
        run_cli_in_root(
            root.clone(),
            ["clean", "--json"],
            Some(Arc::clone(&terminals_dyn)),
        )
        .unwrap();
        run_cli_in_root(
            root.clone(),
            ["attach", "coder-1", "--takeover"],
            Some(terminals_dyn),
        )
        .unwrap();

        std::fs::remove_dir_all(root).unwrap();
    }
}
