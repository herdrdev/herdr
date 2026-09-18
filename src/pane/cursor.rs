use std::time::{Duration, Instant};

use super::terminal::TerminalCursorState;

pub(crate) const CURSOR_POSITION_SETTLE: Duration = Duration::from_millis(20);
const CURSOR_POSITION_MAX_HOLD: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub(crate) struct DecscusrTracker {
    state: DecscusrParseState,
    cursor_shape_overridden: bool,
}

#[derive(Debug, Default)]
enum DecscusrParseState {
    #[default]
    Ground,
    Escape,
    Csi {
        first_param: Option<u16>,
        collecting_first_param: bool,
        has_space_intermediate: bool,
    },
}

impl DecscusrTracker {
    pub(crate) fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.observe_byte(byte);
        }
    }

    fn observe_byte(&mut self, byte: u8) {
        match &mut self.state {
            DecscusrParseState::Ground => {
                if byte == 0x1b {
                    self.state = DecscusrParseState::Escape;
                }
            }
            DecscusrParseState::Escape => {
                self.state = if byte == b'[' {
                    DecscusrParseState::Csi {
                        first_param: None,
                        collecting_first_param: true,
                        has_space_intermediate: false,
                    }
                } else if byte == 0x1b {
                    DecscusrParseState::Escape
                } else {
                    DecscusrParseState::Ground
                };
            }
            DecscusrParseState::Csi {
                first_param,
                collecting_first_param,
                has_space_intermediate,
            } => {
                if byte == 0x1b {
                    self.state = DecscusrParseState::Escape;
                } else if byte.is_ascii_digit() && *collecting_first_param {
                    let digit = u16::from(byte - b'0');
                    *first_param = Some(first_param.unwrap_or(0).saturating_mul(10) + digit);
                } else if byte == b';' || byte == b':' {
                    *collecting_first_param = false;
                } else if byte == b' ' {
                    *has_space_intermediate = true;
                    *collecting_first_param = false;
                } else if (0x40..=0x7e).contains(&byte) {
                    if byte == b'q' && *has_space_intermediate {
                        let param = first_param.unwrap_or(0);
                        if param <= 6 {
                            self.cursor_shape_overridden = param != 0;
                        }
                    }
                    self.state = DecscusrParseState::Ground;
                } else if !(0x20..=0x3f).contains(&byte) {
                    self.state = DecscusrParseState::Ground;
                }
            }
        }
    }

    pub(crate) fn cursor_shape_overridden(&self) -> bool {
        self.cursor_shape_overridden
    }
}

#[derive(Debug, Default)]
pub(crate) struct CursorPositionSettleState {
    settled: Option<TerminalCursorState>,
    candidate: Option<TerminalCursorState>,
    pending_since: Option<Instant>,
    candidate_since: Option<Instant>,
}

impl CursorPositionSettleState {
    pub(crate) fn observe(&mut self, current: Option<TerminalCursorState>, now: Instant) {
        // A quiet candidate was already eligible for display. Preserve it before
        // considering the first (possibly temporary) position of a later redraw.
        if let (Some(candidate), Some(since)) = (self.candidate, self.candidate_since) {
            if now.duration_since(since) >= CURSOR_POSITION_SETTLE {
                self.settle(Some(candidate));
            }
        }
        let Some(current) = current else {
            self.settle(None);
            return;
        };
        if !current.visible {
            self.settle(Some(current));
            return;
        }
        let Some(settled) = self.settled else {
            self.settle(Some(current));
            return;
        };
        if same_cursor_position(settled, current) && settled.visible {
            self.settle(Some(current));
            return;
        }

        let Some(candidate) = self.candidate else {
            self.candidate = Some(current);
            self.pending_since = Some(now);
            self.candidate_since = Some(now);
            return;
        };

        let pending_since = self.pending_since.unwrap_or(now);
        if now.duration_since(pending_since) >= CURSOR_POSITION_MAX_HOLD {
            self.settle(Some(current));
        } else {
            if !same_cursor_position(candidate, current) {
                self.candidate_since = Some(now);
            }
            self.candidate = Some(current);
        }
    }

    pub(crate) fn reported_cursor(
        &self,
        current: Option<TerminalCursorState>,
        now: Instant,
    ) -> Option<TerminalCursorState> {
        let current = current?;
        let Some(candidate) = self.candidate else {
            return Some(current);
        };
        let candidate_since = self.candidate_since.unwrap_or(now);
        let pending_since = self.pending_since.unwrap_or(now);
        if now.duration_since(candidate_since) >= CURSOR_POSITION_SETTLE
            || now.duration_since(pending_since) >= CURSOR_POSITION_MAX_HOLD
        {
            return Some(TerminalCursorState {
                visible: current.visible && candidate.visible,
                shape: current.shape,
                ..candidate
            });
        }
        self.settled
            .map(|settled| TerminalCursorState {
                visible: current.visible && settled.visible,
                shape: current.shape,
                ..settled
            })
            .or(Some(TerminalCursorState {
                visible: false,
                shape: current.shape,
                ..candidate
            }))
    }

    pub(crate) fn pending(&self) -> bool {
        self.candidate.is_some()
    }

    fn settle(&mut self, cursor: Option<TerminalCursorState>) {
        *self = Self {
            settled: cursor,
            ..Self::default()
        };
    }
}

