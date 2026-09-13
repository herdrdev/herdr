//! Optional, conservative local echo. Authoritative terminal cells are never mutated.
//!
//! The first echo on each line is learned from the server. This reduces the chance of
//! displaying input at a non-echoing prompt, but is not an assertion about application
//! echo permissions: the endpoint protocol does not expose those permissions.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::protocol::{
    CellData, ClientKeyCode, ClientKeyKind, ClientPaneInputEvent, FrameData, PaneSurfaceFrame,
    SurfaceRect,
};

const PREDICTION_LIFETIME: Duration = Duration::from_millis(750);

#[derive(Default)]
pub(super) struct InputPrediction {
    line: Option<PredictedLine>,
}

struct PredictedLine {
    boot_id: String,
    pane_id: String,
    geometry: SurfaceRect,
    surface_size: (u16, u16),
    terminal_modes: (bool, bool),
    cursor_visible: bool,
    y: u16,
    x: u16,
    row: Vec<CellData>,
    pending: VecDeque<PendingCharacter>,
    trained: bool,
}

struct PendingCharacter {
    byte: u8,
    sent_at: Instant,
}

struct EligibleRow<'a> {
    geometry: SurfaceRect,
    terminal_modes: (bool, bool),
    cursor_visible: bool,
    x: u16,
    y: u16,
    cells: &'a [CellData],
}

struct SoftwareCursor<'a> {
    cell: &'a CellData,
    padding: &'a CellData,
}

fn blank_cell(cell: &CellData) -> bool {
    cell.symbol == " " && !cell.skip && cell.hyperlink.is_none()
}

fn same_style(left: &CellData, right: &CellData) -> bool {
    left.fg == right.fg && left.bg == right.bg && left.modifier == right.modifier
}

fn eligible_row<'a>(surface: &'a PaneSurfaceFrame, pane_id: &str) -> Option<EligibleRow<'a>> {
    if surface.popup.is_some() {
        return None;
    }
    let pane = surface.panes.iter().find(|pane| pane.pane_id == pane_id)?;
    if !pane.focused
        || pane
            .scroll
            .is_some_and(|scroll| scroll.offset_from_bottom != 0)
    {
        return None;
    }
    // A TUI may draw its own caret while keeping the terminal cursor hidden.
    // Coordinates alone do not grant confidence: an exact echo must still be learned.
    let cursor = surface.frame.cursor.as_ref()?;
    let geometry = pane.inner_rect;
    let right = geometry.x.checked_add(geometry.width)?;
    let bottom = geometry.y.checked_add(geometry.height)?;
    if cursor.x < geometry.x
        || cursor.x >= right
        || cursor.y < geometry.y
        || cursor.y >= bottom
        || right > surface.frame.width
        || bottom > surface.frame.height
    {
        return None;
    }
    let start = usize::from(cursor.y) * usize::from(surface.frame.width) + usize::from(geometry.x);
    Some(EligibleRow {
        geometry,
        terminal_modes: (pane.alternate_screen_active, pane.mouse_reporting),
        cursor_visible: cursor.visible,
        x: cursor.x,
        y: cursor.y,
        cells: surface
            .frame
            .cells
            .get(start..start + usize::from(geometry.width))?,
    })
}

