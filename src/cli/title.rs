use clap::{ArgMatches, Command};

use crate::api::schema::common::ClientWindowTitleSetProjection;
use crate::api::schema::{EmptyParams, Method, Request};

use super::projection::{self, ParseError};

pub(crate) fn command() -> Command {
    Command::new("title")
        .about("Manage the outer terminal title")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            projection::command("set", ClientWindowTitleSetProjection::arguments())
                .about("Set the outer terminal title"),
        )
        .subcommand(Command::new("clear").about("Clear the outer terminal title"))
}

fn request(matches: &ArgMatches) -> Result<Request, ParseError> {
    match matches.subcommand() {
        Some(("set", matches)) => Ok(Request {
            id: "cli:terminal:title:set".into(),
            method: Method::ClientWindowTitleSet(ClientWindowTitleSetProjection::from_matches(
                matches,
            )?),
        }),
        Some(("clear", _)) => Ok(Request {
            id: "cli:terminal:title:clear".into(),
            method: Method::ClientWindowTitleClear(EmptyParams::default()),
        }),
        _ => Err(ParseError::Usage("expected title set or clear".into())),
    }
}

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    match projection::parse(command(), args, request) {
        Ok(request) => super::print_response(&super::send_request(&request)?),
        Err(ParseError::Clap(error)) => super::print_clap_error(&error),
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            Ok(2)
        }
        Err(ParseError::Io(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_binding_preserves_literal_requests() {
        for (args, expected) in [
            (
                vec!["set", "a literal title"],
                serde_json::json!({
                    "id": "cli:terminal:title:set",
                    "method": "client.window_title.set",
                    "params": {"title": "a literal title"},
                }),
            ),
            (
                vec!["clear"],
                serde_json::json!({
                    "id": "cli:terminal:title:clear",
                    "method": "client.window_title.clear",
                    "params": {},
                }),
            ),
        ] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            let actual = projection::parse(command(), &args, request).expect("valid title command");
            assert_eq!(
                serde_json::to_value(actual).expect("request JSON"),
                expected
            );
        }
    }

    #[test]
    fn title_binding_help_and_invalid_arity_stop_before_transport() {
        command().debug_assert();
        for args in [vec!["set"], vec!["clear", "extra"], vec!["set", "a", "b"]] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            assert!(
                matches!(projection::parse(command(), &args, request), Err(ParseError::Clap(error)) if error.exit_code() == 2)
            );
        }
        let args = vec!["clear".into(), "--help".into()];
        assert!(
            matches!(projection::parse(command(), &args, request), Err(ParseError::Clap(error)) if error.exit_code() == 0)
        );
    }
}
