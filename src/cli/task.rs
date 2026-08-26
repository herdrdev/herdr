use crate::api::schema::{
    TaskAttachParams, TaskCreateParams, TaskDispatchParams, TaskReportParams, TaskStatus,
    TaskUpdateParams,
};

pub(super) fn run_task_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("board") => {
            if args.len() != 1 {
                eprintln!("usage: herdr task board");
                Ok(2)
            } else {
                super::runtime::task_board()
            }
        }
        Some("list") => task_list(&args[1..]),
        Some("get") => task_get(&args[1..]),
        Some("create") => task_create(&args[1..]),
        Some("update") => task_update(&args[1..]),
        Some("attach") => task_attach(&args[1..]),
        Some("dispatch") => task_dispatch(&args[1..]),
        Some("report") => task_report(&args[1..]),
        Some("help" | "--help" | "-h") | None => {
            print_task_help();
            Ok(if args.is_empty() { 2 } else { 0 })
        }
        Some(other) => {
            eprintln!("unknown task command: {other}");
            print_task_help();
            Ok(2)
        }
    }
}

fn task_list(args: &[String]) -> std::io::Result<i32> {
    let mut status = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                status = match parse_status(value) {
                    Ok(status) => Some(status),
                    Err(message) => {
                        eprintln!("{message}");
                        return Ok(2);
                    }
                };
                index += 2;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    super::runtime::task_list(status)
}

fn task_get(args: &[String]) -> std::io::Result<i32> {
    let Some(task_id) = args.first() else {
        eprintln!("usage: herdr task get TASK_ID");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr task get TASK_ID");
        return Ok(2);
    }
    super::runtime::task_get(task_id.clone())
}

fn task_create(args: &[String]) -> std::io::Result<i32> {
    let mut title = None;
    let mut description = String::new();
    let mut priority = 100;
    let mut dependencies = Vec::new();
    let mut cwd = None;
    let mut agent_session_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --title");
                    return Ok(2);
                };
                title = Some(value.clone());
                index += 2;
            }
            "--description" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --description");
                    return Ok(2);
                };
                description = value.clone();
                index += 2;
            }
            "--priority" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --priority");
                    return Ok(2);
                };
                priority = match value.parse::<u8>() {
                    Ok(priority) => priority,
                    Err(_) => {
                        eprintln!("priority must be an integer from 0 to 255");
                        return Ok(2);
                    }
                };
                index += 2;
            }
            "--depends-on" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --depends-on");
                    return Ok(2);
                };
                dependencies.push(value.clone());
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--agent-session-id" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent-session-id");
                    return Ok(2);
                };
                agent_session_id = Some(value.clone());
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let Some(title) = title else {
        eprintln!(
            "usage: herdr task create --title TEXT [--description TEXT] [--depends-on ID] [--agent-session-id ID]"
        );
        return Ok(2);
    };
    super::runtime::task_create(TaskCreateParams {
        title,
        description,
        priority,
        dependencies,
        cwd,
        agent_session_id,
    })
}

fn task_update(args: &[String]) -> std::io::Result<i32> {
    let Some(task_id) = args.first().cloned() else {
        eprintln!("usage: herdr task update TASK_ID [options]");
        return Ok(2);
    };
    let mut title = None;
    let mut description = None;
    let mut priority = None;
    let mut status = None;
    let mut message = None;
    let mut index = 1;
    while index < args.len() {
        let value = |index: usize, name: &str| -> Option<String> {
            args.get(index + 1).cloned().or_else(|| {
                eprintln!("missing value for {name}");
                None
            })
        };
        match args[index].as_str() {
            "--title" => {
                title = value(index, "--title");
                if title.is_none() {
                    return Ok(2);
                }
                index += 2;
            }
            "--description" => {
                description = value(index, "--description");
                if description.is_none() {
                    return Ok(2);
                }
                index += 2;
            }
            "--priority" => {
                let Some(raw) = value(index, "--priority") else {
                    return Ok(2);
                };
                priority = match raw.parse::<u8>() {
                    Ok(priority) => Some(priority),
                    Err(_) => {
                        eprintln!("priority must be an integer from 0 to 255");
                        return Ok(2);
                    }
                };
                index += 2;
            }
            "--status" => {
                let Some(raw) = value(index, "--status") else {
                    return Ok(2);
                };
                status = match parse_status(&raw) {
                    Ok(status) => Some(status),
                    Err(message) => {
                        eprintln!("{message}");
                        return Ok(2);
                    }
                };
                index += 2;
            }
            "--message" => {
                message = value(index, "--message");
                if message.is_none() {
                    return Ok(2);
                }
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    super::runtime::task_update(TaskUpdateParams {
        task_id,
        title,
        description,
        priority,
        status,
        message,
    })
}

fn task_attach(args: &[String]) -> std::io::Result<i32> {
    if args.len() != 2 {
        eprintln!("usage: herdr task attach TASK_ID AGENT_OR_PANE");
        return Ok(2);
    }
    super::runtime::task_attach(TaskAttachParams {
        task_id: args[0].clone(),
        target: args[1].clone(),
    })
}

fn task_dispatch(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr task dispatch TASK_ID AGENT_OR_PANE [--prompt TEXT]");
        return Ok(2);
    }
    let task_id = args[0].clone();
    let target = args[1].clone();
    let mut prompt = None;
    let mut index = 2;
    while index < args.len() {
        if args[index] != "--prompt" {
            eprintln!("unknown option: {}", args[index]);
            return Ok(2);
        }
        let Some(value) = args.get(index + 1) else {
            eprintln!("missing value for --prompt");
            return Ok(2);
        };
        prompt = Some(value.clone());
        index += 2;
    }
    super::runtime::task_dispatch(TaskDispatchParams {
        task_id,
        target,
        prompt,
    })
}

fn task_report(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr task report TASK_ID STATUS [--message TEXT]");
        return Ok(2);
    }
    let status = match parse_status(&args[1]) {
        Ok(status) => status,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let mut message = None;
    if args.len() > 2 {
        if args.len() != 4 || args[2] != "--message" {
            eprintln!("usage: herdr task report TASK_ID STATUS [--message TEXT]");
            return Ok(2);
        }
        message = Some(args[3].clone());
    }
    super::runtime::task_report(TaskReportParams {
        task_id: args[0].clone(),
        status,
        message,
    })
}

fn parse_status(raw: &str) -> Result<TaskStatus, String> {
    match raw {
        "backlog" => Ok(TaskStatus::Backlog),
        "ready" => Ok(TaskStatus::Ready),
        "running" => Ok(TaskStatus::Running),
        "blocked" => Ok(TaskStatus::Blocked),
        "review" => Ok(TaskStatus::Review),
        "done" => Ok(TaskStatus::Done),
        "failed" => Ok(TaskStatus::Failed),
        "cancelled" | "canceled" => Ok(TaskStatus::Cancelled),
        _ => Err(format!("unknown task status: {raw}")),
    }
}

fn print_task_help() {
    eprintln!("usage: herdr task <board|list|get|create|update|attach|dispatch|report>");
    eprintln!("  board                                      show tasks grouped by lifecycle state");
    eprintln!("  create --title TEXT [--agent-session-id ID]    create a durable task");
    eprintln!("  attach TASK_ID AGENT_OR_PANE              attach a live agent to a task");
    eprintln!("  dispatch TASK_ID AGENT_OR_PANE            attach and send the task prompt");
    eprintln!("  report TASK_ID STATUS [--message TEXT]    record agent or reviewer progress");
}
