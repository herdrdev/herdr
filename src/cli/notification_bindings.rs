use clap::Command;

use crate::api::schema::common::NotificationShowProjection;
use crate::api::schema::{Method, Request};

use super::super::projection::{self, ParseError};

pub(crate) fn command() -> Command {
    Command::new("notification")
        .about("Show Herdr notifications")
        .subcommand_required(true)
        .subcommand(
            projection::command("show", NotificationShowProjection::arguments())
                .about("Show a notification"),
        )
}

pub(crate) fn request(args: &[String]) -> Result<Request, ParseError> {
    // A bare `help` title historically asks for usage instead of showing a notification.
    // The title is a verbatim slot, so route through the help subcommand.
    let help_args;
    let args = match args {
        [show, title, ..] if show == "show" && title == "help" => {
            help_args = ["help".to_owned(), show.clone()];
            &help_args[..]
        }
        _ => args,
    };
    projection::parse(command().bin_name("herdr notification"), args, |matches| {
        let matches = matches
            .subcommand_matches("show")
            .ok_or_else(|| ParseError::Usage("missing notification show command".into()))?;
        Ok(Request {
            id: "cli:notification:show".into(),
            method: Method::NotificationShow(NotificationShowProjection::from_matches(matches)?),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Request, ParseError> {
        request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn notification_projection_preserves_request_literals_and_last_wins() {
        for (args, expected) in [
            (
                vec!["show", "build failed"],
                serde_json::json!({"id":"cli:notification:show", "method":"notification.show", "params":{"title":"build failed"}}),
            ),
            (
                vec![
                    "show",
                    "--literal-title",
                    "--body",
                    "first",
                    "--body",
                    "--literal-body",
                    "--position",
                    "bottom-left",
                    "--position",
                    "top-right",
                    "--sound",
                    "done",
                    "--sound",
                    "request",
                ],
                serde_json::json!({"id":"cli:notification:show", "method":"notification.show", "params":{"title":"--literal-title", "body":"--literal-body", "position":"top-right", "sound":"request"}}),
            ),
            (
                vec!["show", "title", "--sound=none"],
                serde_json::json!({"id":"cli:notification:show", "method":"notification.show", "params":{"title":"title"}}),
            ),
        ] {
            assert_eq!(
                serde_json::to_value(parse(&args).unwrap()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn notification_projection_preserves_all_cli_enum_values() {
        for position in ["top-left", "top-right", "bottom-left", "bottom-right"] {
            assert_eq!(
                serde_json::to_value(parse(&["show", "title", "--position", position]).unwrap())
                    .unwrap()["params"]["position"],
                position
            );
        }
        for sound in ["done", "request"] {
            assert_eq!(
                serde_json::to_value(parse(&["show", "title", "--sound", sound]).unwrap()).unwrap()
                    ["params"]["sound"],
                sound
            );
        }
        command().debug_assert();
    }

    #[test]
    fn notification_projection_clap_validation_and_help_before_transport() {
        for args in [
            vec!["show"],
            vec!["show", "title", "extra"],
            vec!["show", "title", "--body"],
            vec!["show", "title", "--position", "top_right"],
            vec!["show", "title", "--sound", "DONE"],
        ] {
            assert!(matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2));
        }
        assert!(
            matches!(parse(&["show", "title", "--help"]), Err(ParseError::Clap(error)) if error.exit_code() == 0)
        );
        assert_eq!(
            serde_json::to_value(parse(&["show", "title", "--sound=request"]).unwrap()).unwrap(),
            serde_json::json!({"id":"cli:notification:show", "method":"notification.show", "params":{"title":"title", "sound":"request"}})
        );
    }
}
