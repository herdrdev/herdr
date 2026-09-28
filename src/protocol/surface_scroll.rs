//! Optional scroll-aware delivery of retained pane surface patches.
//!
//! Scrolling output moves every visible row, so a row diff resends the whole
//! pane on each frame. This codec sends each pane's vertical shift once, plus
//! only the rows that still differ after it. The receiver expands the result
//! back into an ordinary [`PaneSurfacePatch`] against its retained grid.

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};

use super::{
    CellData, FrameData, PaneSurfaceFrame, PaneSurfacePatch, PaneSurfacePatchRow, ServerMessage,
    SurfaceRect, MAX_FRAME_SIZE,
};

pub(crate) const CAPABILITY: &str = "surface_scroll";
pub(crate) const MESSAGE_KIND: &str = "endpoint.surface-scroll.v1";

// This byte layout is frozen with MESSAGE_KIND: one count byte, then per
// scroll x, y, width, height (u16 LE) and shift (i16 LE), then one framed
// `ServerMessage::PaneSurfacePatch` decoded by the shared bounded reader.
const MAX_SCROLLS: usize = 64;
const SCROLL_BYTES: usize = 10;

/// Reorders the rows of one pane region before the patch rows apply.
///
/// A positive `shift` moves content up: row `y` shows the previous row
/// `y + shift`. Rows that scroll out of the region rotate into the vacated
/// rows, so applying a scroll never clones cell content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceScroll {
    pub(crate) rect: SurfaceRect,
    pub(crate) shift: i16,
}

#[derive(Debug)]
pub(crate) struct ScrollPatch {
    pub(crate) scrolls: Vec<SurfaceScroll>,
    pub(crate) patch: PaneSurfacePatch,
}

/// The row swaps that define [`SurfaceScroll`]. Both peers must derive the
/// same row order, so encoder and decoder share this one sequence.
fn for_each_swap(height: usize, shift: i16, mut swap: impl FnMut(usize, usize)) {
    let distance = usize::from(shift.unsigned_abs());
    if shift > 0 {
        for y in 0..height - distance {
            swap(y, y + distance);
        }
    } else {
        for y in (distance..height).rev() {
            swap(y, y - distance);
        }
    }
}

fn scroll_fits(scroll: &SurfaceScroll, width: u16, height: u16) -> bool {
    let rect = scroll.rect;
    rect.width > 0
        && rect.height >= 2
        && scroll.shift != 0
        && scroll.shift.unsigned_abs() < rect.height
        && rect
            .x
            .checked_add(rect.width)
            .is_some_and(|end| end <= width)
        && rect
            .y
            .checked_add(rect.height)
            .is_some_and(|end| end <= height)
}

fn rects_overlap(a: SurfaceRect, b: SurfaceRect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/// Pane regions never overlap. Requiring that bounds expansion to one copy of the grid.
fn scrolls_disjoint(scrolls: &[SurfaceScroll]) -> bool {
    scrolls.iter().enumerate().all(|(index, scroll)| {
        scrolls[index + 1..]
            .iter()
            .all(|other| !rects_overlap(scroll.rect, other.rect))
    })
}

fn row_fits(row: &PaneSurfacePatchRow, width: u16, height: u16) -> bool {
    row.y < height && usize::from(row.x).saturating_add(row.cells.len()) <= usize::from(width)
}

/// Applies one validated scroll to a `width`-wide row-major grid.
fn apply_scroll(cells: &mut [CellData], width: u16, scroll: &SurfaceScroll) {
    let rect = scroll.rect;
    let stride = usize::from(width);
    let row_start = |y: usize| (usize::from(rect.y) + y) * stride + usize::from(rect.x);
    for_each_swap(usize::from(rect.height), scroll.shift, |a, b| {
        let (a, b) = (row_start(a), row_start(b));
        for x in 0..usize::from(rect.width) {
            cells.swap(a + x, b + x);
        }
    });
}

/// One pane row as the patch leaves it: committed cells overlaid by the patch
/// spans that touch the row, read in place so detection copies nothing.
struct RowView<'a> {
    base: &'a [CellData],
    left: usize,
    spans: Vec<&'a PaneSurfacePatchRow>,
}

