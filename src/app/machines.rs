//! Machines: where a composer dispatch actually runs.
//!
//! A remote dispatch is deliberately not "run the agent over SSH". The pane is
//! created by the target machine's own Herdr server, so it belongs to that
//! machine: it survives this client quitting, this machine sleeping, and the
//! SSH connection dropping. SSH carries two short CLI calls and nothing else,
//! which is also why this module builds commands rather than holding sessions.
//!
//! The machine list is the fleet the operator already configured for reading
//! host status. Reusing it means a machine you can watch is a machine you can
//! dispatch to, with no second inventory to keep in sync.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::config::{FleetConfig, FleetHostConfig};

/// Deadline for each of the two remote calls. Creating a workspace is cheap;
/// this is a guard against an unreachable host, not a work budget.
const REMOTE_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// One machine the composer can dispatch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Machine {
    /// Display name and the `--hosts` selector the fleet already uses.
    pub(crate) name: String,
    /// Optional sidebar glyph, resolved from the local fleet configuration.
    pub(crate) icon: Option<String>,
    /// `None` for this host. `Some(target)` is an OpenSSH destination.
    pub(crate) target: Option<String>,
    /// Herdr socket override on that host, when its fleet entry declares one.
    pub(crate) socket: Option<String>,
}

impl Machine {
    pub(crate) fn is_local(&self) -> bool {
        self.target.is_none()
    }
}

/// What one remote dispatch needs to know. Separated from `HomeDispatchPlan`
/// so command construction stays testable without a composer or a PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteDispatch {
    pub(crate) target: String,
    pub(crate) socket: Option<String>,
    pub(crate) directory: String,
    pub(crate) label: Option<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) argv: Vec<String>,
}

/// The name a machine list falls back to when no fleet is configured.
const LOCAL_MACHINE: &str = "this machine";

/// Build the composer's machine list from the configured fleet.
///
/// The local machine is always first and always present: dispatching here is
/// the default, and a fleet that forgot to declare a local host must not make
/// the composer unable to launch anything.
pub(crate) fn resolve(fleet: &FleetConfig) -> Vec<Machine> {
    let mut machines: Vec<Machine> = Vec::with_capacity(fleet.hosts.len() + 1);
    let mut remote = Vec::new();
    for host in &fleet.hosts {
        if host.name.trim().is_empty() {
            tracing::warn!("ignoring a fleet host with no name");
            continue;
        }
        let machine = machine_from_host(host);
        if machine.is_local() {
            machines.push(machine);
        } else {
            remote.push(machine);
        }
    }
    if machines.is_empty() {
        machines.push(Machine {
            name: LOCAL_MACHINE.into(),
            icon: None,
            target: None,
            socket: None,
        });
    }
    // A fleet may declare more than one local host. Keep the first as the local
    // entry so the list has exactly one, then append the remotes behind it.
    machines.truncate(1);
    machines.extend(remote);
    machines
}

fn machine_from_host(host: &FleetHostConfig) -> Machine {
    let target = if host.local || host.target.trim().is_empty() {
        None
    } else {
        Some(host.target.clone())
    };
    Machine {
        name: host.name.clone(),
        icon: host.icon.clone(),
        target,
        socket: host.socket.clone(),
    }
}

/// Wrap a value so a remote shell sees exactly these bytes.
///
/// SSH joins its command arguments into one string and hands it to a shell, so
/// anything not quoted here is re-split there. A prompt is the argument most
/// likely to carry spaces and quotes, and it is also the one whose corruption
/// would be silent.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_prefix(socket: Option<&str>) -> String {
    match socket {
        Some(socket) => format!("HERDR_SOCKET_PATH={} herdr", shell_quote(socket)),
        None => "herdr".to_string(),
    }
}

fn ssh_argv(target: &str, remote_command: String) -> Vec<String> {
    vec![
        "ssh".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
        // Terminate option parsing: a target that begins with a dash would
        // otherwise reach ssh as an option. `src/remote/control.rs` does the same.
        "--".into(),
        target.into(),
        remote_command,
    ]
}

/// Step one: ask the target's server for a workspace, and with it a pane.
pub(crate) fn create_workspace_command(dispatch: &RemoteDispatch) -> Vec<String> {
    let mut remote = format!(
        "{} workspace create --cwd {} --no-focus",
        remote_prefix(dispatch.socket.as_deref()),
        shell_quote(&dispatch.directory)
    );
    if let Some(label) = &dispatch.label {
        remote.push_str(&format!(" --label {}", shell_quote(label)));
    }
    for (key, value) in &dispatch.env {
        remote.push_str(&format!(
            " --env {}",
            shell_quote(&format!("{key}={value}"))
        ));
    }
    ssh_argv(&dispatch.target, remote)
}

/// Step two: run the agent in that pane, exactly as a local dispatch would.
///
/// `pane run` rather than `agent start` on purpose: it runs the argv the
/// composer built, so a launch profile with its own command reaches the remote
/// machine intact instead of being re-derived from an agent kind.
pub(crate) fn run_in_pane_command(dispatch: &RemoteDispatch, pane_id: &str) -> Vec<String> {
    let mut remote = format!(
        "{} pane run {}",
        remote_prefix(dispatch.socket.as_deref()),
        shell_quote(pane_id)
    );
    for argument in &dispatch.argv {
        remote.push(' ');
        remote.push_str(&shell_quote(argument));
    }
    ssh_argv(&dispatch.target, remote)
}

