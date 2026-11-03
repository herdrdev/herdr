use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::api::client::{ApiClient, ApiClientError};
use crate::api::schema::{Method, Request};

macro_rules! print {
    ($($arg:tt)*) => {{
        crate::platform::begin_cli_output();
        std::print!($($arg)*);
    }};
}

macro_rules! println {
    ($($arg:tt)*) => {{
        crate::platform::begin_cli_output();
        std::println!($($arg)*);
    }};
}

pub(crate) mod agent;
mod api;
mod completion;
mod integration;
mod machine;
pub(crate) mod notification;
pub(crate) mod pane;
pub(crate) mod plugin;
pub(crate) mod projection;
mod protocol_guard;
mod server;
mod server_not_running;
mod spec;
mod status;
pub(crate) mod tab;
mod target;
mod title;
mod workspace;
pub(crate) mod workspace_bindings;
pub(crate) mod worktree;

const TERMINAL_SESSION_OBSERVE_USAGE: &str =
    "Uso: herdr terminal session observe <target> [--cols N] [--rows N]";
const TERMINAL_SESSION_CONTROL_USAGE: &str =
    "Uso: herdr terminal session control <target> [--takeover] [--cols N] [--rows N]";
pub(crate) const AGENT_HELP_FOOTER: &str = concat!(
    "Você é uma IA? Use estes recursos SOMENTE SE sua tarefa pedir especificamente:\n",
    "  Controlar painéis, agentes ou espaços de trabalho:\n",
    "    IGNORE se a skill Herdr já estiver no seu contexto. Caso contrário, execute: herdr --skill\n",
    "  Ajudar uma pessoa a entender, configurar ou diagnosticar o Herdr:\n",
    "    consulte o código e a documentação: https://github.com/herdrdev/herdr",
);

pub(crate) fn parse_token_assignment(raw: &str) -> Result<(String, Option<String>), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err("token deve usar NAME=VALUE".into());
    };
    if key.is_empty() {
        return Err("nome do token não pode estar vazio".into());
    }
    Ok((key.to_string(), Some(value.to_string())))
}

pub(crate) fn parse_env_assignment(raw: &str) -> Result<(String, String), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err("a variável de ambiente deve usar KEY=VALUE".into());
    };
    if key.is_empty() {
        return Err("o nome da variável de ambiente não pode estar vazio".into());
    }
    if key.contains('\0') || value.contains('\0') {
        return Err("a variável de ambiente não pode conter bytes NUL".into());
    }
    Ok((key.to_string(), value.to_string()))
}

pub enum CommandOutcome {
    Handled(i32),
    NotCli,
}

pub(super) fn print_read_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        eprintln!("{response}");
        return Ok(1);
    }
    if let Some(text) = response["result"]["read"]["text"].as_str() {
        print!("{text}");
    }
    Ok(0)
}

pub(crate) fn maybe_run_machine(args: &[String]) -> Option<std::io::Result<CommandOutcome>> {
    target::maybe_run(args)
}

pub fn maybe_run(args: &[String]) -> std::io::Result<CommandOutcome> {
    let Some(command) = args.get(1).map(|arg| arg.as_str()) else {
        return Ok(CommandOutcome::NotCli);
    };

    if spec::print_requested_help(args)? {
        return Ok(CommandOutcome::Handled(0));
    }

    if args.len() == 2 {
        if let Some(group) = api_group(command) {
            eprintln!("{}", projection::group_commands(&group));
            return Ok(CommandOutcome::Handled(2));
        }
    }

    let exit_code = match command {
        "server" => {
            let Some(exit_code) = server::run_server_command(&args[2..])? else {
                return Ok(CommandOutcome::NotCli);
            };
            exit_code
        }
        "api" => api::run_api_command(&args[2..])?,
        "status" => status::run_status_command(&args[2..])?,
        "completion" | "completions" => completion::run_completion_command(&args[2..])?,
        "config" => run_config_command(&args[2..])?,
        "channel" => run_channel_command(&args[2..])?,
        "machine" => machine::run_machine_command(&args[2..])?,
        "workspace" => workspace::run_workspace_command(&args[2..])?,
        "worktree" => worktree::run_worktree_command(&args[2..])?,
        "tab" => tab::run_tab_command(&args[2..])?,
        "notification" => notification::run_notification_command(&args[2..])?,
        "agent" => agent::run_agent_command(&args[2..])?,
        "terminal" => run_terminal_command(&args[2..])?,
        "pane" => pane::run_pane_command(&args[2..])?,
        "plugin" => plugin::run_plugin_command(&args[2..])?,
        "integration" => integration::run_integration_command(&args[2..])?,
        "session" => run_session_command(&args[2..])?,
        _ => return Ok(CommandOutcome::NotCli),
    };

    Ok(CommandOutcome::Handled(exit_code))
}

