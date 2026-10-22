//! Paged history above a `pane.stream` keyframe (`pane.history`).
//!
//! Row numbers cannot address history across requests: live output appends
//! rows and, at the scrollback limit, trims the oldest ones, so every index
//! shifts between pages. A cursor is instead a random token for a ghostty
//! tracked row, which follows its row as content moves and stops reporting a
//! position once that row is trimmed. Cursors live in the pane's core, under
//! the same lock as the keyframe and the reads that resolve them, so a row can
//! never move between resolving a cursor and rendering the rows above it.

use std::collections::VecDeque;
use std::ops::Range;

use base64::Engine;

use crate::ghostty::{ActiveScreen, Terminal, TrackedRow};

/// Live cursors kept per pane. Each holds a tracked pin that ghostty updates
/// on every scroll and trim, so the table stays small and evicts the least
/// recently used cursor rather than growing with every keyframe and page.
const MAX_HISTORY_CURSORS: usize = 64;

/// A page of rows directly above a cursor's row, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryPage {
    /// ANSI rows separated by `\r\n`, rendered like keyframe history.
    pub ansi: String,
    pub rows: u32,
    /// Cursor for the oldest returned row; `None` once the page reached the
    /// start of retained history.
    pub cursor: Option<String>,
}

/// The cursor is unknown (evicted, from another process, never issued), its
/// row was trimmed, or the page cannot be served; the client stops paging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryCursorExpired;

#[derive(Default)]
pub(super) struct HistoryCursors {
    /// Least recently used first.
    entries: VecDeque<(String, TrackedRow)>,
}

impl HistoryCursors {
    /// Track primary-screen row `y` under a fresh token. `None` when the row
    /// cannot be tracked, in which case the caller offers no further paging.
    pub(super) fn issue(&mut self, terminal: &mut Terminal, y: usize) -> Option<String> {
        let row = terminal.track_screen_row(u32::try_from(y).ok()?).ok()?;
        let token = new_token()?;
        if self.entries.len() == MAX_HISTORY_CURSORS {
            // Dropping the row frees its tracked pin.
            self.entries.pop_front();
        }
        self.entries.push_back((token.clone(), row));
        Some(token)
    }

    /// Resolve `token` to its row's current screen y, marking it most recently
    /// used. A trimmed row's cursor is dropped: it can never resolve again.
    fn resolve(&mut self, terminal: &Terminal, token: &str) -> Option<usize> {
        let index = self.entries.iter().position(|(entry, _)| entry == token)?;
        let entry = self.entries.remove(index)?;
        let y = entry.1.screen_row(terminal)?;
        self.entries.push_back(entry);
        Some(y)
    }

