//! Small adapter for Herdr's local NDJSON socket API.
//! PTYs, workspaces, agent detection, and terminal attach remain owned by Herdr.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

pub type InProcessRpcHandler = dyn Fn(&str, Value) -> Result<Value, HerdrError> + Send + Sync;

#[derive(Debug)]
pub enum HerdrError {
    Io(std::io::Error),
    Api { code: String, message: String },
    Protocol(String),
}

impl std::fmt::Display for HerdrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "herdr: {error}"),
            Self::Api { code, message } => write!(f, "herdr: {code}: {message}"),
            Self::Protocol(message) => write!(f, "herdr: bad response: {message}"),
        }
    }
}

impl std::error::Error for HerdrError {}
impl From<std::io::Error> for HerdrError { fn from(error: std::io::Error) -> Self { Self::Io(error) } }

#[derive(Serialize)]
struct Request<'a> {
    id: String,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct Response {
    id: String,
    result: Option<Value>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError { code: String, message: String }

/// Resolve the socket from Herdr's environment or its own server status command.
/// A named session deliberately ignores HERDR_SOCKET_PATH, which may point at another session.
pub fn resolve_socket(session: Option<&str>) -> Result<PathBuf, HerdrError> {
    if session.unwrap_or_default().is_empty() {
        if let Some(path) = std::env::var_os("HERDR_SOCKET_PATH").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path));
        }
    }
    let mut command = Command::new("herdr");
    if let Some(name) = session.filter(|name| !name.is_empty() && *name != "-") {
        command.args(["--session", name]);
    }
    let output = command.args(["status", "server"]).output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            HerdrError::Protocol("herdr not found in PATH; install Herdr first".into())
        } else { HerdrError::Io(error) }
    })?;
    if !output.status.success() {
        return Err(HerdrError::Protocol(format!("status server failed: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }
    parse_socket_path(&String::from_utf8_lossy(&output.stdout))
        .map(PathBuf::from)
        .ok_or_else(|| HerdrError::Protocol("status server returned no socket path".into()))
}

fn parse_socket_path(output: &str) -> Option<&str> {
    output.lines().find_map(|line| line.trim().strip_prefix("socket:").map(str::trim).filter(|value| !value.is_empty()))
}

pub struct HerdrClient {
    socket: PathBuf,
    in_process: Option<Arc<InProcessRpcHandler>>,
    sequence: AtomicU64,
}

impl Clone for HerdrClient {
    fn clone(&self) -> Self {
        Self {
            socket: self.socket.clone(),
            in_process: self.in_process.clone(),
            sequence: AtomicU64::new(self.sequence.load(Ordering::Relaxed)),
        }
    }
}

impl HerdrClient {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            in_process: None,
            sequence: AtomicU64::new(1),
        }
    }

    /// Construct a `HerdrClient` that dispatches API calls in-process without opening a socket.
    pub fn in_process(
        handler: impl Fn(&str, Value) -> Result<Value, HerdrError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            socket: PathBuf::new(),
            in_process: Some(Arc::new(handler)),
            sequence: AtomicU64::new(1),
        }
    }

    pub fn is_in_process(&self) -> bool {
        self.in_process.is_some()
    }

    /// Check whether the Herdr server is reachable.
    pub fn is_reachable(&self) -> bool {
        if self.in_process.is_some() {
            return true;
        }
        #[cfg(unix)]
        {
            if self.socket.as_os_str().is_empty() || !self.socket.exists() {
                return false;
            }
            use std::{io::{BufRead, BufReader, Write}, os::unix::net::UnixStream};
            let stream = match UnixStream::connect(&self.socket) {
                Ok(s) => s,
                Err(_) => return false,
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
            let req = Request {
                id: "probe-shepherd".into(),
                method: "shepherd.call",
                params: serde_json::json!({ "op": "skills", "args": {} }),
            };
            if let Ok(mut bytes) = serde_json::to_vec(&req) {
                bytes.push(b'\n');
                let mut writer = &stream;
                if writer.write_all(&bytes).is_ok() && writer.flush().is_ok() {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() && !line.trim().is_empty() {
                        if let Ok(resp) = serde_json::from_str::<Value>(&line) {
                            if let Some(err) = resp.get("error") {
                                let code = err.get("code").and_then(Value::as_str).unwrap_or("");
                                if code == "invalid_request" || code == "shepherd_not_running" {
                                    return false;
                                }
                            }
                            return true;
                        }
                    }
                }
            }
            false
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Generic caller for `shepherd.call` operations.
    pub fn shepherd_call(&self, op: &str, args: Value) -> Result<Value, HerdrError> {
        let result = self.call("shepherd.call", serde_json::json!({
            "op": op,
            "args": args,
        }))?;
        if let Some(data) = result.get("data") {
            Ok(data.clone())
        } else {
            Ok(result)
        }
    }

    pub fn from_session(session: Option<&str>) -> Result<Self, HerdrError> {
        Ok(Self::new(resolve_socket(session)?))
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    pub fn ensure_session(session: Option<&str>, autostart: bool) -> Result<Self, HerdrError> {
        let existing = Self::from_session(session);
        if let Ok(client) = &existing {
            if client.call("ping", serde_json::json!({})).is_ok() {
                return Ok(Self::new(client.socket.clone()));
            }
        }
        if !autostart {
            return existing;
        }
        start_server(session)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(client) = Self::from_session(session) {
                if client.call("ping", serde_json::json!({})).is_ok() {
                    return Ok(client);
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(HerdrError::Protocol(
                    "Herdr server was started but did not answer within 20 seconds".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Send a single Herdr API request and return its `result` value.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        if let Some(handler) = &self.in_process {
            return handler(method, params);
        }
        #[cfg(unix)]
        {
            self.call_unix(method, params)
        }
        #[cfg(not(unix))]
        {
            let _ = (method, params);
            Err(HerdrError::Protocol(
                "Herdr socket transport is not available on this platform".into(),
            ))
        }
    }

    pub fn snapshot(&self) -> Result<Value, HerdrError> {
        let result = self.call("session.snapshot", serde_json::json!({}))?;
        Ok(result.get("snapshot").cloned().unwrap_or(result))
    }

    pub fn create_workspace(&self, cwd: &str, label: &str) -> Result<Value, HerdrError> {
        self.call("workspace.create", serde_json::json!({"cwd":cwd,"label":label,"focus":false}))
    }

    pub fn create_tab(&self, workspace_id: &str, cwd: &str, label: &str, env: &std::collections::HashMap<String, String>) -> Result<Value, HerdrError> {
        self.call("tab.create", serde_json::json!({"workspace_id":workspace_id,"cwd":cwd,"label":label,"focus":false,"env":env}))
    }

    pub fn create_worktree(
        &self,
        cwd: &str,
        branch: &str,
        base: Option<&str>,
        label: &str,
    ) -> Result<Value, HerdrError> {
        let mut params = serde_json::json!({
            "cwd": cwd,
            "branch": branch,
            "label": label,
            "focus": false,
        });
        if let Some(base) = base.filter(|s| !s.trim().is_empty()) {
            params["base"] = serde_json::Value::String(base.to_owned());
        }
        self.call("worktree.create", params)
    }

    pub fn start_agent(&self, pane_id: &str, name: &str, kind: &str, args: &[String]) -> Result<Value, HerdrError> {
        self.call("agent.start", serde_json::json!({"pane_id":pane_id,"name":name,"kind":kind,"args":args,"timeout_ms":90000}))
    }

    pub fn prompt_agent(&self, target: &str, text: &str) -> Result<Value, HerdrError> {
        self.call("agent.prompt", serde_json::json!({"target":target,"text":text}))
    }

    pub fn send_input(&self, pane_id: &str, text: &str) -> Result<Value, HerdrError> {
        self.call("pane.send_input", serde_json::json!({"pane_id":pane_id,"text":text,"keys":["Enter"]}))
    }

    pub fn send_keys(&self, pane_id: &str, keys: &[String]) -> Result<Value, HerdrError> {
        self.call("pane.send_input", serde_json::json!({"pane_id":pane_id,"keys":keys}))
    }

    pub fn read_pane(&self, pane_id: &str, lines: u32) -> Result<Value, HerdrError> {
        let result = self.call("pane.read", serde_json::json!({"pane_id":pane_id,"source":"recent","lines":lines,"strip_ansi":true}))?;
        Ok(result.get("read").cloned().unwrap_or(result))
    }

    pub fn wait_agent(&self, target: &str, until: &[&str], timeout: Duration) -> Result<Value, HerdrError> {
        self.call("agent.wait", serde_json::json!({"target":target,"until":until,"timeout_ms":timeout.as_millis() as u64}))
    }

    pub fn get_agent(&self, target: &str) -> Result<Value, HerdrError> {
        let result = self.call("agent.get", serde_json::json!({"target":target}))?;
        Ok(result.get("agent").cloned().unwrap_or(result))
    }

    pub fn close_tab(&self, tab_id: &str) -> Result<Value, HerdrError> {
        self.call("tab.close", serde_json::json!({"tab_id":tab_id}))
    }

    pub fn focus_agent(&self, target: &str) -> Result<Value, HerdrError> {
        self.call("agent.focus", serde_json::json!({"target":target}))
    }

    pub fn focus_tab(&self, tab_id: &str) -> Result<Value, HerdrError> {
        self.call("tab.focus", serde_json::json!({"tab_id":tab_id}))
    }

    pub fn focus_pane(&self, pane_id: &str) -> Result<Value, HerdrError> {
        self.call("pane.focus", serde_json::json!({"pane_id":pane_id}))
    }

    pub fn rename_pane(&self, pane_id: &str, label: &str) -> Result<Value, HerdrError> {
        self.call("pane.rename", serde_json::json!({"pane_id":pane_id,"label":label}))
    }

    pub fn report_metadata(&self, pane_id: &str, role: &str, agent_id: &str, vendor: &str, title: &str) -> Result<Value, HerdrError> {
        self.call("pane.report_metadata", serde_json::json!({"pane_id":pane_id,"source":"shepherd","title":title,"tokens":{"role":role,"agent":agent_id,"vendor":vendor}}))
    }

    #[cfg(unix)]
    fn call_unix(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        use std::{io::{BufRead, BufReader, Write}, os::unix::net::UnixStream};
        let stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let id = format!("shepherd-{}", self.sequence.fetch_add(1, Ordering::Relaxed));
        let request = Request { id: id.clone(), method, params };
        let mut bytes = serde_json::to_vec(&request).map_err(|e| HerdrError::Protocol(e.to_string()))?;
        bytes.push(b'\n');
        let mut writer = &stream;
        writer.write_all(&bytes)?;
        writer.flush()?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.len() > 16 * 1024 * 1024 { return Err(HerdrError::Protocol("response exceeds 16 MiB".into())); }
        let response: Response = serde_json::from_str(&line).map_err(|e| HerdrError::Protocol(e.to_string()))?;
        if response.id != id { return Err(HerdrError::Protocol(format!("response id {:?} does not match request {id:?}", response.id))); }
        if let Some(error) = response.error { return Err(HerdrError::Api { code: error.code, message: error.message }); }
        response.result.ok_or_else(|| HerdrError::Protocol("success response has no result".into()))
    }
}

fn start_server(session: Option<&str>) -> Result<(), HerdrError> {
    use std::{fs::OpenOptions, process::{Command, Stdio}};
    let state_dir = crate::config::state_dir();
    std::fs::create_dir_all(&state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut log_options = OpenOptions::new();
    log_options.create(true).append(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_options.mode(0o600);
    }
    let log = log_options.open(state_dir.join("herdr.log"))?;
    let mut command = Command::new("herdr");
    if let Some(name) = session.filter(|name| !name.is_empty() && *name != "-") { command.args(["--session", name]); }
    command.arg("server").stdin(Stdio::null()).stdout(Stdio::from(log.try_clone()?)).stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound { HerdrError::Protocol("herdr not found in PATH".into()) }
        else { HerdrError::Io(error) }
    })?;
    std::thread::spawn(move || { let mut child = child; let _ = child.wait(); });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_socket_line_and_ignores_blank_values() {
        assert_eq!(parse_socket_path("pid: 42\nsocket: /tmp/herdr.sock\n"), Some("/tmp/herdr.sock"));
        assert_eq!(parse_socket_path("socket: \n"), None);
    }

    #[test]
    fn in_process_client_dispatches_without_socket() {
        let client = HerdrClient::in_process(|method, params| match method {
            "session.snapshot" => Ok(serde_json::json!({
                "snapshot": {
                    "version": "in-process-test",
                    "params": params,
                }
            })),
            other => Err(HerdrError::Api {
                code: "unsupported".into(),
                message: format!("unexpected method {other}"),
            }),
        });
        assert!(client.is_in_process());
        assert!(client.socket().as_os_str().is_empty());
        let snapshot = client.snapshot().unwrap();
        assert_eq!(snapshot["version"], "in-process-test");
        let err = client.call("unknown.method", serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[cfg(unix)]
    #[test]
    fn client_sends_ndjson_and_validates_response_id() {
        use std::{io::{BufRead, BufReader, Write}, os::unix::net::UnixListener};
        let socket = std::env::temp_dir().join(format!("shepherd-herdr-api-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "session.snapshot");
            let response = serde_json::json!({"id": request["id"], "result": {"type":"snapshot", "version":"test"}});
            let mut stream = reader.into_inner();
            writeln!(stream, "{response}").unwrap();
        });
        let result = HerdrClient::new(&socket).call("session.snapshot", serde_json::json!({})).unwrap();
        assert_eq!(result["version"], "test");
        server.join().unwrap();
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn create_worktree_omits_base_when_empty_or_none() {
        let client = HerdrClient::in_process(|method, params| {
            assert_eq!(method, "worktree.create");
            Ok(serde_json::json!({
                "received_params": params,
            }))
        });
        let res_none = client.create_worktree("/repo", "shep/1", None, "label").unwrap();
        assert_eq!(res_none["received_params"].get("base"), None);
        assert_eq!(res_none["received_params"]["branch"], "shep/1");
        assert_eq!(res_none["received_params"]["cwd"], "/repo");
        assert_eq!(res_none["received_params"]["label"], "label");

        let res_empty = client.create_worktree("/repo", "shep/1", Some(""), "label").unwrap();
        assert_eq!(res_empty["received_params"].get("base"), None);

        let res_whitespace = client.create_worktree("/repo", "shep/1", Some("   "), "label").unwrap();
        assert_eq!(res_whitespace["received_params"].get("base"), None);

        let res_base = client.create_worktree("/repo", "shep/1", Some("develop"), "label").unwrap();
        assert_eq!(res_base["received_params"]["base"], "develop");
    }

    #[test]
    fn shepherd_call_and_is_reachable() {
        let client = HerdrClient::in_process(|method, params| {
            assert_eq!(method, "shepherd.call");
            assert_eq!(params["op"], "agents");
            Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": [
                    { "id": "coder-1", "role": "coder" }
                ]
            }))
        });
        assert!(client.is_reachable());
        let res = client.shepherd_call("agents", serde_json::json!({})).unwrap();
        assert_eq!(res[0]["id"], "coder-1");
    }

    #[test]
    fn socket_client_unreachable_when_file_missing() {
        let client = HerdrClient::new(PathBuf::from("/tmp/nonexistent-shepherd-test-12345.sock"));
        assert!(!client.is_reachable());
    }
}
