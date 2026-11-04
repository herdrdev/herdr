//! First executable workflow slice: schedule persisted subtasks, run coders through Shepherd,
//! receive MCP results, and checkpoint Go-compatible task status for review-stage continuation.

use crate::{config::{self, available_vendors, load_config, resolve_role}, count_criteria, find_task, load_agent_registry, parse_plan, parse_review, save_agent_registry, save_task_status, schedule_subtasks, spawn::{self, SpawnOptions, SpawnRequest}, terminals::{connect_terminals, Terminals}, validate_plan};
use std::{collections::HashSet, path::{Path, PathBuf}, process::Command, time::{Duration, Instant}};

pub struct RunRequest { pub root: PathBuf, pub task: String, pub mcp_url: String }

#[derive(Default)]
pub struct RunReport { pub started: Vec<String>, pub completed_coding: Vec<u32>, pub waiting_for_human: Vec<u32>, pub failed: Vec<u32>, pub plan_waiting: bool }

struct ActiveCoder { number: u32, id: String }
struct ReviewOutcome { reviewer: Option<String>, state: String, note: String }

pub fn run_coders(request: RunRequest) -> Result<RunReport, Box<dyn std::error::Error + Send + Sync>> {
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
    let registry_path = registry_path_override.unwrap_or_else(|| config::state_dir().join("agents.json"));
    let active_vendors = active_vendors_override.unwrap_or_else(|| available_vendors(&config));
    let mut task = find_task(&request.root, &request.task)?;
    if task.status.waiting == "plan" { return Ok(RunReport { plan_waiting: true, ..RunReport::default() }); }
    if task.status.subtasks.is_empty() {
        let mut plan = crate::read_task_section(&task, "plan")?;
        if !has_content(&plan) {
            let (vendor, model) = mcp_role_assignment_from(&config, "brain", &active_vendors)?;
            task.status.state = "planning".into();
            task.status.note = "brain is preparing a plan".into();
            checkpoint(&mut task.status, &task.directory)?;
            let brain_prompt = format!("Plan task {}. Read its context and codebase. Split all acceptance criteria into independent, numbered subtasks using headings like `### 1. Short title`; include Files, Depends on, Done when, and exact Verify commands. Save the complete plan with update_task_context(section=\"plan\", content=...). Do not write code or start coders. Finish by calling submit_result with a short confirmation.", task.status.id);
            let brain = spawn::spawn_with_options(SpawnRequest { root: request.root.clone(), role: "brain".into(), task: brain_prompt, task_id: Some(task.status.id.to_string()), vendor: Some(vendor), model: Some(model), mcp_url: Some(request.mcp_url.clone()) }, SpawnOptions { parent_id: None, registry_path: Some(registry_path.clone()), active_vendors: Some(active_vendors.clone()), terminals })?;
            task.status.agents.push(brain.agent.id.clone());
            task.status.note = format!("waiting for plan from {}", brain.agent.id);
            checkpoint(&mut task.status, &task.directory)?;
            match wait_for_result(&registry_path, &brain.agent.id, parse_timeout(&config.limits.agent_timeout).unwrap_or(Duration::from_secs(30 * 60))) {
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
    if config.workflow.confirm && !matches!(task.status.state.as_str(), "coding" | "reviewing" | "pr" | "done") {
        task.status.waiting = "plan".into();
        task.status.state = "planning".into();
        task.status.note = "plan is ready for human approval; run `shepherd approve <task> plan`, then rerun `shepherd run`".into();
        checkpoint(&mut task.status, &task.directory)?;
        return Ok(RunReport { plan_waiting: true, ..RunReport::default() });
    }
    let mut report = RunReport::default();
    report.waiting_for_human = task.status.subtasks.iter().filter(|subtask| subtask.state == "needs-human").map(|subtask| subtask.number).collect();
    report.failed = task.status.subtasks.iter().filter(|subtask| subtask.state == "failed").map(|subtask| subtask.number).collect();
    let timeout = parse_timeout(&config.limits.agent_timeout).unwrap_or(Duration::from_secs(30 * 60));

    loop {
        let registry = load_agent_registry(&registry_path)?;
        let capacity = config.limits.max_concurrent_agents.saturating_sub(registry.live().len());
        if capacity == 0 {
            return Err(format!("agent limit reached ({} live; limits.max_concurrent_agents={}); stop or wait for an agent, then rerun", registry.live().len(), config.limits.max_concurrent_agents).into());
        }
        let old_status = task.status.clone();
        let round = schedule_subtasks(&mut task.status.subtasks, &[], capacity);
        for (number, _) in &round.dependency_failures {
            if !report.failed.contains(number) { report.failed.push(*number); }
        }
        if task.status.subtasks != old_status.subtasks { checkpoint(&mut task.status, &task.directory)?; }
        if round.ready.is_empty() { break; }

        let mut active = Vec::new();
        for number in round.ready {
            let subtask = task.status.subtasks.iter().find(|subtask| subtask.number == number).expect("scheduled subtask exists").clone();
            if let Some(item) = task.status.subtasks.iter_mut().find(|subtask| subtask.number == number) { item.state = "coding".into(); item.note = None; }
            checkpoint(&mut task.status, &task.directory)?;
            let verify = if subtask.verify.is_empty() { String::new() } else { format!("\n\n## Verify commands\n\n{}", subtask.verify.iter().map(|command| format!("- `{command}`")).collect::<Vec<_>>().join("\n")) };
            let prompt = format!("Implement plan subtask {}: {}\n\n{}{}\n\nWhen done, commit your changes, report the commands you ran, and call submit_result with a concise summary.", subtask.number, subtask.title, subtask.body, verify);
            let (vendor, model) = mcp_role_assignment_from(&config, "coder", &active_vendors)?;
            match spawn::spawn_with_options(SpawnRequest { root: request.root.clone(), role: "coder".into(), task: prompt, task_id: Some(task.status.id.to_string()), vendor: Some(vendor), model: Some(model), mcp_url: Some(request.mcp_url.clone()) }, SpawnOptions { parent_id: None, registry_path: Some(registry_path.clone()), active_vendors: Some(active_vendors.clone()), terminals }) {
                Ok(spawned) => {
                    if let Some(item) = task.status.subtasks.iter_mut().find(|item| item.number == number) {
                        item.coder = spawned.agent.id.clone(); item.branch = spawned.agent.branch.clone();
                    }
                    if !task.status.agents.contains(&spawned.agent.id) { task.status.agents.push(spawned.agent.id.clone()); }
                    report.started.push(spawned.agent.id.clone());
                    active.push(ActiveCoder { number, id: spawned.agent.id });
                    checkpoint(&mut task.status, &task.directory)?;
                }
                Err(error) => {
                    if let Some(item) = task.status.subtasks.iter_mut().find(|item| item.number == number) { item.state = "failed".into(); item.note = Some(format!("could not start coder: {error}")); }
                    if !report.failed.contains(&number) { report.failed.push(number); }
                    checkpoint(&mut task.status, &task.directory)?;
                }
            }
        }

        for coder in active {
            let outcome = match wait_for_result(&registry_path, &coder.id, timeout) {
                Ok(result) => {
                    report.completed_coding.push(coder.number);
                    match review_and_check(&request, &config, &task, coder.number, &coder.id, &result, &registry_path, &request.mcp_url, timeout, terminals, &active_vendors) {
                        Ok(outcome) => outcome,
                        Err(error) => ReviewOutcome { reviewer: None, state: "needs-human".into(), note: format!("review/check stage could not continue: {error}") },
                    }
                }
                Err(error) => ReviewOutcome { reviewer: None, state: "needs-human".into(), note: format!("waiting for coder {}: {error}", coder.id) },
            };
            let item = task.status.subtasks.iter_mut().find(|item| item.number == coder.number).expect("active subtask exists");
            item.state = outcome.state;
            item.reviewer = outcome.reviewer.clone().unwrap_or_default();
            item.note = Some(outcome.note);
            if !report.waiting_for_human.contains(&coder.number) { report.waiting_for_human.push(coder.number); }
            if let Some(reviewer) = outcome.reviewer {
                if !task.status.agents.contains(&reviewer) { task.status.agents.push(reviewer); }
            }
            checkpoint(&mut task.status, &task.directory)?;
        }
    }

    let complete = !task.status.subtasks.is_empty() && task.status.subtasks.iter().all(|subtask| matches!(subtask.state.as_str(), "pr" | "done"));
    task.status.state = if complete { "done".into() } else { "reviewing".into() };
    task.status.note = format!("Rust run: {} coder(s) started, {} result(s) received, {} failed; human approval and push/PR creation are not ported yet", report.started.len(), report.completed_coding.len(), report.failed.len());
    checkpoint(&mut task.status, &task.directory)?;
    Ok(report)
}

/// Human approval is explicit in the CLI. Re-verify the worktree, push its branch, then open a PR.
pub fn approve_subtask(root: PathBuf, task_ref: &str, number: u32) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let registry_path = config::state_dir().join("agents.json");
    approve_subtask_with_registry(root, task_ref, number, &registry_path)
}

pub fn approve_subtask_with_registry(
    root: PathBuf,
    task_ref: &str,
    number: u32,
    registry_path: &Path,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    approve_subtask_with_runner(root, task_ref, number, registry_path, |command| command.output())
}

pub fn approve_plan(root: PathBuf, task_ref: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut task = find_task(root, task_ref)?;
    if task.status.waiting != "plan" { return Err("task is not waiting for plan approval".into()); }
    let plan = crate::read_task_section(&task, "plan")?;
    let subtasks = parse_plan(&plan, &task.status.title);
    validate_plan(&subtasks)?;
    task.status.subtasks = subtasks;
    task.status.waiting.clear();
    task.status.state = "coding".into();
    task.status.note = "plan approved by human; rerun `shepherd run` to start coders".into();
    checkpoint(&mut task.status, &task.directory)
}

fn approve_subtask_with_runner(
    root: PathBuf, task_ref: &str, number: u32, registry_path: &Path,
    mut run_command: impl FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut task = find_task(&root, task_ref)?;
    let config = load_config(&root)?;
    let index = task.status.subtasks.iter().position(|subtask| subtask.number == number).ok_or_else(|| format!("task has no subtask {number}"))?;
    let subtask = task.status.subtasks[index].clone();
    if subtask.state != "awaiting-approval" {
        return Err(format!("subtask {number} is {:?}; only reviewed subtasks in awaiting-approval can be approved", subtask.state).into());
    }
    if config.workflow.max_review_rounds > 0 && subtask.reviewer.is_empty() {
        return Err(format!("subtask {number} has no recorded reviewer" ).into());
    }
    let registry = load_agent_registry(registry_path)?;
    let coder = registry.result(&subtask.coder).ok_or_else(|| format!("coder {:?} is missing from agents.json", subtask.coder))?;
    if coder.branch.is_empty() || coder.branch != subtask.branch { return Err("task branch does not match the coder registry".into()); }
    if config.limits.protected_branches.contains(&coder.branch) { return Err(format!("refusing to push protected branch {:?}", coder.branch).into()); }
    if subtask.verify.iter().chain(config.workflow.checks.iter()).any(|command| command.trim().len() > 0) {
        if let Some(problem) = verify_coder(coder, &config.workflow.base_branch, subtask.verify.iter().chain(config.workflow.checks.iter()).cloned()) {
            return Err(format!("approval blocked: {problem}").into());
        }
    } else if let Some(problem) = verify_coder(coder, &config.workflow.base_branch, std::iter::empty()) {
        return Err(format!("approval blocked: {problem}").into());
    }

    let mut push_command = Command::new("git");
    push_command.args(["push", "--set-upstream", "origin", &coder.branch]).current_dir(&coder.worktree);
    let push = run_command(&mut push_command)?;
    if !push.status.success() {
        let detail = clip(&format!("{}{}", String::from_utf8_lossy(&push.stdout), String::from_utf8_lossy(&push.stderr)), 1000);
        task.status.subtasks[index].note = Some(format!("push failed: {detail}"));
        checkpoint(&mut task.status, &task.directory)?;
        return Err(format!("push failed: {detail}").into());
    }

    let base = crate::mcp::default_base(&root.display().to_string(), &config.workflow.base_branch);
    let title = if subtask.title.trim().is_empty() { task.status.title.clone() } else { subtask.title.clone() };
    let body = pr_body(&task, &subtask, &config.workflow.checks);
    let mut command = Command::new("gh");
    command.args(["pr", "create", "--base", &base, "--head", &coder.branch, "--title", &title, "--body", &body]);
    if config.workflow.draft { command.arg("--draft"); }
    let created = match run_command(command.current_dir(&root)) {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            let detail = clip(&format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)), 1000);
            task.status.subtasks[index].note = Some(format!("branch pushed, but PR creation failed: {detail}"));
            checkpoint(&mut task.status, &task.directory)?;
            return Err(format!("branch {} was pushed, but PR creation failed: {detail}", coder.branch).into());
        }
        Err(error) => {
            task.status.subtasks[index].note = Some(format!("branch pushed, but `gh` could not run: {error}"));
            checkpoint(&mut task.status, &task.directory)?;
            return Err(format!("branch {} was pushed, but `gh` could not run: {error}", coder.branch).into());
        }
    };
    let url = String::from_utf8_lossy(&created.stdout).trim().to_owned();
    if url.is_empty() {
        task.status.subtasks[index].note = Some("gh pr create returned no URL; verify the remote pull request before retrying".into());
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

fn pr_body(task: &crate::CreatedTask, subtask: &crate::Subtask, checks: &[String]) -> String {
    let mut body = String::new();
    if let Some(issue) = &task.status.issue { body.push_str(&format!("Closes #{}\n\n", issue.number)); }
    body.push_str(&format!("## {}\n\nTask {} - subtask {}\n\n", subtask.title, task.status.id, subtask.number));
    if !subtask.body.trim().is_empty() { body.push_str(&format!("### Change\n{}\n\n", subtask.body.trim())); }
    body.push_str(&format!("### Review\nApproved by {}.\n\n", if subtask.reviewer.is_empty() { "(review disabled)" } else { &subtask.reviewer }));
    let mut seen = HashSet::new();
    let commands: Vec<_> = subtask.verify.iter().chain(checks.iter()).map(|command| command.trim()).filter(|command| !command.is_empty() && seen.insert(*command)).collect();
    if !commands.is_empty() {
        body.push_str("### Checks passed\n");
        for command in commands { body.push_str(&format!("- `{command}`\n")); }
        body.push('\n');
    }
    body.push_str("Generated by Shepherd.\n");
    body
}

fn review_and_check(
    request: &RunRequest, config: &crate::config::Config, task: &crate::CreatedTask, number: u32,
    coder_id: &str, coder_result: &str, registry_path: &Path, mcp_url: &str, timeout: Duration,
    terminals: &dyn Terminals, active_vendors: &[String],
) -> Result<ReviewOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let subtask = task.status.subtasks.iter().find(|subtask| subtask.number == number).expect("subtask exists");
    let coder = load_agent_registry(registry_path)?.result(coder_id).cloned().ok_or("coder disappeared from agent registry")?;
    let accept = crate::read_task_section(task, "acceptance").unwrap_or_default();
    let wanted = count_criteria(&accept);
    let rounds = config.workflow.max_review_rounds;
    let mut feedback = String::new();
    let mut latest_coder_result = coder_result.to_owned();
    let mut reviewer_id = None;
    let limit = rounds.max(1);

    for round in 1..=limit {
        if rounds > 0 {
            let (vendor, model) = reviewer_assignment(config, active_vendors, &coder.vendor, &coder.model)?;
            let prompt = format!("Review the work of {coder_id} on subtask {number}: {}. This is review round {round} of {rounds}.\n\nRead get_task_context(section=acceptance) and get_task_context(section=spec), then get_diff(agent_id={coder_id}). Review every acceptance criterion and do not modify files. Deliver exactly this format with submit_result:\n\nVERDICT: APPROVE | REQUEST_CHANGES\n\nCRITERIA\n- [x] criterion — evidence (or [ ] if unmet)\n\nCOMMENTS\n- file:line [blocker|major|minor|nit] issue -> fix\n\nSUMMARY\n2-4 lines\n\nAcceptance criteria:\n{accept}\n\nCoder's result:\n{}\n{}", subtask.title, clip(&latest_coder_result, 3000), if feedback.is_empty() { String::new() } else { format!("\nPrior review feedback to verify:\n{feedback}") });
            let reviewer = spawn::spawn_with_options(SpawnRequest { root: request.root.clone(), role: "reviewer".into(), task: prompt, task_id: Some(task.status.id.to_string()), vendor: Some(vendor), model: Some(model), mcp_url: Some(mcp_url.to_owned()) }, SpawnOptions { parent_id: None, registry_path: Some(registry_path.to_path_buf()), active_vendors: Some(active_vendors.to_vec()), terminals })?;
            reviewer_id = Some(reviewer.agent.id.clone());
            match wait_for_result(registry_path, &reviewer.agent.id, timeout) {
                Ok(review_text) => {
                    let verdict = parse_review(&review_text, wanted);
                    let review_text = if !verdict.found {
                        let reminder = "Your review did not contain the required VERDICT line. Reply using exactly this format, answer every acceptance criterion, and call submit_result:\n\nVERDICT: APPROVE | REQUEST_CHANGES\n\nCRITERIA\n- [x] <criterion> - <evidence> (or [ ] if not met)\n\nCOMMENTS\n- path/file:line [blocker|major|minor|nit] <problem> -> <fix>\n\nSUMMARY\n<2-4 lines>";
                        revise_agent(registry_path, reviewer.agent.id.as_str(), reminder, terminals)?;
                        match wait_for_result(registry_path, &reviewer.agent.id, timeout) {
                            Ok(retry) if parse_review(&retry, wanted).found => retry,
                            Ok(retry) => return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("reviewer did not produce the required format after a reminder: {}", clip(&retry, 500)) }),
                            Err(error) => return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("reviewer could not be reminded: {error}") }),
                        }
                    } else { review_text };
                    let verdict = parse_review(&review_text, wanted);
                    if !verdict.approve {
                        feedback = if let Some(reason) = verdict.downgraded { format!("Review approval was downgraded: {reason}. Every acceptance criterion must be answered and met.\n\n{}", verdict.text) } else { verdict.text };
                        if round == limit {
                            return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("review requested changes after {rounds} round(s): {}", clip(&feedback, 700)) });
                        }
                        revise_coder(registry_path, coder_id, &feedback, terminals)?;
                        latest_coder_result = match wait_for_result(registry_path, coder_id, timeout) {
                            Ok(result) => result,
                            Err(error) => return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("coder did not finish review fixes: {error}") }),
                        };
                        continue;
                    }
                }
                Err(error) => return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("reviewer did not return a result: {error}") }),
            }
        }

        let coder = load_agent_registry(registry_path)?.result(coder_id).cloned().ok_or("coder disappeared after review")?;
        if let Some(message) = verify_coder(&coder, config.workflow.base_branch.as_str(), subtask.verify.iter().chain(config.workflow.checks.iter()).cloned()) {
            if round == limit { return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: message }); }
            feedback = message.clone();
            revise_coder(registry_path, coder_id, &message, terminals)?;
            latest_coder_result = match wait_for_result(registry_path, coder_id, timeout) {
                Ok(result) => result,
                Err(error) => return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("coder did not fix failed checks: {error}") }),
            };
            continue;
        }
        return Ok(ReviewOutcome { reviewer: reviewer_id, state: "awaiting-approval".into(), note: "review approved and checks passed; human approval and push/PR are not ported yet".into() });
    }
    Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: "review/check loop ended without approval".into() })
}

