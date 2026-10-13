use std::collections::HashMap;

use clap::{Arg, ArgMatches, Command};

use crate::api::schema::workspaces::{
    WorkspaceCloseProjection, WorkspaceCreateProjection, WorkspaceMetadataProjection,
    WorkspaceRenameProjection,
};
use crate::api::schema::{EmptyParams, Method, Request, WorkspaceTarget};

use super::projection::{cli_projection, Field, Input, ParseError};

pub(crate) use super::projection::{boolean, optional_text};

pub(crate) fn workspace_id() -> Input<String> {
    Input::positional("workspace_id", 0, |slot, value| {
        *slot = super::normalize_workspace_id(value);
        Ok(())
    })
}

pub(crate) fn label() -> Input<String> {
    Input::rest("label", "LABEL", 1, |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .verbatim()
}

pub(crate) fn environment() -> Input<HashMap<String, String>> {
    Input::<HashMap<String, String>>::value("env", "KEY=VALUE", |slot, value| {
        let (key, value) = super::parse_env_assignment(value)?;
        slot.insert(key, value);
        Ok(())
    })
    .help("Set an environment variable for the launched process")
}

pub(crate) fn source() -> Input<String> {
    Input::value("source", "ID", |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .required()
}

pub(crate) fn validate_source(source: &str) -> Result<(), ParseError> {
    if source.trim().is_empty() {
        return Err(ParseError::Usage("missing required --source".into()));
    }
    Ok(())
}

pub(crate) fn token() -> Input<HashMap<String, Option<String>>> {
    Input::<HashMap<String, Option<String>>>::value("token", "NAME=VALUE", |slot, value| {
        let (key, value) = super::parse_token_assignment(value)?;
        slot.insert(key, value);
        Ok(())
    })
}

pub(crate) fn clear_token() -> Input<HashMap<String, Option<String>>> {
    Input::<HashMap<String, Option<String>>>::value("clear-token", "NAME", |slot, value| {
        slot.insert(value.to_owned(), None);
        Ok(())
    })
}

pub(crate) fn validate_tokens(tokens: &HashMap<String, Option<String>>) -> Result<(), ParseError> {
    if tokens.is_empty() {
        return Err(ParseError::Usage("missing token to set or clear".into()));
    }
    Ok(())
}

pub(crate) fn seq() -> Input<Option<u64>> {
    Input::value("seq", "N", |slot, value| {
        *slot = Some(super::parse_u64_flag("--seq", value)?);
        Ok(())
    })
}

pub(crate) fn ttl_ms() -> Input<Option<u64>> {
    Input::value("ttl-ms", "N", |slot, value| {
        *slot = Some(super::parse_u64_flag("--ttl-ms", value)?);
        Ok(())
    })
}

cli_projection! {
    pub(crate) WorkspaceTargetProjection for WorkspaceTarget {
        workspace_id => Field::new(String::new()).input(workspace_id()),
    }
}

cli_projection! {
    pub(crate) WorkspaceListProjection for EmptyParams {}
}

pub(crate) struct Leaf {
    pub(crate) name: &'static str,
    about: &'static str,
    id: &'static str,
    pub(crate) quiet: bool,
    arguments: fn() -> Vec<Arg>,
    method: fn(&ArgMatches) -> Result<Method, ParseError>,
}

impl Leaf {
    pub(crate) fn request(&self, args: &[String]) -> Result<Request, ParseError> {
        let method = super::projection::parse(
            self.command()
                .bin_name(format!("herdr workspace {}", self.name)),
            args,
            self.method,
        )?;
        Ok(Request {
            id: self.id.into(),
            method,
        })
    }

    fn command(&self) -> Command {
        super::projection::command(self.name, (self.arguments)()).about(self.about)
    }
}

pub(crate) const LEAVES: [Leaf; 7] = [
    Leaf {
        name: "list",
        about: "List workspaces",
        id: "cli:workspace:list",
        quiet: false,
        arguments: WorkspaceListProjection::arguments,
        method: |matches| WorkspaceListProjection::from_matches(matches).map(Method::WorkspaceList),
    },
    Leaf {
        name: "create",
        about: "Create a workspace",
        id: "cli:workspace:create",
        quiet: false,
        arguments: WorkspaceCreateProjection::arguments,
        method: |matches| {
            WorkspaceCreateProjection::from_matches(matches).map(Method::WorkspaceCreate)
        },
    },
    Leaf {
        name: "get",
        about: "Show a workspace",
        id: "cli:workspace:get",
        quiet: false,
        arguments: WorkspaceTargetProjection::arguments,
        method: |matches| {
            WorkspaceTargetProjection::from_matches(matches).map(Method::WorkspaceGet)
        },
    },
    Leaf {
        name: "focus",
        about: "Focus a workspace",
        id: "cli:workspace:focus",
        quiet: false,
        arguments: WorkspaceTargetProjection::arguments,
        method: |matches| {
            WorkspaceTargetProjection::from_matches(matches).map(Method::WorkspaceFocus)
        },
    },
    Leaf {
        name: "rename",
        about: "Rename a workspace",
        id: "cli:workspace:rename",
        quiet: false,
        arguments: WorkspaceRenameProjection::arguments,
        method: |matches| {
            WorkspaceRenameProjection::from_matches(matches).map(Method::WorkspaceRename)
        },
    },
    Leaf {
        name: "report-metadata",
        about: "Report display-only workspace metadata",
        id: "cli:request",
        quiet: true,
        arguments: WorkspaceMetadataProjection::arguments,
        method: |matches| {
            WorkspaceMetadataProjection::from_matches(matches).map(Method::WorkspaceReportMetadata)
        },
    },
    Leaf {
        name: "close",
        about: "Close a workspace",
        id: "cli:workspace:close",
        quiet: false,
        arguments: WorkspaceCloseProjection::arguments,
        method: |matches| {
            WorkspaceCloseProjection::from_matches(matches).map(Method::WorkspaceClose)
        },
    },
];

pub(crate) fn command() -> Command {
    Command::new("workspace")
        .about("Manage workspaces over the socket API")
        .subcommands(LEAVES.iter().map(Leaf::command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: &str, args: &[&str]) -> Result<Request, ParseError> {
        LEAVES
            .iter()
            .find(|leaf| leaf.name == command)
            .unwrap()
            .request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn workspace_projection_preserves_defaults_and_ordered_maps() {
        let request = request(
            "create",
            &[
                "--cwd",
                "first",
                "--cwd",
                "--literal",
                "--focus",
                "--no-focus",
                "--env",
                "K=first",
                "--env",
                "K=",
                "--env",
                "V=a=b",
            ],
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "id": "cli:workspace:create", "method": "workspace.create",
                "params": { "cwd": "--literal", "focus": false, "env": {"K": "", "V": "a=b"} }
            })
        );
        assert_eq!(
            serde_json::to_value(super::tests::request("create", &[]).unwrap()).unwrap(),
            serde_json::json!({
                "id": "cli:workspace:create", "method": "workspace.create", "params": {"focus": false}
            })
        );
    }

    #[test]
    fn workspace_projection_preserves_joined_labels_and_hidden_group() {
        assert_eq!(
            serde_json::to_value(request("rename", &["--id", "a", "--flag", "b"]).unwrap())
                .unwrap(),
            serde_json::json!({
                "id": "cli:workspace:rename", "method": "workspace.rename",
                "params": {"workspace_id": "--id", "label": "a --flag b"}
            })
        );
        assert_eq!(
            serde_json::to_value(request("close", &["id", "--group"]).unwrap()).unwrap(),
            serde_json::json!({
                "id": "cli:workspace:close", "method": "workspace.close",
                "params": {"workspace_id": "id", "close_group": true}
            })
        );
        let close = command().find_subcommand("close").unwrap().clone();
        assert!(close
            .get_arguments()
            .any(|arg| arg.get_long() == Some("group") && arg.is_hide_set()));
        command().debug_assert();
    }

    #[test]
    fn workspace_projection_preserves_metadata_patches_and_error_channels() {
        assert_eq!(
            serde_json::to_value(
                request(
                    "report-metadata",
                    &[
                        "id",
                        "--source",
                        "  user:jj  ",
                        "--token",
                        "K=a=b",
                        "--clear-token",
                        "K",
                        "--token",
                        "E=",
                        "--seq",
                        "0",
                        "--ttl-ms",
                        "0",
                    ]
                )
                .unwrap()
            )
            .unwrap(),
            serde_json::json!({
                "id": "cli:request", "method": "workspace.report_metadata",
                "params": {"workspace_id": "id", "source": "  user:jj  ", "tokens": {"K": null, "E": ""}, "seq": 0, "ttl_ms": 0}
            })
        );
        assert!(
            matches!(request("report-metadata", &["id", "--source", "valid", "--seq", "bad"]), Err(ParseError::Io(error)) if error.to_string() == "invalid value for --seq: bad")
        );
        assert!(
            matches!(request("report-metadata", &["id", "--source", " "]), Err(ParseError::Usage(message)) if message == "missing required --source")
        );
        assert!(
            matches!(request("report-metadata", &["id", "--source", "valid"]), Err(ParseError::Usage(message)) if message == "missing token to set or clear")
        );
    }

    #[test]
    fn workspace_projection_uses_authoritative_clap_grammar() {
        assert_eq!(
            serde_json::to_value(request("create", &["--cwd=x"]).unwrap()).unwrap(),
            serde_json::json!({
                "id": "cli:workspace:create", "method": "workspace.create", "params": {"cwd": "x", "focus": false}
            })
        );
        assert_eq!(
            serde_json::to_value(request("close", &["--group", "id"]).unwrap()).unwrap(),
            serde_json::json!({
                "id": "cli:workspace:close", "method": "workspace.close", "params": {"workspace_id": "id", "close_group": true}
            })
        );
        assert!(
            matches!(request("get", &[]), Err(ParseError::Clap(error)) if error.kind() == clap::error::ErrorKind::MissingRequiredArgument)
        );
        assert!(
            matches!(request("get", &["id", "extra"]), Err(ParseError::Clap(error)) if error.kind() == clap::error::ErrorKind::UnknownArgument)
        );
        assert!(
            matches!(request("create", &["--cwd"]), Err(ParseError::Clap(error)) if error.exit_code() == 2)
        );
        assert!(
            matches!(request("get", &["id", "--help"]), Err(ParseError::Clap(error)) if error.kind() == clap::error::ErrorKind::DisplayHelp && error.exit_code() == 0)
        );
        assert!(
            matches!(request("create", &["--env", "=value"]), Err(ParseError::Usage(message)) if message == "env key must not be empty")
        );
    }
}
