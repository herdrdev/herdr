use super::*;
use crate::server::client_shell::resize_popup_runtime;
use crate::server::clients::ClientShellTopology;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShellFocusTarget {
    pub(super) tab_id: String,
    pub(super) workspace_index: usize,
    pub(super) pane_id: crate::layout::PaneId,
}

fn classify_shell_focus_transition<'a>(
    before: Option<&'a ShellFocusTarget>,
    after: Option<&'a ShellFocusTarget>,
    focused_tabs_before: &HashSet<String>,
    focused_tabs_after: &HashSet<String>,
) -> (Option<&'a ShellFocusTarget>, Option<&'a ShellFocusTarget>) {
    if before == after {
        return (None, None);
    }
    if before.map(|target| target.tab_id.as_str()) == after.map(|target| target.tab_id.as_str()) {
        return match (before, after) {
            (Some(before), Some(after)) if focused_tabs_after.contains(&after.tab_id) => {
                (Some(before), Some(after))
            }
            _ => (None, None),
        };
    }
    let lost = before.filter(|target| {
        focused_tabs_before.contains(&target.tab_id) && !focused_tabs_after.contains(&target.tab_id)
    });
    let gained = after.filter(|target| {
        !focused_tabs_before.contains(&target.tab_id) && focused_tabs_after.contains(&target.tab_id)
    });
    (lost, gained)
}

pub(super) fn forward_proxied_api_response(
    proxy: Option<(
        std::sync::mpsc::Sender<String>,
        std::sync::mpsc::Receiver<String>,
    )>,
) -> bool {
    let Some((respond_to, response_rx)) = proxy else {
        return false;
    };
    let Ok(response) = response_rx.recv() else {
        return false;
    };
    let succeeded = serde_json::from_str::<api::schema::SuccessResponse>(&response).is_ok();
    let _ = respond_to.send(response);
    succeeded
}

impl HeadlessServer {
    pub(super) fn default_shell_target(&self) -> Option<crate::ui::TabSurfaceTarget> {
        let workspace_index = self.app.state.active?;
        let workspace = self.app.state.workspaces.get(workspace_index)?;
        Some(crate::ui::TabSurfaceTarget {
            workspace_index,
            tab_index: workspace.active_tab_index(),
        })
    }

    pub(super) fn shell_target_for_client(
        &self,
        client_id: u64,
    ) -> Option<crate::ui::TabSurfaceTarget> {
        let tab_id = self
            .clients
            .get(&client_id)?
            .shell_location
            .as_ref()
            .and_then(crate::server::clients::ClientShellLocation::focused_tab_id);
        tab_id
            .and_then(|tab_id| self.app.parse_tab_id(tab_id))
            .map(|(workspace_index, tab_index)| crate::ui::TabSurfaceTarget {
                workspace_index,
                tab_index,
            })
            .or_else(|| self.default_shell_target())
    }

    fn tab_id_for_target(&self, target: crate::ui::TabSurfaceTarget) -> Option<String> {
        self.app
            .public_tab_id(target.workspace_index, target.tab_index)
    }

    pub(super) fn shell_tab_id_for_client(&self, client_id: u64) -> Option<String> {
        self.shell_target_for_client(client_id)
            .and_then(|target| self.tab_id_for_target(target))
    }

    fn client_shell_topology(&self) -> ClientShellTopology {
        let focused_workspace_id = self
            .app
            .state
            .active
            .map(|workspace_index| self.app.public_workspace_id(workspace_index));
        let fallback_workspace_id = self
            .app
            .state
            .workspaces
            .first()
            .map(|_| self.app.public_workspace_id(0));
        let mut active_tab_ids = HashMap::new();
        let mut tab_workspace_ids = HashMap::new();
        for (workspace_index, workspace) in self.app.state.workspaces.iter().enumerate() {
            let workspace_id = self.app.public_workspace_id(workspace_index);
            if let Some(tab_id) = self
                .app
                .public_tab_id(workspace_index, workspace.active_tab_index())
            {
                active_tab_ids.insert(workspace_id.clone(), tab_id);
            }
            for tab_index in 0..workspace.tabs.len() {
                if let Some(tab_id) = self.app.public_tab_id(workspace_index, tab_index) {
                    tab_workspace_ids.insert(tab_id, workspace_id.clone());
                }
            }
        }
        ClientShellTopology {
            focused_workspace_id,
            fallback_workspace_id,
            active_tab_ids,
            tab_workspace_ids,
        }
    }

