//! Reads omp's `ask` selector off a pane screen and drives it with arrow keys.
//!
//! omp 18.x draws a single-select question as a boxed list (`ask-dialog.ts`):
//!
//! ```text
//! │   ○ Ship it now                      │
//! │ ❯ ○ Wait for review (Recommended)    │
//! │   ○ Other (type your own)            │
//! ```
//!
//! The highlighted row carries the cursor glyph in a two-column slot that is
//! blank on every other row; wrapped labels and descriptions continue on lines
//! indented past that slot. The rows are matched against the option labels in
//! order, so the highlight is only trusted when exactly one contiguous window
//! of labels explains every visible row. Anything else is "not found": the
//! engine never guesses which row it is looking at.

use crate::api::schema::ErrorBody;

/// omp's trailing free-text row, appended after every option.
pub(crate) const OTHER_OPTION: &str = "Other (type your own)";
const RECOMMENDED_SUFFIX: &str = " (Recommended)";
/// Labels omp reserves next to `Other` when disambiguating option labels
/// (`displayOptionLabels` in ask-dialog.ts); an option equal to one of them is
/// rendered with a numeric suffix.
const GUEST_ACTION_LABELS: [&str; 2] = ["Chat about this", "Next →"];

/// Cursor glyphs of omp's unicode, nerd-font and ascii symbol presets.
const CURSOR_GLYPHS: [char; 3] = ['❯', '\u{f054}', '>'];
/// Single-select radio markers of the same presets, unselected and selected.
const RADIO_MARKERS: [&str; 6] = ["○", "◉", "\u{f10c}", "\u{f192}", "( )", "(o)"];
/// Characters that may sit left of the cursor slot: padding and the panel's
/// left border.
const LEFT_EDGE: [char; 5] = [' ', '│', '|', '┃', '║'];
/// Characters that may close a row on the right: the panel border and the
/// scrollbar track/thumb of a long list.
const RIGHT_EDGE: [char; 12] = ['│', '|', '┃', '║', '█', '▐', '▌', '░', '▒', '▓', '╎', '┆'];
const NOTE_MARKER: &str = "✎ note";
/// Status line text omp's older select lists show once the user typed a
/// filter; a filtered list hides and reorders rows, so it is never driven.
const FILTER_STATUS: &str = "Search: ";
/// How many lines below the option block the filter status may sit.
const FILTER_STATUS_REACH: usize = 4;

/// The rows omp renders for a single-select question, in screen order: the
/// option labels as displayed (the recommended one badged, collisions with
/// omp's reserved rows suffixed) followed by `Other`. Index `i < options.len()`
/// is option `i`.
pub(crate) fn display_rows(options: &[String], recommended: Option<usize>) -> Vec<String> {
    let mut taken: Vec<String> = std::iter::once(OTHER_OPTION)
        .chain(GUEST_ACTION_LABELS)
        .map(str::to_owned)
        .collect();
    let mut rows = Vec::with_capacity(options.len() + 1);
    for (index, option) in options.iter().enumerate() {
        let mut base = sanitize_carriage_returns(option);
        if recommended == Some(index) && !base.ends_with(RECOMMENDED_SUFFIX) {
            base.push_str(RECOMMENDED_SUFFIX);
        }
        let mut candidate = base.clone();
        let mut suffix = 2;
        while taken.contains(&candidate) {
            candidate = format!("{base} ({suffix})");
            suffix += 1;
        }
        taken.push(candidate.clone());
        rows.push(candidate);
    }
    rows.push(OTHER_OPTION.to_owned());
    rows
}