impl RowView<'_> {
    fn cell(&self, x: usize) -> &CellData {
        let column = self.left + x;
        for span in self.spans.iter().rev() {
            let start = usize::from(span.x);
            if column >= start && column < start + span.cells.len() {
                return &span.cells[column - start];
            }
        }
        &self.base[x]
    }

    fn hash(&self) -> u64 {
        (0..self.base.len()).fold(0, |hash, x| cell_hash(hash, self.cell(x)))
    }
}

// Row fingerprints only nominate a shift; the residual diff compares real
// cells, so a collision can cost bytes but never correctness.
fn mix(hash: u64, value: u64) -> u64 {
    (hash.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95)
}

fn cell_hash(hash: u64, cell: &CellData) -> u64 {
    let hash = cell
        .symbol
        .bytes()
        .fold(hash, |hash, byte| mix(hash, u64::from(byte)));
    let hash = mix(hash, u64::from(cell.fg) << 32 | u64::from(cell.bg));
    mix(
        hash,
        u64::from(cell.modifier)
            | u64::from(cell.skip) << 16
            | cell.hyperlink.map_or(0, |index| u64::from(index) + 1) << 17,
    )
}

fn fully_inside(row: &PaneSurfacePatchRow, rect: SurfaceRect) -> bool {
    row.y >= rect.y
        && row.y - rect.y < rect.height
        && row.x >= rect.x
        && usize::from(row.x) + row.cells.len() <= usize::from(rect.x) + usize::from(rect.width)
}

/// Finds the shift that best explains one pane's patch and returns it with the
/// rows that still differ afterwards, or `None` when the shift would not at
/// least halve the cells this pane's patch carries.
fn pane_scroll(
    frame: &FrameData,
    rect: SurfaceRect,
    rows: &[PaneSurfacePatchRow],
) -> Option<(i16, Vec<PaneSurfacePatchRow>)> {
    let (width, height) = (usize::from(rect.width), usize::from(rect.height));
    let stride = usize::from(frame.width);
    let previous = |y: usize| {
        let start = (usize::from(rect.y) + y) * stride + usize::from(rect.x);
        &frame.cells[start..start + width]
    };

    let mut next = (0..height)
        .map(|y| RowView {
            base: previous(y),
            left: usize::from(rect.x),
            spans: Vec::new(),
        })
        .collect::<Vec<_>>();
    let mut patch_cells = 0;
    for row in rows {
        let overlaps = row.y >= rect.y
            && row.y - rect.y < rect.height
            && usize::from(row.x) < usize::from(rect.x) + width
            && usize::from(row.x) + row.cells.len() > usize::from(rect.x);
        if overlaps {
            next[usize::from(row.y - rect.y)].spans.push(row);
            if fully_inside(row, rect) {
                patch_cells += row.cells.len();
            }
        }
    }
    // A shift can only beat the plain patch when at least two rows changed.
    if next.iter().filter(|row| !row.spans.is_empty()).count() < 2 {
        return None;
    }

    let old_hashes = next
        .iter()
        .map(|row| row.base.iter().fold(0, cell_hash))
        .collect::<Vec<_>>();
    let new_hashes = next
        .iter()
        .zip(&old_hashes)
        .map(|(row, &old)| {
            if row.spans.is_empty() {
                old
            } else {
                row.hash()
            }
        })
        .collect::<Vec<_>>();
    let matches = |shift: isize| {
        (0..height)
            .filter(|&y| {
                let source = y as isize + shift;
                source >= 0
                    && (source as usize) < height
                    && new_hashes[y] == old_hashes[source as usize]
            })
            .count()
    };
    let unshifted = matches(0);
    let (mut best_shift, mut best) = (0isize, unshifted);
    for distance in 1..height {
        if height - distance <= best {
            break;
        }
        for shift in [distance as isize, -(distance as isize)] {
            let count = matches(shift);
            if count > best {
                (best_shift, best) = (shift, count);
            }
        }
    }
    if best_shift == 0 {
        return None;
    }
    let shift = i16::try_from(best_shift).ok()?;

    let mut order = (0..height).collect::<Vec<_>>();
    for_each_swap(height, shift, |a, b| order.swap(a, b));
    let mut residual = Vec::new();
    let mut residual_cells = 0;
    for (y, &source) in order.iter().enumerate() {
        let (base, target) = (previous(source), &next[y]);
        let mut x = 0;
        while x < width {
            if base[x] == *target.cell(x) {
                x += 1;
                continue;
            }
            let start = x;
            while x < width && base[x] != *target.cell(x) {
                x += 1;
            }
            residual_cells += x - start;
            if residual_cells * 2 >= patch_cells {
                return None;
            }
            residual.push(PaneSurfacePatchRow {
                x: rect.x + start as u16,
                y: rect.y + y as u16,
                cells: (start..x).map(|x| target.cell(x).clone()).collect(),
            });
        }
    }
    Some((shift, residual))
}

