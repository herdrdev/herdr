//! Code verification, review rounds, and reviewer assignment.

use super::{clip, wait_for_result, ReviewOutcome, RunRequest};
use crate::{
    config::resolve_role,
    count_criteria, load_agent_registry, parse_review, save_agent_registry,
    spawn::{self, SpawnOptions, SpawnRequest},
    terminals::Terminals,
};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub(crate) fn review_and_check(
    request: &RunRequest,
    config: &crate::config::Config,
    task: &crate::CreatedTask,
    number: u32,
    coder_id: &str,
    coder_result: &str,
    registry_path: &Path,
    mcp_url: &str,
    timeout: Duration,
    terminals: &dyn Terminals,
    active_vendors: &[String],
) -> Result<ReviewOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let subtask = task
        .status
        .subtasks
        .iter()
        .find(|subtask| subtask.number == number)
        .expect("subtask exists");
    let coder = load_agent_registry(registry_path)?
        .result(coder_id)
        .cloned()
        .ok_or("coder disappeared from agent registry")?;
    let accept = crate::read_task_section(task, "acceptance").unwrap_or_default();
    let wanted = count_criteria(&accept);
    let rounds = config.workflow.max_review_rounds;
    let mut feedback = String::new();
    let mut latest_coder_result = coder_result.to_owned();
    let mut reviewer_id = None;
    let limit = rounds.max(1);

    for round in 1..=limit {
        if rounds > 0 {
            let (vendor, model) =
                reviewer_assignment(config, active_vendors, &coder.vendor, &coder.model)?;
            let prompt = format!("Review the work of {coder_id} on subtask {number}: {}. This is review round {round} of {rounds}.\n\nRead get_task_context(section=acceptance) and get_task_context(section=spec), then get_diff(agent_id={coder_id}). Review every acceptance criterion and do not modify files. Deliver exactly this format with submit_result:\n\nVERDICT: APPROVE | REQUEST_CHANGES\n\nCRITERIA\n- [x] criterion — evidence (or [ ] if unmet)\n\nCOMMENTS\n- file:line [blocker|major|minor|nit] issue -> fix\n\nSUMMARY\n2-4 lines\n\nAcceptance criteria:\n{accept}\n\nCoder's result:\n{}\n{}", subtask.title, clip(&latest_coder_result, 3000), if feedback.is_empty() { String::new() } else { format!("\nPrior review feedback to verify:\n{feedback}") });
            let reviewer = spawn::spawn_with_options(
                SpawnRequest {
                    root: request.root.clone(),
                    role: "reviewer".into(),
                    task: prompt,
                    task_id: Some(task.status.id.to_string()),
                    vendor: Some(vendor),
                    model: Some(model),
                    mcp_url: Some(mcp_url.to_owned()),
                },
                SpawnOptions {
                    parent_id: None,
                    registry_path: Some(registry_path.to_path_buf()),
                    active_vendors: Some(active_vendors.to_vec()),
                    terminals,
                },
            )?;
            reviewer_id = Some(reviewer.agent.id.clone());
            match wait_for_result(registry_path, &reviewer.agent.id, timeout) {
                Ok(review_text) => {
                    let verdict = parse_review(&review_text, wanted);
                    let review_text = if !verdict.found {
                        let reminder = "Your review did not contain the required VERDICT line. Reply using exactly this format, answer every acceptance criterion, and call submit_result:\n\nVERDICT: APPROVE | REQUEST_CHANGES\n\nCRITERIA\n- [x] <criterion> - <evidence> (or [ ] if not met)\n\nCOMMENTS\n- path/file:line [blocker|major|minor|nit] <problem> -> <fix>\n\nSUMMARY\n<2-4 lines>";
                        revise_agent(
                            registry_path,
                            reviewer.agent.id.as_str(),
                            reminder,
                            terminals,
                        )?;
                        match wait_for_result(registry_path, &reviewer.agent.id, timeout) {
                            Ok(retry) if parse_review(&retry, wanted).found => retry,
                            Ok(retry) => return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("reviewer did not produce the required format after a reminder: {}", clip(&retry, 500)) }),
                            Err(error) => return Ok(ReviewOutcome { reviewer: reviewer_id, state: "needs-human".into(), note: format!("reviewer could not be reminded: {error}") }),
                        }
                    } else {
                        review_text
                    };
                    let verdict = parse_review(&review_text, wanted);
                    if !verdict.approve {
                        feedback = if let Some(reason) = verdict.downgraded {
                            format!("Review approval was downgraded: {reason}. Every acceptance criterion must be answered and met.\n\n{}", verdict.text)
                        } else {
                            verdict.text
                        };
                        if round == limit {
                            return Ok(ReviewOutcome {
                                reviewer: reviewer_id,
                                state: "needs-human".into(),
                                note: format!(
                                    "review requested changes after {rounds} round(s): {}",
                                    clip(&feedback, 700)
                                ),
                            });
                        }
                        revise_coder(registry_path, coder_id, &feedback, terminals)?;
                        latest_coder_result =
                            match wait_for_result(registry_path, coder_id, timeout) {
                                Ok(result) => result,
                                Err(error) => {
                                    return Ok(ReviewOutcome {
                                        reviewer: reviewer_id,
                                        state: "needs-human".into(),
                                        note: format!("coder did not finish review fixes: {error}"),
                                    })
                                }
                            };
                        continue;
                    }
                }
                Err(error) => {
                    return Ok(ReviewOutcome {
                        reviewer: reviewer_id,
                        state: "needs-human".into(),
                        note: format!("reviewer did not return a result: {error}"),
                    })
                }
            }
        }

        let coder = load_agent_registry(registry_path)?
            .result(coder_id)
            .cloned()
            .ok_or("coder disappeared after review")?;
        if let Some(message) = verify_coder(
            &coder,
            config.workflow.base_branch.as_str(),
            subtask
                .verify
                .iter()
                .chain(config.workflow.checks.iter())
                .cloned(),
        ) {
            if round == limit {
                return Ok(ReviewOutcome {
                    reviewer: reviewer_id,
                    state: "needs-human".into(),
                    note: message,
                });
            }
            feedback = message.clone();
            revise_coder(registry_path, coder_id, &message, terminals)?;
            latest_coder_result = match wait_for_result(registry_path, coder_id, timeout) {
                Ok(result) => result,
                Err(error) => {
                    return Ok(ReviewOutcome {
                        reviewer: reviewer_id,
                        state: "needs-human".into(),
                        note: format!("coder did not fix failed checks: {error}"),
                    })
                }
            };
            continue;
        }
        return Ok(ReviewOutcome {
            reviewer: reviewer_id,
            state: "awaiting-approval".into(),
            note: "review approved and checks passed; awaiting human approval for push/PR".into(),
        });
    }
    Ok(ReviewOutcome {
        reviewer: reviewer_id,
        state: "needs-human".into(),
        note: "review/check loop ended without approval".into(),
    })
}