/// API-backed command groups, which list their commands when run without one.
fn api_group(name: &str) -> Option<clap::Command> {
    Some(match name {
        "workspace" => workspace_bindings::command(),
        "worktree" => worktree::command(),
        "tab" => tab::command(),
        "notification" => notification::command(),
        "agent" => agent::command(),
        "pane" => pane::command(),
        "plugin" => plugin::command(),
        _ => return None,
    })
}

fn run_channel_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("set") => channel_set(&args[1..]),
        Some("show") if args.len() == 1 => {
            let config = crate::config::Config::load().config;
            println!("{}", config.update.channel.as_str());
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            print_channel_help();
            Ok(0)
        }
        _ => {
            print_channel_help();
            Ok(2)
        }
    }
}

fn channel_set(args: &[String]) -> std::io::Result<i32> {
    if !crate::update::upstream_updates_allowed() {
        eprintln!("Atualize o Pretorian usando o instalador aprovado.");
        return Ok(1);
    }
    let Some(channel) = parse_channel_set_arg(args) else {
        eprintln!("Uso: herdr channel set <stable|preview>");
        return Ok(2);
    };

    if let Some(reason) = channel_set_rejection(
        channel,
        crate::update::preview_channel_rejection_for_current_install(),
    ) {
        eprintln!("{reason}.");
        return Ok(1);
    }

    let path = crate::config::config_path();
    let content = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if let Err(err) = content.parse::<toml::Value>() {
        eprintln!(
            "config file at {} is invalid TOML: {err}. Fix it before changing the update channel.",
            path.display()
        );
        return Ok(1);
    }

    let updated = crate::config::upsert_section_value(
        &content,
        "update",
        "channel",
        &format!("\"{channel}\""),
    );
    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "changing the update channel would make {} invalid TOML: {err}; leaving config unchanged",
            path.display()
        );
        return Ok(1);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)?;
    println!(
        "Herdr update channel set to {channel} in {}.",
        path.display()
    );

    match channel_set_install_action(
        crate::update::package_manager_channel_update_guidance_for_current_install(),
    ) {
        ChannelSetInstallAction::PrintGuidance(guidance) => {
            println!("{guidance}");
            return Ok(0);
        }
        ChannelSetInstallAction::RunSelfUpdate => {}
    }

    crate::platform::end_cli_output();
    if let Err(err) = crate::update::self_update(crate::update::SelfUpdateOptions::default()) {
        eprintln!("update failed: {err}");
        eprintln!("Run `herdr update` to retry.");
        return Ok(1);
    }

    Ok(0)
}

fn parse_channel_set_arg(args: &[String]) -> Option<&str> {
    let channel = args.first().map(|arg| arg.as_str())?;
    if args.len() == 1 && matches!(channel, "stable" | "preview") {
        Some(channel)
    } else {
        None
    }
}

fn channel_set_rejection(
    channel: &str,
    install_rejection: Option<&'static str>,
) -> Option<&'static str> {
    if channel == "preview" {
        return install_rejection;
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelSetInstallAction {
    RunSelfUpdate,
    PrintGuidance(&'static str),
}

fn channel_set_install_action(
    package_manager_guidance: Option<&'static str>,
) -> ChannelSetInstallAction {
    match package_manager_guidance {
        Some(guidance) => ChannelSetInstallAction::PrintGuidance(guidance),
        None => ChannelSetInstallAction::RunSelfUpdate,
    }
}

fn print_channel_help() {
    eprintln!("Comandos de herdr channel:");
    eprintln!("  herdr channel show                  mostrar o canal de atualização configurado");
    eprintln!(
        "  herdr channel set <stable|preview>  alteração indisponível; use o instalador Pretorian"
    );
}

fn run_config_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_config_help();
        return Ok(2);
    };

    match subcommand {
        "check" => config_check(&args[1..]),
        "reset-keys" => config_reset_keys(&args[1..]),
        "help" | "--help" | "-h" => {
            print_config_help();
            Ok(0)
        }
        _ => {
            print_config_help();
            Ok(2)
        }
    }
}

