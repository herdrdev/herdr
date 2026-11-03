//! Terminal runtime port (`trait Terminals`) and adapters (`HerdrInProcess`, `HerdrSocket`, `HcCli`, `InMemoryTerminals`).
//!
//! Shepherd's core workflow, spawner, and MCP server depend only on `Terminals`.

use crate::{config::Config, herdr::{HerdrClient, HerdrError}};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    InProcess,
    Socket,
    Hc,
}

#[derive(Debug)]
pub enum TerminalError {
    Io(std::io::Error),
    Api { code: String, message: String },
    Protocol(String),
    Config(String),
}

impl std::fmt::Display for TerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Api { code, message } => write!(f, "{code}: {message}"),
            Self::Protocol(message) => write!(f, "{message}"),
            Self::Config(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TerminalError {}

impl From<std::io::Error> for TerminalError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<HerdrError> for TerminalError {
    fn from(error: HerdrError) -> Self {
        match error {
            HerdrError::Io(io) => Self::Io(io),
            HerdrError::Api { code, message } => Self::Api { code, message },
            HerdrError::Protocol(message) => Self::Protocol(message),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSpec {
    pub branch: String,
    pub base: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAgentRequest {
    pub root: PathBuf,
    pub role: String,
    pub agent_id: String,
    pub vendor_name: String,
    pub command: String,
    pub herdr_agent: String,
    pub args: Vec<String>,
    pub label: String,
    pub env: HashMap<String, String>,
    pub worktree: Option<WorktreeSpec>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenedAgent {
    pub workspace: String,
    pub tab: String,
    pub pane: String,
    pub worktree: String,
    pub branch: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptOutcome {
    Delivered,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalWaitResult {
    pub ended: bool,
    pub status: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalAgentStatus {
    pub state: String,
    pub status: String,
    pub pane: String,
    pub tab: String,
    pub title: String,
    pub seq: u64,
}

pub trait Terminals: Send + Sync {
    fn open_agent(&self, request: &OpenAgentRequest) -> Result<OpenedAgent, TerminalError>;
    fn prompt(&self, target: &str, text: &str) -> Result<PromptOutcome, TerminalError>;
    fn send_text(&self, target: &str, text: &str) -> Result<(), TerminalError>;
    fn send_keys(&self, target: &str, keys: &[String]) -> Result<(), TerminalError>;
    fn read_screen(&self, target: &str, lines: u32) -> Result<String, TerminalError>;
    fn wait(&self, target: &str, timeout: Duration) -> Result<TerminalWaitResult, TerminalError>;
    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError>;
    fn label(&self, target: &str, labels: &HashMap<String, String>) -> Result<(), TerminalError>;
    fn close(&self, target: &str) -> Result<(), TerminalError>;
    fn attach(&self, target: Option<&str>, takeover: bool) -> Result<(), TerminalError>;
}

/// Select the terminal backend from `SHEPHERD_TERMINALS` (highest priority) or `config.herdr.backend` (default `"socket"`).
pub fn select_backend(config: &Config) -> Result<BackendKind, TerminalError> {
    let env_value = std::env::var("SHEPHERD_TERMINALS").ok();
    select_backend_with_env(config, env_value.as_deref())
}

pub fn select_backend_with_env(config: &Config, env_override: Option<&str>) -> Result<BackendKind, TerminalError> {
    let raw = env_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let cfg = config.herdr.backend.trim();
            if cfg.is_empty() { "socket" } else { cfg }
        });
    match raw {
        "in-process" | "in_process" => Ok(BackendKind::InProcess),
        "socket" => Ok(BackendKind::Socket),
        "hc" => Ok(BackendKind::Hc),
        other => Err(TerminalError::Config(format!(
            "unknown terminal backend {other:?} (expected \"in-process\", \"socket\", or \"hc\")"
        ))),
    }
}

pub type InProcessTerminalsFactory =
    dyn Fn(&Config) -> Result<Box<dyn Terminals>, TerminalError> + Send + Sync;

fn in_process_factory_slot() -> &'static Mutex<Option<Arc<InProcessTerminalsFactory>>> {
    static FACTORY: OnceLock<Mutex<Option<Arc<InProcessTerminalsFactory>>>> = OnceLock::new();
    FACTORY.get_or_init(|| Mutex::new(None))
}

/// Register an in-process `Terminals` factory used when `BackendKind::InProcess` is selected.
pub fn set_in_process_terminals_factory(
    factory: Option<Arc<InProcessTerminalsFactory>>,
) {
    if let Ok(mut guard) = in_process_factory_slot().lock() {
        *guard = factory;
    }
}

/// Instantiate the active `Terminals` adapter according to `SHEPHERD_TERMINALS` and `config.herdr`.
pub fn connect_terminals(config: &Config) -> Result<Box<dyn Terminals>, TerminalError> {
    match select_backend(config)? {
        BackendKind::InProcess => {
            let factory = in_process_factory_slot()
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .ok_or_else(|| {
                    TerminalError::Config(
                        "in-process terminal backend selected, but no in-process runtime is registered"
                            .into(),
                    )
                })?;
            factory(config)
        }
        BackendKind::Socket => Ok(Box::new(HerdrSocket::connect(
            Some(&config.herdr.session),
            config.herdr.autostart,
        )?)),
        BackendKind::Hc => Ok(Box::new(HcCli::new())),
    }
}

/// In-process adapter for Herdr's runtime API (`trait Terminals`), avoiding external NDJSON socket I/O
/// in `shepherd-core` by routing RPC calls through a direct handler (`App` channel or `ApiClient`).
pub struct HerdrInProcess {
    client: HerdrClient,
    session: String,
    attach_handler: Option<Arc<dyn Fn(Option<&str>, bool) -> Result<(), TerminalError> + Send + Sync>>,
}

impl HerdrInProcess {
    pub fn new(
        session: impl Into<String>,
        handler: impl Fn(&str, Value) -> Result<Value, HerdrError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            client: HerdrClient::in_process(handler),
            session: session.into(),
            attach_handler: None,
        }
    }

    pub fn with_client(client: HerdrClient, session: impl Into<String>) -> Self {
        Self {
            client,
            session: session.into(),
            attach_handler: None,
        }
    }

    pub fn with_attach_handler(
        mut self,
        handler: impl Fn(Option<&str>, bool) -> Result<(), TerminalError> + Send + Sync + 'static,
    ) -> Self {
        self.attach_handler = Some(Arc::new(handler));
        self
    }

    pub fn client(&self) -> &HerdrClient {
        &self.client
    }

    pub fn session(&self) -> &str {
        &self.session
    }
}

/// Adapter for Herdr 0.9.5 local NDJSON Unix socket API (`herdr`).
pub struct HerdrSocket {
    client: HerdrClient,
    session: String,
}

impl HerdrSocket {
    pub fn new(client: HerdrClient, session: impl Into<String>) -> Self {
        Self { client, session: session.into() }
    }

    pub fn connect(session: Option<&str>, autostart: bool) -> Result<Self, TerminalError> {
        let client = HerdrClient::ensure_session(session, autostart)?;
        Ok(Self {
            client,
            session: session.unwrap_or("shepherd").to_owned(),
        })
    }

    pub fn client(&self) -> &HerdrClient {
        &self.client
    }
}

pub fn herdr_attach_args(session: &str, target: Option<&str>, takeover: bool) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(target) = target {
        if !session.is_empty() && session != "-" {
            args.extend(["--session".into(), session.into()]);
        }
        args.extend(["agent".into(), "attach".into(), target.into()]);
        if takeover {
            args.push("--takeover".into());
        }
    } else if !session.is_empty() && session != "-" {
        args.extend(["session".into(), "attach".into(), session.into()]);
    }
    args
}

impl Terminals for HerdrInProcess {
    fn open_agent(&self, request: &OpenAgentRequest) -> Result<OpenedAgent, TerminalError> {
        herdr_open_agent(&self.client, request)
    }

    fn prompt(&self, target: &str, text: &str) -> Result<PromptOutcome, TerminalError> {
        deliver_prompt(&self.client, target, text)
    }

    fn send_text(&self, target: &str, text: &str) -> Result<(), TerminalError> {
        self.client.send_input(target, text)?;
        Ok(())
    }

    fn send_keys(&self, target: &str, keys: &[String]) -> Result<(), TerminalError> {
        self.client.send_keys(target, keys)?;
        Ok(())
    }

    fn read_screen(&self, target: &str, lines: u32) -> Result<String, TerminalError> {
        herdr_read_screen(&self.client, target, lines)
    }

    fn wait(&self, target: &str, timeout: Duration) -> Result<TerminalWaitResult, TerminalError> {
        herdr_wait(&self.client, target, timeout)
    }

    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError> {
        herdr_agent_status(&self.client, target)
    }

    fn label(&self, target: &str, labels: &HashMap<String, String>) -> Result<(), TerminalError> {
        herdr_label(&self.client, target, labels)
    }

    fn close(&self, target: &str) -> Result<(), TerminalError> {
        self.client.close_tab(target)?;
        Ok(())
    }

    fn attach(&self, target: Option<&str>, takeover: bool) -> Result<(), TerminalError> {
        if let Some(handler) = &self.attach_handler {
            return handler(target, takeover);
        }
        herdr_attach_process(&self.session, target, takeover)
    }
}

impl Terminals for HerdrSocket {
    fn open_agent(&self, request: &OpenAgentRequest) -> Result<OpenedAgent, TerminalError> {
        herdr_open_agent(&self.client, request)
    }

    fn prompt(&self, target: &str, text: &str) -> Result<PromptOutcome, TerminalError> {
        deliver_prompt(&self.client, target, text)
    }

    fn send_text(&self, target: &str, text: &str) -> Result<(), TerminalError> {
        self.client.send_input(target, text)?;
        Ok(())
    }

    fn send_keys(&self, target: &str, keys: &[String]) -> Result<(), TerminalError> {
        self.client.send_keys(target, keys)?;
        Ok(())
    }

    fn read_screen(&self, target: &str, lines: u32) -> Result<String, TerminalError> {
        herdr_read_screen(&self.client, target, lines)
    }

    fn wait(&self, target: &str, timeout: Duration) -> Result<TerminalWaitResult, TerminalError> {
        herdr_wait(&self.client, target, timeout)
    }

    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError> {
        herdr_agent_status(&self.client, target)
    }

    fn label(&self, target: &str, labels: &HashMap<String, String>) -> Result<(), TerminalError> {
        herdr_label(&self.client, target, labels)
    }

    fn close(&self, target: &str) -> Result<(), TerminalError> {
        self.client.close_tab(target)?;
        Ok(())
    }

    fn attach(&self, target: Option<&str>, takeover: bool) -> Result<(), TerminalError> {
        herdr_attach_process(&self.session, target, takeover)
    }
}

fn herdr_open_agent(
    client: &HerdrClient,
    request: &OpenAgentRequest,
) -> Result<OpenedAgent, TerminalError> {
    let mut worktree_path = String::new();
    let mut branch = String::new();
    let workspace_id;
    let initial_tab_id;
    if let Some(wt) = &request.worktree {
        let created = client.create_worktree(
            &request.root.display().to_string(),
            &wt.branch,
            &wt.base,
            &request.label,
        )?;
        workspace_id = nested_string(&created, &["workspace", "workspace_id"])?;
        initial_tab_id = nested_string(&created, &["tab", "tab_id"])?;
        worktree_path = nested_string(&created, &["worktree", "path"])?;
        branch = nested_string(&created, &["worktree", "branch"]).unwrap_or_else(|_| wt.branch.clone());
    } else {
        workspace_id = match find_workspace(&client.snapshot()?, &request.root) {
            Some(id) => id,
            None => {
                let label = request.root.file_name().unwrap_or_default().to_string_lossy();
                let created = client.create_workspace(&request.root.display().to_string(), &label)?;
                nested_string(&created, &["workspace", "workspace_id"])?
            }
        };
        initial_tab_id = String::new();
    }
    let cwd = if worktree_path.is_empty() {
        request.root.display().to_string()
    } else {
        worktree_path.clone()
    };
    let created_tab = client.create_tab(&workspace_id, &cwd, &request.label, &request.env)?;
    let tab_id = nested_string(&created_tab, &["tab", "tab_id"])?;
    let pane_id = nested_string(&created_tab, &["root_pane", "pane_id"])?;
    if !initial_tab_id.is_empty() {
        let _ = client.close_tab(&initial_tab_id);
    }
    let _ = client.rename_pane(&pane_id, &request.label);
    let _ = client.report_metadata(
        &pane_id,
        &request.role,
        &request.agent_id,
        &request.vendor_name,
        &request.label,
    );

    if request.herdr_agent.is_empty() {
        let command = crate::spawn::shell_join(
            std::iter::once(request.command.as_str()).chain(request.args.iter().map(String::as_str)),
        );
        client.send_input(&pane_id, &command)?;
    } else {
        start_herdr_agent(client, &pane_id, &request.agent_id, &request.herdr_agent, &request.args)?;
    }

    Ok(OpenedAgent {
        workspace: workspace_id,
        tab: tab_id,
        pane: pane_id,
        worktree: worktree_path,
        branch,
    })
}

fn herdr_read_screen(client: &HerdrClient, target: &str, lines: u32) -> Result<String, TerminalError> {
    let result = client.read_pane(target, lines)?;
    Ok(result
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

fn herdr_wait(
    client: &HerdrClient,
    target: &str,
    timeout: Duration,
) -> Result<TerminalWaitResult, TerminalError> {
    let result = client.wait_agent(target, &["idle", "done", "blocked"], timeout)?;
    let event = result.get("event").unwrap_or(&result);
    let name = event
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .to_owned();
    let status = event
        .get("data")
        .and_then(|d| d.get("agent_status"))
        .or_else(|| result.get("agent").and_then(|a| a.get("agent_status")))
        .or_else(|| result.get("agent_status"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(TerminalWaitResult {
        ended: true,
        status,
        reason: name,
    })
}

fn herdr_agent_status(client: &HerdrClient, target: &str) -> Result<TerminalAgentStatus, TerminalError> {
    let agent = client.get_agent(target)?;
    Ok(TerminalAgentStatus {
        state: "live".into(),
        status: agent
            .get("agent_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        pane: agent
            .get("pane_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        tab: agent
            .get("tab_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        title: agent
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        seq: agent
            .get("state_change_seq")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn herdr_label(
    client: &HerdrClient,
    target: &str,
    labels: &HashMap<String, String>,
) -> Result<(), TerminalError> {
    let title = labels
        .get("title")
        .or_else(|| labels.get("label"))
        .map(String::as_str)
        .unwrap_or("");
    if !title.is_empty() {
        let _ = client.rename_pane(target, title);
    }
    client.call(
        "pane.report_metadata",
        serde_json::json!({
            "pane_id": target,
            "source": "shepherd",
            "title": title,
            "tokens": labels,
        }),
    )?;
    Ok(())
}

fn herdr_attach_process(
    session: &str,
    target: Option<&str>,
    takeover: bool,
) -> Result<(), TerminalError> {
    let args = herdr_attach_args(session, target, takeover);
    let status = Command::new("herdr").args(args).status()?;
    if !status.success() {
        return Err(TerminalError::Protocol(format!("herdr attach exited with {status}")));
    }
    Ok(())
}

fn start_herdr_agent(
    client: &HerdrClient,
    pane: &str,
    name: &str,
    kind: &str,
    args: &[String],
) -> Result<(), TerminalError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.start_agent(pane, name, kind, args) {
            Ok(_) => return Ok(()),
            Err(HerdrError::Api { code, .. }) if code == "agent_pane_busy" => {
                if Instant::now() >= deadline {
                    return Err(TerminalError::Protocol(
                        "Herdr pane did not become ready for an agent within 30 seconds".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn deliver_prompt(client: &HerdrClient, target: &str, prompt: &str) -> Result<PromptOutcome, TerminalError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.prompt_agent(target, prompt) {
            Ok(_) => return Ok(PromptOutcome::Delivered),
            Err(HerdrError::Api { code, .. }) if code == "agent_blocked" => {
                return Ok(PromptOutcome::Blocked);
            }
            Err(HerdrError::Api { code, .. }) if code == "agent_not_ready" => {
                if Instant::now() >= deadline {
                    return Err(TerminalError::Protocol(
                        "agent did not become ready within 30 seconds".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn find_workspace(snapshot: &Value, root: &Path) -> Option<String> {
    let workspaces = snapshot.get("workspaces")?.as_array()?;
    workspaces.iter().find_map(|workspace| {
        let worktree = workspace.get("worktree")?;
        let checkout = worktree.get("checkout_path")?.as_str()?;
        (Path::new(checkout) == root)
            .then(|| workspace.get("workspace_id")?.as_str().map(str::to_owned))
            .flatten()
    })
}

fn nested_string(value: &Value, path: &[&str]) -> Result<String, TerminalError> {
    let mut current = value;
    for key in path {
        current = current.get(*key).ok_or_else(|| {
            TerminalError::Protocol(format!("Herdr response is missing {key:?}"))
        })?;
    }
    current.as_str().map(str::to_owned).ok_or_else(|| {
        TerminalError::Protocol(format!("Herdr response field {} is not a string", path.join(".")))
    })
}

/// Adapter for `herdr-core` CLI (`hc`).
pub struct HcCli {
    binary: PathBuf,
}

impl Default for HcCli {
    fn default() -> Self {
        Self::new()
    }
}

impl HcCli {
    pub fn new() -> Self {
        Self { binary: PathBuf::from("hc") }
    }

    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self { binary: binary.into() }
    }

    fn run_json(&self, args: &[String], env: Option<&HashMap<String, String>>) -> Result<Value, TerminalError> {
        let output = self.run_raw(args, env)?;
        if !output.status.success() {
            return Err(parse_hc_error(&output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(trimmed)
            .map_err(|error| TerminalError::Protocol(format!("hc invalid JSON ({error}): {trimmed}")))
    }

    fn run_raw(
        &self,
        args: &[String],
        env: Option<&HashMap<String, String>>,
    ) -> Result<std::process::Output, TerminalError> {
        let mut command = Command::new(&self.binary);
        command.args(args);
        if let Some(env) = env {
            command.envs(env);
        }
        command.output().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TerminalError::Protocol(format!("{} not found in PATH", self.binary.display()))
            } else {
                TerminalError::Io(error)
            }
        })
    }
}

pub fn hc_attach_args(target: Option<&str>, takeover: bool) -> Vec<String> {
    let _ = takeover;
    let mut args = vec!["attach".to_owned()];
    if let Some(target) = target.filter(|t| !t.is_empty()) {
        args.push(target.to_owned());
    }
    args
}

fn create_git_worktree(root: &Path, spec: &WorktreeSpec) -> Result<PathBuf, TerminalError> {
    let repo_name = root.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    let worktrees_root = parent.join(format!("{repo_name}.worktrees"));
    let path = worktrees_root.join(spec.branch.replace('/', "-"));
    if root.join(".git").exists() {
        std::fs::create_dir_all(&worktrees_root)?;
        let mut args = vec![
            "worktree".to_owned(),
            "add".to_owned(),
            "-b".to_owned(),
            spec.branch.clone(),
            path.display().to_string(),
        ];
        if !spec.base.trim().is_empty() {
            args.push(spec.base.clone());
        }
        let output = Command::new("git").args(&args).current_dir(root).output()?;
        if !output.status.success() {
            return Err(TerminalError::Protocol(format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    } else {
        std::fs::create_dir_all(&path)?;
    }
    Ok(path)
}

fn parse_hc_error(output: &std::process::Output) -> TerminalError {
    for stream in [&output.stderr, &output.stdout] {
        let text = String::from_utf8_lossy(stream);
        let trimmed = text.trim();
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(err) = value.get("error") {
                let code = err
                    .get("kind")
                    .or_else(|| err.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("hc_error")
                    .to_owned();
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or(trimmed)
                    .to_owned();
                return TerminalError::Api { code, message };
            }
        }
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    if output.status.code() == Some(9) {
        return TerminalError::Api {
            code: "blocked".into(),
            message: if detail.is_empty() { "terminal is blocked".into() } else { detail },
        };
    }
    TerminalError::Protocol(format!("hc exited with {}: {detail}", output.status))
}

impl Terminals for HcCli {
    fn open_agent(&self, request: &OpenAgentRequest) -> Result<OpenedAgent, TerminalError> {
        let (worktree_path, branch) = match &request.worktree {
            Some(spec) => {
                let path = create_git_worktree(&request.root, spec)?;
                (path.display().to_string(), spec.branch.clone())
            }
            None => (String::new(), String::new()),
        };
        let cwd = if worktree_path.is_empty() {
            request.root.display().to_string()
        } else {
            worktree_path.clone()
        };
        let mut args = vec![
            "run".to_owned(),
            "--bg".to_owned(),
            "--json".to_owned(),
            "--outlive-parent".to_owned(),
            "--no-notify".to_owned(),
            "--name".to_owned(),
            request.agent_id.clone(),
            "--cwd".to_owned(),
            cwd,
            "--label".to_owned(),
            format!("role={}", request.role),
            "--label".to_owned(),
            format!("agent={}", request.agent_id),
            "--label".to_owned(),
            format!("vendor={}", request.vendor_name),
        ];
        if !request.label.is_empty() {
            args.extend(["--label".to_owned(), format!("label={}", request.label)]);
        }
        args.push("--".to_owned());
        args.push(request.command.clone());
        args.extend(request.args.clone());

        let result = self.run_json(&args, Some(&request.env))?;
        let address = result
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or(&request.agent_id)
            .to_owned();
        Ok(OpenedAgent {
            workspace: String::new(),
            tab: address.clone(),
            pane: address,
            worktree: worktree_path,
            branch,
        })
    }

    fn prompt(&self, target: &str, text: &str) -> Result<PromptOutcome, TerminalError> {
        let args = vec![
            "send".to_owned(),
            "--json".to_owned(),
            target.to_owned(),
            text.to_owned(),
        ];
        let output = self.run_raw(&args, None)?;
        if output.status.success() {
            if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
                if value.get("status").and_then(Value::as_str) == Some("blocked")
                    && value.get("started").and_then(Value::as_bool) == Some(false)
                {
                    return Ok(PromptOutcome::Blocked);
                }
            }
            return Ok(PromptOutcome::Delivered);
        }
        match parse_hc_error(&output) {
            TerminalError::Api { code, .. } if code == "blocked" || code == "agent_blocked" => {
                Ok(PromptOutcome::Blocked)
            }
            error => Err(error),
        }
    }

    fn send_text(&self, target: &str, text: &str) -> Result<(), TerminalError> {
        let args = vec![
            "send".to_owned(),
            "--json".to_owned(),
            target.to_owned(),
            text.to_owned(),
        ];
        let _ = self.run_json(&args, None)?;
        Ok(())
    }

    fn send_keys(&self, target: &str, keys: &[String]) -> Result<(), TerminalError> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut args = vec!["keys".to_owned(), "--json".to_owned(), target.to_owned()];
        args.extend(keys.iter().cloned());
        let _ = self.run_json(&args, None)?;
        Ok(())
    }

    fn read_screen(&self, target: &str, lines: u32) -> Result<String, TerminalError> {
        let args = vec![
            "read".to_owned(),
            "--json".to_owned(),
            "--lines".to_owned(),
            lines.to_string(),
            target.to_owned(),
        ];
        let output = self.run_raw(&args, None)?;
        if !output.status.success() {
            return Err(parse_hc_error(&output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(value) = serde_json::from_str::<Value>(stdout.trim()) {
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                return Ok(text.to_owned());
            }
        }
        Ok(stdout.into_owned())
    }

    fn wait(&self, target: &str, timeout: Duration) -> Result<TerminalWaitResult, TerminalError> {
        let secs = timeout.as_secs().max(1);
        let args = vec![
            "wait".to_owned(),
            "--json".to_owned(),
            "--timeout".to_owned(),
            secs.to_string(),
            target.to_owned(),
        ];
        let value = self.run_json(&args, None)?;
        let ended = value.get("ended").and_then(Value::as_bool).unwrap_or(true);
        let status = value.get("status").and_then(Value::as_str).map(str::to_owned);
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .to_owned();
        Ok(TerminalWaitResult { ended, status, reason })
    }

    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError> {
        let args = vec!["ls".to_owned(), "--json".to_owned()];
        let value = self.run_json(&args, None)?;
        let list = value
            .get("terminals")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
            .ok_or_else(|| TerminalError::Protocol("hc ls --json did not return a terminals array".into()))?;
        let item = list
            .iter()
            .find(|entry| {
                entry.get("address").and_then(Value::as_str) == Some(target)
                    || entry.get("name").and_then(Value::as_str) == Some(target)
                    || entry
                        .get("labels")
                        .and_then(|l| l.get("agent"))
                        .and_then(Value::as_str)
                        == Some(target)
            })
            .ok_or_else(|| TerminalError::Api {
                code: "not_found".into(),
                message: format!("terminal {target:?} not found"),
            })?;
        let address = item
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or(target)
            .to_owned();
        let state = item
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("live")
            .to_owned();
        let status = item
            .get("status")
            .and_then(|s| s.get("state").and_then(Value::as_str).or_else(|| s.as_str()))
            .or_else(|| {
                if state == "finished" {
                    item.get("reason").and_then(Value::as_str).or(Some("finished"))
                } else {
                    None
                }
            })
            .unwrap_or("unknown")
            .to_owned();
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| item.get("name").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned();
        let seq = item.get("turn").and_then(Value::as_u64).unwrap_or(0);
        Ok(TerminalAgentStatus {
            state,
            status,
            pane: address.clone(),
            tab: address,
            title,
            seq,
        })
    }

    fn label(&self, target: &str, labels: &HashMap<String, String>) -> Result<(), TerminalError> {
        if labels.is_empty() {
            return Ok(());
        }
        let mut pairs: Vec<_> = labels.iter().collect();
        pairs.sort_by_key(|(k, _)| *k);
        let mut args = vec!["label".to_owned(), "--json".to_owned(), target.to_owned()];
        for (k, v) in pairs {
            args.push(format!("{k}={v}"));
        }
        let _ = self.run_json(&args, None)?;
        Ok(())
    }

    fn close(&self, target: &str) -> Result<(), TerminalError> {
        let args = vec!["kill".to_owned(), "--json".to_owned(), target.to_owned()];
        let _ = self.run_json(&args, None)?;
        Ok(())
    }

    fn attach(&self, target: Option<&str>, takeover: bool) -> Result<(), TerminalError> {
        let args = hc_attach_args(target, takeover);
        let status = Command::new(&self.binary).args(args).status()?;
        if !status.success() {
            return Err(TerminalError::Protocol(format!("hc attach exited with {status}")));
        }
        Ok(())
    }
}

/// In-memory fake implementation of `Terminals` for unit and integration tests.
#[derive(Clone, Default)]
pub struct InMemoryTerminals {
    inner: Arc<Mutex<InMemoryState>>,
}

#[derive(Default)]
struct InMemoryState {
    next_id: u64,
    pub opened: Vec<OpenAgentRequest>,
    pub prompts: Vec<(String, String)>,
    pub sent_text: Vec<(String, String)>,
    pub sent_keys: Vec<(String, Vec<String>)>,
    pub labels: HashMap<String, HashMap<String, String>>,
    pub screens: HashMap<String, String>,
    pub statuses: HashMap<String, TerminalAgentStatus>,
    pub prompt_outcomes: HashMap<String, PromptOutcome>,
    pub closed: Vec<String>,
    pub attached: Vec<(Option<String>, bool)>,
    pub create_real_worktrees: bool,
    pub on_prompt: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
}

impl InMemoryTerminals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_real_worktrees() -> Self {
        let fake = Self::new();
        fake.inner.lock().unwrap().create_real_worktrees = true;
        fake
    }

    pub fn on_prompt(&self, callback: impl Fn(&str, &str) + Send + Sync + 'static) {
        self.inner.lock().unwrap().on_prompt = Some(Arc::new(callback));
    }

    pub fn set_prompt_outcome(&self, target: impl Into<String>, outcome: PromptOutcome) {
        self.inner.lock().unwrap().prompt_outcomes.insert(target.into(), outcome);
    }

    pub fn set_screen(&self, target: impl Into<String>, screen: impl Into<String>) {
        self.inner.lock().unwrap().screens.insert(target.into(), screen.into());
    }

    pub fn set_status(&self, target: impl Into<String>, status: TerminalAgentStatus) {
        self.inner.lock().unwrap().statuses.insert(target.into(), status);
    }

    pub fn opened(&self) -> Vec<OpenAgentRequest> {
        self.inner.lock().unwrap().opened.clone()
    }

    pub fn prompts(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().prompts.clone()
    }

    pub fn sent_text(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().sent_text.clone()
    }

    pub fn sent_keys(&self) -> Vec<(String, Vec<String>)> {
        self.inner.lock().unwrap().sent_keys.clone()
    }

    pub fn closed(&self) -> Vec<String> {
        self.inner.lock().unwrap().closed.clone()
    }

    pub fn labels_for(&self, target: &str) -> HashMap<String, String> {
        self.inner
            .lock()
            .unwrap()
            .labels
            .get(target)
            .cloned()
            .unwrap_or_default()
    }
}

impl Terminals for InMemoryTerminals {
    fn open_agent(&self, request: &OpenAgentRequest) -> Result<OpenedAgent, TerminalError> {
        let mut state = self.inner.lock().unwrap();
        state.next_id += 1;
        let n = state.next_id;
        let workspace = format!("ws-{n}");
        let tab = format!("tab-{n}");
        let pane = format!("pane-{n}");
        let (worktree, branch) = match &request.worktree {
            Some(spec) => {
                let path = if state.create_real_worktrees {
                    create_git_worktree(&request.root, spec)?
                } else {
                    request.root.join(".worktrees").join(spec.branch.replace('/', "-"))
                };
                (path.display().to_string(), spec.branch.clone())
            }
            None => (String::new(), String::new()),
        };
        let status = TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: pane.clone(),
            tab: tab.clone(),
            title: request.label.clone(),
            seq: 1,
        };
        state.statuses.insert(request.agent_id.clone(), status.clone());
        state.statuses.insert(pane.clone(), status);
        state.opened.push(request.clone());
        Ok(OpenedAgent { workspace, tab, pane, worktree, branch })
    }

    fn prompt(&self, target: &str, text: &str) -> Result<PromptOutcome, TerminalError> {
        let callback = {
            let mut state = self.inner.lock().unwrap();
            state.prompts.push((target.to_owned(), text.to_owned()));
            if let Some(outcome) = state.prompt_outcomes.get(target).copied() {
                if outcome == PromptOutcome::Blocked {
                    return Ok(PromptOutcome::Blocked);
                }
            }
            if let Some(status) = state.statuses.get_mut(target) {
                status.status = "working".into();
                status.seq += 1;
            }
            state.on_prompt.clone()
        };
        if let Some(cb) = callback {
            cb(target, text);
        }
        Ok(PromptOutcome::Delivered)
    }

    fn send_text(&self, target: &str, text: &str) -> Result<(), TerminalError> {
        self.inner.lock().unwrap().sent_text.push((target.to_owned(), text.to_owned()));
        Ok(())
    }

    fn send_keys(&self, target: &str, keys: &[String]) -> Result<(), TerminalError> {
        self.inner.lock().unwrap().sent_keys.push((target.to_owned(), keys.to_vec()));
        Ok(())
    }

    fn read_screen(&self, target: &str, lines: u32) -> Result<String, TerminalError> {
        let state = self.inner.lock().unwrap();
        let text = state
            .screens
            .get(target)
            .ok_or_else(|| TerminalError::Api {
                code: "not_found".into(),
                message: format!("no screen recorded for {target:?}"),
            })?;
        if lines == 0 {
            return Ok(text.clone());
        }
        let all_lines: Vec<&str> = text.lines().collect();
        let take = lines as usize;
        if all_lines.len() <= take {
            Ok(text.clone())
        } else {
            Ok(all_lines[all_lines.len() - take..].join("\n"))
        }
    }

    fn wait(&self, target: &str, _timeout: Duration) -> Result<TerminalWaitResult, TerminalError> {
        let state = self.inner.lock().unwrap();
        let status = state
            .statuses
            .get(target)
            .map(|s| s.status.clone())
            .or_else(|| Some("idle".into()));
        Ok(TerminalWaitResult {
            ended: true,
            status,
            reason: "status".into(),
        })
    }

    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError> {
        let state = self.inner.lock().unwrap();
        state
            .statuses
            .get(target)
            .cloned()
            .ok_or_else(|| TerminalError::Api {
                code: "not_found".into(),
                message: format!("unknown agent or terminal {target:?}"),
            })
    }

    fn label(&self, target: &str, labels: &HashMap<String, String>) -> Result<(), TerminalError> {
        let mut state = self.inner.lock().unwrap();
        let entry = state.labels.entry(target.to_owned()).or_default();
        for (k, v) in labels {
            if v.is_empty() {
                entry.remove(k);
            } else {
                entry.insert(k.clone(), v.clone());
            }
        }
        Ok(())
    }

    fn close(&self, target: &str) -> Result<(), TerminalError> {
        let mut state = self.inner.lock().unwrap();
        state.closed.push(target.to_owned());
        if let Some(status) = state.statuses.get_mut(target) {
            status.state = "finished".into();
            status.status = "killed".into();
        }
        Ok(())
    }

    fn attach(&self, target: Option<&str>, takeover: bool) -> Result<(), TerminalError> {
        self.inner
            .lock()
            .unwrap()
            .attached
            .push((target.map(str::to_owned), takeover));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_selection_prioritizes_env_over_config_and_defaults_to_socket() {
        let mut config = Config::default();
        assert_eq!(select_backend_with_env(&config, None).unwrap(), BackendKind::Socket);
        config.herdr.backend = "hc".into();
        assert_eq!(select_backend_with_env(&config, None).unwrap(), BackendKind::Hc);
        config.herdr.backend = "in-process".into();
        assert_eq!(select_backend_with_env(&config, None).unwrap(), BackendKind::InProcess);
        assert_eq!(
            select_backend_with_env(&config, Some("in_process")).unwrap(),
            BackendKind::InProcess
        );
        assert_eq!(
            select_backend_with_env(&config, Some("socket")).unwrap(),
            BackendKind::Socket
        );
        assert_eq!(
            select_backend_with_env(&config, Some("hc")).unwrap(),
            BackendKind::Hc
        );
        assert!(select_backend_with_env(&config, Some("invalid")).is_err());
    }

    #[test]
    fn herdr_in_process_executes_every_terminals_operation_without_socket() {
        let calls: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let attached: Arc<Mutex<Vec<(Option<String>, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_ref = Arc::clone(&calls);
        let attached_ref = Arc::clone(&attached);

        let adapter = HerdrInProcess::new("shepherd", move |method, params| {
            calls_ref.lock().unwrap().push((method.to_owned(), params.clone()));
            match method {
                "worktree.create" => Ok(serde_json::json!({
                    "workspace": { "workspace_id": "w1" },
                    "tab": { "tab_id": "w1:t0" },
                    "worktree": {
                        "path": "/repo/.worktrees/shep-1",
                        "branch": params["branch"],
                    }
                })),
                "session.snapshot" => Ok(serde_json::json!({
                    "snapshot": {
                        "workspaces": [
                            {
                                "workspace_id": "w-existing",
                                "worktree": { "checkout_path": "/repo" }
                            }
                        ]
                    }
                })),
                "tab.create" => Ok(serde_json::json!({
                    "tab": { "tab_id": "w1:t1" },
                    "root_pane": { "pane_id": "w1:p1" },
                })),
                "tab.close" | "pane.rename" | "pane.report_metadata" | "agent.start"
                | "pane.send_input" => Ok(serde_json::json!({"ok": true})),
                "agent.prompt" => {
                    if params["text"] == "BLOCK_ME" {
                        Err(HerdrError::Api {
                            code: "agent_blocked".into(),
                            message: "agent waiting for input".into(),
                        })
                    } else {
                        Ok(serde_json::json!({"delivered": true}))
                    }
                }
                "pane.read" => Ok(serde_json::json!({
                    "read": { "text": "in-process screen line 1\nin-process screen line 2" }
                })),
                "agent.wait" => Ok(serde_json::json!({
                    "type": "agent_info",
                    "agent": {
                        "name": "coder-1",
                        "agent_status": "idle",
                        "pane_id": "w1:p1",
                        "tab_id": "w1:t1",
                        "state_change_seq": 5
                    }
                })),
                "agent.get" => Ok(serde_json::json!({
                    "agent": {
                        "name": "coder-1",
                        "agent_status": "working",
                        "pane_id": "w1:p1",
                        "tab_id": "w1:t1",
                        "state_change_seq": 7
                    }
                })),
                other => Err(HerdrError::Api {
                    code: "unexpected".into(),
                    message: format!("unexpected method {other}"),
                }),
            }
        })
        .with_attach_handler(move |target, takeover| {
            attached_ref
                .lock()
                .unwrap()
                .push((target.map(str::to_owned), takeover));
            Ok(())
        });

        assert!(adapter.client().is_in_process());
        assert_eq!(adapter.session(), "shepherd");

        let opened = adapter
            .open_agent(&OpenAgentRequest {
                root: PathBuf::from("/repo"),
                role: "coder".into(),
                agent_id: "coder-1".into(),
                vendor_name: "codex".into(),
                command: "codex".into(),
                herdr_agent: "codex".into(),
                args: vec!["-m".into(), "gpt-5".into()],
                label: "coder·codex·task".into(),
                env: HashMap::from([("SHEP_AGENT_ID".into(), "coder-1".into())]),
                worktree: Some(WorktreeSpec {
                    branch: "shep/1-task".into(),
                    base: "main".into(),
                }),
            })
            .unwrap();
        assert_eq!(opened.workspace, "w1");
        assert_eq!(opened.tab, "w1:t1");
        assert_eq!(opened.pane, "w1:p1");
        assert_eq!(opened.worktree, "/repo/.worktrees/shep-1");
        assert_eq!(opened.branch, "shep/1-task");

        assert_eq!(
            adapter.prompt("coder-1", "implement F1").unwrap(),
            PromptOutcome::Delivered
        );
        assert_eq!(
            adapter.prompt("coder-1", "BLOCK_ME").unwrap(),
            PromptOutcome::Blocked
        );
        adapter.send_text("w1:p1", "cargo test").unwrap();
        adapter
            .send_keys("w1:p1", &["Enter".into(), "Escape".into()])
            .unwrap();
        assert_eq!(
            adapter.read_screen("w1:p1", 40).unwrap(),
            "in-process screen line 1\nin-process screen line 2"
        );
        let waited = adapter.wait("coder-1", Duration::from_secs(5)).unwrap();
        assert!(waited.ended);
        assert_eq!(waited.status.as_deref(), Some("idle"));

        let status = adapter.agent_status("coder-1").unwrap();
        assert_eq!(status.state, "live");
        assert_eq!(status.status, "working");
        assert_eq!(status.pane, "w1:p1");
        assert_eq!(status.tab, "w1:t1");
        assert_eq!(status.title, "coder-1");
        assert_eq!(status.seq, 7);

        adapter
            .label(
                "w1:p1",
                &HashMap::from([
                    ("role".into(), "coder".into()),
                    ("title".into(), "coder·codex".into()),
                ]),
            )
            .unwrap();
        adapter.attach(Some("coder-1"), true).unwrap();
        adapter.close("w1:t1").unwrap();

        let recorded = calls.lock().unwrap();
        let methods: Vec<&str> = recorded.iter().map(|(m, _)| m.as_str()).collect();
        assert!(methods.contains(&"worktree.create"));
        assert!(methods.contains(&"tab.create"));
        assert!(methods.contains(&"agent.start"));
        assert!(methods.contains(&"agent.prompt"));
        assert!(methods.contains(&"pane.send_input"));
        assert!(methods.contains(&"pane.read"));
        assert!(methods.contains(&"agent.wait"));
        assert!(methods.contains(&"agent.get"));
        assert!(methods.contains(&"pane.report_metadata"));
        assert!(methods.contains(&"tab.close"));
        assert_eq!(
            *attached.lock().unwrap(),
            vec![(Some("coder-1".to_owned()), true)]
        );
    }

    #[test]
    fn in_memory_terminals_support_full_lifecycle() {
        let fake = InMemoryTerminals::new();
        let opened = fake
            .open_agent(&OpenAgentRequest {
                root: PathBuf::from("/repo"),
                role: "coder".into(),
                agent_id: "coder-1".into(),
                vendor_name: "codex".into(),
                command: "codex".into(),
                herdr_agent: "codex".into(),
                args: vec!["-m".into(), "gpt-5".into()],
                label: "coder·codex·task".into(),
                env: HashMap::from([("SHEP_AGENT_ID".into(), "coder-1".into())]),
                worktree: Some(WorktreeSpec {
                    branch: "shep/1-task".into(),
                    base: "main".into(),
                }),
            })
            .unwrap();
        assert_eq!(opened.pane, "pane-1");
        assert_eq!(opened.branch, "shep/1-task");
        assert_eq!(
            fake.prompt("coder-1", "implement feature").unwrap(),
            PromptOutcome::Delivered
        );
        fake.set_prompt_outcome("coder-1", PromptOutcome::Blocked);
        assert_eq!(
            fake.prompt("coder-1", "blocked prompt").unwrap(),
            PromptOutcome::Blocked
        );
        fake.send_text(&opened.pane, "echo hi").unwrap();
        fake.send_keys(&opened.pane, &["Enter".into(), "Escape".into()]).unwrap();
        fake.set_screen(&opened.pane, "line 1\nline 2\n");
        assert_eq!(fake.read_screen(&opened.pane, 20).unwrap(), "line 1\nline 2\n");
        let waited = fake.wait("coder-1", Duration::from_secs(5)).unwrap();
        assert!(waited.ended);
        let status = fake.agent_status("coder-1").unwrap();
        assert_eq!(status.state, "live");
        fake.label(&opened.pane, &HashMap::from([("role".into(), "coder".into())]))
            .unwrap();
        assert_eq!(fake.labels_for(&opened.pane).get("role").map(String::as_str), Some("coder"));
        fake.attach(Some("coder-1"), true).unwrap();
        fake.close(&opened.tab).unwrap();
        assert_eq!(fake.closed(), vec![opened.tab]);
    }

    #[cfg(unix)]
    #[test]
    fn hc_cli_executes_fake_hc_script_for_every_trait_operation() {
        use std::os::unix::fs::PermissionsExt;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("shepherd-fake-hc-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        let log_path = temp.join("hc.log");
        let script_path = temp.join("hc");
        let script = format!(
            r#"#!/bin/sh
LOG="{}"
cmd="$1"
echo "$* | SHEP_AGENT_ID=${{SHEP_AGENT_ID:-}}" >> "$LOG"
case "$cmd" in
  run)
    echo '{{"address":"t7","name":"coder-1","cwd":"/tmp","state":"live","labels":{{}},"argv":["codex"],"created_ms":1,"screen":"exact","cols":80,"rows":24}}'
    ;;
  send)
    for last; do :; done
    if [ "$last" = "BLOCK_ME" ]; then
      echo '{{"error":{{"kind":"blocked","message":"waiting for permission"}}}}' >&2
      exit 9
    fi
    echo '{{"turn":2,"started":true,"status":"working"}}'
    ;;
  keys)
    echo '{{"turn":2,"status":"idle"}}'
    ;;
  read)
    printf '%s\n' '{{"text":"screen line 1\nscreen line 2","complete":true,"how":"screen","why":null}}'
    ;;
  wait)
    echo '{{"terminal":"t7","ended":true,"reason":"status","status":"done","turn":3,"children":[]}}'
    ;;
  ls)
    echo '{{"terminals":[{{"address":"t7","name":"coder-1","cwd":"/tmp","state":"live","status":{{"state":"working","app":"codex"}},"title":"coder·codex","turn":4,"labels":{{"agent":"coder-1","role":"coder"}},"argv":["codex"],"created_ms":1,"screen":"exact","cols":80,"rows":24}}],"hidden_finished":0,"machines":[]}}'
    ;;
  label)
    echo '{{"address":"t7","labels":{{"role":"coder"}},"argv":["codex"],"cwd":"/tmp","state":"live","created_ms":1,"screen":"exact","cols":80,"rows":24}}'
    ;;
  kill)
    echo '{{"address":"t7","labels":{{}},"argv":["codex"],"cwd":"/tmp","state":"finished","reason":"killed","created_ms":1,"screen":"exact","cols":80,"rows":24}}'
    ;;
  attach)
    exit 0
    ;;
  *)
    echo "unexpected command: $cmd" >&2
    exit 1
    ;;
esac
"#,
            log_path.display()
        );
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let hc = HcCli::with_binary(&script_path);
        let repo = temp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let opened = hc
            .open_agent(&OpenAgentRequest {
                root: repo,
                role: "coder".into(),
                agent_id: "coder-1".into(),
                vendor_name: "codex".into(),
                command: "codex".into(),
                herdr_agent: "codex".into(),
                args: vec!["-m".into(), "gpt-5".into()],
                label: "coder·codex·test".into(),
                env: HashMap::from([("SHEP_AGENT_ID".into(), "coder-1".into())]),
                worktree: None,
            })
            .unwrap();
        assert_eq!(opened.pane, "t7");
        assert_eq!(opened.tab, "t7");

        assert_eq!(hc.prompt("t7", "do work").unwrap(), PromptOutcome::Delivered);
        assert_eq!(hc.prompt("t7", "BLOCK_ME").unwrap(), PromptOutcome::Blocked);
        hc.send_text("t7", "hello world").unwrap();
        hc.send_keys("t7", &["down".into(), "enter".into()]).unwrap();
        assert_eq!(hc.read_screen("t7", 50).unwrap(), "screen line 1\nscreen line 2");
        let waited = hc.wait("t7", Duration::from_secs(12)).unwrap();
        assert!(waited.ended);
        assert_eq!(waited.status.as_deref(), Some("done"));
        let status = hc.agent_status("coder-1").unwrap();
        assert_eq!(status.pane, "t7");
        assert_eq!(status.status, "working");
        assert_eq!(status.seq, 4);
        hc.label("t7", &HashMap::from([("role".into(), "coder".into()), ("task".into(), "1".into())]))
            .unwrap();
        hc.attach(Some("t7"), false).unwrap();
        hc.close("t7").unwrap();

        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("run --bg --json --outlive-parent --no-notify --name coder-1"));
        assert!(log.contains("SHEP_AGENT_ID=coder-1"));
        assert!(log.contains("send --json t7 do work"));
        assert!(log.contains("keys --json t7 down enter"));
        assert!(log.contains("read --json --lines 50 t7"));
        assert!(log.contains("wait --json --timeout 12 t7"));
        assert!(log.contains("ls --json"));
        assert!(log.contains("label --json t7 role=coder task=1"));
        assert!(log.contains("attach t7"));
        assert!(log.contains("kill --json t7"));

        std::fs::remove_dir_all(temp).unwrap();
    }
}