    pub(super) fn reconcile_client_shell_locations(&mut self) {
        if self.app.state.popup_pane.is_none() {
            self.popup_owner_tab_id = None;
        } else if self
            .popup_owner_tab_id
            .as_deref()
            .is_some_and(|tab_id| self.app.parse_tab_id(tab_id).is_none())
        {
            self.popup_owner_tab_id = None;
            self.app.close_popup_pane();
        } else if self.popup_owner_tab_id.is_none() {
            self.popup_owner_tab_id = self
                .default_shell_target()
                .and_then(|target| self.tab_id_for_target(target));
        }
        let topology = self.client_shell_topology();
        let live_clients = self.clients.keys().copied().collect::<HashSet<_>>();
        self.tab_geometry_controllers.retain(|tab_id, client_id| {
            topology.tab_workspace_ids.contains_key(tab_id) && live_clients.contains(client_id)
        });
        for client in self
            .clients
            .values_mut()
            .filter(|client| client.is_shell_client())
        {
            let location = client.shell_location.get_or_insert_with(|| {
                crate::server::clients::ClientShellLocation {
                    focused_workspace_id: topology.focused_workspace_id.clone(),
                    active_tab_ids: topology.active_tab_ids.clone(),
                }
            });
            location.reconcile(&topology);
        }
    }

    pub(super) fn focus_all_shell_clients_on_default_target(&mut self) {
        let Some(target) = self.default_shell_target() else {
            return;
        };
        let workspace_id = self.app.public_workspace_id(target.workspace_index);
        let Some(tab_id) = self.tab_id_for_target(target) else {
            return;
        };
        let focus_before = self.shell_focus_targets();
        let focused_tabs_before = self.focused_shell_tabs();
        for client in self
            .clients
            .values_mut()
            .filter(|client| client.is_shell_client())
        {
            if let Some(location) = client.shell_location.as_mut() {
                location.focus_tab(workspace_id.clone(), tab_id.clone());
            }
        }
        let (lost, gained) =
            self.shell_location_focus_transitions(focus_before, &focused_tabs_before);
        self.app.accept_current_focus_with_api_events();
        self.send_shell_focus_transitions(&lost, &gained);
    }

    pub(super) fn focus_shell_client_on_tab(&mut self, client_id: u64, tab_id: &str) -> bool {
        let Some((workspace_index, _)) = self.app.parse_tab_id(tab_id) else {
            return false;
        };
        let workspace_id = self.app.public_workspace_id(workspace_index);
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let Some(location) = client.shell_location.as_mut() else {
            return false;
        };
        location.focus_tab(workspace_id, tab_id.to_owned());
        true
    }

    fn set_default_shell_target_from_client(&mut self, client_id: u64) -> bool {
        let Some(target) = self.shell_target_for_client(client_id) else {
            return false;
        };
        if self.default_shell_target() == Some(target) {
            return false;
        }
        self.app
            .state
            .switch_workspace_tab(target.workspace_index, target.tab_index)
    }

    fn focus_shell_client_on_default_target(&mut self, client_id: u64) -> bool {
        let Some(tab_id) = self
            .default_shell_target()
            .and_then(|target| self.tab_id_for_target(target))
        else {
            return false;
        };
        self.focus_shell_client_on_tab(client_id, &tab_id)
    }

    fn shell_locations_may_need_reconcile(method: &api::schema::Method) -> bool {
        use api::schema::Method;

        matches!(
            method,
            Method::CommandInvoke(_)
                | Method::PaneClose(_)
                | Method::PaneEditScrollback(_)
                | Method::PaneSplit(_)
                | Method::TabClose(_)
                | Method::TabCreate(_)
                | Method::WorkspaceClose(_)
                | Method::WorkspaceCreate(_)
                | Method::WorktreeCreate(_)
                | Method::WorktreeOpen(_)
                | Method::WorktreeRemove(_)
        )
    }

