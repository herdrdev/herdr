//! In-memory and CLI test doubles for `trait Terminals`.

use super::{
    OpenAgentRequest, OpenedAgent, PromptOutcome, TerminalAgentStatus, TerminalError,
    TerminalWaitResult, Terminals, WorktreeSpec,
};
use crate::{git::ParsedWorktree, shepherd::PromptWaitOptions};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

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
        Self {
            binary: PathBuf::from("hc"),
        }
    }

    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    fn run_json(
        &self,
        args: &[String],
        env: Option<&HashMap<String, String>>,
    ) -> Result<Value, TerminalError> {
        let output = self.run_raw(args, env)?;
        if !output.status.success() {
            return Err(parse_hc_error(&output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(trimmed).map_err(|error| {
            TerminalError::Protocol(format!("hc invalid JSON ({error}): {trimmed}"))
        })
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
        let mut attempts = 0;
        loop {
            match command.output() {
                Ok(output) => return Ok(output),
                Err(error) => {
                    // On Linux, parallel tests forking child processes can briefly trigger ETXTBSY (26)
                    // if a write fd was temporarily inherited across fork before close.
                    if error.raw_os_error() == Some(26) && attempts < 20 {
                        attempts += 1;
                        std::thread::sleep(std::time::Duration::from_millis(15));
                        continue;
                    }
                    if error.kind() == std::io::ErrorKind::NotFound {
                        return Err(TerminalError::Protocol(format!(
                            "{} not found in PATH",
                            self.binary.display()
                        )));
                    } else {
                        return Err(TerminalError::Io(error));
                    }
                }
            }
        }
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
        if let Some(base) = spec.base.as_deref().filter(|s| !s.trim().is_empty()) {
            args.push(base.to_owned());
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
            message: if detail.is_empty() {
                "terminal is blocked".into()
            } else {
                detail
            },
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

    fn prompt(
        &self,
        target: &str,
        text: &str,
        _wait: Option<PromptWaitOptions>,
    ) -> Result<PromptOutcome, TerminalError> {
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
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .to_owned();
        Ok(TerminalWaitResult {
            ended,
            status,
            reason,
        })
    }

    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError> {
        let args = vec!["ls".to_owned(), "--json".to_owned()];
        let value = self.run_json(&args, None)?;
        let list = value
            .get("terminals")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
            .ok_or_else(|| {
                TerminalError::Protocol("hc ls --json did not return a terminals array".into())
            })?;
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
            .and_then(|s| {
                s.get("state")
                    .and_then(Value::as_str)
                    .or_else(|| s.as_str())
            })
            .or_else(|| {
                if state == "finished" {
                    item.get("reason")
                        .and_then(Value::as_str)
                        .or(Some("finished"))
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
            return Err(TerminalError::Protocol(format!(
                "hc attach exited with {status}"
            )));
        }
        Ok(())
    }
}

/// In-memory fake implementation of `Terminals` for unit and integration tests.
#[derive(Clone)]
pub struct InMemoryTerminals {
    inner: Arc<Mutex<InMemoryState>>,
    condvar: Arc<std::sync::Condvar>,
}

impl Default for InMemoryTerminals {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(InMemoryState::default())),
            condvar: Arc::new(std::sync::Condvar::new()),
        }
    }
}

pub type PromptCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;

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
    pub prompt_errors: HashMap<String, (String, String)>,
    pub closed: Vec<String>,
    pub attached: Vec<(Option<String>, bool)>,
    pub focused: Vec<String>,
    pub create_real_worktrees: bool,
    pub disable_auto_react: bool,
    pub disable_echo_on_send_text: bool,
    pub on_prompt: Option<PromptCallback>,
    pub notifications: Vec<(String, Option<String>)>,
    pub worktrees: Option<Vec<ParsedWorktree>>,
    pub event_signaled: bool,
}

impl InMemoryTerminals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_echo_on_send_text(&self, enabled: bool) {
        self.inner.lock().unwrap().disable_echo_on_send_text = !enabled;
    }

    pub fn with_real_worktrees() -> Self {
        let fake = Self::new();
        fake.inner.lock().unwrap().create_real_worktrees = true;
        fake
    }

    pub fn on_prompt(&self, callback: impl Fn(&str, &str) + Send + Sync + 'static) {
        self.inner.lock().unwrap().on_prompt = Some(Arc::new(callback));
    }

    pub fn set_auto_react_on_prompt(&self, enabled: bool) {
        self.inner.lock().unwrap().disable_auto_react = !enabled;
    }

    pub fn set_prompt_outcome(&self, target: impl Into<String>, outcome: PromptOutcome) {
        self.inner
            .lock()
            .unwrap()
            .prompt_outcomes
            .insert(target.into(), outcome);
    }

    pub fn set_prompt_error(
        &self,
        target: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.inner
            .lock()
            .unwrap()
            .prompt_errors
            .insert(target.into(), (code.into(), message.into()));
    }

    pub fn set_screen(&self, target: impl Into<String>, screen: impl Into<String>) {
        self.inner
            .lock()
            .unwrap()
            .screens
            .insert(target.into(), screen.into());
    }

    pub fn set_status(&self, target: impl Into<String>, status: TerminalAgentStatus) {
        let mut state = self.inner.lock().unwrap();
        let target_str = target.into();
        if !status.pane.is_empty() && status.pane != target_str {
            state.statuses.insert(status.pane.clone(), status.clone());
        }
        state.statuses.insert(target_str, status);
        state.event_signaled = true;
        self.condvar.notify_all();
    }

    pub fn remove_status(&self, target: &str) {
        let mut state = self.inner.lock().unwrap();
        if let Some(removed) = state.statuses.remove(target) {
            if !removed.pane.is_empty() && removed.pane != target {
                state.statuses.remove(&removed.pane);
            }
        }
    }

    pub fn trigger_agent_event(&self) {
        let mut state = self.inner.lock().unwrap();
        state.event_signaled = true;
        self.condvar.notify_all();
    }

    pub fn notifications(&self) -> Vec<(String, Option<String>)> {
        self.inner.lock().unwrap().notifications.clone()
    }

    pub fn set_worktrees(&self, worktrees: Vec<ParsedWorktree>) {
        self.inner.lock().unwrap().worktrees = Some(worktrees);
    }

    pub fn put_open_pane(
        &self,
        agent_id: impl Into<String>,
        pane: impl Into<String>,
        tab: impl Into<String>,
        title: impl Into<String>,
    ) {
        let agent_id = agent_id.into();
        let pane = pane.into();
        let tab = tab.into();
        let title = title.into();
        let status = TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: pane.clone(),
            tab: tab.clone(),
            title,
            seq: 1,
        };
        let mut state = self.inner.lock().unwrap();
        state.statuses.insert(agent_id, status.clone());
        state.statuses.insert(pane, status);
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

    pub fn focused(&self) -> Vec<String> {
        self.inner.lock().unwrap().focused.clone()
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
        if !request.agent_id.is_empty() {
            if let Some(existing) = state.statuses.get(&request.agent_id) {
                if existing.state != "finished" {
                    return Err(TerminalError::Api {
                        code: "agent_name_taken".into(),
                        message: format!(
                            "agent name {} is already used by pane {} (title {:?})",
                            request.agent_id, existing.pane, existing.title
                        ),
                    });
                }
            }
        }
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
                    request
                        .root
                        .join(".worktrees")
                        .join(spec.branch.replace('/', "-"))
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
        state
            .statuses
            .insert(request.agent_id.clone(), status.clone());
        state.statuses.insert(pane.clone(), status);
        state.opened.push(request.clone());
        Ok(OpenedAgent {
            workspace,
            tab,
            pane,
            worktree,
            branch,
        })
    }

    fn prompt(
        &self,
        target: &str,
        text: &str,
        wait: Option<PromptWaitOptions>,
    ) -> Result<PromptOutcome, TerminalError> {
        let (initial_seq, callback) = {
            let mut state = self.inner.lock().unwrap();
            state.prompts.push((target.to_owned(), text.to_owned()));
            if let Some((code, message)) = state.prompt_errors.get(target) {
                return Err(TerminalError::Api {
                    code: code.clone(),
                    message: message.clone(),
                });
            }
            if let Some(outcome) = state.prompt_outcomes.get(target).copied() {
                if outcome == PromptOutcome::Blocked {
                    return Ok(PromptOutcome::Blocked);
                }
            }
            let initial_seq = state.statuses.get(target).map(|s| s.seq).unwrap_or(0);
            if !state.disable_auto_react && wait.is_some() {
                let mut pane_to_update = None;
                if let Some(status) = state.statuses.get_mut(target) {
                    status.status = "working".into();
                    status.seq += 1;
                    pane_to_update = Some(status.pane.clone());
                }
                if let Some(pane) = pane_to_update.filter(|p| p != target) {
                    if let Some(pane_status) = state.statuses.get_mut(&pane) {
                        pane_status.status = "working".into();
                        pane_status.seq += 1;
                    }
                }
            }
            (initial_seq, state.on_prompt.clone())
        };
        if let Some(cb) = callback {
            cb(target, text);
        }
        if let Some(w) = &wait {
            let state = self.inner.lock().unwrap();
            let current = state.statuses.get(target);
            let current_seq = current.map(|s| s.seq).unwrap_or(0);
            let current_status = current.map(|s| s.status.as_str()).unwrap_or("");
            let reacted = current_status == "working"
                || current_status == "blocked"
                || current_seq > initial_seq;
            if !reacted && state.disable_auto_react {
                return Err(TerminalError::Api {
                    code: "agent_prompt_stalled".into(),
                    message: format!("no reaction within {:?}", w.timeout),
                });
            }
        }
        Ok(PromptOutcome::Delivered)
    }

    fn send_text(&self, target: &str, text: &str) -> Result<(), TerminalError> {
        let mut state = self.inner.lock().unwrap();
        state.sent_text.push((target.to_owned(), text.to_owned()));
        if !state.disable_echo_on_send_text {
            let mut updated = false;
            if let Some(screen) = state.screens.get_mut(target) {
                if !screen.ends_with('\n') && !screen.is_empty() {
                    screen.push('\n');
                }
                screen.push_str(text);
                screen.push('\n');
                updated = true;
            }
            if !updated {
                let alternate = state.statuses.get(target).and_then(|s| {
                    if s.pane != target && !s.pane.is_empty() {
                        Some(s.pane.clone())
                    } else {
                        None
                    }
                });
                if let Some(alt) = alternate {
                    if let Some(screen) = state.screens.get_mut(&alt) {
                        if !screen.ends_with('\n') && !screen.is_empty() {
                            screen.push('\n');
                        }
                        screen.push_str(text);
                        screen.push('\n');
                    }
                }
            }
        }
        Ok(())
    }

    fn send_keys(&self, target: &str, keys: &[String]) -> Result<(), TerminalError> {
        self.inner
            .lock()
            .unwrap()
            .sent_keys
            .push((target.to_owned(), keys.to_vec()));
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

    fn wait_agent_event(&self, timeout: Duration) -> Result<bool, TerminalError> {
        let state = self.inner.lock().unwrap();
        if state.event_signaled {
            drop(state);
            let mut state = self.inner.lock().unwrap();
            state.event_signaled = false;
            return Ok(true);
        }
        let (mut guard, _) = self
            .condvar
            .wait_timeout_while(state, timeout, |st| !st.event_signaled)
            .unwrap();
        if guard.event_signaled {
            guard.event_signaled = false;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn notify(&self, title: &str, body: Option<&str>) -> Result<(), TerminalError> {
        let mut state = self.inner.lock().unwrap();
        state
            .notifications
            .push((title.to_owned(), body.map(str::to_owned)));
        Ok(())
    }

    fn list_worktrees(&self, root: &Path) -> Result<Vec<ParsedWorktree>, TerminalError> {
        let state = self.inner.lock().unwrap();
        if let Some(wts) = &state.worktrees {
            return Ok(wts.clone());
        }
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
        for (key, status) in state.statuses.iter_mut() {
            if key == target || status.pane == target || status.tab == target {
                status.state = "finished".into();
                status.status = "killed".into();
            }
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

    fn focus(&self, target: &str) -> Result<(), TerminalError> {
        let mut state = self.inner.lock().unwrap();
        let exists = state.statuses.iter().any(|(key, status)| {
            status.state != "finished"
                && (key == target || status.pane == target || status.tab == target)
        });
        if !exists {
            return Err(TerminalError::Api {
                code: "not_found".into(),
                message: format!("focus target {target:?} not found"),
            });
        }
        state.focused.push(target.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                shepherd_agent: "codex".into(),
                args: vec!["-m".into(), "gpt-5".into()],
                label: "coder·codex·task".into(),
                env: HashMap::from([("SHEP_AGENT_ID".into(), "coder-1".into())]),
                worktree: Some(WorktreeSpec {
                    branch: "shep/1-task".into(),
                    base: Some("main".into()),
                }),
            })
            .unwrap();
        assert_eq!(opened.pane, "pane-1");
        assert_eq!(opened.branch, "shep/1-task");
        assert_eq!(
            fake.prompt_text("coder-1", "implement feature").unwrap(),
            PromptOutcome::Delivered
        );
        fake.set_prompt_outcome("coder-1", PromptOutcome::Blocked);
        assert_eq!(
            fake.prompt_text("coder-1", "blocked prompt").unwrap(),
            PromptOutcome::Blocked
        );
        fake.send_text(&opened.pane, "echo hi").unwrap();
        fake.send_keys(&opened.pane, &["Enter".into(), "Escape".into()])
            .unwrap();
        fake.set_screen(&opened.pane, "line 1\nline 2\n");
        assert_eq!(
            fake.read_screen(&opened.pane, 20).unwrap(),
            "line 1\nline 2\n"
        );
        let waited = fake.wait("coder-1", Duration::from_secs(5)).unwrap();
        assert!(waited.ended);
        let status = fake.agent_status("coder-1").unwrap();
        assert_eq!(status.state, "live");
        fake.label(
            &opened.pane,
            &HashMap::from([("role".into(), "coder".into())]),
        )
        .unwrap();
        assert_eq!(
            fake.labels_for(&opened.pane)
                .get("role")
                .map(String::as_str),
            Some("coder")
        );
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
        let temp =
            std::env::temp_dir().join(format!("shepherd-fake-hc-{}-{nonce}", std::process::id()));
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
                shepherd_agent: "codex".into(),
                args: vec!["-m".into(), "gpt-5".into()],
                label: "coder·codex·test".into(),
                env: HashMap::from([("SHEP_AGENT_ID".into(), "coder-1".into())]),
                worktree: None,
            })
            .unwrap();
        assert_eq!(opened.pane, "t7");
        assert_eq!(opened.tab, "t7");

        assert_eq!(
            hc.prompt_text("t7", "do work").unwrap(),
            PromptOutcome::Delivered
        );
        assert_eq!(
            hc.prompt_text("t7", "BLOCK_ME").unwrap(),
            PromptOutcome::Blocked
        );
        hc.send_text("t7", "hello world").unwrap();
        hc.send_keys("t7", &["down".into(), "enter".into()])
            .unwrap();
        assert_eq!(
            hc.read_screen("t7", 50).unwrap(),
            "screen line 1\nscreen line 2"
        );
        let waited = hc.wait("t7", Duration::from_secs(12)).unwrap();
        assert!(waited.ended);
        assert_eq!(waited.status.as_deref(), Some("done"));
        let status = hc.agent_status("coder-1").unwrap();
        assert_eq!(status.pane, "t7");
        assert_eq!(status.status, "working");
        assert_eq!(status.seq, 4);
        hc.label(
            "t7",
            &HashMap::from([("role".into(), "coder".into()), ("task".into(), "1".into())]),
        )
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
