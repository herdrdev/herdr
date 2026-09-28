//! Pomodoro timer: a work/break cycle the operator cannot silently ignore.
//!
//! The state here is pure data driven by an injected [`Instant`], so the whole
//! cycle — including the blocking prompt — is testable without a terminal, a
//! client, or a clock that really advances. Everything with a side effect (the
//! break log written next to the notepad notes) lives in the `App` layer.

use std::time::{Duration, Instant};

/// How much text the operator has to type before a due prompt can be dismissed.
/// A bare Enter is what makes a reminder ignorable, so the prompt refuses one.
pub const DEFAULT_MIN_CONFIRM_CHARS: usize = 3;
pub(crate) const SEND_OFF_DURATION: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PomodoroPhase {
    Work,
    ShortBreak,
    LongBreak,
}

impl PomodoroPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Work => "focus",
            Self::ShortBreak => "short break",
            Self::LongBreak => "long break",
        }
    }

    pub fn is_break(self) -> bool {
        matches!(self, Self::ShortBreak | Self::LongBreak)
    }
}

/// The overlay raised when a phase runs out. It owns the confirmation buffer so
/// the operator's answer is part of ordinary app state, not a side channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PomodoroPrompt {
    pub ended: PomodoroPhase,
    pub next: PomodoroPhase,
    pub raised_at: Instant,
    pub input: String,
    /// Set when the operator tried to dismiss the prompt without typing enough.
    pub error: Option<String>,
}

/// The short acknowledgment shown after a prompt is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PomodoroSendOff {
    pub started: PomodoroPhase,
    pub shown_at: Instant,
}

/// What a confirmed prompt hands back for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PomodoroConfirmation {
    pub ended: PomodoroPhase,
    pub started: PomodoroPhase,
    pub note: String,
}

/// The outcome of one tick: whether shared timer lifecycle state changed, and
/// whether the tick is the one that ended a phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PomodoroTick {
    pub changed: bool,
    pub phase_ended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PomodoroState {
    pub enabled: bool,
    pub sidebar_visible: bool,
    pub work: Duration,
    pub short_break: Duration,
    pub long_break: Duration,
    /// Work intervals between long breaks. `4 * 25min` is the ~2h the operator
    /// asked the long break to land on.
    pub long_break_every: u32,
    pub min_confirm_chars: usize,
    pub phase: PomodoroPhase,
    pub completed_work_intervals: u32,
    pub prompt: Option<PomodoroPrompt>,
    pub send_off: Option<PomodoroSendOff>,
    /// The current phase expired while every attached host terminal was unfocused.
    /// It stays at zero until focus returns or the operator changes the timer.
    held: bool,
    /// Set while the phase is counting down. Cleared while paused or prompting,
    /// which is what makes "paused" a single unambiguous fact.
    deadline: Option<Instant>,
    /// Remaining time while not counting down.
    remaining: Duration,
    /// Whole seconds last reported to the renderer. Ticking is cheap and happens
    /// on every server wake, so it only reports a change at 1 Hz.
    rendered_secs: u64,
}

impl Default for PomodoroState {
    fn default() -> Self {
        let work = Duration::from_secs(25 * 60);
        Self {
            enabled: false,
            sidebar_visible: false,
            work,
            short_break: Duration::from_secs(5 * 60),
            long_break: Duration::from_secs(20 * 60),
            long_break_every: 4,
            min_confirm_chars: DEFAULT_MIN_CONFIRM_CHARS,
            phase: PomodoroPhase::Work,
            completed_work_intervals: 0,
            prompt: None,
            send_off: None,
            held: false,
            deadline: None,
            remaining: work,
            rendered_secs: work.as_secs(),
        }
    }
}

impl PomodoroState {
    pub fn from_config(config: &crate::config::PomodoroConfig, now: Instant) -> Self {
        let mut state = Self {
            enabled: config.enabled,
            sidebar_visible: config.sidebar_visible,
            work: minutes(config.work_minutes, 25),
            short_break: minutes(config.short_break_minutes, 5),
            long_break: minutes(config.long_break_minutes, 20),
            long_break_every: config.long_break_every.max(1),
            min_confirm_chars: config.min_confirm_chars,
            ..Self::default()
        };
        state.remaining = state.work;
        state.rendered_secs = state.remaining.as_secs();
        if state.enabled {
            state.start(now);
        }
        state
    }