    fn shell_endpoint_claims_geometry(method: &api::schema::Method) -> bool {
        use api::schema::Method;

        matches!(
            method,
            Method::CommandInvoke(_)
                | Method::LayoutSetSplitRatio(_)
                | Method::PaneClose(_)
                | Method::PaneCopyMotion(_)
                | Method::PaneCopySearch(_)
                | Method::PaneEditScrollback(_)
                | Method::PaneFocus(_)
                | Method::PaneFocusDirection(_)
                | Method::PaneInputSet(_)
                | Method::PaneLinkActivate(_)
                | Method::PaneRename(_)
                | Method::PaneResize(_)
                | Method::PaneScroll(_)
                | Method::PaneSplit(_)
                | Method::PaneSwap(_)
                | Method::PaneZoom(_)
                | Method::TabClose(_)
                | Method::TabCreate(_)
                | Method::TabFocus(_)
                | Method::TabMove(_)
                | Method::TabRename(_)
                | Method::WorkspaceClose(_)
                | Method::WorkspaceCreate(_)
                | Method::WorkspaceFocus(_)
                | Method::WorkspaceMove(_)
                | Method::WorkspaceMoveBlock(_)
                | Method::WorkspaceRename(_)
                | Method::WorktreeCreate(_)
                | Method::WorktreeOpen(_)
                | Method::WorktreeRemove(_)
        )
    }

    fn public_request_may_change_geometry(method: &api::schema::Method) -> bool {
        use api::schema::Method;

        matches!(
            method,
            Method::CommandInvoke(_)
                | Method::LayoutSetSplitRatio(_)
                | Method::PaneClose(_)
                | Method::PaneEditScrollback(_)
                | Method::PaneFocus(_)
                | Method::PaneFocusDirection(_)
                | Method::PaneResize(_)
                | Method::PaneSplit(_)
                | Method::PaneSwap(_)
                | Method::PaneZoom(_)
                | Method::TabClose(_)
                | Method::TabCreate(_)
                | Method::TabFocus(_)
                | Method::WorkspaceClose(_)
                | Method::WorkspaceCreate(_)
                | Method::WorkspaceFocus(_)
                | Method::WorktreeCreate(_)
                | Method::WorktreeOpen(_)
                | Method::WorktreeRemove(_)
        )
    }

    pub(super) fn deferred_endpoint_navigation_tab_id(response: &[u8]) -> Option<String> {
        let response = serde_json::from_slice::<serde_json::Value>(response).ok()?;
        if response.pointer("/result/type")?.as_str()? != "worktree_created" {
            return None;
        }
        response
            .pointer("/result/tab/tab_id")?
            .as_str()
            .map(str::to_owned)
    }

    fn apply_shell_navigation_request(
        &mut self,
        client_id: u64,
        method: &api::schema::Method,
    ) -> bool {
        match method {
            api::schema::Method::WorkspaceFocus(target) => {
                let Some(workspace_index) = self.app.parse_workspace_id(&target.workspace_id)
                else {
                    return false;
                };
                let workspace_id = self.app.public_workspace_id(workspace_index);
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                let Some(location) = client.shell_location.as_mut() else {
                    return false;
                };
                location.focus_workspace(workspace_id);
                true
            }
            api::schema::Method::TabFocus(target) => {
                self.focus_shell_client_on_tab(client_id, &target.tab_id)
            }
            api::schema::Method::PaneFocus(target) => self
                .app
                .parse_pane_id(&target.pane_id)
                .and_then(|(workspace_index, pane_id)| {
                    let tab_index = self.app.state.workspaces[workspace_index]
                        .find_tab_index_for_pane(pane_id)?;
                    self.app.public_tab_id(workspace_index, tab_index)
                })
                .is_some_and(|tab_id| self.focus_shell_client_on_tab(client_id, &tab_id)),
            api::schema::Method::CommandInvoke(params) => params
                .tab_id
                .as_deref()
                .is_some_and(|tab_id| self.focus_shell_client_on_tab(client_id, tab_id)),
            _ => false,
        }
    }

    fn focus_target_for_surface(
        &self,
        target: crate::ui::TabSurfaceTarget,
    ) -> Option<ShellFocusTarget> {
        let pane_id = self
            .app
            .state
            .workspaces
            .get(target.workspace_index)?
            .tabs
            .get(target.tab_index)?
            .layout
            .focused();
        Some(ShellFocusTarget {
            tab_id: self.tab_id_for_target(target)?,
            workspace_index: target.workspace_index,
            pane_id,
        })
    }

    pub(super) fn shell_focus_target(&self, client_id: u64) -> Option<ShellFocusTarget> {
        self.focus_target_for_surface(self.shell_target_for_client(client_id)?)
    }

