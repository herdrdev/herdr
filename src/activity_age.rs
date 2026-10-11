use std::time::{Duration, Instant};

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
const MAX_DISPLAY_DAYS: u64 = 999;

/// Server-owned activity clock for one pane.
///
/// The timestamp covers user input, working or blocked agent transitions,
/// foreground process changes, and changes to the terminal text above the
/// agent's live UI chrome. Raw PTY writes do not advance it because idle agents
/// repaint footers and status lines without doing new work.
pub(crate) struct PaneActivity {
    last_at: Instant,
    last_human_at: Option<Instant>,
    restored_age_at_last_at: Option<Duration>,
    quiet_since: Option<Instant>,
    restored_age_at_quiet_since: Option<Duration>,
    content_revision: Option<u64>,
    detection_agent: Option<crate::detect::Agent>,
    detection_snapshot: Option<String>,
    detection_output_at: Option<Instant>,
    snoozed_until: Option<u64>,
}

impl PaneActivity {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_at: now,
            last_human_at: None,
            restored_age_at_last_at: None,
            quiet_since: None,
            restored_age_at_quiet_since: None,
            content_revision: None,
            detection_agent: None,
            detection_snapshot: None,
            detection_output_at: None,
            snoozed_until: None,
        }
    }

    pub(crate) fn snoozed_until(&self) -> Option<u64> {
        self.snoozed_until
    }

    pub(crate) fn set_snoozed_until(&mut self, deadline: Option<u64>) {
        self.snoozed_until = deadline;
    }

    pub(crate) fn take_snoozed_until(&mut self) -> Option<u64> {
        self.snoozed_until.take()
    }

    pub(crate) fn note(&mut self, now: Instant) {
        self.last_at = now;
        self.restored_age_at_last_at = None;
    }

    pub(crate) fn note_human(&mut self, now: Instant) {
        self.note(now);
        self.last_human_at = Some(now);
    }

    pub(crate) fn last_human_at(&self) -> Option<Instant> {
        self.last_human_at
    }

    pub(crate) fn needs_detection_snapshot(
        &self,
        revision: u64,
        agent: Option<crate::detect::Agent>,
    ) -> bool {
        self.content_revision != Some(revision) || self.detection_agent != agent
    }

    pub(crate) fn observe_detection_snapshot(
        &mut self,
        revision: u64,
        agent: Option<crate::detect::Agent>,
        snapshot: &str,
        now: Instant,
    ) -> bool {
        let classifier_changed = self.detection_agent != agent;
        self.content_revision = Some(revision);
        self.detection_agent = agent;
        if classifier_changed {
            self.detection_snapshot = Some(snapshot.to_string());
            return false;
        }
        let changed = match self.detection_snapshot.replace(snapshot.to_string()) {
            Some(previous) => previous != snapshot,
            None => false,
        };
        if changed {
            self.detection_output_at = Some(now);
            self.note(now);
        }
        changed
    }

    pub(crate) fn inactive_for(&self, now: Instant) -> Duration {
        self.restored_age_at_last_at
            .unwrap_or(Duration::ZERO)
            .saturating_add(now.saturating_duration_since(self.last_at))
    }

    pub(crate) fn deadline_after(&self, quiet_for: Duration) -> Option<Instant> {
        deadline_after_age(self.last_at, self.restored_age_at_last_at, quiet_for)
    }

    #[cfg(test)]
    pub(crate) fn last_at(&self) -> Instant {
        self.last_at
    }

    pub(crate) fn detection_output_at(&self) -> Option<Instant> {
        self.detection_output_at
    }

    pub(crate) fn detection_output_unix_timestamp_at(
        &self,
        now: Instant,
        now_unix: u64,
    ) -> Option<u64> {
        self.detection_output_at.map(|observed_at| {
            now_unix.saturating_sub(now.saturating_duration_since(observed_at).as_secs())
        })
    }

    pub(crate) fn restore_detection_output_unix_timestamp_at(
        &mut self,
        observed_at_unix: u64,
        now: Instant,
        now_unix: u64,
    ) {
        let elapsed = Duration::from_secs(now_unix.saturating_sub(observed_at_unix));
        self.detection_output_at = now.checked_sub(elapsed).or(Some(now));
    }

    pub(crate) fn unix_timestamp_at(&self, now: Instant, now_unix: u64) -> u64 {
        now_unix.saturating_sub(self.inactive_for(now).as_secs())
    }

    pub(crate) fn restore_unix_timestamp_at(
        &mut self,
        last_at_unix: u64,
        now: Instant,
        now_unix: u64,
    ) {
        let elapsed = Duration::from_secs(now_unix.saturating_sub(last_at_unix));
        if let Some(last_at) = now.checked_sub(elapsed) {
            self.last_at = last_at;
            self.restored_age_at_last_at = None;
        } else {
            self.last_at = now;
            self.restored_age_at_last_at = Some(elapsed);
        }
    }

    /// Observe the shared settle predicate without coupling its inputs to this
    /// clock. A missing observation is not evidence that a pane was already
    /// quiet, so the first quiet scan starts a fresh window.
    pub(crate) fn observe_quiet(&mut self, quiet: bool, now: Instant) -> bool {
        match (quiet, self.quiet_since) {
            (true, None) => {
                self.quiet_since = Some(now);
                self.restored_age_at_quiet_since = None;
                true
            }
            (false, Some(_)) => {
                self.quiet_since = None;
                self.restored_age_at_quiet_since = None;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn quiet_observation_changes(&self, quiet: bool) -> bool {
        self.quiet_since.is_some() != quiet
    }

    /// Age from the later of last activity and the latest quiet transition.
    pub(crate) fn quiet_for(&self, now: Instant) -> Option<Duration> {
        let quiet_since = self.quiet_since?;
        let quiet_age = self
            .restored_age_at_quiet_since
            .unwrap_or(Duration::ZERO)
            .saturating_add(now.saturating_duration_since(quiet_since));
        Some(self.inactive_for(now).min(quiet_age))
    }

    pub(crate) fn quiet_deadline_after(&self, threshold: Duration) -> Option<Instant> {
        let quiet_since = self.quiet_since?;
        let activity_deadline = self.deadline_after(threshold)?;
        let quiet_deadline =
            deadline_after_age(quiet_since, self.restored_age_at_quiet_since, threshold)?;
        Some(activity_deadline.max(quiet_deadline))
    }

    pub(crate) fn quiet_unix_timestamp_at(&self, now: Instant, now_unix: u64) -> Option<u64> {
        let quiet_since = self.quiet_since?;
        let age = self
            .restored_age_at_quiet_since
            .unwrap_or(Duration::ZERO)
            .saturating_add(now.saturating_duration_since(quiet_since));
        Some(now_unix.saturating_sub(age.as_secs()))
    }

    pub(crate) fn restore_quiet_unix_timestamp_at(
        &mut self,
        quiet_since_unix: u64,
        now: Instant,
        now_unix: u64,
    ) {
        let elapsed = Duration::from_secs(now_unix.saturating_sub(quiet_since_unix));
        if let Some(quiet_since) = now.checked_sub(elapsed) {
            self.quiet_since = Some(quiet_since);
            self.restored_age_at_quiet_since = None;
        } else {
            self.quiet_since = Some(now);
            self.restored_age_at_quiet_since = Some(elapsed);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_last_at(&mut self, at: Instant) {
        self.last_at = at;
        self.restored_age_at_last_at = None;
    }
}

fn deadline_after_age(
    observed_at: Instant,
    restored_age: Option<Duration>,
    threshold: Duration,
) -> Option<Instant> {
    observed_at.checked_add(threshold.saturating_sub(restored_age.unwrap_or(Duration::ZERO)))
}

pub(crate) fn compact_label(observed_at: Option<Instant>, now: Instant) -> String {
    let Some(observed_at) = observed_at else {
        return "--".into();
    };
    let seconds = now.saturating_duration_since(observed_at).as_secs();
    if seconds < SECONDS_PER_MINUTE {
        format!("{seconds}s")
    } else if seconds < SECONDS_PER_HOUR {
        format!("{}m", seconds / SECONDS_PER_MINUTE)
    } else if seconds < SECONDS_PER_DAY {
        format!("{}h", seconds / SECONDS_PER_HOUR)
    } else {
        format!("{}d", (seconds / SECONDS_PER_DAY).min(MAX_DISPLAY_DAYS))
    }
}

/// Minute-floor variant for the sidebar: a sub-minute age reads `<1m` instead of
/// a per-second counter, so overview rows never animate second by second.
pub(crate) fn coarse_label(observed_at: Option<Instant>, now: Instant) -> String {
    let Some(observed_at) = observed_at else {
        return "--".into();
    };
    if now.saturating_duration_since(observed_at).as_secs() < SECONDS_PER_MINUTE {
        "<1m".into()
    } else {
        compact_label(Some(observed_at), now)
    }
}

/// Repaint boundary matching [`coarse_label`]: nothing changes until the first
/// minute boundary, then it follows the compact buckets.
pub(crate) fn next_coarse_change_at(observed_at: Option<Instant>, now: Instant) -> Option<Instant> {
    let observed_at = observed_at?;
    if now.saturating_duration_since(observed_at).as_secs() < SECONDS_PER_MINUTE {
        observed_at.checked_add(Duration::from_secs(SECONDS_PER_MINUTE))
    } else {
        next_change_at(Some(observed_at), now)
    }
}

pub(crate) fn next_change_at(observed_at: Option<Instant>, now: Instant) -> Option<Instant> {
    let observed_at = observed_at?;
    next_change_after_elapsed(now.saturating_duration_since(observed_at), now)
}

/// Compute the next label boundary from an elapsed age. Persisted wall-clock
/// ages can exceed the platform's monotonic-clock range, so callers must not
/// need to manufacture an `Instant` before host uptime.
pub(crate) fn next_change_after_elapsed(elapsed: Duration, now: Instant) -> Option<Instant> {
    let seconds = elapsed.as_secs();
    let next_elapsed = if seconds < SECONDS_PER_MINUTE {
        seconds.saturating_add(1)
    } else if seconds < SECONDS_PER_HOUR {
        seconds
            .checked_div(SECONDS_PER_MINUTE)?
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_MINUTE)
    } else if seconds < SECONDS_PER_DAY {
        seconds
            .checked_div(SECONDS_PER_HOUR)?
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_HOUR)
    } else if seconds / SECONDS_PER_DAY < MAX_DISPLAY_DAYS {
        seconds
            .checked_div(SECONDS_PER_DAY)?
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_DAY)
    } else {
        return None;
    };
    now.checked_add(Duration::from_secs(next_elapsed).saturating_sub(elapsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_age_uses_stable_unit_buckets() {
        let started = Instant::now();
        for (elapsed, expected) in [
            (Duration::ZERO, "0s"),
            (Duration::from_secs(59), "59s"),
            (Duration::from_secs(60), "1m"),
            (Duration::from_secs(3_599), "59m"),
            (Duration::from_secs(3_600), "1h"),
            (Duration::from_secs(86_400), "1d"),
        ] {
            assert_eq!(compact_label(Some(started), started + elapsed), expected);
        }
        assert_eq!(compact_label(None, started), "--");
    }

    #[test]
    fn coarse_age_never_ticks_below_one_minute() {
        let started = Instant::now();
        assert_eq!(coarse_label(Some(started), started), "<1m");
        assert_eq!(
            coarse_label(Some(started), started + Duration::from_secs(59)),
            "<1m"
        );
        assert_eq!(
            coarse_label(Some(started), started + Duration::from_secs(60)),
            "1m"
        );
        assert_eq!(coarse_label(None, started), "--");
    }

    #[test]
    fn next_coarse_change_waits_for_the_minute_boundary() {
        let started = Instant::now();
        assert_eq!(
            next_coarse_change_at(Some(started), started + Duration::from_secs(7)),
            Some(started + Duration::from_secs(60))
        );
        assert_eq!(
            next_coarse_change_at(Some(started), started + Duration::from_secs(75)),
            Some(started + Duration::from_secs(120))
        );
    }

    #[test]
    fn next_change_matches_the_visible_bucket_boundary() {
        let started = Instant::now();
        assert_eq!(
            next_change_at(Some(started), started + Duration::from_secs(7)),
            Some(started + Duration::from_secs(8))
        );
        assert_eq!(
            next_change_at(Some(started), started + Duration::from_secs(75)),
            Some(started + Duration::from_secs(120))
        );
        assert_eq!(
            next_change_at(Some(started), started + Duration::from_secs(7_200)),
            Some(started + Duration::from_secs(10_800))
        );
    }

    #[test]
    fn elapsed_age_without_observed_instant_refreshes_at_bucket_boundary() {
        let now = Instant::now();
        assert_eq!(
            next_change_after_elapsed(Duration::from_secs(75), now),
            Some(now + Duration::from_secs(45))
        );
        assert_eq!(
            next_change_after_elapsed(Duration::from_millis(75_500), now),
            Some(now + Duration::from_millis(44_500))
        );
    }

    #[test]
    fn restore_preserves_age_beyond_the_monotonic_clock_range() {
        let now = Instant::now();
        let mut activity = PaneActivity::new(now);
        let persisted_age = Duration::from_secs(u64::MAX);
        assert!(now.checked_sub(persisted_age).is_none());

        activity.restore_unix_timestamp_at(0, now, u64::MAX);

        assert!(activity.inactive_for(now) >= persisted_age);
        assert_eq!(
            activity.deadline_after(Duration::from_secs(30 * 60)),
            Some(now)
        );
    }

    #[test]
    fn restore_preserves_quiet_age_beyond_the_monotonic_clock_range() {
        let now = Instant::now();
        let mut activity = PaneActivity::new(now);
        let persisted_age = Duration::from_secs(u64::MAX);

        activity.restore_unix_timestamp_at(0, now, u64::MAX);
        activity.restore_quiet_unix_timestamp_at(0, now, u64::MAX);

        assert!(activity.quiet_for(now).unwrap() >= persisted_age);
        assert_eq!(
            activity.quiet_deadline_after(Duration::from_secs(30 * 60)),
            Some(now)
        );
    }

    #[test]
    fn quiet_age_uses_the_later_of_activity_and_quiet_transition() {
        let started = Instant::now();
        let mut activity = PaneActivity::new(started);
        activity.observe_quiet(true, started);
        activity.note(started + Duration::from_secs(40));

        assert_eq!(
            activity.quiet_for(started + Duration::from_secs(60)),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            activity.quiet_deadline_after(Duration::from_secs(60)),
            Some(started + Duration::from_secs(100))
        );
    }

    #[test]
    fn detection_output_clock_ignores_non_output_activity() {
        let started = Instant::now();
        let mut activity = PaneActivity::new(started);
        assert!(!activity.observe_detection_snapshot(1, None, "prompt", started));
        activity.note(started + Duration::from_secs(1));
        assert_eq!(activity.detection_output_at(), None);

        let output_at = started + Duration::from_secs(2);
        assert!(activity.observe_detection_snapshot(2, None, "notification", output_at));
        assert_eq!(activity.detection_output_at(), Some(output_at));
    }
}
