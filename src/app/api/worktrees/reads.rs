use std::path::PathBuf;

use crate::api::schema::{Method, Request};
use crate::app::App;
use crate::events::{AppEvent, WorktreeReadData, WorktreeReadResult};
use crate::workspace::{GitSpaceMetadata, WorktreeSpaceMembership};

use super::super::responses::encode_error;
use super::{absolute_user_path, find_worktree_entry, ApiFailure, WorktreeSource};

// Capture only source provenance. Filesystem discovery and Git run on the worker;
// workspace indices and the target's open state are resolved again on completion.
struct SourceInput {
    workspace_id: Option<String>,
    membership: Option<WorktreeSpaceMembership>,
    git_space: Option<GitSpaceMetadata>,
    cwd: Option<PathBuf>,
}

impl SourceInput {
    fn resolve(
        mut self,
        allow_linked: bool,
        trust_repository: bool,
    ) -> Result<(Option<String>, WorktreeSource), ApiFailure> {
        let source = if let Some(membership) = self.membership {
            if membership.is_linked_worktree && !allow_linked {
                return Err(linked_source_error());
            }
            let source_checkout_path = if membership.is_linked_worktree {
                self.workspace_id = None;
                membership.repo_root.clone()
            } else {
                membership.checkout_path
            };
            WorktreeSource {
                workspace_idx: None,
                source_checkout_path,
                source_repo_root: membership.repo_root,
                repo_key: membership.key,
                repo_name: membership.label,
            }
        } else {
            let space = self
                .git_space
                .or_else(|| {
                    self.cwd
                        .as_deref()
                        .and_then(crate::workspace::git_space_metadata)
                })
                .ok_or_else(|| {
                    ApiFailure::new(
                        "not_git_worktree",
                        if self.workspace_id.is_some() {
                            "Herdr worktree actions require a workspace inside a Git work tree"
                        } else {
                            "Herdr worktree actions require a path inside a Git work tree"
                        },
                    )
                })?;
            if space.is_linked_worktree {
                if !allow_linked {
                    return Err(linked_source_error());
                }
                self.workspace_id = None;
            }
            worktree_source_from_space(space, allow_linked, trust_repository)
        };
        Ok((self.workspace_id, source))
    }
}

fn linked_source_error() -> ApiFailure {
    ApiFailure::new(
        "linked_worktree_source",
        "New and open worktree actions start from the repo parent workspace.",
    )
}

impl App {
    fn capture_worktree_read_source(
        &self,
        workspace_id: &Option<String>,
        cwd: &Option<String>,
    ) -> Result<SourceInput, ApiFailure> {
        if workspace_id.is_some() && cwd.is_some() {
            return Err(ApiFailure::new(
                "invalid_request",
                "only one of workspace_id or cwd may be supplied",
            ));
        }
        if let Some(cwd) = cwd {
            return Ok(SourceInput {
                workspace_id: None,
                membership: None,
                git_space: None,
                cwd: Some(absolute_user_path(cwd)?),
            });
        }
        let ws_idx = if let Some(workspace_id) = workspace_id {
            self.parse_workspace_id(workspace_id).ok_or_else(|| {
                ApiFailure::new(
                    "workspace_not_found",
                    format!("workspace {workspace_id} not found"),
                )
            })?
        } else {
            self.state
                .active
                .or_else(|| {
                    self.state
                        .workspaces
                        .get(self.state.selected)
                        .map(|_| self.state.selected)
                })
                .ok_or_else(|| {
                    ApiFailure::new(
                        "invalid_request",
                        "workspace_id or cwd is required when no workspace is active",
                    )
                })?
        };
        let ws = &self.state.workspaces[ws_idx];
        self.require_restored_worktree_ready(ws_idx)?;
        Ok(SourceInput {
            workspace_id: Some(ws.id.clone()),
            membership: ws.worktree_space().cloned(),
            git_space: ws.git_space().cloned(),
            cwd: if ws.worktree_space().is_none() && ws.git_space().is_none() {
                ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
            } else {
                None
            },
        })
    }

