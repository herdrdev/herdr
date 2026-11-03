use std::io::Write;

use clap::{Arg, ArgAction, Command, ValueHint};

mod completion;
mod machine;

pub(super) fn command() -> Command {
    let command = Command::new("herdr")
        .about("ambiente de terminal para agentes de IA")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(help_flag())
        .arg(option("session", "NAME").help("Usar ou criar uma sessão persistente nomeada"))
        .arg(
            option("machine", "LABEL-OR-ID")
                .help("Executar comando da API em uma máquina SSH salva"),
        )
        .arg(option("remote", "TARGET").help("Conectar por SSH a um servidor Herdr remoto"))
        .arg(
            option("remote-keybindings", "MODE")
                .value_parser(["local", "server"])
                .help("Escolher atalhos locais ou do servidor para conexão remota"),
        )
        .arg(
            flag("handoff")
                .help("Permitir transferência da sessão na atualização ou conexão remota"),
        )
        .arg(flag("default-config").help("Exibir configuração padrão e sair"))
        .arg(flag("skill").help("Exibir instruções do agente e sair"))
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::SetTrue)
                .help("Exibir versão e sair"),
        )
        .subcommand(completion::command())
        .subcommand(update_command())
        .subcommand(status_command())
        .subcommand(config_command())
        .subcommand(channel_command())
        .subcommand(machine::command())
        .subcommand(server_command())
        .subcommand(api_command())
        .subcommand(workspace_command())
        .subcommand(worktree_command())
        .subcommand(tab_command())
        .subcommand(notification_command())
        .subcommand(agent_command())
        .subcommand(pane_command())
        .subcommand(terminal_command())
        .subcommand(session_command())
        .subcommand(integration_command())
        .subcommand(plugin_command());
    configure_help(command, 0)
}

pub(super) fn configure_help(command: Command, depth: usize) -> Command {
    let command = if depth == 0 {
        command
    } else {
        command.disable_help_flag(true).arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help("Exibir ajuda"),
        )
    };
    let command = if depth == 1 && command.has_subcommands() {
        command.after_help(super::AGENT_HELP_FOOTER)
    } else {
        command
    };
    let mut template = String::from("{before-help}{about-with-newline}\nUso: {usage}\n");
    if command
        .get_subcommands()
        .any(|subcommand| !subcommand.is_hide_set())
    {
        template.push_str("\nComandos:\n{subcommands}\n");
    }
    if command.get_positionals().any(|arg| !arg.is_hide_set()) {
        template.push_str("\nArgumentos:\n{positionals}\n");
    }
    if command
        .get_arguments()
        .any(|arg| !arg.is_positional() && !arg.is_hide_set())
    {
        template.push_str("\nOpções:\n{options}\n");
    }
    template.push_str("{after-help}");
    command
        .help_template(template)
        .mut_args(|arg| {
            let descricao =
                arg.get_help()
                    .map(ToString::to_string)
                    .map(|descricao| match descricao.as_str() {
                        "State to match; repeat for more than one state" => {
                            "Estado a encontrar; repita para mais de um estado".to_string()
                        }
                        "State to match after --wait; repeat for more than one state" => {
                            "Estado a encontrar após --wait; repita para mais de um estado"
                                .to_string()
                        }
                        "Fail after this many milliseconds" => {
                            "Falhar após essa quantidade de milissegundos".to_string()
                        }
                        "Wait for interactive readiness (default: 30000; max: 300000)" => {
                            "Aguardar disponibilidade interativa (padrão: 30000; máximo: 300000)"
                                .to_string()
                        }
                        _ => descricao,
                    });
            let valores = arg
                .get_possible_values()
                .into_iter()
                .filter(|valor| !valor.is_hide_set())
                .map(|valor| match valor.get_help() {
                    Some(ajuda) => format!("{}: {}", valor.get_name(), ajuda),
                    None => valor.get_name().to_string(),
                })
                .collect::<Vec<_>>();
            let tem_valores = !valores.is_empty();
            let descricao = if valores.is_empty() {
                descricao
            } else {
                let valores = format!("Valores possíveis: {}", valores.join(", "));
                Some(match descricao {
                    Some(texto) => format!("{texto}\n\n{valores}"),
                    None => valores,
                })
            };
            let arg = if tem_valores {
                arg.hide_possible_values(true)
            } else {
                arg
            };
            match descricao {
                Some(texto) => arg.help(texto),
                None => arg,
            }
        })
        .disable_help_subcommand(true)
        .mut_subcommands(|subcommand| configure_help(subcommand, depth + 1))
}

