use super::{ghostty_line_from_cells, GhosttyPaneCore, TerminalReadSnapshot};

const CACHE_LINES: usize = 2000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Cache {
    rows: Vec<RenderedLine>,
    last_snapshot: Vec<RenderedLine>,
    usable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderedLine {
    text: String,
    soft_wrapped: bool,
    wrap_continuation: bool,
}

pub(super) fn update(core: &mut GhosttyPaneCore) {
    if !primary_screen_active(core) {
        return;
    }
    if !viewport_is_at_bottom(core) {
        core.recent_fallback.usable = false;
        return;
    }
    let Ok(snapshot) = visible_render_lines(core) else {
        core.recent_fallback.usable = false;
        return;
    };
    if snapshot.is_empty() {
        core.recent_fallback.rows.clear();
        core.recent_fallback.last_snapshot.clear();
        core.recent_fallback.usable = false;
        return;
    }
    if snapshot == core.recent_fallback.last_snapshot {
        core.recent_fallback.usable = true;
        return;
    }

    merge_snapshot(&mut core.recent_fallback.rows, &snapshot);
    core.recent_fallback.last_snapshot = snapshot;
    core.recent_fallback.usable = true;
}

pub(super) fn recent_text(
    core: &GhosttyPaneCore,
    lines: usize,
    unwrap: bool,
) -> TerminalReadSnapshot {
    if !primary_screen_active(core) {
        return TerminalReadSnapshot::default();
    }
    if unwrap {
        unwrapped_text(core, lines)
    } else {
        wrapped_text(core, lines)
    }
}

fn primary_screen_active(core: &GhosttyPaneCore) -> bool {
    matches!(
        core.terminal.active_screen(),
        Ok(crate::ghostty::ActiveScreen::Primary)
    )
}

fn wrapped_text(core: &GhosttyPaneCore, lines: usize) -> TerminalReadSnapshot {
    if !core.recent_fallback.usable {
        return TerminalReadSnapshot::default();
    }
    let rows: Vec<&str> = core
        .recent_fallback
        .rows
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    TerminalReadSnapshot {
        text: cache_text(&rows, lines),
        truncated: rows.len() > lines,
    }
}

fn unwrapped_text(core: &GhosttyPaneCore, lines: usize) -> TerminalReadSnapshot {
    if !core.recent_fallback.usable {
        return TerminalReadSnapshot::default();
    }
    let rendered_rows = &core.recent_fallback.rows;
    let start = rendered_rows.len().saturating_sub(lines);
    let unwrapped = unwrap_render_lines(&rendered_rows[start..]);
    let rows: Vec<&str> = unwrapped.iter().map(String::as_str).collect();
    TerminalReadSnapshot {
        text: cache_text(&rows, rows.len()),
        truncated: rendered_rows.len() > lines,
    }
}

fn viewport_is_at_bottom(core: &GhosttyPaneCore) -> bool {
    let Ok(scrollbar) = core.terminal.scrollbar() else {
        return true;
    };
    scrollbar.offset.saturating_add(scrollbar.len) >= scrollbar.total
}

fn visible_render_lines(
    core: &mut GhosttyPaneCore,
) -> Result<Vec<RenderedLine>, crate::ghostty::Error> {
    let GhosttyPaneCore {
        terminal,
        render_state,
        ..
    } = core;
    render_state.update(terminal)?;
    let mut row_iterator = crate::ghostty::RowIterator::new()?;
    let mut row_cells = crate::ghostty::RowCells::new()?;
    let mut rows = render_state.populate_row_iterator(&mut row_iterator)?;
    let mut lines = Vec::new();
    while rows.next() {
        let (soft_wrapped, wrap_continuation) = rows.wrap_state().unwrap_or((false, false));
        let mut cells = rows.populate_cells(&mut row_cells)?;
        let text = ghostty_line_from_cells(&mut cells)?;
        if !text.is_empty() {
            lines.push(RenderedLine {
                text,
                soft_wrapped,
                wrap_continuation,
            });
        }
    }
    Ok(lines)
}

fn unwrap_render_lines(snapshot: &[RenderedLine]) -> Vec<String> {
    let mut unwrapped = Vec::new();
    let mut current = String::new();
    for line in snapshot {
        if line.wrap_continuation && current.is_empty() {
            continue;
        }
        current.push_str(&line.text);
        if !line.soft_wrapped {
            unwrapped.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        unwrapped.push(current);
    }
    unwrapped
}

fn merge_snapshot(cache: &mut Vec<RenderedLine>, snapshot: &[RenderedLine]) {
    if snapshot.is_empty() {
        return;
    }

    let max_overlap = cache.len().min(snapshot.len());
    let overlap = (0..=max_overlap)
        .rev()
        .find(|count| *count == 0 || cache[cache.len() - count..] == snapshot[..*count])
        .unwrap_or(0);
    cache.extend(snapshot[overlap..].iter().cloned());
    let overflow = cache.len().saturating_sub(CACHE_LINES);
    if overflow > 0 {
        cache.drain(0..overflow);
    }
}

fn cache_text(cache: &[&str], lines: usize) -> String {
    if lines == 0 || cache.is_empty() {
        return String::new();
    }
    let start = cache.len().saturating_sub(lines);
    let text = cache[start..].join("\n");
    if text.is_empty() {
        text
    } else {
        format!("{text}\n")
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::layout::PaneId;
    use crate::pane::terminal::GhosttyPaneTerminal;

    fn rendered_line(
        text: impl Into<String>,
        soft_wrapped: bool,
        wrap_continuation: bool,
    ) -> RenderedLine {
        RenderedLine {
            text: text.into(),
            soft_wrapped,
            wrap_continuation,
        }
    }

    #[test]
    fn merges_overlapping_snapshots() {
        let mut cache = Vec::new();

        merge_snapshot(
            &mut cache,
            &[
                rendered_line("one", false, false),
                rendered_line("two", false, false),
            ],
        );
        merge_snapshot(
            &mut cache,
            &[
                rendered_line("two", false, false),
                rendered_line("three", false, false),
                rendered_line("four", false, false),
            ],
        );
        merge_snapshot(
            &mut cache,
            &[
                rendered_line("three", false, false),
                rendered_line("four", false, false),
                rendered_line("five", false, false),
            ],
        );

        let text: Vec<&str> = cache.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(text, vec!["one", "two", "three", "four", "five"]);
    }

    #[test]
    fn unwraps_soft_wrapped_rows() {
        let snapshot = vec![
            rendered_line("ABCDE", true, false),
            rendered_line("FGHIJ", false, true),
            rendered_line("next", false, false),
        ];

        assert_eq!(unwrap_render_lines(&snapshot), vec!["ABCDEFGHIJ", "next"]);
    }

    #[test]
    fn suppresses_leading_wrap_continuation() {
        let snapshot = vec![
            rendered_line("suffix", false, true),
            rendered_line("next", false, false),
        ];

        assert_eq!(unwrap_render_lines(&snapshot), vec!["next"]);
    }

    #[test]
    fn clears_on_blank_snapshot() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(40, 3, 1024).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        pane.process_pty_bytes(pane_id, 0, b"old\r\n", &tx);
        assert!(pane.recent_text(3).contains("old"));

        pane.process_pty_bytes(pane_id, 0, b"\x1b[2J\x1b[H", &tx);
        assert_eq!(pane.recent_text(3).trim(), "");
    }

    #[test]
    fn ignores_alternate_screen_snapshots() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(40, 3, 1024).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        for line in 0..20 {
            pane.process_pty_bytes(pane_id, 0, format!("{line:06}\r\n").as_bytes(), &tx);
        }
        {
            let core = pane.core.lock().unwrap();
            assert!(core
                .recent_fallback
                .rows
                .iter()
                .any(|line| line.text.contains("000000")));
        }

        pane.process_pty_bytes(pane_id, 0, b"\x1b[?1049h\x1b[2J\x1b[H", &tx);

        let core = pane.core.lock().unwrap();
        assert!(core
            .recent_fallback
            .rows
            .iter()
            .any(|line| line.text.contains("000000")));
    }

    #[test]
    fn invalidates_fallback_when_output_arrives_while_scrolled_up() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(40, 3, 1024).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        for line in 0..20 {
            pane.process_pty_bytes(pane_id, 0, format!("{line:06}\r\n").as_bytes(), &tx);
        }
        let before = pane.scroll_metrics().expect("scroll metrics before scroll");
        pane.set_scroll_offset_from_bottom(before.max_offset_from_bottom);
        pane.process_pty_bytes(pane_id, 0, b"new output\r\n", &tx);

        let core = pane.core.lock().unwrap();
        assert_eq!(recent_text(&core, 3, false).text, "");
        assert_eq!(recent_text(&core, 3, true).text, "");
    }

    #[test]
    fn seed_history_updates_fallback() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(40, 3, 1024).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        pane.seed_history_ansi("seeded history\r\n");

        let core = pane.core.lock().unwrap();
        assert!(recent_text(&core, 3, false).text.contains("seeded history"));
        assert!(recent_text(&core, 3, true).text.contains("seeded history"));
    }

    #[test]
    fn recent_ansi_unwrapped_limits_rendered_rows_before_unwrapping() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(40, 3, 1024).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        {
            let mut core = pane.core.lock().unwrap();
            core.recent_fallback.rows = vec![
                rendered_line("older", false, false),
                rendered_line("wrapped", true, false),
                rendered_line("rows", false, true),
                rendered_line("last", false, false),
            ];
            core.recent_fallback.usable = true;
        }

        let snapshot = pane.recent_unwrapped_ansi_snapshot(3);
        assert_eq!(snapshot.text, "wrappedrows\nlast\n");
        assert!(snapshot.truncated);
        assert_eq!(pane.recent_ansi(3), "wrapped\nrows\nlast\n");
    }
}
