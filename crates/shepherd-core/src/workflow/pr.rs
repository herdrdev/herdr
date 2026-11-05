//! Subtask and plan approvals, pull request creation, and rejections.

use super::verification::verify_coder;
use super::{checkpoint, clip};
use crate::{
    config::{self, load_config},
    find_task, load_agent_registry, parse_plan, validate_plan,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn approve_subtask(
    root: PathBuf,
    task_ref: &str,
    number: u32,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let registry_path = config::state_dir().join("agents.json");
    approve_subtask_with_registry(root, task_ref, number, &registry_path)
}

pub fn approve_subtask_with_registry(
    root: PathBuf,
    task_ref: &str,
    number: u32,
    registry_path: &Path,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    approve_subtask_with_runner(root, task_ref, number, registry_path, |command| {
        command.output()
    })
}

pub fn approve_plan(
    root: PathBuf,
    task_ref: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut task = find_task(root, task_ref)?;
    if task.status.waiting != "plan" {
        return Err("task is not waiting for plan approval".into());
    }
    let plan = crate::read_task_section(&task, "plan")?;
    let subtasks = parse_plan(&plan, &task.status.title);
    validate_plan(&subtasks)?;
    task.status.subtasks = subtasks;
    task.status.waiting.clear();
    task.status.state = "coding".into();
    task.status.note = "plan approved by human; rerun `shepherd run` to start coders".into();
    checkpoint(&mut task.status, &task.directory)
}

pub fn reject_plan(
    root: PathBuf,
    task_ref: &str,
    reason: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut task = find_task(root, task_ref)?;
    if task.status.waiting != "plan" {
        return Err("task is not waiting for plan approval".into());
    }
    task.status.waiting = "plan".into();
    task.status.state = "planning".into();
    let reason_str = reason.unwrap_or("plan rejected by human; revisions requested");
    task.status.note = format!("plan rejected by human: {reason_str}");
    checkpoint(&mut task.status, &task.directory)
}

pub fn reject_subtask(
    root: PathBuf,
    task_ref: &str,
    number: u32,
    reason: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut task = find_task(root, task_ref)?;
    let index = task
        .status
        .subtasks
        .iter()
        .position(|subtask| subtask.number == number)
        .ok_or_else(|| format!("task has no subtask {number}"))?;
    let subtask = &mut task.status.subtasks[index];
    if subtask.state != "awaiting-approval" {
        return Err(format!(
            "subtask {number} is in state {:?}; only subtasks in awaiting-approval can be rejected",
            subtask.state
        )
        .into());
    }
    subtask.state = "pending".into();
    let reason_str = reason.unwrap_or("changes requested");
    subtask.note = Some(format!("rejected by human: {reason_str}"));
    task.status.state = "coding".into();
    task.status.note = format!("subtask {number} rejected by human: {reason_str}");
    checkpoint(&mut task.status, &task.directory)
}

pub fn approve_subtask_with_runner(
    root: PathBuf,
    task_ref: &str,
    number: u32,
    registry_path: &Path,
    mut run_command: impl FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut task = find_task(&root, task_ref)?;
    let config = load_config(&root)?;
    let index = task
        .status
        .subtasks
        .iter()
        .position(|subtask| subtask.number == number)
        .ok_or_else(|| format!("task has no subtask {number}"))?;
    let subtask = task.status.subtasks[index].clone();
    if subtask.state != "awaiting-approval" {
        return Err(format!(
            "subtask {number} is {:?}; only reviewed subtasks in awaiting-approval can be approved",
            subtask.state
        )
        .into());
    }
    if config.workflow.max_review_rounds > 0 && subtask.reviewer.is_empty() {
        return Err(format!("subtask {number} has no recorded reviewer").into());
    }
    let registry = load_agent_registry(registry_path)?;
    let coder = registry
        .result(&subtask.coder)
        .ok_or_else(|| format!("coder {:?} is missing from agents.json", subtask.coder))?;
    if coder.branch.is_empty() || coder.branch != subtask.branch {
        return Err("task branch does not match the coder registry".into());
    }
    if config.limits.protected_branches.contains(&coder.branch) {
        return Err(format!("refusing to push protected branch {:?}", coder.branch).into());
    }
    if subtask
        .verify
        .iter()
        .chain(config.workflow.checks.iter())
        .any(|command| !command.trim().is_empty())
    {
        if let Some(problem) = verify_coder(
            coder,
            &config.workflow.base_branch,
            subtask
                .verify
                .iter()
                .chain(config.workflow.checks.iter())
                .cloned(),
        ) {
            return Err(format!("approval blocked: {problem}").into());
        }
    } else if let Some(problem) =
        verify_coder(coder, &config.workflow.base_branch, std::iter::empty())
    {
        return Err(format!("approval blocked: {problem}").into());
    }

    let mut push_command = Command::new("git");
    push_command
        .args(["push", "--set-upstream", "origin", &coder.branch])
        .current_dir(&coder.worktree);
    let push = run_command(&mut push_command)?;
    if !push.status.success() {
        let detail = clip(
            &format!(
                "{}{}",
                String::from_utf8_lossy(&push.stdout),
                String::from_utf8_lossy(&push.stderr)
            ),
            1000,
        );
        task.status.subtasks[index].note = Some(format!("push failed: {detail}"));
        checkpoint(&mut task.status, &task.directory)?;
        return Err(format!("push failed: {detail}").into());
    }

    let base = crate::mcp::default_base(&root.display().to_string(), &config.workflow.base_branch);
    let title = if subtask.title.trim().is_empty() {
        task.status.title.clone()
    } else {
        subtask.title.clone()
    };
    let body = pr_body(&task, &subtask, &config.workflow.checks);
    let mut command = Command::new("gh");
    command.args([
        "pr",
        "create",
        "--base",
        &base,
        "--head",
        &coder.branch,
        "--title",
        &title,
        "--body",
        &body,
    ]);
    if config.workflow.draft {
        command.arg("--draft");
    }
    let created = match run_command(command.current_dir(&root)) {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            let detail = clip(
                &format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
                1000,
            );
            task.status.subtasks[index].note =
                Some(format!("branch pushed, but PR creation failed: {detail}"));
            checkpoint(&mut task.status, &task.directory)?;
            return Err(format!(
                "branch {} was pushed, but PR creation failed: {detail}",
                coder.branch
            )
            .into());
        }
        Err(error) => {
            task.status.subtasks[index].note =
                Some(format!("branch pushed, but `gh` could not run: {error}"));
            checkpoint(&mut task.status, &task.directory)?;
            return Err(format!(
                "branch {} was pushed, but `gh` could not run: {error}",
                coder.branch
            )
            .into());
        }
    };
    let url = String::from_utf8_lossy(&created.stdout).trim().to_owned();
    if url.is_empty() {
        task.status.subtasks[index].note = Some(
            "gh pr create returned no URL; verify the remote pull request before retrying".into(),
        );
        checkpoint(&mut task.status, &task.directory)?;
        return Err("gh pr create succeeded but returned no pull request URL".into());
    }
    task.status.subtasks[index].state = "pr".into();
    task.status.subtasks[index].pr = url.clone();
    task.status.subtasks[index].note = None;
    task.status.note = format!("subtask {number} pull request created: {url}");
    checkpoint(&mut task.status, &task.directory)?;
    Ok(url)
}