fn encoded_size(value: &impl serde::Serialize) -> Option<usize> {
    let mut writer = bincode::enc::write::SizeWriter::default();
    bincode::serde::encode_into_writer(value, &mut writer, bincode::config::standard()).ok()?;
    Some(writer.bytes_written)
}

/// Encodes `patch` against the committed `last` surface as a scroll message
/// when some pane's shift at least halves its patched cells. Returns `None`
/// otherwise, so ordinary edits never pay for the wrapper.
pub(crate) fn message(last: &PaneSurfaceFrame, patch: &PaneSurfacePatch) -> Option<ServerMessage> {
    let frame = &last.frame;
    if frame.cells.len() != usize::from(frame.width) * usize::from(frame.height) {
        return None;
    }
    let mut scrolls = Vec::new();
    let mut residual = Vec::new();
    for pane in &patch.panes {
        let rect = pane.inner_rect;
        // Both peers hold the committed geometry; never scroll a region that moved.
        let committed = last
            .panes
            .iter()
            .any(|existing| existing.pane_id == pane.pane_id && existing.inner_rect == rect);
        let scroll = SurfaceScroll { rect, shift: 1 };
        if !committed || !scroll_fits(&scroll, frame.width, frame.height) {
            continue;
        }
        if let Some((shift, rows)) = pane_scroll(frame, rect, &patch.rows) {
            scrolls.push(SurfaceScroll { rect, shift });
            residual.extend(rows);
            if scrolls.len() == MAX_SCROLLS {
                break;
            }
        }
    }
    if scrolls.is_empty() {
        return None;
    }

    // Rows that straddle a scrolled region keep their final values, so they
    // stay in the patch and apply after the scroll.
    let rows = patch
        .rows
        .iter()
        .filter(|row| !scrolls.iter().any(|scroll| fully_inside(row, scroll.rect)))
        .cloned()
        .chain(residual)
        .collect();
    let inner = ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
        boot_id: patch.boot_id.clone(),
        projection_revision: patch.projection_revision,
        base_surface_revision: patch.base_surface_revision,
        surface_revision: patch.surface_revision,
        rows,
        panes: patch.panes.clone(),
        cursor: patch.cursor.clone(),
    });
    let mut bytes = Vec::with_capacity(1 + scrolls.len() * SCROLL_BYTES);
    bytes.push(scrolls.len() as u8);
    for scroll in &scrolls {
        let rect = scroll.rect;
        for value in [rect.x, rect.y, rect.width, rect.height] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&scroll.shift.to_le_bytes());
    }
    super::write_message(&mut bytes, &inner).ok()?;
    let message = ServerMessage::EndpointControl {
        kind: MESSAGE_KIND.into(),
        data: STANDARD_NO_PAD.encode(bytes),
    };
    (encoded_size(&message)? <= MAX_FRAME_SIZE).then_some(message)
}