fn reviewer_assignment(config: &crate::config::Config, active: &[String], coder_vendor: &str, coder_model: &str) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    let routed = resolve_role(config, active, "reviewer")?;
    if config.vendors[&routed.vendor].mcp.supported && (routed.vendor != coder_vendor || routed.model != coder_model) {
        return Ok((routed.vendor, routed.model));
    }
    let mut candidates: Vec<_> = active.iter().filter(|vendor| config.vendors[*vendor].mcp.supported).cloned().collect();
    candidates.sort();
    if let Some(vendor) = candidates.iter().find(|vendor| vendor.as_str() != coder_vendor) {
        let model = config.roles["reviewer"].model.clone();
        let selected_model = if !model.is_empty() && (config.vendors[vendor].models.is_empty() || config.vendors[vendor].models.contains(&model)) { model } else { config.vendors[vendor].default_model.clone() };
        return Ok((vendor.clone(), selected_model));
    }
    if candidates.iter().any(|vendor| vendor == coder_vendor) {
        let vendor = &config.vendors[coder_vendor];
        let model = vendor.models.iter().find(|model| model.as_str() != coder_model).cloned().ok_or("no independent MCP-capable reviewer vendor/model is available")?;
        return Ok((coder_vendor.to_owned(), model));
    }
    Err("no active MCP-capable vendor is available for the reviewer".into())
}

