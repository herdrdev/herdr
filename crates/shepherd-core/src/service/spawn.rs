//! Agent spawning with vendor/role routing, worktree setup, and skill installation.

use super::{OrchestrationService, ServiceSpawnRequest};
use crate::config::{available_vendors, load_config, resolve_role};
use crate::skills;
use crate::spawn::{mcp_launch_args, role_prompt, RUNTIME_CONSTRAINTS};
use crate::terminals::{OpenAgentRequest, WorktreeSpec};
use crate::{find_task, task_context, task_slug, Agent, AgentRegistryFile};
use std::collections::HashMap;
use std::path::PathBuf;

impl OrchestrationService {
    /// Requirement 5 & 6: Spawn an agent with role/vendor/model routing, own worktree,
    /// `SHEP_AGENT_ID` + `SHEP_TOKEN` env, per-session skills, depth and concurrency
    /// limits (requiring confirmation before exceeding them), leftover-pane reclamation
    /// on name collision, and first-prompt delivery.
    pub fn spawn(&self, req: ServiceSpawnRequest) -> Result<Agent, String> {
        let result = self.spawn_inner(req);
        self.log_err("spawn", result)
    }

    fn spawn_inner(&self, mut req: ServiceSpawnRequest) -> Result<Agent, String> {
        // A relative root (e.g. "." from a client that omitted it) resolves against the
        // server's cwd, which would later yield a relative worktree path.
        if !req.root.as_os_str().is_empty() && !req.root.is_absolute() {
            req.root = std::fs::canonicalize(&req.root).map_err(|e| {
                format!(
                    "root must be an absolute path ({}: {e})",
                    req.root.display()
                )
            })?;
        }
        if req.role.trim().is_empty() {
            return Err("role is required".into());
        }
        if req.task.trim().is_empty() {
            return Err("task prompt is required".into());
        }
        let project_cfg = if req.root.as_os_str().is_empty() {
            self.shepherd_config.clone()
        } else {
            load_config(&req.root).unwrap_or_else(|_| self.shepherd_config.clone())
        };
        let role_cfg = project_cfg
            .roles
            .get(&req.role)
            .ok_or_else(|| format!("unknown role {:?}", req.role))?
            .clone();

        let active_vendors = self
            .active_vendors_override
            .clone()
            .unwrap_or_else(|| available_vendors(&project_cfg));
        let (vendor_name, model) = match req.vendor.as_deref().filter(|v| !v.is_empty()) {
            Some(name) => {
                let vendor = project_cfg
                    .vendors
                    .get(name)
                    .ok_or_else(|| format!("unknown vendor {name:?}"))?;
                if vendor.disabled {
                    return Err(format!("vendor {name:?} is disabled"));
                }
                let model = req
                    .model
                    .clone()
                    .filter(|m| !m.is_empty())
                    .or_else(|| {
                        (!role_cfg.model.is_empty()
                            && (vendor.models.is_empty()
                                || vendor.models.contains(&role_cfg.model)))
                        .then(|| role_cfg.model.clone())
                    })
                    .unwrap_or_else(|| vendor.default_model.clone());
                (name.to_owned(), model)
            }
            None => {
                let routed = resolve_role(&project_cfg, &active_vendors, &req.role)
                    .map_err(|e| e.to_string())?;
                let model = req
                    .model
                    .clone()
                    .filter(|m| !m.is_empty())
                    .unwrap_or(routed.model);
                (routed.vendor, model)
            }
        };
        let vendor = project_cfg
            .vendors
            .get(&vendor_name)
            .ok_or_else(|| format!("unknown vendor {vendor_name:?}"))?
            .clone();

        let mut registry = self.load_registry()?;
        let mut depth = 0;
        let parent_id = req.parent_id.clone().unwrap_or_default();
        if !parent_id.is_empty() {
            let parent = registry
                .agents
                .iter()
                .find(|a| a.id == parent_id)
                .ok_or_else(|| format!("unknown parent agent {parent_id:?}"))?;
            let parent_role = project_cfg
                .roles
                .get(&parent.role)
                .ok_or_else(|| format!("unknown parent role {:?}", parent.role))?;
            if !parent_role.can_spawn {
                return Err(format!(
                    "agent {parent_id} (role {}) is not allowed to spawn sub-agents",
                    parent.role
                ));
            }
            depth = parent.depth + 1;
            if depth > project_cfg.limits.max_spawn_depth && !req.confirm_over_limit {
                return Err(format!(
                    "confirmation_required: spawn depth {depth} exceeds max_spawn_depth ({}); pass confirm_over_limit to proceed",
                    project_cfg.limits.max_spawn_depth
                ));
            }
        }
        let live_count = self.active_agent_count(&registry);
        if live_count >= project_cfg.limits.max_concurrent_agents && !req.confirm_over_limit {
            return Err(format!(
                "confirmation_required: max_concurrent_agents ({}) reached ({live_count} live); pass confirm_over_limit to proceed",
                project_cfg.limits.max_concurrent_agents
            ));
        }

        let root_path = if req.root.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            req.root.clone()
        };
        // A role's default worktree only applies inside a git work tree: in a plain folder the agent
        // works in the folder itself instead of failing. Asking for a worktree explicitly still fails.
        let use_worktree = match req.worktree {
            Some(explicit) => explicit,
            None => role_cfg.worktree && crate::git::is_git_work_tree(&root_path),
        };
        let task_id = req.task_id.clone().unwrap_or_default();
        let mcp_url = req
            .mcp_url
            .clone()
            .filter(|u| !u.is_empty())
            .or_else(|| self.mcp_url());
        let mcp_enabled = vendor.mcp.supported && mcp_url.is_some();

