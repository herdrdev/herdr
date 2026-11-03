//! Rust spawner: Shepherd selects the role and records identity; `Terminals` creates and owns terminals.

use crate::{
    config::{available_vendors, load_config, resolve_role},
    find_task, new_agent_token, save_agent_registry, task_context, task_slug,
    terminals::{connect_terminals, OpenAgentRequest, PromptOutcome, Terminals, WorktreeSpec},
    Agent, AgentRegistryFile,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct SpawnRequest {
    pub root: PathBuf,
    pub role: String,
    pub task: String,
    pub task_id: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub mcp_url: Option<String>,
}

pub struct SpawnOptions<'a> {
    pub parent_id: Option<String>,
    pub registry_path: Option<PathBuf>,
    pub active_vendors: Option<Vec<String>>,
    pub terminals: &'a dyn Terminals,
}

#[derive(Clone, Debug)]
pub struct SpawnedAgent {
    pub agent: Agent,
    pub task_directory: PathBuf,
}

struct RuntimeAgent {
    workspace: String,
    tab: String,
    pane: String,
    label: String,
    worktree: String,
    branch: String,
    state: String,
    notice: String,
    active_skills: Vec<String>,
}

const RUNTIME_CONSTRAINTS: &str = "## Runtime constraints (take precedence over the role instructions above)\nThe Shepherd MCP server is not connected in this runtime. Do not attempt MCP calls, including get_task_context, read_messages, send_message, update_task_context, or submit_result. Use the task and context included in this prompt, then report your result in the terminal.\n";

pub fn spawn(request: SpawnRequest) -> Result<SpawnedAgent, Box<dyn std::error::Error + Send + Sync>> {
    let config = load_config(&request.root)?;
    let terminals = connect_terminals(&config)?;
    spawn_with_options(
        request,
        SpawnOptions {
            parent_id: None,
            registry_path: None,
            active_vendors: None,
            terminals: terminals.as_ref(),
        },
    )
}

pub fn spawn_with_terminals(
    request: SpawnRequest,
    terminals: &dyn Terminals,
) -> Result<SpawnedAgent, Box<dyn std::error::Error + Send + Sync>> {
    spawn_with_options(
        request,
        SpawnOptions {
            parent_id: None,
            registry_path: None,
            active_vendors: None,
            terminals,
        },
    )
}

pub fn spawn_with_options(
    request: SpawnRequest,
    options: SpawnOptions<'_>,
) -> Result<SpawnedAgent, Box<dyn std::error::Error + Send + Sync>> {
    spawn_child(request, options, crate::SkillAdjust::default(), None)
}

