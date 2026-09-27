#[cfg(unix)]
use serde::{Deserialize, Serialize};

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StallNudgeHandoffState {
    pub nudges_sent: u32,
    #[serde(default)]
    pub nudges_without_human: u32,
    #[serde(default = "default_stall_nudge_episode_active")]
    pub episode_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_nudge_in: Option<std::time::Duration>,
    #[serde(default)]
    pub schedule_failed: bool,
}

#[cfg(unix)]
fn default_stall_nudge_episode_active() -> bool {
    true
}

/// Long-lived pane runtime transferred during server replacement.
///
/// Handoff preserves server-owned session state such as PTYs, processes, agent
/// identity, and durable plugin/session metadata. It intentionally does not
/// preserve transient coordination such as in-flight requests, waits,
/// subscriptions, client sockets, or pane-to-pane messages; clients reconnect
/// and retry those operations after replacement.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffRuntimeState {
    pub pane_id: u32,
    pub child_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty_name: Option<std::path::PathBuf>,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    #[serde(default)]
    pub keyboard_protocol_flags: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_protocol_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_state: Option<crate::pane::InputState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_history_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_activity: Option<crate::terminal::AgentActivityHandoffState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<crate::terminal::TerminalAgentHandoffState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_nudge: Option<StallNudgeHandoffState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_draft: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_seen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_done_for_ms: Option<u64>,
}

#[cfg(unix)]
impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    #[cfg(unix)]
    pub master_fd: std::os::fd::RawFd,
    #[cfg(unix)]
    pub state: HandoffRuntimeState,
}

#[cfg(all(test, unix))]
mod tests {
    use super::{HandoffRuntimeState, StallNudgeHandoffState};

    /// AC6: handoff payloads written before human-draft transfer still deserialize.
    #[test]
    fn handoff_runtime_without_human_draft_still_deserializes() {
        let payload = serde_json::json!({
            "pane_id": 7,
            "child_pid": 11,
            "rows": 24,
            "cols": 80,
            "cell_width_px": 8,
            "cell_height_px": 16
        });

        let state: HandoffRuntimeState =
            serde_json::from_value(payload).expect("older handoff runtime should deserialize");

        assert!(state.human_draft.is_none());
    }

    #[test]
    fn an_older_stall_nudge_handoff_keeps_its_episode_active() {
        let state: StallNudgeHandoffState =
            serde_json::from_value(serde_json::json!({ "nudges_sent": 2 }))
                .expect("older stall nudge state should deserialize");

        assert_eq!(state.nudges_without_human, 0);
        assert!(state.episode_active);
    }
}
