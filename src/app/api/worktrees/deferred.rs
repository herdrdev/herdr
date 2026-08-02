use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, Request, ResponseResult, WorktreeCreateParams,
    WorktreeExactPermit, WorktreeMutationReceipt, WorktreeRemoveParams,
};
use crate::app::App;
use crate::events::{ApiWorktreeAddRequest, ApiWorktreeRemoveRequest, AppEvent};

use super::super::responses::{encode_error, encode_success};
use super::{absolute_user_path, WorktreeSource};

fn validate_permit(permit: &WorktreeExactPermit) -> Result<(), (&'static str, &'static str)> {
    if permit.repo_common_dir.trim().is_empty()
        || permit.checkout_path.trim().is_empty()
        || permit.branch.trim().is_empty()
        || permit.head_oid.trim().is_empty()
    {
        return Err(("incomplete_permit", "all exact permit fields are required"));
    }
    if !Path::new(&permit.repo_common_dir).is_absolute()
        || !Path::new(&permit.checkout_path).is_absolute()
    {
        return Err(("invalid_permit", "permit paths must be absolute"));
    }
    if !matches!(permit.head_oid.len(), 40 | 64)
        || !permit.head_oid.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return Err((
            "invalid_permit",
            "permit head_oid must be a full hexadecimal object ID",
        ));
    }
    Ok(())
}

fn oid_matches(actual: &str, permitted: &str) -> bool {
    actual.eq_ignore_ascii_case(permitted)
}

fn identity_matches(
    identity: &crate::worktree::WorktreeIdentity,
    permit: &WorktreeExactPermit,
) -> bool {
    crate::worktree::canonical_or_original(Path::new(&permit.repo_common_dir))
        == identity.repo_common_dir
        && crate::worktree::canonical_or_original(Path::new(&permit.checkout_path))
            == identity.checkout_path
        && identity.branch.as_deref() == Some(permit.branch.as_str())
        && oid_matches(&identity.head_oid, &permit.head_oid)
}

fn receipt_from_identity(
    operation: &str,
    identity: &crate::worktree::WorktreeIdentity,
) -> Result<WorktreeMutationReceipt, &'static str> {
    let Some(branch) = identity.branch.clone() else {
        return Err("identity mismatch: worktree is detached");
    };
    Ok(WorktreeMutationReceipt {
        operation: operation.to_string(),
        repo_common_dir: identity.repo_common_dir.display().to_string(),
        checkout_path: identity.checkout_path.display().to_string(),
        branch,
        head_oid: identity.head_oid.clone(),
    })
}

fn rollback_invalid_managed_create(
    source_checkout_path: &Path,
    checkout_path: &Path,
    permit: &WorktreeExactPermit,
    mismatch: &str,
) -> String {
    let cleanup =
        crate::worktree::build_worktree_remove_command(source_checkout_path, checkout_path, false);
    let cleanup_result = crate::worktree::run_worktree_command(&cleanup).and_then(|()| {
        crate::worktree::delete_local_branch_if_matches(
            source_checkout_path,
            &permit.branch,
            &permit.head_oid,
        )
    });
    match cleanup_result {
        Ok(()) => format!("identity mismatch: {mismatch}"),
        Err(cleanup_error) => {
            format!("identity mismatch: {mismatch}; managed cleanup failed: {cleanup_error}")
        }
    }
}

impl App {
    pub(crate) fn handle_deferred_worktree_api_request(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        match request.method {
            crate::api::schema::Method::WorktreeCreate(params) => {
                self.start_api_worktree_create(request.id, params, respond_to);
                true
            }
            crate::api::schema::Method::WorktreeRemove(params) => {
                self.start_api_worktree_remove(request.id, params, respond_to);
                true
            }
            _ => false,
        }
    }

    fn send_api_response(respond_to: std::sync::mpsc::Sender<String>, response: String) {
        let _ = respond_to.send(response);
    }

