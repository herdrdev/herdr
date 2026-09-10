use std::cell::RefCell;
use std::io;

use crate::api::client::{ApiClient, ConnectionTarget};
use crate::client::endpoint::{EndpointCatalog, SavedSshEndpoint};

thread_local! {
    // CLI dispatch is synchronous. Scope routing to this command, never the runtime or TUI.
    static TARGET: RefCell<Option<MachineTarget>> = const { RefCell::new(None) };
}

struct MachineTarget {
    profile: SavedSshEndpoint,
    bridge: Option<crate::remote::SavedSshApiBridge>,
    #[cfg(test)]
    client_override: Option<ApiClient>,
}

struct TargetScope(Option<MachineTarget>);

impl Drop for TargetScope {
    fn drop(&mut self) {
        TARGET.with(|target| *target.borrow_mut() = self.0.take());
    }
}

pub(super) fn maybe_run(args: &[String]) -> Option<io::Result<super::CommandOutcome>> {
    let (selector, args) = match parse_machine_prefix(args) {
        Ok(Some(target)) => target,
        Ok(None) => return None,
        Err(error) => return Some(usage_error(error)),
    };
    Some((|| {
        if let Err(error) = validate_machine_command(&args) {
            return usage_error(error);
        }
        if super::spec::print_requested_help(&args)? {
            return Ok(super::CommandOutcome::Handled(0));
        }
        let profiles = EndpointCatalog::load_profiles().map_err(io::Error::other)?;
        let profile = match resolve_machine(&profiles, &selector) {
            Ok(profile) => profile.clone(),
            Err(error) => return usage_error(error),
        };
        let _scope = TARGET.with(|target| {
            TargetScope(target.replace(Some(MachineTarget {
                profile,
                bridge: None,
                #[cfg(test)]
                client_override: None,
            })))
        });
        super::maybe_run(&args)
    })())
}

fn usage_error(error: String) -> io::Result<super::CommandOutcome> {
    eprintln!("error: {error}");
    Ok(super::CommandOutcome::Handled(2))
}

pub(super) fn is_remote() -> bool {
    TARGET.with(|target| target.borrow().is_some())
}

pub(super) fn api_client() -> io::Result<ApiClient> {
    TARGET.with(|target| {
        let mut target = target.borrow_mut();
        let Some(target) = target.as_mut() else {
            return Ok(ApiClient::local());
        };
        #[cfg(test)]
        if let Some(client) = &target.client_override {
            return Ok(client.clone());
        }
        if target.bridge.is_none() {
            target.bridge = Some(
                crate::remote::SavedSshApiBridge::start(
                    target.profile.id.as_str(),
                    &target.profile.target,
                    &target.profile.session,
                )
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("machine '{}': {error}", target.profile.label),
                    )
                })?,
            );
        }
        let bridge = target
            .bridge
            .as_ref()
            .ok_or_else(|| io::Error::other("machine bridge unavailable"))?;
        Ok(ApiClient::for_target(ConnectionTarget::SocketPath(
            bridge.socket_path().to_owned(),
        )))
    })
}

pub(super) fn remote_error(error: io::Error) -> io::Error {
    TARGET.with(|target| {
        let target = target.borrow();
        let Some(target) = target.as_ref() else {
            return error;
        };
        let error = target
            .bridge
            .as_ref()
            .and_then(|bridge| bridge.reported_failure())
            .unwrap_or(error);
        io::Error::new(
            error.kind(),
            format!(
                "machine '{}' (session {}): {error}",
                target.profile.label, target.profile.session
            ),
        )
    })
}

pub(super) fn restart_guidance() -> String {
    TARGET.with(|target| match target.borrow().as_ref() {
        Some(target) => format!("Update Herdr and restart the server on machine '{}' (session {}). Stopping the server exits its pane processes.", target.profile.label, target.profile.session),
        None => crate::session::active_restart_after_update_guidance(),
    })
}

pub(super) fn remote_identity() -> Option<(String, String)> {
    TARGET.with(|target| {
        target.borrow().as_ref().map(|target| {
            (
                target.profile.id.to_string(),
                target.profile.session.clone(),
            )
        })
    })
}