        let now = self.clock.now_rfc3339();
        let short_slug = task_slug(if !task_id.is_empty() {
            &task_id
        } else {
            &req.task
        });
        let label = format!(
            "{}·{}·{}",
            req.role,
            vendor_name,
            &short_slug[..short_slug.len().min(16)]
        );
        let mut agent = registry
            .create_secure(
                Agent {
                    role: req.role.clone(),
                    vendor: vendor_name.clone(),
                    model: model.clone(),
                    task_id: task_id.clone(),
                    task_text: req.task.clone(),
                    parent_id,
                    depth,
                    root: root_path.display().to_string(),
                    mcp: mcp_enabled,
                    skills: req.skills.clone(),
                    label,
                    ..Agent::default()
                },
                &now,
            )
            .map_err(|e| e.to_string())?;

        let session_dir = self.service_config.agent_session_dir(&agent.id);
        let catalog = skills::Catalog::load(Some(&root_path), Some(&skills::user_skills_dir()));
        let (_, langs) = skills::detect_checks(&root_path);
        // Every agent learns how to talk to the others; pattern agents also learn the topic tools.
        let mut skill_adjust = req.skills.clone();
        let mut baseline = vec!["shepherd-comms"];
        if req.pattern_prompt.is_some() {
            baseline.push("shepherd-events");
        }
        for name in baseline {
            if !role_cfg.skills.iter().any(|n| n == name)
                && !skill_adjust.add.iter().any(|n| n == name)
                && !skill_adjust.remove.iter().any(|n| n == name)
            {
                skill_adjust.add.push(name.to_owned());
            }
        }
        let (resolved, _problems) =
            catalog.effective_for(&project_cfg, &req.role, &langs, &skill_adjust);
        agent.active_skills = resolved.iter().map(|r| r.skill.name.clone()).collect();
        let skill_launch = skills::install(skills::Plan {
            vendor: vendor.clone(),
            session_dir: session_dir.clone(),
            workdir: root_path.clone(),
            skills: resolved.into_iter().map(|r| r.skill).collect(),
            workspace: skills::workspace_instructions(&root_path),
            repo_docs: skills::repo_instruction_docs(&root_path),
        })?;

