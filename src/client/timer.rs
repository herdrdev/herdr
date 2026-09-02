use std::time::{Duration, Instant};

pub(super) struct ClientLoopTimer {
    deadline: Option<Instant>,
}

impl ClientLoopTimer {
    pub(super) fn new() -> Self {
        Self { deadline: None }
    }

    pub(super) fn deadline(&mut self, now: Instant, delay: Duration) -> Instant {
        let requested = now.checked_add(delay).unwrap_or(now);
        let deadline = self
            .deadline
            .map_or(requested, |current| current.min(requested));
        self.deadline = Some(deadline);
        deadline
    }

    pub(super) fn fired(&mut self) {
        self.deadline = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_events_do_not_postpone_timer_deadline() {
        let start = Instant::now();
        let delay = Duration::from_millis(100);
        let mut timer = ClientLoopTimer::new();

        let first_deadline = timer.deadline(start, delay);
        assert_eq!(
            timer.deadline(start + Duration::from_millis(25), delay),
            first_deadline
        );
        assert_eq!(
            timer.deadline(start + Duration::from_millis(50), delay),
            first_deadline
        );
        assert_eq!(
            timer.deadline(start + Duration::from_millis(75), delay),
            first_deadline
        );

        timer.fired();
        assert_eq!(
            timer.deadline(first_deadline, delay),
            first_deadline + delay
        );
    }

    #[test]
    fn earlier_client_work_can_pull_the_timer_deadline_forward() {
        let start = Instant::now();
        let mut timer = ClientLoopTimer::new();

        assert_eq!(
            timer.deadline(start, Duration::from_millis(100)),
            start + Duration::from_millis(100)
        );
        assert_eq!(
            timer.deadline(start + Duration::from_millis(20), Duration::from_millis(10)),
            start + Duration::from_millis(30)
        );
        assert_eq!(
            timer.deadline(
                start + Duration::from_millis(25),
                Duration::from_millis(100)
            ),
            start + Duration::from_millis(30)
        );
    }
}
