use std::sync::mpsc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossterm::event::{KeyModifiers, MouseEventKind};
use tracing::debug;

use crate::api::schema::{PaneReadResult, ResponseResult, SuccessResponse};
use crate::terminal::{ScreenSnapshot, TerminalId, TerminalRuntime, UpwardMerge};

const STEP_SETTLE: Duration = Duration::from_millis(120);
const MAX_DURATION: Duration = Duration::from_secs(15);
const MAX_RESTORE_DURATION: Duration = Duration::from_secs(5);
const MAX_UNALIGNED_CHECKS: u8 = 4;
const WHEEL_STEP_EVENTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    SettleInitial { checks: u8 },
    ProbeBottom,
    RestoreProbe,
    Harvest { unaligned_checks: u8 },
    Restore { stable_checks: u8 },
}

pub(crate) struct PendingAltScreenRead {
    pub(crate) terminal_id: TerminalId,
    request_id: String,
    respond_to: mpsc::Sender<String>,
    fallback_response: String,
    read: PaneReadResult,
    lines: usize,
    unwrap: bool,
    initial: ScreenSnapshot,
    previous: ScreenSnapshot,
    history: Vec<crate::ghostty::ScreenTextRow>,
    phase: Phase,
    next_poll_at: Instant,
    started_at: Instant,
    restore_started_at: Option<Instant>,
    upward_events: usize,
    reached_top: bool,
    valid: bool,
}

impl PendingAltScreenRead {
    pub(crate) fn start(
        terminal_id: TerminalId,
        request_id: String,
        respond_to: mpsc::Sender<String>,
        fallback_response: String,
        read: PaneReadResult,
        lines: usize,
        unwrap: bool,
        initial: ScreenSnapshot,
        now: Instant,
    ) -> Self {
        Self {
            terminal_id,
            request_id,
            respond_to,
            fallback_response,
            read,
            lines,
            unwrap,
            previous: initial.clone(),
            history: initial.rows.clone(),
            initial,
            phase: Phase::SettleInitial { checks: 0 },
            next_poll_at: now + STEP_SETTLE,
            started_at: now,
            restore_started_at: None,
            upward_events: 0,
            reached_top: false,
            valid: true,
        }
    }

    pub(crate) fn next_deadline(&self) -> Instant {
        self.next_poll_at
    }

    pub(crate) fn frozen_snapshot(
        &self,
        source: crate::api::schema::ReadSource,
        lines: Option<u32>,
    ) -> crate::pane::TerminalReadSnapshot {
        let line_limit = lines.map(|lines| lines.min(1000) as usize);
        match source {
            crate::api::schema::ReadSource::Recent
            | crate::api::schema::ReadSource::RecentUnwrapped => {
                let limit = line_limit.unwrap_or(80);
                crate::terminal::snapshot_text(
                    &self.initial.rows,
                    limit,
                    source == crate::api::schema::ReadSource::RecentUnwrapped,
                    self.initial.rows.len() > limit,
                )
            }
            crate::api::schema::ReadSource::Visible | crate::api::schema::ReadSource::Detection => {
                let snapshot = crate::terminal::snapshot_text(
                    &self.initial.rows,
                    self.initial.rows.len(),
                    false,
                    false,
                );
                crate::app::limit_snapshot_lines(snapshot.text, line_limit)
            }
        }
    }

    pub(crate) fn abort(mut self, runtime: Option<&TerminalRuntime>, now: Instant) -> PollOutcome {
        self.valid = false;
        match self.phase {
            Phase::SettleInitial { .. } => self.complete_fallback(),
            Phase::Harvest { .. } => match runtime {
                Some(runtime) => self.start_restore(runtime, now),
                None => self.complete_fallback(),
            },
            Phase::ProbeBottom | Phase::RestoreProbe | Phase::Restore { .. } => {
                self.poll(runtime, now)
            }
        }
    }