    pub(super) fn focused_shell_tabs(&self) -> HashSet<String> {
        self.clients
            .iter()
            .filter(|(_, client)| {
                client.is_shell_client() && client.outer_terminal_focus == Some(true)
            })
            .filter_map(|(&client_id, _)| self.shell_tab_id_for_client(client_id))
            .collect()
    }

    pub(super) fn shell_focus_targets(&self) -> Vec<(u64, Option<ShellFocusTarget>)> {
        self.clients
            .iter()
            .filter(|(_, client)| client.is_shell_client())
            .map(|(&client_id, _)| (client_id, self.shell_focus_target(client_id)))
            .collect()
    }

    pub(super) fn send_shell_focus_target(
        &self,
        target: &ShellFocusTarget,
        event: crate::ghostty::FocusEvent,
    ) {
        self.app
            .send_pane_focus_event(target.workspace_index, target.pane_id, event);
    }

    fn shell_location_focus_transitions(
        &self,
        focus_before: Vec<(u64, Option<ShellFocusTarget>)>,
        focused_tabs_before: &HashSet<String>,
    ) -> (
        HashMap<String, ShellFocusTarget>,
        HashMap<String, ShellFocusTarget>,
    ) {
        let focused_tabs_after = self.focused_shell_tabs();
        let mut lost = HashMap::<String, ShellFocusTarget>::new();
        let mut gained = HashMap::<String, ShellFocusTarget>::new();
        for (client_id, before) in focus_before {
            let after = self.shell_focus_target(client_id);
            let (lost_target, gained_target) = classify_shell_focus_transition(
                before.as_ref(),
                after.as_ref(),
                focused_tabs_before,
                &focused_tabs_after,
            );
            if let Some(target) = lost_target {
                lost.entry(target.tab_id.clone())
                    .or_insert_with(|| target.clone());
            }
            if let Some(target) = gained_target {
                gained
                    .entry(target.tab_id.clone())
                    .or_insert_with(|| target.clone());
            }
        }
        (lost, gained)
    }

    fn send_shell_focus_transitions(
        &self,
        lost: &HashMap<String, ShellFocusTarget>,
        gained: &HashMap<String, ShellFocusTarget>,
    ) {
        for target in lost.values() {
            self.send_shell_focus_target(target, crate::ghostty::FocusEvent::Lost);
        }
        for target in gained.values() {
            self.send_shell_focus_target(target, crate::ghostty::FocusEvent::Gained);
        }
    }

    pub(super) fn finish_shell_location_reconciliation(
        &mut self,
        focus_before: Vec<(u64, Option<ShellFocusTarget>)>,
        focused_tabs_before: &HashSet<String>,
    ) {
        let (lost, gained) =
            self.shell_location_focus_transitions(focus_before, focused_tabs_before);
        self.app.accept_current_focus_without_events();
        self.send_shell_focus_transitions(&lost, &gained);
    }

    pub(super) fn send_shell_navigation_focus_events(
        &self,
        before: Option<&ShellFocusTarget>,
        after: Option<&ShellFocusTarget>,
        focused_tabs_before: &HashSet<String>,
        focused_tabs_after: &HashSet<String>,
    ) {
        let (lost, gained) =
            classify_shell_focus_transition(before, after, focused_tabs_before, focused_tabs_after);
        if let Some(target) = lost {
            self.send_shell_focus_target(target, crate::ghostty::FocusEvent::Lost);
        }
        if let Some(target) = gained {
            self.send_shell_focus_target(target, crate::ghostty::FocusEvent::Gained);
        }
    }

    pub(super) fn shell_client_views_pane(
        &self,
        client_id: u64,
        workspace_index: usize,
        pane_id: crate::layout::PaneId,
    ) -> bool {
        let Some(target) = self.shell_target_for_client(client_id) else {
            return false;
        };
        if target.workspace_index != workspace_index {
            return false;
        }
        let Some(tab) = self
            .app
            .state
            .workspaces
            .get(workspace_index)
            .and_then(|workspace| workspace.tabs.get(target.tab_index))
        else {
            return false;
        };
        if tab.zoomed {
            tab.layout.focused() == pane_id
        } else {
            tab.layout.pane_ids().contains(&pane_id)
        }
    }

