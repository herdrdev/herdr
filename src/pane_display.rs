//! BiDi display layout for pane rows.
//!
//! The client lays out each pane row segment in visual order after it has
//! composed the row, including logical selection, search, and link
//! highlights, and before the frame reaches the outer terminal. A segment
//! never extends past the pane's inner rectangle, so borders, scrollbars, the
//! sidebar, and neighbouring panes stay in place.
//!
//! Layout only changes presentation. Terminal state, copied text, and
//! `herdr agent read` keep logical order.

use crate::protocol::CellData;

mod bidi;
#[cfg(test)]
mod tests;

/// Cheap check that does not allocate. `false` means [`layout_segment`]
/// would leave `segment` unchanged.
pub(crate) fn affects(segment: &[CellData]) -> bool {
    bidi::affects(segment)
}

/// Lays out one pane row segment in visual order, in place, without changing
/// its width.
///
/// A segment has one cell per column. A wide grapheme is a head cell followed
/// by spacer-tail cells whose symbol is empty; combining marks live inside
/// the head cell's symbol. Returns the visual-to-logical column map when
/// cells moved. When the result is not a valid permutation that keeps wide
/// graphemes intact, the segment is restored to logical order and `None` is
/// returned.
pub(crate) fn layout_segment(
    segment: &mut [CellData],
    scratch: &mut Scratch,
) -> Option<Box<[u16]>> {
    if !affects(segment) {
        return None;
    }
    layout_with(segment, scratch, bidi::reorder)
}

/// Runs `reorder` on `segment` and validates its column map.
fn layout_with(
    segment: &mut [CellData],
    scratch: &mut Scratch,
    reorder: impl FnOnce(&mut [CellData], &mut Vec<u16>, &mut Scratch) -> bool,
) -> Option<Box<[u16]>> {
    let len = segment.len();
    if len == 0 || len > usize::from(u16::MAX) {
        return None;
    }
    copy_cells(&mut scratch.original, segment);
    let mut map = std::mem::take(&mut scratch.map);
    map.clear();
    let changed = reorder(segment, &mut map, scratch);
    let result = if !changed {
        None
    } else if !valid_permutation(&scratch.original, &map, &mut scratch.seen) {
        crate::render_prof::event("pane_display.invalid_fallback");
        segment.clone_from_slice(&scratch.original);
        None
    } else if map.iter().enumerate().all(|(v, &l)| usize::from(l) == v) {
        None
    } else {
        // The returned map is the only per-row allocation, and only for
        // rows that moved.
        Some(Box::from(map.as_slice()))
    };
    scratch.map = map;
    result
}

/// Copies `source` into `target`, reusing the existing cells' string buffers.
fn copy_cells(target: &mut Vec<CellData>, source: &[CellData]) {
    target.truncate(source.len());
    let reused = target.len();
    target.clone_from_slice(&source[..reused]);
    target.extend_from_slice(&source[reused..]);
}

/// Checks that `visual_to_logical` is a bijection and that every spacer tail
/// is still drawn directly after the cell that precedes it in logical order.
fn valid_permutation(
    logical: &[CellData],
    visual_to_logical: &[u16],
    seen: &mut Vec<bool>,
) -> bool {
    let len = logical.len();
    if visual_to_logical.len() != len {
        return false;
    }
    seen.clear();
    seen.resize(len, false);
    for &source in visual_to_logical {
        let source = usize::from(source);
        if source >= len || std::mem::replace(&mut seen[source], true) {
            return false;
        }
    }
    visual_to_logical.iter().enumerate().all(|(v, &source)| {
        let source = usize::from(source);
        !logical[source].symbol.is_empty()
            || source == 0
            || (v > 0 && usize::from(visual_to_logical[v - 1]) + 1 == source)
    })
}

/// Column map for one reordered pane row segment, in screen coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowMap {
    pub(crate) x: u16,
    pub(crate) y: u16,
    visual_to_logical: Box<[u16]>,
    logical_to_visual: Box<[u16]>,
}

impl RowMap {
    pub(crate) fn new(x: u16, y: u16, visual_to_logical: Box<[u16]>) -> Self {
        let mut logical_to_visual = vec![0u16; visual_to_logical.len()];
        for (visual, &logical) in visual_to_logical.iter().enumerate() {
            if let Some(slot) = logical_to_visual.get_mut(usize::from(logical)) {
                *slot = visual as u16;
            }
        }
        Self {
            x,
            y,
            visual_to_logical,
            logical_to_visual: logical_to_visual.into_boxed_slice(),
        }
    }

    fn width(&self) -> u16 {
        self.visual_to_logical.len() as u16
    }

    pub(crate) fn contains(&self, x: u16, y: u16) -> bool {
        y == self.y && x >= self.x && x < self.x.saturating_add(self.width())
    }

    /// Logical screen column of the cell drawn at visual `screen_x`.
    pub(crate) fn to_logical(&self, screen_x: u16) -> u16 {
        self.map(screen_x, &self.visual_to_logical)
    }

    /// Visual screen column where logical `screen_x` is drawn.
    pub(crate) fn to_visual(&self, screen_x: u16) -> u16 {
        self.map(screen_x, &self.logical_to_visual)
    }

    fn map(&self, screen_x: u16, table: &[u16]) -> u16 {
        screen_x
            .checked_sub(self.x)
            .and_then(|offset| table.get(usize::from(offset)))
            .map_or(screen_x, |&mapped| self.x + mapped)
    }
}

/// Reusable buffers kept across frames. After warm-up, rows without
/// right-to-left text allocate nothing. Affected rows also allocate inside
/// `unicode-bidi`, and rows that move allocate their stored column map.
#[derive(Default)]
pub(crate) struct Scratch {
    map: Vec<u16>,
    seen: Vec<bool>,
    text: String,
    units: Vec<bidi::Unit>,
    levels: Vec<unicode_bidi::Level>,
    cells: Vec<CellData>,
    original: Vec<CellData>,
}
