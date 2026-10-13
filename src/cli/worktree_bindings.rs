use clap::Command;

use crate::api::schema::worktrees::{
    WorktreeCreateProjection, WorktreeListProjection, WorktreeOpenProjection,
    WorktreeRemoveProjection,
};
use crate::api::schema::{Method, Request};

use super::super::projection::{self, Input, Leaf, ParseError};

pub(crate) fn path(name: &'static str) -> Input<Option<String>> {
    Input::value(name, "PATH", |slot, value| {
        *slot = Some(super::normalize_path_arg(value)?);
        Ok(())
    })
    .path()
}

const LEAVES: [Leaf; 4] = [
    Leaf {
        name: "list",
        about: "List worktree workspaces",
        id: "cli:worktree:list",
        arguments: WorktreeListProjection::arguments,
        method: |matches| WorktreeListProjection::from_matches(matches).map(Method::WorktreeList),
    },
    Leaf {
        name: "create",
        about: "Create and open a Git worktree",
        id: "cli:worktree:create",
        arguments: WorktreeCreateProjection::arguments,
        method: |matches| {
            WorktreeCreateProjection::from_matches(matches).map(Method::WorktreeCreate)
        },
    },
    Leaf {
        name: "open",
        about: "Open an existing Git worktree",
        id: "cli:worktree:open",
        arguments: WorktreeOpenProjection::arguments,
        method: |matches| WorktreeOpenProjection::from_matches(matches).map(Method::WorktreeOpen),
    },
    Leaf {
        name: "remove",
        about: "Remove a worktree checkout",
        id: "cli:worktree:remove",
        arguments: WorktreeRemoveProjection::arguments,
        method: |matches| {
            WorktreeRemoveProjection::from_matches(matches).map(Method::WorktreeRemove)
        },
    },
];

pub(crate) fn command() -> Command {
    Command::new("worktree")
        .about("Manage Git worktree-backed workspaces")
        .subcommand_required(true)
        .subcommands(LEAVES.iter().map(Leaf::command))
        .mut_subcommand("open", |open| {
            open.group(
                clap::ArgGroup::new("open_target")
                    .args(["path", "branch"])
                    .required(true),
            )
        })
}

pub(crate) fn request(args: &[String]) -> Result<Request, ParseError> {
    projection::group_request(command(), &LEAVES, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Request, ParseError> {
        request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn worktree_projection_preserves_literal_requests_and_hidden_json() {
        for (args, expected) in [
            (
                vec!["list", "--json"],
                serde_json::json!({"id":"cli:worktree:list","method":"worktree.list","params":{}}),
            ),
            (
                vec!["create", "--json"],
                serde_json::json!({"id":"cli:worktree:create","method":"worktree.create","params":{"focus":false}}),
            ),
            (
                vec![
                    "create",
                    "--workspace",
                    "2",
                    "--branch",
                    "topic",
                    "--base",
                    "HEAD",
                    "--label",
                    "name",
                    "--focus",
                    "--no-focus",
                    "--focus",
                    "--trust-repository",
                    "--json",
                ],
                serde_json::json!({"id":"cli:worktree:create","method":"worktree.create","params":{"workspace_id":"2","branch":"topic","base":"HEAD","label":"name","focus":true,"trust_repository":true}}),
            ),
            (
                vec!["open", "--branch", "topic", "--json"],
                serde_json::json!({"id":"cli:worktree:open","method":"worktree.open","params":{"branch":"topic","focus":false}}),
            ),
            (
                vec!["remove", "--workspace=2", "--json"],
                serde_json::json!({"id":"cli:worktree:remove","method":"worktree.remove","params":{"workspace_id":"2","force":false}}),
            ),
            (
                vec![
                    "remove",
                    "--workspace",
                    "2",
                    "--force",
                    "--trust-repository",
                    "--json",
                ],
                serde_json::json!({"id":"cli:worktree:remove","method":"worktree.remove","params":{"workspace_id":"2","force":true,"trust_repository":true}}),
            ),
        ] {
            assert_eq!(
                serde_json::to_value(parse(&args).unwrap()).unwrap(),
                expected
            );
        }
        for leaf in command().get_subcommands() {
            assert!(leaf
                .get_arguments()
                .any(|arg| arg.get_long() == Some("json") && arg.is_hide_set()));
            let mut leaf = leaf.clone();
            assert!(!leaf.render_long_help().to_string().contains("--json"));
        }
        command().debug_assert();
    }

    #[test]
    fn worktree_projection_normalizes_only_local_paths() {
        let expected = std::env::current_dir()
            .unwrap()
            .join("relative")
            .display()
            .to_string();
        assert_eq!(
            serde_json::to_value(parse(&["open", "--path", "relative"]).unwrap()).unwrap(),
            serde_json::json!({
                "id":"cli:worktree:open", "method":"worktree.open", "params":{"path":expected, "focus":false}
            })
        );
        crate::cli::target::with_test_client(crate::api::client::ApiClient::local(), || {
            assert_eq!(
                serde_json::to_value(
                    parse(&["create", "--cwd", "~/repo", "--path", "/remote/new"]).unwrap()
                )
                .unwrap(),
                serde_json::json!({
                    "id":"cli:worktree:create", "method":"worktree.create", "params":{"cwd":"~/repo", "path":"/remote/new", "focus":false}
                })
            );
            assert!(
                matches!(parse(&["open", "--path", "relative"]), Err(ParseError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput && error.to_string() == "remote worktree paths must be absolute or start with ~/")
            );
        });
    }

    #[test]
    fn worktree_projection_keeps_context_and_xor_validations() {
        for args in [
            vec!["list", "--workspace", "2", "--cwd", "."],
            vec!["create", "--workspace", "2", "--cwd", "."],
            vec!["open"],
            vec!["open", "--path", ".", "--branch", "topic"],
        ] {
            assert!(
                matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2),
                "{args:?}"
            );
        }
        assert!(
            matches!(parse(&["remove"]), Err(ParseError::Clap(error)) if error.exit_code() == 2)
        );
        assert!(
            matches!(parse(&["open", "--help"]), Err(ParseError::Clap(error)) if error.exit_code() == 0)
        );
    }
}
