const WATCHER_PLUGIN_ID: &str = "herdr-agent-watcher";
const TITLE_SYNC_PLUGIN_ID: &str = "herdr-agent-title-sync";

pub(crate) fn coexistence_warnings<'a>(
    watcher_enabled: bool,
    title_sync_enabled: bool,
    enabled_plugins: impl Iterator<Item = &'a str>,
) -> Vec<&'static str> {
    let enabled = enabled_plugins.collect::<std::collections::HashSet<_>>();
    let mut warnings = Vec::new();
    if watcher_enabled && enabled.contains(WATCHER_PLUGIN_ID) {
        warnings.push(
            "standalone agent watcher is enabled alongside the built-in watcher; disable it with `herdr plugin disable herdr-agent-watcher`",
        );
    }
    if title_sync_enabled && enabled.contains(TITLE_SYNC_PLUGIN_ID) {
        warnings.push(
            "standalone title sync is enabled alongside built-in title sync; disable it with `herdr plugin disable herdr-agent-title-sync`",
        );
    }
    warnings
}

#[cfg(unix)]
mod platform {
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use herdr_agent_watcher::daemon::state_wire::Hello;

    use super::{coexistence_warnings, WATCHER_PLUGIN_ID};

    pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
        match args.first().map(String::as_str) {
            Some("status") if args.len() == 1 => status(),
            Some("doctor") if args.len() == 1 => doctor(),
            Some("sidebar") if args.len() == 1 => {
                set_watcher_state_env()?;
                Ok(herdr_agent_watcher::sidebar::tui::run())
            }
            Some("claude-bridge") => claude_bridge(&args[1..]),
            Some("kimi-consent") => kimi_consent(&args[1..]),
            _ => {
                eprintln!(
                    "usage: herdr watcher <status|doctor|sidebar|claude-bridge|kimi-consent>"
                );
                Ok(2)
            }
        }
    }

    fn state_dir() -> PathBuf {
        crate::plugin_paths::plugin_state_dir(WATCHER_PLUGIN_ID)
    }

    fn set_watcher_state_env() -> std::io::Result<PathBuf> {
        let state = state_dir();
        std::fs::create_dir_all(&state)?;
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", &state);
        Ok(state)
    }

    fn status() -> std::io::Result<i32> {
        let socket =
            herdr_agent_watcher::daemon::DaemonOptions::new(state_dir()).state_socket_path();
        let snapshot = match read_snapshot(&socket) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                println!("daemon: not running");
                println!("state socket: {}", socket.display());
                eprintln!("{error}");
                return Ok(1);
            }
        };
        let mut agents = BTreeMap::<String, usize>::new();
        for agent in snapshot
            .panes
            .values()
            .filter_map(|telemetry| telemetry.agent.as_ref())
        {
            *agents.entry(agent.clone()).or_default() += 1;
        }
        println!("daemon: running");
        if let Some(build) = snapshot.build {
            println!("build: {build}");
        }
        println!("bound agents: {}", agents.values().sum::<usize>());
        for (agent, count) in agents {
            println!("  {agent}: {count}");
        }
        Ok(0)
    }

    fn read_snapshot(socket: &Path) -> std::io::Result<Hello> {
        let mut stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"{\"method\":\"snapshot\"}\n")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }

    fn doctor() -> std::io::Result<i32> {
        set_watcher_state_env()?;
        let code = herdr_agent_watcher::agents::claude_bridge::cli_doctor(&[]);
        let config = crate::config::Config::load().config;
        let plugins = crate::persist::plugin_registry::load();
        for warning in coexistence_warnings(
            config.agent_watcher.enabled,
            config.title_sync.enabled,
            plugins
                .iter()
                .filter(|plugin| plugin.enabled)
                .map(|plugin| plugin.plugin_id.as_str()),
        ) {
            eprintln!("warning: {warning}");
        }
        Ok(code)
    }

    fn claude_bridge(args: &[String]) -> std::io::Result<i32> {
        match args.first().map(String::as_str) {
            Some("enable") if args.len() == 1 => {
                let executable = std::env::current_exe()?;
                let state = set_watcher_state_env()?;
                match herdr_agent_watcher::agents::claude_bridge::install_claude_bridge(
                    &executable,
                    &["watcher", "claude-bridge"],
                    &state,
                ) {
                    Ok(path) => {
                        println!("{}", path.display());
                        Ok(0)
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        Ok(1)
                    }
                }
            }
            Some("disable") if args.len() == 1 => {
                let state = set_watcher_state_env()?;
                match herdr_agent_watcher::agents::claude_bridge::uninstall_claude_bridge(&state) {
                    Ok(path) => {
                        println!("{}", path.display());
                        Ok(0)
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        Ok(1)
                    }
                }
            }
            Some(argument) if argument.starts_with('-') => {
                Ok(herdr_agent_watcher::agents::claude_bridge::cli_claude_bridge(args))
            }
            _ => {
                eprintln!("usage: herdr watcher claude-bridge <enable|disable>");
                Ok(2)
            }
        }
    }

    fn kimi_consent(args: &[String]) -> std::io::Result<i32> {
        use herdr_agent_watcher::agents::consent;

        let state = set_watcher_state_env()?;
        let path = herdr_agent_watcher::daemon::DaemonOptions::new(state).kimi_consent_path();
        match args.first().map(String::as_str) {
            Some("on") if args.len() == 1 => {
                consent::set_and_persist(&path, true)?;
                println!("enabled");
                Ok(0)
            }
            Some("off") if args.len() == 1 => {
                consent::set_and_persist(&path, false)?;
                println!("disabled");
                Ok(0)
            }
            Some("status") if args.len() == 1 => {
                consent::load_into_memory(&path);
                let enabled = consent::enabled();
                println!("{}", if enabled { "enabled" } else { "disabled" });
                Ok(i32::from(!enabled))
            }
            _ => {
                eprintln!("usage: herdr watcher kimi-consent <on|off|status>");
                Ok(2)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::coexistence_warnings;

        #[test]
        fn coexistence_warnings_name_exact_disable_commands() {
            let warnings = coexistence_warnings(
                true,
                true,
                ["herdr-agent-watcher", "herdr-agent-title-sync"].into_iter(),
            );
            assert_eq!(warnings.len(), 2);
            assert!(warnings[0].contains("herdr plugin disable herdr-agent-watcher"));
            assert!(warnings[1].contains("herdr plugin disable herdr-agent-title-sync"));
        }
    }
}

pub(super) fn run_watcher_command(args: &[String]) -> std::io::Result<i32> {
    #[cfg(unix)]
    {
        platform::run(args)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        eprintln!("watcher commands are unsupported on this platform");
        Ok(1)
    }
}