fn revise_coder(registry_path: &Path, coder_id: &str, prompt: &str, terminals: &dyn Terminals) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    revise_agent(registry_path, coder_id, &format!("Review feedback: fix all blockers and major issues, run the required checks, commit the changes, then call submit_result with your new summary.\n\n{prompt}"), terminals)
}

fn revise_agent(registry_path: &Path, agent_id: &str, prompt: &str, terminals: &dyn Terminals) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut registry = load_agent_registry(registry_path)?;
    registry.clear_result(agent_id, &super::now_rfc3339())?;
    save_agent_registry(registry_path, &registry)?;
    terminals.prompt(agent_id, prompt)?;
    Ok(())
}

fn mcp_role_assignment_from(config: &crate::config::Config, role: &str, active: &[String]) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    let routed = resolve_role(config, active, role)?;
    if config.vendors[&routed.vendor].mcp.supported { return Ok((routed.vendor, routed.model)); }
    let mut candidates: Vec<_> = active.iter().filter(|vendor| config.vendors[*vendor].mcp.supported).cloned().collect();
    candidates.sort();
    let vendor = candidates.first().cloned().ok_or_else(|| format!("no active MCP-capable vendor is available for {role}"))?;
    let requested = &config.roles[role].model;
    let model = if !requested.is_empty() && (config.vendors[&vendor].models.is_empty() || config.vendors[&vendor].models.contains(requested)) { requested.clone() } else { config.vendors[&vendor].default_model.clone() };
    Ok((vendor, model))
}