    fn finish_shell_tab_geometry_change(&mut self, start_pending_agent_resumes: bool) {
        for client in self.clients.values_mut() {
            client.request_repaint();
        }
        if !start_pending_agent_resumes {
            self.app.pending_agent_resume_deadline = None;
            return;
        }
        let now = Instant::now();
        self.app.sync_pending_agent_resume_deadline(now);
        if self
            .app
            .start_pending_agent_resumes(self.app.pending_agent_resume_due(now))
        {
            for client in self.clients.values_mut() {
                client.request_repaint();
            }
        }
    }

    pub(super) fn apply_shell_tab_geometry(
        &mut self,
        client_id: u64,
        start_pending_agent_resumes: bool,
    ) -> bool {
        let Some(target) = self.shell_target_for_client(client_id) else {
            return false;
        };
        self.apply_shell_tab_geometry_to_target(client_id, target, start_pending_agent_resumes)
    }

    fn apply_shell_tab_geometry_to_target(
        &mut self,
        client_id: u64,
        target: crate::ui::TabSurfaceTarget,
        start_pending_agent_resumes: bool,
    ) -> bool {
        if !self.resize_shell_tab_geometry_to_target(client_id, target) {
            return false;
        }
        self.finish_shell_tab_geometry_change(start_pending_agent_resumes);
        true
    }

    fn resize_shell_tab_geometry_to_target(
        &mut self,
        client_id: u64,
        target: crate::ui::TabSurfaceTarget,
    ) -> bool {
        let Some(client) = self.clients.get(&client_id) else {
            return false;
        };
        let (cols, rows) = client.terminal_size;
        let cell_size = if client.cell_size.is_known() {
            client.cell_size
        } else {
            crate::kitty_graphics::HostCellSize::default()
        };
        let area = Rect::new(0, 0, cols, rows);
        if self.app_client_count() == 1 {
            for (workspace_index, workspace) in self.app.state.workspaces.iter().enumerate() {
                for tab_index in 0..workspace.tabs.len() {
                    crate::ui::resize_tab_surface(
                        &self.app.state,
                        &self.app.terminal_runtimes,
                        workspace_index,
                        tab_index,
                        area,
                        cell_size,
                    );
                }
            }
        } else {
            crate::ui::compute_tab_surface_for(
                &self.app.state,
                &self.app.terminal_runtimes,
                Some(target),
                area,
                true,
                cell_size,
            );
        }
        if self
            .popup_owner_tab_id
            .as_deref()
            .is_some_and(|owner| self.tab_id_for_target(target).as_deref() == Some(owner))
        {
            let _ = resize_popup_runtime(&self.app, Rect::new(0, 0, cols, rows), cell_size);
        }
        true
    }

    pub(super) fn resize_tabs_for_only_shell_client(
        &mut self,
        start_pending_agent_resumes: bool,
    ) -> bool {
        if self.app_client_count() != 1 {
            return false;
        }
        let Some(client_id) = self.clients.iter().find_map(|(&client_id, client)| {
            (client.is_shell_client() && client.writer.is_some()).then_some(client_id)
        }) else {
            return false;
        };
        self.apply_shell_tab_geometry(client_id, start_pending_agent_resumes)
    }

    pub(super) fn reapply_controlled_shell_tab_geometry(
        &mut self,
        start_pending_agent_resumes: bool,
    ) -> bool {
        let mut viewed_tabs = HashMap::<String, Vec<u64>>::new();
        for (&client_id, client) in &self.clients {
            if !client.is_shell_client() || client.writer.is_none() {
                continue;
            }
            let Some(tab_id) = self.shell_tab_id_for_client(client_id) else {
                continue;
            };
            viewed_tabs.entry(tab_id).or_default().push(client_id);
        }
        for viewers in viewed_tabs.values_mut() {
            viewers.sort_unstable();
        }
        for (tab_id, viewers) in viewed_tabs {
            let controller_is_viewing = self
                .tab_geometry_controllers
                .get(&tab_id)
                .is_some_and(|controller| viewers.contains(controller));
            if !controller_is_viewing {
                self.tab_geometry_controllers.insert(tab_id, viewers[0]);
            }
        }

        if self.resize_tabs_for_only_shell_client(start_pending_agent_resumes) {
            return true;
        }

        let mut controlled_tabs = self
            .tab_geometry_controllers
            .iter()
            .filter_map(|(tab_id, &client_id)| {
                self.app
                    .parse_tab_id(tab_id)
                    .map(|(workspace_index, tab_index)| {
                        (
                            tab_id.clone(),
                            client_id,
                            crate::ui::TabSurfaceTarget {
                                workspace_index,
                                tab_index,
                            },
                        )
                    })
            })
            .collect::<Vec<_>>();
        controlled_tabs.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let mut reapplied = false;
        for (_, client_id, target) in controlled_tabs {
            reapplied |= self.resize_shell_tab_geometry_to_target(client_id, target);
        }
        if reapplied {
            self.finish_shell_tab_geometry_change(start_pending_agent_resumes);
        }
        reapplied
    }

