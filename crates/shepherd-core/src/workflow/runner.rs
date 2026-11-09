//! Orchestrator for scheduling subtasks, running coders, and invoking reviews.

use super::verification::{mcp_role_assignment_from, review_and_check};
use super::{
    checkpoint, has_content, parse_timeout, validate_mcp_url, wait_for_result, ReviewOutcome,
    RunReport, RunRequest,
};
use crate::{
    config::{self, available_vendors, load_config},
    find_task, load_agent_registry, parse_plan, schedule_subtasks,
    spawn::{self, SpawnOptions, SpawnRequest},
    terminals::{connect_terminals, Terminals},
    validate_plan,
};
use std::{path::PathBuf, time::Duration};

struct ActiveCoder {
    number: u32,
    id: String,
}

pub fn run_coders(
    request: RunRequest,
) -> Result<RunReport, Box<dyn std::error::Error + Send + Sync>> {
    validate_mcp_url(&request.mcp_url)?;
    let config = load_config(&request.root)?;
    let terminals = connect_terminals(&config)?;
    run_coders_with_terminals(request, terminals.as_ref(), None, None)
}

pub fn run_coders_with_terminals(
    request: RunRequest,
    terminals: &dyn Terminals,
    registry_path_override: Option<PathBuf>,
    active_vendors_override: Option<Vec<String>>,
) -> Result<RunReport, Box<dyn std::error::Error + Send + Sync>> {
    validate_mcp_url(&request.mcp_url)?;
    let config = load_config(&request.root)?;
    let registry_path =
        registry_path_override.unwrap_or_else(|| config::state_dir().join("agents.json"));
    let active_vendors = active_vendors_override.unwrap_or_else(|| available_vendors(&config));
    let mut task = find_task(&request.root, &request.task)?;
    if task.status.waiting == "plan" {
        return Ok(RunReport {
            plan_waiting: true,
            ..RunReport::default()
        });
    }
    if task.status.subtasks.is_empty() {
        let mut plan = crate::read_task_section(&task, "plan")?;
        if !has_content(&plan) {
            let (vendor, model) = mcp_role_assignment_from(&config, "brain", &active_vendors)?;
            task.status.state = "planning".into();
            task.status.note = "brain is preparing a plan".into();
            checkpoint(&mut task.status, &task.directory)?;
            let brain_prompt = format!("Plan task {}. Read its context and codebase. Split all acceptance criteria into independent, numbered subtasks using headings like `### 1. Short title`; include Files, Depends on, Done when, and exact Verify commands. Save the complete plan with update_task_context(section=\"plan\", content=...). Do not write code or start coders. Finish by calling submit_result with a short confirmation.", task.status.id);
            let brain = spawn::spawn_with_options(
                SpawnRequest {
                    root: request.root.clone(),
                    role: "brain".into(),
                    task: brain_prompt,
                    task_id: Some(task.status.id.to_string()),
                    vendor: Some(vendor),
                    model: Some(model),
                    mcp_url: Some(request.mcp_url.clone()),
                },
                SpawnOptions {
                    parent_id: None,
                    registry_path: Some(registry_path.clone()),
                    active_vendors: Some(active_vendors.clone()),
                    terminals,
                },
            )?;
            task.status.agents.push(brain.agent.id.clone());
            task.status.note = format!("waiting for plan from {}", brain.agent.id);
            checkpoint(&mut task.status, &task.directory)?;
            match wait_for_result(
                &registry_path,
                &brain.agent.id,
                parse_timeout(&config.limits.agent_timeout).unwrap_or(Duration::from_secs(30 * 60)),
            ) {
                Ok(result) => {
                    plan = crate::read_task_section(&task, "plan")?;
                    if !has_content(&plan) {
                        plan = format!("# Plan\n\n{result}\n");
                        crate::write_task_section(&task, "plan", &plan)?;
                    }
                }
                Err(error) => {
                    task.status.note = format!("brain did not deliver a plan: {error}");
                    checkpoint(&mut task.status, &task.directory)?;
                    return Err(task.status.note.clone().into());
                }
            }
        }
        task.status.subtasks = parse_plan(&plan, &task.status.title);
    }
    validate_plan(&task.status.subtasks)?;
    if config.workflow.confirm
        && !matches!(
            task.status.state.as_str(),
            "coding" | "reviewing" | "pr" | "done"
        )
    {
        task.status.waiting = "plan".into();
        task.status.state = "planning".into();
        task.status.note = "plan is ready for human approval; run `shepherd approve <task> plan`, then rerun `shepherd run`".into();
        checkpoint(&mut task.status, &task.directory)?;
        return Ok(RunReport {
            plan_waiting: true,
            ..RunReport::default()
        });
    }
    let mut report = RunReport {
        waiting_for_human: task
            .status
            .subtasks
            .iter()
            .filter(|subtask| subtask.state == "needs-human")
            .map(|subtask| subtask.number)
            .collect(),
        failed: task
            .status
            .subtasks
            .iter()
            .filter(|subtask| subtask.state == "failed")
            .map(|subtask| subtask.number)
            .collect(),
        ..RunReport::default()
    };
    let timeout =
        parse_timeout(&config.limits.agent_timeout).unwrap_or(Duration::from_secs(30 * 60));

    loop {
        let registry = load_agent_registry(&registry_path)?;
        let capacity = config
            .limits
            .max_concurrent_agents
            .saturating_sub(registry.live().len());
        if capacity == 0 {
            return Err(format!("agent limit reached ({} live; limits.max_concurrent_agents={}); stop or wait for an agent, then rerun", registry.live().len(), config.limits.max_concurrent_agents).into());
        }
        let old_status = task.status.clone();
        let round = schedule_subtasks(&mut task.status.subtasks, &[], capacity);
        for (number, _) in &round.dependency_failures {
            if !report.failed.contains(number) {
                report.failed.push(*number);
            }
        }
        if task.status.subtasks != old_status.subtasks {
            checkpoint(&mut task.status, &task.directory)?;
        }
        if round.ready.is_empty() {
            break;
        }

        let mut active = Vec::new();
        for number in round.ready {
            let subtask = task
                .status
                .subtasks
                .iter()
                .find(|subtask| subtask.number == number)
                .expect("scheduled subtask exists")
                .clone();
            if let Some(item) = task
                .status
                .subtasks
                .iter_mut()
                .find(|subtask| subtask.number == number)
            {
                item.state = "coding".into();
                item.note = None;
            }
            checkpoint(&mut task.status, &task.directory)?;
            let verify = if subtask.verify.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\n## Verify commands\n\n{}",
                    subtask
                        .verify
                        .iter()
                        .map(|command| format!("- `{command}`"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            let prompt = format!("Implement plan subtask {}: {}\n\n{}{}\n\nWhen done, commit your changes, report the commands you ran, and call submit_result with a concise summary.", subtask.number, subtask.title, subtask.body, verify);
            let (vendor, model) = mcp_role_assignment_from(&config, "coder", &active_vendors)?;
            match spawn::spawn_with_options(
                SpawnRequest {
                    root: request.root.clone(),
                    role: "coder".into(),
                    task: prompt,
                    task_id: Some(task.status.id.to_string()),
                    vendor: Some(vendor),
                    model: Some(model),
                    mcp_url: Some(request.mcp_url.clone()),
                },
                SpawnOptions {
                    parent_id: None,
                    registry_path: Some(registry_path.clone()),
                    active_vendors: Some(active_vendors.clone()),
                    terminals,
                },
            ) {
                Ok(spawned) => {
                    if let Some(item) = task
                        .status
                        .subtasks
                        .iter_mut()
                        .find(|item| item.number == number)
                    {
                        item.coder = spawned.agent.id.clone();
                        item.branch = spawned.agent.branch.clone();
                    }
                    if !task.status.agents.contains(&spawned.agent.id) {
                        task.status.agents.push(spawned.agent.id.clone());
                    }
                    report.started.push(spawned.agent.id.clone());
                    active.push(ActiveCoder {
                        number,
                        id: spawned.agent.id,
                    });
                    checkpoint(&mut task.status, &task.directory)?;
                }
                Err(error) => {
                    if let Some(item) = task
                        .status
                        .subtasks
                        .iter_mut()
                        .find(|item| item.number == number)
                    {
                        item.state = "failed".into();
                        item.note = Some(format!("could not start coder: {error}"));
                    }
                    if !report.failed.contains(&number) {
                        report.failed.push(number);
                    }
                    checkpoint(&mut task.status, &task.directory)?;
                }
            }
        }

        for coder in active {
            let outcome = match wait_for_result(&registry_path, &coder.id, timeout) {
                Ok(result) => {
                    report.completed_coding.push(coder.number);
                    match review_and_check(
                        &request,
                        &config,
                        &task,
                        coder.number,
                        &coder.id,
                        &result,
                        &registry_path,
                        &request.mcp_url,
                        timeout,
                        terminals,
                        &active_vendors,
                    ) {
                        Ok(outcome) => outcome,
                        Err(error) => ReviewOutcome {
                            reviewer: None,
                            state: "needs-human".into(),
                            note: format!("review/check stage could not continue: {error}"),
                        },
                    }
                }
                Err(error) => ReviewOutcome {
                    reviewer: None,
                    state: "needs-human".into(),
                    note: format!("waiting for coder {}: {error}", coder.id),
                },
            };
            let item = task
                .status
                .subtasks
                .iter_mut()
                .find(|item| item.number == coder.number)
                .expect("active subtask exists");
            item.state = outcome.state;
            item.reviewer = outcome.reviewer.clone().unwrap_or_default();
            item.note = Some(outcome.note);
            if !report.waiting_for_human.contains(&coder.number) {
                report.waiting_for_human.push(coder.number);
            }
            if let Some(reviewer) = outcome.reviewer {
                if !task.status.agents.contains(&reviewer) {
                    task.status.agents.push(reviewer);
                }
            }
            checkpoint(&mut task.status, &task.directory)?;
        }
    }

    let complete = !task.status.subtasks.is_empty()
        && task
            .status
            .subtasks
            .iter()
            .all(|subtask| matches!(subtask.state.as_str(), "pr" | "done"));
    task.status.state = if complete {
        "done".into()
    } else {
        "reviewing".into()
    };
    task.status.note = format!("Rust run: {} coder(s) started, {} result(s) received, {} failed; awaiting human approval for push/PR", report.started.len(), report.completed_coding.len(), report.failed.len());
    checkpoint(&mut task.status, &task.directory)?;
    Ok(report)
}