/// The pane the first call created.
pub(crate) fn pane_id_from_response(stdout: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|error| format!("could not read the response: {error}"))?;
    if let Some(message) = value
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
    {
        return Err(message.to_string());
    }
    value
        .pointer("/result/root_pane/pane_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the response named no pane".to_string())
}

fn run(argv: &[String]) -> Result<String, String> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| "empty command".to_string())?;
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("could not run ssh: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.trim().lines().next_back().unwrap_or("ssh failed");
        return Err(reason.to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Create the pane on the target machine and start the agent in it.
///
/// Returns the remote pane id so the caller can say where the work went. The
/// caller is responsible for keeping this off the render thread: it blocks on
/// two SSH round trips.
pub(crate) fn dispatch(dispatch: &RemoteDispatch) -> Result<String, String> {
    let _ = REMOTE_CALL_TIMEOUT;
    let created = run(&create_workspace_command(dispatch))?;
    let pane_id = pane_id_from_response(&created)?;
    run(&run_in_pane_command(dispatch, &pane_id))?;
    Ok(pane_id)
}

/// A directory as the remote shell should see it.
pub(crate) fn remote_directory(directory: &Path) -> String {
    directory.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, target: &str, local: bool) -> FleetHostConfig {
        FleetHostConfig {
            name: name.into(),
            icon: None,
            target: target.into(),
            local,
            socket: None,
            session: None,
        }
    }

    fn dispatch_for_test() -> RemoteDispatch {
        RemoteDispatch {
            target: "ub1".into(),
            socket: None,
            directory: "/home/ubuntu/Repos/herdr".into(),
            label: Some("fix the thing".into()),
            env: vec![("ANTHROPIC_MODEL".into(), "claude-opus-5".into())],
            argv: vec!["claude".into(), "why isn't it working?".into()],
        }
    }

    #[test]
    fn the_local_machine_is_always_first_and_always_present() {
        let empty = resolve(&FleetConfig::default());
        assert_eq!(empty.len(), 1);
        assert!(empty[0].is_local());

        let fleet = FleetConfig {
            hosts: vec![
                host("ub1", "ub1", false),
                host("ub2", "", true),
                host("mbpro", "mac", false),
            ],
            ..FleetConfig::default()
        };
        let machines = resolve(&fleet);
        let names: Vec<&str> = machines.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["ub2", "ub1", "mbpro"], "the local host leads");
        assert!(machines[0].is_local());
        assert_eq!(machines[1].target.as_deref(), Some("ub1"));
        let mut customized = fleet;
        customized.hosts[0].icon = Some("◆".into());
        assert_eq!(resolve(&customized)[1].icon.as_deref(), Some("◆"));
    }

    #[test]
    fn a_prompt_with_quotes_survives_the_remote_shell() {
        let mut dispatch = dispatch_for_test();
        dispatch.argv = vec!["claude".into(), "it's \"broken\"; rm -rf /".into()];
        let argv = run_in_pane_command(&dispatch, "w3:p1");
        let remote = argv.last().expect("remote command");
        // The whole prompt stays one argument: no unquoted ; or " escapes it.
        assert!(
            remote.ends_with(r#"'claude' 'it'\''s "broken"; rm -rf /'"#),
            "{remote}"
        );
    }

    #[test]
    fn a_socket_override_prefixes_both_calls() {
        let mut dispatch = dispatch_for_test();
        dispatch.socket = Some("/run/herdr.sock".into());
        for argv in [
            create_workspace_command(&dispatch),
            run_in_pane_command(&dispatch, "w3:p1"),
        ] {
            let remote = argv.last().expect("remote command");
            assert!(
                remote.starts_with("HERDR_SOCKET_PATH='/run/herdr.sock' herdr "),
                "{remote}"
            );
        }
    }

    #[test]
    fn the_create_call_carries_the_directory_label_and_env() {
        let argv = create_workspace_command(&dispatch_for_test());
        assert_eq!(argv[0], "ssh");
        assert!(argv.contains(&"BatchMode=yes".to_string()));
        let terminator = argv
            .iter()
            .position(|arg| arg == "--")
            .expect("option terminator");
        assert_eq!(argv[terminator + 1], dispatch_for_test().target);
        let remote = argv.last().expect("remote command");
        assert_eq!(
            remote,
            "herdr workspace create --cwd '/home/ubuntu/Repos/herdr' --no-focus \
             --label 'fix the thing' --env 'ANTHROPIC_MODEL=claude-opus-5'"
        );
    }

    #[test]
    fn the_response_yields_a_pane_or_says_why_not() {
        let created =
            r#"{"id":"cli:workspace:create","result":{"root_pane":{"pane_id":"w30:p1"}}}"#;
        assert_eq!(pane_id_from_response(created), Ok("w30:p1".into()));

        let refused = r#"{"id":"x","error":{"code":"nope","message":"no such directory"}}"#;
        assert_eq!(
            pane_id_from_response(refused),
            Err("no such directory".into())
        );

        assert!(pane_id_from_response("not json").is_err());
        assert!(pane_id_from_response(r#"{"result":{}}"#).is_err());
    }
}
