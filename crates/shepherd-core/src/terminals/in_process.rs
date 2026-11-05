//! In-process terminal adapter routing calls through direct API handlers.

use super::{
    deliver_prompt, shepherd_agent_status, shepherd_attach_process, shepherd_focus, shepherd_label,
    shepherd_list_worktrees, shepherd_notify, shepherd_open_agent, shepherd_read_screen,
    shepherd_wait, OpenAgentRequest, OpenedAgent, PromptOutcome, TerminalAgentStatus,
    TerminalError, TerminalWaitResult, Terminals,
};
use crate::{
    git::ParsedWorktree,
    shepherd::{PromptWaitOptions, ShepherdClient, ShepherdError},
};
use serde_json::Value;
use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

pub type AttachHandler = Arc<dyn Fn(Option<&str>, bool) -> Result<(), TerminalError> + Send + Sync>;
pub type EventWaiter = Arc<dyn Fn(Duration) -> Result<bool, TerminalError> + Send + Sync>;

/// In-process adapter for Shepherd's runtime API (`trait Terminals`), avoiding external NDJSON socket I/O
/// in `shepherd-core` by routing RPC calls through a direct handler (`App` channel or `ApiClient`).
pub struct ShepherdInProcess {
    client: ShepherdClient,
    session: String,
    attach_handler: Option<AttachHandler>,
    event_waiter: Option<EventWaiter>,
}

impl ShepherdInProcess {
    pub fn new(
        session: impl Into<String>,
        handler: impl Fn(&str, Value) -> Result<Value, ShepherdError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            client: ShepherdClient::in_process(handler),
            session: session.into(),
            attach_handler: None,
            event_waiter: None,
        }
    }

    pub fn with_client(client: ShepherdClient, session: impl Into<String>) -> Self {
        Self {
            client,
            session: session.into(),
            attach_handler: None,
            event_waiter: None,
        }
    }

    pub fn with_attach_handler(
        mut self,
        handler: impl Fn(Option<&str>, bool) -> Result<(), TerminalError> + Send + Sync + 'static,
    ) -> Self {
        self.attach_handler = Some(Arc::new(handler));
        self
    }

    pub fn with_event_waiter(
        mut self,
        waiter: impl Fn(Duration) -> Result<bool, TerminalError> + Send + Sync + 'static,
    ) -> Self {
        self.event_waiter = Some(Arc::new(waiter));
        self
    }

    pub fn client(&self) -> &ShepherdClient {
        &self.client
    }

    pub fn session(&self) -> &str {
        &self.session
    }
}

impl Terminals for ShepherdInProcess {
    fn open_agent(&self, request: &OpenAgentRequest) -> Result<OpenedAgent, TerminalError> {
        shepherd_open_agent(&self.client, request)
    }

    fn prompt(
        &self,
        target: &str,
        text: &str,
        wait: Option<PromptWaitOptions>,
    ) -> Result<PromptOutcome, TerminalError> {
        deliver_prompt(&self.client, target, text, wait)
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
        shepherd_read_screen(&self.client, target, lines)
    }

    fn wait(&self, target: &str, timeout: Duration) -> Result<TerminalWaitResult, TerminalError> {
        shepherd_wait(&self.client, target, timeout)
    }

    fn wait_agent_event(&self, timeout: Duration) -> Result<bool, TerminalError> {
        if let Some(waiter) = &self.event_waiter {
            return waiter(timeout);
        }
        std::thread::sleep(timeout);
        Ok(false)
    }

    fn agent_status(&self, target: &str) -> Result<TerminalAgentStatus, TerminalError> {
        shepherd_agent_status(&self.client, target)
    }

    fn label(&self, target: &str, labels: &HashMap<String, String>) -> Result<(), TerminalError> {
        shepherd_label(&self.client, target, labels)
    }

    fn notify(&self, title: &str, body: Option<&str>) -> Result<(), TerminalError> {
        shepherd_notify(&self.client, title, body)
    }

    fn list_worktrees(&self, root: &Path) -> Result<Vec<ParsedWorktree>, TerminalError> {
        shepherd_list_worktrees(&self.client, root)
    }

    fn close(&self, target: &str) -> Result<(), TerminalError> {
        self.client.close_tab(target)?;
        Ok(())
    }

    fn attach(&self, target: Option<&str>, takeover: bool) -> Result<(), TerminalError> {
        if let Some(handler) = &self.attach_handler {
            return handler(target, takeover);
        }
        shepherd_attach_process(&self.session, target, takeover)
    }

    fn focus(&self, target: &str) -> Result<(), TerminalError> {
        shepherd_focus(&self.client, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminals::WorktreeSpec;
    use std::{path::PathBuf, sync::Mutex};

    type AttachedLog = Arc<Mutex<Vec<(Option<String>, bool)>>>;

    #[test]
    fn shepherd_in_process_executes_every_terminals_operation_without_socket() {
        let calls: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let attached: AttachedLog = Arc::new(Mutex::new(Vec::new()));
        let calls_ref = Arc::clone(&calls);
        let attached_ref = Arc::clone(&attached);

        let adapter = ShepherdInProcess::new("shepherd", move |method, params| {
            calls_ref
                .lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
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
                "tab.close"
                | "pane.rename"
                | "pane.report_metadata"
                | "agent.start"
                | "pane.send_input" => Ok(serde_json::json!({"ok": true})),
                "agent.prompt" => {
                    if params["text"] == "BLOCK_ME" {
                        Err(ShepherdError::Api {
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
                other => Err(ShepherdError::Api {
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
        assert_eq!(opened.workspace, "w1");
        assert_eq!(opened.tab, "w1:t1");
        assert_eq!(opened.pane, "w1:p1");
        assert_eq!(opened.worktree, "/repo/.worktrees/shep-1");
        assert_eq!(opened.branch, "shep/1-task");

        assert_eq!(
            adapter.prompt_text("coder-1", "implement F1").unwrap(),
            PromptOutcome::Delivered
        );
        assert_eq!(
            adapter.prompt_text("coder-1", "BLOCK_ME").unwrap(),
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
}