pub(super) fn socket_label() -> String {
    match remote_identity() {
        Some((id, session)) => format!("machine:{id}/{session}"),
        None => crate::api::socket_path().display().to_string(),
    }
}

pub(super) fn remote_path_is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

pub(super) fn caller_pane_id() -> Option<String> {
    if is_remote() {
        return None;
    }
    std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn parse_machine_prefix(args: &[String]) -> Result<Option<(String, Vec<String>)>, String> {
    let mut index = 1;
    let mut machine = None;
    let mut other_prefix = false;
    while let Some(arg) = args.get(index) {
        if arg == "--machine" || arg.starts_with("--machine=") {
            if machine.is_some() {
                return Err("--machine can only be specified once".into());
            }
            let value = if let Some(value) = arg.strip_prefix("--machine=") {
                value.to_owned()
            } else {
                index += 1;
                args.get(index)
                    .cloned()
                    .ok_or("missing value for --machine")?
            };
            if value.trim().is_empty() || value.starts_with('-') {
                return Err("--machine requires a saved machine label or profile ID".into());
            }
            machine = Some(value);
        } else if arg.starts_with('-') && arg != "--" {
            other_prefix = true;
            if matches!(
                arg.as_str(),
                "--session" | "--remote" | "--remote-keybindings"
            ) {
                index += 1;
            }
        } else {
            break;
        }
        index += 1;
    }
    let Some(machine) = machine else {
        return Ok(None);
    };
    if other_prefix {
        return Err("--machine cannot be combined with other launch options; it uses the saved machine's session".into());
    }
    if index >= args.len() || args[index] == "--" {
        return Err("usage: herdr --machine <label-or-id> <command>".into());
    }
    let mut cleaned = vec![args[0].clone()];
    cleaned.extend_from_slice(&args[index..]);
    Ok(Some((machine, cleaned)))
}

fn resolve_machine<'a>(
    profiles: &'a [SavedSshEndpoint],
    selector: &str,
) -> Result<&'a SavedSshEndpoint, String> {
    let profile = if let Some(profile) = profiles
        .iter()
        .find(|profile| profile.id.as_str() == selector)
    {
        profile
    } else {
        let mut matches = profiles.iter().filter(|profile| profile.label == selector);
        let profile = matches
            .next()
            .ok_or_else(|| format!("unknown machine '{selector}'; use `herdr machine list`"))?;
        if matches.next().is_some() {
            return Err(format!(
                "machine label '{selector}' is ambiguous; use its profile ID"
            ));
        }
        profile
    };
    if !profile.enabled {
        return Err(format!("machine '{selector}' is disabled"));
    }
    Ok(profile)
}

fn validate_machine_command(args: &[String]) -> Result<(), String> {
    let command = args.get(1).map(String::as_str).unwrap_or_default();
    let subcommand = args.get(2).map(String::as_str).unwrap_or_default();
    let supported = match command {
        "workspace" | "worktree" | "tab" | "pane" | "notification" => true,
        "agent" => {
            subcommand != "attach"
                && !(subcommand == "explain"
                    && args[3..]
                        .iter()
                        .any(|arg| arg == "--file" || arg.starts_with("--file=")))
        }
        "api" => subcommand == "snapshot",
        "status" => subcommand == "server",
        "plugin" => matches!(
            subcommand,
            "link" | "unlink" | "enable" | "disable" | "list" | "action" | "log" | "logs" | "pane"
        ),
        "server" => matches!(
            subcommand,
            "stop" | "reload-config" | "agent-manifests" | "reload-agent-manifests"
        ),
        _ => false,
    };
    if supported {
        Ok(())
    } else {
        Err(format!("`{command} {subcommand}` is not an API-backed machine command; --machine does not run local management commands or attach a TUI"))
    }
}

