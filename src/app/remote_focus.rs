//! Server-owned state for the cross-host `agent.focus` operation.
//!
//! A remote focus opens one local proxy pane whose screen is fed by the wire
//! transport through `AppEvent::RemoteFocusFrame`. User input stays disabled
//! until the first complete frame and `ControlReady` have both arrived; the
//! proxy owns the remote terminal's dimensions while attached; closing the
//! pane detaches the lease and closes the operation.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::api::schema::{AgentRef, ErrorBody, RemoteControlContext, RemoteFocusState};
use crate::layout::PaneId;
use crate::pane::RemoteProxyChannels;
use crate::terminal::TerminalId;

/// A deterministic failure transport for tests that need to exercise the app
/// operation lifecycle without opening an SSH connection.
#[allow(dead_code)] // Keep the stub available to tests that inject a failing transport.
#[derive(Debug, Default)]
pub(crate) struct StubRemoteFocusTransport;

impl RemoteFocusTransport for StubRemoteFocusTransport {
    fn start(
        &mut self,
        _operation_id: &str,
        _agent_ref: &AgentRef,
        _proxy_pane_id: &str,
        _channels: RemoteProxyChannels,
        _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        Err(ErrorBody {
            code: "host_unreachable".to_owned(),
            message: "remote focus transport is not available yet".to_owned(),
        })
    }
}

/// Terminal operation records remain queryable for five minutes.
pub(crate) const REMOTE_FOCUS_TERMINAL_TTL: Duration = Duration::from_secs(5 * 60);
/// At most sixteen operations may be connecting or active at once.
pub(crate) const REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS: usize = 16;
/// Retained terminal records also have a hard cap so an idle API cannot grow
/// the map without bound before the expiry sweep runs.
const REMOTE_FOCUS_MAX_RETAINED_OPERATIONS: usize = 32;

#[derive(Debug, Clone)]
pub(crate) enum RemoteFocusTransition {
    // Constructed by the Unix wire transport and tests; on Windows no code
    // path reaches Active yet.
    #[cfg_attr(not(unix), allow(dead_code))]
    Active(Box<RemoteControlContext>),
    Failed(ErrorBody),
    Closed,
}

/// The wire client implements this seam. Implementations must return without
/// performing blocking I/O. `Err` reports an immediate start failure; after
/// `Ok`, deliver state changes through `AppEvent::RemoteFocusTransition` and
/// terminal frames through `AppEvent::RemoteFocusFrame`. The transport owns
/// the outbound channel: user input, resize markers (latest dimensions live
/// in the resize slot), and detach. `detach` must not block; it releases the
/// remote lease on a live session and is a no-op otherwise.
pub(crate) trait RemoteFocusTransport: Send {
    /// Resolve admission and connection details from the same live snapshot.
    /// Implementations that do not support remote focus reject every alias.
    fn accepts_remote_host(&self, _host: &str) -> bool {
        false
    }

    /// Replace the connection snapshot. Returned operation IDs are sessions
    /// revoked because their configured connection changed or disappeared.
    fn reload_fleet(&mut self, _fleet: &crate::config::FleetConfig) -> Vec<String> {
        Vec::new()
    }

    /// Observe the last successful fleet inventory without doing I/O. The
    /// transport may use it to bind a configured host alias to the identity
    /// reported by that host's server.
    fn observe_fleet_snapshot(&mut self, _snapshot: &crate::fleet::Snapshot) {}

    /// Report whether a configured remote host has a current, unambiguous
    /// identity observation. This is checked before creating a proxy pane so
    /// the API does not report a connecting operation that cannot be started.
    fn remote_host_ready(&self, _host: &str) -> bool {
        false
    }

