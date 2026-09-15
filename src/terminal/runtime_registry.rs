use std::collections::HashMap;

use super::{TerminalId, TerminalRuntime};

/// Server-owned live terminal runtimes, keyed by durable terminal id.
///
/// This sits outside `AppState` so pure state can stay focused on workspace,
/// pane, and terminal metadata while the server/application layer owns PTYs,
/// parser backends, detector tasks, and channels.
#[derive(Default)]
pub(crate) struct TerminalRuntimeRegistry {
    runtimes: HashMap<TerminalId, TerminalRuntime>,
}

impl TerminalRuntimeRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self, terminal_id: &TerminalId) -> Option<&TerminalRuntime> {
        self.runtimes.get(terminal_id)
    }

    pub(crate) fn insert(
        &mut self,
        terminal_id: TerminalId,
        runtime: TerminalRuntime,
    ) -> Option<TerminalRuntime> {
        self.runtimes.insert(terminal_id, runtime)
    }

    pub(crate) fn remove(&mut self, terminal_id: &TerminalId) -> Option<TerminalRuntime> {
        self.runtimes.remove(terminal_id)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &TerminalRuntime> {
        self.runtimes.values()
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&TerminalId, &TerminalRuntime)> {
        self.runtimes.iter()
    }

    #[cfg(unix)]
    pub(crate) fn set_handoff_readers_paused(&self, paused: bool) {
        for runtime in self.runtimes.values() {
            runtime.set_handoff_reader_paused(paused);
        }
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn assume_handoff_ownership(&mut self) {
        for runtime in self.runtimes.values_mut() {
            runtime.assume_handoff_ownership();
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.runtimes.len()
    }

    #[cfg(unix)]
    pub(crate) fn nudge_child_redraw_after_handoff(&self) {
        for runtime in self.runtimes.values() {
            runtime.nudge_child_redraw_after_handoff();
        }
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn drain_for_handoff(
        &mut self,
    ) -> impl Iterator<Item = (TerminalId, TerminalRuntime)> + '_ {
        self.runtimes.drain()
    }

    #[cfg(windows)]
    pub(crate) fn activate_after_handoff(&self) {
        for (terminal_id, runtime) in &self.runtimes {
            if let Err(err) = runtime.activate_after_handoff() {
                // A timeout leaves activation queued. Continue with every pane;
                // after COMMIT an ACK error cannot unwind the owning server.
                tracing::warn!(terminal = %terminal_id, %err, "failed to acknowledge handoff activation; continuing as owner");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn drain(&mut self) -> impl Iterator<Item = (TerminalId, TerminalRuntime)> + '_ {
        self.runtimes.drain()
    }
}

impl From<HashMap<TerminalId, TerminalRuntime>> for TerminalRuntimeRegistry {
    fn from(runtimes: HashMap<TerminalId, TerminalRuntime>) -> Self {
        Self { runtimes }
    }
}
