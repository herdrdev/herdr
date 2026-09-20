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
    /// True when this candidate jumped away from the settled caret (a different
    /// row, or a large same-row column move).
    ///
    /// Those are the shape of a redraw parking the cursor on a temporary cell,
    /// so they are held for the max window before being shown. Ordinary caret
    /// steps are small and same-row, and settle on the normal window.
    candidate_jump: bool,
}

impl CursorPositionSettleState {
    pub(crate) fn observe(&mut self, current: Option<TerminalCursorState>, now: Instant) {
        // A return to the caret's position or row ends the redraw hold, even
        // when typing advanced its column. Otherwise the accumulated typing
        // deadline can settle a later repair cell and anchor redraws there.
        if let (Some(candidate), Some(since)) = (self.candidate, self.candidate_since) {
            let restored = self.settled.zip(current).is_some_and(|(settled, current)| {
                settled.visible
                    && (same_cursor_position(settled, current)
                        || (candidate.y != settled.y
                            && current.y == settled.y
                            && now.duration_since(since) < self.candidate_hold()))
            });
            if restored {
                self.settle(current);
                return;
            }
            // Preserve an eligible caret before a later redraw moves it away.
            if now.duration_since(since) >= self.candidate_hold() {
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
            self.candidate_jump = is_jump(settled, current);
            return;
        };

        let pending_since = self.pending_since.unwrap_or(now);
        if now.duration_since(pending_since) >= CURSOR_POSITION_MAX_HOLD {
            self.settle(Some(current));
        } else {
            if !same_cursor_position(candidate, current) {
                self.candidate_since = Some(now);
                self.candidate_jump = is_jump(settled, current);
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
        // A jump-shaped candidate is treated as a redraw park until proven
        // otherwise, so it waits for the max window. An ordinary caret step only
        // waits the normal settle window; the max hold bounds either case.
        if now.duration_since(candidate_since) >= self.candidate_hold()
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

    pub(crate) fn render_delay(&self) -> Option<Duration> {
        self.pending().then(|| self.candidate_hold())
    }

    fn candidate_hold(&self) -> Duration {
        if self.candidate_jump {
            CURSOR_POSITION_MAX_HOLD
        } else {
            CURSOR_POSITION_SETTLE
        }
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

/// A cursor move that changes row, or jumps more than a couple of columns, is
/// the shape of a redraw parking the cursor rather than an ordinary caret step.
fn is_jump(settled: TerminalCursorState, current: TerminalCursorState) -> bool {
    current.y != settled.y || current.x.abs_diff(settled.x) > 2
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
        let caret = cursor(2, 12, true, 0);
        for (next_caret, repair) in [
            (cursor(3, 12, true, 0), cursor(0, 10, true, 0)),
            (cursor(20, 12, true, 0), cursor(0, 10, true, 0)),
            (cursor(2, 13, true, 0), cursor(0, 10, true, 0)),
            (cursor(2, 13, true, 0), cursor(0, 12, true, 0)),
        ] {
            let mut settle = CursorPositionSettleState::default();
            settle.observe(Some(caret), now);
            settle.observe(Some(next_caret), now + Duration::from_millis(1));

            // Preserve real typing, Home/End, and row moves before the next
            // ConPTY redraw parks briefly at its repair cell.
            settle.observe(Some(repair), now + Duration::from_millis(160));
            assert_eq!(
                settle.reported_cursor(Some(repair), now + Duration::from_millis(161)),
                Some(next_caret)
            );
            settle.observe(Some(next_caret), now + Duration::from_millis(170));
            assert_eq!(
                settle.reported_cursor(Some(next_caret), now + Duration::from_millis(171)),
                Some(next_caret)
            );
        }
    }

    #[test]
    fn cursor_settle_recovers_from_a_late_restore_during_continuous_redraws() {
        let now = Instant::now();
        let caret = cursor(6, 9, true, 0);
        let park = cursor(0, 7, true, 0);
        for (restored, first_restore) in [
            (caret, 161),
            (cursor(7, 9, true, 0), 71),
            (cursor(30, 9, true, 0), 71),
        ] {
            let mut settle = CursorPositionSettleState::default();
            settle.observe(Some(caret), now);
            settle.observe(Some(park), now + Duration::from_millis(1));

            // Exact restoration can miss the max hold. A changed column only
            // restores the old row while the new row is still provisional.
            for ms in (first_restore..first_restore + 1800).step_by(30) {
                let restored_at = now + Duration::from_millis(ms);
                settle.observe(Some(restored), restored_at);
                assert_eq!(
                    settle.reported_cursor(Some(restored), restored_at),
                    Some(restored),
                    "the repair cell must not become the anchor for later redraws"
                );
                settle.observe(Some(park), restored_at + Duration::from_millis(20));
            }
        }
    }

    #[test]
    fn cursor_settle_keeps_typing_deadlines_from_adopting_redraw_positions() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        let park = cursor(0, 37, true, 0);
        settle.observe(Some(cursor(3, 39, true, 0)), now);
        settle.observe(Some(cursor(4, 39, true, 0)), now + Duration::from_millis(1));

        // Fast typing advances the caret while redraws briefly visit another
        // row. A burst's max deadline can fall on one of those repair writes.
        for step in 0..60 {
            let parked_at = now + Duration::from_millis(11 + step * 30);
            settle.observe(Some(park), parked_at);
            assert_eq!(
                settle.reported_cursor(Some(park), parked_at).unwrap().y,
                39,
                "a fresh repair position must not inherit the typing deadline"
            );
            let caret = cursor(6 + step as u16 * 2, 39, true, 0);
            settle.observe(Some(caret), parked_at + Duration::from_millis(10));
        }
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
        for step in [10, 30] {
            let mut settle = CursorPositionSettleState::default();
            settle.observe(Some(cursor(0, 0, true, 0)), now);
            for ms in (1..=91).step_by(step) {
                settle.observe(
                    Some(cursor(ms as u16, 1, true, 0)),
                    now + Duration::from_millis(ms),
                );
            }
            assert_eq!(
                settle.reported_cursor(
                    Some(cursor(91, 1, true, 0)),
                    now + Duration::from_millis(101)
                ),
                Some(cursor(91, 1, true, 0))
            );
        }
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
    fn cursor_settle_repeated_jump_position_does_not_starve() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        let next = cursor(2, 1, true, 0);
        settle.observe(Some(cursor(2, 0, true, 0)), now);
        for ms in [1, 31, 61, 91] {
            settle.observe(Some(next), now + Duration::from_millis(ms));
        }
        assert_eq!(
            settle.reported_cursor(Some(next), now + Duration::from_millis(101)),
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

    #[test]
    fn cursor_settle_does_not_publish_a_jump_before_the_max_hold() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        let caret = cursor(2, 26, true, 0);
        let park = cursor(0, 25, true, 0);
        settle.observe(Some(caret), now);

        // A transition redraw parks the cursor on a cell above the composer and
        // restores the caret in a later write, more than one settle window away.
        // The park must not be published in the meantime.
        settle.observe(Some(park), now + Duration::from_millis(1));
        assert_eq!(
            settle.reported_cursor(
                Some(park),
                now + CURSOR_POSITION_SETTLE + Duration::from_millis(1)
            ),
            Some(caret)
        );
        assert_eq!(
            settle.reported_cursor(Some(park), now + Duration::from_millis(70)),
            Some(caret)
        );

        // An ordinary same-row caret step still settles on the normal window.
        settle.observe(
            Some(cursor(3, 26, true, 0)),
            now + Duration::from_millis(80),
        );
        settle.observe(
            Some(cursor(4, 26, true, 0)),
            now + Duration::from_millis(81),
        );
        assert_eq!(
            settle.reported_cursor(
                Some(cursor(4, 26, true, 0)),
                now + CURSOR_POSITION_SETTLE + Duration::from_millis(82)
            ),
            Some(cursor(4, 26, true, 0))
        );
    }
}
