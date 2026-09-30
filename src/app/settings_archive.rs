//! The Archive section of the settings screen.
//!
//! Archive is a second view onto the same settled panes the sidebar shows, so
//! it reads the workspace/pane state directly and acts through the same paths
//! the sidebar's settled menu uses.

use crate::app::state::{AppState, PaneFocusTarget};

/// One settled pane, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveEntry {
    pub(crate) target: PaneFocusTarget,
    /// Workspace label, for the left column.
    pub(crate) workspace: String,
    /// The thread's own title, or its cwd when it has none.
    pub(crate) title: String,
    /// Unix seconds the pane settled at.
    pub(crate) settled_at: u64,
}

impl AppState {
    /// Settled panes in workspace then tab order, matching the sidebar. A
    /// settled list that reorders itself between visits is unusable.
    pub(crate) fn archive_entries(&self) -> Vec<ArchiveEntry> {
        let mut entries = Vec::new();
        for workspace in &self.workspaces {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let Some(settled_at) = pane.settled_at else {
                        continue;
                    };
                    let terminal = self.terminals.get(&pane.attached_terminal_id);
                    let title = terminal
                        .map(crate::terminal::TerminalState::effective_work_context)
                        .and_then(|context| {
                            context
                                .work_title
                                .clone()
                                .or_else(|| context.session_name.clone())
                        })
                        .or_else(|| terminal.map(|terminal| terminal.cwd.display().to_string()))
                        .unwrap_or_else(|| format!("{pane_id:?}"));
                    entries.push(ArchiveEntry {
                        target: PaneFocusTarget {
                            workspace_id: workspace.id.clone(),
                            pane_id: *pane_id,
                        },
                        workspace: workspace.display_name_from_terminals(&self.terminals),
                        title,
                        settled_at,
                    });
                }
            }
        }
        entries
    }
}

impl super::App {
    /// Unsettle the pane and focus it, the same transition the sidebar's
    /// "resume thread" performs.
    pub(crate) fn restore_archived_pane(&mut self, target: &PaneFocusTarget) -> bool {
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return false;
        };
        self.state
            .note_human_pane_activity_at(target.pane_id, std::time::Instant::now());
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
        self.flush_pane_settlement_events();
        true
    }

    /// Close the pane through the runtime close path, which applies the
    /// existing close confirmation.
    pub(crate) fn delete_archived_pane(&mut self, target: &PaneFocusTarget) -> bool {
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return false;
        };
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
        self.close_focused_pane_via_api_requires_confirmation();
        self.flush_pane_settlement_events();
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::app::state::AppState;

    #[test]
    fn archive_lists_only_settled_panes() {
        let mut state = AppState::test_new();
        let workspace = crate::workspace::Workspace::test_new("settled");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .pane_state(pane_id)
            .expect("root pane")
            .attached_terminal_id
            .clone();
        state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id, "/repo".into()),
        );
        state.workspaces.push(workspace);
        assert!(state.archive_entries().is_empty());

        let ws_idx = state.workspaces.len() - 1;
        assert!(state.settle_pane_at(ws_idx, pane_id, 1_700_000_000));

        let entries = state.archive_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target.pane_id, pane_id);
        assert_eq!(entries[0].settled_at, 1_700_000_000);
    }
}
