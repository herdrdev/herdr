use std::io::Read;
use std::process::Stdio;

use super::manifest::{effective_platforms, ensure_platform_supported};
use super::plugin_manifest_available;
use crate::api::schema::{
    InstalledPluginInfo, PluginCommandLogInfo, PluginCommandStatus, PluginInvocationContext,
};
use crate::app::App;

const PLUGIN_COMMAND_OUTPUT_MAX_BYTES: usize = 64 * 1024;
pub(super) const MAX_PLUGIN_COMMANDS_IN_FLIGHT: usize = 32;
const PLUGIN_COMMAND_LOG_LIMIT: usize = 200;

/// Ensures a `PluginCommandFinished` event is always delivered for a spawned
/// plugin command. `plugin_commands_in_flight` is only decremented when the
/// app processes that event, so if the executing thread panics or otherwise
/// never reaches the normal `send`, the guard's Drop emits a terminal (failed)
/// finished event and the in-flight slot is always released instead of leaking
/// until `MAX_PLUGIN_COMMANDS_IN_FLIGHT` blocks all further plugin commands.
struct PluginCommandFinishedGuard {
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    log_id: String,
    sent: bool,
}

impl PluginCommandFinishedGuard {
    fn new(event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>, log_id: String) -> Self {
        Self {
            event_tx,
            log_id,
            sent: false,
        }
    }

    fn send(mut self, finished: crate::events::AppEvent) {
        self.sent = true;
        let _ = self.event_tx.blocking_send(finished);
    }
}

impl Drop for PluginCommandFinishedGuard {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        // The thread panicked or never reached a normal send; release the slot.
        let finished = crate::events::AppEvent::PluginCommandFinished {
            log_id: self.log_id.clone(),
            finished_unix_ms: current_unix_ms(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some("plugin command thread terminated before completion".to_string()),
        };
        let _ = self.event_tx.blocking_send(finished);
    }
}