impl PredictedLine {
    fn software_cursor(&self) -> Option<SoftwareCursor<'_>> {
        if self.cursor_visible {
            return None;
        }
        let index = usize::from(self.x - self.geometry.x);
        let cell = self.row.get(index)?;
        let padding = self.row.get(index + 1)?;
        // A hidden hardware cursor can anchor an application-painted blank caret.
        // Its style may already be resolved into colors rather than REVERSED.
        // This is only a candidate: observe must prove that this exact style moves
        // with echoed text, leaving ordinary padding style behind.
        (blank_cell(cell) && blank_cell(padding) && cell != padding)
            .then_some(SoftwareCursor { cell, padding })
    }

    fn matches_context(&self, surface: &PaneSurfaceFrame, row: &EligibleRow<'_>) -> bool {
        // Global projection revisions also change for unrelated sidebar metadata.
        // Actual pane identity, focus (checked by eligible_row), modes and geometry
        // define the input context; the row comparison below validates the prompt.
        self.boot_id == surface.boot_id
            && self.geometry == row.geometry
            && self.terminal_modes == row.terminal_modes
            && self.cursor_visible == row.cursor_visible
            && self.surface_size == (surface.frame.width, surface.frame.height)
            && self.y == row.y
    }

    fn visible(&self) -> bool {
        self.trained && !self.pending.is_empty()
    }

    fn deadline(&self) -> Option<Instant> {
        // Only outstanding input can time out. Pausing at a confirmed, unchanged
        // prompt must not turn the next character into another network round trip.
        self.pending
            .front()
            .map(|pending| pending.sent_at + PREDICTION_LIFETIME)
    }
}

impl InputPrediction {
    pub(super) fn has_pending(&self) -> bool {
        self.line
            .as_ref()
            .is_some_and(|line| !line.pending.is_empty())
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.line.as_ref().and_then(PredictedLine::deadline)
    }

    /// Returns whether removing the prediction needs a repaint.
    pub(super) fn clear(&mut self) -> bool {
        self.line.take().is_some_and(|line| line.visible())
    }