pub(super) fn print_requested_help(args: &[String]) -> std::io::Result<bool> {
    let mut stdout = std::io::stdout().lock();
    write_requested_help(args, &mut stdout, crate::platform::begin_cli_output)
}

fn write_requested_help(
    args: &[String],
    output: &mut impl Write,
    before_write: impl FnOnce(),
) -> std::io::Result<bool> {
    let Some(help_index) = args
        .iter()
        .position(|arg| matches!(arg.as_str(), "--help" | "-h"))
    else {
        return Ok(false);
    };
    if help_index < 2 {
        return Ok(false);
    }
    if args[1..help_index].iter().any(|arg| arg == "--") {
        return Ok(false);
    }

    let mut root = command();
    root.build();
    let mut selected = &mut root;
    let mut path = vec!["herdr".to_string()];
    for segment in &args[1..help_index] {
        if selected.find_subcommand(segment).is_none() {
            break;
        }
        path.push(segment.clone());
        selected = selected
            .find_subcommand_mut(segment)
            .expect("subcommand checked immediately before mutable lookup");
    }
    if path.len() == 1 || help_index != path.len() {
        return Ok(false);
    }

    selected.set_bin_name(path.join(" "));
    before_write();
    selected.write_long_help(&mut *output)?;
    writeln!(output)?;
    Ok(true)
}

fn update_command() -> Command {
    Command::new("update")
        .about("Atualização indisponível; use o instalador do Pretorian")
        .arg(flag("handoff").help("Transferência após instalação indisponível neste fork"))
}

fn status_command() -> Command {
    Command::new("status")
        .about("Mostrar estado do cliente local e do servidor ativo")
        .arg(json_flag())
        .subcommand(
            Command::new("server")
                .about("Mostrar estado do servidor ativo")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("client")
                .about("Mostrar estado do cliente local")
                .arg(json_flag()),
        )
}

fn config_command() -> Command {
    Command::new("config")
        .about("Gerenciar configuração local")
        .subcommand(Command::new("check").about("Validar config.toml e exibir diagnóstico"))
        .subcommand(Command::new("reset-keys").about("Remover atalhos personalizados"))
}

fn channel_command() -> Command {
    Command::new("channel")
        .about("Consultar o canal; alteração indisponível neste fork")
        .subcommand(Command::new("show").about("Exibir canal de atualização configurado"))
        .subcommand(
            Command::new("set")
                .about("Alteração de canal indisponível neste fork")
                .arg(
                    Arg::new("channel")
                        .value_name("CHANNEL")
                        .required(true)
                        .value_parser(["stable", "preview"]),
                ),
        )
}

fn server_command() -> Command {
    let command = Command::new("server")
        .about("Executar ou controlar o servidor sem interface")
        .subcommand(super::server::live_handoff_command())
        .subcommand(Command::new("stop").about("Encerrar o servidor ativo"))
        .subcommand(
            Command::new("reload-config").about("Recarregar configuração no servidor ativo"),
        )
        .subcommand(
            Command::new("agent-manifests")
                .about("Mostrar manifestos ativos de detecção de agentes")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("update-agent-manifests")
                .about("Atualização indisponível; use o instalador do Pretorian")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("reload-agent-manifests")
                .about("Recarregar substituições locais dos manifestos de detecção"),
        );
    #[cfg(windows)]
    let command = command.arg(
        flag("allow-unelevated-clients")
            .help("Permitir que clientes comuns da mesma conta controlem este servidor elevado"),
    );
    command
}