    fn forget(&mut self, token: &str) {
        self.entries.retain(|(entry, _)| entry != token);
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 128 random bits: a cursor names a row of one pane in one server process and
/// must not be guessable or reusable as a row number.
fn new_token() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).ok()?;
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Screen rows of the up to `lines` normal-screen scrollback rows directly
/// above the viewport the keyframe seed renders. Anchored to the viewport (not
/// the bottom) so history and seed meet exactly even when a local client has
/// scrolled the pane back. `None` on the alternate screen, which has no
/// scrollback of its own, or when there is nothing above the viewport.
pub(super) fn keyframe_history_rows(
    terminal: &Terminal,
    lines: usize,
) -> Result<Option<Range<usize>>, crate::ghostty::Error> {
    if lines == 0 || terminal.active_screen()? != ActiveScreen::Primary {
        return Ok(None);
    }
    let viewport_start = terminal.scrollbar()?.offset;
    if terminal.cols()? == 0 || viewport_start == 0 {
        return Ok(None);
    }
    Ok(Some(viewport_start.saturating_sub(lines)..viewport_start))
}

/// Render screen `rows` as ANSI, oldest first, without unwrapping so each
/// screen row stays one row at the terminal's width.
pub(super) fn read_rows_ansi(
    terminal: &Terminal,
    rows: Range<usize>,
) -> Result<String, crate::ghostty::Error> {
    let cols = terminal.cols()?;
    if cols == 0 || rows.is_empty() {
        return Ok(String::new());
    }
    terminal.read_ansi_screen(
        (0, rows.start as u32),
        (cols.saturating_sub(1), (rows.end - 1) as u32),
        false,
        false,
    )
}

/// Serve the page of up to `lines` rows directly above `token`'s row.
pub(super) fn read_page(
    terminal: &mut Terminal,
    cursors: &mut HistoryCursors,
    token: &str,
    lines: usize,
) -> Result<HistoryPage, HistoryCursorExpired> {
    // Cursors track primary-screen rows, but reads address the active screen.
    // While the alternate screen is up those rows cannot be rendered, and a
    // client on the alternate screen has no scrollback to page into anyway.
    if terminal.active_screen().ok() != Some(ActiveScreen::Primary) {
        cursors.forget(token);
        return Err(HistoryCursorExpired);
    }
    let end = cursors
        .resolve(terminal, token)
        .ok_or(HistoryCursorExpired)?;
    let start = end.saturating_sub(lines.max(1));
    let rows = start..end;
    let mut ansi = read_rows_ansi(terminal, rows.clone()).map_err(|_| HistoryCursorExpired)?;
    pad_trimmed_rows(&mut ansi, rows.len());
    let cursor = if start == 0 {
        None
    } else {
        // A page the client could not continue from would read as the start
        // of history, so failing to track its oldest row fails the page.
        Some(cursors.issue(terminal, start).ok_or(HistoryCursorExpired)?)
    };
    Ok(HistoryPage {
        ansi,
        rows: u32::try_from(rows.len()).map_err(|_| HistoryCursorExpired)?,
        cursor,
    })
}

/// The formatter always drops trailing blank rows. A page is prepended above
/// the rows the client already holds, so those blanks must survive or the
/// pages would close up around them: restore the separators for the missing
/// rows so the page holds exactly `rows` rows.
fn pad_trimmed_rows(ansi: &mut String, rows: usize) {
    if rows == 0 {
        return;
    }
    let rendered_separators = ansi.matches("\r\n").count();
    for _ in rendered_separators..rows - 1 {
        ansi.push_str("\r\n");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use tokio::sync::mpsc;

    use super::*;
    use crate::pane::terminal::GhosttyPaneTerminal;
    use crate::pane::PaneId;

    const COLS: u16 = 20;
    const ROWS: u16 = 5;

    fn pane(scrollback_limit_bytes: usize) -> GhosttyPaneTerminal {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = Terminal::new(COLS, ROWS, scrollback_limit_bytes).unwrap();
        GhosttyPaneTerminal::new(terminal, tx).unwrap()
    }

    fn write(pane: &GhosttyPaneTerminal, bytes: &[u8]) {
        let (tx, _rx) = mpsc::channel(64);
        pane.process_pty_bytes(PaneId::from_raw(1), 0, bytes, &tx);
    }

    fn write_numbered(pane: &GhosttyPaneTerminal, range: Range<usize>) {
        let text: String = range.map(|line| format!("line {line:05}\r\n")).collect();
        write(pane, text.as_bytes());
    }

    fn keyframe(pane: &GhosttyPaneTerminal, lines: usize) -> crate::pane::OutputSnapshot {
        let ring = crate::pane::output_ring::OutputRing::new(7, 1 << 16, COLS, ROWS, Weak::new());
        pane.output_snapshot(&ring, lines).unwrap()
    }

    /// Rendered rows as plain text: the formatter's SGR and OSC sequences are
    /// not what these tests compare.
    fn plain(ansi: &str) -> String {
        let mut text = String::with_capacity(ansi.len());
        let mut chars = ansi.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '\x1b' {
                text.push(ch);
                continue;
            }
            match chars.next() {
                Some('[') => {
                    for ch in chars.by_ref() {
                        if ('@'..='~').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' || (ch == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        text
    }

    /// The numbered lines in rendered rows, oldest first, failing on any row
    /// that is not a numbered line.
    fn numbers(ansi: &str) -> Vec<usize> {
        let text = plain(ansi);
        text.split("\r\n")
            .map(|row| {
                row.trim()
                    .strip_prefix("line ")
                    .and_then(|number| number.parse().ok())
                    .unwrap_or_else(|| panic!("unexpected row {row:?} in {text:?}"))
            })
            .collect()
    }

    fn page(pane: &GhosttyPaneTerminal, cursor: &str, lines: usize) -> HistoryPage {
        pane.history_page(cursor, lines).expect("live cursor")
    }

    #[test]
    fn pages_walk_contiguously_back_to_the_start_while_output_is_appended() {
        let pane = pane(10 * 1024 * 1024);
        write_numbered(&pane, 0..2500);

        let seed = keyframe(&pane, 1000);
        let history = numbers(seed.history.as_deref().unwrap());
        assert_eq!(history.len(), 1000);
        // The keyframe history ends at the screen: the last line written sits
        // on the screen above the cursor row, the four before it on screen too.
        assert_eq!(*history.last().unwrap(), 2500 - usize::from(ROWS));
        let mut oldest = history[0];
        let mut cursor = seed.history_cursor.expect("older rows exist");

        let mut next_line = 2500;
        let mut pages = 0;
        loop {
            // New output between page requests must not shift the next page.
            write_numbered(&pane, next_line..next_line + 37);
            next_line += 37;

            let page = page(&pane, &cursor, 500);
            let rows = numbers(&page.ansi);
            assert_eq!(rows.len(), page.rows as usize);
            let expected: Vec<usize> = (oldest - rows.len()..oldest).collect();
            assert_eq!(rows, expected, "page {pages} must end right above {oldest}");
            oldest = rows[0];
            pages += 1;
            match page.cursor {
                Some(next) => cursor = next,
                None => break,
            }
        }
        assert_eq!(oldest, 0, "paging must reach the first line written");
        assert_eq!(pages, 3);
    }

    #[test]
    fn a_lost_page_can_be_requested_again_with_the_same_cursor() {
        let pane = pane(10 * 1024 * 1024);
        write_numbered(&pane, 0..30);
        let cursor = keyframe(&pane, 10).history_cursor.unwrap();

        let first = page(&pane, &cursor, 500);
        // A lost response is retried with the same cursor and answers the same rows.
        assert_eq!(page(&pane, &cursor, 500), first);
        assert_eq!(
            numbers(&first.ansi),
            (0..30 - 5 - 10 + 1).collect::<Vec<_>>()
        );
        assert_eq!(first.cursor, None);
    }

    #[test]
    fn keyframe_offers_no_cursor_without_older_rows_or_history() {
        let pane = pane(10 * 1024 * 1024);
        write_numbered(&pane, 0..30);
        // Every history row fits in the keyframe: nothing older to page into.
        let seed = keyframe(&pane, 1000);
        assert_eq!(numbers(seed.history.as_deref().unwrap())[0], 0);
        assert_eq!(seed.history_cursor, None);
        // History not requested.
        assert_eq!(keyframe(&pane, 0).history_cursor, None);
    }

    #[test]
    fn alternate_screen_keyframe_has_no_cursor_and_pages_expire() {
        let pane = pane(10 * 1024 * 1024);
        write_numbered(&pane, 0..100);
        let cursor = keyframe(&pane, 10).history_cursor.unwrap();

        write(&pane, b"\x1b[?1049h");
        assert_eq!(keyframe(&pane, 10).history_cursor, None);
        assert_eq!(pane.history_page(&cursor, 500), Err(HistoryCursorExpired));
    }

    #[test]
    fn unknown_and_trimmed_cursors_expire() {
        let pane = pane(4096);
        write_numbered(&pane, 0..200);
        assert_eq!(
            pane.history_page("not-a-cursor", 10),
            Err(HistoryCursorExpired)
        );

        let cursor = keyframe(&pane, 10).history_cursor.unwrap();
        let older = pane.history_page(&cursor, 1).unwrap().cursor.unwrap();
        assert_eq!(pane.core.lock().unwrap().history_cursors.len(), 2);
        // Push both cursors' rows out of the small scrollback.
        write_numbered(&pane, 200..5000);
        assert_eq!(pane.history_page(&cursor, 10), Err(HistoryCursorExpired));
        assert_eq!(pane.history_page(&older, 10), Err(HistoryCursorExpired));
        assert_eq!(
            pane.core.lock().unwrap().history_cursors.len(),
            0,
            "a trimmed cursor is dropped with its tracked row"
        );
    }

    #[test]
    fn page_keeps_blank_rows_directly_above_the_cursor() {
        let pane = pane(10 * 1024 * 1024);
        write(&pane, b"line 00000\r\n\r\n\r\nline 00003\r\n");
        write_numbered(&pane, 4..40);
        // Viewport starts at row 36; 33 keyframe rows leave rows 0..3 above.
        let cursor = keyframe(&pane, 33).history_cursor.unwrap();

        let page = page(&pane, &cursor, 3);
        assert_eq!(page.rows, 3);
        let text = plain(&page.ansi);
        let rows: Vec<&str> = text.split("\r\n").map(str::trim).collect();
        assert_eq!(rows, ["line 00000", "", ""]);
        assert!(page.cursor.is_none());
    }

    #[test]
    fn soft_wrapped_rows_count_as_separate_rows() {
        let pane = pane(10 * 1024 * 1024);
        let long = "x".repeat(usize::from(COLS) * 2 + 3);
        write(&pane, format!("{long}\r\n").as_bytes());
        write_numbered(&pane, 0..40);
        // The long line wraps onto rows 0..3; the viewport starts at row 39.
        let cursor = keyframe(&pane, 36).history_cursor.unwrap();

        let page = page(&pane, &cursor, 500);
        assert_eq!(page.rows, 3);
        assert_eq!(page.ansi.matches("\r\n").count(), 2);
    }

    #[test]
    fn cursor_table_evicts_least_recently_used() {
        let pane = pane(10 * 1024 * 1024);
        write_numbered(&pane, 0..200);
        let first = keyframe(&pane, 10).history_cursor.unwrap();
        let second = keyframe(&pane, 10).history_cursor.unwrap();
        // Using the first makes the second the least recently used.
        assert!(pane.history_page(&first, 1).is_ok());
        for _ in 0..MAX_HISTORY_CURSORS - 3 {
            keyframe(&pane, 10).history_cursor.unwrap();
        }
        assert_eq!(
            pane.core.lock().unwrap().history_cursors.len(),
            MAX_HISTORY_CURSORS
        );

        keyframe(&pane, 10).history_cursor.unwrap();
        assert_eq!(
            pane.core.lock().unwrap().history_cursors.len(),
            MAX_HISTORY_CURSORS
        );
        assert_eq!(pane.history_page(&second, 1), Err(HistoryCursorExpired));
        assert!(pane.history_page(&first, 1).is_ok());
    }
}
