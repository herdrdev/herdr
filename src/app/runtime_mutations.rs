use crate::api::schema::{
    EmptyParams, LayoutSetSplitRatioParams, Method, PaneFocusDirectionParams, PaneInputSetParams,
    PaneRenameParams, PaneResizeParams, PaneSnoozeParams, PaneSplitParams, PaneSwapParams,
    PaneTarget, PaneWorkContextSetParams, PaneZoomParams, TabCreateParams, TabMoveParams,
    TabPinParams, TabRenameParams, TabStarParams, TabTarget, WorkspaceCloseParams,
    WorkspaceCreateParams, WorkspaceMoveBlockParams, WorkspaceMoveParams, WorkspaceRenameParams,
    WorkspaceTarget, WorktreeCreateParams, WorktreeOpenParams, WorktreeRemoveParams,
};

use super::App;

impl App {
    pub(crate) fn show_pod_mutation_error(&mut self, title: &str, response: &str) {
        let context = serde_json::from_str::<crate::api::schema::ErrorResponse>(response)
            .map(|error| error.error.message)
            .unwrap_or_else(|_| "pod mutation failed".to_string());
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: title.to_string(),
            context,
            position: None,
            target: None,
        });
    }

    pub(crate) fn runtime_delete_pod(&mut self, record: crate::groups::GroupRecord) {
        let response = self.dispatch_runtime_mutation(
            "sidebar-pod-delete",
            Method::GroupDelete(crate::api::schema::GroupDeleteParams {
                group_id: record.id,
                expected_revision: record.revision,
            }),
        );
        if serde_json::from_str::<crate::api::schema::ErrorResponse>(&response).is_ok() {
            self.show_pod_mutation_error("Pod not deleted", &response);
        }
    }

    pub(crate) fn dispatch_runtime_mutation(&mut self, id: &'static str, method: Method) -> String {
        self.dispatch_api_request(id, method)
    }

    pub(crate) fn dispatch_deferred_runtime_mutation(
        &mut self,
        id: &'static str,
        method: Method,
    ) -> Option<String> {
        self.dispatch_deferred_api_request(id, method)
    }

    pub(crate) fn runtime_pane_group_set(
        &mut self,
        params: crate::api::schema::PaneGroupSetParams,
    ) {
        let response =
            self.dispatch_runtime_mutation("sidebar-pod-assign", Method::PaneGroupSet(params));
        let Ok(error) = serde_json::from_str::<crate::api::schema::ErrorResponse>(&response) else {
            return;
        };
        let context = match error.error.code.as_str() {
            "revision_conflict" => "membership changed elsewhere — not moved".to_string(),
            "authority_not_fresh" => format!("pod owner unreachable: {}", error.error.message),
            "group_not_found" => format!("pod no longer exists: {}", error.error.message),
            _ => error.error.message,
        };
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "Pod not moved".to_string(),
            context,
            position: None,
            target: None,
        });
    }

    pub(crate) fn runtime_workspace_focus(
        &mut self,
        id: &'static str,
        workspace_id: String,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceFocus(WorkspaceTarget { workspace_id }))
    }

    pub(crate) fn runtime_workspace_create(
        &mut self,
        id: &'static str,
        params: WorkspaceCreateParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceCreate(params))
    }

    pub(crate) fn runtime_workspace_rename(
        &mut self,
        id: &'static str,
        params: WorkspaceRenameParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceRename(params))
    }

    pub(crate) fn runtime_workspace_move(
        &mut self,
        id: &'static str,
        params: WorkspaceMoveParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceMove(params))
    }

    pub(crate) fn runtime_workspace_move_block(
        &mut self,
        id: &'static str,
        params: WorkspaceMoveBlockParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceMoveBlock(params))
    }

    pub(crate) fn runtime_workspace_close_group(
        &mut self,
        id: &'static str,
        workspace_id: String,
    ) -> String {
        self.dispatch_runtime_mutation(
            id,
            Method::WorkspaceClose(WorkspaceCloseParams {
                workspace_id,
                close_group: true,
            }),
        )
    }

    pub(crate) fn runtime_tab_create(
        &mut self,
        id: &'static str,
        params: TabCreateParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::TabCreate(params))
    }

    pub(crate) fn runtime_tab_focus(&mut self, id: &'static str, tab_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::TabFocus(TabTarget { tab_id }))
    }

    pub(crate) fn runtime_tab_rename(
        &mut self,
        id: &'static str,
        params: TabRenameParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::TabRename(params))
    }

    pub(crate) fn runtime_tab_pin(&mut self, id: &'static str, params: TabPinParams) -> String {
        self.dispatch_runtime_mutation(id, Method::TabPin(params))
    }

    pub(crate) fn runtime_tab_star(&mut self, id: &'static str, params: TabStarParams) -> String {
        self.dispatch_runtime_mutation(id, Method::TabStar(params))
    }

    pub(crate) fn runtime_tab_move(&mut self, id: &'static str, params: TabMoveParams) -> String {
        self.dispatch_runtime_mutation(id, Method::TabMove(params))
    }

    pub(crate) fn runtime_tab_close(&mut self, id: &'static str, tab_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::TabClose(TabTarget { tab_id }))
    }

    pub(crate) fn runtime_server_reload_config(&mut self, id: &'static str) -> String {
        self.dispatch_runtime_mutation(id, Method::ServerReloadConfig(EmptyParams::default()))
    }

    pub(crate) fn runtime_pane_focus(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneFocus(PaneTarget { pane_id }))
    }

    pub(crate) fn runtime_pane_close(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneClose(PaneTarget { pane_id }))
    }

    pub(crate) fn runtime_pane_settle(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneSettle(PaneTarget { pane_id }))
    }

    pub(crate) fn runtime_pane_snooze(
        &mut self,
        id: &'static str,
        params: PaneSnoozeParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneSnooze(params))
    }

    pub(crate) fn runtime_pane_unsnooze(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneUnsnooze(PaneTarget { pane_id }))
    }

    pub(crate) fn dispatch_remote_pane_lifecycle(
        &mut self,
        agent_ref: crate::api::schema::AgentRef,
        method: Method,
    ) -> Result<(), String> {
        let route = self
            .state
            .fleet_snapshot
            .fresh_agent_host(&agent_ref)
            .ok_or_else(|| format!("owner {} is unreachable", agent_ref.host))?;
        self.authority_mutation_router.enqueue_pane_lifecycle(
            route,
            agent_ref,
            self.state.fleet_snapshot.config_generation,
            crate::api::schema::Request {
                id: "tui.remote-pane-lifecycle".into(),
                method,
            },
            self.event_tx.clone(),
        )
    }

    pub(crate) fn remote_pane_settle(
        &mut self,
        agent_ref: crate::api::schema::AgentRef,
    ) -> Result<(), String> {
        let pane_id = agent_ref.agent.clone();
        self.dispatch_remote_pane_lifecycle(agent_ref, Method::PaneSettle(PaneTarget { pane_id }))
    }

    pub(crate) fn remote_pane_close(
        &mut self,
        agent_ref: crate::api::schema::AgentRef,
    ) -> Result<(), String> {
        let pane_id = agent_ref.agent.clone();
        self.dispatch_remote_pane_lifecycle(agent_ref, Method::PaneClose(PaneTarget { pane_id }))
    }

    pub(crate) fn remote_pane_snooze(
        &mut self,
        agent_ref: crate::api::schema::AgentRef,
        params: PaneSnoozeParams,
    ) -> Result<(), String> {
        self.dispatch_remote_pane_lifecycle(agent_ref, Method::PaneSnooze(params))
    }

    pub(crate) fn remote_pane_unsnooze(
        &mut self,
        agent_ref: crate::api::schema::AgentRef,
    ) -> Result<(), String> {
        let pane_id = agent_ref.agent.clone();
        self.dispatch_remote_pane_lifecycle(agent_ref, Method::PaneUnsnooze(PaneTarget { pane_id }))
    }

    pub(crate) fn runtime_pane_rename(
        &mut self,
        id: &'static str,
        params: PaneRenameParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneRename(params))
    }

    pub(crate) fn runtime_pane_input_set(
        &mut self,
        id: &'static str,
        params: PaneInputSetParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneInputSet(params))
    }

    pub(crate) fn runtime_pane_work_context_set(
        &mut self,
        id: &'static str,
        params: PaneWorkContextSetParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneWorkContextSet(params))
    }

    pub(crate) fn runtime_pane_focus_direction(
        &mut self,
        id: &'static str,
        params: PaneFocusDirectionParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneFocusDirection(params))
    }

    pub(crate) fn runtime_pane_resize(
        &mut self,
        id: &'static str,
        params: PaneResizeParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneResize(params))
    }

    pub(crate) fn runtime_pane_swap(&mut self, id: &'static str, params: PaneSwapParams) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneSwap(params))
    }

    pub(crate) fn runtime_pane_split(
        &mut self,
        id: &'static str,
        params: PaneSplitParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneSplit(params))
    }

    pub(crate) fn runtime_pane_zoom(&mut self, id: &'static str, params: PaneZoomParams) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneZoom(params))
    }

    pub(crate) fn runtime_layout_set_split_ratio(
        &mut self,
        id: &'static str,
        params: LayoutSetSplitRatioParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::LayoutSetSplitRatio(params))
    }

    pub(crate) fn runtime_worktree_create_deferred(
        &mut self,
        id: &'static str,
        params: WorktreeCreateParams,
    ) -> Option<String> {
        self.dispatch_deferred_runtime_mutation(id, Method::WorktreeCreate(params))
    }

    pub(crate) fn runtime_worktree_open(
        &mut self,
        id: &'static str,
        params: WorktreeOpenParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorktreeOpen(params))
    }

    pub(crate) fn runtime_worktree_remove_deferred(
        &mut self,
        id: &'static str,
        params: WorktreeRemoveParams,
    ) -> Option<String> {
        self.dispatch_deferred_runtime_mutation(id, Method::WorktreeRemove(params))
    }
}