    fn start(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        proxy_pane_id: &str,
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody>;

    fn detach(&mut self, _operation_id: &str) {}
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteFocusOperation {
    pub(crate) agent_ref: AgentRef,
    pub(crate) proxy_pane_id: String,
    pub(crate) state: RemoteFocusState,
    pub(crate) context: Option<RemoteControlContext>,
    pub(crate) error: Option<ErrorBody>,
    operation_state: std::sync::Arc<crate::remote::RemoteFocusOperationState>,
    /// Local proxy pane and its terminal, once the pane exists.
    proxy: Option<(PaneId, TerminalId)>,
    first_frame_processed: bool,
    /// The error came from lifecycle reconciliation, not from the transport.
    /// A later detailed failure replaces it.
    error_is_placeholder: bool,
    created_at: Instant,
    operation_sequence: u64,
    completed_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteFocusOperationStart {
    pub(crate) operation_id: String,
    pub(crate) agent_ref: AgentRef,
    pub(crate) proxy_pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteFocusOperationSnapshot {
    pub(crate) operation_id: String,
    pub(crate) agent_ref: AgentRef,
    pub(crate) proxy_pane_id: String,
    pub(crate) state: RemoteFocusState,
    pub(crate) context: Option<RemoteControlContext>,
    pub(crate) error: Option<ErrorBody>,
}

#[derive(Debug)]
pub(crate) struct RemoteFocusOperations {
    operations: HashMap<String, RemoteFocusOperation>,
    /// Reverse index from a proxy pane's terminal to its operation, so the
    /// terminal shutdown path can release the remote lease.
    proxy_by_terminal: HashMap<TerminalId, String>,
    next_operation_id: u64,
}

impl Default for RemoteFocusOperations {
    fn default() -> Self {
        Self {
            operations: HashMap::new(),
            proxy_by_terminal: HashMap::new(),
            next_operation_id: 1,
        }
    }
}

impl RemoteFocusOperations {
    pub(crate) fn begin(
        &mut self,
        agent_ref: AgentRef,
        now: Instant,
    ) -> Result<RemoteFocusOperationStart, ErrorBody> {
        self.reconcile_terminal_operations(now);
        self.prune(now);

        let concurrent = self
            .operations
            .values()
            .filter(|operation| {
                matches!(
                    operation.state,
                    RemoteFocusState::Connecting | RemoteFocusState::Active
                )
            })
            .count();
        if concurrent >= REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS {
            return Err(ErrorBody {
                code: "operation_limit_reached".into(),
                message: format!(
                    "remote focus operation limit reached ({REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS})"
                ),
            });
        }

        if self.operations.len() >= REMOTE_FOCUS_MAX_RETAINED_OPERATIONS {
            let oldest_terminal = self
                .operations
                .iter()
                .filter(|(_, operation)| {
                    matches!(
                        operation.state,
                        RemoteFocusState::Failed | RemoteFocusState::Closed
                    )
                })
                .min_by_key(|(_, operation)| operation.created_at)
                .map(|(operation_id, _)| operation_id.clone());
            if let Some(operation_id) = oldest_terminal {
                self.operations.remove(&operation_id);
                self.proxy_by_terminal
                    .retain(|_, candidate| candidate != &operation_id);
            } else {
                return Err(ErrorBody {
                    code: "operation_limit_reached".into(),
                    message: "remote focus operation retention limit reached".into(),
                });
            }
        }

        let (operation_id, operation_sequence) = self.next_operation_id();
        let proxy_pane_id = format!("remote-focus-proxy-{operation_id}");
        self.operations.insert(
            operation_id.clone(),
            RemoteFocusOperation {
                agent_ref: agent_ref.clone(),
                proxy_pane_id: proxy_pane_id.clone(),
                state: RemoteFocusState::Connecting,
                context: None,
                error: None,
                operation_state: crate::remote::RemoteFocusOperationState::new(),
                proxy: None,
                first_frame_processed: false,
                error_is_placeholder: false,
                created_at: now,
                operation_sequence,
                completed_at: None,
            },
        );
        Ok(RemoteFocusOperationStart {
            operation_id,
            agent_ref,
            proxy_pane_id,
        })
    }

    pub(crate) fn operation_state(
        &self,
        operation_id: &str,
    ) -> Option<std::sync::Arc<crate::remote::RemoteFocusOperationState>> {
        self.operations
            .get(operation_id)
            .map(|operation| std::sync::Arc::clone(&operation.operation_state))
    }

    pub(crate) fn snapshot(
        &mut self,
        operation_id: &str,
        now: Instant,
    ) -> Result<RemoteFocusOperationSnapshot, ErrorBody> {
        self.reconcile_terminal_operations(now);
        self.prune(now);
        let Some(operation) = self.operations.get(operation_id) else {
            return Err(ErrorBody {
                code: "unknown_operation".into(),
                message: format!("unknown remote focus operation: {operation_id}"),
            });
        };
        Ok(RemoteFocusOperationSnapshot {
            operation_id: operation_id.to_string(),
            agent_ref: operation.agent_ref.clone(),
            proxy_pane_id: operation.proxy_pane_id.clone(),
            state: operation.state,
            context: operation.context.clone(),
            error: operation.error.clone(),
        })
    }

    /// Projects the transport-owned terminal bit into the app-owned record.
    /// Failure events still carry the detailed error and drive normal UI
    /// updates, but delivery of one is not required for lifecycle correctness.
    pub(crate) fn reconcile_terminal_operations(&mut self, now: Instant) -> Vec<String> {
        if self.operations.is_empty() {
            return Vec::new();
        }
        let mut reconciled = Vec::new();
        for (operation_id, operation) in &mut self.operations {
            if operation.operation_state.is_terminal()
                && matches!(
                    operation.state,
                    RemoteFocusState::Connecting | RemoteFocusState::Active
                )
            {
                operation.state = RemoteFocusState::Failed;
                operation.context = None;
                operation.error = Some(ErrorBody {
                    code: "connection_lost".into(),
                    message: "remote focus connection terminated".into(),
                });
                operation.error_is_placeholder = true;
                operation.completed_at = Some(now);
                reconciled.push(operation_id.clone());
            }
        }
        reconciled
    }

    pub(crate) fn transition(
        &mut self,
        operation_id: &str,
        transition: RemoteFocusTransition,
        now: Instant,
    ) -> bool {
        let Some(operation) = self.operations.get_mut(operation_id) else {
            return false;
        };
        if matches!(
            operation.state,
            RemoteFocusState::Failed | RemoteFocusState::Closed
        ) {
            if operation.state == RemoteFocusState::Failed && operation.error_is_placeholder {
                if let RemoteFocusTransition::Failed(error) = transition {
                    operation.error = Some(error);
                    operation.error_is_placeholder = false;
                    return true;
                }
            }
            return false;
        }
        if operation.operation_state.is_terminal()
            && matches!(&transition, RemoteFocusTransition::Active(_))
        {
            return false;
        }

        match transition {
            RemoteFocusTransition::Active(context) => {
                if matches!(
                    operation.state,
                    RemoteFocusState::Connecting | RemoteFocusState::Active
                ) {
                    operation.state = RemoteFocusState::Active;
                    operation.context = Some(*context);
                }
            }
            RemoteFocusTransition::Failed(error) => {
                operation.operation_state.terminate();
                operation.state = RemoteFocusState::Failed;
                operation.context = None;
                operation.error = Some(error);
                operation.error_is_placeholder = false;
                operation.completed_at = Some(now);
            }
            RemoteFocusTransition::Closed => {
                operation.operation_state.terminate();
                operation.state = RemoteFocusState::Closed;
                operation.context = None;
                operation.error = None;
                operation.completed_at = Some(now);
            }
        }
        true
    }

    /// Binds the local proxy pane to the operation and publishes the real
    /// public pane id in place of the placeholder.
    pub(crate) fn attach_proxy(
        &mut self,
        operation_id: &str,
        pane_id: PaneId,
        terminal_id: TerminalId,
        public_pane_id: String,
    ) {
        let Some(operation) = self.operations.get_mut(operation_id) else {
            return;
        };
        operation.proxy = Some((pane_id, terminal_id.clone()));
        operation.proxy_pane_id = public_pane_id;
        self.proxy_by_terminal
            .insert(terminal_id, operation_id.to_string());
    }

    pub(crate) fn proxy_location(&self, operation_id: &str) -> Option<(PaneId, TerminalId)> {
        self.operations
            .get(operation_id)
            .and_then(|operation| operation.proxy.clone())
    }

    pub(crate) fn proxy_pane_for_agent(
        &self,
        agent_ref: &AgentRef,
        pane_exists: impl Fn(PaneId) -> bool,
    ) -> Option<PaneId> {
        self.operations
            .iter()
            .filter(|(_, operation)| {
                operation.agent_ref == *agent_ref
                    && matches!(
                        operation.state,
                        RemoteFocusState::Connecting | RemoteFocusState::Active
                    )
                    && !operation.operation_state.is_terminal()
            })
            .filter_map(|(operation_id, operation)| {
                let pane_id = operation.proxy.as_ref().map(|(pane_id, _)| *pane_id)?;
                pane_exists(pane_id).then_some((operation_id, operation, pane_id))
            })
            .max_by(|(_, left, _), (_, right, _)| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.operation_sequence.cmp(&right.operation_sequence))
            })
            .map(|(_, _, pane_id)| pane_id)
    }

    pub(crate) fn agent_ref_for_proxy_pane(&self, pane_id: PaneId) -> Option<&AgentRef> {
        self.operations.values().find_map(|operation| {
            (operation
                .proxy
                .as_ref()
                .is_some_and(|(proxy, _)| *proxy == pane_id)
                && matches!(
                    operation.state,
                    RemoteFocusState::Connecting | RemoteFocusState::Active
                ))
            .then_some(&operation.agent_ref)
        })
    }

    fn proxy_by_terminal_id(&self, terminal_id: &TerminalId) -> Option<PaneId> {
        self.proxy_by_terminal
            .get(terminal_id)
            .and_then(|operation_id| self.proxy_location(operation_id))
            .map(|(pane_id, _)| pane_id)
    }

    pub(crate) fn agent_ref(&self, operation_id: &str) -> Option<&AgentRef> {
        self.operations
            .get(operation_id)
            .map(|operation| &operation.agent_ref)
    }

    /// Removes the reverse index entry for a shutting-down proxy terminal and
    /// returns its operation.
    pub(crate) fn take_proxy_terminal(&mut self, terminal_id: &TerminalId) -> Option<String> {
        self.proxy_by_terminal.remove(terminal_id)
    }

    /// Records that one complete frame reached the proxy screen. Returns true
    /// when input may now be enabled: the operation is active (ControlReady
    /// arrived) and a complete frame was processed.
    pub(crate) fn mark_frame_processed(
        &mut self,
        operation_id: &str,
        frame_complete: bool,
    ) -> bool {
        let Some(operation) = self.operations.get_mut(operation_id) else {
            return false;
        };
        if operation.operation_state.is_terminal() {
            return false;
        }
        if frame_complete {
            operation.first_frame_processed = true;
        }
        operation.state == RemoteFocusState::Active && operation.first_frame_processed
    }

    /// Whether the operation is active and at least one complete frame has
    /// been processed, the two halves of the input gate.
    pub(crate) fn input_gate_open(&self, operation_id: &str) -> bool {
        self.operations.get(operation_id).is_some_and(|operation| {
            !operation.operation_state.is_terminal()
                && operation.state == RemoteFocusState::Active
                && operation.first_frame_processed
        })
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.operations.len()
    }

    fn next_operation_id(&mut self) -> (String, u64) {
        loop {
            let number = self.next_operation_id;
            self.next_operation_id = self.next_operation_id.saturating_add(1);
            let operation_id = format!("remote-focus-{number}");
            if !self.operations.contains_key(&operation_id) {
                return (operation_id, number);
            }
        }
    }

    fn prune(&mut self, now: Instant) {
        self.operations.retain(|_, operation| {
            operation.completed_at.is_none_or(|completed_at| {
                now.checked_duration_since(completed_at)
                    .is_none_or(|age| age < REMOTE_FOCUS_TERMINAL_TTL)
            })
        });
        self.proxy_by_terminal
            .retain(|_, operation_id| self.operations.contains_key(operation_id));
    }
}

/// The compact identity line a proxy pane shows once `ControlReady` arrives:
/// configured host alias plus `agent`, remote user, cwd, tty, and foreground
/// process.
pub(crate) fn remote_proxy_identity_line(
    configured_host: &str,
    context: &RemoteControlContext,
) -> String {
    format!(
        "{}::{} · {} · {} · {} · {}",
        configured_host,
        context.pane_id,
        context.user,
        context.cwd,
        context.tty,
        context.foreground_process.name
    )
}

const REMOTE_PROXY_PENDING_LABEL: &str = "Remote focus (connecting)";

impl crate::app::App {
    #[cfg(unix)]
    pub(crate) fn remote_control_context(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody> {
        self.remote_control_context_unix(agent_ref)
    }

    #[cfg(unix)]
    fn remote_control_context_unix(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody> {
        if agent_ref.host != self.state.agent_host_name {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent reference host does not match this server".into(),
            });
        }
        if self.fleet_marks_windowless(agent_ref) {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {} is a windowless run", agent_ref),
            });
        }
        let resolved = self
            .resolve_agent_target(&agent_ref.agent)
            .map_err(|_| ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} no longer resolves to a pane", agent_ref),
            })?;
        let Some(workspace) = self.state.workspaces.get(resolved.ws_idx) else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} workspace no longer exists", agent_ref),
            });
        };
        let Some(tab_idx) = workspace.find_tab_index_for_pane(resolved.pane_id) else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} tab no longer exists", agent_ref),
            });
        };
        let Some(pane) = workspace.pane_state(resolved.pane_id) else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} pane no longer exists", agent_ref),
            });
        };
        let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id) else {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {} has no terminal runtime", agent_ref),
            });
        };
        let Some(runtime) = self.terminal_runtimes.get(&pane.attached_terminal_id) else {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {} is windowless", agent_ref),
            });
        };
        let Some(known_agent) = terminal.effective_known_agent() else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} is not detected in its pane", agent_ref),
            });
        };
        if terminal.managed_agent_launch_pending() && !terminal.managed_agent_control_ready() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent launch is still pending".into(),
            });
        }
        if !terminal.managed_agent_control_ready() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent is not interactive_ready".into(),
            });
        }
        let Some(user) = crate::platform::effective_user_name() else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "effective remote user is unavailable".into(),
            });
        };
        let Some(tty) = runtime
            .tty_name()
            .and_then(|tty| tty.to_str().map(str::to_owned))
            .filter(|tty| !tty.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "terminal tty is unavailable".into(),
            });
        };
        let Some(shell_pid) = runtime.child_pid() else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "terminal shell process is unavailable".into(),
            });
        };
        let Some(cwd) = crate::platform::process_cwd(shell_pid)
            .and_then(|cwd| cwd.to_str().map(str::to_owned))
            .filter(|cwd| !cwd.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "terminal cwd is unavailable".into(),
            });
        };
        let Some(job) = crate::detect::foreground_job(shell_pid) else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process is unavailable".into(),
            });
        };
        let Some(process) = job
            .processes
            .iter()
            .find(|process| process.pid == job.process_group_id)
            .or_else(|| job.processes.first())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process identity is unavailable".into(),
            });
        };
        if !crate::app::agents::runtime_hosts_agent_in_job(&job, known_agent) {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: format!(
                    "agent {} is no longer the live foreground process",
                    agent_ref
                ),
            });
        }
        let Some(foreground_cwd) = crate::platform::process_cwd(process.pid)
            .and_then(|cwd| cwd.to_str().map(str::to_owned))
            .filter(|cwd| !cwd.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground cwd is unavailable".into(),
            });
        };
        let process_cwd = foreground_cwd.clone();
        let Some(argv) = process.argv.clone() else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process argv is unavailable".into(),
            });
        };
        if argv.is_empty() || process.name.trim().is_empty() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process identity is incomplete".into(),
            });
        }
        let workspace_id = self.public_workspace_id(resolved.ws_idx);
        let Some(tab_id) = self.public_tab_id(resolved.ws_idx, tab_idx) else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "tab identity is unavailable".into(),
            });
        };
        let Some(pane_id) = self.public_pane_id(resolved.ws_idx, resolved.pane_id) else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "pane identity is unavailable".into(),
            });
        };
        let state_change_seq = terminal
            .last_agent_state_change_seq
            .ok_or_else(|| ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent state epoch is unavailable".into(),
            })?;
        let human_draft = self
            .state
            .pending_human_drafts
            .contains_key(&resolved.pane_id);
        if human_draft {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "a human input draft is pending".into(),
            });
        }
        Ok(RemoteControlContext {
            host: self.state.agent_host_name.clone(),
            user,
            workspace_id,
            tab_id,
            pane_id,
            terminal_id: terminal.id.to_string(),
            cwd: cwd.to_owned(),
            foreground_cwd,
            tty,
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: process.pid,
                process_group_id: job.process_group_id,
                name: process.name.clone(),
                argv,
                cwd: process_cwd,
            },
            detected_agent: crate::detect::agent_label(known_agent).to_owned(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq,
            revision: terminal.revision,
            context_epoch: terminal.revision,
        })
    }

    pub(crate) fn fleet_marks_windowless(&self, agent_ref: &AgentRef) -> bool {
        self.state.fleet_snapshot.hosts.iter().any(|host| {
            host.entries.iter().any(|entry| {
                entry.agent_ref == *agent_ref
                    && entry.source == crate::fleet::EvidenceSource::RunState
            })
        })
    }

    pub(crate) fn start_remote_focus_operation(
        &mut self,
        agent_ref: AgentRef,
    ) -> Result<RemoteFocusOperationStart, ErrorBody> {
        self.reconcile_remote_focus_lifecycle();
        // Windowless runs have no remote PTY to display or control.
        if self.fleet_marks_windowless(&agent_ref) {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {agent_ref} is a windowless run"),
            });
        }
        let mut started = self
            .remote_focus_operations
            .begin(agent_ref.clone(), Instant::now())?;
        let operation_state = self
            .remote_focus_operations
            .operation_state(&started.operation_id)
            .expect("operation state exists after begin");
        let channels = match self.create_remote_proxy_pane(&agent_ref.host, operation_state) {
            Ok((pane_id, terminal_id, public_pane_id, channels)) => {
                started.proxy_pane_id = public_pane_id.clone();
                self.remote_focus_operations.attach_proxy(
                    &started.operation_id,
                    pane_id,
                    terminal_id,
                    public_pane_id,
                );
                self.state.remote_focus_proxy_panes.insert(pane_id);
                channels
            }
            Err(error) => {
                self.apply_remote_focus_transition(
                    &started.operation_id,
                    RemoteFocusTransition::Failed(error),
                );
                return Ok(started);
            }
        };
        let event_tx = self.event_tx.clone();
        if let Err(error) = self.remote_focus_transport.start(
            &started.operation_id,
            &agent_ref,
            &started.proxy_pane_id,
            channels,
            event_tx,
        ) {
            self.apply_remote_focus_transition(
                &started.operation_id,
                RemoteFocusTransition::Failed(error),
            );
        }
        Ok(started)
    }

    /// Opens one local tab holding a single proxy pane in the active
    /// workspace and focuses it. No SSH, wire I/O, or process work happens
    /// here; the pane is a local terminal surface until the transport proves
    /// the remote control lease.
    fn create_remote_proxy_pane(
        &mut self,
        remote_host: &str,
        operation_state: std::sync::Arc<crate::remote::RemoteFocusOperationState>,
    ) -> Result<(PaneId, TerminalId, String, RemoteProxyChannels), ErrorBody> {
        let fleet_ws_idx = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.is_fleet);
        let cwd = fleet_ws_idx
            .or(self.state.active)
            .or_else(|| {
                (self.state.selected < self.state.workspaces.len()).then_some(self.state.selected)
            })
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
            .map(|workspace| workspace.identity_cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| ErrorBody {
                code: "host_unreachable".into(),
                message: "no working directory is available for the remote focus proxy pane".into(),
            })?;
        let (rows, cols) = self.state.estimate_pane_size();
        let pane_id = PaneId::alloc();
        let terminal_id = TerminalId::alloc();
        let (runtime, channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            pane_id,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.render_notify.clone(),
            self.render_dirty.clone(),
            operation_state,
        )
        .map_err(|error| ErrorBody {
            code: "host_unreachable".into(),
            message: format!("failed to create remote focus proxy pane: {error}"),
        })?;
        let mut terminal = crate::terminal::TerminalState::new(terminal_id.clone(), cwd.clone());
        // Until ControlReady, the client-supplied agent_ref is not an
        // identity claim. Keep the tab visibly provisional instead of
        // allowing chrome to fall back to an unlabeled numeric tab.
        terminal.manual_label = Some(REMOTE_PROXY_PENDING_LABEL.to_owned());
        terminal.remote_proxy_host = Some(remote_host.to_owned());
        self.state.terminals.insert(terminal_id.clone(), terminal);
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let moved = crate::workspace::MovedPane {
            pane_id,
            pane_state: crate::pane::PaneState::new(terminal_id.clone()),
        };
        let (ws_idx, tab_idx, created_workspace) = if let Some(ws_idx) = fleet_ws_idx {
            let workspace = &mut self.state.workspaces[ws_idx];
            if workspace.custom_name.is_none() {
                workspace.custom_name = Some("Fleet".to_owned());
            }
            let tab_idx = workspace.create_tab_from_existing_pane(
                moved,
                None,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
            (ws_idx, tab_idx, false)
        } else {
            let mut workspace = crate::workspace::Workspace::from_existing_pane(
                Some("Fleet".to_owned()),
                None,
                cwd,
                moved,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
            workspace.is_fleet = true;
            self.state.workspaces.push(workspace);
            (self.state.workspaces.len() - 1, 0, true)
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        if created_workspace {
            let workspace_id = self.state.workspaces[ws_idx].id.clone();
            crate::logging::workspace_created(&workspace_id, pane_id.raw());
            self.emit_workspace_open_events(ws_idx);
        } else {
            self.emit_tab_created_events(ws_idx, tab_idx);
        }
        self.state.mark_session_dirty();
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return Err(ErrorBody {
                code: "host_unreachable".into(),
                message: "remote focus proxy pane has no public identity".into(),
            });
        };
        Ok((pane_id, terminal_id, public_pane_id, channels))
    }

    pub(crate) fn remote_focus_status(
        &mut self,
        operation_id: &str,
    ) -> Result<RemoteFocusOperationSnapshot, ErrorBody> {
        self.reconcile_remote_focus_lifecycle();
        let snapshot = self
            .remote_focus_operations
            .snapshot(operation_id, Instant::now())?;
        if matches!(
            snapshot.state,
            RemoteFocusState::Failed | RemoteFocusState::Closed
        ) {
            self.close_remote_proxy_pane(operation_id);
        }
        Ok(snapshot)
    }

    pub(crate) fn reconcile_remote_focus_lifecycle(&mut self) -> bool {
        let terminal_operations = self
            .remote_focus_operations
            .reconcile_terminal_operations(Instant::now());
        let changed = !terminal_operations.is_empty();
        for operation_id in terminal_operations {
            self.close_remote_proxy_pane(&operation_id);
        }
        changed
    }

    pub(crate) fn apply_remote_focus_transition(
        &mut self,
        operation_id: &str,
        transition: RemoteFocusTransition,
    ) {
        let activation = match &transition {
            RemoteFocusTransition::Active(context) => Some((**context).clone()),
            _ => None,
        };
        let applied =
            self.remote_focus_operations
                .transition(operation_id, transition, Instant::now());
        if !applied {
            return;
        }
        let state = self
            .remote_focus_operations
            .snapshot(operation_id, Instant::now())
            .map(|snapshot| snapshot.state);
        match (state, activation) {
            (Ok(RemoteFocusState::Active), Some(context)) => {
                self.activate_remote_proxy(operation_id, &context);
            }
            (Ok(RemoteFocusState::Failed | RemoteFocusState::Closed), _) => {
                self.close_remote_proxy_pane(operation_id);
            }
            _ => {}
        }
    }

    /// Applies the authoritative control context to the proxy pane: the
    /// identity line becomes the pane label, and the input gate opens when
    /// the first complete frame has also arrived.
    fn activate_remote_proxy(&mut self, operation_id: &str, context: &RemoteControlContext) {
        let Some((_pane_id, terminal_id)) =
            self.remote_focus_operations.proxy_location(operation_id)
        else {
            return;
        };
        let configured_host = self
            .remote_focus_operations
            .agent_ref(operation_id)
            .map(|agent_ref| agent_ref.host.as_str())
            .unwrap_or(context.host.as_str());
        let identity = remote_proxy_identity_line(configured_host, context);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.manual_label = Some(identity);
            terminal.cwd = std::path::PathBuf::from(&context.cwd);
        }
        if self.remote_focus_operations.input_gate_open(operation_id) {
            if let Some(runtime) = self.terminal_runtimes.get(&terminal_id) {
                runtime.set_remote_proxy_input_enabled(true);
            }
        }
    }

    /// Feeds one complete remote terminal frame into the proxy pane. Returns
    /// true when the frame painted new content and the client should redraw.
    pub(crate) fn apply_remote_focus_frame(
        &mut self,
        operation_id: &str,
        frame: &crate::protocol::TerminalFrame,
    ) -> bool {
        let Some((_pane_id, terminal_id)) =
            self.remote_focus_operations.proxy_location(operation_id)
        else {
            return false;
        };
        let Some(runtime) = self.terminal_runtimes.get(&terminal_id) else {
            return false;
        };
        let painted = runtime.process_remote_frame(&frame.bytes);
        if self
            .remote_focus_operations
            .mark_frame_processed(operation_id, frame.full)
        {
            runtime.set_remote_proxy_input_enabled(true);
        }
        painted
    }

    /// Closes the proxy pane for an operation that failed or closed. Closing
    /// the pane shuts its runtime down, which releases the remote lease
    /// through `detach_remote_proxy_for_terminal`.
    fn close_remote_proxy_pane(&mut self, operation_id: &str) {
        let Some((pane_id, terminal_id)) =
            self.remote_focus_operations.proxy_location(operation_id)
        else {
            return;
        };
        self.state.remote_focus_proxy_panes.remove(&pane_id);
        let Some((ws_idx, _)) = self.find_pane(pane_id) else {
            return;
        };
        let should_close_workspace = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|workspace| workspace.remove_pane(pane_id));
        self.state.remove_plugin_pane_records([pane_id]);
        self.state.mark_session_dirty();
        if should_close_workspace {
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
        } else {
            self.state.remove_unattached_terminal_ids([terminal_id]);
        }
        self.shutdown_detached_terminal_runtimes();
    }

    /// Releases the remote lease when a proxy pane's terminal shuts down,
    /// whatever local close path got there (pane, tab, workspace close, or
    /// operation failure). The remote server also rejects a second controller
    /// for the terminal, so the lease must go back exactly once.
    pub(crate) fn detach_remote_proxy_for_terminal(&mut self, terminal_id: &TerminalId) {
        if let Some(pane_id) = self
            .remote_focus_operations
            .proxy_by_terminal_id(terminal_id)
        {
            self.state.remote_focus_proxy_panes.remove(&pane_id);
        }
        let Some(operation_id) = self
            .remote_focus_operations
            .take_proxy_terminal(terminal_id)
        else {
            return;
        };
        self.remote_focus_transport.detach(&operation_id);
        self.remote_focus_operations.transition(
            &operation_id,
            RemoteFocusTransition::Closed,
            Instant::now(),
        );
    }
}