        let mut args = vendor.args.clone();
        let effective_model = if model.is_empty() {
            &vendor.default_model
        } else {
            &model
        };
        if !effective_model.is_empty() && !vendor.model_flag.is_empty() {
            args.extend([vendor.model_flag.clone(), effective_model.clone()]);
        }
        if let Some(ref url) = mcp_url {
            if vendor.mcp.supported {
                let mcp_args =
                    mcp_launch_args(&vendor, url, &agent.token).map_err(|e| e.to_string())?;
                args.extend(mcp_args);
            }
        }
        args.extend(skill_launch.args.clone());

        let mut env = HashMap::new();
        env.insert("SHEP_AGENT_ID".into(), agent.id.clone());
        env.insert("SHEP_ROLE".into(), req.role.clone());
        env.insert("SHEP_TOKEN".into(), agent.token.clone());
        if !task_id.is_empty() {
            env.insert("SHEP_TASK_ID".into(), task_id.clone());
        }

        let worktree = if use_worktree {
            let suffix = task_slug(if !task_id.is_empty() {
                &task_id
            } else {
                &req.task
            });
            let branch = if suffix.is_empty() {
                format!("shep/{}", agent.seq)
            } else {
                format!("shep/{}-{suffix}", agent.seq)
            };
            let base = (!project_cfg.workflow.base_branch.trim().is_empty())
                .then(|| project_cfg.workflow.base_branch.clone());
            Some(WorktreeSpec { branch, base })
        } else {
            None
        };

        // Agents join the workspace of whoever started them, else of the live agents of the same
        // task or pattern run, so a task or run never scatters across workspaces.
        let home_workspace = req.workspace.clone().filter(|w| !w.is_empty()).or_else(|| {
            registry
                .agents
                .iter()
                .filter(|a| a.id != agent.id && !a.gone() && !a.workspace.is_empty())
                .find(|a| !agent.parent_id.is_empty() && a.id == agent.parent_id)
                .or_else(|| {
                    registry.agents.iter().find(|a| {
                        a.id != agent.id
                            && !a.gone()
                            && !a.workspace.is_empty()
                            && !task_id.is_empty()
                            && a.task_id == task_id
                    })
                })
                .map(|a| a.workspace.clone())
        });

        let open_req = OpenAgentRequest {
            root: root_path.clone(),
            role: req.role.clone(),
            agent_id: agent.id.clone(),
            vendor_name: vendor_name.clone(),
            command: vendor.command.clone(),
            shepherd_agent: vendor.shepherd_agent.clone(),
            args,
            label: agent.label.clone(),
            env,
            worktree,
            workspace: home_workspace,
        };

        // Requirement 6: Open agent, reclaiming a Shepherd leftover pane once if name collides.
        let opened = match self.open_or_reclaim_leftover(&open_req, &registry) {
            Ok(opened) => opened,
            Err(err) => {
                // Keep monotonic counters (`seq` and `next`) intact even when spawn fails!
                registry.remove(&agent.id);
                let _ = self.save_registry(&registry);
                return Err(err);
            }
        };

        agent.workspace = opened.workspace;
        agent.tab = opened.tab;
        agent.pane = opened.pane;
        agent.worktree = opened.worktree;
        agent.branch = opened.branch;
        agent.updated = self.clock.now_rfc3339();
        if let Some(slot) = registry.agents.iter_mut().find(|a| a.id == agent.id) {
            *slot = agent.clone();
        }
        self.save_registry(&registry)?;