/// Mirrors omp's `sanitizeCarriageReturns`: CRLF → LF, CR runs → one space.
fn sanitize_carriage_returns(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    let text = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    let mut in_cr = false;
    for ch in text.chars() {
        if ch == '\r' {
            if !in_cr {
                out.push(' ');
            }
            in_cr = true;
        } else {
            in_cr = false;
            out.push(ch);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScreenError {
    /// No highlighted row whose surrounding block matches the expected rows.
    NotFound,
    /// The selector is on screen but a filter query is active.
    Filtered,
    /// More than one block on screen could be the selector.
    Ambiguous,
}

/// Where the selector's highlight sits, as an index into the expected rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectorView {
    pub highlighted: usize,
}

/// Find the selector for `rows` on `screen` and report its highlighted row.
pub(crate) fn locate(screen: &str, rows: &[String]) -> Result<SelectorView, ScreenError> {
    let lines: Vec<&str> = screen.lines().map(trim_right_edge).collect();
    let expected: Vec<String> = rows.iter().map(|row| normalize(row)).collect();
    let mut found = None;
    let mut filtered = false;
    for (index, line) in lines.iter().enumerate() {
        let Some(cursor) = cursor_row(line) else {
            continue;
        };
        let block = block_around(&lines, index, &cursor);
        // Checked before alignment: a filtered list shows a subset of rows,
        // often in fuzzy-match order, so it rarely aligns at all.
        if block_is_filtered(&lines, index, block.end) {
            filtered = true;
            continue;
        }
        let Some(start) = align(&block.rows, &expected) else {
            continue;
        };
        if found.is_some() {
            return Err(ScreenError::Ambiguous);
        }
        found = Some(SelectorView {
            highlighted: start + block.highlighted,
        });
    }
    match found {
        Some(view) => Ok(view),
        None if filtered => Err(ScreenError::Filtered),
        None => Err(ScreenError::NotFound),
    }
}

/// A highlighted row: its left edge (border/padding) and the text after the
/// cursor slot.
struct CursorRow<'a> {
    edge: &'a str,
    text: &'a str,
}

fn cursor_row(line: &str) -> Option<CursorRow<'_>> {
    let rest = line.trim_start_matches(LEFT_EDGE);
    let edge = &line[..line.len() - rest.len()];
    let mut chars = rest.chars();
    let glyph = chars.next()?;
    if !CURSOR_GLYPHS.contains(&glyph) || chars.next() != Some(' ') {
        return None;
    }
    let text = &rest[glyph.len_utf8() + 1..];
    if text.is_empty() || text.starts_with(char::is_whitespace) {
        return None;
    }
    Some(CursorRow { edge, text })
}

/// The option rows of one selector block, the highlighted row's position in
/// them, and the screen line just past the block.
struct Block {
    rows: Vec<String>,
    highlighted: usize,
    end: usize,
}

enum Neighbor {
    Row(String),
    Continuation,
    End,
}

/// Classify a line next to the highlighted row: same left edge, then a blank
/// cursor slot, then either row text or (further indented) continuation text.
fn neighbor(line: &str, edge: &str) -> Neighbor {
    let Some(after_edge) = line.strip_prefix(edge) else {
        return Neighbor::End;
    };
    let Some(after_slot) = after_edge.strip_prefix("  ") else {
        return Neighbor::End;
    };
    if after_slot.trim().is_empty() || is_list_status(after_slot) {
        return Neighbor::End;
    }
    if after_slot.starts_with(char::is_whitespace) {
        Neighbor::Continuation
    } else {
        Neighbor::Row(normalize_row(after_slot))
    }
}

/// The status line older select lists print under their rows ("Type to
/// search", "(3/12)", "Search: q", "No matching options"); it sits at row
/// indentation but is not an option.
fn is_list_status(text: &str) -> bool {
    let counter = text
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
        .is_some_and(|(count, _)| {
            count
                .split_once('/')
                .is_some_and(|(a, b)| is_number(a) && is_number(b))
        });
    counter
        || text.starts_with("Type to search")
        || text.starts_with("No matching options")
        || text.contains(FILTER_STATUS)
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn block_around(lines: &[&str], index: usize, cursor: &CursorRow<'_>) -> Block {
    let mut above = Vec::new();
    for line in lines[..index].iter().rev() {
        match neighbor(line, cursor.edge) {
            Neighbor::Row(text) => above.push(text),
            Neighbor::Continuation => {}
            Neighbor::End => break,
        }
    }
    let mut rows: Vec<String> = above.into_iter().rev().collect();
    let highlighted = rows.len();
    rows.push(normalize_row(cursor.text));
    let mut end = index + 1;
    while let Some(line) = lines.get(end) {
        match neighbor(line, cursor.edge) {
            Neighbor::Row(text) => rows.push(text),
            Neighbor::Continuation => {}
            Neighbor::End => break,
        }
        end += 1;
    }
    Block {
        rows,
        highlighted,
        end,
    }
}

/// The unique offset at which `visible` lines up with a contiguous run of
/// `expected` rows; `None` when no offset or more than one fits.
fn align(visible: &[String], expected: &[String]) -> Option<usize> {
    if visible.is_empty() || visible.len() > expected.len() {
        return None;
    }
    let mut fits = (0..=expected.len() - visible.len()).filter(|&start| {
        visible
            .iter()
            .zip(&expected[start..])
            .all(|(row, label)| row_shows_label(row, label))
    });
    let start = fits.next()?;
    fits.next().is_none().then_some(start)
}

/// A rendered row shows `label` when it is the whole label or a leading part
/// of it: omp wraps long labels onto continuation lines and older select
/// lists cut them at the pane width, with or without an ellipsis.
fn row_shows_label(row: &str, label: &str) -> bool {
    if row == label {
        return true;
    }
    let cut = row
        .strip_suffix('…')
        .or_else(|| row.strip_suffix("..."))
        .unwrap_or(row)
        .trim_end();
    !cut.is_empty() && label.starts_with(cut)
}

/// Whether a filter status sits on the highlighted row's block or just below
/// it (the status line can parse as a row of the block).
fn block_is_filtered(lines: &[&str], highlighted: usize, end: usize) -> bool {
    lines
        .iter()
        .take(end + FILTER_STATUS_REACH)
        .skip(highlighted)
        .any(|line| line.contains(FILTER_STATUS))
}

fn trim_right_edge(line: &str) -> &str {
    let mut line = line.trim_end();
    // Border plus scrollbar at most; more would start eating label text.
    for _ in 0..2 {
        match line.strip_suffix(RIGHT_EDGE) {
            Some(rest) => line = rest.trim_end(),
            None => break,
        }
    }
    line
}

fn normalize_row(text: &str) -> String {
    let mut text = text.trim();
    for marker in RADIO_MARKERS {
        if let Some(rest) = text.strip_prefix(marker) {
            if let Some(rest) = rest.strip_prefix(' ') {
                text = rest;
                break;
            }
        }
    }
    let text = text.strip_suffix(NOTE_MARKER).unwrap_or(text);
    normalize(text)
}

/// Collapse whitespace and drop inline-markdown emphasis/code markers, which
/// omp renders as styling rather than characters.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['`', '*'], "")
}

