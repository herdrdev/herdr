use std::io::Write;

use clap_complete::{generate, Shell};

pub(super) const SUPPORTED_SHELLS: [&str; 5] = ["bash", "elvish", "fish", "powershell", "zsh"];

pub(super) fn supported_shells_usage() -> String {
    SUPPORTED_SHELLS.join("|")
}

pub(super) fn run_completion_command(args: &[String]) -> std::io::Result<i32> {
    let Some(shell) = args.first().map(String::as_str) else {
        print_completion_help();
        return Ok(2);
    };
    if matches!(shell, "help" | "--help" | "-h") {
        print_completion_help();
        return Ok(0);
    }
    if args.len() != 1 {
        print_completion_help();
        return Ok(2);
    }

    let Some(shell) = parse_shell(shell) else {
        eprintln!("unknown shell: {shell}");
        print_completion_help();
        return Ok(2);
    };

    let mut source = super::spec::command();
    source.build();
    let mut command = completion_command(&source);
    if matches!(shell, Shell::Zsh) {
        let mut output = Vec::new();
        generate(shell, &mut command, "herdr", &mut output);
        let script = String::from_utf8(output).map_err(|err| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, err.utf8_error())
        })?;
        crate::platform::begin_cli_output();
        std::io::stdout().write_all(space_separated_zsh_long_options(&script).as_bytes())?;
    } else {
        crate::platform::begin_cli_output();
        generate(shell, &mut command, "herdr", &mut std::io::stdout());
    }
    Ok(0)
}

// Completion generators do not consistently filter hidden metadata. This tree is
// presentation metadata only: parsing and its relations remain on the source.
fn completion_command(source: &clap::Command) -> clap::Command {
    let mut command = clap::Command::new(source.get_name().to_owned())
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .disable_version_flag(true)
        .visible_aliases(source.get_visible_aliases().map(str::to_owned));
    if let Some(about) = source.get_about() {
        command = command.about(about.clone());
    }
    for source_arg in source.get_arguments().filter(|arg| !arg.is_hide_set()) {
        let mut arg = clap::Arg::new(source_arg.get_id().clone())
            .action(source_arg.get_action().clone())
            .required(source_arg.is_required_set())
            .value_hint(source_arg.get_value_hint());
        if let Some(long) = source_arg.get_long() {
            arg = arg.long(long.to_owned());
        }
        if let Some(short) = source_arg.get_short() {
            arg = arg.short(short);
        }
        if let Some(aliases) = source_arg.get_visible_aliases() {
            arg = arg.visible_aliases(aliases.into_iter().map(str::to_owned));
        }
        if let Some(aliases) = source_arg.get_visible_short_aliases() {
            arg = arg.visible_short_aliases(aliases);
        }
        if let Some(help) = source_arg.get_help() {
            arg = arg.help(help.clone());
        }
        if let Some(names) = source_arg.get_value_names() {
            arg = arg.value_names(names.iter().cloned());
        }
        if let Some(arity) = source_arg.get_num_args() {
            arg = arg.num_args(arity);
        }
        if let Some(index) = source_arg.get_index() {
            arg = arg.index(index);
        }
        if let Some(values) = source_arg.get_value_parser().possible_values() {
            arg = arg.value_parser(clap::builder::PossibleValuesParser::new(
                values.filter(|value| !value.is_hide_set()),
            ));
        }
        command = command.arg(arg);
    }
    command.subcommands(
        source
            .get_subcommands()
            .filter(|command| !command.is_hide_set())
            .map(completion_command),
    )
}

fn space_separated_zsh_long_options(script: &str) -> String {
    let mut output = String::with_capacity(script.len());
    let mut rest = script;

    while let Some(start) = rest.find("--") {
        output.push_str(&rest[..start]);
        let candidate = &rest[start + 2..];
        let name_len = candidate
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .map(char::len_utf8)
            .sum::<usize>();
        if name_len > 0 && candidate[name_len..].starts_with("=[") {
            output.push_str("--");
            output.push_str(&candidate[..name_len]);
            output.push('[');
            rest = &candidate[name_len + 2..];
        } else {
            output.push_str("--");
            rest = candidate;
        }
    }

    output.push_str(rest);
    output
}

