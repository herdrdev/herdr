use clap::{Arg, Command};

use super::{json_flag, option};

pub(super) fn command() -> Command {
    Command::new("machine")
        .about("Manage saved SSH machines")
        .subcommand(
            Command::new("list")
                .about("List saved SSH machines")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("add")
                .about("Prepare the remote Herdr server and save an SSH machine")
                .arg(
                    Arg::new("ssh-target")
                        .value_name("SSH_TARGET")
                        .required(true),
                )
                .arg(
                    option("label", "LABEL")
                        .help("Override the SSH target shown as the machine label"),
                )
                .arg(
                    option("remote-session", "NAME")
                        .help("Select an explicit Herdr session instead of discovering one"),
                ),
        )
        .subcommand(
            profile_command("rename", "Rename a saved SSH machine").arg(
                option("label", "LABEL")
                    .required(true)
                    .help("Set the machine label shown in the sidebar"),
            ),
        )
        .subcommand(profile_command("remove", "Remove a saved SSH machine"))
        .subcommand(profile_command("enable", "Enable a saved SSH machine"))
        .subcommand(profile_command("disable", "Disable a saved SSH machine"))
}

fn profile_command(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).arg(
        Arg::new("profile-id")
            .value_name("PROFILE_ID")
            .required(true),
    )
}
