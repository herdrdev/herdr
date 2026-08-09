use serde::Serialize;

use crate::api;
use crate::api::client::{ApiClient, ApiClientError};

pub(super) fn run_status_command(args: &[String]) -> std::io::Result<i32> {
    let Some((scope, json)) = parse_status_args(args) else {
        return Ok(2);
    };

    match scope {
        StatusScope::Full => print_full_status(json),
        StatusScope::Server => print_server_status(json),
        StatusScope::Client => {
            print_client_status(json)?;
            Ok(0)
        }
        StatusScope::Help => {
            print_status_help();
            Ok(0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusScope {
    Full,
    Server,
    Client,
    Help,
}

fn parse_status_args(args: &[String]) -> Option<(StatusScope, bool)> {
    match args.first().map(|arg| arg.as_str()) {
        None => Some((StatusScope::Full, false)),
        Some("--json") if args.len() == 1 => Some((StatusScope::Full, true)),
        Some("server") => {
            parse_status_scope_args(args, StatusScope::Server, "herdr status server [--json]")
        }
        Some("client") => {
            parse_status_scope_args(args, StatusScope::Client, "herdr status client [--json]")
        }
        Some("help" | "--help" | "-h") => {
            if args.len() > 1 {
                print_status_help();
                return None;
            }
            Some((StatusScope::Help, false))
        }
        Some(_) => {
            print_status_help();
            None
        }
    }
}

fn parse_status_scope_args(
    args: &[String],
    scope: StatusScope,
    usage: &str,
) -> Option<(StatusScope, bool)> {
    match args.get(1).map(|arg| arg.as_str()) {
        None => Some((scope, false)),
        Some("--json") if args.len() == 2 => Some((scope, true)),
        _ => {
            eprintln!("usage: {usage}");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerRuntimeStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
        capabilities: Option<crate::api::schema::ServerCapabilities>,
        build_identity: Option<crate::api::schema::BuildIdentity>,
    },
    NotRunning,
}

fn print_full_status(json: bool) -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;

    if json {
        print_json(&FullStatusJson {
            client: client_status_json(),
            server: server_status_json(&server),
            update: update_status_json(&server),
        })?;
        return Ok(0);
    }

    println!("client:");
    println!("  version: {}", crate::build_info::version());
    println!(
        "  channel: {}",
        crate::config::Config::load().config.update.channel.as_str()
    );
    println!("  protocol: {}", crate::protocol::PROTOCOL_VERSION);
    println!();
    println!("server:");
    print_server_status_body(&server, "  ");
    println!();
    println!("update:");
    println!("  restart_needed: {}", restart_needed_label(&server));

    Ok(0)
}

fn print_server_status(json: bool) -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;
    if json {
        print_json(&server_status_json(&server))?;
        return Ok(0);
    }
    print_server_status_body(&server, "");
    Ok(0)
}

fn print_client_status(json: bool) -> std::io::Result<()> {
    if json {
        print_json(&client_status_json())?;
        return Ok(());
    }

    println!("version: {}", crate::build_info::version());
    println!(
        "channel: {}",
        crate::config::Config::load().config.update.channel.as_str()
    );
    println!("protocol: {}", crate::protocol::PROTOCOL_VERSION);
    println!("binary: {}", current_exe_label());
    Ok(())
}

fn print_server_status_body(server: &ServerRuntimeStatus, indent: &str) {
    match server {
        ServerRuntimeStatus::Running {
            version, protocol, ..
        } => {
            println!("{indent}status: running");
            println!("{indent}version: {}", option_label(version.as_deref()));
            println!("{indent}protocol: {}", protocol_label(*protocol));
            println!("{indent}compatible: {}", compatibility_label(*protocol));
            println!("{indent}socket: {}", api::socket_path().display());
        }
        ServerRuntimeStatus::NotRunning => {
            println!("{indent}status: not running");
            println!("{indent}socket: {}", api::socket_path().display());
        }
    }
}
fn read_server_runtime_status() -> std::io::Result<ServerRuntimeStatus> {
    match ApiClient::local().status() {
        Ok(status) => Ok(ServerRuntimeStatus::Running {
            version: status.version,
            protocol: status.protocol,
            capabilities: status.capabilities,
            build_identity: status.build_identity,
        }),
        Err(ApiClientError::Io(err)) if super::server_not_running_error(&err) => {
            Ok(ServerRuntimeStatus::NotRunning)
        }
        Err(err) => Err(api_client_error_to_io(err)),
    }
}

fn api_client_error_to_io(err: ApiClientError) -> std::io::Error {
    match err {
        ApiClientError::Io(err) => err,
        err => std::io::Error::other(err),
    }
}

fn option_label(value: Option<&str>) -> &str {
    value.unwrap_or("unknown")
}

fn protocol_label(protocol: Option<u32>) -> String {
    protocol
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn compatibility_label(protocol: Option<u32>) -> &'static str {
    match protocol {
        Some(protocol) if protocol == crate::protocol::PROTOCOL_VERSION => "yes",
        Some(_) => "no",
        None => "unknown",
    }
}

fn restart_needed_label(server: &ServerRuntimeStatus) -> &'static str {
    match restart_needed_bool(server) {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

#[derive(Serialize)]
struct FullStatusJson {
    client: ClientStatusJson,
    server: ServerStatusJson,
    update: UpdateStatusJson,
}

#[derive(Serialize)]
struct ClientStatusJson {
    version: String,
    channel: &'static str,
    protocol: u32,
    binary: String,
    session: Option<String>,
    source_commit: Option<String>,
    executable_sha256: Option<String>,
    release_manifest_digest: Option<String>,
}

#[derive(Serialize)]
struct ServerStatusJson {
    status: &'static str,
    running: bool,
    version: Option<String>,
    protocol: Option<u32>,
    capabilities: Option<ServerCapabilitiesJson>,
    compatible: Option<bool>,
    socket: String,
    session: Option<String>,
    restart_needed: Option<bool>,
    source_commit: Option<String>,
    executable_sha256: Option<String>,
    release_manifest_digest: Option<String>,
}

#[derive(Serialize)]
struct ServerCapabilitiesJson {
    live_handoff: bool,
    detached_server_daemon: bool,
    conditional_mutations: ConditionalMutationsJson,
}

#[derive(Serialize)]
struct ConditionalMutationsJson {
    pane_close: u32,
}

#[derive(Serialize)]
struct UpdateStatusJson {
    restart_needed: Option<bool>,
}

fn client_status_json() -> ClientStatusJson {
    ClientStatusJson {
        version: crate::build_info::version(),
        channel: crate::config::Config::load().config.update.channel.as_str(),
        protocol: crate::protocol::PROTOCOL_VERSION,
        binary: current_exe_label(),
        session: crate::session::active_name(),
        source_commit: crate::build_info::source_commit().map(str::to_string),
        executable_sha256: crate::build_info::executable_sha256(),
        release_manifest_digest: crate::build_info::release_manifest_digest(),
    }
}

fn server_status_json(server: &ServerRuntimeStatus) -> ServerStatusJson {
    match server {
        ServerRuntimeStatus::Running {
            version,
            protocol,
            capabilities,
            build_identity,
        } => ServerStatusJson {
            status: "running",
            running: true,
            version: version.clone(),
            protocol: *protocol,
            capabilities: capabilities
                .as_ref()
                .map(|capabilities| ServerCapabilitiesJson {
                    live_handoff: capabilities.live_handoff,
                    detached_server_daemon: capabilities.detached_server_daemon,
                    conditional_mutations: ConditionalMutationsJson {
                        pane_close: capabilities.conditional_mutations.pane_close,
                    },
                }),
            compatible: protocol.map(|value| value == crate::protocol::PROTOCOL_VERSION),
            socket: api::socket_path().display().to_string(),
            session: crate::session::active_name(),
            restart_needed: restart_needed_bool(server),
            source_commit: build_identity
                .as_ref()
                .and_then(|identity| identity.source_commit.clone()),
            executable_sha256: build_identity
                .as_ref()
                .and_then(|identity| identity.executable_sha256.clone()),
            release_manifest_digest: build_identity
                .as_ref()
                .and_then(|identity| identity.release_manifest_digest.clone()),
        },
        ServerRuntimeStatus::NotRunning => ServerStatusJson {
            status: "not_running",
            running: false,
            version: None,
            protocol: None,
            capabilities: None,
            compatible: None,
            socket: api::socket_path().display().to_string(),
            session: crate::session::active_name(),
            restart_needed: Some(false),
            source_commit: None,
            executable_sha256: None,
            release_manifest_digest: None,
        },
    }
}

fn update_status_json(server: &ServerRuntimeStatus) -> UpdateStatusJson {
    UpdateStatusJson {
        restart_needed: restart_needed_bool(server),
    }
}

fn restart_needed_bool(server: &ServerRuntimeStatus) -> Option<bool> {
    let ServerRuntimeStatus::Running {
        version,
        protocol,
        capabilities,
        build_identity,
    } = server
    else {
        return Some(false);
    };
    if version.as_deref()? != crate::build_info::version()
        || (*protocol)? != crate::protocol::PROTOCOL_VERSION
    {
        return Some(true);
    }
    let expected_pane_close = u32::from(crate::platform::capabilities().pane_close_if);
    if capabilities
        .as_ref()
        .map(|value| value.conditional_mutations.pane_close)?
        != expected_pane_close
    {
        return Some(true);
    }
    let server_executable = build_identity.as_ref()?.executable_sha256.as_deref()?;
    let client_executable = crate::build_info::executable_sha256()?;
    if server_executable != client_executable {
        return Some(true);
    }
    if let (Some(server_manifest), Some(client_manifest)) = (
        build_identity
            .as_ref()
            .and_then(|value| value.release_manifest_digest.as_deref()),
        crate::build_info::release_manifest_digest(),
    ) {
        if server_manifest != client_manifest {
            return Some(true);
        }
    }
    Some(false)
}

fn print_json(value: &impl Serialize) -> std::io::Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn current_exe_label() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unknown ({err})"))
}

fn print_status_help() {
    eprintln!("herdr status commands:");
    eprintln!("  herdr status [--json]         show local client and running server status");
    eprintln!("  herdr status server [--json]  show running server status");
    eprintln!("  herdr status client [--json]  show local client binary status");
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_status_json_keeps_absent_build_identity_keys_visible() {
        let status = server_status_json(&ServerRuntimeStatus::Running {
            version: Some("0.1.0".into()),
            protocol: Some(crate::protocol::PROTOCOL_VERSION),
            capabilities: Some(crate::api::schema::ServerCapabilities {
                live_handoff: true,
                detached_server_daemon: false,
                conditional_mutations: crate::api::schema::ConditionalMutations { pane_close: 1 },
            }),
            build_identity: Some(crate::api::schema::BuildIdentity::default()),
        });
        let json = serde_json::to_value(status).unwrap();
        assert!(json.get("source_commit").is_some());
        assert!(json.get("executable_sha256").is_some());
        assert!(json.get("release_manifest_digest").is_some());
        assert!(json["source_commit"].is_null());
    }

    #[test]
    fn restart_needed_bool_requires_a_full_runtime_match() {
        let executable_sha256 =
            crate::build_info::executable_sha256().expect("test executable has an identity");
        let matching_capabilities = crate::api::schema::ServerCapabilities {
            conditional_mutations: crate::api::schema::ConditionalMutations {
                pane_close: u32::from(crate::platform::capabilities().pane_close_if),
            },
            ..Default::default()
        };
        let matching_identity = crate::api::schema::BuildIdentity {
            executable_sha256: Some(executable_sha256.clone()),
            ..Default::default()
        };
        let matching = ServerRuntimeStatus::Running {
            version: Some(crate::build_info::version()),
            protocol: Some(crate::protocol::PROTOCOL_VERSION),
            capabilities: Some(matching_capabilities),
            build_identity: Some(matching_identity),
        };
        assert_eq!(restart_needed_bool(&matching), Some(false));

        let mut protocol_mismatch = matching.clone();
        if let ServerRuntimeStatus::Running { protocol, .. } = &mut protocol_mismatch {
            *protocol = Some(crate::protocol::PROTOCOL_VERSION + 1);
        }
        assert_eq!(restart_needed_bool(&protocol_mismatch), Some(true));

        let mut capability_mismatch = matching.clone();
        if let ServerRuntimeStatus::Running { capabilities, .. } = &mut capability_mismatch {
            capabilities
                .as_mut()
                .unwrap()
                .conditional_mutations
                .pane_close += 1;
        }
        assert_eq!(restart_needed_bool(&capability_mismatch), Some(true));

        let mut identity_mismatch = matching.clone();
        if let ServerRuntimeStatus::Running { build_identity, .. } = &mut identity_mismatch {
            build_identity.as_mut().unwrap().executable_sha256 =
                Some(if executable_sha256 == "0".repeat(64) {
                    "1".repeat(64)
                } else {
                    "0".repeat(64)
                });
        }
        assert_eq!(restart_needed_bool(&identity_mismatch), Some(true));

        let mut missing_identity = matching;
        if let ServerRuntimeStatus::Running { build_identity, .. } = &mut missing_identity {
            *build_identity = Some(crate::api::schema::BuildIdentity::default());
        }
        assert_eq!(restart_needed_bool(&missing_identity), None);
    }
}
