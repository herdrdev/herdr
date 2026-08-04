use crate::terminal::TerminalId;

/// Viewport state for a pane.
///
/// Terminal identity, cwd, labels, and agent metadata live in TerminalState.
pub struct PaneState {
    pub attached_terminal_id: TerminalId,
    /// Monotonic, tab-local number used by the generated `pane N` label.
    ///
    /// This is deliberately separate from `TerminalState::manual_label`: the
    /// number belongs to the pane's position in a tab and is reassigned when a
    /// live pane moves into a different tab.
    pub auto_name_number: usize,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
}

impl PaneState {
    pub fn new(attached_terminal_id: TerminalId) -> Self {
        Self {
            attached_terminal_id,
            auto_name_number: 1,
            seen: true,
        }
    }

    pub fn with_auto_name_number(mut self, number: usize) -> Self {
        self.auto_name_number = number.max(1);
        self
    }

    pub fn auto_name(&self) -> String {
        format!("pane {}", self.auto_name_number)
    }
}