impl App {
    pub(super) fn start_plugin_command(
        &mut self,
        plugin: &InstalledPluginInfo,
        action_id: Option<String>,
        event: Option<String>,
        command: Vec<String>,
        context: &PluginInvocationContext,
        event_json: Option<String>,
    ) -> Result<PluginCommandLogInfo, (&'static str, String)> {
        let Some(program) = command.first().cloned() else {
            return Err((
                "invalid_plugin_command",
                "command must not be empty".to_string(),
            ));
        };
        let args = command.iter().skip(1).cloned().collect::<Vec<_>>();
        let context_json = serde_json::to_string(context)
            .map_err(|err| ("invalid_plugin_context", err.to_string()))?;
        super::env::ensure_plugin_user_dirs(plugin)
            .map_err(|err| ("plugin_user_dir_create_failed", err.to_string()))?;
        let log_id = format!("plugin-log-{}", self.state.next_plugin_command_log_id);
        self.state.next_plugin_command_log_id += 1;
        let started_unix_ms = current_unix_ms();
        let mut env = super::env::plugin_path_env(plugin);
        env.extend([
            (
                crate::api::SOCKET_PATH_ENV_VAR.to_string(),
                crate::api::socket_path().display().to_string(),
            ),
            ("HERDR_ENV".to_string(), "1".to_string()),
            ("HERDR_PLUGIN_ID".to_string(), plugin.plugin_id.clone()),
            ("HERDR_PLUGIN_CONTEXT_JSON".to_string(), context_json),
        ]);
        if let Ok(current_exe) = std::env::current_exe() {
            env.push((
                "HERDR_BIN_PATH".to_string(),
                current_exe.display().to_string(),
            ));
        }
        if let Some(action_id) = action_id.as_ref() {
            env.push(("HERDR_PLUGIN_ACTION_ID".to_string(), action_id.clone()));
        }
        if let Some(event) = event.as_ref() {
            env.push(("HERDR_PLUGIN_EVENT".to_string(), event.clone()));
        }
        if let Some(event_json) = event_json {
            env.push(("HERDR_PLUGIN_EVENT_JSON".to_string(), event_json));
        }
        if let Some(workspace_id) = context.workspace_id.as_ref() {
            env.push(("HERDR_WORKSPACE_ID".to_string(), workspace_id.clone()));
        }
        if let Some(tab_id) = context.tab_id.as_ref() {
            env.push(("HERDR_TAB_ID".to_string(), tab_id.clone()));
        }
        if let Some(pane_id) = context.focused_pane_id.as_ref() {
            env.push(("HERDR_PANE_ID".to_string(), pane_id.clone()));
        }
        if let Some(clicked_url) = context.clicked_url.as_ref() {
            env.push(("HERDR_PLUGIN_CLICKED_URL".to_string(), clicked_url.clone()));
        }
        if let Some(link_handler_id) = context.link_handler_id.as_ref() {
            env.push((
                "HERDR_PLUGIN_LINK_HANDLER_ID".to_string(),
                link_handler_id.clone(),
            ));
        }
        if self.state.plugin_commands_in_flight >= MAX_PLUGIN_COMMANDS_IN_FLIGHT {
            let message = format!(
                "maximum concurrent plugin commands reached ({MAX_PLUGIN_COMMANDS_IN_FLIGHT})"
            );
            let log = PluginCommandLogInfo {
                log_id,
                plugin_id: plugin.plugin_id.clone(),
                action_id,
                event,
                command,
                status: PluginCommandStatus::Failed,
                started_unix_ms,
                finished_unix_ms: Some(started_unix_ms),
                exit_code: None,
                stdout: Some(String::new()),
                stderr: Some(String::new()),
                error: Some(message.clone()),
            };
            self.push_plugin_command_log(log);
            return Err(("plugin_command_limit_reached", message));
        }
        let plugin_root = std::path::PathBuf::from(&plugin.plugin_root);
        let log = PluginCommandLogInfo {
            log_id: log_id.clone(),
            plugin_id: plugin.plugin_id.clone(),
            action_id,
            event,
            command: command.clone(),
            status: PluginCommandStatus::Running,
            started_unix_ms,
            finished_unix_ms: None,
            exit_code: None,
            stdout: None,
            stderr: None,
            error: None,
        };
        self.push_plugin_command_log(log.clone());
        self.state.plugin_commands_in_flight += 1;
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            // Drop of this guard emits a plugin.finished fallback if the thread
            // panics or never calls `send`, releasing the in-flight counter.
            let guard = PluginCommandFinishedGuard::new(event_tx, log_id.clone());
            let child =
                crate::plugin_command::command_for_argv_in_dir(&program, &args, &plugin_root)
                    .envs(env)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn();
            let finished = match child {
                Ok(mut child) => {
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();
                    let stdout_reader = stdout.map(|stdout| {
                        std::thread::spawn(move || {
                            read_capped_plugin_output(stdout, PLUGIN_COMMAND_OUTPUT_MAX_BYTES)
                        })
                    });
                    let stderr_reader = stderr.map(|stderr| {
                        std::thread::spawn(move || {
                            read_capped_plugin_output(stderr, PLUGIN_COMMAND_OUTPUT_MAX_BYTES)
                        })
                    });
                    match child.wait() {
                        Ok(status) => crate::events::AppEvent::PluginCommandFinished {
                            log_id,
                            finished_unix_ms: current_unix_ms(),
                            exit_code: status.code(),
                            stdout: stdout_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            stderr: stderr_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            error: None,
                        },
                        Err(err) => crate::events::AppEvent::PluginCommandFinished {
                            log_id,
                            finished_unix_ms: current_unix_ms(),
                            exit_code: None,
                            stdout: stdout_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            stderr: stderr_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            error: Some(err.to_string()),
                        },
                    }
                }
                Err(err) => crate::events::AppEvent::PluginCommandFinished {
                    log_id,
                    finished_unix_ms: current_unix_ms(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some(err.to_string()),
                },
            };
            guard.send(finished);
        });
        Ok(log)
    }

    pub(crate) fn run_plugin_startup_hooks(&mut self) {
        let mut context = self.current_plugin_context("plugin.startup");
        context.invocation_source = Some("startup".to_string());
        let mut plugins = self
            .state
            .installed_plugins
            .values()
            .filter(|plugin| {
                plugin.enabled && plugin_manifest_available(plugin) && !plugin.startup.is_empty()
            })
            .cloned()
            .collect::<Vec<_>>();
        plugins.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        for plugin in plugins {
            for startup in plugin.startup.clone() {
                if ensure_platform_supported(
                    &effective_platforms(&startup.platforms, &plugin.platforms).clone(),
                    "startup",
                )
                .is_err()
                {
                    continue;
                }
                let _ = self.start_plugin_command(
                    &plugin,
                    None,
                    Some("startup".to_string()),
                    startup.command,
                    &context,
                    None,
                );
            }
        }
    }

    pub(crate) fn run_plugin_event_hooks(&mut self, event: &crate::api::schema::EventEnvelope) {
        let event_name = event.event.dot_name();
        if !crate::api::schema::PLUGIN_HOOK_EVENT_KINDS.contains(&event.event) {
            return;
        }
        if let Err(err) = self.refresh_installed_plugins() {
            tracing::warn!(err = %err, "failed to refresh plugin registry before event hooks");
            return;
        }
        let plugins = self
            .state
            .installed_plugins
            .values()
            .filter(|plugin| {
                plugin.enabled
                    && plugin_manifest_available(plugin)
                    && plugin.events.iter().any(|hook| hook.on == event_name)
            })
            .cloned()
            .collect::<Vec<_>>();
        if plugins.is_empty() {
            return;
        }
        let event_json = serde_json::to_string(event).ok();
        let context = self.plugin_context_for_event(event, event_name);
        for plugin in plugins {
            for hook in plugin.events.clone() {
                if hook.on != event_name {
                    continue;
                }
                if ensure_platform_supported(
                    &effective_platforms(&hook.platforms, &plugin.platforms).clone(),
                    event_name,
                )
                .is_err()
                {
                    continue;
                }
                let _ = self.start_plugin_command(
                    &plugin,
                    None,
                    Some(event_name.to_string()),
                    hook.command.clone(),
                    &context,
                    event_json.clone(),
                );
            }
        }
    }

    fn push_plugin_command_log(&mut self, log: PluginCommandLogInfo) {
        self.state.plugin_command_logs.push(log);
        if self.state.plugin_command_logs.len() > PLUGIN_COMMAND_LOG_LIMIT {
            let extra = self.state.plugin_command_logs.len() - PLUGIN_COMMAND_LOG_LIMIT;
            self.state.plugin_command_logs.drain(0..extra);
        }
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

pub(super) fn read_capped_plugin_output(mut reader: impl Read, cap: usize) -> String {
    let mut kept = Vec::with_capacity(cap.min(8192));
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let remaining = cap.saturating_sub(kept.len());
                if remaining > 0 {
                    kept.extend_from_slice(&buf[..n.min(remaining)]);
                }
                if n > remaining {
                    truncated = true;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let mut output = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        output.push_str(&format!(
            "\n[herdr truncated plugin output after {cap} bytes]"
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recv_finished(
        rx: &mut tokio::sync::mpsc::Receiver<crate::events::AppEvent>,
    ) -> crate::events::AppEvent {
        rx.blocking_recv()
            .expect("expected a PluginCommandFinished event")
    }

    #[test]
    fn guard_emits_fallback_on_drop_without_send() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::events::AppEvent>(8);
        {
            let _guard = PluginCommandFinishedGuard::new(tx, "log-drop".to_string());
            // Dropped without a successful send: the in-flight slot must still
            // be released via a fallback finished event.
        }
        match recv_finished(&mut rx) {
            crate::events::AppEvent::PluginCommandFinished {
                log_id,
                error,
                exit_code,
                ..
            } => {
                assert_eq!(log_id, "log-drop");
                assert!(error.is_some(), "fallback must be marked failed");
                assert_eq!(exit_code, None);
            }
            _ => panic!("expected PluginCommandFinished"),
        }
        // Exactly one event was emitted.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn guard_does_not_double_send_after_explicit_send() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::events::AppEvent>(8);
        let guard = PluginCommandFinishedGuard::new(tx, "log-send".to_string());
        guard.send(crate::events::AppEvent::PluginCommandFinished {
            log_id: "log-send".to_string(),
            finished_unix_ms: 1,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            error: None,
        });
        match recv_finished(&mut rx) {
            crate::events::AppEvent::PluginCommandFinished {
                log_id,
                error,
                exit_code,
                ..
            } => {
                assert_eq!(log_id, "log-send");
                assert!(error.is_none());
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!("expected PluginCommandFinished"),
        }
        // The guard's Drop must not emit a second event.
        assert!(rx.try_recv().is_err());
    }
}