    fn next_api_worktree_operation_id(&mut self) -> u64 {
        let id = self.next_api_worktree_operation_id;
        self.next_api_worktree_operation_id = self.next_api_worktree_operation_id.saturating_add(1);
        id
    }

    fn api_create_source_workspace_idx(&self, api: &ApiWorktreeAddRequest) -> Option<usize> {
        let Some(source_workspace_id) = api.source_workspace_id.as_ref() else {
            return self.find_parent_workspace_by_key(&api.repo_key);
        };
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|ws| &ws.id == source_workspace_id)
        else {
            return self.find_parent_workspace_by_key(&api.repo_key);
        };
        let workspace = &self.state.workspaces[ws_idx];
        if let Some(expected) = api.source_existing_membership.as_ref() {
            if workspace.worktree_space() == Some(expected) {
                return Some(ws_idx);
            }
            return self.find_parent_workspace_by_key(&api.repo_key);
        }

        if let Some(current) = workspace.worktree_space() {
            let expected = crate::workspace::WorktreeSpaceMembership {
                key: api.repo_key.clone(),
                label: api.repo_name.clone(),
                repo_root: api.source_repo_root.clone(),
                checkout_path: api.source_checkout_path.clone(),
                is_linked_worktree: false,
            };
            if current == &expected {
                return Some(ws_idx);
            }
            return self.find_parent_workspace_by_key(&api.repo_key);
        }
        let git_space = workspace.git_space().cloned().or_else(|| {
            workspace
                .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                .as_deref()
                .and_then(crate::workspace::git_space_metadata)
        });
        if git_space.is_some_and(|space| {
            !space.is_linked_worktree
                && space.key == api.repo_key
                && crate::worktree::canonical_or_original(&space.repo_root)
                    == crate::worktree::canonical_or_original(&api.source_repo_root)
        }) {
            Some(ws_idx)
        } else {
            self.find_parent_workspace_by_key(&api.repo_key)
        }
    }

    fn start_api_worktree_create(
        &mut self,
        id: String,
        params: WorktreeCreateParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let managed_permit = params.permit.clone();
        if let Some(permit) = managed_permit.as_ref() {
            if let Err((code, message)) = validate_permit(permit) {
                Self::send_api_response(respond_to, encode_error(id, code, message));
                return;
            }
            if params.path.is_none() || params.branch.is_none() || params.base.is_none() {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "incomplete_permit",
                        "managed create requires explicit path, branch, and base",
                    ),
                );
                return;
            }
        }
        let branch = params
            .branch
            .clone()
            .unwrap_or_else(|| {
                let seed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
                    .unwrap_or(0);
                crate::worktree::generated_branch_slug(seed)
            })
            .trim()
            .to_string();
        if branch.is_empty() {
            Self::send_api_response(
                respond_to,
                encode_error(id, "invalid_request", "branch is required"),
            );
            return;
        }
        let base = params.base.clone().unwrap_or_else(|| "HEAD".into());
        let source = match self.resolve_worktree_source(params.workspace_id, params.cwd) {
            Ok(source) => source,
            Err(err) => {
                Self::send_api_response(respond_to, encode_error(id, err.code, err.message));
                return;
            }
        };
        if let Some(permit) = managed_permit.as_ref() {
            let identity =
                match crate::worktree::read_worktree_identity(&source.source_checkout_path) {
                    Ok(identity) => identity,
                    Err(message) => {
                        Self::send_api_response(
                            respond_to,
                            encode_error(id, "identity_mismatch", message),
                        );
                        return;
                    }
                };
            if crate::worktree::canonical_or_original(Path::new(&permit.repo_common_dir))
                != identity.repo_common_dir
                || branch != permit.branch
            {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "identity_mismatch",
                        "live source identity does not match permit",
                    ),
                );
                return;
            }
            let resolved_base =
                match crate::worktree::resolve_git_revision(&source.source_checkout_path, &base) {
                    Ok(oid) => oid,
                    Err(message) => {
                        Self::send_api_response(
                            respond_to,
                            encode_error(id, "identity_mismatch", message),
                        );
                        return;
                    }
                };
            if !oid_matches(&resolved_base, &permit.head_oid) {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "identity_mismatch",
                        "resolved base does not match permit head_oid",
                    ),
                );
                return;
            }
        }
        let checkout_path = match params.path {
            Some(path) => match absolute_user_path(&path) {
                Ok(path) => path,
                Err(err) => {
                    Self::send_api_response(respond_to, encode_error(id, err.code, err.message));
                    return;
                }
            },
            None => crate::worktree::default_checkout_path(
                &self.state.worktree_directory,
                &source.repo_name,
                &branch,
            ),
        };
        if let Some(permit) = managed_permit.as_ref() {
            if crate::worktree::canonical_or_original(Path::new(&permit.checkout_path))
                != crate::worktree::canonical_or_original(&checkout_path)
            {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "identity_mismatch",
                        "checkout path does not match permit",
                    ),
                );
                return;
            }
        }
        let checkout_key = crate::worktree::canonical_or_original(&checkout_path);
        if self
            .pending_api_worktree_creates
            .contains_key(&checkout_key)
            || self
                .pending_api_worktree_remove_paths
                .contains_key(&checkout_key)
        {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "worktree_operation_in_progress",
                    "worktree operation is already in progress for this checkout",
                ),
            );
            return;
        }
        let operation_id = self.next_api_worktree_operation_id();
        self.pending_api_worktree_creates
            .insert(checkout_key.clone(), operation_id);

        let parent_dir = checkout_path.parent().map(Path::to_path_buf);
        let source_workspace_id = source
            .workspace_idx
            .and_then(|idx| self.state.workspaces.get(idx).map(|ws| ws.id.clone()));
        let source_existing_membership = source_workspace_id.as_ref().and_then(|workspace_id| {
            self.state
                .workspaces
                .iter()
                .find(|ws| &ws.id == workspace_id)
                .and_then(|ws| ws.worktree_space().cloned())
        });
        let api_request = ApiWorktreeAddRequest {
            id,
            operation_id,
            checkout_key,
            source_workspace_id,
            source_existing_membership,
            source_checkout_path: source.source_checkout_path,
            source_repo_root: source.source_repo_root,
            repo_key: source.repo_key,
            repo_name: source.repo_name,
            branch,
            permit: managed_permit,
            label: params.label,
            focus: params.focus,
            respond_to,
        };
        let path = checkout_path;
        let source_checkout_path = api_request.source_checkout_path.clone();
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let (result, receipt) = if let Some(permit) = api_request.permit.as_ref() {
                let result = if let Some(parent_dir) = parent_dir {
                    std::fs::create_dir_all(&parent_dir).map_err(|err| err.to_string())
                } else {
                    Ok(())
                }
                .and_then(|()| {
                    let command = crate::worktree::build_worktree_add_new_branch_command(
                        &source_checkout_path,
                        &path,
                        &api_request.branch,
                        &permit.head_oid,
                    );
                    crate::worktree::run_worktree_command(&command)
                })
                .and_then(|()| {
                    let identity = match crate::worktree::read_worktree_identity(&path) {
                        Ok(identity) => identity,
                        Err(message) => {
                            return Err(rollback_invalid_managed_create(
                                &source_checkout_path,
                                &path,
                                permit,
                                &message,
                            ));
                        }
                    };
                    if !identity_matches(&identity, permit) {
                        return Err(rollback_invalid_managed_create(
                            &source_checkout_path,
                            &path,
                            permit,
                            "created worktree does not match permit",
                        ));
                    }
                    receipt_from_identity("create", &identity)
                        .map_err(|message| message.to_string())
                });
                match result {
                    Ok(receipt) => (Ok(()), Some(receipt)),
                    Err(message) => (Err(message), None),
                }
            } else {
                let result = if let Some(parent_dir) = parent_dir {
                    std::fs::create_dir_all(&parent_dir).map_err(|err| err.to_string())
                } else {
                    Ok(())
                }
                .and_then(|()| {
                    crate::worktree::run_worktree_add_command(
                        &source_checkout_path,
                        &path,
                        &api_request.branch,
                        &base,
                    )
                });
                (result, None)
            };
            let _ = event_tx.blocking_send(AppEvent::WorktreeAddFinished(Box::new(
                crate::events::WorktreeAddResult {
                    path,
                    api_request: Some(api_request),
                    receipt,
                    result,
                },
            )));
        });
    }

    fn start_api_worktree_remove(
        &mut self,
        id: String,
        params: WorktreeRemoveParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let Some(ws_idx) = self.parse_workspace_id(&params.workspace_id) else {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "workspace_not_found",
                    format!("workspace {} not found", params.workspace_id),
                ),
            );
            return;
        };
        let Some(space) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.worktree_space().cloned())
        else {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "not_linked_worktree",
                    "workspace is not a Herdr-managed worktree checkout",
                ),
            );
            return;
        };
        if !space.is_linked_worktree {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "not_linked_worktree",
                    "workspace is not a linked worktree checkout",
                ),
            );
            return;
        }
        let managed_permit = params.permit.clone();
        if let Some(permit) = managed_permit.as_ref() {
            if let Err((code, message)) = validate_permit(permit) {
                Self::send_api_response(respond_to, encode_error(id, code, message));
                return;
            }
            if params.force {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "forbidden_force",
                        "force is forbidden for managed worktree removal",
                    ),
                );
                return;
            }
            if crate::worktree::canonical_or_original(Path::new(&permit.checkout_path))
                != crate::worktree::canonical_or_original(&space.checkout_path)
            {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "identity_mismatch",
                        "checkout path does not match permit",
                    ),
                );
                return;
            }
        }

        #[cfg(windows)]
        {
            if !params.force
                && crate::worktree::checkout_has_dirty_files(&space.checkout_path).unwrap_or(false)
            {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "dirty_worktree_requires_force",
                        crate::worktree::worktree_dirty_remove_message(&space.checkout_path),
                    ),
                );
                return;
            }
        }

        let workspace_internal_id = self.state.workspaces[ws_idx].id.clone();
        let checkout_key = crate::worktree::canonical_or_original(&space.checkout_path);
        if self
            .pending_api_worktree_removes
            .contains_key(&workspace_internal_id)
            || self
                .pending_api_worktree_remove_paths
                .contains_key(&checkout_key)
            || self
                .pending_api_worktree_creates
                .contains_key(&checkout_key)
        {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "worktree_operation_in_progress",
                    "worktree operation is already in progress for this checkout",
                ),
            );
            return;
        }

        if Self::should_shutdown_workspace_terminal_runtimes_for_worktree_remove(params.force) {
            self.shutdown_workspace_terminal_runtimes_for_worktree_remove(ws_idx);
        }

        let operation_id = self.next_api_worktree_operation_id();
        self.pending_api_worktree_removes
            .insert(workspace_internal_id.clone(), operation_id);
        self.pending_api_worktree_remove_paths
            .insert(checkout_key.clone(), operation_id);
        let workspace_snapshot = self.workspace_info(ws_idx);
        let worktree = self.worktree_info_for_membership(&space, None);
        let api_request = ApiWorktreeRemoveRequest {
            id,
            operation_id,
            checkout_key,
            permit: managed_permit.clone(),
            respond_to,
        };
        let repo_root = space.repo_root;
        let path = space.checkout_path;
        let force = params.force;
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let (result, receipt) = if let Some(permit) = api_request.permit.as_ref() {
                match crate::worktree::read_worktree_identity(&path) {
                    Ok(identity) if identity_matches(&identity, permit) => {
                        let receipt = receipt_from_identity("remove", &identity)
                            .map_err(|message| message.to_string());
                        let command = crate::worktree::build_worktree_remove_command(
                            &repo_root, &path, false,
                        );
                        let result = receipt.and_then(|receipt| {
                            crate::worktree::run_worktree_command(&command)?;
                            if crate::worktree::worktree_list_contains_path(&repo_root, &path)? {
                                return Err("worktree removal did not clear Git inventory".into());
                            }
                            Ok(receipt)
                        });
                        match result {
                            Ok(receipt) => (Ok(()), Some(receipt)),
                            Err(message) => (Err(message), None),
                        }
                    }
                    Ok(_) => (
                        Err("identity mismatch: live worktree does not match permit".into()),
                        None,
                    ),
                    Err(message) => (Err(format!("identity mismatch: {message}")), None),
                }
            } else {
                let command =
                    crate::worktree::build_worktree_remove_command(&repo_root, &path, force);
                (
                    crate::worktree::run_worktree_remove_command_with_recovery(
                        &command, &repo_root, &path, force,
                    ),
                    None,
                )
            };
            let _ = event_tx.blocking_send(AppEvent::WorktreeRemoveFinished(Box::new(
                crate::events::WorktreeRemoveResult {
                    workspace_id: workspace_internal_id,
                    path,
                    workspace: Some(Box::new(workspace_snapshot)),
                    worktree: Some(Box::new(worktree)),
                    forced: force,
                    receipt,
                    api_request: Some(api_request),
                    result,
                },
            )));
        });
    }

    pub(crate) fn handle_api_worktree_add_finished(
        &mut self,
        mut result: crate::events::WorktreeAddResult,
    ) {
        let Some(api) = result.api_request.take() else {
            return;
        };
        let checkout_key = api.checkout_key.clone();
        let operation_matches = self
            .pending_api_worktree_creates
            .get(&checkout_key)
            .is_some_and(|operation_id| *operation_id == api.operation_id);
        if !operation_matches {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "stale_worktree_operation",
                    "worktree create completed after the operation was superseded",
                ),
            );
            return;
        }
        self.pending_api_worktree_creates.remove(&checkout_key);

        if let Err(err) = result.result {
            if let Some(create) = &mut self.state.worktree_create {
                if create.checkout_path == result.path {
                    create.creating = false;
                    create.error = Some(err.clone());
                }
            }
            let code = if api.permit.is_some() && err.starts_with("identity mismatch") {
                "identity_mismatch"
            } else {
                "worktree_create_failed"
            };
            Self::send_api_response(api.respond_to, encode_error(api.id, code, err));
            return;
        }
        if api.permit.is_some() && result.receipt.is_none() {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "identity_mismatch",
                    "managed create completed without an exact receipt",
                ),
            );
            return;
        }

        let source_workspace_idx = self.api_create_source_workspace_idx(&api);
        let mut source = WorktreeSource {
            workspace_idx: source_workspace_idx,
            source_checkout_path: api.source_checkout_path,
            source_repo_root: api.source_repo_root,
            repo_key: api.repo_key,
            repo_name: api.repo_name,
        };
        if let Err(err) = self.ensure_source_parent_membership(&mut source, true) {
            Self::send_api_response(api.respond_to, encode_error(api.id, err.code, err.message));
            return;
        }

        let (ws_idx, created_workspace) =
            if let Some(ws_idx) = self.open_workspace_idx_for_checkout(&result.path) {
                if api.focus {
                    self.state.switch_workspace(ws_idx);
                }
                (ws_idx, false)
            } else {
                match self.create_workspace_with_options(result.path.clone(), api.focus) {
                    Ok(ws_idx) => (ws_idx, true),
                    Err(err) => {
                        Self::send_api_response(
                            api.respond_to,
                            encode_error(
                                api.id,
                                "worktree_open_failed",
                                format!("created worktree but failed to open workspace: {err}"),
                            ),
                        );
                        return;
                    }
                }
            };

        self.mark_worktree_membership(
            &source,
            ws_idx,
            result.path.clone(),
            true,
            !created_workspace,
        );
        if let Some(label) = api.label {
            if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                ws.set_custom_name(label);
            }
        }
        if self
            .state
            .worktree_create
            .as_ref()
            .is_some_and(|create| create.checkout_path == result.path)
        {
            self.state.worktree_create = None;
            self.state.name_input.clear();
            self.state.name_input_replace_on_type = false;
            self.state.mode = crate::app::Mode::Terminal;
        }
        self.state.mark_session_dirty();
        if created_workspace {
            self.emit_workspace_open_events(ws_idx);
        }
        let Some(worktree) = self.worktree_info_for_workspace(ws_idx) else {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "worktree_open_failed",
                    "created workspace has no linked worktree identity",
                ),
            );
            return;
        };
        let receipt = result.receipt.clone();
        self.emit_worktree_created_event_with_receipt(ws_idx, worktree.clone(), receipt.clone());
        let tab_idx = self.state.workspaces[ws_idx].active_tab;
        let response = encode_success(
            api.id,
            ResponseResult::WorktreeCreated {
                workspace: self.workspace_info(ws_idx),
                tab: self
                    .tab_info(ws_idx, tab_idx)
                    .expect("created worktree workspace should have an active tab"),
                root_pane: self
                    .root_pane_info(ws_idx, tab_idx)
                    .expect("created worktree workspace should have an active root pane"),
                worktree,
                receipt,
            },
        );
        Self::send_api_response(api.respond_to, response);
    }

    pub(crate) fn handle_api_worktree_remove_finished(
        &mut self,
        mut result: crate::events::WorktreeRemoveResult,
    ) {
        let Some(api) = result.api_request.take() else {
            return;
        };
        let operation_matches = self
            .pending_api_worktree_removes
            .get(&result.workspace_id)
            .is_some_and(|operation_id| *operation_id == api.operation_id)
            && self
                .pending_api_worktree_remove_paths
                .get(&api.checkout_key)
                .is_some_and(|operation_id| *operation_id == api.operation_id);
        if !operation_matches {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "stale_worktree_operation",
                    "worktree remove completed after the operation was superseded",
                ),
            );
            return;
        }
        self.pending_api_worktree_removes
            .remove(&result.workspace_id);
        self.pending_api_worktree_remove_paths
            .remove(&api.checkout_key);

        if let Err(message) = result.result {
            let code = if api.permit.is_some() && message.starts_with("identity mismatch") {
                "identity_mismatch"
            } else if !result.forced && crate::worktree::is_dirty_worktree_remove_error(&message) {
                "dirty_worktree_requires_force"
            } else {
                "worktree_remove_failed"
            };
            if let Some(remove) = &mut self.state.worktree_remove {
                if remove.workspace_id == result.workspace_id && remove.path == result.path {
                    remove.removing = false;
                    if code == "dirty_worktree_requires_force" && !remove.force_confirmation {
                        remove.force_confirmation = true;
                        remove.error = None;
                    } else {
                        remove.error = Some(message.clone());
                    }
                }
            }
            Self::send_api_response(api.respond_to, encode_error(api.id, code, message));
            return;
        }

        let mut workspace_id = result.workspace_id.clone();
        let mut workspace_snapshot = result.workspace.as_deref().cloned();
        let mut worktree = result.worktree.as_deref().cloned();
        if let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|ws| ws.id == result.workspace_id)
        {
            let current_matches =
                self.state.workspaces[ws_idx]
                    .worktree_space()
                    .is_some_and(|space| {
                        space.is_linked_worktree && space.checkout_path == result.path
                    });
            if current_matches {
                workspace_id = self.public_workspace_id(ws_idx);
                workspace_snapshot.get_or_insert_with(|| self.workspace_info(ws_idx));
                if worktree.is_none() {
                    worktree = self.state.workspaces[ws_idx]
                        .worktree_space()
                        .cloned()
                        .map(|space| self.worktree_info_for_membership(&space, None));
                }
                self.close_removed_linked_worktree_workspace(ws_idx);
                self.shutdown_detached_terminal_runtimes();
                self.emit_event(EventEnvelope {
                    event: EventKind::WorkspaceClosed,
                    data: EventData::WorkspaceClosed {
                        workspace_id: workspace_id.clone(),
                        workspace: workspace_snapshot.clone(),
                    },
                });
            } else if let Some(snapshot) = workspace_snapshot.as_ref() {
                workspace_id = snapshot.workspace_id.clone();
            }
        } else if let Some(snapshot) = workspace_snapshot.as_ref() {
            workspace_id = snapshot.workspace_id.clone();
        }

        let Some(worktree) = worktree else {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "worktree_remove_failed",
                    "removed worktree but lost worktree snapshot",
                ),
            );
            return;
        };
        if api.permit.is_some() && result.receipt.is_none() {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "identity_mismatch",
                    "managed remove completed without an exact receipt",
                ),
            );
            return;
        }
        let receipt = result.receipt.clone();
        self.emit_worktree_removed_event_with_receipt(
            workspace_id.clone(),
            workspace_snapshot,
            worktree,
            result.forced,
            receipt.clone(),
        );
        if self.state.worktree_remove.as_ref().is_some_and(|remove| {
            remove.workspace_id == result.workspace_id && remove.path == result.path
        }) {
            self.state.worktree_remove = None;
            self.state.mode = if self.state.active.is_some() {
                crate::app::Mode::Terminal
            } else {
                crate::app::Mode::Navigate
            };
        }
        let response = encode_success(
            api.id,
            ResponseResult::WorktreeRemoved {
                workspace_id,
                path: result.path.display().to_string(),
                forced: result.forced,
                receipt,
            },
        );
        Self::send_api_response(api.respond_to, response);
    }
}

