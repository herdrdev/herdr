//! MCP tool definitions and dispatch for agent workflows.

use super::events::handle_event_tool;
use super::State;
use crate::{
    agents_json,
    bus::{Message, MessageBus, KIND_MESSAGE, KIND_RESULT},
    config, find_task, load_agent_registry, read_task_section, save_agent_registry,
    skills::{self, SkillAdjust},
    spawn::{self, SpawnOptions, SpawnRequest},
    task_context,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) fn tool_list() -> Value {
    json!({"tools": [
        {
            "name": "spawn_agent",
            "description": "Start a sub-agent in its own terminal (requires a role with can_spawn: true).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role": {"type": "string"},
                    "task": {"type": "string"},
                    "vendor": {"type": "string"},
                    "model": {"type": "string"},
                    "task_id": {"type": "string"},
                    "use_worktree": {"type": "string"},
                    "add_skills": {"type": "array", "items": {"type": "string"}},
                    "remove_skills": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["role", "task"]
            }
        },
        {
            "name": "send_message",
            "description": "Send a message to another agent by ID or role.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["to", "content"]
            }
        },
        {
            "name": "read_messages",
            "description": "Read and clear your pending agent messages.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "wait_for",
            "description": "Block until every listed agent has submitted a result or exited.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent_ids": {"type": "array", "items": {"type": "string"}},
                    "timeout_seconds": {"type": "integer"}
                },
                "required": ["agent_ids"]
            }
        },
        {
            "name": "submit_result",
            "description": "Deliver your final result to Shepherd.",
            "inputSchema": {
                "type": "object",
                "properties": {"summary": {"type": "string"}},
                "required": ["summary"]
            }
        },
        {
            "name": "get_result",
            "description": "Return an agent's current state and submitted result without blocking.",
            "inputSchema": {
                "type": "object",
                "properties": {"agent_id": {"type": "string"}},
                "required": ["agent_id"]
            }
        },
        {
            "name": "list_agents",
            "description": "List all agents in this session with their role, vendor, state, and worktree.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "get_diff",
            "description": "Read a coder's complete worktree diff against the configured base branch.",
            "inputSchema": {
                "type": "object",
                "properties": {"agent_id": {"type": "string"}},
                "required": ["agent_id"]
            }
        },
        {
            "name": "read_output",
            "description": "Read the last N lines of an agent's terminal pane.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "lines": {"type": "integer"}
                },
                "required": ["agent_id"]
            }
        },
        {
            "name": "ask_docs",
            "description": "Send a question to the docs agent (spawning one if needed) and wait for its answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "timeout_seconds": {"type": "integer"}
                },
                "required": ["question"]
            }
        },
        {
            "name": "get_task_context",
            "description": "Read this task's issue, specification, acceptance criteria, links, or plan.",
            "inputSchema": {
                "type": "object",
                "properties": {"section": {"type": "string"}}
            }
        },
        {
            "name": "update_task_context",
            "description": "Update a task context section (brain and docs agents only).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "section": {"type": "string"},
                    "content": {"type": "string"},
                    "append": {"type": "boolean"}
                },
                "required": ["section", "content"]
            }
        },
        {
            "name": "get_workspace_instructions",
            "description": "Return workspace instructions, repo instruction files, effective skills, and detected checks.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "publish",
            "description": "Publish a message to a topic. Key hashes to a partition; without key, messages are distributed across partitions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string"},
                    "content": {"type": "string"},
                    "key": {"type": "string"}
                },
                "required": ["topic", "content"]
            }
        },
        {
            "name": "subscribe",
            "description": "Subscribe to a topic, optionally joining a consumer group (defaults to your own agent ID for pub/sub delivery).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string"},
                    "group": {"type": "string"}
                },
                "required": ["topic"]
            }
        },
        {
            "name": "poll",
            "description": "Poll unacknowledged topic messages assigned to this agent in its consumer group.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string"},
                    "max": {"type": "integer"}
                },
                "required": ["topic"]
            }
        },
        {
            "name": "ack",
            "description": "Acknowledge (commit) processed messages for a topic and partition up through an offset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string"},
                    "partition": {"type": "integer"},
                    "offset": {"type": "integer"},
                    "through": {"type": "integer"}
                },
                "required": ["topic"]
            }
        },
        {
            "name": "replay",
            "description": "Read historical messages from a topic without advancing consumer group offsets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string"},
                    "from_offset": {"type": "integer"},
                    "partition": {"type": "integer"},
                    "limit": {"type": "integer"}
                },
                "required": ["topic", "from_offset"]
            }
        },
        {
            "name": "list_topics",
            "description": "List available topics, partition counts, and message volumes.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ]})
}