fn has_content(plan: &str) -> bool {
    let trimmed = plan.trim();
    !trimmed.is_empty() && trimmed != "# Plan"
}

fn verify_coder(coder: &crate::Agent, configured_base: &str, commands: impl Iterator<Item = String>) -> Option<String> {
    if coder.worktree.is_empty() { return None; }
    let status = match Command::new("git").args(["status", "--porcelain"]).current_dir(&coder.worktree).output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).into_owned(),
        Ok(output) => return Some(format!("Could not inspect coder worktree: {}", String::from_utf8_lossy(&output.stderr).trim())),
        Err(error) => return Some(format!("Could not inspect coder worktree: {error}")),
    };
    if !status.trim().is_empty() { return Some(format!("Coder left uncommitted or untracked changes:\n{}", clip(&status, 1200))); }
    let base = crate::mcp::default_base(&coder.worktree, configured_base);
    match Command::new("git").args(["rev-list", "--count", &format!("{base}..HEAD")]).current_dir(&coder.worktree).output() {
        Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "0" => return Some(format!("Coder branch has no commits beyond {base}; commit the implementation and resubmit.")),
        Ok(output) if !output.status.success() => return Some(format!("Could not compare coder branch with {base}: {}", String::from_utf8_lossy(&output.stderr).trim())),
        Err(error) => return Some(format!("Could not inspect coder commits: {error}")),
        _ => {}
    }
    let mut seen = HashSet::new();
    for command in commands.map(|command| command.trim().to_owned()).filter(|command| !command.is_empty() && seen.insert(command.clone())) {
        match Command::new("sh").args(["-lc", &command]).current_dir(&coder.worktree).output() {
            Ok(output) if output.status.success() => {}
            Ok(output) => return Some(format!("Check `{command}` failed ({}):\n{}", output.status, clip(&format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)), 1500))),
            Err(error) => return Some(format!("Could not run check `{command}`: {error}")),
        }
    }
    None
}