fn config_check(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => {}
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            eprintln!("Uso: herdr config check");
            return Ok(0);
        }
        _ => {
            eprintln!("Uso: herdr config check");
            return Ok(2);
        }
    }

    let diagnostics = crate::config::Config::load().diagnostics;
    if diagnostics.is_empty() {
        println!("config: ok");
    } else {
        println!("config: problemas encontrados");
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
    }

    Ok(i32::from(!diagnostics.is_empty()))
}

fn config_reset_keys(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("Uso: herdr config reset-keys");
        return Ok(2);
    }

    let path = crate::config::config_path();
    if !path.exists() {
        println!(
            "Nenhum arquivo de configuração encontrado em {}. Os atalhos padrão v2 já estão ativos.",
            path.display()
        );
        return Ok(0);
    }

    let content = std::fs::read_to_string(&path)?;
    let parsed = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "O arquivo de configuração em {} contém TOML inválido: {err}. Corrija-o ou mova-o para usar os padrões.",
                path.display()
            );
            return Ok(1);
        }
    };
    let Some(table) = parsed.as_table() else {
        eprintln!(
            "O arquivo de configuração em {} contém TOML inválido: a configuração principal deve ser uma tabela.",
            path.display()
        );
        return Ok(1);
    };

    if !table.contains_key("keys") {
        println!(
            "Nenhuma configuração [keys] encontrada em {}. Os atalhos padrão v2 já estão ativos.",
            path.display()
        );
        return Ok(0);
    }

    let (updated, removed) = crate::config::remove_keybinding_config_sections(&content);
    if !removed {
        eprintln!(
            "Não foi possível remover os atalhos de {} com segurança sem reescrever comentários; edite o arquivo ou remova a configuração keys principal.",
            path.display()
        );
        return Ok(1);
    }
    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "Remover os atalhos tornaria {} um TOML inválido: {err}; configuração preservada",
            path.display()
        );
        return Ok(1);
    }

    let backup_path = key_config_backup_path(&path);
    std::fs::copy(&path, &backup_path)?;
    std::fs::write(&path, updated)?;

    println!("Cópia de segurança criada: {}", backup_path.display());
    println!(
        "Removidos [keys], [keys.indexed] e [[keys.command]] de {}.",
        path.display()
    );
    println!(
        "Os atalhos padrão v2 serão aplicados após reiniciar o Herdr ou recarregar a configuração."
    );
    println!(
        "Se o servidor Herdr estiver ativo, execute `herdr server reload-config` para aplicar agora."
    );
    println!(
        "Para restaurar: cp {} {}",
        backup_path.display(),
        path.display()
    );
    Ok(0)
}

fn key_config_backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    path.with_file_name(format!("{file_name}.bak-keybind-v2-{timestamp}"))
}

fn run_terminal_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_terminal_help();
        return Ok(2);
    };

    match subcommand {
        "attach" => terminal_attach(&args[1..]),
        "session" => terminal_session(&args[1..]),
        "title" => terminal_title(&args[1..]),
        "help" | "--help" | "-h" => {
            print_terminal_help();
            Ok(0)
        }
        _ => {
            print_terminal_help();
            Ok(2)
        }
    }
}

fn run_session_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_session_help();
        return Ok(2);
    };

    match subcommand {
        "list" => session_list(&args[1..]),
        "attach" => session_attach_help(&args[1..]),
        "stop" => session_stop(&args[1..]),
        "delete" => session_delete(&args[1..]),
        "help" | "--help" | "-h" => {
            print_session_help();
            Ok(0)
        }
        _ => {
            print_session_help();
            Ok(2)
        }
    }
}

