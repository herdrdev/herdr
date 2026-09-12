use std::collections::HashMap;

use crate::ghostty::{CellWide, ScreenTextRow};
use crate::pane::TerminalReadSnapshot;

const MIN_ALIGNMENT_RATIO_PERCENT: usize = 30;
const SIMILAR_VIEWPORT_RATIO_PERCENT: usize = 70;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScreenSnapshot {
    pub(crate) cols: u16,
    pub(crate) rows: Vec<ScreenTextRow>,
}

impl ScreenSnapshot {
    pub(crate) fn similar_text(&self, other: &Self) -> bool {
        if self.cols != other.cols || self.rows.len() != other.rows.len() {
            return false;
        }
        let left = row_identities(&self.rows);
        let right = row_identities(&other.rows);
        let comparable = left
            .iter()
            .zip(&right)
            .filter(|(left, right)| !left.is_empty() || !right.is_empty())
            .count();
        if comparable == 0 {
            return true;
        }
        let matches = left
            .iter()
            .zip(&right)
            .filter(|(left, right)| left == right && (!left.is_empty() || !right.is_empty()))
            .count();
        matches.saturating_mul(100) >= comparable.saturating_mul(SIMILAR_VIEWPORT_RATIO_PERCENT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpwardMerge {
    Advanced { rows: usize },
    Unchanged,
    Unaligned,
}

pub(crate) fn merge_scrolled_up(
    history: &mut Vec<ScreenTextRow>,
    previous: &ScreenSnapshot,
    next: &ScreenSnapshot,
) -> UpwardMerge {
    if previous.cols != next.cols || previous.rows.len() != next.rows.len() {
        return UpwardMerge::Unaligned;
    }
    let previous_text = row_identities(&previous.rows);
    let next_text = row_identities(&next.rows);
    if previous_text == next_text {
        return UpwardMerge::Unchanged;
    }
    let Some((shift, anchor)) = upward_alignment(&previous_text, &next_text) else {
        return UpwardMerge::Unaligned;
    };
    let history_text = row_identities(history);
    // A retained fragment may lack a complete scrollbar track/thumb pattern;
    // keep exact text matching alongside normalized identities for that case.
    let anchor_text = row_text(&previous.rows[anchor]);
    let Some(history_anchor) = history_text
        .iter()
        .take(previous.rows.len())
        .position(|text| text == &previous_text[anchor] || text.as_str() == anchor_text.trim_end())
    else {
        return UpwardMerge::Unaligned;
    };
    let boundary = anchor + shift;
    // Refresh the overlap as well as new rows: a pinned header may have obscured
    // transcript text at the top of the previous viewport.
    history.splice(0..history_anchor, next.rows[..boundary].iter().cloned());
    UpwardMerge::Advanced {
        rows: boundary.saturating_sub(history_anchor),
    }
}

pub(crate) fn snapshot_text(
    rows: &[ScreenTextRow],
    lines: usize,
    unwrap: bool,
    truncated: bool,
) -> TerminalReadSnapshot {
    let start = rows.len().saturating_sub(lines);
    let rows = &rows[start..];
    let text = if unwrap {
        unwrapped_text(rows)
    } else {
        wrapped_text(rows)
    };
    TerminalReadSnapshot { text, truncated }
}

fn upward_alignment(previous: &[String], next: &[String]) -> Option<(usize, usize)> {
    let mut previous_counts = HashMap::new();
    let mut next_counts = HashMap::new();
    for text in previous {
        *previous_counts.entry(text.as_str()).or_insert(0usize) += 1;
    }
    for text in next {
        *next_counts.entry(text.as_str()).or_insert(0usize) += 1;
    }
    let mut alignment = None;
    for shift in 1..previous.len() {
        let overlap = previous.len() - shift;
        let mut comparable = 0usize;
        let mut matches = 0usize;
        let mut first_anchor = None;
        for index in 0..overlap {
            let before = &previous[index];
            let after = &next[index + shift];
            if before.is_empty() || after.is_empty() {
                continue;
            }
            comparable += 1;
            if before == after {
                matches += 1;
                if previous_counts.get(before.as_str()) == Some(&1)
                    && next_counts.get(after.as_str()) == Some(&1)
                {
                    first_anchor.get_or_insert(index);
                }
            }
        }
        // Repeated rows alone cannot distinguish a small scroll from a larger one.
        let Some(anchor) = first_anchor else {
            continue;
        };
        if matches.saturating_mul(100) < comparable.saturating_mul(MIN_ALIGNMENT_RATIO_PERCENT) {
            continue;
        }
        // A row unique in each viewport can still occur elsewhere in the transcript.
        // Competing plausible shifts are ambiguous, not votes for the best score.
        if alignment.is_some() {
            return None;
        }
        alignment = Some((shift, anchor));
    }
    alignment
}

fn row_identities(rows: &[ScreenTextRow]) -> Vec<String> {
    let mut identities: Vec<_> = rows
        .iter()
        .map(|row| row_text(row).trim_end().to_string())
        .collect();
    let mut start = 0;
    while start < rows.len() {
        let mut end = start;
        let mut track = false;
        let mut thumb = false;
        while end < rows.len() && rows[end].cells.len() == rows[start].cells.len() {
            match rows[end].cells.last().map(|cell| cell.graphemes.as_slice()) {
                Some([0x2502]) => track = true,
                Some([0x2503 | 0x2588]) => thumb = true,
                _ => break,
            }
            end += 1;
        }
        // Require a vertical track and thumb, not an isolated border character.
        // Normalize comparison keys only; retained terminal cells stay untouched.
        if end - start >= 3 && track && thumb {
            for identity in &mut identities[start..end] {
                identity.pop();
                identity.truncate(identity.trim_end().len());
            }
        }
        start = end.max(start + 1);
    }
    identities
}

fn wrapped_text(rows: &[ScreenTextRow]) -> String {
    let mut lines: Vec<_> = rows
        .iter()
        .map(|row| row_text(row).trim_end().to_string())
        .collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines_to_text(lines)
}

fn unwrapped_text(rows: &[ScreenTextRow]) -> String {
    let mut lines = Vec::new();
    let mut current = String::new();
    for row in rows {
        let text = row_text(row);
        if row.soft_wrapped {
            current.push_str(text.trim_end());
        } else {
            current.push_str(text.trim_end());
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines_to_text(lines)
}

fn lines_to_text(lines: Vec<String>) -> String {
    let text = lines.join("\n");
    if text.is_empty() {
        text
    } else {
        format!("{text}\n")
    }
}

fn row_text(row: &ScreenTextRow) -> String {
    let mut text = String::new();
    for cell in &row.cells {
        if cell.wide == CellWide::SpacerTail {
            continue;
        }
        if cell.graphemes.is_empty()
            || cell.graphemes.first().copied() == Some(crate::ghostty::KITTY_UNICODE_PLACEHOLDER)
        {
            text.push(' ');
        } else {
            text.extend(cell.graphemes.iter().map(|codepoint| {
                char::from_u32(*codepoint).unwrap_or(char::REPLACEMENT_CHARACTER)
            }));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghostty::{ScreenTextCell, ScreenTextRow};

    fn row(text: &str) -> ScreenTextRow {
        ScreenTextRow {
            cells: text
                .chars()
                .map(|ch| ScreenTextCell {
                    wide: CellWide::Narrow,
                    graphemes: vec![ch as u32],
                })
                .collect(),
            soft_wrapped: false,
            wrap_continuation: false,
        }
    }

    fn snapshot(lines: &[&str]) -> ScreenSnapshot {
        ScreenSnapshot {
            cols: 20,
            rows: lines.iter().map(|line| row(line)).collect(),
        }
    }

    #[test]
    fn viewport_similarity_tolerates_small_dynamic_regions() {
        let initial = snapshot(&["line 1", "line 2", "worked for 2s", "prompt"]);
        let status_changed = snapshot(&["line 1", "line 2", "worked for 3s", "prompt"]);
        let scrolled = snapshot(&["older", "line 1", "line 2", "prompt"]);

        assert!(initial.similar_text(&status_changed));
        assert!(!initial.similar_text(&scrolled));
    }

    #[test]
    fn controlled_upward_scroll_prepends_only_new_rows() {
        let previous = snapshot(&["line 3", "line 4", "line 5", "status"]);
        let next = snapshot(&["line 1", "line 2", "line 3", "line 4"]);
        let mut history = previous.rows.clone();

        assert_eq!(
            merge_scrolled_up(&mut history, &previous, &next),
            UpwardMerge::Advanced { rows: 2 }
        );
        assert_eq!(
            row_identities(&history),
            ["line 1", "line 2", "line 3", "line 4", "line 5", "status"]
        );
    }

    #[test]
    fn fixed_header_is_not_repeated_or_counted_as_scrolled_history() {
        let previous = snapshot(&["sticky", "line 4", "line 5", "line 6", "line 7"]);
        let next = snapshot(&["sticky", "line 2", "line 3", "line 4", "line 5"]);
        let mut history = previous.rows.clone();

        assert_eq!(
            merge_scrolled_up(&mut history, &previous, &next),
            UpwardMerge::Advanced { rows: 2 }
        );
        assert_eq!(
            row_identities(&history),
            ["sticky", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7"]
        );
    }

    #[test]
    fn later_overlap_recovers_text_obscured_by_a_pinned_header() {
        let initial = snapshot(&["line 4", "line 5", "line 6", "line 7", "line 8"]);
        let scrolled = snapshot(&["pinned", "line 3", "line 4", "line 5", "line 6"]);
        let older = snapshot(&["pinned", "line 1", "line 2", "line 3", "line 4"]);
        let top = snapshot(&["title", "line 0", "line 1", "line 2", "line 3"]);
        let mut history = initial.rows.clone();
        for (previous, next) in [(&initial, &scrolled), (&scrolled, &older), (&older, &top)] {
            assert!(matches!(
                merge_scrolled_up(&mut history, previous, next),
                UpwardMerge::Advanced { .. }
            ));
        }
        assert_eq!(
            row_identities(&history),
            [
                "title", "line 0", "line 1", "line 2", "line 3", "line 4", "line 5", "line 6",
                "line 7", "line 8"
            ]
        );
    }

    #[test]
    fn scrolling_preserves_repeated_continuation_rows() {
        let rows: Vec<_> = (1..=18)
            .flat_map(|number| {
                [
                    row(&format!("line {number:03} begins")),
                    row("same continuation"),
                    row("same ending"),
                ]
            })
            .collect();
        for shift in [3, 15] {
            for offset in 0..3 {
                let previous = ScreenSnapshot {
                    cols: 20,
                    rows: rows[offset + shift..offset + shift + 36].to_vec(),
                };
                let next = ScreenSnapshot {
                    cols: 20,
                    rows: rows[offset..offset + 36].to_vec(),
                };
                let mut history = previous.rows.clone();

                assert_eq!(
                    merge_scrolled_up(&mut history, &previous, &next),
                    UpwardMerge::Advanced { rows: shift },
                    "shift={shift}, offset={offset}"
                );
                assert_eq!(&history[..shift], &next.rows[..shift]);
                assert_eq!(&history[shift..], &previous.rows);
            }
        }
    }

    fn transcript(first: usize, scrollbar: bool) -> ScreenSnapshot {
        let mut rows: Vec<_> = (first..first + 10)
            .enumerate()
            .map(|(index, number)| {
                let edge = if !scrollbar {
                    ' '
                } else if (3..6).contains(&index) {
                    '┃'
                } else {
                    '│'
                };
                row(&format!("{:<19}{edge}", format!("line {number}")))
            })
            .collect();
        rows.extend([row("prompt"), row("status")]);
        ScreenSnapshot { cols: 20, rows }
    }

    #[test]
    fn scrolling_with_an_appearing_scrollbar_recovers_history_and_preserves_cells() {
        let initial = transcript(10, false);
        let scrolled = transcript(7, true);
        let mut history = initial.rows.clone();

        assert_eq!(
            merge_scrolled_up(&mut history, &initial, &scrolled),
            UpwardMerge::Advanced { rows: 3 }
        );
        assert_eq!(&history[..3], &scrolled.rows[..3]);
        assert_eq!(&history[3..], &initial.rows);
        assert!(initial.similar_text(&transcript(10, true)));
        assert!(!initial.similar_text(&scrolled));

        let mut older = transcript(4, true);
        for line in &mut older.rows[..10] {
            let edge = line.cells.last_mut().unwrap();
            edge.graphemes = if edge.graphemes == ['│' as u32] {
                vec!['█' as u32]
            } else {
                vec!['│' as u32]
            };
        }
        assert_eq!(
            merge_scrolled_up(&mut history, &scrolled, &older),
            UpwardMerge::Advanced { rows: 3 }
        );
        assert_eq!(&history[..3], &older.rows[..3]);
    }

    #[test]
    fn chained_scroll_matches_a_retained_anchor_after_the_scrollbar_thumb_moves() {
        let initial = transcript(10, true);
        let mut scrolled = transcript(7, true);
        let mut older = transcript(4, true);
        for line in &mut scrolled.rows[..3] {
            *line = row(&format!("{:<19}│", "repeated"));
        }
        for line in &mut older.rows[3..6] {
            *line = row(&format!("{:<19}┃", "repeated"));
        }
        let mut history = initial.rows.clone();
        assert_eq!(
            merge_scrolled_up(&mut history, &initial, &scrolled),
            UpwardMerge::Advanced { rows: 3 }
        );
        assert_eq!(
            merge_scrolled_up(&mut history, &scrolled, &older),
            UpwardMerge::Advanced { rows: 3 }
        );
        assert_eq!(&history[..6], &older.rows[..6]);
        assert_eq!(&history[6..], &initial.rows);
    }

    #[test]
    fn scrollbar_normalization_does_not_hide_real_edge_text_or_align_unrelated_output() {
        let initial = transcript(10, false);
        let mut changed = transcript(10, true);
        for line in &mut changed.rows[..10] {
            line.cells[18].graphemes = vec!['x' as u32];
        }
        assert!(!initial.similar_text(&changed));
        let mut history = initial.rows.clone();
        assert_eq!(
            merge_scrolled_up(&mut history, &initial, &changed),
            UpwardMerge::Unaligned
        );
        assert_eq!(history, initial.rows);

        let mut real_edge = initial.clone();
        for line in &mut real_edge.rows[..10] {
            line.cells[19].graphemes = vec!['x' as u32];
        }
        assert!(!real_edge.similar_text(&transcript(10, true)));

        let boxed = snapshot(&["one   │", "two   │", "three │", "four  │"]);
        assert_eq!(
            row_identities(&boxed.rows),
            ["one   │", "two   │", "three │", "four  │"]
        );
    }

    #[test]
    fn unchanged_and_unaligned_frames_do_not_change_history() {
        let previous = snapshot(&["line 1", "line 2", "line 3"]);
        let mut history = previous.rows.clone();

        assert_eq!(
            merge_scrolled_up(&mut history, &previous, &previous),
            UpwardMerge::Unchanged
        );
        assert_eq!(
            merge_scrolled_up(
                &mut history,
                &previous,
                &snapshot(&["other a", "other b", "other c"]),
            ),
            UpwardMerge::Unaligned
        );
        assert_eq!(history, previous.rows);
    }

    #[test]
    fn history_anchor_matches_text_despite_different_wrap_metadata() {
        let previous = snapshot(&["line 3", "line 4", "line 5", "status"]);
        let next = snapshot(&["line 1", "line 2", "line 3", "line 4"]);
        let mut history = previous.rows.clone();
        history[0].wrap_continuation = true;
        assert_eq!(
            merge_scrolled_up(&mut history, &previous, &next),
            UpwardMerge::Advanced { rows: 2 }
        );
        assert_eq!(
            row_identities(&history),
            ["line 1", "line 2", "line 3", "line 4", "line 5", "status"]
        );
    }

    #[test]
    fn competing_unique_anchors_do_not_prove_scroll_distance() {
        let previous = snapshot(&["U1", "U2", "U3", "c", "d"]);
        let next = snapshot(&["a", "b", "U2", "U3", "U1"]);
        let mut history = previous.rows.clone();
        assert_eq!(
            merge_scrolled_up(&mut history, &previous, &next),
            UpwardMerge::Unaligned
        );
        assert_eq!(history, previous.rows);
    }

    #[test]
    fn repeated_rows_alone_do_not_prove_scroll_distance() {
        let previous = snapshot(&["a", "b", "a", "b", "a", "b"]);
        let next = snapshot(&["b", "a", "b", "a", "b", "a"]);
        let mut history = previous.rows.clone();
        assert_eq!(
            merge_scrolled_up(&mut history, &previous, &next),
            UpwardMerge::Unaligned
        );
        assert_eq!(history, previous.rows);
    }

    #[test]
    fn snapshot_text_limits_rendered_rows_before_unwrapping() {
        let mut first = row("hello ");
        first.soft_wrapped = true;
        let mut second = row("world");
        second.wrap_continuation = true;
        let rows = vec![row("older"), first, second];

        assert_eq!(
            snapshot_text(&rows, 2, false, true),
            TerminalReadSnapshot {
                text: "hello\nworld\n".into(),
                truncated: true,
            }
        );
        assert_eq!(
            snapshot_text(&rows, 2, true, true),
            TerminalReadSnapshot {
                text: "helloworld\n".into(),
                truncated: true,
            }
        );
    }
}