/// Decodes one scroll message. Geometry is validated against the receiver's
/// baseline by [`apply`], never trusted here.
pub(crate) fn decode(data: &str) -> Result<ScrollPatch, String> {
    let limit = base64::encoded_len(MAX_FRAME_SIZE, false).unwrap_or(usize::MAX);
    if data.len() > limit {
        return Err("surface scroll exceeds the frame limit".into());
    }
    let bytes = STANDARD_NO_PAD
        .decode(data)
        .map_err(|error| format!("invalid surface scroll: {error}"))?;
    let count = usize::from(*bytes.first().ok_or("empty surface scroll")?);
    if count == 0 || count > MAX_SCROLLS {
        return Err("surface scroll has an invalid scroll count".into());
    }
    let header = 1 + count * SCROLL_BYTES;
    if bytes.len() < header {
        return Err("surface scroll is truncated".into());
    }
    let scrolls = bytes[1..header]
        .chunks_exact(SCROLL_BYTES)
        .map(|chunk| {
            let value = |at: usize| u16::from_le_bytes([chunk[at], chunk[at + 1]]);
            SurfaceScroll {
                rect: SurfaceRect {
                    x: value(0),
                    y: value(2),
                    width: value(4),
                    height: value(6),
                },
                shift: i16::from_le_bytes([chunk[8], chunk[9]]),
            }
        })
        .collect();
    let mut frame = &bytes[header..];
    let decoded = super::read_message::<_, ServerMessage>(&mut frame, MAX_FRAME_SIZE);
    if !frame.is_empty() {
        return Err("surface scroll has trailing bytes".into());
    }
    match decoded {
        Ok(ServerMessage::PaneSurfacePatch(patch)) => Ok(ScrollPatch { scrolls, patch }),
        Ok(_) => Err("surface scroll does not carry a pane patch".into()),
        Err(error) => Err(format!("invalid surface scroll patch: {error}")),
    }
}

/// Applies a decoded scroll patch to a retained grid and returns the ordinary
/// patch a receiver without scroll support would have needed. The grid is
/// untouched when validation fails.
pub(crate) fn apply(
    cells: &mut [CellData],
    width: u16,
    height: u16,
    scroll_patch: ScrollPatch,
) -> Result<PaneSurfacePatch, String> {
    let ScrollPatch { scrolls, mut patch } = scroll_patch;
    if cells.len() != usize::from(width) * usize::from(height) {
        return Err("surface scroll baseline has an invalid grid".into());
    }
    if !scrolls
        .iter()
        .all(|scroll| scroll_fits(scroll, width, height))
        || !scrolls_disjoint(&scrolls)
        || !patch.rows.iter().all(|row| row_fits(row, width, height))
    {
        return Err("surface scroll exceeds the cell baseline".into());
    }
    for scroll in &scrolls {
        apply_scroll(cells, width, scroll);
    }
    for row in &patch.rows {
        let start = usize::from(row.y) * usize::from(width) + usize::from(row.x);
        cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
    }
    // Each scrolled row is emitted once with its final cells, so the expanded patch keeps
    // only the parts of other rows outside every region. Receivers require disjoint rows.
    let mut rows = Vec::with_capacity(patch.rows.len());
    for row in std::mem::take(&mut patch.rows) {
        rows.extend(outside_scrolls(row, &scrolls));
    }
    for scroll in &scrolls {
        let rect = scroll.rect;
        for y in rect.y..rect.y + rect.height {
            let start = usize::from(y) * usize::from(width) + usize::from(rect.x);
            rows.push(PaneSurfacePatchRow {
                x: rect.x,
                y,
                cells: cells[start..start + usize::from(rect.width)].to_vec(),
            });
        }
    }
    patch.rows = rows;
    Ok(patch)
}