pub(crate) fn reviewer_assignment(
    config: &crate::config::Config,
    active: &[String],
    coder_vendor: &str,
    coder_model: &str,
) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    let routed = resolve_role(config, active, "reviewer")?;
    if config.vendors[&routed.vendor].mcp.supported
        && (routed.vendor != coder_vendor || routed.model != coder_model)
    {
        return Ok((routed.vendor, routed.model));
    }
    let mut candidates: Vec<_> = active
        .iter()
        .filter(|vendor| config.vendors[*vendor].mcp.supported)
        .cloned()
        .collect();
    candidates.sort();
    if let Some(vendor) = candidates
        .iter()
        .find(|vendor| vendor.as_str() != coder_vendor)
    {
        let model = config.roles["reviewer"].model.clone();
        let selected_model = if !model.is_empty()
            && (config.vendors[vendor].models.is_empty()
                || config.vendors[vendor].models.contains(&model))
        {
            model
        } else {
            config.vendors[vendor].default_model.clone()
        };
        return Ok((vendor.clone(), selected_model));
    }
    if candidates.iter().any(|vendor| vendor == coder_vendor) {
        let vendor = &config.vendors[coder_vendor];
        let model = vendor
            .models
            .iter()
            .find(|model| model.as_str() != coder_model)
            .cloned()
            .ok_or("no independent MCP-capable reviewer vendor/model is available")?;
        return Ok((coder_vendor.to_owned(), model));
    }
    Err("no active MCP-capable vendor is available for the reviewer".into())
}