fn same_cursor_position(left: TerminalCursorState, right: TerminalCursorState) -> bool {
    left.x == right.x && left.y == right.y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(x: u16, y: u16, visible: bool, shape: u8) -> TerminalCursorState {
        TerminalCursorState {
            x,
            y,
            visible,
            shape,
        }
    }

    #[test]
    fn cursor_settle_holds_position_change_until_quiet_window() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(20, 5, true, 0)), now + Duration::from_millis(1));

        let reported = settle
            .reported_cursor(Some(cursor(20, 5, true, 0)), now + Duration::from_millis(2))
            .unwrap();

        assert_eq!((reported.x, reported.y), (1, 0));
    }

    #[test]
    fn cursor_settle_adopts_position_change_after_quiet_window() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));

        let reported = settle
            .reported_cursor(
                Some(cursor(2, 0, true, 0)),
                now + CURSOR_POSITION_SETTLE + Duration::from_millis(1),
            )
            .unwrap();

        assert_eq!((reported.x, reported.y), (2, 0));
    }

    #[test]
    fn cursor_settle_keeps_previous_caret_during_next_system_conpty_redraw() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        let caret = cursor(2, 12, true, 0);
        let typed_caret = cursor(3, 12, true, 0);
        let repair = cursor(0, 10, true, 0);
        settle.observe(Some(caret), now);
        settle.observe(Some(typed_caret), now + Duration::from_millis(1));

        // System ConPTY closes the next frame at the repair cell, then emits
        // the caret restoration separately about 10 ms later.
        settle.observe(Some(repair), now + Duration::from_millis(160));
        assert_eq!(
            settle.reported_cursor(Some(repair), now + Duration::from_millis(161)),
            Some(typed_caret)
        );
        settle.observe(Some(typed_caret), now + Duration::from_millis(170));
        assert_eq!(
            settle.reported_cursor(Some(typed_caret), now + Duration::from_millis(171)),
            Some(typed_caret)
        );
    }

    #[test]
    fn cursor_settle_restarts_quiet_window_when_candidate_moves() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        let initial = cursor(1, 0, true, 0);
        let latest = cursor(3, 0, true, 0);
        settle.observe(Some(initial), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));
        settle.observe(Some(latest), now + Duration::from_millis(19));
        assert_eq!(
            settle.reported_cursor(Some(latest), now + Duration::from_millis(22)),
            Some(initial)
        );
        assert_eq!(
            settle.reported_cursor(Some(latest), now + Duration::from_millis(39)),
            Some(latest)
        );
    }

    #[test]
    fn cursor_settle_caps_continuous_position_changes_from_first_pending_time() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));
        for ms in (11..=91).step_by(10) {
            settle.observe(
                Some(cursor(ms as u16, 0, true, 0)),
                now + Duration::from_millis(ms),
            );
        }
        settle.observe(
            Some(cursor(3, 0, true, 0)),
            now + CURSOR_POSITION_MAX_HOLD + Duration::from_millis(1),
        );

        assert!(!settle.pending());
        assert_eq!(
            settle.reported_cursor(
                Some(cursor(3, 0, true, 0)),
                now + CURSOR_POSITION_MAX_HOLD + Duration::from_millis(2),
            ),
            Some(cursor(3, 0, true, 0))
        );
    }

    #[test]
    fn cursor_settle_caps_hold_even_without_another_observation() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(0, 0, true, 0)), now);
        for ms in (1..=91).step_by(10) {
            settle.observe(
                Some(cursor(ms as u16, 0, true, 0)),
                now + Duration::from_millis(ms),
            );
        }
        assert_eq!(
            settle.reported_cursor(
                Some(cursor(91, 0, true, 0)),
                now + Duration::from_millis(101)
            ),
            Some(cursor(91, 0, true, 0))
        );
    }

    #[test]
    fn cursor_settle_repeated_position_does_not_restart_quiet_window() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        let next = cursor(2, 0, true, 0);
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(next), now + Duration::from_millis(1));
        settle.observe(Some(next), now + Duration::from_millis(19));
        assert_eq!(
            settle.reported_cursor(Some(next), now + Duration::from_millis(21)),
            Some(next)
        );
    }

    #[test]
    fn cursor_settle_keeps_render_read_pure() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));

        assert!(settle.pending());
        let _ = settle.reported_cursor(
            Some(cursor(2, 0, true, 0)),
            now + CURSOR_POSITION_SETTLE + Duration::from_millis(1),
        );

        assert!(settle.pending());
    }

    #[test]
    fn cursor_settle_passes_shape_through_while_position_is_held() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 2)), now);
        settle.observe(Some(cursor(2, 0, true, 6)), now + Duration::from_millis(1));

        let reported = settle
            .reported_cursor(Some(cursor(2, 0, true, 6)), now + Duration::from_millis(2))
            .unwrap();

        assert_eq!((reported.x, reported.y, reported.shape), (1, 0, 6));
    }

    #[test]
    fn cursor_settle_passes_shape_through_after_quiet_window() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 2)), now);
        settle.observe(Some(cursor(2, 0, true, 2)), now + Duration::from_millis(1));

        let reported = settle
            .reported_cursor(
                Some(cursor(2, 0, true, 6)),
                now + CURSOR_POSITION_SETTLE + Duration::from_millis(1),
            )
            .unwrap();

        assert_eq!((reported.x, reported.y, reported.shape), (2, 0, 6));
    }

    #[test]
    fn cursor_settle_hides_immediately_and_waits_to_reveal() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(1, 0, false, 0)), now + Duration::from_millis(1));

        assert_eq!(
            settle.reported_cursor(Some(cursor(1, 0, false, 0)), now + Duration::from_millis(2)),
            Some(cursor(1, 0, false, 0))
        );

        settle.observe(Some(cursor(1, 0, true, 0)), now + Duration::from_millis(3));
        assert_eq!(
            settle.reported_cursor(Some(cursor(1, 0, true, 0)), now + Duration::from_millis(4)),
            Some(cursor(1, 0, false, 0))
        );
    }
}
