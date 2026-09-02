use super::App;

impl App {
    pub(crate) fn should_shutdown_workspace_terminal_runtimes_for_worktree_remove(
        force: bool,
    ) -> bool {
        force || cfg!(windows)
    }

    pub(crate) fn close_removed_linked_worktree_workspace(&mut self, ws_idx: usize) {
        let removed_workspace_was_active = self.state.active == Some(ws_idx);
        let parent_key = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.worktree_space())
            .filter(|space| space.is_linked_worktree)
            .map(|space| space.key.clone());

        self.state.selected = ws_idx;
        self.state.close_selected_workspace();

        if !removed_workspace_was_active {
            return;
        }
        let Some(parent_key) = parent_key else {
            return;
        };
        let Some(parent_idx) = self.state.workspaces.iter().position(|workspace| {
            workspace
                .worktree_space()
                .is_some_and(|space| !space.is_linked_worktree && space.key == parent_key)
        }) else {
            return;
        };
        self.state.switch_workspace(parent_idx);
    }

    pub(crate) fn shutdown_workspace_terminal_runtimes_for_worktree_remove(
        &mut self,
        ws_idx: usize,
    ) -> Vec<crate::layout::PaneId> {
        let mut shutdown_panes = Vec::new();
        for pane_id in self.state.pane_ids_for_workspace(ws_idx) {
            let Some(terminal_id) = self.state.terminal_id_for_pane(ws_idx, pane_id) else {
                continue;
            };
            if self.terminal_runtimes.get(&terminal_id).is_none() {
                continue;
            }
            tracing::debug!(
                workspace_index = ws_idx,
                terminal_id = %terminal_id,
                "shutting down terminal runtime before worktree removal"
            );
            *self
                .pending_worktree_remove_runtime_exits
                .entry(pane_id)
                .or_default() += 1;
            shutdown_panes.push(pane_id);
            self.shutdown_terminal_runtime(terminal_id);
        }
        shutdown_panes
    }
}

#[cfg(test)]
mod tests {
    use super::App;

    #[test]
    fn worktree_remove_runtime_shutdown_policy_preserves_windows_safe_remove() {
        assert_eq!(
            App::should_shutdown_workspace_terminal_runtimes_for_worktree_remove(false),
            cfg!(windows)
        );
        assert!(App::should_shutdown_workspace_terminal_runtimes_for_worktree_remove(true));
    }
}