    /// Applies a live config reload without resetting a cycle that is mid-flight.
    /// Only the durations of phases the operator is not currently inside change
    /// immediately; the running phase keeps its deadline.
    pub fn apply_config(&mut self, config: &crate::config::PomodoroConfig, now: Instant) {
        let was_enabled = self.enabled;
        self.work = minutes(config.work_minutes, 25);
        self.short_break = minutes(config.short_break_minutes, 5);
        self.long_break = minutes(config.long_break_minutes, 20);
        self.long_break_every = config.long_break_every.max(1);
        self.min_confirm_chars = config.min_confirm_chars;
        self.enabled = config.enabled;
        self.sidebar_visible = config.sidebar_visible;
        if !self.enabled {
            self.deadline = None;
            self.prompt = None;
            self.send_off = None;
            self.held = false;
        } else if !was_enabled {
            self.reset(now);
        }
    }

    pub fn running(&self) -> bool {
        self.deadline.is_some()
    }

    pub fn paused(&self) -> bool {
        self.enabled && !self.held && self.prompt.is_none() && self.deadline.is_none()
    }

    pub fn held(&self) -> bool {
        self.enabled && self.held
    }

    pub fn phase_duration(&self, phase: PomodoroPhase) -> Duration {
        match phase {
            PomodoroPhase::Work => self.work,
            PomodoroPhase::ShortBreak => self.short_break,
            PomodoroPhase::LongBreak => self.long_break,
        }
    }

    pub fn remaining_at(&self, now: Instant) -> Duration {
        match self.deadline {
            Some(deadline) => deadline.saturating_duration_since(now),
            None => self.remaining,
        }
    }

    /// `mm:ss`, or `hh:mm:ss` for a phase longer than an hour.
    pub fn label_at(&self, now: Instant) -> String {
        format_remaining(self.remaining_at(now))
    }

    pub fn start(&mut self, now: Instant) {
        if !self.enabled || self.held || self.prompt.is_some() {
            return;
        }
        self.deadline = Some(now + self.remaining);
        self.rendered_secs = self.remaining.as_secs();
    }

    pub fn pause(&mut self, now: Instant) {
        if let Some(deadline) = self.deadline.take() {
            self.remaining = deadline.saturating_duration_since(now);
            self.rendered_secs = self.remaining.as_secs();
        }
    }

    pub fn toggle_pause(&mut self, now: Instant) {
        if !self.enabled || self.held || self.prompt.is_some() {
            return;
        }
        if self.running() {
            self.pause(now);
        } else {
            self.start(now);
        }
    }

    /// Restarts the current phase from the top.
    pub fn reset(&mut self, now: Instant) {
        self.remaining = self.phase_duration(self.phase);
        self.rendered_secs = self.remaining.as_secs();
        self.deadline = None;
        self.prompt = None;
        self.send_off = None;
        self.held = false;
        if self.enabled {
            self.start(now);
        }
    }

    /// Moves to the next phase without raising a prompt. This is the deliberate
    /// operator action ("I am done early"), not the reminder path.
    pub fn skip(&mut self, now: Instant) {
        if !self.enabled {
            return;
        }
        self.prompt = None;
        self.send_off = None;
        self.enter(self.next_phase(), now);
    }

    /// Advances the countdown. Returns whether the frame has to be redrawn and
    /// whether this tick is the one that ended a phase.
    #[cfg(test)]
    pub fn tick(&mut self, now: Instant) -> PomodoroTick {
        self.tick_with_host_focus(now, true)
    }

    /// Advances the countdown, holding an expired phase while no host terminal
    /// is focused. Unknown focus support is resolved by the caller.
    pub fn tick_with_host_focus(&mut self, now: Instant, host_focused: bool) -> PomodoroTick {
        if !self.enabled {
            return PomodoroTick::default();
        }
        let mut tick = PomodoroTick {
            changed: self.expire_send_off_at(now),
            phase_ended: false,
        };
        // The countdown deadline keeps advancing behind the send-off, but its
        // covered timer does not need redraws until the card expires.
        if self.send_off.is_some() {
            return tick;
        }
        if self.held {
            tick.changed |= host_focused && self.resume_held(now);
            return tick;
        }
        let Some(deadline) = self.deadline else {
            return tick;
        };
        if now >= deadline {
            self.deadline = None;
            self.remaining = Duration::ZERO;
            self.rendered_secs = 0;
            if host_focused {
                self.raise_prompt(now);
            } else {
                self.held = true;
            }
            tick.changed = true;
            tick.phase_ended = true;
            return tick;
        }
        let secs = deadline.saturating_duration_since(now).as_secs();
        if secs == self.rendered_secs {
            return tick;
        }
        self.rendered_secs = secs;
        tick.changed = true;
        tick
    }