pub(crate) fn call_tool(
    state: &State,
    agent: &crate::Agent,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tool name is required")?;
    let args = params.get("arguments").unwrap_or(&Value::Null);
    let text = match name {
        "spawn_agent" => {
            let role = args
                .get("role")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("role is required")?;
            let task = args
                .get("task")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("task is required")?;
            let vendor = args
                .get("vendor")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let model = args
                .get("model")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let task_id = args
                .get("task_id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| (!agent.task_id.is_empty()).then(|| agent.task_id.clone()));
            let use_worktree = args
                .get("use_worktree")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let add_skills = string_array_arg(args, "add_skills");
            let remove_skills = string_array_arg(args, "remove_skills");

            let root = PathBuf::from(&agent.root);
            let terminals = state.resolve_terminals(&root)?;
            let spawned = spawn::spawn_child(
                SpawnRequest {
                    root,
                    role: role.to_owned(),
                    task: task.to_owned(),
                    task_id,
                    vendor,
                    model,
                    mcp_url: state.mcp_url.clone(),
                },
                SpawnOptions {
                    parent_id: Some(agent.id.clone()),
                    registry_path: Some(state.registry.clone()),
                    active_vendors: state.active_vendors.clone(),
                    terminals: terminals.as_ref(),
                },
                SkillAdjust {
                    add: add_skills,
                    remove: remove_skills,
                },
                use_worktree,
            )
            .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&spawned.agent.public()).map_err(|e| e.to_string())?
        }
        "submit_result" => {
            let summary = args
                .get("summary")
                .and_then(Value::as_str)
                .ok_or("summary is required")?;
            let mut registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            registry.submit_result(&agent.id, summary, &crate::now_rfc3339())?;
            save_agent_registry(&state.registry, &registry).map_err(|error| error.to_string())?;
            // The task is done when everyone assigned to it has delivered; a plain task has no other
            // flow that would close it.
            let others_working = registry
                .agents
                .iter()
                .any(|a| a.task_id == agent.task_id && a.id != agent.id && !a.terminal());
            if !agent.task_id.is_empty() && !agent.root.is_empty() && !others_working {
                let _ = crate::complete_direct_task(&agent.root, &agent.task_id);
            }
            if !agent.parent_id.is_empty() {
                if let Ok(bus) = MessageBus::open(&state.bus_path) {
                    let _ = bus.send(Message::directed(
                        agent.id.clone(),
                        agent.parent_id.clone(),
                        KIND_RESULT,
                        format!("{} finished: {}", agent.id, summary.trim()),
                    ));
                }
            }
            "result recorded".to_owned()
        }
        "get_result" => {
            let id = args
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("agent_id is required")?;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let target = registry
                .result(id)
                .ok_or_else(|| format!("unknown agent {id:?}"))?;
            serde_json::to_string_pretty(&json!({
                "id": target.id,
                "state": target.state,
                "has_result": target.has_result,
                "result": target.result,
                "error": target.error,
                "worktree": target.worktree,
                "branch": target.branch
            }))
            .map_err(|e| e.to_string())?
        }
        "list_agents" => {
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            agents_json(&registry).map_err(|e| e.to_string())?
        }
        "wait_for" => {
            let ids = string_array_arg(args, "agent_ids");
            if ids.is_empty() {
                return Err("agent_ids is empty".into());
            }
            let timeout_secs = args
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(1800);
            let deadline = Instant::now() + Duration::from_secs(timeout_secs);
            loop {
                let registry =
                    load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
                let mut out = BTreeMap::new();
                let mut all_done = true;
                for id in &ids {
                    match registry.result(id) {
                        None => {
                            out.insert(id.clone(), json!({"error": "unknown agent"}));
                        }
                        Some(a) => {
                            if !a.has_result && !a.terminal() {
                                all_done = false;
                            }
                            out.insert(
                                id.clone(),
                                json!({
                                    "state": a.state,
                                    "result": a.result,
                                    "error": a.error
                                }),
                            );
                        }
                    }
                }
                if all_done {
                    break serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
                }
                if Instant::now() >= deadline {
                    let partial =
                        serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".into());
                    return Err(format!("timeout waiting for {ids:?}: {partial}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        "read_output" => {
            let id = args
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("agent_id is required")?;
            let lines = args
                .get("lines")
                .and_then(Value::as_u64)
                .filter(|&n| n > 0)
                .unwrap_or(80) as u32;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let target = registry
                .result(id)
                .ok_or_else(|| format!("unknown agent {id:?}"))?;
            let pane = if !target.pane.is_empty() {
                &target.pane
            } else {
                &target.id
            };
            let terminals = state.resolve_terminals(Path::new(&agent.root))?;
            terminals
                .read_screen(pane, lines)
                .map_err(|e| e.to_string())?
        }
        "ask_docs" => {
            let question = args
                .get("question")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("question is required")?;
            let timeout_secs = args
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(300);
            let root = PathBuf::from(&agent.root);
            let terminals = state.resolve_terminals(&root)?;

            let docs_id = {
                let registry =
                    load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
                if let Some(existing) = registry.live().into_iter().find(|a| a.role == "docs") {
                    existing.id.clone()
                } else {
                    let spawned = spawn::spawn_child(
                        SpawnRequest {
                            root: root.clone(),
                            role: "docs".into(),
                            task:
                                "Answer documentation questions from other agents via Shepherd messages."
                                    .into(),
                            task_id: (!agent.task_id.is_empty()).then(|| agent.task_id.clone()),
                            vendor: None,
                            model: None,
                            mcp_url: state.mcp_url.clone(),
                        },
                        SpawnOptions {
                            parent_id: None,
                            registry_path: Some(state.registry.clone()),
                            active_vendors: state.active_vendors.clone(),
                            terminals: terminals.as_ref(),
                        },
                        SkillAdjust::default(),
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                    spawned.agent.id
                }
            };

            let prompt_msg = format!(
                "Question from {}: {question}\nReply with send_message(to={:?}, content=...) or submit_result.",
                agent.id, agent.id
            );
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let sent = bus
                .send(Message::directed(
                    agent.id.clone(),
                    docs_id.clone(),
                    KIND_MESSAGE,
                    prompt_msg.clone(),
                ))
                .map_err(|e| e.to_string())?;
            let _ = terminals.prompt_text(&docs_id, &prompt_msg);

            let deadline = Instant::now() + Duration::from_secs(timeout_secs);
            loop {
                let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
                for msg in bus.since(sent.id).map_err(|e| e.to_string())? {
                    if msg.from == docs_id && msg.to == agent.id {
                        return Ok(tool_ok(msg.content));
                    }
                }
                let registry =
                    load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
                if let Some(docs_agent) = registry.result(&docs_id) {
                    if docs_agent.has_result {
                        return Ok(tool_ok(docs_agent.result.clone()));
                    }
                    if docs_agent.terminal() {
                        return Err(format!(
                            "docs agent {docs_id} ended in state {} without answering",
                            docs_agent.state
                        ));
                    }
                }
                if Instant::now() >= deadline {
                    return Err(format!("timed out waiting for docs agent {docs_id}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        "get_task_context" => {
            if agent.task_id.is_empty() {
                return Err("this agent has no task context".into());
            }
            if let Some(ctx) = state.registry.parent().and_then(|dir| {
                crate::service::patterns_run::pattern_run_context(
                    dir,
                    std::path::Path::new(&agent.root),
                    &agent.task_id,
                    &agent.id,
                )
            }) {
                return Ok(tool_ok(ctx));
            }
            let task = find_task(&agent.root, &agent.task_id).map_err(|error| error.to_string())?;
            match args
                .get("section")
                .and_then(Value::as_str)
                .filter(|section| !section.is_empty())
            {
                Some(section) => {
                    read_task_section(&task, section).map_err(|error| error.to_string())?
                }
                None => task_context(&task, 0),
            }
        }
        "read_messages" => {
            let bus = MessageBus::open(&state.bus_path).map_err(|error| error.to_string())?;
            let messages = bus.read(&agent.id).map_err(|error| error.to_string())?;
            serde_json::to_string(&json!({"messages": messages}))
                .map_err(|error| error.to_string())?
        }
        "send_message" => {
            let to = args
                .get("to")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or("recipient is required")?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or("message content is required")?;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let recipient = if to == "user" || registry.result(to).is_some() {
                to.to_owned()
            } else {
                registry
                    .live()
                    .into_iter()
                    .find(|candidate| candidate.role == to)
                    .map(|candidate| candidate.id.clone())
                    .ok_or_else(|| format!("no live agent or role named {to:?}"))?
            };
            let bus = MessageBus::open(&state.bus_path).map_err(|error| error.to_string())?;
            let message = bus
                .send(Message::directed(
                    agent.id.clone(),
                    recipient.clone(),
                    KIND_MESSAGE,
                    content.to_owned(),
                ))
                .map_err(|error| error.to_string())?;
            format!("delivered message {} to {}", message.id, recipient)
        }
        "get_diff" => {
            let id = args
                .get("agent_id")
                .and_then(Value::as_str)
                .ok_or("agent_id is required")?;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let coder = registry
                .result(id)
                .ok_or_else(|| format!("unknown agent {id:?}"))?;
            if coder.worktree.is_empty() {
                return Err(format!("agent {id:?} has no worktree"));
            }
            let config = config::load_config(&coder.root).map_err(|error| error.to_string())?;
            let base = default_base(&coder.root, &config.workflow.base_branch);
            let diff = std::process::Command::new("git")
                .args(["diff", &base])
                .current_dir(&coder.worktree)
                .output()
                .map_err(|error| error.to_string())?;
            if !diff.status.success() {
                return Err(String::from_utf8_lossy(&diff.stderr).trim().to_owned());
            }
            let mut text = String::from_utf8_lossy(&diff.stdout).into_owned();
            let untracked = std::process::Command::new("git")
                .args(["ls-files", "--others", "--exclude-standard"])
                .current_dir(&coder.worktree)
                .output()
                .map_err(|error| error.to_string())?;
            if untracked.status.success() && !untracked.stdout.is_empty() {
                text.push_str("\n\n# untracked files (not in the diff above):\n# ");
                text.push_str(
                    &String::from_utf8_lossy(&untracked.stdout)
                        .trim()
                        .replace('\n', "\n# "),
                );
            }
            if text.len() > 200_000 {
                let mut end = 200_000;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str("\n... (diff truncated)");
            }
            text
        }
        "update_task_context" => {
            if !matches!(agent.role.as_str(), "brain" | "docs") {
                return Err(format!("role {:?} may not update task context", agent.role));
            }
            if agent.task_id.is_empty() {
                return Err("this agent has no task context".into());
            }
            let section = args
                .get("section")
                .and_then(Value::as_str)
                .ok_or("section is required")?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or("content is required")?;
            let task = find_task(&agent.root, &agent.task_id).map_err(|error| error.to_string())?;
            let content = if args.get("append").and_then(Value::as_bool).unwrap_or(false) {
                let previous = read_task_section(&task, section).unwrap_or_default();
                format!("{}\n\n{}\n", previous.trim_end(), content)
            } else {
                content.to_owned()
            };
            crate::write_task_section(&task, section, &content)
                .map_err(|error| error.to_string())?;
            crate::touch_task(&task).map_err(|error| error.to_string())?;
            "task context updated".to_owned()
        }
        "get_workspace_instructions" => {
            let root = Path::new(&agent.root);
            let cfg = config::load_config(root).unwrap_or_default();
            let cat = skills::Catalog::load(Some(root), Some(&skills::user_skills_dir()));
            let (mut checks, langs) = skills::detect_checks(root);
            if !cfg.workflow.checks.is_empty() {
                checks = cfg.workflow.checks.clone();
            }
            let (eff, problems) = cat.effective_for(&cfg, &agent.role, &langs, &agent.skills);
            let skill_items: Vec<Value> = eff
                .into_iter()
                .map(|r| {
                    json!({
                        "name": r.skill.name,
                        "description": r.skill.description,
                        "scope": r.skill.scope,
                        "level": r.level,
                        "dir": r.skill.dir
                    })
                })
                .collect();
            let payload = json!({
                "instructions": skills::workspace_instructions(root),
                "repo_docs": skills::repo_instruction_docs(root),
                "skills": skill_items,
                "global_skills": cat.global_names(&cfg),
                "disabled": cfg.skills.disabled,
                "problems": problems,
                "checks": checks,
                "languages": langs
            });
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        }

        other => handle_event_tool(state, agent, other, args)?,
    };
    Ok(tool_ok(text))
}

fn tool_ok(text: String) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": {"text": text},
        "isError": false
    })
}

fn string_array_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn default_base(root: &str, configured: &str) -> String {
    if !configured.is_empty() {
        return configured.to_owned();
    }
    if let Ok(output) = std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .current_dir(root)
        .output()
    {
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !value.is_empty() {
                return value;
            }
        }
    }
    for branch in ["main", "master", "develop"] {
        if std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", branch])
            .current_dir(root)
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return branch.into();
        }
    }
    "HEAD".into()
}
