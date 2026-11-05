//! Terminal runtime port (`trait Terminals`) and adapters (`ShepherdInProcess`, `ShepherdSocket`, `HcCli`, `InMemoryTerminals`).
//!
//! Shepherd's core workflow, spawner, and MCP server depend only on `Terminals`.

pub mod fakes;
pub mod in_process;
pub mod socket;

pub use fakes::{hc_attach_args, HcCli, InMemoryTerminals};
pub use in_process::ShepherdInProcess;
pub use socket::{shepherd_attach_args, ShepherdSocket};

use crate::{
    config::Config,
    git::ParsedWorktree,
    shepherd::{PromptWaitOptions, ShepherdClient, ShepherdError},
};
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

impl From<ShepherdError> for TerminalError {
    fn from(error: ShepherdError) -> Self {
        match error {
            ShepherdError::Io(io) => Self::Io(io),
            ShepherdError::Api { code, message } => Self::Api { code, message },
            ShepherdError::Protocol(message) => Self::Protocol(message),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSpec {
    pub branch: String,
    pub base: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAgentRequest {
    pub root: PathBuf,
    pub role: String,
    pub agent_id: String,
    pub vendor_name: String,
    pub command: String,
    pub shepherd_agent: String,
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
    fn prompt(
        &self,
        target: &str,
        text: &str,
        wait: Option<PromptWaitOptions>,
    ) -> Result<PromptOutcome, TerminalError>;
    fn prompt_text(&self, target: &str, text: &str) -> Result<PromptOutcome, TerminalError> {
        self.prompt(target, text, None)
    }
    fn send_text(&self, target: &str, text: &str) -> Result<(), TerminalError>;
    fn send_keys(&self, target: &str, keys: &[String]) -> Result<(), TerminalError>;
    fn read_screen(&self, target: &str, lines: u32) -> Result<String, TerminalError>;
    fn wait(&self, target: &str, timeout: Duration) -> Result<TerminalWaitResult, TerminalError>;
    fn wait_agent_event(&self, timeout: Duration) -> Result<bool, TerminalError> {
        std::thread::sleep(timeout);
        Ok(false)
    }
    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError>;
    fn label(&self, target: &str, labels: &HashMap<String, String>) -> Result<(), TerminalError>;
    fn notify(&self, title: &str, body: Option<&str>) -> Result<(), TerminalError> {
        let _ = (title, body);
        Ok(())
    }
    fn list_worktrees(&self, root: &Path) -> Result<Vec<ParsedWorktree>, TerminalError> {
        let output = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(root)
            .output()?;
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(crate::git::parse_git_worktree_porcelain(&text))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(TerminalError::Api {
                code: "git_failed".into(),
                message: stderr.trim().to_string(),
            })
        }
    }
    fn close(&self, target: &str) -> Result<(), TerminalError>;
    fn attach(&self, target: Option<&str>, takeover: bool) -> Result<(), TerminalError>;
    fn focus(&self, target: &str) -> Result<(), TerminalError> {
        self.attach(Some(target), false)
    }
}

/// Select the terminal backend from `SHEPHERD_TERMINALS` (highest priority) or `config.shepherd.backend` (default `"socket"`).
pub fn select_backend(config: &Config) -> Result<BackendKind, TerminalError> {
    let env_value = std::env::var("SHEPHERD_TERMINALS").ok();
    select_backend_with_env(config, env_value.as_deref())
}

pub fn select_backend_with_env(
    config: &Config,
    env_override: Option<&str>,
) -> Result<BackendKind, TerminalError> {
    let raw = env_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let cfg = config.shepherd.backend.trim();
            if cfg.is_empty() {
                "socket"
            } else {
                cfg
            }
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
pub fn set_in_process_terminals_factory(factory: Option<Arc<InProcessTerminalsFactory>>) {
    if let Ok(mut guard) = in_process_factory_slot().lock() {
        *guard = factory;
    }
}

/// Instantiate the active `Terminals` adapter according to `SHEPHERD_TERMINALS` and `config.shepherd`.
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
        BackendKind::Socket => Ok(Box::new(ShepherdSocket::connect(
            Some(&config.shepherd.session),
            config.shepherd.autostart,
        )?)),
        BackendKind::Hc => Ok(Box::new(HcCli::new())),
    }
}

pub(super) fn shepherd_focus(client: &ShepherdClient, target: &str) -> Result<(), TerminalError> {
    if client.focus_agent(target).is_ok() {
        return Ok(());
    }
    if client.focus_pane(target).is_ok() {
        return Ok(());
    }
    client.focus_tab(target)?;
    Ok(())
}

pub(super) fn shepherd_open_agent(
    client: &ShepherdClient,
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
            wt.base.as_deref(),
            &request.label,
        )?;
        workspace_id = nested_string(&created, &["workspace", "workspace_id"])?;
        initial_tab_id = nested_string(&created, &["tab", "tab_id"])?;
        worktree_path = nested_string(&created, &["worktree", "path"])?;
        branch =
            nested_string(&created, &["worktree", "branch"]).unwrap_or_else(|_| wt.branch.clone());
    } else {
        workspace_id = match find_workspace(&client.snapshot()?, &request.root) {
            Some(id) => id,
            None => {
                let label = request
                    .root
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                let created =
                    client.create_workspace(&request.root.display().to_string(), &label)?;
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

    if request.shepherd_agent.is_empty() {
        let command = crate::spawn::shell_join(
            std::iter::once(request.command.as_str())
                .chain(request.args.iter().map(String::as_str)),
        );
        client.send_input(&pane_id, &command)?;
    } else {
        start_shepherd_agent(
            client,
            &pane_id,
            &request.agent_id,
            &request.shepherd_agent,
            &request.args,
        )?;
    }

    Ok(OpenedAgent {
        workspace: workspace_id,
        tab: tab_id,
        pane: pane_id,
        worktree: worktree_path,
        branch,
    })
}

pub(super) fn shepherd_read_screen(
    client: &ShepherdClient,
    target: &str,
    lines: u32,
) -> Result<String, TerminalError> {
    let result = client.read_pane(target, lines)?;
    Ok(result
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

pub(super) fn shepherd_wait(
    client: &ShepherdClient,
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

pub(super) fn shepherd_agent_status(
    client: &ShepherdClient,
    target: &str,
) -> Result<TerminalAgentStatus, TerminalError> {
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
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| agent.get("name").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned(),
        seq: agent
            .get("state_change_seq")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

pub(super) fn shepherd_label(
    client: &ShepherdClient,
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

pub(super) fn shepherd_attach_process(
    session: &str,
    target: Option<&str>,
    takeover: bool,
) -> Result<(), TerminalError> {
    let args = shepherd_attach_args(session, target, takeover);
    let status = Command::new("shepherd").args(args).status()?;
    if !status.success() {
        return Err(TerminalError::Protocol(format!(
            "shepherd attach exited with {status}"
        )));
    }
    Ok(())
}

fn start_shepherd_agent(
    client: &ShepherdClient,
    pane: &str,
    name: &str,
    kind: &str,
    args: &[String],
) -> Result<(), TerminalError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.start_agent(pane, name, kind, args) {
            Ok(_) => return Ok(()),
            Err(ShepherdError::Api { code, .. }) if code == "agent_pane_busy" => {
                if Instant::now() >= deadline {
                    return Err(TerminalError::Protocol(
                        "Shepherd pane did not become ready for an agent within 30 seconds".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

pub(super) fn deliver_prompt(
    client: &ShepherdClient,
    target: &str,
    prompt: &str,
    wait: Option<PromptWaitOptions>,
) -> Result<PromptOutcome, TerminalError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.prompt_agent_with_wait(target, prompt, wait.as_ref()) {
            Ok(_) => return Ok(PromptOutcome::Delivered),
            Err(ShepherdError::Api { code, .. }) if code == "agent_blocked" => {
                return Ok(PromptOutcome::Blocked);
            }
            Err(ShepherdError::Api { code, .. }) if code == "agent_not_ready" => {
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

pub(super) fn shepherd_notify(
    client: &ShepherdClient,
    title: &str,
    body: Option<&str>,
) -> Result<(), TerminalError> {
    let mut params = serde_json::json!({
        "title": title,
        "sound": "request",
    });
    if let Some(b) = body {
        params["body"] = serde_json::json!(b);
    }
    let _ = client.call("notification.show", params);
    Ok(())
}

pub(super) fn shepherd_list_worktrees(
    client: &ShepherdClient,
    root: &Path,
) -> Result<Vec<ParsedWorktree>, TerminalError> {
    let params = serde_json::json!({
        "cwd": root.display().to_string(),
    });
    let result = client.call("worktree.list", params)?;
    let worktrees = result
        .get("worktrees")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TerminalError::Protocol("missing worktrees array in worktree.list response".into())
        })?;
    let parsed = worktrees
        .iter()
        .map(|wt| {
            let path = PathBuf::from(wt.get("path").and_then(Value::as_str).unwrap_or_default());
            let branch = wt.get("branch").and_then(Value::as_str).map(str::to_owned);
            let is_detached = wt
                .get("is_detached")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let is_bare = wt.get("is_bare").and_then(Value::as_bool).unwrap_or(false);
            let is_prunable = wt
                .get("is_prunable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            ParsedWorktree {
                path,
                head: String::new(),
                branch,
                is_detached,
                is_bare,
                is_locked: false,
                prunable: is_prunable,
            }
        })
        .collect();
    Ok(parsed)
}

pub(super) fn start_socket_event_subscription(
    socket_path: &Path,
) -> Option<std::sync::mpsc::Receiver<()>> {
    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        let stream = UnixStream::connect(socket_path).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("shepherd-socket-events".into())
            .spawn(move || {
                let mut stream = stream;
                let sub_req = r#"{"id":"shepherd-events-sub","method":"events.subscribe","params":{"subscriptions":[{"type":"pane.agent_status_changed"},{"type":"pane.agent_detected"},{"type":"pane.closed"},{"type":"pane.exited"}]}}"#;
                if stream.write_all(sub_req.as_bytes()).is_err()
                    || stream.write_all(b"\n").is_err()
                {
                    return;
                }
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while let Ok(n) = reader.read_line(&mut line) {
                    if n == 0 {
                        break;
                    }
                    if line.contains("pane.agent_status_changed")
                        || line.contains("pane.agent_detected")
                        || line.contains("pane.closed")
                        || line.contains("pane.exited")
                        || line.contains("PaneAgentStatusChanged")
                    {
                        let _ = tx.send(());
                    }
                    line.clear();
                }
            });
        Some(rx)
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        None
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
            TerminalError::Protocol(format!("Shepherd response is missing {key:?}"))
        })?;
    }
    current.as_str().map(str::to_owned).ok_or_else(|| {
        TerminalError::Protocol(format!(
            "Shepherd response field {} is not a string",
            path.join(".")
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_selection_prioritizes_env_over_config_and_defaults_to_socket() {
        let mut config = Config::default();
        assert_eq!(
            select_backend_with_env(&config, None).unwrap(),
            BackendKind::Socket
        );
        config.shepherd.backend = "hc".into();
        assert_eq!(
            select_backend_with_env(&config, None).unwrap(),
            BackendKind::Hc
        );
        config.shepherd.backend = "in-process".into();
        assert_eq!(
            select_backend_with_env(&config, None).unwrap(),
            BackendKind::InProcess
        );
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
}