    /// Resolves a phase that expired while all host terminals were unfocused.
    /// Work still requires the normal confirmation; a finished break starts
    /// the next focus interval immediately.
    pub fn resume_held(&mut self, now: Instant) -> bool {
        if !self.held {
            return false;
        }
        self.held = false;
        if self.phase.is_break() {
            self.enter(PomodoroPhase::Work, now);
        } else {
            self.raise_prompt(now);
        }
        true
    }

    /// Rejects an answer shorter than `min_confirm_chars` so the overlay cannot
    /// be cleared by leaning on Enter.
    pub fn confirm(&mut self, now: Instant) -> Option<PomodoroConfirmation> {
        let prompt = self.prompt.as_mut()?;
        let note = prompt.input.trim().to_string();
        if note.chars().count() < self.min_confirm_chars {
            prompt.error = Some(format!(
                "type at least {} characters to dismiss",
                self.min_confirm_chars
            ));
            return None;
        }
        let ended = prompt.ended;
        self.prompt = None;
        let started = self.next_phase();
        self.enter(started, now);
        self.send_off = Some(PomodoroSendOff {
            started,
            shown_at: now,
        });
        Some(PomodoroConfirmation {
            ended,
            started,
            note,
        })
    }

    /// The escape hatch: drops a due prompt without logging an answer and
    /// leaves the next phase paused. A reminder that fires at the wrong moment
    /// must not be able to lock the operator out of Herdr.
    pub fn dismiss_and_pause(&mut self, now: Instant) {
        if self.prompt.take().is_none() {
            return;
        }
        let next = self.next_phase();
        self.enter(next, now);
        self.pause(now);
    }

    pub fn dismiss_send_off_at(&mut self, now: Instant) -> bool {
        let visible = self
            .send_off
            .is_some_and(|send_off| now < send_off.shown_at + SEND_OFF_DURATION);
        if visible || self.send_off.is_some() {
            self.send_off = None;
        }
        visible
    }

    pub(crate) fn expire_send_off_at(&mut self, now: Instant) -> bool {
        if self
            .send_off
            .is_some_and(|send_off| now >= send_off.shown_at + SEND_OFF_DURATION)
        {
            self.send_off = None;
            true
        } else {
            false
        }
    }

    /// The phase that follows the current one. A long break replaces the short
    /// one every `long_break_every` completed work intervals.
    pub fn next_phase(&self) -> PomodoroPhase {
        match self.phase {
            PomodoroPhase::Work => {
                let completed = self.completed_work_intervals.saturating_add(1);
                if completed.is_multiple_of(self.long_break_every) {
                    PomodoroPhase::LongBreak
                } else {
                    PomodoroPhase::ShortBreak
                }
            }
            PomodoroPhase::ShortBreak | PomodoroPhase::LongBreak => PomodoroPhase::Work,
        }
    }

    fn enter(&mut self, phase: PomodoroPhase, now: Instant) {
        if self.phase == PomodoroPhase::Work {
            self.completed_work_intervals = self.completed_work_intervals.saturating_add(1);
        }
        self.phase = phase;
        self.remaining = self.phase_duration(phase);
        self.rendered_secs = self.remaining.as_secs();
        self.deadline = None;
        self.held = false;
        self.start(now);
    }

    fn raise_prompt(&mut self, now: Instant) {
        self.prompt = Some(PomodoroPrompt {
            ended: self.phase,
            next: self.next_phase(),
            raised_at: now,
            input: String::new(),
            error: None,
        });
    }
}

/// A zero or absurd minute count in configuration falls back to the default
/// rather than producing a phase that ends instantly or never.
fn minutes(value: u64, fallback: u64) -> Duration {
    let minutes = if value == 0 {
        fallback
    } else {
        value.min(24 * 60)
    };
    Duration::from_secs(minutes * 60)
}

