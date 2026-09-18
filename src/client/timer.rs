use std::time::{Duration, Instant};

/// Hard ceiling on how far out the client event loop may park on its timer.
///
/// The loop's only other wake sources are the supervisor/event channels and
/// (since the quit-wake fix) the quit wake. If a far or stale deadline were
/// registered without a `fired()` in between, the loop could park for longer
/// than this cap, starving the heartbeat (`tick_health`) and the quit-flag
/// check. Clamping the deadline keeps the loop re-waking at least this often
/// regardless of any requested delay. Normal delays (100 ms) are unaffected.
pub(super) const MAX_LOOP_TIMER_DELAY: Duration = Duration::from_millis(250);
pub(super) struct ClientLoopTimer {
    deadline: Option<Instant>,
}

impl ClientLoopTimer {
    pub(super) fn new() -> Self {
        Self { deadline: None }
    }

    pub(super) fn deadline(&mut self, now: Instant, delay: Duration) -> Instant {
        let requested = now
            .checked_add(delay.min(MAX_LOOP_TIMER_DELAY))
            .unwrap_or(now);
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
    fn far_deadline_is_clamped_to_the_wake_cap() {
        let start = Instant::now();
        let mut timer = ClientLoopTimer::new();

        // A far/stale deadline (e.g. a missed autoscroll-settle wake) must not
        // park the loop further out than the cap.
        let far = timer.deadline(start, Duration::from_secs(60));
        assert_eq!(far, start + MAX_LOOP_TIMER_DELAY);

        // A second, even farther request does not extend the clamped deadline.
        assert_eq!(
            timer.deadline(start + Duration::from_millis(100), Duration::from_secs(120)),
            far
        );

        // A normal delay is not affected by the cap.
        timer.fired();
        assert_eq!(
            timer.deadline(start, Duration::from_millis(100)),
            start + Duration::from_millis(100)
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
