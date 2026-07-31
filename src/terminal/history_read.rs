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
    let Some(shift) = best_upward_shift(&previous_text, &next_text) else {
        return UpwardMerge::Unaligned;
    };
    let Some(boundary) = (0..previous_text.len().saturating_sub(shift)).find_map(|index| {
        let next_index = index + shift;
        (!previous_text[index].is_empty() && previous_text[index] == next_text[next_index])
            .then_some(next_index)
    }) else {
        return UpwardMerge::Unaligned;
    };
    let added: Vec<_> = next.rows[..boundary]
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            next_text[*index].is_empty() || previous_text.get(*index) != Some(&next_text[*index])
        })
        .map(|(_, row)| row.clone())
        .collect();
    if added.is_empty() {
        return UpwardMerge::Unaligned;
    }
    let rows = added.len();
    history.splice(0..0, added);
    UpwardMerge::Advanced { rows }
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

fn best_upward_shift(previous: &[String], next: &[String]) -> Option<usize> {
    let mut best = None;
    for shift in 1..previous.len() {
        let overlap = previous.len() - shift;
        let mut comparable = 0usize;
        let mut matches = 0usize;
        for index in 0..overlap {
            let before = &previous[index];
            let after = &next[index + shift];
            if before.is_empty() || after.is_empty() {
                continue;
            }
            comparable += 1;
            if before == after {
                matches += 1;
            }
        }
        if comparable == 0
            || matches.saturating_mul(100) < comparable.saturating_mul(MIN_ALIGNMENT_RATIO_PERCENT)
        {
            continue;
        }
        if best.is_none_or(|(_, best_matches, best_comparable)| {
            matches > best_matches || (matches == best_matches && comparable > best_comparable)
        }) {
            best = Some((shift, matches, comparable));
        }
    }
    best.map(|(shift, _, _)| shift)
}

fn row_identities(rows: &[ScreenTextRow]) -> Vec<String> {
    rows.iter()
        .map(|row| row_text(row).trim_end().to_string())
        .collect()
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
            ["line 2", "line 3", "sticky", "line 4", "line 5", "line 6", "line 7"]
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