fn session_attach_help(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("Uso: herdr session attach <name>");
        return Ok(0);
    }
    eprintln!("Uso: herdr session attach <name>");
    Ok(2)
}

fn session_list(args: &[String]) -> std::io::Result<i32> {
    let json = match parse_session_json_only(args, "Uso: herdr session list [--json]") {
        Ok(json) => json,
        Err(code) => return Ok(code),
    };

    let sessions = crate::session::list_sessions()?;
    if json {
        _print_json(&serde_json::json!({
            "sessions": sessions,
        }));
    } else {
        print_session_table(&sessions);
    }
    Ok(0)
}

fn session_stop(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "Uso: herdr session stop <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let target = match crate::session::parse_target_name(&name) {
        Ok(target) => target,
        Err(message) => {
            print_session_error("invalid_session_name", &message);
            return Ok(1);
        }
    };
    match crate::session::stop_session(target.as_deref()) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "stopped": true,
                    "session": session,
                }));
            } else {
                println!("sessão encerrada {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_stop_failed", &message);
            Ok(1)
        }
    }
}

fn session_delete(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "Uso: herdr session delete <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    match crate::session::delete_session(&name) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "deleted": true,
                    "session": session,
                }));
            } else {
                println!("sessão excluída {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_delete_failed", &message);
            Ok(1)
        }
    }
}

fn terminal_attach(args: &[String]) -> std::io::Result<i32> {
    let (terminal_id, takeover) = match parse_attach_target(
        args,
        "Uso: herdr terminal attach <terminal_id> [--takeover]",
    ) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(code),
    };
    crate::client::run_terminal_attach(terminal_id, takeover)?;
    Ok(0)
}

fn terminal_session(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("control") => terminal_session_control(&args[1..]),
        Some("observe") => terminal_session_observe(&args[1..]),
        Some("help" | "--help" | "-h") => {
            eprintln!("{TERMINAL_SESSION_CONTROL_USAGE}");
            eprintln!("{TERMINAL_SESSION_OBSERVE_USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{TERMINAL_SESSION_CONTROL_USAGE}");
            eprintln!("{TERMINAL_SESSION_OBSERVE_USAGE}");
            Ok(2)
        }
    }
}

fn terminal_session_control(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_terminal_session_options(
        args,
        TERMINAL_SESSION_CONTROL_USAGE,
        "control",
        true,
    )? {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };

    crate::client::run_terminal_session_control(
        options.target,
        options.takeover,
        options.cols,
        options.rows,
    )?;
    Ok(0)
}

fn terminal_session_observe(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_terminal_session_options(
        args,
        TERMINAL_SESSION_OBSERVE_USAGE,
        "observe",
        false,
    )? {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };

    crate::client::run_terminal_session_observe(options.target, options.cols, options.rows)?;
    Ok(0)
}

struct TerminalSessionOptions {
    target: String,
    cols: u16,
    rows: u16,
    takeover: bool,
}

fn parse_terminal_session_options(
    args: &[String],
    usage: &str,
    command: &str,
    allow_takeover: bool,
) -> std::io::Result<Result<TerminalSessionOptions, i32>> {
    if matches!(
        args.first().map(|arg| arg.as_str()),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{usage}");
        return Ok(Err(0));
    }
    let Some(target) = args.first() else {
        eprintln!("{usage}");
        return Ok(Err(2));
    };

    let mut cols = 120;
    let mut rows = 40;
    let mut takeover = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--takeover" if allow_takeover => {
                takeover = true;
                i += 1;
            }
            "--cols" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{usage}");
                    return Ok(Err(2));
                };
                cols = parse_terminal_dimension(value, "--cols")?;
                i += 2;
            }
            "--rows" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{usage}");
                    return Ok(Err(2));
                };
                rows = parse_terminal_dimension(value, "--rows")?;
                i += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("{usage}");
                return Ok(Err(0));
            }
            other => {
                eprintln!("opção desconhecida de terminal session {command}: {other}");
                eprintln!("{usage}");
                return Ok(Err(2));
            }
        }
    }

    Ok(Ok(TerminalSessionOptions {
        target: target.clone(),
        cols,
        rows,
        takeover,
    }))
}

