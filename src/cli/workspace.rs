use super::projection::ParseError;
use super::workspace_bindings::{command, LEAVES};

pub(super) fn run_workspace_command(args: &[String]) -> std::io::Result<i32> {
    let print_commands = || eprintln!("{}", super::projection::group_commands(&command()));
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_commands();
        return Ok(2);
    };
    if matches!(subcommand, "help" | "--help" | "-h") {
        print_commands();
        return Ok(0);
    }
    let Some(leaf) = LEAVES.iter().find(|leaf| leaf.name == subcommand) else {
        print_commands();
        return Ok(2);
    };
    let request = match leaf.request(&args[1..]) {
        Ok(request) => request,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    if leaf.quiet {
        super::send_ok_request(request.method)
    } else {
        super::print_response(&super::send_request(&request)?)
    }
}
