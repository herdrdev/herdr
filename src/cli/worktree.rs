use crate::api::schema::{
    WorktreeCreateParams, WorktreeListParams, WorktreeOpenParams, WorktreeRemoveParams,
};

// Worktree output is always JSON. The parsers retain `--json` as a hidden compatibility no-op.
pub(super) fn run_worktree_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_worktree_help();
        return Ok(2);
    };

    match subcommand {
        "list" => worktree_list(&args[1..]),
        "create" => worktree_create(&args[1..]),
        "open" => worktree_open(&args[1..]),
        "remove" => worktree_remove(&args[1..]),
        "help" | "--help" | "-h" => {
            print_worktree_help();
            Ok(0)
        }
        _ => {
            print_worktree_help();
            Ok(2)
        }
    }
}

fn worktree_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    if workspace_id.is_some() && cwd.is_some() {
        eprintln!("usage: herdr worktree list [--workspace ID | --cwd PATH]");
        return Ok(2);
    }

    super::runtime::worktree_list(WorktreeListParams { workspace_id, cwd })
}

fn worktree_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut branch = None;
    let mut base = None;
    let mut path = None;
    let mut label = None;
    let mut focus = false;
    let mut permit_common_dir = None;
    let mut permit_path = None;
    let mut permit_branch = None;
    let mut permit_head = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--branch" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --branch");
                    return Ok(2);
                };
                branch = Some(value.clone());
                index += 2;
            }
            "--base" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --base");
                    return Ok(2);
                };
                base = Some(value.clone());
                index += 2;
            }
            "--path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --path");
                    return Ok(2);
                };
                path = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--permit-common-dir" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-common-dir");
                    return Ok(2);
                };
                permit_common_dir = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--permit-path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-path");
                    return Ok(2);
                };
                permit_path = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--permit-branch" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-branch");
                    return Ok(2);
                };
                permit_branch = Some(value.clone());
                index += 2;
            }
            "--permit-head" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-head");
                    return Ok(2);
                };
                permit_head = Some(value.clone());
                index += 2;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let permit = match parse_permit(permit_common_dir, permit_path, permit_branch, permit_head) {
        Ok(permit) => permit,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    if workspace_id.is_some() && cwd.is_some() {
        eprintln!(
            "usage: herdr worktree create [--workspace ID | --cwd PATH] [--branch NAME] [--base REF] [--path PATH] [--label TEXT] [--focus] [--no-focus]"
        );
        return Ok(2);
    }

    super::runtime::worktree_create(WorktreeCreateParams {
        workspace_id,
        cwd,
        branch,
        base,
        path,
        label,
        focus,
        permit,
    })
}
fn worktree_open(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut path = None;
    let mut branch = None;
    let mut label = None;
    let mut focus = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --path");
                    return Ok(2);
                };
                path = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--branch" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --branch");
                    return Ok(2);
                };
                branch = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    if workspace_id.is_some() && cwd.is_some() {
        eprintln!(
            "usage: herdr worktree open [--workspace ID | --cwd PATH] (--path PATH | --branch NAME) [--label TEXT] [--focus] [--no-focus]"
        );
        return Ok(2);
    }
    if path.is_some() == branch.is_some() {
        eprintln!(
            "usage: herdr worktree open [--workspace ID | --cwd PATH] (--path PATH | --branch NAME) [--label TEXT] [--focus] [--no-focus]"
        );
        return Ok(2);
    }

    super::runtime::worktree_open(WorktreeOpenParams {
        workspace_id,
        cwd,
        path,
        branch,
        label,
        focus,
    })
}

fn worktree_remove(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut force = false;
    let mut permit_common_dir = None;
    let mut permit_path = None;
    let mut permit_branch = None;
    let mut permit_head = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--permit-common-dir" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-common-dir");
                    return Ok(2);
                };
                permit_common_dir = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--permit-path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-path");
                    return Ok(2);
                };
                permit_path = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--permit-branch" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-branch");
                    return Ok(2);
                };
                permit_branch = Some(value.clone());
                index += 2;
            }
            "--permit-head" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --permit-head");
                    return Ok(2);
                };
                permit_head = Some(value.clone());
                index += 2;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(workspace_id) = workspace_id else {
        eprintln!("usage: herdr worktree remove --workspace ID [--force]");
        return Ok(2);
    };

    let permit = match parse_permit(permit_common_dir, permit_path, permit_branch, permit_head) {
        Ok(permit) => permit,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::runtime::worktree_remove(WorktreeRemoveParams {
        workspace_id,
        force,
        permit,
    })
}

fn print_worktree_help() {
    eprintln!("herdr worktree commands:");
    eprintln!("  herdr worktree list [--workspace ID | --cwd PATH]");
    eprintln!(
        "  herdr worktree create [--workspace ID | --cwd PATH] [--branch NAME] [--base REF] [--path PATH] [--label TEXT] [--focus] [--no-focus] [--permit-common-dir PATH --permit-path PATH --permit-branch NAME --permit-head OID]"
    );
    eprintln!(
        "  herdr worktree open [--workspace ID | --cwd PATH] (--path PATH | --branch NAME) [--label TEXT] [--focus] [--no-focus]"
    );
    eprintln!(
        "  herdr worktree remove --workspace ID [--force] [--permit-common-dir PATH --permit-path PATH --permit-branch NAME --permit-head OID]"
    );
}

fn normalize_path_arg(value: &str) -> std::io::Result<String> {
    let path = crate::worktree::expand_tilde_path(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.display().to_string())
}

fn parse_permit(
    common_dir: Option<String>,
    path: Option<String>,
    branch: Option<String>,
    head_oid: Option<String>,
) -> Result<Option<crate::api::schema::WorktreeExactPermit>, &'static str> {
    let supplied = [
        common_dir.is_some(),
        path.is_some(),
        branch.is_some(),
        head_oid.is_some(),
    ];
    if supplied.iter().any(|supplied| *supplied) && supplied.iter().any(|supplied| !*supplied) {
        return Err("all four permit options are required together");
    }
    Ok(
        common_dir.map(|repo_common_dir| crate::api::schema::WorktreeExactPermit {
            repo_common_dir,
            checkout_path: path.expect("permit path present when permit is present"),
            branch: branch.expect("permit branch present when permit is present"),
            head_oid: head_oid.expect("permit head present when permit is present"),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::parse_permit;

    #[test]
    fn permit_options_must_be_all_or_none() {
        let result = parse_permit(
            Some("/repo/.git".into()),
            Some("/checkout".into()),
            None,
            Some("deadbeef".into()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn complete_permit_is_preserved() {
        let permit = parse_permit(
            Some("/repo/.git".into()),
            Some("/checkout".into()),
            Some("feature".into()),
            Some("deadbeef".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(permit.repo_common_dir, "/repo/.git");
        assert_eq!(permit.checkout_path, "/checkout");
        assert_eq!(permit.branch, "feature");
        assert_eq!(permit.head_oid, "deadbeef");
    }
}