pub(crate) fn revise_coder(
    registry_path: &Path,
    coder_id: &str,
    prompt: &str,
    terminals: &dyn Terminals,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    revise_agent(registry_path, coder_id, &format!("Review feedback: fix all blockers and major issues, run the required checks, commit the changes, then call submit_result with your new summary.\n\n{prompt}"), terminals)
}

pub(crate) fn revise_agent(
    registry_path: &Path,
    agent_id: &str,
    prompt: &str,
    terminals: &dyn Terminals,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut registry = load_agent_registry(registry_path)?;
    registry.clear_result(agent_id, &crate::now_rfc3339())?;
    save_agent_registry(registry_path, &registry)?;
    terminals.prompt_text(agent_id, prompt)?;
    Ok(())
}

pub(crate) fn mcp_role_assignment_from(
    config: &crate::config::Config,
    role: &str,
    active: &[String],
) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    let routed = resolve_role(config, active, role)?;
    if config.vendors[&routed.vendor].mcp.supported {
        return Ok((routed.vendor, routed.model));
    }
    let mut candidates: Vec<_> = active
        .iter()
        .filter(|vendor| config.vendors[*vendor].mcp.supported)
        .cloned()
        .collect();
    candidates.sort();
    let vendor = candidates
        .first()
        .cloned()
        .ok_or_else(|| format!("no active MCP-capable vendor is available for {role}"))?;
    let requested = &config.roles[role].model;
    let model = if !requested.is_empty()
        && (config.vendors[&vendor].models.is_empty()
            || config.vendors[&vendor].models.contains(requested))
    {
        requested.clone()
    } else {
        config.vendors[&vendor].default_model.clone()
    };
    Ok((vendor, model))
}

pub(crate) fn verify_coder(
    coder: &crate::Agent,
    configured_base: &str,
    commands: impl Iterator<Item = String>,
) -> Option<String> {
    if coder.worktree.is_empty() {
        return None;
    }
    let status = match Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&coder.worktree)
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => {
            return Some(format!(
                "Could not inspect coder worktree: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
        Err(error) => return Some(format!("Could not inspect coder worktree: {error}")),
    };
    if !status.trim().is_empty() {
        return Some(format!(
            "Coder left uncommitted or untracked changes:\n{}",
            clip(&status, 1200)
        ));
    }
    let base = crate::mcp::default_base(&coder.worktree, configured_base);
    match Command::new("git").args(["rev-list", "--count", &format!("{base}..HEAD")]).current_dir(&coder.worktree).output() {
        Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "0" => return Some(format!("Coder branch has no commits beyond {base}; commit the implementation and resubmit.")),
        Ok(output) if !output.status.success() => return Some(format!("Could not compare coder branch with {base}: {}", String::from_utf8_lossy(&output.stderr).trim())),
        Err(error) => return Some(format!("Could not inspect coder commits: {error}")),
        _ => {}
    }
    let mut seen = HashSet::new();
    for command in commands
        .map(|command| command.trim().to_owned())
        .filter(|command| !command.is_empty() && seen.insert(command.clone()))
    {
        match Command::new("sh")
            .args(["-lc", &command])
            .current_dir(&coder.worktree)
            .output()
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return Some(format!(
                    "Check `{command}` failed ({}):\n{}",
                    output.status,
                    clip(
                        &format!(
                            "{}{}",
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        ),
                        1500
                    )
                ))
            }
            Err(error) => return Some(format!("Could not run check `{command}`: {error}")),
        }
    }
    None
}
