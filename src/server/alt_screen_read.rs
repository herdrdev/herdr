use std::sync::mpsc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossterm::event::{KeyModifiers, MouseEventKind};
use tracing::debug;

use crate::api::schema::{PaneReadResult, ResponseResult, SuccessResponse};
use crate::terminal::{ScreenSnapshot, TerminalId, TerminalRuntime, UpwardMerge};

const INITIAL_QUIET: Duration = Duration::from_millis(10);
const OUTPUT_QUIET: Duration = Duration::from_millis(10);
const STEP_TIMEOUT: Duration = Duration::from_millis(120);
const MAX_DURATION: Duration = Duration::from_secs(15);
const MAX_RESTORE_DURATION: Duration = Duration::from_secs(5);
const WHEEL_STEP_EVENTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    SettleInitial,
    ProbeBottom,
    RestoreProbe,
    Harvest,
    Restore,
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
    step_deadline: Instant,
    output_quiet_until: Option<Instant>,
    step_observed_output: bool,
    synchronized_redraw_pending: bool,
    started_at: Instant,
    restore_started_at: Option<Instant>,
    observed_content_seq: u64,
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
        content_seq: u64,
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
            phase: Phase::SettleInitial,
            next_poll_at: now + INITIAL_QUIET,
            step_deadline: now + INITIAL_QUIET,
            output_quiet_until: None,
            step_observed_output: false,
            synchronized_redraw_pending: false,
            started_at: now,
            restore_started_at: None,
            observed_content_seq: content_seq,
            upward_events: 0,
            reached_top: false,
            valid: true,
        }
    }

    pub(crate) fn next_deadline(&self) -> Instant {
        self.output_quiet_until.unwrap_or(self.next_poll_at)
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
            Phase::SettleInitial => self.complete_fallback(),
            Phase::Harvest => match runtime {
                Some(runtime) => self.start_restore(runtime, now, None),
                None => self.complete_fallback(),
            },
            Phase::ProbeBottom | Phase::RestoreProbe | Phase::Restore => self.poll(runtime, now),
        }
    }

    pub(crate) fn poll(mut self, runtime: Option<&TerminalRuntime>, now: Instant) -> PollOutcome {
        let Some(runtime) = runtime else {
            return self.complete_fallback();
        };
        let restore_expired = self
            .restore_started_at
            .is_some_and(|started| now.duration_since(started) >= MAX_RESTORE_DURATION);
        if restore_expired {
            return self.complete_fallback();
        }
        let traversal_expired = now.duration_since(self.started_at) >= MAX_DURATION
            && matches!(
                self.phase,
                Phase::SettleInitial | Phase::ProbeBottom | Phase::Harvest
            );
        if traversal_expired {
            self.valid = false;
            match self.phase {
                Phase::SettleInitial => return self.complete_fallback(),
                Phase::Harvest => return self.start_restore(runtime, now, None),
                Phase::ProbeBottom => {}
                Phase::RestoreProbe | Phase::Restore => unreachable!(),
            }
        }

        let content_seq = runtime.content_seq();
        if content_seq != self.observed_content_seq {
            self.observed_content_seq = content_seq;
            self.step_observed_output = true;
            let synchronized_frame_complete =
                self.synchronized_redraw_pending && !runtime.synchronized_output_active();
            if synchronized_frame_complete {
                self.synchronized_redraw_pending = false;
                self.output_quiet_until = None;
            } else {
                self.output_quiet_until = Some(now + OUTPUT_QUIET);
                if !traversal_expired {
                    return Some(self);
                }
            }
        }
        if !traversal_expired && self.output_quiet_until.is_some_and(|quiet| now < quiet) {
            return Some(self);
        }
        self.output_quiet_until = None;
        if !traversal_expired && runtime.synchronized_output_active() {
            self.synchronized_redraw_pending = true;
            self.next_poll_at = now + OUTPUT_QUIET;
            return Some(self);
        }
        let step_expired = now >= self.step_deadline;
        let output_observed = self.step_observed_output;
        if !step_expired && !output_observed {
            return Some(self);
        }
        let Some((screen, snapshot, snapshot_seq)) = runtime.screen_text_snapshot_with_seq() else {
            if traversal_expired {
                return self.complete_fallback();
            }
            self.observed_content_seq = runtime.content_seq();
            self.step_observed_output = false;
            self.output_quiet_until = None;
            self.next_poll_at = now + OUTPUT_QUIET;
            return Some(self);
        };
        if runtime.content_seq() != snapshot_seq {
            self.observed_content_seq = snapshot_seq;
            self.step_observed_output = false;
            self.next_poll_at = now + OUTPUT_QUIET;
            return Some(self);
        }
        if screen != crate::ghostty::ActiveScreen::Alternate
            || snapshot.cols != self.initial.cols
            || snapshot.rows.len() != self.initial.rows.len()
        {
            return self.complete_fallback();
        }
        match self.phase {
            Phase::SettleInitial => {
                if output_observed || !snapshot.similar_text(&self.initial) {
                    self.initial = snapshot.clone();
                    self.previous = snapshot.clone();
                    self.history = snapshot.rows;
                    self.observed_content_seq = snapshot_seq;
                    self.step_observed_output = false;
                    self.next_poll_at = now + INITIAL_QUIET;
                    self.step_deadline = self.next_poll_at;
                    return Some(self);
                }
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
                self.arm_step(snapshot_seq, now);
                Some(self)
            }
            Phase::ProbeBottom => {
                let at_bottom = snapshot.similar_text(&self.initial);
                if output_observed && at_bottom && !step_expired && !traversal_expired {
                    self.step_observed_output = false;
                    return Some(self);
                }
                debug!(
                    terminal_id = %self.terminal_id,
                    at_bottom,
                    "alternate-screen read bottom probe settled"
                );
                if at_bottom {
                    if self.valid {
                        self.start_harvest(runtime, now, snapshot_seq)
                    } else {
                        self.complete_fallback()
                    }
                } else {
                    if send_wheel(
                        runtime,
                        MouseEventKind::ScrollUp,
                        WHEEL_STEP_EVENTS,
                        &snapshot,
                    )
                    .is_err()
                    {
                        return self.complete_fallback();
                    }
                    self.phase = Phase::RestoreProbe;
                    self.restore_started_at = Some(now);
                    self.arm_step(snapshot_seq, now);
                    Some(self)
                }
            }
            Phase::RestoreProbe => {
                if snapshot.similar_text(&self.initial) {
                    self.complete_fallback()
                } else {
                    self.step_observed_output = false;
                    if step_expired {
                        self.next_poll_at = now + STEP_TIMEOUT;
                        self.step_deadline = self.next_poll_at;
                    }
                    Some(self)
                }
            }
            Phase::Harvest => {
                let merge = crate::terminal::merge_scrolled_up(
                    &mut self.history,
                    &self.previous,
                    &snapshot,
                );
                debug!(
                    terminal_id = %self.terminal_id,
                    ?merge,
                    retained_rows = self.history.len(),
                    batch_events = WHEEL_STEP_EVENTS,
                    step_expired,
                    "alternate-screen harvest snapshot"
                );
                match merge {
                    UpwardMerge::Advanced { .. } => {
                        self.previous = snapshot;
                        if self.history.len() >= self.lines {
                            self.start_restore(runtime, now, Some(snapshot_seq))
                        } else {
                            self.start_harvest(runtime, now, snapshot_seq)
                        }
                    }
                    UpwardMerge::Unchanged if step_expired => {
                        self.reached_top = true;
                        self.start_restore(runtime, now, Some(snapshot_seq))
                    }
                    UpwardMerge::Unaligned if step_expired => {
                        self.valid = false;
                        self.start_restore(runtime, now, Some(snapshot_seq))
                    }
                    UpwardMerge::Unchanged | UpwardMerge::Unaligned => {
                        self.step_observed_output = false;
                        Some(self)
                    }
                }
            }
            Phase::Restore => {
                if snapshot.similar_text(&self.initial) {
                    if self.valid {
                        self.complete_success()
                    } else {
                        self.complete_fallback()
                    }
                } else if step_expired || !snapshot.similar_text(&self.previous) {
                    if send_wheel(
                        runtime,
                        MouseEventKind::ScrollDown,
                        restore_batch_size(&snapshot),
                        &snapshot,
                    )
                    .is_err()
                    {
                        return self.complete_fallback();
                    }
                    self.previous = snapshot;
                    self.arm_step(snapshot_seq, now);
                    Some(self)
                } else {
                    self.step_observed_output = false;
                    Some(self)
                }
            }
        }
    }

    fn start_harvest(
        mut self,
        runtime: &TerminalRuntime,
        now: Instant,
        baseline_seq: u64,
    ) -> PollOutcome {
        let events = WHEEL_STEP_EVENTS;
        if send_wheel(runtime, MouseEventKind::ScrollUp, events, &self.previous).is_err() {
            return self.complete_fallback();
        }
        self.upward_events = self.upward_events.saturating_add(events);
        self.phase = Phase::Harvest;
        self.arm_step(baseline_seq, now);
        Some(self)
    }

    fn start_restore(
        mut self,
        runtime: &TerminalRuntime,
        now: Instant,
        baseline_seq: Option<u64>,
    ) -> PollOutcome {
        if self.upward_events == 0 {
            return self.complete_fallback();
        }
        let baseline_seq = baseline_seq.unwrap_or_else(|| runtime.content_seq());
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
        self.phase = Phase::Restore;
        self.restore_started_at = Some(now);
        self.arm_step(baseline_seq, now);
        Some(self)
    }

    fn arm_step(&mut self, baseline_seq: u64, now: Instant) {
        self.observed_content_seq = baseline_seq;
        self.output_quiet_until = None;
        self.step_observed_output = false;
        self.synchronized_redraw_pending = false;
        self.next_poll_at = now + STEP_TIMEOUT;
        self.step_deadline = self.next_poll_at;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ReadFormat, ReadSource};

    fn draw(lines: &[&str], enter_alt_screen: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if enter_alt_screen {
            bytes.extend_from_slice(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        }
        bytes.extend_from_slice(b"\x1b[2J\x1b[H");
        bytes.extend_from_slice(lines.join("\r\n").as_bytes());
        bytes
    }

    fn pending_read(
        runtime: &TerminalRuntime,
        now: Instant,
        lines: usize,
    ) -> (PendingAltScreenRead, mpsc::Receiver<String>) {
        let (_, initial) = runtime.screen_text_snapshot().expect("initial snapshot");
        let (respond_to, response_rx) = mpsc::channel();
        let pending = PendingAltScreenRead::start(
            TerminalId::alloc(),
            "read".into(),
            respond_to,
            "fallback".into(),
            PaneReadResult {
                pane_id: "w1:p1".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                source: ReadSource::Recent,
                format: ReadFormat::Text,
                text: String::new(),
                revision: 0,
                truncated: false,
            },
            lines,
            false,
            initial,
            runtime.content_seq(),
            now,
        );
        (pending, response_rx)
    }

    fn response_text(response_rx: &mpsc::Receiver<String>) -> String {
        let response: SuccessResponse = serde_json::from_str(
            &response_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("read response"),
        )
        .expect("valid response");
        let ResponseResult::PaneRead { read } = response.result else {
            panic!("expected pane read response");
        };
        read.text
    }

    #[test]
    fn hard_deadline_wins_over_continuous_output_coalescing() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let (runtime, _input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&draw(&["16", "17", "18", "19", "20"], true));
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);

        runtime.test_process_pty_bytes(b"\x1b]0;still changing\x07");
        assert!(pending
            .poll(Some(&runtime), started + MAX_DURATION)
            .is_none());
        assert_eq!(
            response_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("fallback response"),
            "fallback"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn probe_bottom_hard_deadline_wins_over_continuous_output() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&draw(&["16", "17", "18", "19", "20"], true));
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);
        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");

        runtime.test_process_pty_bytes(b"\x1b]0;still changing\x07");
        assert!(pending
            .poll(Some(&runtime), started + MAX_DURATION)
            .is_none());
        assert_eq!(
            response_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("fallback response"),
            "fallback"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn probe_bottom_hard_deadline_wins_over_synchronized_output() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&draw(&["16", "17", "18", "19", "20"], true));
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);
        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");

        runtime.test_process_pty_bytes(b"\x1b[?2026h");
        assert!(pending
            .poll(Some(&runtime), started + MAX_DURATION)
            .is_none());
        assert_eq!(
            response_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("fallback response"),
            "fallback"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn redraw_events_advance_harvest_and_restore_without_settle_delays() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let initial_bytes = draw(&["16", "17", "18", "19", "20"], true);
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&initial_bytes);
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);

        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");
        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET + STEP_TIMEOUT)
            .expect("history harvest");
        input_rx.try_recv().expect("upward wheel batch");

        runtime.test_process_pty_bytes(&draw(&["13", "14", "15", "16", "17"], false));
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(1),
            )
            .expect("redraw coalescing");
        assert!(input_rx.try_recv().is_err());
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(11),
            )
            .expect("viewport restore");
        input_rx.try_recv().expect("restore wheel batch");

        runtime.test_process_pty_bytes(&draw(&["16", "17", "18", "19", "20"], false));
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(12),
            )
            .expect("restore redraw coalescing");
        assert!(
            pending
                .poll(
                    Some(&runtime),
                    started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(22),
                )
                .is_none(),
            "restored redraw should complete after coalescing"
        );
        assert_eq!(
            response_text(&response_rx),
            "13\n14\n15\n16\n17\n18\n19\n20\n"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn restore_keeps_trying_after_a_slow_redraw_exceeds_the_step_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&draw(&["16", "17", "18", "19", "20"], true));
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);
        let harvest_started = started + INITIAL_QUIET + STEP_TIMEOUT;

        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");
        let pending = pending
            .poll(Some(&runtime), harvest_started)
            .expect("history harvest");
        input_rx.try_recv().expect("upward wheel batch");
        runtime.test_process_pty_bytes(&draw(&["13", "14", "15", "16", "17"], false));
        let pending = pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(1))
            .expect("redraw coalescing");
        let restore_started = harvest_started + Duration::from_millis(11);
        let pending = pending
            .poll(Some(&runtime), restore_started)
            .expect("viewport restore");
        input_rx.try_recv().expect("restore wheel batch");

        let retry_at = restore_started + STEP_TIMEOUT;
        let pending = pending
            .poll(Some(&runtime), retry_at)
            .expect("slow restore must remain pending");
        input_rx.try_recv().expect("retry restore wheel batch");

        runtime.test_process_pty_bytes(&draw(&["16", "17", "18", "19", "20"], false));
        let pending = pending
            .poll(Some(&runtime), retry_at + Duration::from_millis(1))
            .expect("restore redraw coalescing");
        assert!(pending
            .poll(Some(&runtime), retry_at + Duration::from_millis(11))
            .is_none());
        assert_eq!(
            response_text(&response_rx),
            "13\n14\n15\n16\n17\n18\n19\n20\n"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn unrelated_output_does_not_retry_restore_before_the_step_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let initial = ["16", "17", "18", "19", "20"];
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&draw(&initial, true));
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);
        let harvest_started = started + INITIAL_QUIET + STEP_TIMEOUT;

        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");
        let pending = pending
            .poll(Some(&runtime), harvest_started)
            .expect("history harvest");
        input_rx.try_recv().expect("upward wheel batch");
        runtime.test_process_pty_bytes(&draw(&["13", "14", "15", "16", "17"], false));
        let pending = pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(1))
            .expect("redraw coalescing");
        let restore_started = harvest_started + Duration::from_millis(11);
        let pending = pending
            .poll(Some(&runtime), restore_started)
            .expect("viewport restore");
        input_rx.try_recv().expect("restore wheel batch");

        runtime.test_process_pty_bytes(b"\x1b]0;unrelated title\x07");
        let pending = pending
            .poll(Some(&runtime), restore_started + Duration::from_millis(1))
            .expect("unrelated output coalescing");
        let pending = pending
            .poll(Some(&runtime), restore_started + Duration::from_millis(11))
            .expect("restore remains pending");
        assert!(input_rx.try_recv().is_err());

        runtime.test_process_pty_bytes(&draw(&initial, false));
        let pending = pending
            .poll(Some(&runtime), restore_started + Duration::from_millis(12))
            .expect("restore redraw coalescing");
        assert!(pending
            .poll(Some(&runtime), restore_started + Duration::from_millis(22))
            .is_none());
        assert_eq!(
            response_text(&response_rx),
            "13\n14\n15\n16\n17\n18\n19\n20\n"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn synchronized_redraw_is_not_consumed_after_the_step_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let initial = ["16", "17", "18", "19", "20"];
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&draw(&initial, true));
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);
        let harvest_started = started + INITIAL_QUIET + STEP_TIMEOUT;

        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");
        let pending = pending
            .poll(Some(&runtime), harvest_started)
            .expect("history harvest");
        input_rx.try_recv().expect("upward wheel batch");

        let mut synchronized = b"\x1b[?2026h".to_vec();
        synchronized.extend(draw(&["13", "14", "15", "16", "17"], false));
        runtime.test_process_pty_bytes(&synchronized);
        let pending = pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(1))
            .expect("synchronized redraw observed");
        let pending = pending
            .poll(
                Some(&runtime),
                harvest_started + STEP_TIMEOUT + Duration::from_millis(1),
            )
            .expect("synchronized redraw must remain pending");
        assert!(input_rx.try_recv().is_err());

        runtime.test_process_pty_bytes(b"\x1b[?2026l");
        let pending = pending
            .poll(
                Some(&runtime),
                harvest_started + STEP_TIMEOUT + Duration::from_millis(2),
            )
            .expect("viewport restore after synchronized redraw");
        input_rx.try_recv().expect("restore wheel batch");

        runtime.test_process_pty_bytes(&draw(&initial, false));
        let pending = pending
            .poll(
                Some(&runtime),
                harvest_started + STEP_TIMEOUT + Duration::from_millis(3),
            )
            .expect("restore redraw coalescing");
        assert!(pending
            .poll(
                Some(&runtime),
                harvest_started + STEP_TIMEOUT + Duration::from_millis(13)
            )
            .is_none());
        assert_eq!(
            response_text(&response_rx),
            "13\n14\n15\n16\n17\n18\n19\n20\n"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn aligned_intermediate_redraw_is_coalesced_before_scrolling_again() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let initial = ["16", "17", "18", "19", "20", "ready"];
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 6, 8);
        runtime.test_process_pty_bytes(&draw(&initial, true));
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 9);
        let harvest_started = started + INITIAL_QUIET + STEP_TIMEOUT;

        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");
        let pending = pending
            .poll(Some(&runtime), harvest_started)
            .expect("history harvest");
        input_rx.try_recv().expect("upward wheel batch");

        runtime.test_process_pty_bytes(&draw(&["13", "14", "15", "16", "17", "loading"], false));
        let pending = pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(1))
            .expect("intermediate redraw coalescing");
        assert!(input_rx.try_recv().is_err());
        runtime.test_process_pty_bytes(&draw(&["13", "14", "15", "16", "17", "ready"], false));
        let pending = pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(5))
            .expect("completed redraw coalescing");
        assert!(input_rx.try_recv().is_err());
        let pending = pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(15))
            .expect("viewport restore after completed redraw");
        input_rx.try_recv().expect("restore wheel batch");

        runtime.test_process_pty_bytes(&draw(&initial, false));
        let pending = pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(16))
            .expect("restore redraw coalescing");
        assert!(pending
            .poll(Some(&runtime), harvest_started + Duration::from_millis(26))
            .is_none());
        assert_eq!(
            response_text(&response_rx),
            "13\n14\n15\n16\n17\n18\n19\n20\nready\n"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn incomplete_redraw_waits_for_an_aligned_screen_without_resetting_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _guard = rt.enter();
        let initial_bytes = draw(&["16", "17", "18", "19", "20"], true);
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel_capacity(20, 5, 8);
        runtime.test_process_pty_bytes(&initial_bytes);
        let started = Instant::now();
        let (pending, response_rx) = pending_read(&runtime, started, 8);

        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET)
            .expect("bottom probe");
        input_rx.try_recv().expect("bottom wheel probe");
        let pending = pending
            .poll(Some(&runtime), started + INITIAL_QUIET + STEP_TIMEOUT)
            .expect("history harvest");
        input_rx.try_recv().expect("upward wheel batch");

        runtime.test_process_pty_bytes(&draw(&["13"], false));
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(1),
            )
            .expect("partial redraw coalescing");
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(11),
            )
            .expect("partial redraw must not complete");
        assert!(
            input_rx.try_recv().is_err(),
            "partial redraw must not scroll again"
        );

        runtime.test_process_pty_bytes(&draw(&["13", "14", "15", "16", "17"], false));
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(12),
            )
            .expect("aligned redraw coalescing");
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(22),
            )
            .expect("aligned redraw should start restore");
        input_rx.try_recv().expect("restore wheel batch");
        runtime.test_process_pty_bytes(&draw(&["16", "17", "18", "19", "20"], false));
        let pending = pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(23),
            )
            .expect("restore redraw coalescing");
        assert!(pending
            .poll(
                Some(&runtime),
                started + INITIAL_QUIET + STEP_TIMEOUT + Duration::from_millis(33),
            )
            .is_none());
        assert_eq!(
            response_text(&response_rx),
            "13\n14\n15\n16\n17\n18\n19\n20\n"
        );

        drop(runtime);
        drop(_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }
}