fn parse_shell(shell: &str) -> Option<Shell> {
    match shell {
        value if value == SUPPORTED_SHELLS[0] => Some(Shell::Bash),
        value if value == SUPPORTED_SHELLS[1] => Some(Shell::Elvish),
        value if value == SUPPORTED_SHELLS[2] => Some(Shell::Fish),
        value if value == SUPPORTED_SHELLS[3] => Some(Shell::PowerShell),
        value if value == SUPPORTED_SHELLS[4] => Some(Shell::Zsh),
        _ => None,
    }
}

fn print_completion_help() {
    eprintln!("usage: herdr completion <{}>", supported_shells_usage());
}

#[cfg(test)]
mod tests {
    fn script(shell: clap_complete::Shell, source: &clap::Command) -> String {
        let mut command = super::completion_command(source);
        let mut output = Vec::new();
        clap_complete::generate(shell, &mut command, "herdr", &mut output);
        String::from_utf8(output).expect("completion is utf8")
    }

    fn advertises(script: &str, flag: &str) -> bool {
        script.contains(&format!("--{flag}")) || script.contains(&format!("-l {flag}"))
    }

    #[test]
    fn completions_hide_private_commands_arguments_and_values() {
        let mut root = crate::cli::spec::command();
        root.build();
        let family = |name: &str| root.find_subcommand(name).expect("family");
        let plugin = family("plugin");
        let open = plugin
            .find_subcommand("pane")
            .and_then(|pane| pane.find_subcommand("open"))
            .expect("plugin pane open");
        let worktree = family("worktree");
        for shell in super::SUPPORTED_SHELLS {
            let shell = super::parse_shell(shell).expect("supported shell");
            let full = script(shell, &root);
            assert!(!full.contains("live-handoff"), "{shell}: hidden command");
            assert!(!advertises(&full, "group"), "{shell}: hidden group");
            assert!(advertises(&full, "version"), "{shell}: public root flag");
            let worktree = script(shell, worktree);
            assert!(
                !advertises(&worktree, "json"),
                "{shell}: hidden worktree json"
            );
            assert!(
                advertises(&worktree, "base"),
                "{shell}: public worktree flag"
            );
            let plugin = script(shell, open);
            for hidden in ["width", "height"] {
                assert!(!advertises(&plugin, hidden), "{shell}: hidden {hidden}");
            }
            for hidden in ["popup", "fullscreen"] {
                assert!(!plugin.contains(hidden), "{shell}: hidden {hidden}");
            }
            assert!(
                advertises(&plugin, "placement"),
                "{shell}: public placement"
            );
        }
    }

    #[test]
    fn projection_preserves_completion_metadata_without_execution_relations() {
        let mut source = clap::Command::new("demo")
            .visible_alias("visible")
            .arg(
                clap::Arg::new("path")
                    .long("path")
                    .visible_alias("file")
                    .short('p')
                    .visible_short_alias('f')
                    .value_hint(clap::ValueHint::FilePath)
                    .requires("private"),
            )
            .arg(clap::Arg::new("private").long("private").hide(true));
        source.build();
        let mut projected = super::completion_command(&source);
        projected.build();
        assert_eq!(
            projected.get_visible_aliases().collect::<Vec<_>>(),
            ["visible"]
        );
        let arg = projected
            .get_arguments()
            .find(|arg| arg.get_id() == "path")
            .expect("path");
        assert_eq!(arg.get_visible_aliases(), Some(vec!["file"]));
        assert_eq!(arg.get_visible_short_aliases(), Some(vec!['f']));
        assert_eq!(arg.get_value_hint(), clap::ValueHint::FilePath);
    }

    #[test]
    fn parses_supported_shells() {
        assert!(matches!(
            super::parse_shell("zsh"),
            Some(clap_complete::Shell::Zsh)
        ));
        assert!(matches!(
            super::parse_shell("powershell"),
            Some(clap_complete::Shell::PowerShell)
        ));
        assert!(super::parse_shell("tcsh").is_none());
    }

    #[test]
    fn zsh_long_options_use_space_separated_arguments() {
        let script =
            "'--cwd=[]:PATH:_files' \\\n'--json[]' \\\n'--split=[]:DIRECTION:(right down)' \\";
        let normalized = super::space_separated_zsh_long_options(script);
        assert!(normalized.contains("'--cwd[]:PATH:_files'"));
        assert!(normalized.contains("'--json[]'"));
        assert!(normalized.contains("'--split[]:DIRECTION:(right down)'"));
        assert!(!normalized.contains("--cwd=[]"));
        assert!(!normalized.contains("--split=[]"));
    }
}