    pub(crate) fn poll(mut self, runtime: Option<&TerminalRuntime>, now: Instant) -> PollOutcome {
        if now < self.next_poll_at {
            return Some(self);
        }
        let Some(runtime) = runtime else {
            return self.complete_fallback();
        };
        let Some((screen, snapshot)) = runtime.screen_text_snapshot() else {
            return self.complete_fallback();
        };
        if screen != crate::ghostty::ActiveScreen::Alternate
            || snapshot.cols != self.initial.cols
            || snapshot.rows.len() != self.initial.rows.len()
        {
            return self.complete_fallback();
        }
        if self
            .restore_started_at
            .is_some_and(|started| now.duration_since(started) >= MAX_RESTORE_DURATION)
        {
            return self.complete_fallback();
        }
        if now.duration_since(self.started_at) >= MAX_DURATION
            && !matches!(
                self.phase,
                Phase::ProbeBottom | Phase::Restore { .. } | Phase::RestoreProbe
            )
        {
            self.valid = false;
            return self.start_restore(runtime, now);
        }

        match self.phase {
            Phase::SettleInitial { checks } => {
                if snapshot.similar_text(&self.initial) {
                    if checks >= 1 {
                        if send_wheel(
                            runtime,
                            MouseEventKind::ScrollDown,
                            WHEEL_STEP_EVENTS,
                            &snapshot,
                        )
                        .is_err()
                        {
                            return self.complete_fallback();
                        }
                        self.phase = Phase::ProbeBottom;
                    } else {
                        self.phase = Phase::SettleInitial { checks: checks + 1 };
                    }
                } else {
                    self.initial = snapshot.clone();
                    self.previous = snapshot.clone();
                    self.history = snapshot.rows;
                    self.phase = Phase::SettleInitial { checks: 0 };
                }
                self.next_poll_at = now + STEP_SETTLE;
                Some(self)
            }
            Phase::ProbeBottom => {
                debug!(
                    terminal_id = %self.terminal_id,
                    at_bottom = snapshot.similar_text(&self.initial),
                    "alternate-screen read bottom probe settled"
                );
                if snapshot.similar_text(&self.initial) {
                    if self.valid {
                        self.start_harvest(runtime, now)
                    } else {
                        self.complete_fallback()
                    }
                } else if send_wheel(
                    runtime,
                    MouseEventKind::ScrollUp,
                    WHEEL_STEP_EVENTS,
                    &snapshot,
                )
                .is_ok()
                {
                    self.phase = Phase::RestoreProbe;
                    self.restore_started_at = Some(now);
                    self.next_poll_at = now + STEP_SETTLE;
                    Some(self)
                } else {
                    self.complete_fallback()
                }
            }
            Phase::RestoreProbe => {
                if snapshot.similar_text(&self.initial) {
                    self.complete_fallback()
                } else {
                    self.next_poll_at = now + STEP_SETTLE;
                    Some(self)
                }
            }
            Phase::Harvest { unaligned_checks } => {
                let merge = crate::terminal::merge_scrolled_up(
                    &mut self.history,
                    &self.previous,
                    &snapshot,
                );
                match merge {
                    UpwardMerge::Advanced { .. } => {
                        self.previous = snapshot;
                        if self.history.len() >= self.lines {
                            self.start_restore(runtime, now)
                        } else {
                            self.start_harvest(runtime, now)
                        }
                    }
                    UpwardMerge::Unchanged => {
                        self.reached_top = true;
                        self.start_restore(runtime, now)
                    }
                    UpwardMerge::Unaligned if unaligned_checks + 1 < MAX_UNALIGNED_CHECKS => {
                        self.phase = Phase::Harvest {
                            unaligned_checks: unaligned_checks + 1,
                        };
                        self.next_poll_at = now + STEP_SETTLE;
                        Some(self)
                    }
                    UpwardMerge::Unaligned => {
                        self.valid = false;
                        self.start_restore(runtime, now)
                    }
                }
            }
            Phase::Restore { stable_checks } => {
                if snapshot.similar_text(&self.previous) {
                    if stable_checks >= 1 {
                        if self.valid {
                            self.complete_success()
                        } else {
                            self.complete_fallback()
                        }
                    } else {
                        self.phase = Phase::Restore {
                            stable_checks: stable_checks + 1,
                        };
                        self.next_poll_at = now + STEP_SETTLE;
                        Some(self)
                    }
                } else {
                    self.previous = snapshot;
                    if send_wheel(
                        runtime,
                        MouseEventKind::ScrollDown,
                        restore_batch_size(&self.previous),
                        &self.previous,
                    )
                    .is_ok()
                    {
                        self.phase = Phase::Restore { stable_checks: 0 };
                        self.next_poll_at = now + STEP_SETTLE;
                        Some(self)
                    } else {
                        self.complete_fallback()
                    }
                }
            }
        }
    }