fn parse_terminal_dimension(raw: &str, flag: &str) -> std::io::Result<u16> {
    let parsed = raw.parse::<u16>().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{flag} deve ser um inteiro entre 1 e {}", u16::MAX),
        )
    })?;
    if parsed == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{flag} deve ser maior que 0"),
        ));
    }
    Ok(parsed)
}

fn terminal_title(args: &[String]) -> std::io::Result<i32> {
    title::run(args)
}

pub(super) fn parse_attach_target(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let Some(target) = args.first() else {
        eprintln!("{usage}");
        return Err(2);
    };
    let mut takeover = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--takeover" => takeover = true,
            "help" | "--help" | "-h" => {
                eprintln!("{usage}");
                return Err(0);
            }
            other => {
                eprintln!("opção desconhecida: {other}");
                return Err(2);
            }
        }
    }
    Ok((target.clone(), takeover))
}

pub(super) fn print_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(response).unwrap());
    Ok(0)
}

/// Sends a parsed request and prints its JSON response; parse failures never connect.
fn run_json_request(request: Result<Request, projection::ParseError>) -> std::io::Result<i32> {
    let request = match request {
        Ok(request) => request,
        Err(projection::ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(projection::ParseError::Io(error)) => return Err(error),
        Err(projection::ParseError::Clap(error)) => return print_clap_error(&error),
    };
    print_response(&send_request(&request)?)
}

/// Prints a Clap diagnostic or help page and returns its exit code.
pub(super) fn print_clap_error(error: &clap::Error) -> std::io::Result<i32> {
    crate::platform::begin_cli_output();
    error.print()?;
    Ok(error.exit_code())
}

pub(super) fn send_ok_request(method: Method) -> std::io::Result<i32> {
    let response = send_request(&Request {
        id: "cli:request".into(),
        method,
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    Ok(0)
}

pub(super) fn send_request(request: &Request) -> std::io::Result<serde_json::Value> {
    let client = target::api_client()?;
    ensure_server_protocol_compatible(&client, &request.id)?;
    client
        .request_value(request)
        .map_err(|err| map_server_not_running_or_io(err, &request.id, &client))
}

pub(super) fn send_request_unchecked(request: &Request) -> std::io::Result<serde_json::Value> {
    let client = target::api_client()?;
    client
        .request_value(request)
        .map_err(|err| map_server_not_running_or_io(err, &request.id, &client))
}

fn ensure_server_protocol_compatible(client: &ApiClient, request_id: &str) -> std::io::Result<()> {
    let status = target::server_status(client)
        .map_err(|err| map_server_not_running_or_io(err, request_id, client))?;
    let server_protocol = status.protocol.ok_or_else(|| {
        std::io::Error::other("a resposta do servidor não incluiu a versão do protocolo")
    })?;
    let Some(response) =
        protocol_guard::mismatch_response(request_id, server_protocol, &target::restart_guidance())
    else {
        return Ok(());
    };

    eprintln!(
        "{}",
        serde_json::to_string(&response).map_err(std::io::Error::other)?
    );
    Err(protocol_guard::reported_error())
}

pub(crate) fn protocol_mismatch_was_reported(err: &std::io::Error) -> bool {
    protocol_guard::was_reported(err)
}

pub(crate) fn server_not_running_was_reported(err: &std::io::Error) -> bool {
    server_not_running::was_reported(err)
}

/// Returns the `ErrorResponse` carried by a `server_not_running` marker, if any,
/// so the edge that surfaces the error can print it exactly once (deferred
/// printing: recovering callers like plugin offline fallback print nothing).
pub(crate) fn server_not_running_reported_response(
    err: &std::io::Error,
) -> Option<&crate::api::schema::ErrorResponse> {
    server_not_running::reported_response(err)
}

/// True when an io::Error indicates nothing is listening on the API socket.
/// Classify by `ErrorKind` only: Windows named pipes surface different raw
/// errno values than Unix domain sockets but the same error kinds.
pub(super) fn server_not_running_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Maps an `ApiClientError` from a socket command into the io::Error that
/// bubbles up to `main`. A dead-server connect failure is reported as a
/// friendly `server_not_running` JSON error plus a recognizable marker; all
/// other errors fall through unchanged so existing handling is preserved.
fn map_server_not_running_or_io(
    err: ApiClientError,
    request_id: &str,
    client: &ApiClient,
) -> std::io::Error {
    if target::is_remote() {
        return target::remote_error(api_client_error_to_io(err));
    }
    match err {
        ApiClientError::Io(io_err) if server_not_running_error(&io_err) => {
            server_not_running::reported_error(server_not_running::response(
                request_id,
                &client.socket_path(),
            ))
        }
        err => api_client_error_to_io(err),
    }
}

fn api_client_error_to_io(err: ApiClientError) -> std::io::Error {
    match err {
        ApiClientError::Io(err) => err,
        err => std::io::Error::other(err),
    }
}

pub(super) fn normalize_workspace_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn normalize_tab_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn normalize_pane_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn parse_u32_flag(flag: &str, value: &str) -> std::io::Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| std::io::Error::other(format!("valor inválido para {flag}: {value}")))
}

pub(super) fn parse_u64_flag(flag: &str, value: &str) -> std::io::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| std::io::Error::other(format!("valor inválido para {flag}: {value}")))
}