pub(crate) fn pr_body(
    task: &crate::CreatedTask,
    subtask: &crate::Subtask,
    checks: &[String],
) -> String {
    let mut body = String::new();
    if let Some(issue) = &task.status.issue {
        body.push_str(&format!("Closes #{}\n\n", issue.number));
    }
    body.push_str(&format!(
        "## {}\n\nTask {} - subtask {}\n\n",
        subtask.title, task.status.id, subtask.number
    ));
    if !subtask.body.trim().is_empty() {
        body.push_str(&format!("### Change\n{}\n\n", subtask.body.trim()));
    }
    body.push_str(&format!(
        "### Review\nApproved by {}.\n\n",
        if subtask.reviewer.is_empty() {
            "(review disabled)"
        } else {
            &subtask.reviewer
        }
    ));
    let mut seen = HashSet::new();
    let commands: Vec<_> = subtask
        .verify
        .iter()
        .chain(checks.iter())
        .map(|command| command.trim())
        .filter(|command| !command.is_empty() && seen.insert(*command))
        .collect();
    if !commands.is_empty() {
        body.push_str("### Checks passed\n");
        for command in commands {
            body.push_str(&format!("- `{command}`\n"));
        }
        body.push('\n');
    }
    body.push_str("Generated by Shepherd.\n");
    body
}
