use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::layout::PaneId;

#[derive(Debug, Default)]
pub(crate) struct RenderRequest {
    pub(crate) generic: bool,
    pub(crate) pty_sources: HashSet<PaneId>,
}

/// Coalesces render requests while retaining enough origin information for the
/// headless server to discard PTY-only updates hidden from every client.
#[derive(Debug, Default)]
pub(crate) struct RenderSignal {
    pending: AtomicBool,
    request: Mutex<RenderRequest>,
}

impl RenderSignal {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub(crate) fn request_generic(&self) {
        let mut request = self
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        request.generic = true;
        self.pending.store(true, Ordering::Release);
    }

    /// Returns true when this request transitions the signal from idle to pending.
    pub(crate) fn request_pty(&self, pane_id: PaneId) -> bool {
        let mut request = self
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        request.pty_sources.insert(pane_id);
        !self.pending.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn take(&self) -> RenderRequest {
        let mut request = self
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pending.store(false, Ordering::Release);
        std::mem::take(&mut *request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_pty_sources_until_taken() {
        let signal = RenderSignal::new();
        let first = PaneId::from_raw(10);
        let second = PaneId::from_raw(20);

        assert!(signal.request_pty(first));
        assert!(!signal.request_pty(first));
        assert!(!signal.request_pty(second));

        let request = signal.take();
        assert!(!request.generic);
        assert_eq!(request.pty_sources, HashSet::from([first, second]));
        assert!(!signal.is_pending());
    }

    #[test]
    fn keeps_generic_and_pty_requests_distinct() {
        let signal = RenderSignal::new();
        let pane_id = PaneId::from_raw(10);

        signal.request_generic();
        assert!(!signal.request_pty(pane_id));

        let request = signal.take();
        assert!(request.generic);
        assert_eq!(request.pty_sources, HashSet::from([pane_id]));
    }
}