#[cfg(unix)]
impl crate::server::remote_control::RemoteControlContextProvider for crate::app::App {
    fn fresh_remote_control_context(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody> {
        self.remote_control_context(agent_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::ProxyOutbound;

    #[derive(Debug, Default)]
    struct RecordingState {
        started: Vec<(String, AgentRef)>,
        detached: Vec<String>,
        outbound: Option<tokio::sync::mpsc::Receiver<ProxyOutbound>>,
    }

    #[derive(Debug, Clone, Default)]
    struct RecordingTransport {
        state: std::sync::Arc<std::sync::Mutex<RecordingState>>,
    }

    impl RemoteFocusTransport for RecordingTransport {
        fn start(
            &mut self,
            operation_id: &str,
            agent_ref: &AgentRef,
            _proxy_pane_id: &str,
            channels: RemoteProxyChannels,
            _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        ) -> Result<(), ErrorBody> {
            let mut state = self.state.lock().expect("recording state lock");
            state
                .started
                .push((operation_id.to_string(), agent_ref.clone()));
            state.outbound = Some(channels.outbound_rx);
            Ok(())
        }

        fn detach(&mut self, operation_id: &str) {
            self.state
                .lock()
                .expect("recording state lock")
                .detached
                .push(operation_id.to_string());
        }
    }

    fn proxy_app() -> (
        crate::app::App,
        std::sync::Arc<std::sync::Mutex<RecordingState>>,
    ) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("local")];
        app.state.workspaces[0].is_fleet = true;
        app.state.workspaces[0].custom_name = Some("Fleet".to_owned());
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(crate::app::Mode::Terminal);
        app.state.agent_host_name = "local".into();
        let recording = std::sync::Arc::new(std::sync::Mutex::new(RecordingState::default()));
        app.remote_focus_transport = Box::new(RecordingTransport {
            state: std::sync::Arc::clone(&recording),
        });
        (app, recording)
    }

    fn proxy_terminal_id(app: &crate::app::App, operation_id: &str) -> TerminalId {
        app.remote_focus_operations
            .proxy_location(operation_id)
            .expect("proxy location")
            .1
    }

    #[test]
    fn fleet_workspace_remote_focus_proxy_uses_a_dedicated_workspace() {
        let (mut app, _recording) = proxy_app();
        app.state.workspaces[0].is_fleet = false;
        app.state.workspaces[0].custom_name = Some("local".to_owned());
        let local_tab_count = app.state.workspaces[0].tabs.len();

        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");

        let fleet_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.is_fleet)
            .expect("remote focus creates Fleet workspace");
        assert_eq!(fleet_idx, 1);
        assert_eq!(app.state.workspaces[0].tabs.len(), local_tab_count);
        assert_eq!(
            app.state.workspaces[fleet_idx].custom_name.as_deref(),
            Some("Fleet")
        );
        let (pane_id, _) = app
            .remote_focus_operations
            .proxy_location(&started.operation_id)
            .expect("proxy location");
        assert!(app.state.workspaces[fleet_idx]
            .pane_state(pane_id)
            .is_some());
        assert!(app.state.active == Some(fleet_idx));
        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "connection_lost".to_owned(),
                message: "test cleanup".to_owned(),
            }),
        );
        app.shutdown_detached_terminal_runtimes();
    }

    fn full_frame(bytes: &[u8]) -> crate::protocol::TerminalFrame {
        crate::protocol::TerminalFrame {
            seq: 1,
            width: 80,
            height: 24,
            full: true,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn start_opens_one_focused_proxy_tab_with_a_real_public_pane_id() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");

        assert_eq!(recording.lock().expect("lock").started.len(), 1);
        let workspace = &app.state.workspaces[0];
        assert_eq!(workspace.tabs.len(), 2);
        let proxy_tab = &workspace.tabs[workspace.active_tab_index()];
        assert!(
            proxy_tab.custom_name.is_none(),
            "the requested agent_ref is client-supplied identity; tab chrome \
             must fall back to the pane label until the server context arrives"
        );
        let (pane_id, terminal_id) = app
            .remote_focus_operations
            .proxy_location(&started.operation_id)
            .expect("proxy bound to operation");
        assert!(proxy_tab.panes.contains_key(&pane_id));
        assert_eq!(proxy_tab.layout.focused(), pane_id);
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("proxy terminal state");
        assert!(
            terminal.manual_label.as_deref() == Some(REMOTE_PROXY_PENDING_LABEL),
            "the connecting proxy must show a provisional label"
        );
        assert_eq!(terminal.remote_proxy_host.as_deref(), Some("buildbox"));
        assert!(!terminal
            .manual_label
            .as_deref()
            .is_some_and(|label| label.contains("buildbox::w1:p3")));
        assert_eq!(
            workspace
                .active_tab_display_name_from(&app.state.terminals)
                .as_deref(),
            Some(REMOTE_PROXY_PENDING_LABEL)
        );
        assert_eq!(
            started.proxy_pane_id,
            app.public_pane_id(0, pane_id).expect("public pane id")
        );
        assert!(
            started.proxy_pane_id.contains(":p"),
            "the placeholder id is replaced by the real public pane id: {}",
            started.proxy_pane_id
        );

        // Input stays refused until the first frame and ControlReady.
        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime");
        assert!(runtime.is_remote_proxy());
        assert!(
            runtime
                .try_send_bytes(bytes::Bytes::from_static(b"early"))
                .is_err(),
            "keystrokes before ControlReady are refused, not buffered"
        );
        let mut recording = recording.lock().expect("lock");
        assert!(
            recording
                .outbound
                .as_mut()
                .expect("outbound channel")
                .try_recv()
                .is_err(),
            "refused keystrokes never reach the transport"
        );
    }

    #[test]
    fn connecting_proxy_deduplicates_source_and_remote_row_refocuses_it() {
        let (mut app, recording) = proxy_app();
        let source = agent_ref();
        let snapshot = crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: source.host.clone(),
                target: source.host.clone(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                entries: vec![crate::fleet::FleetRow::test_agent_row(
                    &source.host,
                    &source.agent,
                )],
            }],
            ..crate::fleet::Snapshot::default()
        };
        app.state.remote_agent_panel_entries =
            crate::ui::remote_agent_panel_entries(&snapshot, false);
        app.state.collapsed_sidebar_groups.remove("repo:Fleet");

        let started = app
            .start_remote_focus_operation(source.clone())
            .expect("operation starts");
        let (proxy_pane, _) = app
            .remote_focus_operations
            .proxy_location(&started.operation_id)
            .expect("connecting proxy location");
        assert_eq!(
            app.remote_focus_operations.agent_ref(&started.operation_id),
            Some(&source),
            "dedup identity starts with the proxy, before ControlReady"
        );
        assert!(app.state.remote_focus_proxy_panes.contains(&proxy_pane));
        let rows = crate::ui::sidebar_rows(&app.state);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, crate::ui::SidebarRow::RemoteAgent { entry, .. } if entry.agent_ref == source))
                .count(),
            1
        );
        assert!(!rows.iter().any(|row| matches!(
            row,
            crate::ui::SidebarRow::Tab { entry, .. }
                | crate::ui::SidebarRow::Agent { entry, .. }
                if entry
                    .local_target()
                    .is_some_and(|target| target.pane_id == proxy_pane)
        )));

        let original = app.state.workspaces[0].tabs[0].root_pane;
        app.state.focus_pane_in_workspace(0, original);
        app.open_fleet_host_focused(&source.host, Some(&source.agent));
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        let workspace = &app.state.workspaces[0];
        assert_eq!(
            workspace.tabs[workspace.active_tab_index()]
                .layout
                .focused(),
            proxy_pane
        );
        assert_eq!(recording.lock().expect("lock").started.len(), 1);
    }

    #[test]
    fn input_gate_opens_only_after_control_ready_and_first_frame() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);

        // A frame before ControlReady paints but does not open the gate.
        assert!(app.apply_remote_focus_frame(
            &started.operation_id,
            &full_frame(b"\x1b[1;1Hremote question"),
        ));
        {
            let runtime = app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("proxy runtime");
            assert!(runtime.visible_text().contains("remote question"));
            assert!(
                runtime
                    .try_send_bytes(bytes::Bytes::from_static(b"too early"))
                    .is_err(),
                "a frame without ControlReady keeps input disabled"
            );
        }

        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        assert!(app
            .remote_focus_operations
            .input_gate_open(&started.operation_id));
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(bytes::Bytes::from_static(b"answer"))
            .expect("input flows after ControlReady and first frame");
        let mut recording = recording.lock().expect("lock");
        assert_eq!(
            recording
                .outbound
                .as_mut()
                .expect("outbound channel")
                .try_recv(),
            Ok(ProxyOutbound::Input(bytes::Bytes::from_static(b"answer")))
        );

        // The authoritative context becomes the compact identity line.
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("proxy terminal");
        assert_eq!(
            terminal.manual_label.as_deref(),
            Some("buildbox::w1:p3 · operator · /work/repo · /dev/pts/4 · agent")
        );
    }

    #[test]
    fn remote_frame_marks_the_proxy_dirty_without_full_render_impact() {
        let (mut app, _recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );

        assert!(!app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::RemoteFocusFrame {
                operation_id: started.operation_id.clone(),
                frame: Box::new(full_frame(b"remote frame")),
            }
        ));
        assert_eq!(
            app.handle_internal_event(crate::events::AppEvent::RemoteFocusFrame {
                operation_id: started.operation_id,
                frame: Box::new(full_frame(b"remote frame 2")),
            }),
            Some(false)
        );
    }

    #[test]
    fn connection_loss_keeps_late_frames_and_context_updates_terminal() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let operation_id = started.operation_id.clone();
        let terminal_id = proxy_terminal_id(&app, &operation_id);

        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        app.apply_remote_focus_frame(&operation_id, &full_frame(b"ready"));
        assert!(app.remote_focus_operations.input_gate_open(&operation_id));

        let operation_state = app
            .remote_focus_operations
            .operation_state(&operation_id)
            .expect("operation state");
        assert!(
            operation_state.terminate(),
            "loss terminates the operation once"
        );
        assert!(
            !operation_state.terminate(),
            "repeated loss cannot release the operation twice"
        );
        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime");
        runtime.set_remote_proxy_input_enabled(false);
        assert!(
            !runtime.set_remote_proxy_input_enabled(true),
            "a terminal operation cannot reopen its pane input"
        );

        // This frame was queued before the transport observed the loss, but
        // the app delivers it after the shared operation state is terminal.
        let queued_frame = full_frame(b"late frame");
        app.apply_remote_focus_frame(&operation_id, &queued_frame);
        assert!(!app.remote_focus_operations.input_gate_open(&operation_id));
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(bytes::Bytes::from_static(b"after loss"))
            .is_err());

        let snapshot = app
            .remote_focus_status(&operation_id)
            .expect("operation status");
        assert_eq!(snapshot.context, None);
        assert_eq!(snapshot.state, RemoteFocusState::Failed);
        assert!(!app.remote_focus_operations.input_gate_open(&operation_id));
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_none(),
            "status reconciliation closes the terminal proxy"
        );

        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "connection_lost".into(),
                message: "stream ended".into(),
            }),
        );
        assert_eq!(
            recording.lock().expect("recording lock").detached,
            vec![operation_id]
        );
    }

    #[test]
    fn dropped_failure_event_reconciles_state_closes_pane_and_allows_retry() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let operation_id = started.operation_id.clone();
        let (pane_id, terminal_id) = app
            .remote_focus_operations
            .proxy_location(&operation_id)
            .expect("proxy location");

        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        app.apply_remote_focus_frame(&operation_id, &full_frame(b"ready"));
        let operation_state = app
            .remote_focus_operations
            .operation_state(&operation_id)
            .expect("operation state");
        assert!(operation_state.terminate(), "loss terminates the operation");

        // No RemoteFocusTransition::Failed is delivered. The shared terminal
        // bit is the only loss signal available to the app here.
        let snapshot = app
            .remote_focus_status(&operation_id)
            .expect("terminal operation remains queryable");
        assert_eq!(snapshot.state, RemoteFocusState::Failed);
        assert_eq!(
            snapshot.error.as_ref().map(|error| error.code.as_str()),
            Some("connection_lost")
        );
        assert!(
            app.find_pane(pane_id).is_none(),
            "terminal loss closes the proxy pane"
        );
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_none(),
            "terminal loss shuts down the proxy runtime"
        );
        assert_eq!(
            recording.lock().expect("recording lock").detached,
            vec![operation_id.clone()]
        );

        let retry = app
            .start_remote_focus_operation(agent_ref())
            .expect("same host and agent can be focused again");
        assert_ne!(retry.operation_id, operation_id);
        assert_eq!(retry.agent_ref, agent_ref());
        assert_eq!(
            app.remote_focus_status(&retry.operation_id)
                .expect("retry status")
                .state,
            RemoteFocusState::Connecting
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
    }

    #[test]
    fn detailed_failure_replaces_reconciliation_placeholder_without_reopening() {
        let (mut app, _recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let operation_id = started.operation_id.clone();

        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        app.apply_remote_focus_frame(&operation_id, &full_frame(b"ready"));
        assert!(app.remote_focus_operations.input_gate_open(&operation_id));

        let operation_state = app
            .remote_focus_operations
            .operation_state(&operation_id)
            .expect("operation state");
        assert!(operation_state.terminate(), "loss terminates the operation");

        let reconciled = app
            .remote_focus_status(&operation_id)
            .expect("reconciled operation remains queryable");
        assert_eq!(reconciled.state, RemoteFocusState::Failed);
        assert_eq!(
            reconciled.error.as_ref().map(|error| error.code.as_str()),
            Some("connection_lost")
        );
        let completed_at = app
            .remote_focus_operations
            .operations
            .get(&operation_id)
            .and_then(|operation| operation.completed_at)
            .expect("reconciliation completes the operation");

        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "ssh_write_failed".into(),
                message: "remote control stream stopped reading".into(),
            }),
        );

        let detailed = app
            .remote_focus_status(&operation_id)
            .expect("detailed failure remains queryable");
        assert_eq!(detailed.state, RemoteFocusState::Failed);
        assert_eq!(
            detailed.error,
            Some(ErrorBody {
                code: "ssh_write_failed".into(),
                message: "remote control stream stopped reading".into(),
            })
        );
        assert_eq!(
            app.remote_focus_operations
                .operations
                .get(&operation_id)
                .and_then(|operation| operation.completed_at),
            Some(completed_at)
        );
        assert!(!app.remote_focus_operations.input_gate_open(&operation_id));
    }

    #[test]
    fn control_ready_before_first_frame_keeps_the_gate_closed() {
        let (mut app, _recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);

        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        {
            let runtime = app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("proxy runtime");
            assert!(
                runtime
                    .try_send_bytes(bytes::Bytes::from_static(b"too early"))
                    .is_err(),
                "ControlReady without a complete frame keeps input disabled"
            );
        }

        assert!(app
            .apply_remote_focus_frame(&started.operation_id, &full_frame(b"\x1b[1;1Hlate frame"),));
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(bytes::Bytes::from_static(b"answer"))
            .expect("the first complete frame after ControlReady opens the gate");
    }

    #[test]
    fn partial_first_frame_does_not_open_the_input_gate() {
        let (mut app, _recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);

        let mut partial = full_frame(b"partial frame");
        partial.full = false;
        assert!(app.apply_remote_focus_frame(&started.operation_id, &partial));
        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );

        assert!(!app
            .remote_focus_operations
            .input_gate_open(&started.operation_id));
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(bytes::Bytes::from_static(b"too early"))
            .is_err());
    }

    #[test]
    fn refused_proxy_press_drops_a_later_held_key_repeat() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);
        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::empty(),
        )
        .with_windows_record(crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 65,
            virtual_scan_code: 30,
            unicode: b'a' as u16,
            control_key_state: 0,
        });

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(key.clone())],
            false,
        );
        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        app.apply_remote_focus_frame(&started.operation_id, &full_frame(b"ready"));
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .is_remote_proxy());

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(
                key.with_kind(crossterm::event::KeyEventKind::Repeat),
            )],
            false,
        );

        assert!(recording
            .lock()
            .expect("lock")
            .outbound
            .as_mut()
            .expect("outbound channel")
            .try_recv()
            .is_err());
    }

    #[test]
    fn proxy_identity_line_uses_configured_alias_for_a_remote_context() {
        let (mut app, _recording) = proxy_app();
        let requested = AgentRef::new("ub1", "w1:p3").expect("valid agent reference");
        let started = app
            .start_remote_focus_operation(requested)
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);
        let mut remote_context = context();
        // The wire transport translates the remote server identity back to the
        // configured alias before the app consumes this context.
        remote_context.host = "ub1".into();

        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(remote_context.clone())),
        );
        app.apply_remote_focus_frame(&started.operation_id, &full_frame(b"ready"));

        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .and_then(|terminal| terminal.manual_label.as_deref()),
            Some("ub1::w1:p3 · operator · /work/repo · /dev/pts/4 · agent")
        );
        let status = app
            .remote_focus_status(&started.operation_id)
            .expect("operation status");
        assert_eq!(status.agent_ref.host, "ub1");
        assert_eq!(status.context, Some(remote_context));
    }

    #[test]
    fn failure_closes_the_proxy_pane_and_releases_the_lease() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let operation_id = started.operation_id.clone();
        let terminal_id = proxy_terminal_id(&app, &operation_id);
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);

        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "connection_lost".into(),
                message: "stream ended".into(),
            }),
        );

        assert_eq!(
            app.state.workspaces[0].tabs.len(),
            1,
            "a failed focus closes its proxy pane"
        );
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_none(),
            "the proxy runtime shuts down with the pane"
        );
        assert_eq!(
            recording.lock().expect("lock").detached,
            vec![operation_id.clone()]
        );
        let snapshot = app
            .remote_focus_status(&operation_id)
            .expect("operation stays queryable");
        assert_eq!(snapshot.state, RemoteFocusState::Failed);
        assert_eq!(
            snapshot.error.map(|error| error.code),
            Some("connection_lost".to_string())
        );
    }

    #[test]
    fn closing_the_proxy_pane_detaches_and_closes_the_operation() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let operation_id = started.operation_id.clone();
        let (pane_id, terminal_id) = app
            .remote_focus_operations
            .proxy_location(&operation_id)
            .expect("proxy location");
        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        app.apply_remote_focus_frame(&operation_id, &full_frame(b"\x1b[1;1Hlive"));

        // The user closes the pane: the workspace close path queues the
        // terminal for shutdown, and the shutdown releases the lease.
        let should_close = app.state.workspaces[0].remove_pane(pane_id);
        assert!(!should_close);
        app.state.remove_unattached_terminal_ids([terminal_id]);
        app.shutdown_detached_terminal_runtimes();

        assert_eq!(
            recording.lock().expect("lock").detached,
            vec![operation_id.clone()]
        );
        let snapshot = app
            .remote_focus_status(&operation_id)
            .expect("operation stays queryable");
        assert_eq!(snapshot.state, RemoteFocusState::Closed);
    }

    #[test]
    fn windowless_run_rows_are_rejected_before_a_pane_opens() {
        let (mut app, recording) = proxy_app();
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: "buildbox".into(),
                target: "buildbox".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                entries: vec![crate::fleet::FleetRow::test_run_row(
                    "buildbox", "w1:p3", false,
                )],
            }],
            ..crate::fleet::Snapshot::default()
        };

        let error = app
            .start_remote_focus_operation(agent_ref())
            .expect_err("windowless runs are not attachable");
        assert_eq!(error.code, "agent_not_attachable");
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(recording.lock().expect("lock").started.is_empty());
    }

    fn agent_ref() -> AgentRef {
        AgentRef::new("buildbox", "w1:p3").expect("valid agent reference")
    }

    fn context() -> RemoteControlContext {
        RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: "w1".into(),
            tab_id: "t2".into(),
            pane_id: "w1:p3".into(),
            terminal_id: "terminal-id".into(),
            cwd: "/work/repo".into(),
            foreground_cwd: "/work/repo".into(),
            tty: "/dev/pts/4".into(),
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into(), "run".into()],
                cwd: "/work/repo".into(),
            },
            detected_agent: "claude".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 9,
            revision: 41,
            context_epoch: 12,
        }
    }

    #[test]
    fn connecting_active_closed_and_failed_transitions_are_terminally_stable() {
        let now = Instant::now();
        let mut operations = RemoteFocusOperations::default();
        let started = operations
            .begin(agent_ref(), now)
            .expect("operation starts");

        operations.transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
            now,
        );
        assert_eq!(
            operations
                .snapshot(&started.operation_id, now)
                .expect("active operation")
                .state,
            RemoteFocusState::Active
        );
        operations.transition(&started.operation_id, RemoteFocusTransition::Closed, now);
        operations.transition(
            &started.operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "connection_lost".into(),
                message: "late failure".into(),
            }),
            now,
        );
        assert_eq!(
            operations
                .snapshot(&started.operation_id, now)
                .expect("closed operation")
                .state,
            RemoteFocusState::Closed
        );

        let failed = operations
            .begin(
                AgentRef::new("buildbox", "w1:p4").expect("valid agent reference"),
                now,
            )
            .expect("second operation starts");
        operations.transition(
            &failed.operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "host_unreachable".into(),
                message: "not implemented".into(),
            }),
            now,
        );
        operations.transition(
            &failed.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
            now,
        );
        let failed_snapshot = operations
            .snapshot(&failed.operation_id, now)
            .expect("failed operation");
        assert_eq!(failed_snapshot.state, RemoteFocusState::Failed);
        assert_eq!(
            failed_snapshot
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("host_unreachable")
        );
    }

    #[test]
    fn proxy_reuse_prefers_the_retained_live_operation_with_an_existing_pane() {
        let now = Instant::now();
        let source = agent_ref();
        let mut operations = RemoteFocusOperations::default();

        let closed = operations
            .begin(source.clone(), now)
            .expect("closed operation starts");
        let closed_pane = PaneId::from_raw(101);
        operations.attach_proxy(
            &closed.operation_id,
            closed_pane,
            TerminalId::alloc(),
            "closed-proxy".into(),
        );
        operations.transition(&closed.operation_id, RemoteFocusTransition::Closed, now);

        let live = operations
            .begin(source.clone(), now + Duration::from_secs(1))
            .expect("live operation starts");
        let live_pane = PaneId::from_raw(102);
        operations.attach_proxy(
            &live.operation_id,
            live_pane,
            TerminalId::alloc(),
            "live-proxy".into(),
        );

        let missing = operations
            .begin(source.clone(), now + Duration::from_secs(2))
            .expect("operation with removed pane starts");
        operations.attach_proxy(
            &missing.operation_id,
            PaneId::from_raw(103),
            TerminalId::alloc(),
            "missing-proxy".into(),
        );

        assert_eq!(
            operations.proxy_pane_for_agent(&source, |pane_id| pane_id == live_pane),
            Some(live_pane)
        );
    }

    #[test]
    fn proxy_reuse_skips_a_terminated_but_unreconciled_operation() {
        let now = Instant::now();
        let source = agent_ref();
        let mut operations = RemoteFocusOperations::default();

        let live = operations
            .begin(source.clone(), now)
            .expect("live operation starts");
        let live_pane = PaneId::from_raw(104);
        operations.attach_proxy(
            &live.operation_id,
            live_pane,
            TerminalId::alloc(),
            "live-proxy".into(),
        );

        let terminated = operations
            .begin(source.clone(), now + Duration::from_secs(1))
            .expect("terminated operation starts");
        let terminated_pane = PaneId::from_raw(105);
        operations.attach_proxy(
            &terminated.operation_id,
            terminated_pane,
            TerminalId::alloc(),
            "terminated-proxy".into(),
        );
        let operation_state = operations
            .operation_state(&terminated.operation_id)
            .expect("terminated operation state");
        assert!(operation_state.terminate(), "transport marks the loss");

        assert_eq!(
            operations.proxy_pane_for_agent(&source, |pane_id| {
                pane_id == live_pane || pane_id == terminated_pane
            }),
            Some(live_pane)
        );
        assert_eq!(
            operations
                .operations
                .get(&terminated.operation_id)
                .expect("terminated operation remains retained")
                .state,
            RemoteFocusState::Connecting,
            "the app record is still unreconciled while reuse checks transport state"
        );
    }

    #[test]
    fn proxy_reuse_prefers_newest_operation_when_created_at_ties() {
        let now = Instant::now();
        let source = agent_ref();
        let mut operations = RemoteFocusOperations::default();
        let mut selected = None;

        for operation_number in 1..=10 {
            let operation = operations
                .begin(source.clone(), now)
                .expect("operation starts");
            if operation_number == 9 {
                let pane_id = PaneId::from_raw(106);
                operations.attach_proxy(
                    &operation.operation_id,
                    pane_id,
                    TerminalId::alloc(),
                    "older-proxy".into(),
                );
            } else if operation_number == 10 {
                let pane_id = PaneId::from_raw(107);
                operations.attach_proxy(
                    &operation.operation_id,
                    pane_id,
                    TerminalId::alloc(),
                    "newer-proxy".into(),
                );
                selected = Some(pane_id);
            }
        }

        assert_eq!(
            operations.proxy_pane_for_agent(&source, |pane_id| {
                pane_id == PaneId::from_raw(106) || pane_id == PaneId::from_raw(107)
            }),
            selected
        );
    }

    #[test]
    fn transport_terminal_state_releases_a_concurrent_slot_without_an_event() {
        let now = Instant::now();
        let mut operations = RemoteFocusOperations::default();
        for index in 0..REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS {
            operations
                .begin(
                    AgentRef::new("buildbox", format!("w1:p{index}"))
                        .expect("valid agent reference"),
                    now,
                )
                .expect("operation stays under concurrent cap");
        }

        let first_state = operations
            .operation_state("remote-focus-1")
            .expect("first operation state");
        assert!(first_state.terminate(), "transport marks the first loss");
        let retry = operations
            .begin(agent_ref(), now)
            .expect("terminal transport state releases the slot");
        assert_eq!(retry.operation_id, "remote-focus-17");
        assert_eq!(
            operations
                .snapshot("remote-focus-1", now)
                .expect("terminal operation remains queryable")
                .state,
            RemoteFocusState::Failed
        );
    }

    #[test]
    fn concurrent_cap_and_terminal_expiry_bound_the_operation_map() {
        let now = Instant::now();
        let mut operations = RemoteFocusOperations::default();
        for index in 0..REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS {
            operations
                .begin(
                    AgentRef::new("buildbox", format!("w1:p{index}"))
                        .expect("valid agent reference"),
                    now,
                )
                .expect("operation stays under concurrent cap");
        }
        let error = operations
            .begin(agent_ref(), now)
            .expect_err("concurrent cap must reject another operation");
        assert_eq!(error.code, "operation_limit_reached");
        assert_eq!(operations.len(), REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS);

        let first = "remote-focus-1";
        operations.transition(
            first,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "host_unreachable".into(),
                message: "stub".into(),
            }),
            now,
        );
        operations
            .operations
            .get_mut(first)
            .expect("first operation")
            .completed_at = now.checked_sub(REMOTE_FOCUS_TERMINAL_TTL + Duration::from_secs(1));
        let expired = operations
            .snapshot(first, now)
            .expect_err("expired operation is no longer queryable");
        assert_eq!(expired.code, "unknown_operation");
        assert_eq!(operations.len(), REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS - 1);

        let mut retained = RemoteFocusOperations::default();
        for index in 0..REMOTE_FOCUS_MAX_RETAINED_OPERATIONS {
            let operation_now = now + Duration::from_secs(index as u64);
            let started = retained
                .begin(
                    AgentRef::new("buildbox", format!("w1:retained-{index}"))
                        .expect("valid agent reference"),
                    operation_now,
                )
                .expect("terminal operation starts");
            retained.transition(
                &started.operation_id,
                RemoteFocusTransition::Failed(ErrorBody {
                    code: "host_unreachable".into(),
                    message: "stub".into(),
                }),
                operation_now,
            );
        }
        assert_eq!(retained.len(), REMOTE_FOCUS_MAX_RETAINED_OPERATIONS);
        retained
            .begin(
                agent_ref(),
                now + Duration::from_secs(REMOTE_FOCUS_MAX_RETAINED_OPERATIONS as u64),
            )
            .expect("terminal record is evicted at the retention cap");
        assert_eq!(retained.len(), REMOTE_FOCUS_MAX_RETAINED_OPERATIONS);
        let unknown = retained
            .snapshot("remote-focus-1", now)
            .expect_err("evicted operation is no longer queryable");
        assert_eq!(unknown.code, "unknown_operation");
    }
}
