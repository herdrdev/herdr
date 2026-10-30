//! Unicode Bidirectional Algorithm (UAX #9) for pane rows.
//!
//! Matches WezTerm's `bidi_direction = "AutoLeftToRight"`: each pane row
//! segment is a paragraph with a left-to-right base direction, and
//! right-to-left runs are drawn in visual order. Runs of box-drawing
//! characters split a segment into independent paragraphs and never move, so
//! table frames keep their columns. Rule L4 mirroring covers common paired
//! brackets.

use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo};

use super::Scratch;
use crate::protocol::CellData;

/// Kitty graphics placeholder; rows carrying images are never reordered.
const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// A grapheme cell plus the spacer-tail cells that follow it.
pub(super) struct Unit {
    first_cell: usize,
    end_cell: usize,
    text_offset: usize,
}

/// Whether `segment` holds right-to-left text that may move. Does not
/// allocate; rows with skip cells or Kitty image placeholders never move.
pub(super) fn affects(segment: &[CellData]) -> bool {
    // Every scalar below U+0580 has a UTF-8 lead byte below 0xD6, so
    // ASCII and Latin rows cost one byte compare per symbol byte.
    if !segment
        .iter()
        .any(|cell| cell.symbol.bytes().any(|byte| byte >= 0xD6))
    {
        return false;
    }
    let mut right_to_left = false;
    for cell in segment {
        if cell.skip {
            return false;
        }
        for c in cell.symbol.chars() {
            if c == KITTY_PLACEHOLDER {
                return false;
            }
            right_to_left |= !c.is_ascii() && starts_right_to_left(c);
        }
    }
    right_to_left
}

/// Rewrites `segment` in visual order and writes the visual-to-logical
/// column map into `visual_to_logical`, one entry per cell. Returns `false`
/// when nothing changed.
pub(super) fn reorder(
    segment: &mut [CellData],
    visual_to_logical: &mut Vec<u16>,
    scratch: &mut Scratch,
) -> bool {
    let len = segment.len();
    visual_to_logical.clear();
    visual_to_logical.extend(0..len as u16);
    let mut changed = false;
    let mut start = 0;
    while start < len {
        if is_box_drawing(&segment[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < len && !is_box_drawing(&segment[end]) {
            end += 1;
        }
        changed |= reorder_paragraph(segment, start..end, visual_to_logical, scratch);
        start = end;
    }
    if !changed {
        return false;
    }
    super::copy_cells(&mut scratch.cells, segment);
    for (target, &source) in segment.iter_mut().zip(visual_to_logical.iter()) {
        target.clone_from(&scratch.cells[usize::from(source)]);
    }
    true
}

/// Resolves one paragraph, mirrors brackets at right-to-left levels in
/// place, and writes its visual order into `visual_to_logical`.
fn reorder_paragraph(
    segment: &mut [CellData],
    range: std::ops::Range<usize>,
    visual_to_logical: &mut [u16],
    scratch: &mut Scratch,
) -> bool {
    scratch.text.clear();
    scratch.units.clear();
    for index in range.clone() {
        let symbol = segment[index].symbol.as_str();
        if symbol.is_empty() {
            if let Some(unit) = scratch.units.last_mut() {
                unit.end_cell = index + 1;
                continue;
            }
        }
        scratch.units.push(Unit {
            first_cell: index,
            end_cell: index + 1,
            text_offset: scratch.text.len(),
        });
        scratch
            .text
            .push_str(if symbol.is_empty() { " " } else { symbol });
    }

    let info = ParagraphBidiInfo::new(&scratch.text, Some(Level::ltr()));
    if !info.has_rtl() {
        return false;
    }
    let levels = info.reordered_levels(0..scratch.text.len());
    scratch.levels.clear();
    scratch
        .levels
        .extend(scratch.units.iter().map(|unit| levels[unit.text_offset]));

    let mut mirrored = false;
    for (unit, level) in scratch.units.iter().zip(&scratch.levels) {
        if !level.is_rtl() {
            continue;
        }
        let cell = &mut segment[unit.first_cell];
        let mut chars = cell.symbol.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if let Some(mirror) = mirrored_bracket(c) {
                cell.symbol.clear();
                cell.symbol.push(mirror);
                mirrored = true;
            }
        }
    }

    let mut column = range.start;
    let mut moved = false;
    for logical in ParagraphBidiInfo::reorder_visual(&scratch.levels) {
        let unit = &scratch.units[logical];
        for cell in unit.first_cell..unit.end_cell {
            moved |= cell != column;
            visual_to_logical[column] = cell as u16;
            column += 1;
        }
    }
    moved || mirrored
}

fn starts_right_to_left(c: char) -> bool {
    matches!(
        unicode_bidi::bidi_class(c),
        BidiClass::R
            | BidiClass::AL
            | BidiClass::AN
            | BidiClass::RLE
            | BidiClass::RLO
            | BidiClass::RLI
            | BidiClass::FSI
    )
}

fn is_box_drawing(cell: &CellData) -> bool {
    cell.symbol
        .chars()
        .next()
        .is_some_and(|c| ('\u{2500}'..='\u{259F}').contains(&c))
}

/// Rule L4 for common paired brackets (Bidi_Mirroring_Glyph).
fn mirrored_bracket(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        _ => return None,
    })
}