pub fn format_remaining(remaining: Duration) -> String {
    let total = remaining.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_state(now: Instant) -> PomodoroState {
        let config = crate::config::PomodoroConfig {
            enabled: true,
            ..Default::default()
        };
        PomodoroState::from_config(&config, now)
    }

    #[test]
    fn disabled_timer_never_ticks() {
        let now = Instant::now();
        let mut state = PomodoroState::default();
        assert_eq!(
            state.tick(now + Duration::from_secs(3600)),
            PomodoroTick::default()
        );
        assert!(state.prompt.is_none());
    }

    #[test]
    fn expiring_work_phase_raises_a_prompt_and_stops_the_countdown() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        let tick = state.tick(now + Duration::from_secs(25 * 60));
        assert!(tick.phase_ended);
        let prompt = state.prompt.as_ref().expect("prompt raised");
        assert_eq!(prompt.ended, PomodoroPhase::Work);
        assert_eq!(prompt.next, PomodoroPhase::ShortBreak);
        assert!(!state.running());
    }

    #[test]
    fn f1_unfocused_work_expiry_is_held_without_a_prompt() {
        let now = Instant::now();
        let mut state = enabled_state(now);

        let tick = state.tick_with_host_focus(now + Duration::from_secs(25 * 60), false);

        assert_eq!(
            tick,
            PomodoroTick {
                changed: true,
                phase_ended: true,
            }
        );
        assert!(state.held());
        assert!(state.prompt.is_none());
        assert!(!state.paused());
        assert_eq!(state.remaining_at(now), Duration::ZERO);
    }

    #[test]
    fn f3_focus_return_raises_work_prompt_but_starts_focus_after_break() {
        let now = Instant::now();
        let mut work = enabled_state(now);
        work.tick_with_host_focus(now + Duration::from_secs(25 * 60), false);
        assert!(work.resume_held(now));
        assert!(!work.held());
        let prompt = work.prompt.as_ref().expect("work reminder raised");
        assert_eq!(prompt.ended, PomodoroPhase::Work);
        assert_eq!(prompt.next, PomodoroPhase::ShortBreak);

        let mut break_state = enabled_state(now);
        break_state.skip(now);
        break_state.tick_with_host_focus(now + Duration::from_secs(5 * 60), false);
        assert!(break_state.held());
        assert!(break_state.resume_held(now));
        assert_eq!(break_state.phase, PomodoroPhase::Work);
        assert!(break_state.prompt.is_none());
        assert!(break_state.running());
    }

    #[test]
    fn f4_focus_loss_keeps_an_existing_prompt() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        state.tick(now + Duration::from_secs(25 * 60));
        let prompt = state.prompt.clone();

        let tick = state.tick_with_host_focus(now + Duration::from_secs(60 * 60), false);
        assert!(!tick.changed, "the timer state did not change");
        assert!(!tick.phase_ended);
        assert_eq!(state.prompt, prompt);
        assert!(!state.held());
    }

    #[test]
    fn f4_held_phase_respects_pause_skip_reset_and_config_reload() {
        let now = Instant::now();
        let mut held = enabled_state(now);
        held.tick_with_host_focus(now + Duration::from_secs(25 * 60), false);

        held.toggle_pause(now);
        assert!(held.held());
        assert!(!held.running());

        held.apply_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                short_break_minutes: 9,
                ..Default::default()
            },
            now,
        );
        assert!(held.held());

        let mut disabled = held.clone();
        disabled.apply_config(&crate::config::PomodoroConfig::default(), now);
        assert!(!disabled.enabled);
        assert!(!disabled.held());
        assert!(disabled.prompt.is_none());

        let mut skipped = held.clone();
        skipped.skip(now);
        assert_eq!(skipped.phase, PomodoroPhase::ShortBreak);
        assert_eq!(skipped.remaining_at(now), Duration::from_secs(9 * 60));
        assert!(skipped.running());
        assert!(!skipped.held());

        held.reset(now);
        assert_eq!(held.phase, PomodoroPhase::Work);
        assert_eq!(held.remaining_at(now), Duration::from_secs(25 * 60));
        assert!(held.running());
        assert!(!held.held());
    }

    #[test]
    fn a_short_answer_cannot_dismiss_the_prompt() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        state.tick(now + Duration::from_secs(25 * 60));
        if let Some(prompt) = state.prompt.as_mut() {
            prompt.input = "ok".into();
        }
        assert!(state.confirm(now).is_none());
        assert!(state.prompt.is_some());
        assert!(state
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.error.is_some()));
    }

    #[test]
    fn confirming_starts_the_break_and_reports_the_note() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        state.tick(now + Duration::from_secs(25 * 60));
        if let Some(prompt) = state.prompt.as_mut() {
            prompt.input = "  stretching  ".into();
        }
        let confirmation = state.confirm(now).expect("confirmed");
        assert_eq!(confirmation.note, "stretching");
        assert_eq!(confirmation.ended, PomodoroPhase::Work);
        assert_eq!(confirmation.started, PomodoroPhase::ShortBreak);
        assert_eq!(state.phase, PomodoroPhase::ShortBreak);
        assert_eq!(state.completed_work_intervals, 1);
        assert!(state.running());
        assert_eq!(state.remaining_at(now), Duration::from_secs(5 * 60));
        let send_off = state.send_off.expect("confirm raises the send-off");
        assert_eq!(send_off.started, PomodoroPhase::ShortBreak);
        assert_eq!(send_off.shown_at, now);
    }

    #[test]
    fn send_off_expires_after_six_seconds_without_blocking_the_timer() {
        let started = Instant::now();
        let prompt_at = started + Duration::from_secs(25 * 60);
        let mut state = enabled_state(started);
        state.tick(prompt_at);
        state.prompt.as_mut().expect("prompt").input = "water".into();
        state.confirm(prompt_at).expect("confirm");

        assert!(
            !state
                .tick(prompt_at + SEND_OFF_DURATION - Duration::from_millis(1))
                .changed
        );
        assert!(state.send_off.is_some());
        assert!(state.tick(prompt_at + SEND_OFF_DURATION).changed);
        assert!(state.send_off.is_none());
        assert!(state.running());
    }

    #[test]
    fn send_off_can_be_dismissed_early() {
        let started = Instant::now();
        let prompt_at = started + Duration::from_secs(25 * 60);
        let mut state = enabled_state(started);
        state.tick(prompt_at);
        state.prompt.as_mut().expect("prompt").input = "walk".into();
        state.confirm(prompt_at).expect("confirm");

        assert!(state.dismiss_send_off_at(prompt_at));
        assert!(state.send_off.is_none());
        assert!(!state.dismiss_send_off_at(prompt_at));
    }

    #[test]
    fn every_fourth_work_interval_ends_in_a_long_break() {
        let mut now = Instant::now();
        let mut state = enabled_state(now);
        let mut breaks = Vec::new();
        for _ in 0..4 {
            now += Duration::from_secs(25 * 60);
            state.tick(now);
            if let Some(prompt) = state.prompt.as_mut() {
                prompt.input = "noted".into();
            }
            let confirmation = state.confirm(now).expect("work confirmed");
            breaks.push(confirmation.started);
            now += state.phase_duration(state.phase);
            state.tick(now);
            if let Some(prompt) = state.prompt.as_mut() {
                prompt.input = "noted".into();
            }
            state.confirm(now).expect("break confirmed");
        }
        assert_eq!(
            breaks,
            vec![
                PomodoroPhase::ShortBreak,
                PomodoroPhase::ShortBreak,
                PomodoroPhase::ShortBreak,
                PomodoroPhase::LongBreak,
            ]
        );
    }

    #[test]
    fn pausing_freezes_the_remaining_time() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        state.toggle_pause(now + Duration::from_secs(60));
        assert!(state.paused());
        let frozen = state.remaining_at(now + Duration::from_secs(600));
        assert_eq!(frozen, Duration::from_secs(24 * 60));
        state.toggle_pause(now + Duration::from_secs(600));
        assert!(state.running());
        assert_eq!(
            state.remaining_at(now + Duration::from_secs(600)),
            Duration::from_secs(24 * 60)
        );
    }

    #[test]
    fn ticking_reports_a_change_only_once_per_second() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        // The label crosses into 24:59 as soon as the first second is not whole.
        assert!(state.tick(now + Duration::from_millis(100)).changed);
        assert!(!state.tick(now + Duration::from_millis(200)).changed);
        assert!(!state.tick(now + Duration::from_millis(999)).changed);
        assert!(state.tick(now + Duration::from_millis(1100)).changed);
        assert!(!state.tick(now + Duration::from_millis(1200)).changed);
    }

    #[test]
    fn skipping_advances_without_a_prompt() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        state.skip(now);
        assert!(state.prompt.is_none());
        assert_eq!(state.phase, PomodoroPhase::ShortBreak);
        assert!(state.running());
    }

    #[test]
    fn a_prompt_can_be_dismissed_without_an_answer_into_a_paused_next_phase() {
        let now = Instant::now();
        let mut state = enabled_state(now);
        state.tick(now + Duration::from_secs(25 * 60));
        state.dismiss_and_pause(now);
        assert!(state.prompt.is_none());
        assert_eq!(state.phase, PomodoroPhase::ShortBreak);
        assert!(state.paused());
        assert_eq!(state.remaining_at(now), Duration::from_secs(5 * 60));
    }

    #[test]
    fn remaining_is_formatted_as_minutes_and_seconds() {
        assert_eq!(format_remaining(Duration::from_secs(65)), "1:05");
        assert_eq!(format_remaining(Duration::from_secs(0)), "0:00");
        assert_eq!(format_remaining(Duration::from_secs(3725)), "1:02:05");
    }
}
