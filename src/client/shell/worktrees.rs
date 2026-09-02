use super::*;

impl ClientShellState {
    fn endpoint_worktree_directory(&self) -> Option<std::path::PathBuf> {
        self.snapshot
            .as_deref()
            .map(|snapshot| std::path::PathBuf::from(&snapshot.worktree_directory))
    }

    pub(super) fn insert_worktree_overlay_text(&mut self, text: &str) -> bool {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::WorktreeCreate(create)) if !create.creating => {
                if create.replace_on_type {
                    create.branch.clear();
                    create.replace_on_type = false;
                }
                create.branch.push_str(text);
                self.sync_worktree_create_path();
                true
            }
            Some(ClientShellOverlay::WorktreeOpen(open))
                if open.search_focused && !open.opening =>
            {
                open.query.push_str(text);
                if let Some(first) = open.filtered_indices().first().copied() {
                    open.selected = first;
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn route_worktree_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::WorktreeCreate(_)) => {
                let creating = matches!(
                    self.overlay,
                    Some(ClientShellOverlay::WorktreeCreate(
                        ClientWorktreeCreateOverlay { creating: true, .. }
                    ))
                );
                match code {
                    KeyCode::Esc if !creating => {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                    KeyCode::Enter => self.submit_worktree_create(outcome),
                    KeyCode::Backspace if !creating => {
                        if let Some(ClientShellOverlay::WorktreeCreate(create)) =
                            self.overlay.as_mut()
                        {
                            if create.replace_on_type {
                                create.branch.clear();
                                create.replace_on_type = false;
                            } else {
                                create.branch.pop();
                            }
                        }
                        self.sync_worktree_create_path();
                        outcome.repaint = true;
                    }
                    KeyCode::Char(character)
                        if !creating
                            && modifiers
                                .difference(crossterm::event::KeyModifiers::SHIFT)
                                .is_empty() =>
                    {
                        let text = key
                            .generated_text
                            .clone()
                            .unwrap_or_else(|| character.to_string());
                        self.insert_worktree_overlay_text(&text);
                        outcome.repaint = true;
                    }
                    _ => {}
                }
                true
            }
            Some(ClientShellOverlay::WorktreeOpen(_)) => {
                let opening = matches!(
                    self.overlay,
                    Some(ClientShellOverlay::WorktreeOpen(
                        ClientWorktreeOpenOverlay { opening: true, .. }
                    ))
                );
                let search_focused = matches!(
                    self.overlay,
                    Some(ClientShellOverlay::WorktreeOpen(
                        ClientWorktreeOpenOverlay {
                            search_focused: true,
                            ..
                        }
                    ))
                );
                match code {
                    KeyCode::Esc if !opening => {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                    KeyCode::Enter => self.submit_worktree_open(outcome),
                    KeyCode::Up if !opening => {
                        self.move_worktree_open_selection(-1);
                        outcome.repaint = true;
                    }
                    KeyCode::Down if !opening => {
                        self.move_worktree_open_selection(1);
                        outcome.repaint = true;
                    }
                    KeyCode::Char('/') if !opening && !search_focused => {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.search_focused = true;
                        }
                        outcome.repaint = true;
                    }
                    KeyCode::Backspace if !opening && search_focused => {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.query.pop();
                            if let Some(first) = open.filtered_indices().first().copied() {
                                open.selected = first;
                            }
                        }
                        outcome.repaint = true;
                    }
                    KeyCode::Char(character)
                        if !opening
                            && search_focused
                            && modifiers
                                .difference(crossterm::event::KeyModifiers::SHIFT)
                                .is_empty() =>
                    {
                        let text = key
                            .generated_text
                            .clone()
                            .unwrap_or_else(|| character.to_string());
                        self.insert_worktree_overlay_text(&text);
                        outcome.repaint = true;
                    }
                    _ => {}
                }
                true
            }
            Some(ClientShellOverlay::WorktreeRemove(_)) => {
                let removing = matches!(
                    self.overlay,
                    Some(ClientShellOverlay::WorktreeRemove(
                        ClientWorktreeRemoveOverlay { removing: true, .. }
                    ))
                );
                match code {
                    KeyCode::Esc if !removing => {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                    KeyCode::Enter => self.submit_worktree_remove(outcome),
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn begin_worktree_action(
        &mut self,
        action: crate::input::KeybindAction,
        outcome: &mut ClientShellInput,
    ) {
        let Some(workspace_id) = self.workspace_action_id() else {
            return;
        };
        self.begin_worktree_action_for(action, workspace_id, outcome);
    }

    pub(super) fn begin_worktree_action_for(
        &mut self,
        action: crate::input::KeybindAction,
        workspace_id: String,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{Method, WorktreeListParams};
        use crate::input::KeybindAction;

        let workspace = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
        });
        let linked = workspace
            .and_then(|workspace| workspace.worktree.as_ref())
            .is_some_and(|worktree| worktree.is_linked_worktree);
        let kind = match action {
            KeybindAction::NewWorktree | KeybindAction::OpenWorktree if linked => {
                self.endpoint_error = Some(
                    "New and open worktree actions start from the repo parent workspace."
                        .to_owned(),
                );
                outcome.repaint = true;
                return;
            }
            KeybindAction::NewWorktree => PendingEndpointKind::PrepareWorktreeCreate {
                workspace_id: workspace_id.clone(),
            },
            KeybindAction::OpenWorktree => PendingEndpointKind::PrepareWorktreeOpen {
                workspace_id: workspace_id.clone(),
            },
            KeybindAction::RemoveWorktree if !linked => {
                self.endpoint_error =
                    Some("This workspace is not a Herdr-managed worktree checkout.".to_owned());
                outcome.repaint = true;
                return;
            }
            KeybindAction::RemoveWorktree => PendingEndpointKind::PrepareWorktreeRemove {
                workspace_id: workspace_id.clone(),
            },
            _ => return,
        };
        self.push_endpoint_method_with_kind(
            Method::WorktreeList(WorktreeListParams {
                workspace_id: Some(workspace_id),
                cwd: None,
                trust_repository: false,
            }),
            kind,
            outcome,
        );
    }

    pub(super) fn sync_worktree_create_path(&mut self) {
        let Some(worktree_directory) = self.endpoint_worktree_directory() else {
            return;
        };
        let Some(ClientShellOverlay::WorktreeCreate(create)) = self.overlay.as_mut() else {
            return;
        };
        create.checkout_path = crate::worktree::default_checkout_path(
            &worktree_directory,
            &create.repo_name,
            &create.branch,
        )
        .display()
        .to_string();
        create.error = None;
    }

    pub(super) fn submit_worktree_create(&mut self, outcome: &mut ClientShellInput) {
        let Some(worktree_directory) = self.endpoint_worktree_directory() else {
            return;
        };
        let Some(ClientShellOverlay::WorktreeCreate(create)) = self.overlay.as_mut() else {
            return;
        };
        if create.creating {
            return;
        }
        let branch = create.branch.trim().to_owned();
        if branch.is_empty() {
            create.error = Some("branch is required".to_owned());
            outcome.repaint = true;
            return;
        }
        create.branch = branch.clone();
        create.replace_on_type = false;
        create.checkout_path =
            crate::worktree::default_checkout_path(&worktree_directory, &create.repo_name, &branch)
                .display()
                .to_string();
        create.creating = true;
        create.error = None;
        let workspace_id = create.source_workspace_id.clone();
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::WorktreeCreate(crate::api::schema::WorktreeCreateParams {
                workspace_id: Some(workspace_id),
                cwd: None,
                branch: Some(branch),
                base: Some("HEAD".to_owned()),
                path: None,
                label: None,
                focus: true,
                trust_repository: false,
            }),
            PendingEndpointKind::WorktreeCreate,
            outcome,
        ) {
            if let Some(ClientShellOverlay::WorktreeCreate(create)) = self.overlay.as_mut() {
                create.creating = false;
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn move_worktree_open_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut() else {
            return;
        };
        let filtered = open.filtered_indices();
        if filtered.is_empty() {
            open.selected = 0;
            return;
        }
        let current = filtered
            .iter()
            .position(|index| *index == open.selected)
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, filtered.len() as isize - 1) as usize;
        open.selected = filtered[next];
    }

    pub(super) fn submit_worktree_open(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut() else {
            return;
        };
        if open.opening {
            return;
        }
        let Some(index) = open.selected_entry_index() else {
            return;
        };
        let Some(entry) = open.entries.get(index) else {
            return;
        };
        let workspace_id = open.source_workspace_id.clone();
        let path = entry.path.clone();
        open.selected = index;
        open.opening = true;
        open.error = None;
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::WorktreeOpen(crate::api::schema::WorktreeOpenParams {
                workspace_id: Some(workspace_id),
                cwd: None,
                path: Some(path),
                branch: None,
                label: None,
                focus: true,
                trust_repository: false,
            }),
            PendingEndpointKind::WorktreeOpen,
            outcome,
        ) {
            if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut() {
                open.opening = false;
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn submit_worktree_remove(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::WorktreeRemove(remove)) = self.overlay.as_mut() else {
            return;
        };
        if remove.removing {
            return;
        }
        let workspace_id = remove.workspace_id.clone();
        let forced = remove.force_confirmation;
        remove.removing = true;
        remove.error = None;
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::WorktreeRemove(crate::api::schema::WorktreeRemoveParams {
                workspace_id,
                force: forced,
                trust_repository: false,
            }),
            PendingEndpointKind::WorktreeRemove { forced },
            outcome,
        ) {
            if let Some(ClientShellOverlay::WorktreeRemove(remove)) = self.overlay.as_mut() {
                remove.removing = false;
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn handle_worktree_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        use crate::api::schema::ResponseResult;

        match (kind, result) {
            (
                PendingEndpointKind::PrepareWorktreeCreate { workspace_id },
                Ok(ResponseResult::WorktreeList { source, .. }),
            ) => {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
                    .unwrap_or(0);
                let branch = crate::worktree::generated_branch_slug(seed);
                let Some(worktree_directory) = self.endpoint_worktree_directory() else {
                    return false;
                };
                let checkout_path = crate::worktree::default_checkout_path(
                    &worktree_directory,
                    &source.repo_name,
                    &branch,
                )
                .display()
                .to_string();
                self.overlay = Some(ClientShellOverlay::WorktreeCreate(
                    ClientWorktreeCreateOverlay {
                        source_workspace_id: workspace_id,
                        repo_name: source.repo_name,
                        branch,
                        checkout_path,
                        replace_on_type: true,
                        error: None,
                        creating: false,
                    },
                ));
                true
            }
            (
                PendingEndpointKind::PrepareWorktreeOpen { workspace_id },
                Ok(ResponseResult::WorktreeList { worktrees, .. }),
            ) => {
                let entries = worktrees
                    .into_iter()
                    .filter(|entry| !entry.is_bare && !entry.is_prunable)
                    .map(|entry| {
                        let label = entry.branch.clone().unwrap_or_else(|| entry.label.clone());
                        ClientWorktreeOpenEntry {
                            path: entry.path,
                            branch: entry.branch,
                            is_linked_worktree: entry.is_linked_worktree,
                            is_detached: entry.is_detached,
                            open_workspace_id: entry.open_workspace_id,
                            label,
                        }
                    })
                    .collect::<Vec<_>>();
                if entries.is_empty() {
                    self.endpoint_error = Some("No Git worktrees found for this repo.".to_owned());
                } else {
                    self.overlay = Some(ClientShellOverlay::WorktreeOpen(
                        ClientWorktreeOpenOverlay {
                            source_workspace_id: workspace_id,
                            entries,
                            selected: 0,
                            query: String::new(),
                            search_focused: false,
                            error: None,
                            opening: false,
                        },
                    ));
                }
                true
            }
            (
                PendingEndpointKind::PrepareWorktreeRemove { workspace_id },
                Ok(ResponseResult::WorktreeList { worktrees, .. }),
            ) => {
                let path = worktrees
                    .into_iter()
                    .find(|entry| entry.open_workspace_id.as_deref() == Some(&workspace_id))
                    .map(|entry| entry.path);
                if let Some(path) = path {
                    self.overlay = Some(ClientShellOverlay::WorktreeRemove(
                        ClientWorktreeRemoveOverlay {
                            workspace_id,
                            path,
                            error: None,
                            removing: false,
                            force_confirmation: false,
                        },
                    ));
                } else {
                    self.endpoint_error =
                        Some("This workspace is not a Herdr-managed worktree checkout.".to_owned());
                }
                true
            }
            (PendingEndpointKind::WorktreeCreate, Ok(ResponseResult::WorktreeCreated { .. }))
            | (PendingEndpointKind::WorktreeOpen, Ok(ResponseResult::WorktreeOpened { .. }))
            | (
                PendingEndpointKind::WorktreeRemove { .. },
                Ok(ResponseResult::WorktreeRemoved { .. }),
            ) => {
                self.overlay = None;
                true
            }
            (PendingEndpointKind::WorktreeCreate, Err(error)) => {
                if let Some(ClientShellOverlay::WorktreeCreate(create)) = self.overlay.as_mut() {
                    create.creating = false;
                    create.error = Some(error.message);
                }
                true
            }
            (PendingEndpointKind::WorktreeOpen, Err(error)) => {
                if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut() {
                    open.opening = false;
                    open.error = Some(error.message);
                }
                true
            }
            (PendingEndpointKind::WorktreeRemove { forced: false }, Err(error))
                if error.code.as_deref() == Some("dirty_worktree_requires_force") =>
            {
                if let Some(ClientShellOverlay::WorktreeRemove(remove)) = self.overlay.as_mut() {
                    remove.removing = false;
                    remove.force_confirmation = true;
                    remove.error = None;
                }
                true
            }
            (PendingEndpointKind::WorktreeRemove { .. }, Err(error)) => {
                if let Some(ClientShellOverlay::WorktreeRemove(remove)) = self.overlay.as_mut() {
                    remove.removing = false;
                    remove.error = Some(error.message);
                }
                true
            }
            (
                PendingEndpointKind::PrepareWorktreeCreate { .. }
                | PendingEndpointKind::PrepareWorktreeOpen { .. }
                | PendingEndpointKind::PrepareWorktreeRemove { .. },
                Err(_),
            ) => true,
            (_, Ok(_)) => {
                self.endpoint_error =
                    Some("endpoint returned an unexpected worktree result".to_owned());
                true
            }
            (
                PendingEndpointKind::Generic
                | PendingEndpointKind::ProductAnnouncementDismiss { .. }
                | PendingEndpointKind::ReleaseNotesDismiss
                | PendingEndpointKind::PopupCommand
                | PendingEndpointKind::ReloadConfig
                | PendingEndpointKind::IntegrationList
                | PendingEndpointKind::IntegrationInstall
                | PendingEndpointKind::SelectionCopy { .. }
                | PendingEndpointKind::PaneScroll { .. }
                | PendingEndpointKind::WordSelection { .. }
                | PendingEndpointKind::PaneLinkActivate { .. }
                | PendingEndpointKind::CopyMotion { .. }
                | PendingEndpointKind::CopySearch { .. },
                Err(_),
            ) => true,
        }
    }
}
