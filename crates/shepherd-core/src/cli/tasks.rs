//! Task creation, listing, showing, planning, editing, and archiving CLI commands.

use super::usage;
use crate::{
    archive_task, create_task, find_task, list_tasks, parse_plan, read_task_section,
    save_task_status, schedule_subtasks, shepherd::ShepherdClient, task_context, task_list_json,
    task_section_path, task_show_json, touch_task, validate_plan, write_task_section, CreatedTask,
    NewTask,
};
use serde_json::Value;
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;

pub(crate) fn create_new_task(
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
            let dir = data.get("directory").and_then(Value::as_str).unwrap_or("");
            writeln!(writer, "created task {id}-{slug}")?;
            writeln!(writer, "{dir}")?;
        }
        return Ok(());
    }
    let task = create_task(&root, input)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&task)?)?;
    } else {
        writeln!(
            writer,
            "created task {}-{}",
            task.status.id, task.status.slug
        )?;
        writeln!(writer, "{}", task.directory.display())?;
    }
    Ok(())
}

pub(crate) fn list_task_command(
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
        writeln!(
            writer,
            "no {}tasks",
            if archived { "archived " } else { "" }
        )?;
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
        writeln!(
            writer,
            "no {}tasks",
            if archived { "archived " } else { "" }
        )?;
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

pub fn show_task_command(
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

pub fn plan_task_command(
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

pub fn edit_task_command(
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

pub fn task_path_command(
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

pub(crate) fn archive_task_command(
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