/// Expand `--flag=value` tokens into separate `--flag` and `value` tokens so
/// the hand-rolled subcommand parsers accept the same `--flag=value` form the
/// clap-generated help and completions imply. Only `value_options` are split:
/// boolean and unknown options keep their attached value so they still reach
/// the parser's unknown-option branch.
pub(super) fn expand_equals_args(args: &[String], value_options: &[&str]) -> Vec<String> {
    let mut expanded = Vec::with_capacity(args.len());
    for arg in args {
        match arg.split_once('=') {
            Some((flag, value)) if value_options.contains(&flag) => {
                expanded.push(flag.to_string());
                expanded.push(value.to_string());
            }
            _ => expanded.push(arg.clone()),
        }
    }
    expanded
}

fn parse_session_json_only(args: &[String], usage: &str) -> Result<bool, i32> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn parse_session_name_and_json(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let mut name = None;
    let mut json = false;
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
        } else if !options_ended && arg == "--json" {
            json = true;
        } else if name.is_none() {
            name = Some(arg.clone());
        } else {
            eprintln!("{usage}");
            return Err(2);
        }
    }

    let Some(name) = name else {
        eprintln!("{usage}");
        return Err(2);
    };
    Ok((name, json))
}

fn print_session_table(sessions: &[crate::session::SessionInfo]) {
    println!("{:<20} {:<12} {:<48} socket", "nome", "estado", "diretório");
    for session in sessions {
        println!(
            "{:<20} {:<12} {:<48} {}",
            session.name,
            if session.connection_error.is_some() {
                "indisponível"
            } else if session.running {
                "ativa"
            } else {
                "encerrada"
            },
            session.session_dir,
            session.socket_path
        );
        if let Some(error) = &session.connection_error {
            println!("  {error}");
        }
    }
}

fn print_session_error(code: &str, message: &str) {
    eprintln!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        }))
        .unwrap()
    );
}

fn print_config_help() {
    eprintln!("Comandos de herdr config:");
    eprintln!("  herdr config check  validar config.toml e mostrar diagnósticos");
    eprintln!(
        "  herdr config reset-keys  salvar cópia de config.toml e remover atalhos personalizados"
    );
}

fn print_terminal_help() {
    eprintln!("Comandos de herdr terminal:");
    eprintln!("  herdr terminal attach <terminal_id> [--takeover]");
    eprintln!("  herdr terminal session control <target> [--takeover] [--cols N] [--rows N]");
    eprintln!("  herdr terminal session observe <target> [--cols N] [--rows N]");
    eprintln!("  herdr terminal title set <title>");
    eprintln!("  herdr terminal title clear");
    eprintln!("  desconecte com ctrl+b q; envie ctrl+b literal com ctrl+b ctrl+b");
}

