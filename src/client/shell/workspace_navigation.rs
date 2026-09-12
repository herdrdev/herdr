use super::*;

/// A client-only preview. Snapshot identity prevents Enter from using a reused workspace ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkspaceNavigationTarget {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) workspace_id: String,
    boot_id: String,
    generation: Option<u64>,
}

impl WorkspaceNavigationTarget {
    pub(super) fn matches(&self, endpoint_id: &ClientEndpointId, workspace_id: &str) -> bool {
        &self.endpoint_id == endpoint_id && self.workspace_id == workspace_id
    }
}

impl ClientShellState {
    pub(super) fn navigation_target(
        &self,
        endpoint_id: &ClientEndpointId,
        workspace_id: &str,
    ) -> Option<WorkspaceNavigationTarget> {
        let endpoint = self
            .endpoints
            .iter()
            .find(|entry| &entry.endpoint_id == endpoint_id)?;
        let snapshot = endpoint.snapshot.as_deref()?;
        Some(WorkspaceNavigationTarget {
            endpoint_id: endpoint_id.clone(),
            workspace_id: workspace_id.to_owned(),
            boot_id: snapshot.boot_id.clone(),
            generation: endpoint.snapshot_generation,
        })
    }

    pub(super) fn focused_navigation_target(&self) -> Option<WorkspaceNavigationTarget> {
        let workspace_id = self.snapshot.as_deref()?.focused_workspace_id.as_deref()?;
        self.navigation_target(&self.active_endpoint_id, workspace_id)
    }

    pub(super) fn navigation_target_valid(&self, target: &WorkspaceNavigationTarget) -> bool {
        self.endpoints.iter().any(|endpoint| {
            endpoint.endpoint_id == target.endpoint_id
                && endpoint.status == ClientEndpointStatus::Online
                && endpoint.snapshot_generation == target.generation
                && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                    snapshot.boot_id == target.boot_id
                        && snapshot
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.workspace_id == target.workspace_id)
                })
        })
    }

    pub(super) fn workspace_preview_action_blocked(&self) -> bool {
        self.navigate_workspace_id.as_ref().is_some_and(|target| {
            target.endpoint_id != self.active_endpoint_id || !self.navigation_target_valid(target)
        })
    }

    pub(super) fn move_navigate_workspace(&mut self, delta: isize) {
        let mobile = self.mobile_layout_active();
        let surface_available = self.snapshot.is_some() && self.pane_surface.is_some();
        let empty_collapsed_groups = HashSet::new();
        let mut targets = Vec::new();
        for endpoint in &self.endpoints {
            if endpoint.status != ClientEndpointStatus::Online {
                continue;
            }
            let Some(snapshot) = endpoint.snapshot.as_deref() else {
                continue;
            };
            let entries = if self.sidebar_collapsed && !mobile && surface_available {
                snapshot
                    .workspaces
                    .iter()
                    .enumerate()
                    .map(|(index, _)| WorkspaceEntry {
                        index,
                        indented: false,
                        last_child: false,
                    })
                    .collect()
            } else {
                let collapsed_groups = if mobile && surface_available {
                    &empty_collapsed_groups
                } else {
                    self.collapsed_groups_for_endpoint(&endpoint.endpoint_id)
                        .unwrap_or(&empty_collapsed_groups)
                };
                render::workspace_entries(snapshot, collapsed_groups)
            };
            for entry in entries {
                targets.push(WorkspaceNavigationTarget {
                    endpoint_id: endpoint.endpoint_id.clone(),
                    workspace_id: snapshot.workspaces[entry.index].workspace_id.clone(),
                    boot_id: snapshot.boot_id.clone(),
                    generation: endpoint.snapshot_generation,
                });
            }
        }
        if targets.is_empty() {
            return;
        }
        let current = self
            .navigate_workspace_id
            .as_ref()
            .and_then(|selected| targets.iter().position(|target| target == selected));
        let next = match current {
            Some(current) if mobile => {
                (current as isize + delta).clamp(0, targets.len() as isize - 1) as usize
            }
            Some(current) => (current as isize + delta).rem_euclid(targets.len() as isize) as usize,
            None if delta < 0 => targets.len() - 1,
            None => 0,
        };
        let target = targets.swap_remove(next);
        self.collapsed_endpoints.remove(&target.endpoint_id);
        if self.endpoints.len() == 1 && !mobile {
            self.reveal_workspace(&target.workspace_id);
        }
        self.navigate_workspace_id = Some(target);
        self.reveal_mobile_workspace = mobile;
        self.reveal_navigation_workspace =
            !mobile || self.snapshot.is_none() || self.pane_surface.is_none();
    }

    pub(super) fn accept_navigate_workspace(&mut self, outcome: &mut ClientShellInput) {
        let Some(target) = self.navigate_workspace_id.clone() else {
            self.mode = self.copy_or_terminal_mode();
            outcome.repaint = true;
            return;
        };
        if !self.navigation_target_valid(&target) {
            self.receive_endpoint_unavailable(
                "Workspace is no longer available; select a connected workspace".into(),
            );
            outcome.repaint = true;
            return;
        }
        if self.focus_or_activate(
            target.endpoint_id,
            ClientEndpointFocusTarget::Workspace(target.workspace_id),
            outcome,
        ) {
            self.mode = ClientShellMode::Terminal;
            self.navigate_workspace_id = None;
        }
        outcome.repaint = true;
    }
}