/// A pane hosting the selector: screen reads, key writes, and the pause
/// between polls (a no-op in tests).
pub(crate) trait SelectorPane {
    fn read_screen(&mut self) -> Result<String, ErrorBody>;
    fn send_keys(&mut self, keys: &[&str]) -> Result<(), ErrorBody>;
    fn pause(&mut self);
}

/// How many reads (one pause apart) to allow for the selector to appear and
/// for the highlight to settle after arrows.
pub(crate) const SETTLE_POLLS: usize = 30;

/// Move the highlight to `rows[target]` and press Enter once the screen shows
/// it there. Enter is never sent unless the highlighted row matches.
pub(crate) fn select_and_submit(
    pane: &mut impl SelectorPane,
    rows: &[String],
    target: usize,
) -> Result<(), ErrorBody> {
    let view = settle(pane, rows, |_| true)?.map_err(|err| ui_error(err, None))?;
    // Step toward the target without crossing a list edge: omp's ask dialog
    // clamps there while older select lists wrap, so the direct path is the
    // only one with the same result on both.
    let presses = target.abs_diff(view.highlighted);
    let key = if target > view.highlighted {
        "down"
    } else {
        "up"
    };
    if presses > 0 {
        pane.send_keys(&vec![key; presses])?;
    }
    match settle(pane, rows, |view| view.highlighted == target)? {
        Ok(_) => pane.send_keys(&["enter"]),
        Err(err) => Err(ui_error(err, Some(&rows[target]))),
    }
}

/// The last read outcome after polling until `done` holds or the poll budget
/// runs out: `Ok(view)` when it held, otherwise why it did not.
#[allow(clippy::type_complexity)]
fn settle(
    pane: &mut impl SelectorPane,
    rows: &[String],
    done: impl Fn(&SelectorView) -> bool,
) -> Result<Result<SelectorView, Settle>, ErrorBody> {
    let mut last = Settle::Screen(ScreenError::NotFound);
    for poll in 0..SETTLE_POLLS {
        if poll > 0 {
            pane.pause();
        }
        let screen = pane.read_screen()?;
        match locate(&screen, rows) {
            Ok(view) if done(&view) => return Ok(Ok(view)),
            Ok(view) => last = Settle::Elsewhere(view),
            // A filter is the user's state, not a redraw in flight.
            Err(ScreenError::Filtered) => return Ok(Err(Settle::Screen(ScreenError::Filtered))),
            Err(err) => last = Settle::Screen(err),
        }
    }
    Ok(Err(last))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settle {
    Screen(ScreenError),
    Elsewhere(SelectorView),
}

fn ui_error(settle: Settle, wanted: Option<&str>) -> ErrorBody {
    let (code, message) = match (settle, wanted) {
        (Settle::Elsewhere(_), Some(wanted)) => (
            "selection_mismatch",
            format!("the selector did not highlight {wanted:?}; nothing was submitted"),
        ),
        (Settle::Screen(ScreenError::Filtered), _) => (
            "ask_ui_not_found",
            "the question selector is filtered; clear the search in the terminal".to_owned(),
        ),
        (Settle::Screen(ScreenError::Ambiguous), _) => (
            "ask_ui_not_found",
            "more than one selector matches the question on screen".to_owned(),
        ),
        _ => (
            "ask_ui_not_found",
            "the question selector is not on the pane screen".to_owned(),
        ),
    };
    ErrorBody {
        code: code.to_owned(),
        message,
    }
}

#[cfg(test)]
mod tests;