fn validate_mcp_url(url: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(authority) = url.strip_prefix("http://").and_then(|rest| rest.strip_suffix("/mcp")) else {
        return Err("MCP URL must be a local HTTP endpoint ending in /mcp (for example http://127.0.0.1:4317/mcp)".into());
    };
    let host = authority.split(':').next().unwrap_or("");
    if !matches!(host, "127.0.0.1" | "localhost") || !authority.rsplit_once(':').and_then(|(_, port)| port.parse::<u16>().ok()).is_some() {
        return Err("MCP endpoint must use localhost or 127.0.0.1 and include a valid port".into());
    }
    Ok(())
}

fn wait_for_result(path: &std::path::Path, id: &str, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let registry = load_agent_registry(path).map_err(|error| error.to_string())?;
        let agent = registry.result(id).ok_or_else(|| format!("agent {id} disappeared from agents.json"))?;
        if agent.has_result { return Ok(agent.result.clone()); }
        if agent.terminal() { return Err(if agent.error.is_empty() { format!("agent ended in state {} without a result", agent.state) } else { agent.error.clone() }); }
        if Instant::now() >= deadline { return Err(format!("timed out after {} seconds", timeout.as_secs())); }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn checkpoint(status: &mut crate::TaskStatus, directory: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if status.waiting == "plan" { status.state = "planning".into(); }
    else { status.state = crate::aggregate_task_state(&status.subtasks, &status.state); }
    status.updated = super::now_rfc3339();
    save_task_status(directory.join("status.json"), status)
}

fn parse_timeout(value: &str) -> Option<Duration> {
    let (number, unit) = value.split_at(value.find(|ch: char| !ch.is_ascii_digit())?);
    let number = number.parse::<u64>().ok()?;
    match unit { "s" => Some(Duration::from_secs(number)), "m" => Some(Duration::from_secs(number * 60)), "h" => Some(Duration::from_secs(number * 3600)), _ => None }
}

fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.len() <= limit { return text.to_owned(); }
    let mut end = limit;
    while !text.is_char_boundary(end) { end -= 1; }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_loopback_mcp_endpoint_and_parses_timeouts() {
        assert!(validate_mcp_url("http://127.0.0.1:4317/mcp").is_ok());
        assert!(validate_mcp_url("https://example.test/mcp").is_err());
        assert!(validate_mcp_url("http://0.0.0.0:4317/mcp").is_err());
        assert_eq!(parse_timeout("30m"), Some(Duration::from_secs(1800)));
        assert_eq!(parse_timeout("bad"), None);
    }

    #[test]
    fn routes_reviewers_to_a_different_mcp_vendor() {
        let config = crate::config::Config::default();
        let active = vec!["claude".into(), "codex".into(), "gemini".into()];
        assert_eq!(reviewer_assignment(&config, &active, "codex", "gpt-5").unwrap(), ("claude".into(), "opus".into()));
    }

    #[test]
    fn routes_brain_off_a_vendor_without_mcp_and_approves_persisted_plan() {
        let mut config = crate::config::Config::default();
        config.roles.get_mut("brain").unwrap().vendor = "gemini".into();
        let active = vec!["claude".into(), "gemini".into()];
        assert_eq!(mcp_role_assignment_from(&config, "brain", &active).unwrap(), ("claude".into(), "opus".into()));

        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("shepherd-plan-approval-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let task = crate::create_task(&root, crate::NewTask { id: 1, title: "Plan approval".into(), description: String::new(), criteria: Vec::new(), issue: None, issue_markdown: None, links: Vec::new() }).unwrap();
        crate::write_task_section(&task, "plan", "# Plan\n\n## Subtask 1: Do work\nVerify: true\n").unwrap();
        let mut status = task.status.clone();
        status.waiting = "plan".into();
        status.state = "planning".into();
        crate::save_task_status(task.directory.join("status.json"), &status).unwrap();
        approve_plan(root.clone(), "1").unwrap();
        let approved = crate::find_task(&root, "1").unwrap();
        assert!(approved.status.waiting.is_empty());
        assert_eq!(approved.status.state, "coding");
        assert_eq!(approved.status.subtasks.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verifies_clean_commits_and_configured_checks_in_a_worktree() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("shepherd-verify-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| Command::new("git").args(args).current_dir(&root).output().unwrap();
        assert!(git(&["init", "-b", "main"]).status.success());
        assert!(git(&["config", "user.email", "test@example.test"]).status.success());
        assert!(git(&["config", "user.name", "Shepherd Test"]).status.success());
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(git(&["add", "base.txt"]).status.success());
        assert!(git(&["commit", "-m", "base"]).status.success());
        assert!(git(&["checkout", "-b", "feature"]).status.success());
        std::fs::write(root.join("change.txt"), "change\n").unwrap();
        assert!(git(&["add", "change.txt"]).status.success());
        assert!(git(&["commit", "-m", "change"]).status.success());
        let coder = crate::Agent { root: root.display().to_string(), worktree: root.display().to_string(), branch: "feature".into(), ..crate::Agent::default() };
        let verified = verify_coder(&coder, "main", ["true".to_owned()].into_iter());
        assert!(verified.is_none(), "{verified:?}");
        std::fs::write(root.join("uncommitted.txt"), "unfinished\n").unwrap();
        assert!(verify_coder(&coder, "main", std::iter::empty()).unwrap().contains("uncommitted"));
        std::fs::remove_file(root.join("uncommitted.txt")).unwrap();
        assert!(verify_coder(&coder, "main", ["false".to_owned()].into_iter()).unwrap().contains("Check `false` failed"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pull_request_body_links_issue_and_records_review_and_checks() {
        let task = crate::CreatedTask {
            status: crate::TaskStatus { id: 7, slug: "oauth".into(), title: "OAuth".into(), state: "pr".into(),
                issue: Some(crate::IssueRef { number: 42, url: "https://example.test/issues/42".into(), repo: String::new() }),
                branch: String::new(), pr: 0, subtasks: Vec::new(), waiting: String::new(), note: String::new(), agents: Vec::new(),
                created: "now".into(), updated: "now".into() }, directory: PathBuf::new(),
        };
        let subtask = crate::Subtask { number: 1, title: "OAuth API".into(), body: "Implement the endpoint".into(), depends_on: Vec::new(), verify: Vec::new(),
            state: "awaiting-approval".into(), note: None, coder: "coder-1".into(), reviewer: "reviewer-1".into(), round: 1, branch: "feature".into(), pr: String::new() };
        let body = pr_body(&task, &subtask, &["cargo test".into()]);
        assert!(body.contains("Closes #42"));
        assert!(body.contains("Approved by reviewer-1"));
        assert!(body.contains("`cargo test`"));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_approval_pushes_and_opens_pr_then_persists_url() {
        use std::os::unix::process::ExitStatusExt;
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("shepherd-approve-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(root.join(".shepherd")).unwrap();
        std::fs::write(root.join(".shepherd/config.yaml"), "workflow:\n  base_branch: main\n  checks: [\"true\"]\n  draft: true\n").unwrap();
        let git = |args: &[&str]| Command::new("git").args(args).current_dir(&root).output().unwrap();
        assert!(git(&["init", "-b", "main"]).status.success());
        assert!(git(&["config", "user.email", "test@example.test"]).status.success());
        assert!(git(&["config", "user.name", "Shepherd Test"]).status.success());
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(git(&["add", "base.txt"]).status.success());
        assert!(git(&["commit", "-m", "base"]).status.success());
        assert!(git(&["checkout", "-b", "feature"]).status.success());
        std::fs::write(root.join("change.txt"), "done\n").unwrap();
        assert!(git(&["add", "change.txt"]).status.success());
        assert!(git(&["commit", "-m", "feat: change"]).status.success());

        let task = crate::create_task(&root, crate::NewTask { id: 9, title: "Publish change".into(), description: String::new(), criteria: Vec::new(), issue: None, issue_markdown: None, links: Vec::new() }).unwrap();
        let mut status = task.status.clone();
        status.subtasks.push(crate::Subtask { number: 1, title: "Change".into(), body: "Add change".into(), depends_on: Vec::new(), verify: vec!["true".into()],
            state: "awaiting-approval".into(), note: None, coder: "coder-1".into(), reviewer: "reviewer-1".into(), round: 1, branch: "feature".into(), pr: String::new() });
        crate::save_task_status(task.directory.join("status.json"), &status).unwrap();
        assert!(git(&["add", ".shepherd"]).status.success());
        assert!(git(&["commit", "-m", "test: persist workflow fixture"]).status.success());
        let mut registry = crate::AgentRegistryFile::default();
        registry.create(crate::Agent { role: "coder".into(), vendor: "codex".into(), root: root.display().to_string(), worktree: root.display().to_string(), branch: "feature".into(), ..crate::Agent::default() }, "coder-token".into(), "now");
        let registry_path = std::env::temp_dir().join(format!("shepherd-approve-registry-{}-{nonce}.json", std::process::id()));
        crate::save_agent_registry(&registry_path, &registry).unwrap();
        let mut calls = Vec::<Vec<String>>::new();
        let url = approve_subtask_with_runner(root.clone(), "9", 1, &registry_path, |command| {
            calls.push(std::iter::once(command.get_program().to_string_lossy().into_owned()).chain(command.get_args().map(|arg| arg.to_string_lossy().into_owned())).collect());
            Ok(std::process::Output { status: std::process::ExitStatus::from_raw(0), stdout: b"https://example.test/pr/12\n".to_vec(), stderr: Vec::new() })
        }).unwrap();
        assert_eq!(url, "https://example.test/pr/12");
        assert_eq!(calls.len(), 2);
        assert_eq!(&calls[0][..5], ["git", "push", "--set-upstream", "origin", "feature"]);
        assert!(calls[1].iter().any(|argument| argument == "--draft"));
        let updated = crate::find_task(&root, "9").unwrap();
        assert_eq!(updated.status.subtasks[0].state, "pr");
        assert_eq!(updated.status.subtasks[0].pr, "https://example.test/pr/12");
        std::fs::remove_file(registry_path).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revise_coder_clears_previous_result_and_prompts_through_terminals_trait() {
        use crate::terminals::{InMemoryTerminals, OpenAgentRequest};
        use std::collections::HashMap;

        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let registry_path = std::env::temp_dir().join(format!("shepherd-revise-registry-{}-{nonce}.json", std::process::id()));
        let mut registry = crate::AgentRegistryFile::default();
        registry.create(crate::Agent { role: "coder".into(), vendor: "codex".into(), ..crate::Agent::default() }, "tok".into(), "now");
        registry.submit_result("coder-1", "initial summary", "now").unwrap();
        crate::save_agent_registry(&registry_path, &registry).unwrap();

        let fake = InMemoryTerminals::new();
        fake.open_agent(&OpenAgentRequest {
            root: PathBuf::from("/tmp"),
            role: "coder".into(),
            agent_id: "coder-1".into(),
            vendor_name: "codex".into(),
            command: "codex".into(),
            shepherd_agent: "codex".into(),
            args: Vec::new(),
            label: "coder".into(),
            env: HashMap::new(),
            worktree: None,
        }).unwrap();

        revise_coder(&registry_path, "coder-1", "Please fix unit tests", &fake).unwrap();
        let updated = crate::load_agent_registry(&registry_path).unwrap();
        let coder = updated.result("coder-1").unwrap();
        assert!(!coder.has_result);
        assert_eq!(coder.state, "working");
        let prompts = fake.prompts();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].0, "coder-1");
        assert!(prompts[0].1.contains("Please fix unit tests"));
        std::fs::remove_file(registry_path).unwrap();
    }
}