fn api_command() -> Command {
    Command::new("api")
        .about("Consultar metadados da API de socket e estado atual")
        .subcommand(Command::new("snapshot").about("Exibir instantâneo da sessão ativa"))
        .subcommand(
            Command::new("schema")
                .about("Exibir ou gravar o esquema da API incluído")
                .arg(json_flag())
                .arg(path_option("output", "PATH")),
        )
}

fn workspace_command() -> Command {
    super::workspace_bindings::command()
}

fn worktree_command() -> Command {
    super::worktree::command()
}

fn tab_command() -> Command {
    super::tab::command()
}

fn notification_command() -> Command {
    super::notification::command()
}

fn agent_command() -> Command {
    super::agent::command()
}

fn pane_command() -> Command {
    super::pane::command()
}

fn terminal_command() -> Command {
    Command::new("terminal")
        .about("Conectar ou observar fluxos brutos de terminal")
        .subcommand(
            Command::new("attach")
                .about("Conectar diretamente a um fluxo de terminal")
                .arg(required("terminal_id", "TERMINAL_ID"))
                .arg(flag("takeover")),
        )
        .subcommand(
            Command::new("session")
                .about("Gerenciar sessões de terminal")
                .subcommand(
                    Command::new("control")
                        .about("Controlar um fluxo de terminal")
                        .arg(required("target", "TARGET"))
                        .arg(flag("takeover"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                )
                .subcommand(
                    Command::new("observe")
                        .about("Observar um fluxo de terminal")
                        .arg(required("target", "TARGET"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                ),
        )
        .subcommand(super::title::command())
}

fn session_command() -> Command {
    Command::new("session")
        .about("Gerenciar sessões persistentes nomeadas")
        .subcommand(
            Command::new("list")
                .about("Listar sessões")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("attach")
                .about("Conectar a uma sessão")
                .arg(required("name", "NAME")),
        )
        .subcommand(
            Command::new("stop")
                .about("Encerrar uma sessão")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("delete")
                .about("Excluir uma sessão encerrada")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
}

fn integration_command() -> Command {
    Command::new("integration")
        .about("Gerenciar integrações de agentes incluídas")
        .subcommand(
            Command::new("install")
                .about("Instalar uma integração")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Desinstalar uma integração")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("status")
                .about("Mostrar estado das integrações")
                .arg(flag("outdated-only")),
        )
}

fn plugin_command() -> Command {
    super::plugin::command()
}

fn integration_target_arg() -> Arg {
    Arg::new("target")
        .value_name("TARGET")
        .required(true)
        .value_parser(integration_target_values())
}

fn integration_target_values() -> Vec<&'static str> {
    let mut values: Vec<&'static str> = crate::api::schema::IntegrationTarget::ALL
        .into_iter()
        .map(crate::integration::integration_target_label)
        .collect();
    values.extend_from_slice(crate::integration::EXPERIMENTAL_INTEGRATION_TARGET_LABELS);
    values
}

fn json_flag() -> Arg {
    flag("json")
}

fn help_flag() -> Arg {
    Arg::new("help")
        .short('h')
        .long("help")
        .action(ArgAction::SetTrue)
        .help("Exibir ajuda")
}

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}

fn option(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .value_name(value_name)
        .action(ArgAction::Set)
}

fn path_option(name: &'static str, value_name: &'static str) -> Arg {
    option(name, value_name).value_hint(ValueHint::AnyPath)
}

fn required(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name).value_name(value_name).required(true)
}

#[cfg(test)]
mod tests {
    use clap::{Arg, Command};

    fn command_path<'a>(cmd: &'a Command, path: &[&str]) -> &'a Command {
        let mut current = cmd;
        for name in path {
            current = current
                .get_subcommands()
                .find(|subcommand| subcommand.get_name() == *name)
                .unwrap_or_else(|| panic!("missing command path segment {name}"));
        }
        current
    }

    fn option_values(cmd: &Command, option: &str) -> Vec<String> {
        let arg = cmd
            .get_arguments()
            .find(|arg| arg.get_long() == Some(option))
            .unwrap_or_else(|| panic!("missing --{option}"));
        arg.get_value_parser()
            .possible_values()
            .into_iter()
            .flatten()
            .map(|value| value.get_name().to_string())
            .collect()
    }

    fn has_option(cmd: &Command, option: &str) -> bool {
        cmd.get_arguments()
            .any(|arg| arg.get_long() == Some(option))
    }

    fn option_arg<'a>(cmd: &'a Command, option: &str) -> &'a Arg {
        cmd.get_arguments()
            .find(|arg| arg.get_long() == Some(option))
            .unwrap_or_else(|| panic!("missing --{option}"))
    }

    fn argument<'a>(cmd: &'a Command, id: &str) -> &'a Arg {
        cmd.get_arguments()
            .find(|arg| arg.get_id() == id)
            .unwrap_or_else(|| panic!("missing argument {id}"))
    }

    fn collect_subcommand_paths(
        cmd: &Command,
        path: &mut Vec<String>,
        paths: &mut Vec<Vec<String>>,
    ) {
        for subcommand in cmd.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            paths.push(path.clone());
            collect_subcommand_paths(subcommand, path, paths);
            path.pop();
        }
    }

    fn assert_command_descriptions(cmd: &Command, path: &mut Vec<String>) {
        if !path.is_empty() {
            assert!(
                cmd.get_about().is_some(),
                "missing completion description for {}",
                path.join(" ")
            );
        }
        for subcommand in cmd.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            assert_command_descriptions(subcommand, path);
            path.pop();
        }
    }

    #[test]
    fn spec_describes_all_completion_commands() {
        let cmd = super::command();
        assert_command_descriptions(&cmd, &mut Vec::new());
    }

    #[test]
    fn spec_passes_clap_invariants() {
        super::command().debug_assert();
    }

    #[test]
    fn every_spec_subcommand_renders_short_and_long_help() {
        let mut paths = Vec::new();
        collect_subcommand_paths(&super::command(), &mut Vec::new(), &mut paths);

        for path in paths {
            for flag in ["-h", "--help"] {
                let mut args = vec!["herdr".to_string()];
                args.extend(path.iter().cloned());
                args.push(flag.to_string());
                let mut output = Vec::new();
                assert!(
                    super::write_requested_help(&args, &mut output, || {}).unwrap(),
                    "help was not handled for herdr {} {flag}",
                    path.join(" ")
                );
                let output = String::from_utf8(output).unwrap();
                assert!(
                    output.contains(&format!("Uso: herdr {}", path.join(" "))),
                    "unexpected help for herdr {}: {output}",
                    path.join(" ")
                );
                for rotulo in [
                    "\nUsage:",
                    "\nArguments:",
                    "\nOptions:",
                    "\nCommands:",
                    "possible values:",
                    "Print help",
                ] {
                    assert!(
                        !output.contains(rotulo),
                        "rótulo {rotulo:?} em herdr {}: {output}",
                        path.join(" ")
                    );
                }
            }
        }
    }

    #[test]
    fn spec_includes_completion_alias_and_shells() {
        let cmd = super::command();
        let completion = command_path(&cmd, &["completion"]);
        assert!(completion
            .get_all_aliases()
            .any(|alias| alias == "completions"));
        let shells = completion
            .get_arguments()
            .find(|arg| arg.get_id() == "shell")
            .unwrap()
            .get_value_parser()
            .possible_values()
            .unwrap()
            .map(|value| value.get_name().to_string())
            .collect::<Vec<_>>();
        assert!(shells.contains(&"zsh".to_string()));
        assert!(shells.contains(&"fish".to_string()));
    }

    #[test]
    fn spec_matches_all_integration_targets() {
        let cmd = super::command();
        let install = command_path(&cmd, &["integration", "install"]);
        let mut expected: Vec<String> = crate::api::schema::IntegrationTarget::ALL
            .map(crate::integration::integration_target_label)
            .map(str::to_string)
            .to_vec();
        expected.extend(
            crate::integration::EXPERIMENTAL_INTEGRATION_TARGET_LABELS
                .iter()
                .map(|label| (*label).to_string()),
        );
        assert_eq!(
            argument(install, "target")
                .get_value_parser()
                .possible_values()
                .unwrap()
                .map(|value| value.get_name().to_string())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn spec_marks_runtime_required_options_as_required() {
        for (path, options) in [
            (&["workspace", "report-metadata"][..], &["source"][..]),
            (&["pane", "neighbor"][..], &["direction"][..]),
            (&["pane", "focus"][..], &["direction"][..]),
            (&["pane", "resize"][..], &["direction"][..]),
            (&["pane", "report-agent"][..], &["source", "agent"][..]),
            (
                &["pane", "report-agent-session"][..],
                &["source", "agent"][..],
            ),
            (&["pane", "release-agent"][..], &["source", "agent"][..]),
            (&["pane", "report-metadata"][..], &["source"][..]),
        ] {
            let cmd = command_path(&super::command(), path).clone();
            for option in options {
                assert!(
                    option_arg(&cmd, option).is_required_set(),
                    "herdr {} --{option} should be required",
                    path.join(" ")
                );
            }
        }
    }

    #[test]
    fn agent_prompt_until_requires_wait() {
        let error = super::command()
            .try_get_matches_from([
                "herdr", "agent", "prompt", "reviewer", "hello", "--until", "idle",
            ])
            .unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn agent_rename_requires_exactly_one_name_or_clear() {
        for valid in [
            &["herdr", "agent", "rename", "reviewer", "worker"][..],
            &["herdr", "agent", "rename", "reviewer", "--clear"][..],
        ] {
            assert!(super::command().try_get_matches_from(valid).is_ok());
        }
        for invalid in [
            &["herdr", "agent", "rename", "reviewer"][..],
            &["herdr", "agent", "rename", "reviewer", "worker", "--clear"][..],
        ] {
            assert!(super::command().try_get_matches_from(invalid).is_err());
        }

        let mut help = Vec::new();
        super::write_requested_help(
            &[
                "herdr".to_string(),
                "agent".to_string(),
                "rename".to_string(),
                "--help".to_string(),
            ],
            &mut help,
            || {},
        )
        .unwrap();
        assert!(String::from_utf8(help)
            .unwrap()
            .contains("Uso: herdr agent rename <TARGET> <NAME>|--clear"));
    }

    #[test]
    fn worktree_json_compatibility_flag_stays_out_of_public_help() {
        let cmd = super::command();
        for subcommand in ["list", "create", "open", "remove"] {
            let worktree_command = command_path(&cmd, &["worktree", subcommand]);
            assert!(option_arg(worktree_command, "json").is_hide_set());
            assert!(
                !worktree_command
                    .clone()
                    .render_long_help()
                    .to_string()
                    .contains("--json"),
                "herdr worktree {subcommand} should not advertise --json"
            );
        }
    }

    #[test]
    fn spec_includes_nested_plugin_pane_open_options() {
        let cmd = super::command();
        let open = command_path(&cmd, &["plugin", "pane", "open"]);
        assert!(open
            .get_arguments()
            .any(|arg| arg.get_long() == Some("entrypoint")));
        assert!(option_values(open, "placement").contains(&"zoomed".to_string()));
    }

    #[test]
    fn spec_keeps_agent_wait_status_free() {
        let cmd = super::command();
        let wait = command_path(&cmd, &["agent", "wait"]);
        assert!(!has_option(wait, "status"));
        assert_eq!(
            option_values(wait, "until"),
            ["idle", "working", "blocked", "done", "unknown"]
        );
        assert!(has_option(wait, "timeout"));
    }

    #[test]
    fn spec_matches_refactored_agent_and_pane_commands() {
        let cmd = super::command();
        assert!(cmd
            .get_subcommands()
            .all(|subcommand| subcommand.get_name() != "wait"));

        let agent = command_path(&cmd, &["agent"]);
        assert!(agent
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "send-keys"));
        assert!(agent
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "wait"));
        assert!(agent
            .get_subcommands()
            .all(|subcommand| subcommand.get_name() != "send"));

        let pane = command_path(&cmd, &["pane"]);
        assert!(pane
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "wait-output"));
    }

    #[test]
    fn spec_includes_pane_read_raw_flag() {
        let cmd = super::command();
        let pane_read = command_path(&cmd, &["pane", "read"]);
        assert!(has_option(pane_read, "raw"));
    }

    #[test]
    fn spec_matches_pane_split_direction_flag() {
        let cmd = super::command();
        let pane_split = command_path(&cmd, &["pane", "split"]);
        assert!(has_option(pane_split, "direction"));
        assert!(!has_option(pane_split, "split"));
        assert_eq!(option_values(pane_split, "direction"), ["right", "down"]);
    }

    #[test]
    fn spec_models_agent_start_target_and_trailing_args() {
        let cmd = super::command();
        let agent_start = command_path(&cmd, &["agent", "start"]);
        assert!(has_option(agent_start, "kind"));
        assert_eq!(
            option_values(agent_start, "kind"),
            crate::detect::Agent::ALL
                .map(crate::detect::agent_label)
                .map(str::to_string)
        );
        assert!(has_option(agent_start, "pane"));
        for legacy in ["cwd", "workspace", "tab", "split", "focus", "env", "argv"] {
            assert!(!has_option(agent_start, legacy), "legacy option --{legacy}");
        }
        assert!(agent_start
            .get_arguments()
            .any(|arg| arg.get_id() == "agent_args"));
    }

    fn long_help(path: &[&str]) -> String {
        let mut args = vec!["herdr".to_string()];
        args.extend(path.iter().map(|segment| segment.to_string()));
        args.push("--help".to_string());
        let mut output = Vec::new();
        assert!(
            super::write_requested_help(&args, &mut output, || {}).unwrap(),
            "help was not handled for herdr {}",
            path.join(" ")
        );
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn ajuda_subcomandos_apresenta_portugues_sem_alterar_opcoes() {
        let ajuda = long_help(&["agent", "wait"]);
        assert!(ajuda.contains("Aguardar"), "{ajuda}");
        assert!(ajuda.contains("Uso:"), "{ajuda}");
        assert!(ajuda.contains("--until"), "{ajuda}");
        assert!(ajuda.contains("--timeout"), "{ajuda}");
        assert!(ajuda.contains("blocked"), "{ajuda}");
        assert!(long_help(&["update"]).contains("indisponível"));
        assert!(long_help(&["channel", "set"]).contains("indisponível"));
    }

    #[test]
    fn agent_resources_appear_on_command_groups_but_not_leaf_commands() {
        for group in ["agent", "pane", "workspace", "terminal"] {
            let help = long_help(&[group]);
            assert!(
                help.contains(super::super::AGENT_HELP_FOOTER),
                "herdr {group} is missing agent resources: {help}"
            );
        }

        let leaf = long_help(&["agent", "wait"]);
        assert!(
            !leaf.contains(super::super::AGENT_HELP_FOOTER),
            "leaf help should stay focused: {leaf}"
        );
    }

    #[test]
    fn next_step_hints_render_without_replacing_existing_after_help() {
        let agent_start = long_help(&["agent", "start"]);
        assert!(
            agent_start.contains("O painel deve estar no prompt de shell interativo."),
            "agent start dropped its existing after_help: {agent_start}"
        );
        assert!(
            agent_start.contains("Próximo passo: herdr agent prompt <TARGET> <TEXT> --wait"),
            "agent start is missing its next-step hint: {agent_start}"
        );

        let pane_send_text = long_help(&["pane", "send-text"]);
        assert!(
            pane_send_text.contains(
                "Próximo passo: herdr pane run <PANE_ID> <COMMAND> envia texto e Enter em uma chamada"
            ),
            "pane send-text is missing its next-step hint: {pane_send_text}"
        );
    }

    #[test]
    fn completion_generation_succeeds_for_every_supported_shell() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Elvish,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
            clap_complete::Shell::Zsh,
        ] {
            let mut cmd = super::command();
            let mut output = Vec::new();
            clap_complete::generate(shell, &mut cmd, "herdr", &mut output);
            assert!(!output.is_empty(), "empty {shell:?} completion output");
        }
    }
}