pub fn spawn_child(
    request: SpawnRequest,
    options: SpawnOptions<'_>,
    skills: crate::SkillAdjust,
    use_worktree: Option<String>,
) -> Result<SpawnedAgent, Box<dyn std::error::Error + Send + Sync>> {
    if request.task.trim().is_empty() {
        return Err("a spawn needs a task description".into());
    }
    let config = load_config(&request.root)?;
    let active = options
        .active_vendors
        .unwrap_or_else(|| available_vendors(&config));
    let mut assignment = resolve_role(&config, &active, &request.role)?;
    if let Some(vendor) = &request.vendor {
        if !active.contains(vendor) {
            return Err(format!(
                "vendor {vendor:?} is not installed/enabled (active: {})",
                active.join(", ")
            )
            .into());
        }
        assignment.vendor = vendor.clone();
        assignment.model = config.vendors[vendor].default_model.clone();
        assignment.fallback = vendor != &config.roles[&request.role].vendor;
    }
    if let Some(model) = &request.model {
        assignment.model = model.clone();
    }
    if !config.vendors.contains_key(&assignment.vendor) {
        return Err(format!("unknown vendor {:?}", assignment.vendor).into());
    }

    let task = request
        .task_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|id| find_task(&request.root, id))
        .transpose()?;
    let registry_path = options
        .registry_path
        .unwrap_or_else(|| crate::config::state_dir().join("agents.json"));
    let mut registry = load_agent_registry_at(&registry_path)?;

    let mut depth = 0;
    let parent_id = options.parent_id.unwrap_or_default();
    if !parent_id.is_empty() {
        let Some(parent) = registry.result(&parent_id) else {
            return Err(format!("unknown parent agent {parent_id:?}").into());
        };
        let can_spawn = config
            .roles
            .get(&parent.role)
            .is_some_and(|role| role.can_spawn);
        if !can_spawn {
            return Err(format!(
                "role {:?} is not allowed to spawn agents (can_spawn: false)",
                parent.role
            )
            .into());
        }
        depth = parent.depth + 1;
    }
    if depth > config.limits.max_spawn_depth {
        return Err(format!(
            "needs human confirmation: spawn depth {depth} exceeds limits.max_spawn_depth={}",
            config.limits.max_spawn_depth
        )
        .into());
    }
    let live = registry.live().len();
    if live >= config.limits.max_concurrent_agents {
        return Err(format!(
            "needs human confirmation: {live} agents already running (limits.max_concurrent_agents={})",
            config.limits.max_concurrent_agents
        )
        .into());
    }

    let vendor = &config.vendors[&assignment.vendor];
    let mcp = vendor.mcp.supported
        && request
            .mcp_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty());
    let token = new_agent_token()?;
    let mut agent = registry.create(
        Agent {
            role: request.role.clone(),
            vendor: assignment.vendor.clone(),
            model: assignment.model.clone(),
            task_id: request.task_id.clone().unwrap_or_default(),
            task_text: request.task.clone(),
            parent_id,
            depth,
            root: request.root.display().to_string(),
            mcp,
            skills,
            ..Agent::default()
        },
        token,
        &now_timestamp(),
    );
    save_agent_registry(&registry_path, &registry)?;

    let session_root = registry_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("sessions")
        .join("current")
        .join("agents")
        .join(&agent.id);

    let result = spawn_in_terminals(
        &request,
        &config,
        &assignment.vendor,
        &agent,
        task.as_ref().map(|t| task_context(t, 8000)).unwrap_or_default(),
        &session_root,
        use_worktree.as_deref(),
        options.terminals,
    );
    match result {
        Ok(runtime) => {
            agent.workspace = runtime.workspace;
            agent.tab = runtime.tab;
            agent.pane = runtime.pane;
            agent.label = runtime.label;
            agent.worktree = runtime.worktree;
            agent.branch = runtime.branch;
            agent.state = runtime.state;
            agent.notice = runtime.notice;
            agent.active_skills = runtime.active_skills;
            let record = registry
                .agents
                .iter_mut()
                .find(|record| record.id == agent.id)
                .expect("new agent exists");
            *record = agent.clone();
            save_agent_registry(&registry_path, &registry)?;
            Ok(SpawnedAgent {
                agent,
                task_directory: task.map(|t| t.directory.clone()).unwrap_or_default(),
            })
        }
        Err(error) => {
            if let Some(record) = registry.agents.iter_mut().find(|record| record.id == agent.id) {
                record.state = "failed".into();
                record.error = error.to_string();
            }
            let _ = save_agent_registry(&registry_path, &registry);
            Err(error)
        }
    }
}