#[cfg(test)]
mod permit_tests {
    use super::*;
    use std::path::Path;

    fn permit() -> WorktreeExactPermit {
        WorktreeExactPermit {
            repo_common_dir: "/repo/.git".into(),
            checkout_path: "/repo/worktree".into(),
            branch: "feature".into(),
            head_oid: "0123456789abcdef0123456789abcdef01234567".into(),
        }
    }

    #[test]
    fn incomplete_permit_is_rejected() {
        let mut permit = permit();
        permit.branch.clear();
        assert_eq!(
            validate_permit(&permit),
            Err(("incomplete_permit", "all exact permit fields are required"))
        );
    }

    #[test]
    fn identity_mismatch_is_fail_closed() {
        let identity = crate::worktree::WorktreeIdentity {
            repo_common_dir: Path::new("/repo/.git").to_path_buf(),
            checkout_path: Path::new("/repo/worktree").to_path_buf(),
            branch: Some("other".into()),
            head_oid: "0123456789abcdef0123456789abcdef01234567".into(),
        };
        assert!(!identity_matches(&identity, &permit()));
    }

    #[test]
    fn receipt_uses_git_identity_and_operation() {
        let identity = crate::worktree::WorktreeIdentity {
            repo_common_dir: Path::new("/repo/.git").to_path_buf(),
            checkout_path: Path::new("/repo/worktree").to_path_buf(),
            branch: Some("feature".into()),
            head_oid: "0123456789abcdef0123456789abcdef01234567".into(),
        };
        let receipt = receipt_from_identity("remove", &identity).unwrap();
        assert_eq!(receipt.operation, "remove");
        assert_eq!(receipt.branch, "feature");
        assert_eq!(receipt.head_oid, identity.head_oid);
    }
}