        if req.deliver_prompt {
            let task_ctx = (!task_id.is_empty())
                .then(|| find_task(&root_path, &task_id).ok())
                .flatten()
                .map(|t| task_context(&t, 8000))
                .unwrap_or_default();
            let mut full_prompt = match &req.pattern_prompt {
                // Pattern mode: the pattern prompt replaces the role script and task section.
                Some(pp) => pp.replace(super::pattern_prompt::AGENT_ID_PLACEHOLDER, &agent.id),
                None => {
                    let mut p = role_prompt(&root_path, &req.role, &role_cfg);
                    p.push_str(&format!(
                        "\n\nYour agent id is {} (role {}).",
                        agent.id, agent.role
                    ));
                    if !agent.parent_id.is_empty() {
                        p.push_str(&format!(" You were started by {}.", agent.parent_id));
                    }
                    if !agent.mcp {
                        p.push_str(" The Shepherd MCP tools are not available yet; report your result in this terminal when finished.\n");
                    } else {
                        p.push('\n');
                    }
                    p
                }
            };
            if !skill_launch.prompt.is_empty() {
                full_prompt.push_str("\n\n");
                full_prompt.push_str(&skill_launch.prompt);
            }
            if req.pattern_prompt.is_none() {
                if !task_ctx.is_empty() {
                    full_prompt.push_str("\n\n");
                    full_prompt.push_str(&task_ctx);
                }
                full_prompt.push_str(&format!("\n\n## Your task\n\n{}\n", req.task.trim()));
            }
            if !agent.mcp {
                full_prompt.push('\n');
                full_prompt.push_str(RUNTIME_CONSTRAINTS);
            }
            let terminal_status = self.terminals.agent_status(&agent.id).ok();
            let is_ready = terminal_status
                .as_ref()
                .map(|s| matches!(s.status.as_str(), "working" | "idle"))
                .unwrap_or(false);
            if is_ready {
                self.deliver_first_prompt(&agent.id, &full_prompt)?;
                let refreshed = self.load_registry()?;
                if let Some(updated) = refreshed.result(&agent.id) {
                    return Ok(updated.clone());
                }
            } else {
                let this = self.clone();
                let ag_id = agent.id.clone();
                let prompt_text = full_prompt.clone();
                std::thread::Builder::new()
                    .name(format!("shepherd-deliver-{}", agent.id))
                    .spawn(move || {
                        let _ = this.deliver_first_prompt(&ag_id, &prompt_text);
                    })
                    .map_err(|e| e.to_string())?;
            }
        }

        Ok(agent)
    }

    /// Requirement 6: Unique agent names among open panes.
    /// If `open_agent` collides with an occupied name:
    /// - Check if the occupying pane is clearly a Shepherd leftover (label starts with `<role>·`
    ///   and no live registered agent in `registry` lives in it).
    /// - If so, close it and retry `open_agent` once.
    /// - Otherwise, NEVER close it and return an error explaining which pane/agent occupies it.
    fn open_or_reclaim_leftover(
        &self,
        open_req: &OpenAgentRequest,
        registry: &AgentRegistryFile,
    ) -> Result<crate::terminals::OpenedAgent, String> {
        if let Ok(existing) = self.terminals.agent_status(&open_req.agent_id) {
            if existing.state != "finished" {
                let expected_prefix = format!("{}·", open_req.role);
                let has_leftover_label = existing.title.starts_with(&expected_prefix);
                let occupied_by_live_registered = registry.live().iter().any(|live| {
                    live.id != open_req.agent_id
                        && ((!existing.pane.is_empty() && live.pane == existing.pane)
                            || (!existing.tab.is_empty() && live.tab == existing.tab))
                }) || registry
                    .live()
                    .iter()
                    .any(|live| live.id == open_req.agent_id && !live.pane.is_empty());

                if has_leftover_label && !occupied_by_live_registered {
                    let close_target = if !existing.tab.is_empty() {
                        existing.tab.as_str()
                    } else if !existing.pane.is_empty() {
                        existing.pane.as_str()
                    } else {
                        open_req.agent_id.as_str()
                    };
                    self.terminals.close(close_target).map_err(|e| {
                        format!("failed to close leftover pane {close_target}: {e}")
                    })?;
                    return self
                        .terminals
                        .open_agent(open_req)
                        .map_err(|e| e.to_string());
                } else {
                    return Err(format!(
                        "el nombre de agente {:?} está ocupado por el panel {:?} (etiqueta {:?}); no es un resto de Shepherd y no se cerrará",
                        open_req.agent_id, existing.pane, existing.title
                    ));
                }
            }
        }
        self.terminals
            .open_agent(open_req)
            .map_err(|e| e.to_string())
    }
}
