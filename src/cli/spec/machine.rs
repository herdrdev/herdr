use clap::{Arg, Command};

use super::{json_flag, option};

pub(super) fn command() -> Command {
    Command::new("machine")
        .about("Gerenciar máquinas SSH salvas")
        .subcommand(
            Command::new("list")
                .about("Listar máquinas SSH salvas")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("status")
                .about("Verificar máquinas salvas sem solicitar autenticação")
                .arg(Arg::new("machine").value_name("LABEL_OR_ID"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("reconnect")
                .about("Autenticar uma máquina salva neste terminal e verificar a conexão")
                .arg(Arg::new("machine").value_name("LABEL_OR_ID").required(true)),
        )
        .subcommand(
            Command::new("add")
                .about("Preparar o servidor Herdr remoto e salvar uma máquina SSH")
                .arg(
                    Arg::new("ssh-target")
                        .value_name("SSH_TARGET")
                        .required(true),
                )
                .arg(option("label", "LABEL").help(
                    "Definir nome da máquina na barra lateral (padrão: host SSH ou host/sessão)",
                ))
                .arg(
                    option("remote-session", "NAME")
                        .help("Selecionar sessão explicitamente (padrão sem terminal interativo)"),
                ),
        )
        .subcommand(
            profile_command("rename", "Renomear máquina SSH salva").arg(
                option("label", "LABEL")
                    .required(true)
                    .help("Definir nome da máquina na barra lateral"),
            ),
        )
        .subcommand(profile_command("remove", "Remover máquina SSH salva"))
        .subcommand(profile_command("enable", "Ativar máquina SSH salva"))
        .subcommand(profile_command("disable", "Desativar máquina SSH salva"))
}

fn profile_command(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).arg(
        Arg::new("profile-id")
            .value_name("PROFILE_ID")
            .required(true),
    )
}