    pub(super) fn claim_shell_tab_geometry(
        &mut self,
        client_id: u64,
        start_pending_agent_resumes: bool,
    ) -> bool {
        let Some(tab_id) = self.shell_tab_id_for_client(client_id) else {
            return false;
        };
        if self.tab_geometry_controllers.insert(tab_id, client_id) == Some(client_id) {
            return false;
        }
        self.apply_shell_tab_geometry(client_id, start_pending_agent_resumes)
    }

    pub(super) fn claim_unowned_shell_tab_geometry(
        &mut self,
        client_id: u64,
        start_pending_agent_resumes: bool,
    ) -> bool {
        let Some(tab_id) = self.shell_tab_id_for_client(client_id) else {
            return false;
        };
        if self.tab_geometry_controllers.contains_key(&tab_id) {
            return false;
        }
        self.tab_geometry_controllers.insert(tab_id, client_id);
        self.apply_shell_tab_geometry(client_id, start_pending_agent_resumes)
    }

    pub(super) fn resize_shell_tab_if_controller(
        &mut self,
        client_id: u64,
        start_pending_agent_resumes: bool,
    ) -> bool {
        let Some(tab_id) = self.shell_tab_id_for_client(client_id) else {
            return false;
        };
        if self.tab_geometry_controllers.get(&tab_id) != Some(&client_id) {
            return false;
        }
        self.apply_shell_tab_geometry(client_id, start_pending_agent_resumes)
    }

    pub(super) fn shell_geometry_controller_for_terminal(
        &self,
        terminal_id: &str,
    ) -> Option<(u64, crate::ui::TabSurfaceTarget)> {
        let target = if self
            .app
            .state
            .popup_pane
            .as_ref()
            .is_some_and(|popup| popup.terminal_id.as_str() == terminal_id)
        {
            self.popup_owner_tab_id
                .as_deref()
                .and_then(|tab_id| self.app.parse_tab_id(tab_id))
                .map(|(workspace_index, tab_index)| crate::ui::TabSurfaceTarget {
                    workspace_index,
                    tab_index,
                })?
        } else {
            self.app.state.workspaces.iter().enumerate().find_map(
                |(workspace_index, workspace)| {
                    workspace
                        .tabs
                        .iter()
                        .enumerate()
                        .find_map(|(tab_index, tab)| {
                            tab.panes
                                .values()
                                .any(|pane| pane.attached_terminal_id.as_str() == terminal_id)
                                .then_some(crate::ui::TabSurfaceTarget {
                                    workspace_index,
                                    tab_index,
                                })
                        })
                },
            )?
        };
        let tab_id = self.tab_id_for_target(target)?;
        self.tab_geometry_controllers
            .get(&tab_id)
            .copied()
            .map(|client_id| (client_id, target))
    }

    pub(super) fn restore_shell_tab_geometry(
        &mut self,
        client_id: u64,
        target: crate::ui::TabSurfaceTarget,
    ) -> bool {
        self.apply_shell_tab_geometry_to_target(client_id, target, true)
    }