/// The spans of `row` not covered by any scrolled region.
fn outside_scrolls(
    row: PaneSurfacePatchRow,
    scrolls: &[SurfaceScroll],
) -> Vec<PaneSurfacePatchRow> {
    let start = usize::from(row.x);
    let mut covered = scrolls
        .iter()
        .map(|scroll| scroll.rect)
        .filter(|rect| row.y >= rect.y && row.y - rect.y < rect.height)
        .map(|rect| {
            (
                usize::from(rect.x),
                usize::from(rect.x) + usize::from(rect.width),
            )
        })
        .collect::<Vec<_>>();
    if covered.is_empty() {
        return vec![row];
    }
    covered.sort_unstable();
    let end = start + row.cells.len();
    let mut spans = Vec::new();
    let mut cursor = start;
    for (left, right) in covered.into_iter().chain([(end, end)]) {
        let span_end = left.clamp(cursor, end);
        if span_end > cursor {
            spans.push(PaneSurfacePatchRow {
                x: cursor as u16,
                y: row.y,
                cells: row.cells[cursor - start..span_end - start].to_vec(),
            });
        }
        cursor = cursor.max(right.min(end));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FrameData, PaneSurfacePane};
    use ratatui::{buffer::Buffer, layout::Rect};

    const WIDTH: u16 = 30;
    const HEIGHT: u16 = 12;
    const PANE: SurfaceRect = SurfaceRect {
        x: 2,
        y: 1,
        width: 20,
        height: 10,
    };

    fn pane() -> PaneSurfacePane {
        PaneSurfacePane {
            pane_id: "w1:p1".into(),
            content_revision: 1,
            rect: SurfaceRect {
                x: 1,
                y: 0,
                width: 22,
                height: 12,
            },
            inner_rect: PANE,
            scrollbar_rect: None,
            scroll: None,
            focused: true,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    fn surface() -> PaneSurfaceFrame {
        let mut frame =
            FrameData::from_ratatui_buffer(&Buffer::empty(Rect::new(0, 0, WIDTH, HEIGHT)), None);
        for y in 0..PANE.height {
            write(&mut frame, y, &line(i32::from(y)));
        }
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame,
            panes: vec![pane()],
            splits: Vec::new(),
            popup: None,
            graphics: Default::default(),
        }
    }

    /// Distinct output lines, like a log or build scrolling past.
    fn line(n: i32) -> String {
        let word = ["alpha", "bravo", "charlie", "delta", "echo"][n.rem_euclid(5) as usize];
        format!("{n:>3} {word} {}", word.to_uppercase())
    }

    fn write(frame: &mut FrameData, pane_row: u16, text: &str) {
        let start = usize::from(PANE.y + pane_row) * usize::from(WIDTH) + usize::from(PANE.x);
        for (x, cell) in frame.cells[start..start + usize::from(PANE.width)]
            .iter_mut()
            .enumerate()
        {
            cell.symbol = text.chars().nth(x).map_or(" ".into(), String::from);
        }
    }

    /// The changed-cell spans the retained renderer would publish for `next`.
    fn row_patch(last: &PaneSurfaceFrame, next: &FrameData) -> PaneSurfacePatch {
        let mut rows = Vec::new();
        for y in 0..HEIGHT {
            let start = usize::from(y) * usize::from(WIDTH);
            let (old, new) = (
                &last.frame.cells[start..start + usize::from(WIDTH)],
                &next.cells[start..start + usize::from(WIDTH)],
            );
            let mut x = 0;
            while x < old.len() {
                if old[x] == new[x] {
                    x += 1;
                    continue;
                }
                let first = x;
                while x < old.len() && old[x] != new[x] {
                    x += 1;
                }
                rows.push(PaneSurfacePatchRow {
                    x: first as u16,
                    y,
                    cells: new[first..x].to_vec(),
                });
            }
        }
        PaneSurfacePatch {
            boot_id: last.boot_id.clone(),
            projection_revision: last.projection_revision,
            base_surface_revision: last.surface_revision,
            surface_revision: last.surface_revision + 1,
            rows,
            panes: vec![pane()],
            cursor: None,
        }
    }

    fn round_trip(last: &PaneSurfaceFrame, patch: &PaneSurfacePatch) -> (usize, FrameData) {
        let message = message(last, patch).expect("scroll message");
        let size = encoded_size(&message).unwrap();
        let ServerMessage::EndpointControl { kind, data } = message else {
            panic!("scroll must be an endpoint control frame");
        };
        assert_eq!(kind, MESSAGE_KIND);
        let mut cells = last.frame.cells.clone();
        let expanded = apply(&mut cells, WIDTH, HEIGHT, decode(&data).unwrap()).unwrap();
        // A receiver without scroll support must reach the same grid.
        let mut plain = last.frame.clone();
        for row in &expanded.rows {
            let start = usize::from(row.y) * usize::from(WIDTH) + usize::from(row.x);
            plain.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
        }
        assert_eq!(plain.cells, cells);
        assert_disjoint(&expanded.rows);
        (size, plain)
    }

    /// Receivers only take the fast presentation path for non-overlapping rows.
    fn assert_disjoint(rows: &[PaneSurfacePatchRow]) {
        for (index, a) in rows.iter().enumerate() {
            for b in &rows[index + 1..] {
                let overlap = a.y == b.y
                    && usize::from(a.x) < usize::from(b.x) + b.cells.len()
                    && usize::from(b.x) < usize::from(a.x) + a.cells.len();
                assert!(!overlap, "expanded rows overlap: {a:?} / {b:?}");
            }
        }
    }

    #[test]
    fn rows_crossing_a_scrolled_region_keep_only_their_outside_cells() {
        let last = surface();
        let mut next = last.frame.clone();
        for y in 0..PANE.height {
            write(&mut next, y, &line(i32::from(y) + 1));
        }
        let mut patch = row_patch(&last, &next);
        // One full-width row that runs across the pane, as a border repaint would.
        let y = PANE.y + 4;
        let start = usize::from(y) * usize::from(WIDTH);
        patch.rows.push(PaneSurfacePatchRow {
            x: 0,
            y,
            cells: next.cells[start..start + usize::from(WIDTH)].to_vec(),
        });
        let (_, result) = round_trip(&last, &patch);
        assert_eq!(result.cells, next.cells);
    }

    #[test]
    fn scrolling_output_sends_the_shift_and_only_new_rows() {
        let last = surface();
        let mut next = last.frame.clone();
        for y in 0..PANE.height {
            write(&mut next, y, &line(i32::from(y) + 1));
        }
        let patch = row_patch(&last, &next);

        let (size, result) = round_trip(&last, &patch);
        assert_eq!(result.cells, next.cells);
        let plain = encoded_size(&patch).unwrap();
        assert!(
            size * 3 < plain,
            "scroll must be much smaller: {size} vs {plain}"
        );
    }

    #[test]
    fn reverse_scroll_and_edits_reach_the_same_grid() {
        let last = surface();
        let mut next = last.frame.clone();
        for y in 0..PANE.height {
            write(&mut next, y, &line(i32::from(y) - 2));
        }
        write(&mut next, 5, "edited in place");
        let (_, result) = round_trip(&last, &row_patch(&last, &next));
        assert_eq!(result.cells, next.cells);
    }

    #[test]
    fn a_single_row_change_stays_a_plain_patch() {
        let last = surface();
        let mut next = last.frame.clone();
        write(&mut next, 9, "typed");
        assert!(message(&last, &row_patch(&last, &next)).is_none());
    }

    #[test]
    fn unrelated_rewrites_stay_a_plain_patch() {
        let last = surface();
        let mut next = last.frame.clone();
        for y in 0..PANE.height {
            write(&mut next, y, &format!("other {y}"));
        }
        assert!(message(&last, &row_patch(&last, &next)).is_none());
    }

    #[test]
    fn decode_rejects_regions_outside_the_baseline() {
        let last = surface();
        let mut next = last.frame.clone();
        for y in 0..PANE.height {
            write(&mut next, y, &line(i32::from(y) + 1));
        }
        let Some(ServerMessage::EndpointControl { data, .. }) =
            message(&last, &row_patch(&last, &next))
        else {
            panic!("scroll message");
        };
        let mut cells = last.frame.cells.clone();
        let mut decoded = decode(&data).unwrap();
        decoded.scrolls[0].rect.height = HEIGHT;
        assert!(apply(&mut cells, WIDTH, HEIGHT, decoded).is_err());
        assert_eq!(
            cells, last.frame.cells,
            "a rejected scroll must not touch the grid"
        );

        let mut decoded = decode(&data).unwrap();
        decoded.scrolls.push(decoded.scrolls[0]);
        assert!(
            apply(&mut cells, WIDTH, HEIGHT, decoded).is_err(),
            "overlapping regions"
        );
        assert_eq!(cells, last.frame.cells);

        let mut decoded = decode(&data).unwrap();
        decoded.scrolls[0].shift = PANE.height as i16;
        assert!(apply(&mut cells, WIDTH, HEIGHT, decoded).is_err());
        let mut trailing = STANDARD_NO_PAD.decode(&data).unwrap();
        trailing.push(0);
        assert!(decode(&STANDARD_NO_PAD.encode(trailing)).is_err());
        assert!(decode("").is_err());
        assert!(decode(&STANDARD_NO_PAD.encode([0u8])).is_err());
        assert!(decode(&STANDARD_NO_PAD.encode([1u8, 0, 0])).is_err());
    }
}
