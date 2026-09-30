#[derive(Clone, Default)]
pub struct EventHub {
    inner: std::sync::Arc<std::sync::Mutex<EventHubState>>,
}

#[derive(Default)]
struct EventHubState {
    next_sequence: u64,
    evicted_through: u64,
    events: Vec<(u64, crate::api::schema::EventEnvelope)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventHistoryError {
    Lost,
    Unavailable,
}

impl EventHub {
    const MAX_EVENTS: usize = 512;

    pub fn push(&self, event: crate::api::schema::EventEnvelope) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        state.next_sequence += 1;
        let sequence = state.next_sequence;
        if event.event == crate::api::schema::EventKind::SessionChanged {
            state.events.retain(|(_, retained)| {
                retained.event != crate::api::schema::EventKind::SessionChanged
            });
        }
        state.events.push((sequence, event));
        let overflow = state.events.len().saturating_sub(Self::MAX_EVENTS);
        if overflow > 0 {
            let evicted_through = {
                let mut evicted = state.events.drain(0..overflow);
                evicted.next_back().map(|(sequence, _)| sequence)
            };
            if let Some(sequence) = evicted_through {
                state.evicted_through = sequence;
            }
        }
    }

    pub fn events_after(&self, sequence: u64) -> Vec<(u64, crate::api::schema::EventEnvelope)> {
        let Ok(state) = self.inner.lock() else {
            return Vec::new();
        };
        state
            .events
            .iter()
            .filter(|(event_sequence, _)| *event_sequence > sequence)
            .cloned()
            .collect()
    }

    pub(super) fn events_after_checked(
        &self,
        sequence: u64,
    ) -> Result<Vec<(u64, crate::api::schema::EventEnvelope)>, EventHistoryError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| EventHistoryError::Unavailable)?;
        if sequence < state.evicted_through {
            return Err(EventHistoryError::Lost);
        }
        Ok(state
            .events
            .iter()
            .filter(|(event_sequence, _)| *event_sequence > sequence)
            .cloned()
            .collect())
    }

    pub fn current_sequence(&self) -> u64 {
        let Ok(state) = self.inner.lock() else {
            return 0;
        };
        state.next_sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{EventData, EventEnvelope, EventKind};

    fn event() -> EventEnvelope {
        EventEnvelope {
            event: EventKind::WorkspaceFocused,
            data: EventData::WorkspaceFocused {
                workspace_id: "workspace_1".into(),
            },
        }
    }

    fn session_event(revision: u64) -> EventEnvelope {
        EventEnvelope {
            event: EventKind::SessionChanged,
            data: EventData::SessionChanged {
                epoch: "epoch-1".into(),
                revision,
                snapshot: Box::new(crate::api::schema::SessionSnapshot {
                    epoch: Some("epoch-1".into()),
                    revision: Some(revision),
                    version: "0.9.1".into(),
                    protocol: 16,
                    focused_workspace_id: None,
                    focused_tab_id: None,
                    focused_pane_id: None,
                    workspaces: Vec::new(),
                    tabs: Vec::new(),
                    panes: Vec::new(),
                    layouts: Vec::new(),
                    agents: Vec::new(),
                }),
            },
        }
    }

    #[test]
    fn history_coalesces_session_snapshots_and_retains_other_events() {
        let hub = EventHub::default();
        for revision in 1..=40 {
            hub.push(event());
            hub.push(session_event(revision));
        }

        let retained = hub.events_after_checked(0).unwrap();
        assert_eq!(
            retained
                .iter()
                .filter(|(_, event)| event.event == EventKind::SessionChanged)
                .count(),
            1
        );
        assert_eq!(
            retained
                .iter()
                .filter(|(_, event)| event.event == EventKind::WorkspaceFocused)
                .count(),
            40
        );
        assert!(matches!(
            retained.last().map(|(_, event)| &event.data),
            Some(EventData::SessionChanged { revision: 40, .. })
        ));
    }

    #[test]
    fn checked_history_distinguishes_retained_boundary_from_lost_events() {
        let hub = EventHub::default();
        assert!(hub.events_after_checked(0).unwrap().is_empty());
        for _ in 0..EventHub::MAX_EVENTS {
            hub.push(event());
        }
        assert_eq!(
            hub.events_after_checked(0).unwrap().len(),
            EventHub::MAX_EVENTS
        );
        hub.push(event());
        assert_eq!(hub.events_after_checked(0), Err(EventHistoryError::Lost));
        let retained = hub.events_after_checked(1).unwrap();
        assert_eq!(retained.len(), EventHub::MAX_EVENTS);
        assert_eq!(retained.first().unwrap().0, 2);
        assert_eq!(retained.last().unwrap().0, hub.current_sequence());
        assert!(hub
            .events_after_checked(hub.current_sequence())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn checked_history_reports_unavailable_instead_of_empty_after_poison() {
        let hub = EventHub::default();
        assert!(std::panic::catch_unwind(|| {
            let _guard = hub.inner.lock().unwrap();
            panic!("poison the test event history");
        })
        .is_err());
        assert_eq!(
            hub.events_after_checked(0),
            Err(EventHistoryError::Unavailable)
        );
    }
}