    pub(super) fn start_api_worktree_read(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
        client_local: bool,
    ) {
        let (workspace_id, cwd, allow_linked, trust_repository) = match &request.method {
            Method::WorktreeList(params) => (
                &params.workspace_id,
                &params.cwd,
                true,
                params.trust_repository,
            ),
            Method::WorktreeOpen(params) => {
                if params.path.is_some() == params.branch.is_some() {
                    let _ = respond_to.send(encode_error(
                        request.id,
                        "invalid_request",
                        "exactly one of path or branch is required",
                    ));
                    return;
                }
                (
                    &params.workspace_id,
                    &params.cwd,
                    false,
                    params.trust_repository,
                )
            }
            _ => unreachable!("only worktree list/open use background discovery"),
        };
        let input = match self.capture_worktree_read_source(workspace_id, cwd) {
            Ok(input) => input,
            Err(err) => {
                let _ = respond_to.send(encode_error(request.id, err.code, err.message));
                return;
            }
        };
        let Ok(permit) = self.worktree_read_slots.clone().try_acquire_owned() else {
            let _ = respond_to.send(encode_error(
                request.id,
                "worktree_busy",
                "too many worktree checks are pending; retry shortly",
            ));
            return;
        };
        let event_tx = self.event_tx.clone();
        let spawn_error_response = respond_to.clone();
        let request_id = request.id.clone();
        let spawned = std::thread::Builder::new()
            .name("worktree-read".into())
            .spawn(move || {
                let source_cwd = input.cwd.clone();
                let mut source_workspace_id = None;
                let result = input
                    .resolve(allow_linked, trust_repository)
                    .and_then(|(workspace_id, source)| {
                        source_workspace_id = workspace_id;
                        let mut entries = crate::worktree::list_existing_worktrees(
                            &source.source_repo_root,
                            trust_repository,
                        )
                        .map_err(|err| ApiFailure::new("worktree_list_failed", err))?;
                        if let Method::WorktreeOpen(params) = &request.method {
                            entries = vec![find_worktree_entry(
                                entries,
                                params.path.clone(),
                                params.branch.clone(),
                            )?];
                        }
                        Ok(WorktreeReadData {
                            source_checkout_path: source.source_checkout_path,
                            source_repo_root: source.source_repo_root,
                            repo_key: source.repo_key,
                            repo_name: source.repo_name,
                            entries,
                        })
                    })
                    .map_err(|err| (err.code.to_string(), err.message));
                let _ = event_tx.blocking_send(AppEvent::WorktreeReadFinished(Box::new(
                    WorktreeReadResult {
                        _permit: permit,
                        request,
                        client_local,
                        source_workspace_id,
                        source_cwd,
                        result,
                        respond_to,
                    },
                )));
            });
        if let Err(err) = spawned {
            let _ = spawn_error_response.send(encode_error(
                request_id,
                "worktree_list_failed",
                format!("could not start worktree discovery: {err}"),
            ));
        }
    }

    pub(crate) fn handle_api_worktree_read_finished(&mut self, result: WorktreeReadResult) {
        let response = match result.result {
            Err((code, message)) => encode_error(result.request.id, &code, message),
            Ok(data) => {
                let mut source = WorktreeSource {
                    workspace_idx: None,
                    source_checkout_path: data.source_checkout_path,
                    source_repo_root: data.source_repo_root,
                    repo_key: data.repo_key,
                    repo_name: data.repo_name,
                };
                // An index captured before Git ran could now name a different workspace.
                source.workspace_idx = result
                    .source_workspace_id
                    .as_ref()
                    .and_then(|id| self.state.workspaces.iter().position(|ws| &ws.id == id))
                    .filter(|&idx| {
                        let ws = &self.state.workspaces[idx];
                        if let Some(current) = ws.worktree_space() {
                            !current.is_linked_worktree
                                && current.key == source.repo_key
                                && current.repo_root == source.source_repo_root
                                && current.checkout_path == source.source_checkout_path
                        } else if let Some(current) = ws.git_space() {
                            !current.is_linked_worktree
                                && current.key == source.repo_key
                                && current.repo_root == source.source_repo_root
                        } else {
                            // Git was already discovered on the worker. A cache miss here
                            // must only compare the captured source path, not rediscover it.
                            result.source_cwd.as_ref().is_some_and(|cwd| {
                                ws.resolved_identity_cwd_from(
                                    &self.state.terminals,
                                    &self.terminal_runtimes,
                                )
                                .as_ref()
                                    == Some(cwd)
                            })
                        }
                    })
                    .or_else(|| self.find_parent_workspace_by_key(&source.repo_key))
                    .or_else(|| self.open_workspace_idx_for_checkout(&source.source_checkout_path));
                match result.request.method {
                    Method::WorktreeList(_) => {
                        self.finish_worktree_list(result.request.id, source, data.entries)
                    }
                    Method::WorktreeOpen(params) => self.finish_worktree_open(
                        result.request.id,
                        params,
                        source,
                        data.entries
                            .into_iter()
                            .next()
                            .expect("open discovery selects one worktree"),
                    ),
                    _ => unreachable!("only worktree list/open use background discovery"),
                }
            }
        };
        let _ = result.respond_to.send(response);
    }
}

fn worktree_source_from_space(
    space: crate::workspace::GitSpaceMetadata,
    allow_linked: bool,
    trust_repository: bool,
) -> WorktreeSource {
    let source_checkout_path = if allow_linked {
        parent_checkout_path_for_space(&space, trust_repository)
    } else {
        space.repo_root.clone()
    };
    WorktreeSource {
        workspace_idx: None,
        source_checkout_path: source_checkout_path.clone(),
        source_repo_root: source_checkout_path,
        repo_key: space.key,
        repo_name: space.repo_name,
    }
}

fn parent_checkout_path_for_space(
    space: &crate::workspace::GitSpaceMetadata,
    trust_repository: bool,
) -> PathBuf {
    if !space.is_linked_worktree {
        return space.repo_root.clone();
    }

    crate::worktree::list_existing_worktrees(&space.repo_root, trust_repository)
        .ok()
        .and_then(|entries| {
            entries.into_iter().find_map(|entry| {
                let entry_space = crate::workspace::git_space_metadata(&entry.path)?;
                if entry_space.key == space.key && !entry_space.is_linked_worktree {
                    Some(entry_space.repo_root)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| space.repo_root.clone())
}
