use clap::{Arg, Command};

pub(super) fn command() -> Command {
    Command::new("completion")
        .visible_alias("completions")
        .about("Gerar scripts de complementação para o shell")
        .arg(
            Arg::new("shell")
                .value_name("SHELL")
                .required(true)
                .value_parser(super::super::completion::SUPPORTED_SHELLS)
                .help("Shell para o qual gerar complementações"),
        )
}