fn print_session_help() {
    eprintln!("Comandos de herdr session:");
    eprintln!("  herdr session list [--json]");
    eprintln!("  herdr session attach <name>");
    eprintln!("  herdr session stop <name> [--json]");
    eprintln!("  herdr session delete <name> [--json]");
    eprintln!("  use 'default' como <name> para encerrar a sessão padrão");
}

fn _print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).unwrap());
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_channel_set_argument() {
        assert_eq!(
            super::parse_channel_set_arg(&["preview".to_string()]),
            Some("preview")
        );
        assert_eq!(
            super::parse_channel_set_arg(&["stable".to_string()]),
            Some("stable")
        );
        assert_eq!(super::parse_channel_set_arg(&["nightly".to_string()]), None);
        assert_eq!(
            super::parse_channel_set_arg(&["preview".to_string(), "stable".to_string()]),
            None
        );
    }

    #[test]
    fn channel_set_only_applies_package_rejection_to_preview() {
        assert_eq!(
            super::channel_set_rejection("preview", Some("no preview")),
            Some("no preview")
        );
        assert_eq!(
            super::channel_set_rejection("stable", Some("no preview")),
            None
        );
        assert_eq!(super::channel_set_rejection("preview", None), None);
    }

    #[test]
    fn channel_set_skips_self_update_for_package_manager_guidance() {
        assert_eq!(
            super::channel_set_install_action(Some("use package manager")),
            super::ChannelSetInstallAction::PrintGuidance("use package manager")
        );
        assert_eq!(
            super::channel_set_install_action(None),
            super::ChannelSetInstallAction::RunSelfUpdate
        );
    }

    #[test]
    fn session_name_parser_accepts_option_terminator() {
        for name in ["-h", "--json"] {
            assert_eq!(
                super::parse_session_name_and_json(&["--".to_string(), name.to_string()], "usage",),
                Ok((name.to_string(), false))
            );
        }
    }

    #[test]
    fn parse_env_assignment_accepts_empty_values() {
        assert_eq!(
            super::parse_env_assignment("HERDR_ROLE=").unwrap(),
            ("HERDR_ROLE".to_string(), String::new())
        );
    }

    #[test]
    fn parse_env_assignment_requires_key_value_separator() {
        assert_eq!(
            super::parse_env_assignment("HERDR_ROLE").unwrap_err(),
            "a variável de ambiente deve usar KEY=VALUE"
        );
    }

    #[test]
    fn maps_dead_server_connect_failure_to_friendly_error() {
        use crate::api::client::{ApiClient, ApiClientError};

        let client = ApiClient::local();
        let socket = client.socket_path().display().to_string();

        // The helper does NOT print; it returns a recognizable marker carrying
        // the ErrorResponse so the surfacing edge can print it exactly once.
        let mapped = super::map_server_not_running_or_io(
            ApiClientError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)),
            "cli:workspace:create",
            &client,
        );

        let response = super::server_not_running::reported_response(&mapped)
            .expect("dead-server connect failure should carry a server_not_running response");
        assert_eq!(response.id, "cli:workspace:create");
        assert_eq!(response.error.code, "server_not_running");
        assert!(response.error.message.contains(&socket));

        // The mapping is recognizable without string matching.
        assert!(super::server_not_running::was_reported(&mapped));
    }

    #[test]
    fn classifier_ignores_unrelated_io_kinds() {
        use crate::api::client::{ApiClient, ApiClientError};

        let client = ApiClient::local();
        let mapped = super::map_server_not_running_or_io(
            ApiClientError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            "cli:workspace:create",
            &client,
        );
        assert!(!super::server_not_running::was_reported(&mapped));
    }

    #[test]
    fn expand_equals_args_splits_value_options_only() {
        // Known value options split; values may contain `=`. Boolean and
        // unknown options keep the attached form so parsers still reject them.
        let args = vec![
            "--match=a=b".to_string(),
            "name=value".to_string(),
            "--raw=value".to_string(),
            "--bogus=value".to_string(),
            "--timeout=5000".to_string(),
        ];
        assert_eq!(
            super::expand_equals_args(&args, &["--match", "--timeout"]),
            vec![
                "--match",
                "a=b",
                "name=value",
                "--raw=value",
                "--bogus=value",
                "--timeout",
                "5000",
            ]
        );
    }
}