#[cfg(test)]
pub(super) fn with_test_client<T>(client: ApiClient, run: impl FnOnce() -> T) -> T {
    let _scope = TARGET.with(|target| {
        TargetScope(
            target.replace(Some(MachineTarget {
                profile: SavedSshEndpoint::new("test-machine", "unused", "remote-session")
                    .expect("valid test profile"),
                bridge: None,
                client_override: Some(client),
            })),
        )
    });
    run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn machine_prefix_routes_without_consuming_command_payload() {
        for prefix in [args(&["--machine", "mac"]), args(&["--machine=mac"])] {
            let mut input = args(&["herdr"]);
            input.extend(prefix);
            input.extend(args(&["agent", "prompt", "w4:p1", "--machine"]));
            assert_eq!(
                parse_machine_prefix(&input).unwrap(),
                Some((
                    "mac".into(),
                    args(&["herdr", "agent", "prompt", "w4:p1", "--machine"])
                ))
            );
        }
        assert_eq!(
            parse_machine_prefix(&args(&[
                "herdr",
                "agent",
                "prompt",
                "w4:p1",
                "--machine=mac"
            ]))
            .unwrap(),
            None
        );
    }

    #[test]
    fn machine_prefix_rejects_missing_target_and_conflicting_global_options() {
        for input in [
            args(&["herdr", "--machine"]),
            args(&["herdr", "--machine="]),
            args(&["herdr", "--machine", "--help"]),
            args(&["herdr", "--machine", "mac"]),
            args(&[
                "herdr",
                "--machine",
                "mac",
                "--machine",
                "other",
                "agent",
                "list",
            ]),
            args(&[
                "herdr",
                "--machine",
                "mac",
                "--session",
                "other",
                "agent",
                "list",
            ]),
            args(&[
                "herdr",
                "--session",
                "other",
                "--machine",
                "mac",
                "agent",
                "list",
            ]),
            args(&[
                "herdr",
                "--remote",
                "other",
                "--machine",
                "mac",
                "agent",
                "list",
            ]),
        ] {
            assert!(parse_machine_prefix(&input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn machine_resolution_requires_a_unique_enabled_saved_machine() {
        let mac = SavedSshEndpoint::new("mac", "mac-ssh", "agents").unwrap();
        let other = SavedSshEndpoint::new("build", "builder", "default").unwrap();
        let profiles = vec![mac.clone(), other];
        assert_eq!(resolve_machine(&profiles, "mac").unwrap(), &mac);
        assert_eq!(resolve_machine(&profiles, mac.id.as_str()).unwrap(), &mac);
        let shadow = SavedSshEndpoint::new(mac.id.as_str(), "shadow", "default").unwrap();
        assert_eq!(
            resolve_machine(&[mac.clone(), shadow], mac.id.as_str()).unwrap(),
            &mac
        );
        assert!(resolve_machine(&profiles, "mac-ssh").is_err());
        assert!(resolve_machine(&profiles, "missing").is_err());
        let duplicate = SavedSshEndpoint::new("mac", "other", "default").unwrap();
        assert!(resolve_machine(&[mac.clone(), duplicate], "mac").is_err());
        let mut disabled = mac;
        disabled.enabled = false;
        assert!(resolve_machine(&[disabled], "mac").is_err());
    }

    #[test]
    fn machine_commands_reject_local_side_effects_and_tui_attach() {
        for command in [
            &["update"][..],
            &["config", "reset-keys"],
            &["machine", "remove", "mac"],
            &["session", "delete", "default"],
            &["server", "live-handoff"],
            &["agent", "attach", "w4:p1"],
            &["terminal", "attach", "w4:p1"],
            &["terminal", "session", "control", "w4:p1"],
            &["plugin", "install", "./plugin"],
            &["integration", "install", "pi"],
            &["api", "schema", "--output", "schema.json"],
            &["status", "client"],
        ] {
            let mut input = args(&["herdr"]);
            input.extend(args(command));
            assert!(validate_machine_command(&input).is_err(), "{input:?}");
        }
        for command in [
            &["agent", "list"][..],
            &["agent", "wait", "w4:p1"],
            &["pane", "split", "w4:p1", "--direction", "right"],
            &["workspace", "list"],
            &["worktree", "create", "--branch", "feature"],
            &["tab", "list"],
            &["api", "snapshot"],
            &["server", "stop"],
        ] {
            let mut input = args(&["herdr"]);
            input.extend(args(command));
            assert!(validate_machine_command(&input).is_ok(), "{input:?}");
        }
    }
}
