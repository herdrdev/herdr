use super::*;

impl ClientShellState {
    pub(super) fn active_endpoint_workspace_at(&self, point: (u16, u16)) -> Option<String> {
        self.hits
            .workspaces
            .iter()
            .find(|hit| {
                hit.endpoint_id == self.active_endpoint_id && super::contains(hit.rect, point)
            })
            .map(|hit| hit.workspace_id.clone())
    }

    pub(super) fn endpoint_workspace_is_draggable(&self, press: &ClientWorkspacePress) -> bool {
        press.endpoint_id == self.active_endpoint_id
            && self
                .snapshot
                .as_deref()
                .and_then(|snapshot| {
                    snapshot
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.workspace_id == press.workspace_id)
                })
                .is_some_and(|workspace| {
                    !workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree)
                })
    }

    pub(super) fn finish_endpoint_workspace_press(
        &mut self,
        press: ClientWorkspacePress,
        outcome: &mut ClientShellInput,
    ) {
        self.focus_or_activate(
            press.endpoint_id,
            ClientEndpointFocusTarget::Workspace(press.workspace_id),
            outcome,
        );
    }

    pub(super) fn handle_endpoint_machine_click(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(hit) = self
            .hits
            .machines
            .iter()
            .find(|hit| super::contains(hit.rect, point))
        else {
            return false;
        };
        let endpoint_id = hit.endpoint_id.clone();
        let collapse_toggle = super::contains(hit.collapse_toggle, point);
        if collapse_toggle || endpoint_id == self.active_endpoint_id {
            if !self.collapsed_endpoints.remove(&endpoint_id) {
                self.collapsed_endpoints.insert(endpoint_id.clone());
            }
            outcome.repaint = true;
            if !collapse_toggle && endpoint_id.is_local() {
                self.activate_endpoint(endpoint_id, outcome);
            }
        } else if endpoint_id.is_local() || self.endpoint_is_online(&endpoint_id) {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id,
                target: None,
            });
        } else {
            let label = self.endpoint_label(&endpoint_id).to_owned();
            self.receive_endpoint_unavailable(format!("{label} is not ready"));
            outcome.repaint = true;
        }
        true
    }

    pub(super) fn handle_endpoint_agent_click(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some((endpoint_id, pane_id)) = self
            .hits
            .endpoint_agents
            .iter()
            .find(|(rect, _, _)| super::contains(*rect, point))
            .map(|(_, endpoint_id, pane_id)| (endpoint_id.clone(), pane_id.clone()))
        else {
            return false;
        };
        self.focus_or_activate(
            endpoint_id,
            ClientEndpointFocusTarget::Pane(pane_id),
            outcome,
        );
        true
    }

    pub(super) fn handle_endpoint_navigation(
        &mut self,
        action: crate::input::KeybindAction,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crate::input::KeybindAction;
        if !self.multi_endpoint_active() {
            return false;
        }
        if matches!(
            action,
            KeybindAction::PreviousWorkspace | KeybindAction::NextWorkspace
        ) {
            let workspaces = self
                .endpoints
                .iter()
                .filter(|endpoint| endpoint.status == ClientEndpointStatus::Online)
                .flat_map(|endpoint| {
                    endpoint
                        .snapshot
                        .as_deref()
                        .map_or_else(Vec::new, |snapshot| {
                            render::workspace_entries(snapshot, &HashSet::new())
                                .into_iter()
                                .filter_map(|entry| {
                                    snapshot.workspaces.get(entry.index).map(|workspace| {
                                        (
                                            endpoint.endpoint_id.clone(),
                                            workspace.workspace_id.clone(),
                                        )
                                    })
                                })
                                .collect()
                        })
                })
                .collect::<Vec<_>>();
            if workspaces.is_empty() {
                return true;
            }
            let focused = self
                .snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.focused_workspace_id.as_deref());
            let current = workspaces.iter().position(|(endpoint_id, workspace_id)| {
                endpoint_id == &self.active_endpoint_id && Some(workspace_id.as_str()) == focused
            });
            let next = match (current, action) {
                (Some(index), KeybindAction::PreviousWorkspace) => {
                    (index + workspaces.len() - 1) % workspaces.len()
                }
                (Some(index), KeybindAction::NextWorkspace) => (index + 1) % workspaces.len(),
                (None, KeybindAction::PreviousWorkspace) => workspaces.len() - 1,
                (None, KeybindAction::NextWorkspace) => 0,
                _ => unreachable!("endpoint workspace navigation"),
            };
            let (endpoint_id, workspace_id) = workspaces[next].clone();
            self.focus_or_activate(
                endpoint_id,
                ClientEndpointFocusTarget::Workspace(workspace_id),
                outcome,
            );
            return true;
        }
        if matches!(
            action,
            KeybindAction::PreviousAgent | KeybindAction::NextAgent | KeybindAction::FocusAgent(_)
        ) {
            let agents = super::aggregate_navigation::online_agent_targets(
                &self.endpoints,
                &self.active_endpoint_id,
                self.config.agent_panel_sort,
            );
            if agents.is_empty() {
                return true;
            }
            let next = match action {
                KeybindAction::FocusAgent(index) => {
                    if index >= agents.len() {
                        return true;
                    }
                    index
                }
                KeybindAction::PreviousAgent | KeybindAction::NextAgent => {
                    let focused = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| snapshot.focused_pane_id.as_deref());
                    let current = agents.iter().position(|target| {
                        target.endpoint_id == self.active_endpoint_id
                            && Some(target.pane_id.as_str()) == focused
                    });
                    match (current, action) {
                        (Some(index), KeybindAction::PreviousAgent) => {
                            (index + agents.len() - 1) % agents.len()
                        }
                        (Some(index), KeybindAction::NextAgent) => (index + 1) % agents.len(),
                        (None, KeybindAction::PreviousAgent) => agents.len() - 1,
                        _ => 0,
                    }
                }
                _ => unreachable!("endpoint agent navigation"),
            };
            let target = &agents[next];
            if self.focus_or_activate(
                target.endpoint_id.clone(),
                ClientEndpointFocusTarget::Pane(target.pane_id.clone()),
                outcome,
            ) {
                if target.endpoint_id == self.active_endpoint_id {
                    self.reveal_endpoint_agent(
                        &target.endpoint_id,
                        &target.pane_id,
                        self.hits.agent_body.height,
                    );
                } else {
                    self.pending_agent_reveal =
                        Some((target.endpoint_id.clone(), target.pane_id.clone()));
                }
                outcome.repaint = true;
            }
            return true;
        }
        false
    }

    pub(super) fn activate_endpoint(
        &mut self,
        endpoint_id: ClientEndpointId,
        outcome: &mut ClientShellInput,
    ) -> bool {
        self.pending_agent_reveal = None;
        let online = self.endpoint_is_online(&endpoint_id);
        if !online && !endpoint_id.is_local() {
            let label = self.endpoint_label(&endpoint_id).to_owned();
            self.receive_endpoint_unavailable(format!("{label} is not ready"));
            outcome.repaint = true;
            return false;
        }
        if (endpoint_id.is_local() && (self.multi_endpoint_active() || !online))
            || endpoint_id != self.active_endpoint_id
        {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id,
                target: None,
            });
        }
        true
    }

    pub(super) fn focus_or_activate(
        &mut self,
        endpoint_id: ClientEndpointId,
        target: ClientEndpointFocusTarget,
        outcome: &mut ClientShellInput,
    ) -> bool {
        self.pending_agent_reveal = None;
        let online = self.endpoint_is_online(&endpoint_id);
        if !online && !endpoint_id.is_local() {
            let label = self.endpoint_label(&endpoint_id).to_owned();
            self.receive_endpoint_unavailable(format!("{label} is not ready"));
            outcome.repaint = true;
            return false;
        }
        // Local can still be displayed while a remote activation is pending.
        // Route explicit selections through the runtime so they can cancel that handoff.
        if endpoint_id == self.active_endpoint_id
            && !(endpoint_id.is_local() && (self.multi_endpoint_active() || !online))
        {
            let method = match target {
                ClientEndpointFocusTarget::Workspace(workspace_id) => {
                    crate::api::schema::Method::WorkspaceFocus(
                        crate::api::schema::WorkspaceTarget { workspace_id },
                    )
                }
                ClientEndpointFocusTarget::Tab(tab_id) => {
                    crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget { tab_id })
                }
                ClientEndpointFocusTarget::Pane(pane_id) => {
                    crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                        pane_id,
                    })
                }
            };
            self.push_endpoint_method(method, outcome);
        } else {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id,
                target: Some(target),
            });
        }
        true
    }
}
