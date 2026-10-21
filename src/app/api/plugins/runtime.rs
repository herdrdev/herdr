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
        if let Ok(current_exe) = crate::platform::launch_executable() {
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
        let event_tx = self.event_tx.clone();
        let installation_lease =
            crate::plugin_installations::command_lease(&self.plugin_installation_leases, plugin);
        let spawned = crate::thread_spawn::spawn_named("herdr-plugin-command", move || {
            // The worker may outlive App during normal server teardown.
            let _installation_lease = installation_lease;
            let child =
                crate::plugin_command::command_for_argv_in_dir(&program, &args, &plugin_root)
                    .envs(env)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn();
            let finished = match child {
                Ok(child) => finish_plugin_child(log_id, child),
                Err(err) => crate::events::AppEvent::PluginCommandFinished {
                    log_id,
                    finished_unix_ms: current_unix_ms(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some(err.to_string()),
                },
            };
            let _ = event_tx.blocking_send(finished);
        });
        if let Err(err) = spawned {
            tracing::warn!(err = %err, "failed to spawn plugin command thread");
            let log = PluginCommandLogInfo {
                status: PluginCommandStatus::Failed,
                finished_unix_ms: Some(current_unix_ms()),
                stdout: Some(String::new()),
                stderr: Some(String::new()),
                error: Some(format!("could not start plugin command: {err}")),
                ..log
            };
            self.push_plugin_command_log(log.clone());
            return Ok(log);
        }
        self.push_plugin_command_log(log.clone());
        self.state.plugin_commands_in_flight += 1;
        Ok(log)
    }

    pub(crate) fn run_plugin_startup_hooks(&mut self) {
        if self.policy.persist_plugin_registry && !self.plugin_installation_cleanup_allowed {
            tracing::warn!("plugin cleanup skipped: some installations could not be pinned");
        } else if self.policy.persist_plugin_registry {
            if let Err(err) = crate::plugin_installations::cleanup() {
                tracing::warn!(%err, "plugin cleanup deferred");
            }
        }
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

type PluginOutputReader = std::thread::JoinHandle<String>;

fn finish_plugin_child(log_id: String, mut child: std::process::Child) -> crate::events::AppEvent {
    let (stdout_reader, stderr_reader) =
        match spawn_plugin_output_readers(child.stdout.take(), child.stderr.take()) {
            Ok(readers) => readers,
            Err(err) => {
                // A missing reader would leave the plugin writing into a closed
                // pipe, so stop it rather than report a misleading result.
                tracing::warn!(err = %err, "failed to spawn plugin output reader");
                let _ = child.kill();
                let _ = child.wait();
                return crate::events::AppEvent::PluginCommandFinished {
                    log_id,
                    finished_unix_ms: current_unix_ms(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some(format!("could not start output reader: {err}")),
                };
            }
        };
    let wait = child.wait();
    // Descendants can hold the pipes open after the command exits; the run
    // ends when the command does, not when its output closes.
    let finished_unix_ms = current_unix_ms();
    let join = |reader: Option<PluginOutputReader>| {
        reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    };
    let stdout = join(stdout_reader);
    let stderr = join(stderr_reader);
    let (exit_code, error) = match wait {
        Ok(status) => (status.code(), None),
        Err(err) => (None, Some(err.to_string())),
    };
    crate::events::AppEvent::PluginCommandFinished {
        log_id,
        finished_unix_ms,
        exit_code,
        stdout,
        stderr,
        error,
    }
}

fn spawn_plugin_output_readers(
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
) -> std::io::Result<(Option<PluginOutputReader>, Option<PluginOutputReader>)> {
    let stdout = stdout
        .map(|stdout| {
            crate::thread_spawn::spawn_named("herdr-plugin-stdout", move || {
                read_capped_plugin_output(stdout, PLUGIN_COMMAND_OUTPUT_MAX_BYTES)
            })
        })
        .transpose()?;
    let stderr = stderr
        .map(|stderr| {
            crate::thread_spawn::spawn_named("herdr-plugin-stderr", move || {
                read_capped_plugin_output(stderr, PLUGIN_COMMAND_OUTPUT_MAX_BYTES)
            })
        })
        .transpose()?;
    Ok((stdout, stderr))
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn plugin_output_reader_spawn_failure_stops_the_command() {
        let child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();

        crate::thread_spawn::test_hook::fail_next_spawns(1);
        let finished = finish_plugin_child("log".into(), child);

        let crate::events::AppEvent::PluginCommandFinished {
            exit_code, error, ..
        } = finished
        else {
            panic!("expected plugin command result");
        };
        assert_eq!(exit_code, None);
        assert!(error.is_some_and(|error| error.starts_with("could not start output reader")));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn plugin_second_output_reader_spawn_failure_stops_the_command() {
        let child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();

        crate::thread_spawn::test_hook::fail_spawns_after(1, 1);
        let finished = finish_plugin_child("log".into(), child);

        let crate::events::AppEvent::PluginCommandFinished {
            exit_code, error, ..
        } = finished
        else {
            panic!("expected plugin command result");
        };
        assert_eq!(exit_code, None);
        assert!(error.is_some_and(|error| error.starts_with("could not start output reader")));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn plugin_finish_time_is_taken_when_the_command_exits() {
        // The background `cat` keeps both output pipes open after `sh` exits,
        // until the test closes its stdin.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "cat <&0 >/dev/null &"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let pid = child.id().to_string();
        let releaser = std::thread::spawn(move || {
            // Hold the pipes until `sh` has exited and been reaped.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline
                && std::process::Command::new("ps")
                    .args(["-p", &pid])
                    .stdout(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            let released_unix_ms = current_unix_ms();
            drop(stdin);
            released_unix_ms
        });

        let finished = finish_plugin_child("log".into(), child);
        let released_unix_ms = releaser.join().unwrap();

        let crate::events::AppEvent::PluginCommandFinished {
            finished_unix_ms,
            exit_code,
            ..
        } = finished
        else {
            panic!("expected plugin command result");
        };
        assert_eq!(exit_code, Some(0));
        assert!(finished_unix_ms <= released_unix_ms);
    }
}