    pub(super) fn expire(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            tracing::debug!("remote input prediction expired");
            self.clear()
        } else {
            false
        }
    }

    /// Reconcile against an accepted full surface or an already-applied surface patch.
    /// Surface revisions are not input acknowledgments: unchanged rows can arrive while
    /// input is still travelling, and must not erase predictions prematurely.
    pub(super) fn observe(&mut self, surface: &PaneSurfaceFrame, now: Instant) -> bool {
        let repaint = self.expire(now);
        let Some(line) = self.line.as_ref() else {
            return repaint;
        };
        let Some(row) = eligible_row(surface, &line.pane_id) else {
            return self.clear() || repaint;
        };
        if !line.matches_context(surface, &row) {
            return self.clear() || repaint;
        }
        let Some(confirmed) = row.x.checked_sub(line.x).map(usize::from) else {
            return self.clear() || repaint;
        };
        if confirmed > line.pending.len() {
            return self.clear() || repaint;
        }
        let start = usize::from(line.x - line.geometry.x);
        let software_cursor = line.software_cursor();
        let matches = row
            .cells
            .iter()
            .zip(&line.row)
            .enumerate()
            .all(|(index, (actual, base))| {
                if index >= start && index < start + confirmed {
                    let pending = &line.pending[index - start];
                    // Applications may style their echoed text. Geometry and the unchanged
                    // remainder of the row still have to agree exactly.
                    actual.symbol.as_bytes() == [pending.byte]
                        && !actual.skip
                        && actual.hyperlink.is_none()
                        && software_cursor
                            .as_ref()
                            .is_none_or(|cursor| same_style(actual, cursor.padding))
                } else if confirmed != 0 && index == start + confirmed && software_cursor.is_some()
                {
                    // Permit only the exact painted caret moving to its new anchor;
                    // other style/content changes anywhere on this row invalidate it.
                    software_cursor
                        .as_ref()
                        .is_some_and(|cursor| actual == cursor.cell && base == cursor.padding)
                } else {
                    actual == base
                }
            });
        if !matches {
            return self.clear() || repaint;
        }
        if confirmed == 0 {
            return repaint;
        }
        let Some(line) = self.line.as_mut() else {
            return repaint;
        };
        let was_visible = line.visible();
        // A blank echoed space alone does not establish that the application displays
        // characters; some password inputs move the cursor while leaving blank cells.
        line.trained |= line
            .pending
            .iter()
            .take(confirmed)
            .any(|pending| pending.byte != b' ');
        line.pending.drain(..confirmed);
        line.row.clone_from_slice(row.cells);
        line.x = row.x;
        tracing::debug!(
            confirmed,
            pending = line.pending.len(),
            "remote input prediction confirmed"
        );
        repaint || was_visible || line.visible()
    }

    /// Record only events which the shell has already routed to a pane. Enter, editing,
    /// paste, mouse and other controls invalidate confidence. Releases have no echo.
    pub(super) fn record_input(
        &mut self,
        surface: &PaneSurfaceFrame,
        pane_id: &str,
        event: &ClientPaneInputEvent,
        now: Instant,
    ) -> bool {
        if matches!(
            event,
            ClientPaneInputEvent::Key {
                kind: ClientKeyKind::Release,
                ..
            }
        ) {
            return self.expire(now);
        }
        let Some(text) = printable_text(event) else {
            return self.clear();
        };
        let mut repaint = self.observe(surface, now);
        if self
            .line
            .as_ref()
            .is_some_and(|line| line.pane_id != pane_id)
        {
            repaint |= self.clear();
        }
        let Some(row) = eligible_row(surface, pane_id) else {
            return self.clear() || repaint;
        };
        if self.line.is_none() {
            self.line = Some(PredictedLine {
                boot_id: surface.boot_id.clone(),
                pane_id: pane_id.to_owned(),
                geometry: row.geometry,
                surface_size: (surface.frame.width, surface.frame.height),
                terminal_modes: row.terminal_modes,
                cursor_visible: row.cursor_visible,
                y: row.y,
                x: row.x,
                row: row.cells.to_vec(),
                pending: VecDeque::new(),
                trained: false,
            });
        }
        let Some(line) = self.line.as_mut() else {
            return repaint;
        };
        let start = usize::from(line.x - line.geometry.x) + line.pending.len();
        let end = start.saturating_add(text.len());
        // Leave a spare cell: writing the final column can trigger terminal wrapping.
        if text.is_empty()
            || end >= line.row.len()
            || !line.row[start..end].iter().all(blank_cell)
            || line.software_cursor().is_some_and(|cursor| {
                let after_cursor = usize::from(line.x - line.geometry.x) + 1;
                !line.row[after_cursor..=end]
                    .iter()
                    .all(|cell| cell == cursor.padding)
            })
        {
            return self.clear() || repaint;
        }
        line.pending.extend(
            text.bytes()
                .map(|byte| PendingCharacter { byte, sent_at: now }),
        );
        repaint || line.visible()
    }

    /// Apply to the composed client frame only, with the pane surface's client origin.
    pub(super) fn apply(&self, frame: &mut FrameData, origin: (u16, u16)) {
        let Some(line) = self.line.as_ref().filter(|line| line.visible()) else {
            return;
        };
        let (Some(x), Some(y)) = (line.x.checked_add(origin.0), line.y.checked_add(origin.1))
        else {
            return;
        };
        let end = usize::from(x) + line.pending.len();
        if end >= usize::from(frame.width) || y >= frame.height {
            return;
        }
        let Some(cursor) = frame.cursor.as_ref() else {
            return;
        };
        if cursor.x != x || cursor.y != y {
            return;
        }
        let start = usize::from(y) * usize::from(frame.width) + usize::from(x);
        let software_cursor = line.software_cursor();
        let count = line.pending.len() + usize::from(software_cursor.is_some());
        let Some(cells) = frame.cells.get_mut(start..start + count) else {
            return;
        };
        for (cell, pending) in cells.iter_mut().zip(&line.pending) {
            if let Some(cursor) = &software_cursor {
                cell.fg = cursor.padding.fg;
                cell.bg = cursor.padding.bg;
                cell.modifier = cursor.padding.modifier;
            }
            cell.symbol = char::from(pending.byte).to_string();
            cell.modifier |= ratatui::style::Modifier::UNDERLINED.bits();
            cell.hyperlink = None;
            cell.skip = false;
        }
        if let Some(cursor) = software_cursor {
            cells[line.pending.len()].clone_from(cursor.cell);
        }
        if let Some(cursor) = frame.cursor.as_mut() {
            cursor.x = end as u16;
        }
    }
}

