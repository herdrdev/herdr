#[derive(Clone, Default)]
pub struct EventHub {
    inner: std::sync::Arc<std::sync::Mutex<EventHubState>>,
}

#[derive(Default)]
struct EventHubState {
    next_sequence: u64,
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
        state.events.push((sequence, event));
        let overflow = state.events.len().saturating_sub(Self::MAX_EVENTS);
        if overflow > 0 {
            state.events.drain(0..overflow);
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
        if state
            .events
            .first()
            .is_some_and(|(first, _)| sequence < first.saturating_sub(1))
        {
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