fn spawn_in_terminals(
    request: &SpawnRequest,
    config: &crate::config::Config,
    vendor_name: &str,
    agent: &Agent,
    task_context: String,
    session_dir: &Path,
    use_worktree: Option<&str>,
    terminals: &dyn Terminals,
) -> Result<RuntimeAgent, Box<dyn std::error::Error + Send + Sync>> {
    let vendor = &config.vendors[vendor_name];
    let short = task_slug(&request.task);
    let label = format!(
        "{}·{}·{}",
        request.role,
        vendor_name,
        &short[..short.len().min(16)]
    );
    let mut env = HashMap::from([
        ("SHEP_AGENT_ID".to_owned(), agent.id.clone()),
        ("SHEP_ROLE".to_owned(), request.role.clone()),
    ]);
    if agent.mcp {
        env.insert("SHEP_TOKEN".into(), agent.token.clone());
    }

    let mut args = vendor.args.clone();
    let model = if agent.model.is_empty() {
        &vendor.default_model
    } else {
        &agent.model
    };
    if !model.is_empty() && !vendor.model_flag.is_empty() {
        args.extend([vendor.model_flag.clone(), model.clone()]);
    }
    if agent.mcp {
        args.extend(mcp_launch_args(
            vendor,
            request.mcp_url.as_deref().expect("MCP URL was checked"),
            &agent.token,
        )?);
    }

    let (open_root, worktree, explicit_wt) = if let Some(wt) = use_worktree.filter(|s| !s.trim().is_empty()) {
        let wt_path = PathBuf::from(wt);
        if !wt_path.exists() {
            return Err(format!("{wt} is not a worktree of {}", request.root.display()).into());
        }
        (wt_path, None, Some(wt.to_owned()))
    } else {
        let wt = config.roles[&request.role].worktree.then(|| WorktreeSpec {
            branch: format!("shep/{}-{}", agent.seq, task_slug(&request.task)),
            base: config.workflow.base_branch.clone(),
        });
        (request.root.clone(), wt, None)
    };

    let cat = crate::skills::Catalog::load(
        Some(&request.root),
        Some(&crate::skills::user_skills_dir()),
    );
    let (_, langs) = crate::skills::detect_checks(&open_root);
    let (resolved, _problems) = cat.effective_for(config, &request.role, &langs, &agent.skills);
    let active_skills: Vec<String> = resolved.iter().map(|r| r.skill.name.clone()).collect();
    let skill_launch = crate::skills::install(crate::skills::Plan {
        vendor: vendor.clone(),
        session_dir: session_dir.to_path_buf(),
        workdir: open_root.clone(),
        skills: resolved.into_iter().map(|r| r.skill).collect(),
        workspace: crate::skills::workspace_instructions(&request.root),
        repo_docs: crate::skills::repo_instruction_docs(&request.root),
    })?;
    args.extend(skill_launch.args);

    let opened = terminals.open_agent(&OpenAgentRequest {
        root: open_root,
        role: request.role.clone(),
        agent_id: agent.id.clone(),
        vendor_name: vendor_name.to_owned(),
        command: vendor.command.clone(),
        herdr_agent: vendor.herdr_agent.clone(),
        args,
        label: label.clone(),
        env,
        worktree,
    })?;

    let mut prompt = role_prompt(&request.root, &request.role, &config.roles[&request.role]);
    prompt.push_str(&format!(
        "\n\nYour agent id is {} (role {}).",
        agent.id, agent.role
    ));
    if !agent.parent_id.is_empty() {
        prompt.push_str(&format!(" You were started by {}.", agent.parent_id));
    }
    if !agent.mcp {
        prompt.push_str(" The Shepherd MCP tools are not available yet; report your result in this terminal when finished.\n");
    } else {
        prompt.push('\n');
    }
    if !skill_launch.prompt.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&skill_launch.prompt);
    }
    if !task_context.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&task_context);
    }
    prompt.push_str(&format!("\n\n## Your task\n\n{}\n", request.task.trim()));
    if !agent.mcp {
        prompt.push('\n');
        prompt.push_str(RUNTIME_CONSTRAINTS);
    }

    let (state, notice) = if vendor.herdr_agent.is_empty() {
        terminals.send_text(&opened.pane, &prompt)?;
        ("starting".to_owned(), String::new())
    } else {
        match terminals.prompt(&agent.id, &prompt)? {
            PromptOutcome::Delivered => ("starting".to_owned(), String::new()),
            PromptOutcome::Blocked => (
                "blocked".to_owned(),
                "Herdr says the agent is waiting for human input; attach to its pane, resolve the prompt, then send its task.".into(),
            ),
        }
    };

    Ok(RuntimeAgent {
        workspace: opened.workspace,
        tab: opened.tab,
        pane: opened.pane,
        label,
        worktree: explicit_wt.unwrap_or(opened.worktree),
        branch: opened.branch,
        state,
        notice,
        active_skills,
    })
}

fn role_prompt(root: &Path, role_name: &str, role: &crate::config::Role) -> String {
    let mut candidates = Vec::new();
    if !role.prompt.is_empty() {
        candidates.push(if Path::new(&role.prompt).is_absolute() {
            PathBuf::from(&role.prompt)
        } else {
            root.join(&role.prompt)
        });
    }
    candidates.push(root.join(".shepherd/prompts").join(format!("{role_name}.md")));
    if let Some(prompt) = candidates
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
    {
        return prompt.trim().to_owned();
    }
    match role_name {
        "brain" => include_str!("../assets/prompts/brain.md").trim().to_owned(),
        "coder" => include_str!("../assets/prompts/coder.md").trim().to_owned(),
        "reviewer" => include_str!("../assets/prompts/reviewer.md").trim().to_owned(),
        "docs" => include_str!("../assets/prompts/docs.md").trim().to_owned(),
        "tester" => include_str!("../assets/prompts/tester.md").trim().to_owned(),
        _ => include_str!("../assets/prompts/generic.md")
            .replace("{{role}}", role_name)
            .trim()
            .to_owned(),
    }
}

pub(crate) fn shell_join(args: impl Iterator<Item = impl AsRef<str>>) -> String {
    args.map(|argument| {
        let argument = argument.as_ref();
        if !argument.is_empty()
            && argument
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@".contains(c))
        {
            argument.to_owned()
        } else {
            format!("'{}'", argument.replace('\'', "'\\''"))
        }
    })
    .collect::<Vec<_>>()
    .join(" ")
}

fn mcp_launch_args(
    vendor: &crate::config::Vendor,
    url: &str,
    token: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(match vendor.mcp.style.as_str() {
        "claude-config" => {
            let config = serde_json::json!({"mcpServers":{"shepherd":{"type":"http","url":url,"headers":{"Authorization":format!("Bearer {token}")}}}});
            vec![
                "--mcp-config".into(),
                serde_json::to_string(&config)?,
                "--allowedTools".into(),
                "mcp__shepherd".into(),
            ]
        }
        "codex-overrides" => vec![
            "-c".into(),
            format!("mcp_servers.shepherd.url={}", serde_json::to_string(url)?),
            "-c".into(),
            "mcp_servers.shepherd.bearer_token_env_var=\"SHEP_TOKEN\"".into(),
            "-c".into(),
            "mcp_servers.shepherd.default_tools_approval_mode=\"approve\"".into(),
        ],
        style => return Err(format!("unknown MCP launch style {style:?}").into()),
    })
}