    fn start_harvest(mut self, runtime: &TerminalRuntime, now: Instant) -> PollOutcome {
        let events = WHEEL_STEP_EVENTS;
        if send_wheel(runtime, MouseEventKind::ScrollUp, events, &self.previous).is_err() {
            return self.complete_fallback();
        }
        self.upward_events = self.upward_events.saturating_add(events);
        self.phase = Phase::Harvest {
            unaligned_checks: 0,
        };
        self.next_poll_at = now + STEP_SETTLE;
        Some(self)
    }

    fn start_restore(mut self, runtime: &TerminalRuntime, now: Instant) -> PollOutcome {
        if self.upward_events == 0 {
            return self.complete_fallback();
        }
        if send_wheel(
            runtime,
            MouseEventKind::ScrollDown,
            self.upward_events,
            &self.previous,
        )
        .is_err()
        {
            return self.complete_fallback();
        }
        self.phase = Phase::Restore { stable_checks: 0 };
        self.restore_started_at = Some(now);
        self.next_poll_at = now + STEP_SETTLE;
        Some(self)
    }

    fn complete_success(mut self) -> PollOutcome {
        debug!(
            terminal_id = %self.terminal_id,
            retained_rows = self.history.len(),
            requested_rows = self.lines,
            reached_top = self.reached_top,
            "alternate-screen read completed"
        );
        let truncated = !self.reached_top || self.history.len() > self.lines;
        let snapshot =
            crate::terminal::snapshot_text(&self.history, self.lines, self.unwrap, truncated);
        self.read.text = snapshot.text;
        self.read.truncated = snapshot.truncated;
        let response = serde_json::to_string(&SuccessResponse {
            id: self.request_id,
            result: ResponseResult::PaneRead { read: self.read },
        })
        .unwrap_or(self.fallback_response);
        let _ = self.respond_to.send(response);
        None
    }

    fn complete_fallback(self) -> PollOutcome {
        debug!(
            terminal_id = %self.terminal_id,
            ?self.phase,
            retained_rows = self.history.len(),
            upward_events = self.upward_events,
            valid = self.valid,
            "alternate-screen read fell back to passive snapshot"
        );
        let _ = self.respond_to.send(self.fallback_response);
        None
    }
}

pub(crate) type PollOutcome = Option<PendingAltScreenRead>;

fn restore_batch_size(snapshot: &ScreenSnapshot) -> usize {
    snapshot.rows.len().saturating_div(2).max(1)
}

fn send_wheel(
    runtime: &TerminalRuntime,
    kind: MouseEventKind,
    events: usize,
    snapshot: &ScreenSnapshot,
) -> Result<(), ()> {
    if runtime.wheel_routing() != Some(crate::pane::WheelRouting::MouseReport) {
        return Err(());
    }
    let column = snapshot.cols.saturating_sub(1) / 2;
    let row = u16::try_from(snapshot.rows.len().saturating_sub(1) / 2).unwrap_or(0);
    let event = runtime
        .encode_mouse_wheel(
            kind,
            crate::input::mouse::Position::Cell { column, row },
            KeyModifiers::empty(),
        )
        .ok_or(())?;
    let mut bytes = Vec::with_capacity(event.len().saturating_mul(events));
    for _ in 0..events {
        bytes.extend_from_slice(&event);
    }
    runtime.try_send_bytes(Bytes::from(bytes)).map_err(|_| ())
}
