//! Reusable CLI entrypoint for Shepherd (`shepherd <subcommand>` and `shepherd shepherd <subcommand>`).

pub mod patterns;
pub mod runtime;
pub mod tasks;
pub mod topics;

#[cfg(test)]
mod tests;

pub use patterns::{pattern_edit_command, pattern_new_command};
pub use runtime::{attach_to_shepherd, config_paths, config_validate, roles_list, vendors_list};
pub use tasks::{edit_task_command, plan_task_command, show_task_command, task_path_command};

use crate::shepherd::{self, ShepherdClient};
use crate::terminals::Terminals;
use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
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

    match dispatch_cli(
        root,
        clean_args,
        terminals,
        server_client.as_ref(),
        json,
        writer,
    ) {
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
        return runtime::agents_command(root, server, json, writer);
    }
    if area == "spawn" {
        return runtime::spawn_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "kill" {
        return runtime::kill_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "clean" {
        return runtime::clean_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "focus" {
        return runtime::focus_command(root, args_iter.collect(), terminals, server, json, writer);
    }
    if area == "git" {
        return runtime::git_command(root, args_iter.collect(), server, json, writer);
    }
    if area == "messages" || area == "msg" {
        return Err("operation not available (service does not support msg/messages)".into());
    }
    let Some(command) = args_iter.next() else {
        if area == "tasks" {
            return tasks::list_task_command(root, Vec::new(), server, json, writer);
        }
        if area == "patterns" || area == "pattern" {
            return patterns::patterns_list_command(root, Vec::new(), server, json, writer);
        }
        if area == "topics" || area == "topic" {
            return topics::topic_list_command(root, Vec::new(), server, json, writer);
        }
        return usage();
    };
    let rest: Vec<String> = args_iter.collect();

    match area.as_str() {
        "topic" => match command.as_str() {
            "list" => topics::topic_list_command(root, rest, server, json, writer),
            "create" => topics::topic_create_command(root, rest, server, json, writer),
            "publish" => topics::topic_publish_command(root, rest, server, json, writer),
            "tail" => topics::topic_tail_command(root, rest, server, json, writer),
            "replay" => topics::topic_replay_command(root, rest, server, json, writer),
            "groups" => topics::topic_groups_command(root, rest, server, json, writer),
            _ => usage(),
        },
        "topics" => {
            let mut list_args = vec![command];
            list_args.extend(rest);
            topics::topic_list_command(root, list_args, server, json, writer)
        }
        "pattern" => match command.as_str() {
            "list" => patterns::patterns_list_command(root, rest, server, json, writer),
            "show" => patterns::pattern_show_command(root, rest, server, json, writer),
            "new" => patterns::pattern_new_command(root, rest, server, json, writer),
            "validate" => patterns::pattern_validate_command(root, rest, server, json, writer),
            "delete" => patterns::pattern_delete_command(root, rest, server, json, writer),
            "edit" => patterns::pattern_edit_command(root, rest, writer),
            _ => usage(),
        },
        "patterns" => {
            let mut list_args = vec![command];
            list_args.extend(rest);
            patterns::patterns_list_command(root, list_args, server, json, writer)
        }
        "task" => match command.as_str() {
            "new" => tasks::create_new_task(root, rest, server, json, writer),
            "list" => tasks::list_task_command(root, rest, server, json, writer),
            "show" => tasks::show_task_command(root, rest, json, writer),
            "plan" => tasks::plan_task_command(root, rest, json, writer),
            "edit" => tasks::edit_task_command(root, rest, writer),
            "path" => tasks::task_path_command(root, rest, json, writer),
            "archive" => tasks::archive_task_command(root, rest, server, json, writer),
            _ => usage(),
        },
        "tasks" => {
            let mut list_args = vec![command];
            list_args.extend(rest);
            tasks::list_task_command(root, list_args, server, json, writer)
        }
        "skills" if command == "list" && rest.is_empty() => {
            runtime::skills_list(root, rest, server, json, writer)
        }
        "runs" if command == "list" && rest.is_empty() => {
            runtime::runs_list(root, rest, server, json, writer)
        }
        "roles" if command == "list" => runtime::roles_list(root, rest, json, writer),
        "vendors" if command == "list" && rest.is_empty() => {
            runtime::vendors_list(root, json, writer)
        }
        "run" => runtime::run_command(root, command, rest, terminals, server, json, writer),
        "approve" if rest.len() == 1 => {
            runtime::approve_command(root, &command, &rest[0], server, json, writer)
        }
        "reject" if !rest.is_empty() => {
            runtime::reject_command(root, &command, rest, server, json, writer)
        }
        "mcp" if command == "serve" && rest.len() <= 1 => {
            let port = rest
                .first()
                .map(String::as_str)
                .unwrap_or("0")
                .parse::<u16>()?;
            crate::mcp::serve_with_terminals(port, terminals)
        }
        "config" if command == "path" && rest.is_empty() => {
            runtime::config_paths(root, json, writer)
        }
        "config" if command == "validate" && rest.is_empty() => {
            runtime::config_validate(root, json, writer)
        }
        _ => usage(),
    }
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

pub(crate) fn usage<T>() -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    Err("usage: shepherd attach [shepherd-agent-name-or-pane-id] [--takeover] | shepherd mcp serve [port] | shepherd run <task-id> [--mcp-url <url>] [--auto-push] [--wait] [--json] | shepherd approve <task-id> plan|<subtask-number> [--json] | shepherd reject <task-id> plan|<subtask-number> [--reason <text>] [--json] | shepherd spawn <role> <task...> [--task-id <id>] [--vendor <name>] [--model <name>] [--mcp-url <url>] [--json] | shepherd agents [--json] | shepherd skills list [--json] | shepherd runs list [--json] | shepherd roles list [--json] | shepherd vendors list [--json] | shepherd config path|validate [--json] | shepherd task new <title> [--description <text>] [--criteria <text>]... [--id <number>] [--json] | task list [--archived] [--json] | task show <id-or-name> [section] [--json] | task plan <id-or-name> [--save] [--json] | task edit <id-or-name> [section] | task path <id-or-name> [--json] | task archive <id-or-name> [--json] | shepherd pattern list [--json] | pattern show <name> [--json] | pattern new <name> --from <template|name> [--global] [--json] | pattern validate <name|--file path> [--json] | pattern delete <name> [--json] | pattern edit <name> | shepherd topic list [--json] | topic create <name> [--partitions <n>] [--json] | topic publish <topic> <content> [--key <key>] [--partition <p>] [--from <from>] [--json] | topic tail <topic> [--partition <p>] [--limit <n>] [--json] | topic replay <topic> [--partition <p>] [--from <offset>] [--limit <n>] [--json] | topic groups [topic] [--json]".into())
}