fn load_agent_registry_at(
    path: &Path,
) -> Result<AgentRegistryFile, Box<dyn std::error::Error + Send + Sync>> {
    crate::load_agent_registry(path)
}

fn now_timestamp() -> String {
    super::now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminals::InMemoryTerminals;
    use serde_json::Value;

    #[test]
    fn shell_join_quotes_arguments_without_shell_expansion() {
        assert_eq!(
            shell_join(["claude", "--model", "opus 4", "it's $HOME"].into_iter()),
            "claude --model 'opus 4' 'it'\\''s $HOME'"
        );
        assert_eq!(
            shell_join(["gemini", "--model", "gemini-2.5"].into_iter()),
            "gemini --model gemini-2.5"
        );
    }

    #[test]
    fn prompt_marks_runtime_limit_as_higher_priority() {
        assert!(RUNTIME_CONSTRAINTS.contains("take precedence"));
        assert!(RUNTIME_CONSTRAINTS.contains("MCP server is not connected"));
    }

    #[test]
    fn vendor_mcp_arguments_configure_authentication() {
        let config = crate::config::Config::default();
        let codex =
            mcp_launch_args(&config.vendors["codex"], "http://127.0.0.1:4321/mcp", "secret")
                .unwrap();
        assert_eq!(
            codex[1],
            "mcp_servers.shepherd.url=\"http://127.0.0.1:4321/mcp\""
        );
        assert_eq!(
            codex[3],
            "mcp_servers.shepherd.bearer_token_env_var=\"SHEP_TOKEN\""
        );
        assert!(!codex.join(" ").contains("secret"));
        let claude =
            mcp_launch_args(&config.vendors["claude"], "http://127.0.0.1:4321/mcp", "secret")
                .unwrap();
        let launch_config: Value = serde_json::from_str(&claude[1]).unwrap();
        assert_eq!(
            launch_config["mcpServers"]["shepherd"]["headers"]["Authorization"],
            "Bearer secret"
        );
    }

    #[test]
    fn spawn_uses_terminals_trait_and_enforces_can_spawn_and_depth_limits() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("shepherd-spawn-trait-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let registry_path = root.join("agents.json");
        let terminals = InMemoryTerminals::new();
        let active = vec!["claude".to_owned(), "codex".to_owned(), "gemini".to_owned()];

        let brain = spawn_with_options(
            SpawnRequest {
                root: root.clone(),
                role: "brain".into(),
                task: "Plan task".into(),
                task_id: None,
                vendor: None,
                model: None,
                mcp_url: Some("http://127.0.0.1:4317/mcp".into()),
            },
            SpawnOptions {
                parent_id: None,
                registry_path: Some(registry_path.clone()),
                active_vendors: Some(active.clone()),
                terminals: &terminals,
            },
        )
        .unwrap();
        assert_eq!(brain.agent.id, "brain-1");
        assert_eq!(brain.agent.pane, "pane-1");
        assert!(brain.agent.worktree.is_empty());

        let coder = spawn_with_options(
            SpawnRequest {
                root: root.clone(),
                role: "coder".into(),
                task: "Implement feature".into(),
                task_id: None,
                vendor: None,
                model: None,
                mcp_url: Some("http://127.0.0.1:4317/mcp".into()),
            },
            SpawnOptions {
                parent_id: Some(brain.agent.id.clone()),
                registry_path: Some(registry_path.clone()),
                active_vendors: Some(active.clone()),
                terminals: &terminals,
            },
        )
        .unwrap();
        assert_eq!(coder.agent.id, "coder-1");
        assert_eq!(coder.agent.parent_id, "brain-1");
        assert_eq!(coder.agent.depth, 1);
        assert_eq!(coder.agent.branch, "shep/2-implement-feature");

        // Coder role has can_spawn: false, so spawning a child from coder must fail
        let denied = spawn_with_options(
            SpawnRequest {
                root: root.clone(),
                role: "tester".into(),
                task: "Run tests".into(),
                task_id: None,
                vendor: None,
                model: None,
                mcp_url: None,
            },
            SpawnOptions {
                parent_id: Some(coder.agent.id.clone()),
                registry_path: Some(registry_path),
                active_vendors: Some(active),
                terminals: &terminals,
            },
        );
        assert!(denied.is_err());
        assert!(denied.unwrap_err().to_string().contains("can_spawn: false"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
