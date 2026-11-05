//! Unix domain socket adapter for Shepherd runtime.

use super::{
    deliver_prompt, shepherd_agent_status, shepherd_attach_process, shepherd_focus, shepherd_label,
    shepherd_list_worktrees, shepherd_notify, shepherd_open_agent, shepherd_read_screen,
    shepherd_wait, start_socket_event_subscription, OpenAgentRequest, OpenedAgent, PromptOutcome,
    TerminalAgentStatus, TerminalError, TerminalWaitResult, Terminals,
};
use crate::{
    git::ParsedWorktree,
    shepherd::{PromptWaitOptions, ShepherdClient},
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Adapter for Shepherd 0.9.5 local NDJSON Unix socket API (`shepherd`).
pub struct ShepherdSocket {
    client: ShepherdClient,
    session: String,
    event_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<()>>>>,
}

impl ShepherdSocket {
    pub fn new(client: ShepherdClient, session: impl Into<String>) -> Self {
        let event_rx = start_socket_event_subscription(client.socket());
        Self {
            client,
            session: session.into(),
            event_rx: event_rx.map(|rx| Arc::new(Mutex::new(rx))),
        }
    }

    pub fn connect(session: Option<&str>, autostart: bool) -> Result<Self, TerminalError> {
        let client = ShepherdClient::ensure_session(session, autostart)?;
        let session_name = session.unwrap_or("shepherd").to_owned();
        let event_rx = start_socket_event_subscription(client.socket());
        Ok(Self {
            client,
            session: session_name,
            event_rx: event_rx.map(|rx| Arc::new(Mutex::new(rx))),
        })
    }

    pub fn client(&self) -> &ShepherdClient {
        &self.client
    }
}

pub fn shepherd_attach_args(session: &str, target: Option<&str>, takeover: bool) -> Vec<String> {
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

impl Terminals for ShepherdSocket {
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
        if let Some(rx) = &self.event_rx {
            if let Ok(rx_guard) = rx.lock() {
                match rx_guard.recv_timeout(timeout) {
                    Ok(()) => return Ok(true),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(false),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                }
            }
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
        shepherd_attach_process(&self.session, target, takeover)
    }

    fn focus(&self, target: &str) -> Result<(), TerminalError> {
        shepherd_focus(&self.client, target)
    }
}