    /// Applies a public socket request, including its session-wide focus projection.
    pub(super) fn handle_api_request_with_shutdown_check(
        &mut self,
        mut msg: api::ApiRequestMessage,
    ) -> bool {
        let target_before = self.default_shell_target();
        let popup_before = self.app.state.popup_pane.is_some();
        let method_claims_geometry = Self::public_request_may_change_geometry(&msg.request.method);
        let explicit_public_focus_target = match &msg.request.method {
            api::schema::Method::WorkspaceFocus(params) => self
                .app
                .parse_workspace_id(&params.workspace_id)
                .and_then(|workspace_index| {
                    let workspace = self.app.state.workspaces.get(workspace_index)?;
                    Some(crate::ui::TabSurfaceTarget {
                        workspace_index,
                        tab_index: workspace.active_tab_index(),
                    })
                }),
            api::schema::Method::TabFocus(params) => {
                self.app
                    .parse_tab_id(&params.tab_id)
                    .map(|(workspace_index, tab_index)| crate::ui::TabSurfaceTarget {
                        workspace_index,
                        tab_index,
                    })
            }
            api::schema::Method::PaneFocus(params) => self
                .app
                .parse_pane_id(&params.pane_id)
                .and_then(|(workspace_index, pane_id)| {
                    let workspace = self.app.state.workspaces.get(workspace_index)?;
                    Some(crate::ui::TabSurfaceTarget {
                        workspace_index,
                        tab_index: workspace.find_tab_index_for_pane(pane_id)?,
                    })
                }),
            _ => None,
        };
        let create_focus_requested = match &msg.request.method {
            api::schema::Method::WorkspaceCreate(params) => params.focus,
            api::schema::Method::TabCreate(params) => params.focus,
            _ => false,
        };
        let inspect_worktree_open = matches!(
            &msg.request.method,
            api::schema::Method::WorktreeOpen(params) if params.focus
        );
        let response_proxy = inspect_worktree_open.then(|| {
            let (proxy_tx, proxy_rx) = std::sync::mpsc::channel();
            let original = std::mem::replace(&mut msg.respond_to, proxy_tx);
            (original, proxy_rx)
        });
        let reconcile = Self::shell_locations_may_need_reconcile(&msg.request.method);
        let changed = self.handle_api_request_with_shutdown_check_inner(msg, false);
        let worktree_open_succeeded = forward_proxied_api_response(response_proxy);
        let target_changed = self.default_shell_target() != target_before;
        let public_focus_succeeded = explicit_public_focus_target
            .is_some_and(|target| self.default_shell_target() == Some(target))
            || (create_focus_requested && target_changed)
            || worktree_open_succeeded;
        if public_focus_succeeded {
            self.focus_all_shell_clients_on_default_target();
        }
        if reconcile || target_changed || self.app.state.popup_pane.is_some() != popup_before {
            self.reconcile_client_shell_locations();
        }
        let geometry_changed =
            method_claims_geometry && self.reapply_controlled_shell_tab_geometry(false);
        changed | geometry_changed
    }

    pub(super) fn handle_client_shell_api_request(
        &mut self,
        client_id: u64,
        msg: api::ApiRequestMessage,
    ) -> bool {
        let focus_before = self.shell_focus_target(client_id);
        let focused_tabs_before = self.focused_shell_tabs();
        let method_claims_geometry = Self::shell_endpoint_claims_geometry(&msg.request.method);
        let reconcile = Self::shell_locations_may_need_reconcile(&msg.request.method);
        let all_focus_before = reconcile.then(|| self.shell_focus_targets());
        let navigation_changed =
            self.apply_shell_navigation_request(client_id, &msg.request.method);
        self.set_default_shell_target_from_client(client_id);
        let popup_before = self.app.state.popup_pane.is_some();
        let popup_owner = self.shell_tab_id_for_client(client_id);
        let changed = self.handle_api_request_with_shutdown_check_inner(msg, false);
        self.focus_shell_client_on_default_target(client_id);
        if !popup_before && self.app.state.popup_pane.is_some() {
            self.popup_owner_tab_id = popup_owner;
        }
        if reconcile || self.app.state.popup_pane.is_some() != popup_before {
            self.reconcile_client_shell_locations();
        }
        if let Some(all_focus_before) = all_focus_before {
            self.finish_shell_location_reconciliation(all_focus_before, &focused_tabs_before);
        } else {
            let focus_after = self.shell_focus_target(client_id);
            let focused_tabs_after = self.focused_shell_tabs();
            self.app.accept_current_focus_without_events();
            self.send_shell_navigation_focus_events(
                focus_before.as_ref(),
                focus_after.as_ref(),
                &focused_tabs_before,
                &focused_tabs_after,
            );
        }
        let geometry_changed = method_claims_geometry
            && if reconcile {
                self.reapply_controlled_shell_tab_geometry(false)
            } else {
                self.claim_shell_tab_geometry(client_id, false)
                    || self.resize_shell_tab_if_controller(client_id, false)
            };
        changed | navigation_changed | geometry_changed
    }
}