fn printable_text(event: &ClientPaneInputEvent) -> Option<String> {
    let text = match event {
        ClientPaneInputEvent::TextCommit(text) if text.len() <= 256 => text.clone(),
        ClientPaneInputEvent::Key {
            code: ClientKeyCode::Char(character),
            modifiers,
            kind: ClientKeyKind::Press | ClientKeyKind::Repeat,
            repeat_count,
            generated_text,
            ..
        } if *modifiers & !crossterm::event::KeyModifiers::SHIFT.bits() == 0 => {
            if *repeat_count > 256 || generated_text.as_ref().is_some_and(|text| text.len() > 256) {
                return None;
            }
            let text = generated_text
                .clone()
                .unwrap_or_else(|| character.to_string());
            // OS repeat records are bounded before allocating; normal terminal repeats
            // arrive as independent events. Large/batched input remains authoritative.
            let repeats = usize::from((*repeat_count).max(1));
            if text.len().saturating_mul(repeats) > 256 {
                return None;
            }
            text.repeat(repeats)
        }
        _ => return None,
    };
    (!text.is_empty()
        && text.len() <= 256
        && text.bytes().all(|byte| (b' '..=b'~').contains(&byte)))
    .then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CursorState, PaneSurfacePane, PaneSurfaceScrollMetrics};

    fn surface(text: &str) -> PaneSurfaceFrame {
        let mut frame = FrameData::from_ratatui_buffer(
            &ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 30, 4)),
            Some(CursorState {
                x: text.len() as u16,
                y: 1,
                visible: true,
                shape: 2,
            }),
        );
        for (index, character) in text.chars().enumerate() {
            frame.cells[30 + index].symbol = character.to_string();
        }
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame,
            panes: vec![PaneSurfacePane {
                pane_id: "pane".into(),
                content_revision: 2,
                rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width: 30,
                    height: 4,
                },
                inner_rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width: 30,
                    height: 4,
                },
                scrollbar_rect: None,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: vec![],
            popup: None,
            graphics: Default::default(),
        }
    }

    fn key(code: ClientKeyCode) -> ClientPaneInputEvent {
        ClientPaneInputEvent::Key {
            code,
            modifiers: 0,
            kind: ClientKeyKind::Press,
            repeat_count: 1,
            shifted_codepoint: None,
            generated_text: None,
            tracks_release: false,
            physical_key_id: None,
            windows_record: None,
        }
    }

    fn record(
        prediction: &mut InputPrediction,
        surface: &PaneSurfaceFrame,
        text: &str,
        now: Instant,
    ) -> bool {
        prediction.record_input(
            surface,
            "pane",
            &ClientPaneInputEvent::TextCommit(text.into()),
            now,
        )
    }

    fn rendered(prediction: &InputPrediction, surface: &PaneSurfaceFrame) -> FrameData {
        let mut frame = surface.frame.clone();
        prediction.apply(&mut frame, (0, 0));
        frame
    }

    fn row_text(frame: &FrameData) -> String {
        frame.cells[30..60]
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>()
    }

    fn trained(now: Instant) -> (InputPrediction, PaneSurfaceFrame) {
        let mut prediction = InputPrediction::default();
        record(&mut prediction, &surface("$ "), "a", now);
        let echoed = surface("$ a");
        prediction.observe(&echoed, now + Duration::from_millis(100));
        (prediction, echoed)
    }

    // Pi hides the hardware cursor and paints a blank at its coordinates. The
    // terminal renderer resolves reverse video into colors before serialization.
    fn software_cursor_surface(text: &str) -> PaneSurfaceFrame {
        let mut surface = surface(text);
        surface.frame.cursor.as_mut().unwrap().visible = false;
        let cell = &mut surface.frame.cells[30 + text.len()];
        cell.fg = 0x02_00_00_00;
        cell.bg = 0x02_ff_ff_ff;
        surface
    }

    #[test]
    fn prediction_learns_moving_software_cursor_and_moves_it_through_partial_echo() {
        let now = Instant::now();
        let mut prediction = InputPrediction::default();
        let initial = software_cursor_surface("");
        assert!(!record(&mut prediction, &initial, "a", now));
        let echoed = software_cursor_surface("a");
        prediction.observe(&echoed, now + Duration::from_millis(100));
        assert!(record(
            &mut prediction,
            &echoed,
            "bc",
            now + Duration::from_millis(110)
        ));
        let frame = rendered(&prediction, &echoed);
        assert!(row_text(&frame).starts_with("abc "));
        for index in 31..33 {
            assert_eq!(frame.cells[index].fg, 0);
            assert_eq!(frame.cells[index].bg, 0);
            assert_eq!(
                frame.cells[index].modifier,
                ratatui::style::Modifier::UNDERLINED.bits()
            );
        }
        assert_eq!(frame.cells[33], echoed.frame.cells[31]);
        assert!(!frame.cursor.as_ref().unwrap().visible);
        assert_eq!(frame.cursor.as_ref().unwrap().x, 3);
        assert_eq!(echoed.frame.cells[31].symbol, " ");
        let partial = software_cursor_surface("ab");
        prediction.observe(&partial, now + Duration::from_millis(150));
        assert_eq!(
            rendered(&prediction, &partial).cells[32..34],
            frame.cells[32..34]
        );
        let complete = software_cursor_surface("abc");
        prediction.observe(&complete, now + Duration::from_millis(200));
        assert!(!prediction.has_pending());
        assert_eq!(rendered(&prediction, &complete), complete.frame);
        assert!(record(
            &mut prediction,
            &complete,
            "d",
            now + Duration::from_secs(2)
        ));
        assert!(prediction.expire(now + Duration::from_secs(3)));
        assert_eq!(rendered(&prediction, &complete), complete.frame);
    }

    #[test]
    fn software_cursor_prediction_rejects_non_cursor_style_changes_and_wrong_echo() {
        let now = Instant::now();
        let changes: [fn(&mut PaneSurfaceFrame); 4] = [
            |surface| surface.frame.cells[30].symbol = "!".into(),
            |surface| surface.frame.cells[30].fg = 5,
            |surface| surface.frame.cells[31].bg = 5,
            |surface| surface.frame.cells[35].fg = 5,
        ];
        for change in changes {
            let mut prediction = InputPrediction::default();
            record(&mut prediction, &software_cursor_surface(""), "a", now);
            let mut echoed = software_cursor_surface("a");
            change(&mut echoed);
            prediction.observe(&echoed, now + Duration::from_millis(100));
            assert!(!prediction.has_pending());
            assert_eq!(rendered(&prediction, &echoed), echoed.frame);
        }
    }

    #[test]
    fn software_cursor_without_echo_never_reveals_input() {
        let now = Instant::now();
        let prompt = software_cursor_surface("");
        let mut prediction = InputPrediction::default();
        record(&mut prediction, &prompt, "secret", now);
        prediction.observe(&prompt, now + Duration::from_millis(100));
        assert_eq!(rendered(&prediction, &prompt), prompt.frame);
        let mut moved = software_cursor_surface("      ");
        moved.frame.cursor.as_mut().unwrap().x = 6;
        prediction.observe(&moved, now + Duration::from_millis(200));
        assert!(!prediction.has_pending());
        assert_eq!(rendered(&prediction, &moved), moved.frame);
    }

    #[test]
    fn prediction_waits_for_echo_then_displays_pending_suffix_without_mutating_authority() {
        let now = Instant::now();
        let mut prediction = InputPrediction::default();
        let initial = surface("$ ");
        assert!(!record(&mut prediction, &initial, "abc", now));
        assert_eq!(rendered(&prediction, &initial), initial.frame);

        let echoed = surface("$ a");
        assert!(prediction.observe(&echoed, now + Duration::from_millis(100)));
        let frame = rendered(&prediction, &echoed);
        assert!(row_text(&frame).starts_with("$ abc"));
        assert_eq!(frame.cursor.as_ref().unwrap().x, 5);
        assert_eq!(echoed.frame.cells[33].symbol, " ");
        assert_eq!(
            frame.cells[32].modifier & ratatui::style::Modifier::UNDERLINED.bits(),
            0
        );
        assert_ne!(
            frame.cells[33].modifier & ratatui::style::Modifier::UNDERLINED.bits(),
            0
        );
    }

    #[test]
    fn partial_and_complete_echo_retire_exactly_the_confirmed_prefix() {
        let now = Instant::now();
        let (mut prediction, echoed) = trained(now);
        assert!(record(
            &mut prediction,
            &echoed,
            "bcd",
            now + Duration::from_millis(110)
        ));
        let partial = surface("$ abc");
        prediction.observe(&partial, now + Duration::from_millis(200));
        assert!(row_text(&rendered(&prediction, &partial)).starts_with("$ abcd"));
        let complete = surface("$ abcd");
        prediction.observe(&complete, now + Duration::from_millis(300));
        assert!(!prediction.has_pending());
        assert_eq!(rendered(&prediction, &complete), complete.frame);
    }

    #[test]
    fn subsequent_input_uses_the_end_of_pending_text() {
        let now = Instant::now();
        let (mut prediction, echoed) = trained(now);
        record(
            &mut prediction,
            &echoed,
            "b",
            now + Duration::from_millis(110),
        );
        record(
            &mut prediction,
            &echoed,
            "c",
            now + Duration::from_millis(120),
        );
        assert!(row_text(&rendered(&prediction, &echoed)).starts_with("$ abc"));
    }

    #[test]
    fn unrelated_output_and_new_surface_revisions_do_not_count_as_input_acknowledgments() {
        let now = Instant::now();
        let (mut prediction, mut echoed) = trained(now);
        record(
            &mut prediction,
            &echoed,
            "b",
            now + Duration::from_millis(110),
        );
        echoed.surface_revision += 1;
        echoed.frame.cells[0].symbol = "!".into();
        prediction.observe(&echoed, now + Duration::from_millis(120));
        assert!(prediction.has_pending());
        assert!(row_text(&rendered(&prediction, &echoed)).starts_with("$ ab"));
    }

    #[test]
    fn mismatched_echo_rolls_back_and_requires_new_training() {
        let now = Instant::now();
        let (mut prediction, echoed) = trained(now);
        record(
            &mut prediction,
            &echoed,
            "b",
            now + Duration::from_millis(110),
        );
        let mismatch = surface("$ a!");
        assert!(prediction.observe(&mismatch, now + Duration::from_millis(120)));
        assert!(!prediction.has_pending());
        assert!(!record(
            &mut prediction,
            &mismatch,
            "c",
            now + Duration::from_millis(130)
        ));
        assert_eq!(rendered(&prediction, &mismatch), mismatch.frame);
    }

    #[test]
    fn pending_deadline_does_not_extend_with_more_input_or_unchanged_frames() {
        let now = Instant::now();
        let (mut prediction, echoed) = trained(now);
        let sent = now + Duration::from_millis(110);
        record(&mut prediction, &echoed, "b", sent);
        record(
            &mut prediction,
            &echoed,
            "c",
            sent + Duration::from_millis(500),
        );
        prediction.observe(&echoed, sent + Duration::from_millis(600));
        assert_eq!(prediction.deadline(), Some(sent + PREDICTION_LIFETIME));
        assert!(prediction.expire(sent + PREDICTION_LIFETIME));
        assert!(!prediction.has_pending());
        assert_eq!(rendered(&prediction, &echoed), echoed.frame);
    }

    #[test]
    fn non_echoing_password_input_never_appears_and_expires() {
        let now = Instant::now();
        let mut prediction = InputPrediction::default();
        let prompt = surface("Password: ");
        assert!(!record(&mut prediction, &prompt, "secret", now));
        prediction.observe(&prompt, now + Duration::from_millis(500));
        assert_eq!(rendered(&prediction, &prompt), prompt.frame);
        prediction.expire(now + PREDICTION_LIFETIME);
        assert!(!prediction.has_pending());
    }

    #[test]
    fn cursor_motion_over_blank_space_does_not_train_echo() {
        let now = Instant::now();
        let mut prediction = InputPrediction::default();
        record(&mut prediction, &surface("$ "), " ", now);
        let blank_echo = surface("$  ");
        prediction.observe(&blank_echo, now + Duration::from_millis(100));
        assert!(!record(
            &mut prediction,
            &blank_echo,
            "secret",
            now + Duration::from_millis(110)
        ));
        assert_eq!(rendered(&prediction, &blank_echo), blank_echo.frame);
    }

    #[test]
    fn controls_paste_and_non_ascii_clear_pending_text_and_confidence() {
        let now = Instant::now();
        let mut control = key(ClientKeyCode::Char('r'));
        if let ClientPaneInputEvent::Key { modifiers, .. } = &mut control {
            *modifiers = crossterm::event::KeyModifiers::CONTROL.bits();
        }
        for event in [
            key(ClientKeyCode::Enter),
            key(ClientKeyCode::Backspace),
            key(ClientKeyCode::Left),
            control,
            ClientPaneInputEvent::Paste("secret".into()),
            ClientPaneInputEvent::TextCommit("é".into()),
        ] {
            let (mut prediction, echoed) = trained(now);
            record(
                &mut prediction,
                &echoed,
                "b",
                now + Duration::from_millis(110),
            );
            assert!(prediction.record_input(
                &echoed,
                "pane",
                &event,
                now + Duration::from_millis(120)
            ));
            assert!(!prediction.has_pending());
            assert!(!record(
                &mut prediction,
                &echoed,
                "c",
                now + Duration::from_millis(130)
            ));
        }
    }

    #[test]
    fn context_changes_clear_predictions() {
        let now = Instant::now();
        let changes: [fn(&mut PaneSurfaceFrame); 7] = [
            |surface| surface.boot_id = "new-boot".into(),
            |surface| surface.panes[0].focused = false,
            |surface| surface.panes[0].alternate_screen_active = true,
            |surface| surface.panes[0].mouse_reporting = true,
            |surface| surface.frame.cursor.as_mut().unwrap().visible = false,
            |surface| surface.panes[0].inner_rect.width -= 1,
            |surface| {
                surface.panes[0].scroll = Some(PaneSurfaceScrollMetrics {
                    offset_from_bottom: 1,
                    max_offset_from_bottom: 10,
                    viewport_rows: 4,
                })
            },
        ];
        for change in changes {
            let (mut prediction, mut echoed) = trained(now);
            record(
                &mut prediction,
                &echoed,
                "b",
                now + Duration::from_millis(110),
            );
            change(&mut echoed);
            assert!(prediction.observe(&echoed, now + Duration::from_millis(120)));
            assert!(!prediction.has_pending());
        }
    }

    #[test]
    fn occupied_cells_and_terminal_right_edge_are_never_predicted() {
        let now = Instant::now();
        let (mut prediction, mut echoed) = trained(now);
        echoed.frame.cells[34].symbol = "!".into();
        assert!(!record(
            &mut prediction,
            &echoed,
            "bc",
            now + Duration::from_millis(110)
        ));
        assert!(!prediction.has_pending());
        let initial = surface(&"x".repeat(28));
        let mut prediction = InputPrediction::default();
        assert!(!record(&mut prediction, &initial, "ab", now));
        assert!(!prediction.has_pending());
    }

    #[test]
    fn prediction_offsets_cells_and_cursor_into_composed_frame() {
        let now = Instant::now();
        let (mut prediction, echoed) = trained(now);
        record(
            &mut prediction,
            &echoed,
            "b",
            now + Duration::from_millis(110),
        );
        let mut frame = FrameData::from_ratatui_buffer(
            &ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 8)),
            Some(CursorState {
                x: 8,
                y: 3,
                visible: true,
                shape: 2,
            }),
        );
        prediction.apply(&mut frame, (5, 2));
        assert_eq!(frame.cells[3 * 40 + 8].symbol, "b");
        assert_eq!(frame.cursor.unwrap().x, 9);
    }

    #[test]
    fn key_release_and_idle_pause_preserve_confirmed_prompt_confidence() {
        let now = Instant::now();
        let (mut prediction, echoed) = trained(now);
        let mut release = key(ClientKeyCode::Char('a'));
        if let ClientPaneInputEvent::Key { kind, .. } = &mut release {
            *kind = ClientKeyKind::Release;
        }
        assert!(!prediction.record_input(
            &echoed,
            "pane",
            &release,
            now + Duration::from_millis(110)
        ));
        assert!(record(
            &mut prediction,
            &echoed,
            "b",
            now + Duration::from_millis(120)
        ));
        assert!(!prediction.record_input(
            &echoed,
            "pane",
            &release,
            now + Duration::from_millis(130)
        ));
        assert!(prediction.has_pending());
        let (mut prediction, echoed) = trained(now);
        assert_eq!(prediction.deadline(), None);
        prediction.expire(now + Duration::from_millis(100) + PREDICTION_LIFETIME);
        assert!(record(
            &mut prediction,
            &echoed,
            "b",
            now + Duration::from_secs(1)
        ));
    }

    #[test]
    fn semantic_key_presses_and_repeat_counts_predict_exact_echoed_text() {
        let now = Instant::now();
        let (mut prediction, echoed) = trained(now);
        let mut event = key(ClientKeyCode::Char('b'));
        if let ClientPaneInputEvent::Key {
            kind, repeat_count, ..
        } = &mut event
        {
            *kind = ClientKeyKind::Repeat;
            *repeat_count = 3;
        }
        assert!(prediction.record_input(&echoed, "pane", &event, now + Duration::from_millis(110)));
        assert!(row_text(&rendered(&prediction, &echoed)).starts_with("$ abbb"));
        let complete = surface("$ abbb");
        prediction.observe(&complete, now + Duration::from_millis(120));
        assert_eq!(rendered(&prediction, &complete), complete.frame);
    }

    #[test]
    fn popup_or_missing_cursor_invalidates_prediction() {
        let now = Instant::now();
        for popup in [false, true] {
            let (mut prediction, mut echoed) = trained(now);
            record(
                &mut prediction,
                &echoed,
                "b",
                now + Duration::from_millis(110),
            );
            if popup {
                echoed.popup = Some(Box::new(crate::protocol::ClientShellPopupSurface {
                    terminal_id: "popup".into(),
                    title: "Popup".into(),
                    width: None,
                    height: None,
                    frame: echoed.frame.clone(),
                    mouse_reporting: false,
                    sgr_pixel_mouse: false,
                    pixel_width: 0,
                    pixel_height: 0,
                }));
            } else {
                echoed.frame.cursor = None;
            }
            assert!(prediction.observe(&echoed, now + Duration::from_millis(120)));
            assert!(!prediction.has_pending());
        }
    }

    #[test]
    fn cursor_movement_without_matching_characters_does_not_confirm_pending_input() {
        let now = Instant::now();
        let (mut prediction, mut echoed) = trained(now);
        record(
            &mut prediction,
            &echoed,
            "b",
            now + Duration::from_millis(110),
        );
        echoed.frame.cursor.as_mut().unwrap().x += 1;
        assert!(prediction.observe(&echoed, now + Duration::from_millis(120)));
        assert!(!prediction.has_pending());
    }
}
