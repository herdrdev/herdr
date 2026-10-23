use super::*;

/// The screen of a real omp 18.10.4 `ask` (synthetic question, captured from
/// an isolated omp against a scripted model): the transcript's question box,
/// then the live dialog with the recommended option highlighted.
const RECORDED_OMP_18_10_4: &str = "\
 please ask me


╭─── Ask 1 questions ──────────────────────────────────────────────────────────────────────────────╮
├─── [deploy] · options:3 ─────────────────────────────────────────────────────────────────────────┤
│  Which deploy path should I take?                                                                │
│  ○ Ship it now                                                                                   │
│  ○ Wait for review                                                                               │
│  ○ Abort the change                                                                              │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯

  ⎋ Asking the deploy question
╭─ Ask ────────────────────────────────────────────────────────────────────────────────────────────╮
│ Which deploy path should I take?                                                                 │
├──────────────────────────────────────────────────────────────────────────────────────────────────┤
│   ○ Ship it now                                                                                  │
│ ❯ ○ Wait for review (Recommended)                                                                │
│   ○ Abort the change                                                                             │
│   ○ Other (type your own)                                                                        │
│                                                                                                  │
│                                                                                                  │
├──────────────────────────────────────────────────────────────────────────────────────────────────┤
│ ⏎ select · n note · ↑/↓ move · ⎋ cancel · Alt+H hide                                             │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
";

fn labels(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn deploy_rows() -> Vec<String> {
    display_rows(
        &labels(&["Ship it now", "Wait for review", "Abort the change"]),
        Some(1),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// omp 18.x boxed ask dialog: radio markers, long labels wrap.
    Dialog,
    /// Older inline select list: no box, no markers, labels cut at the width.
    Inline,
}

/// A pane running a selector: renders its state as a screen and applies the
/// keys it is sent the way the selector would.
struct FakeSelector {
    rows: Vec<String>,
    highlighted: usize,
    style: Style,
    width: usize,
    /// Older select lists wrap at the edges; omp 18.x's dialog clamps.
    wraps: bool,
    /// Arrow keys go elsewhere (e.g. a note editor owns input).
    ignores_arrows: bool,
    /// Rows moved per arrow, as when the list skips a disabled row.
    step: usize,
    filter: Option<String>,
    /// Options shown at most; the window follows the highlight.
    max_visible: usize,
    descriptions: bool,
    sent: Vec<String>,
    submitted: Option<usize>,
}

impl FakeSelector {
    fn new(rows: Vec<String>, highlighted: usize) -> Self {
        Self {
            rows,
            highlighted,
            style: Style::Dialog,
            width: 60,
            wraps: false,
            ignores_arrows: false,
            step: 1,
            filter: None,
            max_visible: usize::MAX,
            descriptions: false,
            sent: Vec::new(),
            submitted: None,
        }
    }

    fn move_by(&mut self, delta: isize) {
        let len = self.rows.len() as isize;
        let next = self.highlighted as isize + delta;
        self.highlighted = if self.wraps {
            next.rem_euclid(len) as usize
        } else {
            next.clamp(0, len - 1) as usize
        };
    }

    fn window(&self) -> std::ops::Range<usize> {
        let count = self.rows.len().min(self.max_visible);
        let start = self
            .highlighted
            .saturating_sub(count / 2)
            .min(self.rows.len() - count);
        start..start + count
    }

    fn render(&self) -> String {
        match self.style {
            Style::Dialog => self.render_dialog(),
            Style::Inline => self.render_inline(),
        }
    }

    fn boxed(content: &str, inner: usize) -> String {
        let pad = inner.saturating_sub(content.chars().count());
        format!("│ {content}{} │\n", " ".repeat(pad))
    }

    fn render_dialog(&self) -> String {
        let inner = self.width - 4;
        let mut out = String::from("some earlier output\n\n");
        out.push_str(&format!("╭─ Ask {}╮\n", "─".repeat(inner - 4)));
        out.push_str(&Self::boxed("Pick one", inner));
        out.push_str(&format!("├{}┤\n", "─".repeat(inner + 2)));
        for index in self.window() {
            let cursor = if index == self.highlighted {
                "❯ "
            } else {
                "  "
            };
            let label_width = inner - 4;
            let chars: Vec<char> = self.rows[index].chars().collect();
            let mut chunks = chars.chunks(label_width);
            let first: String = chunks.next().unwrap_or_default().iter().collect();
            out.push_str(&Self::boxed(&format!("{cursor}○ {first}"), inner));
            for chunk in chunks {
                let rest: String = chunk.iter().collect();
                out.push_str(&Self::boxed(&format!("    {rest}"), inner));
            }
            if self.descriptions && index + 1 < self.rows.len() {
                out.push_str(&Self::boxed(&format!("      about {}", index + 1), inner));
            }
        }
        out.push_str(&Self::boxed("", inner));
        out.push_str(&format!("├{}┤\n", "─".repeat(inner + 2)));
        out.push_str(&Self::boxed("⏎ select · ↑/↓ move · ⎋ cancel", inner));
        out.push_str(&format!("╰{}╯\n", "─".repeat(inner + 2)));
        out
    }

    fn render_inline(&self) -> String {
        let mut out = String::from(" Pick one\n\n");
        let window = self.window();
        for index in window.clone() {
            let cursor = if index == self.highlighted {
                "❯ "
            } else {
                "  "
            };
            // select-list.ts: `truncateToWidth(value, width - prefix - 2, Ellipsis.Omit)`.
            let fit: String = self.rows[index].chars().take(self.width - 4).collect();
            out.push_str(&format!(" {cursor}{fit}\n"));
        }
        if let Some(filter) = &self.filter {
            out.push_str(&format!(
                "   ({}/{})  Search: {filter}\n",
                self.highlighted + 1,
                self.rows.len()
            ));
        } else if window.len() < self.rows.len() {
            out.push_str("   Type to search\n");
        }
        out.push_str("\n up/down navigate  enter select  esc cancel\n");
        out
    }
}

impl SelectorPane for FakeSelector {
    fn read_screen(&mut self) -> Result<String, ErrorBody> {
        Ok(self.render())
    }

    fn send_keys(&mut self, keys: &[&str]) -> Result<(), ErrorBody> {
        for key in keys {
            self.sent.push((*key).to_owned());
            let step = self.step as isize;
            match *key {
                "up" if !self.ignores_arrows => self.move_by(-step),
                "down" if !self.ignores_arrows => self.move_by(step),
                "enter" => self.submitted = Some(self.highlighted),
                _ => {}
            }
        }
        Ok(())
    }

    fn pause(&mut self) {}
}

fn keys(sent: &[String]) -> Vec<&str> {
    sent.iter().map(String::as_str).collect()
}

#[test]
fn display_rows_badge_recommended_and_reserve_other() {
    let rows = display_rows(
        &labels(&[
            "Other (type your own)",
            "Keep",
            "Keep (Recommended)",
            "Next →",
        ]),
        Some(1),
    );
    assert_eq!(
        rows,
        labels(&[
            "Other (type your own) (2)",
            "Keep (Recommended)",
            "Keep (Recommended) (2)",
            "Next → (2)",
            "Other (type your own)",
        ])
    );
    assert_eq!(
        display_rows(&labels(&["Go\r\nnow", "Stop\r\rhere"]), None),
        labels(&["Go\nnow", "Stop here", "Other (type your own)"])
    );
}

#[test]
fn recorded_omp_screen_highlights_the_recommended_option() {
    assert_eq!(
        locate(RECORDED_OMP_18_10_4, &deploy_rows()),
        Ok(SelectorView { highlighted: 1 })
    );
}

#[test]
fn recorded_omp_screen_needs_the_recommended_badge_where_omp_draws_it() {
    // Same options, no recommendation: row 1 then lacks the badge omp drew,
    // so the screen is not this question's selector.
    let rows = display_rows(
        &labels(&["Ship it now", "Wait for review", "Abort the change"]),
        None,
    );
    assert_eq!(
        locate(RECORDED_OMP_18_10_4, &rows),
        Err(ScreenError::NotFound)
    );
}

#[test]
fn other_options_do_not_match_the_recorded_screen() {
    let rows = display_rows(&labels(&["Ship it now", "Abort the change"]), None);
    assert_eq!(
        locate(RECORDED_OMP_18_10_4, &rows),
        Err(ScreenError::NotFound)
    );
}

#[test]
fn moves_up_and_down_from_the_recommended_row_then_submits() {
    for (target, expected) in [
        (0, vec!["up", "enter"]),
        (1, vec!["enter"]),
        (2, vec!["down", "enter"]),
    ] {
        let mut pane = FakeSelector::new(deploy_rows(), 1);
        select_and_submit(&mut pane, &deploy_rows(), target).unwrap();
        assert_eq!(keys(&pane.sent), expected, "target {target}");
        assert_eq!(pane.submitted, Some(target));
    }
}

#[test]
fn wrapping_list_is_driven_the_direct_way_in_both_directions() {
    let rows = display_rows(&labels(&["A one", "B two", "C three", "D four"]), None);
    // Highlight on Other (last row): reaching the first option must not rely
    // on wrapping past the bottom edge.
    let mut pane = FakeSelector::new(rows.clone(), 4);
    pane.wraps = true;
    select_and_submit(&mut pane, &rows, 0).unwrap();
    assert_eq!(keys(&pane.sent), ["up", "up", "up", "up", "enter"]);
    assert_eq!(pane.submitted, Some(0));

    // After a wrap moved the highlight to the top, a down move still lands.
    let mut pane = FakeSelector::new(rows.clone(), 4);
    pane.wraps = true;
    pane.send_keys(&["down"]).unwrap();
    assert_eq!(
        locate(&pane.render(), &rows),
        Ok(SelectorView { highlighted: 0 })
    );
    pane.sent.clear();
    select_and_submit(&mut pane, &rows, 3).unwrap();
    assert_eq!(keys(&pane.sent), ["down", "down", "down", "enter"]);
    assert_eq!(pane.submitted, Some(3));
}

#[test]
fn option_text_truncated_by_the_pane_width_still_matches() {
    let rows = display_rows(
        &labels(&[
            "Rebuild the whole index from scratch tonight",
            "Rebuild only the stale shards",
            "Skip the rebuild",
        ]),
        Some(1),
    );
    let mut pane = FakeSelector::new(rows.clone(), 1);
    pane.style = Style::Inline;
    pane.width = 24;
    let screen = pane.render();
    assert!(
        !screen.contains("Rebuild only the stale shards"),
        "the fixture must actually cut the labels:\n{screen}"
    );
    assert_eq!(locate(&screen, &rows), Ok(SelectorView { highlighted: 1 }));
    select_and_submit(&mut pane, &rows, 0).unwrap();
    assert_eq!(keys(&pane.sent), ["up", "enter"]);
    assert_eq!(pane.submitted, Some(0));
}

#[test]
fn ellipsis_cut_labels_match() {
    let screen = "\
│   ○ Deploy to the staging clus…   │
│ ❯ ○ Deploy to production (Recom…  │
│   ○ Other (type your own)         │
";
    let rows = display_rows(
        &labels(&["Deploy to the staging cluster", "Deploy to production"]),
        Some(1),
    );
    assert_eq!(locate(screen, &rows), Ok(SelectorView { highlighted: 1 }));
}

#[test]
fn wrapped_labels_and_descriptions_are_skipped_as_continuations() {
    let rows = display_rows(
        &labels(&[
            "Short",
            "A considerably longer option label that has to wrap onto a second row",
            "Last",
        ]),
        Some(0),
    );
    let mut pane = FakeSelector::new(rows.clone(), 0);
    pane.descriptions = true;
    pane.width = 40;
    select_and_submit(&mut pane, &rows, 2).unwrap();
    assert_eq!(keys(&pane.sent), ["down", "down", "enter"]);
    assert_eq!(pane.submitted, Some(2));
}

#[test]
fn scrolled_window_is_placed_by_its_rows() {
    let options: Vec<String> = (1..=12).map(|n| format!("Choice number {n}")).collect();
    let rows = display_rows(&options, Some(9));
    let mut pane = FakeSelector::new(rows.clone(), 9);
    pane.max_visible = 5;
    select_and_submit(&mut pane, &rows, 2).unwrap();
    assert_eq!(
        keys(&pane.sent),
        ["up", "up", "up", "up", "up", "up", "up", "enter"]
    );
    assert_eq!(pane.submitted, Some(2));
}

#[test]
fn prefix_labels_are_told_apart_by_position() {
    let rows = display_rows(&labels(&["Ship", "Ship it", "Ship it now"]), None);
    let mut pane = FakeSelector::new(rows.clone(), 0);
    select_and_submit(&mut pane, &rows, 1).unwrap();
    assert_eq!(pane.submitted, Some(1));
}

#[test]
fn active_filter_is_refused_before_any_key() {
    let rows = deploy_rows();
    let mut pane = FakeSelector::new(rows.clone(), 1);
    pane.style = Style::Inline;
    pane.filter = Some("wa".into());
    assert_eq!(locate(&pane.render(), &rows), Err(ScreenError::Filtered));
    let err = select_and_submit(&mut pane, &rows, 0).unwrap_err();
    assert_eq!(err.code, "ask_ui_not_found");
    assert!(pane.sent.is_empty(), "sent {:?}", pane.sent);
}

#[test]
fn unfiltered_search_hint_is_not_a_filter() {
    let options: Vec<String> = (1..=8).map(|n| format!("Pick {n}")).collect();
    let rows = display_rows(&options, None);
    let mut pane = FakeSelector::new(rows.clone(), 0);
    pane.style = Style::Inline;
    pane.max_visible = 4;
    assert!(pane.render().contains("Type to search"));
    select_and_submit(&mut pane, &rows, 6).unwrap();
    assert_eq!(pane.submitted, Some(6));
}

#[test]
fn arrows_that_do_not_land_never_send_enter() {
    let rows = deploy_rows();
    let mut pane = FakeSelector::new(rows.clone(), 0);
    pane.ignores_arrows = true;
    let err = select_and_submit(&mut pane, &rows, 2).unwrap_err();
    assert_eq!(err.code, "selection_mismatch");
    assert_eq!(keys(&pane.sent), ["down", "down"]);
    assert_eq!(pane.submitted, None);

    // A list that jumps two rows per press overshoots: still no Enter.
    let mut pane = FakeSelector::new(rows.clone(), 0);
    pane.step = 2;
    let err = select_and_submit(&mut pane, &rows, 1).unwrap_err();
    assert_eq!(err.code, "selection_mismatch");
    assert_eq!(keys(&pane.sent), ["down"]);
    assert_eq!(pane.submitted, None);
}

#[test]
fn missing_or_duplicated_selector_sends_nothing() {
    let rows = deploy_rows();
    let mut pane = FakeSelector::new(rows.clone(), 1);
    pane.rows = display_rows(&labels(&["Something", "Else"]), None);
    pane.highlighted = 0;
    let err = select_and_submit(&mut pane, &rows, 0).unwrap_err();
    assert_eq!(err.code, "ask_ui_not_found");
    assert!(pane.sent.is_empty());

    let twice = format!("{RECORDED_OMP_18_10_4}{RECORDED_OMP_18_10_4}");
    assert_eq!(locate(&twice, &rows), Err(ScreenError::Ambiguous));
}

#[test]
fn ascii_symbol_preset_is_recognised() {
    let screen = "\
| Which deploy path should I take?   |
|   ( ) Ship it now                  |
| > (o) Wait for review (Recommended)|
|   ( ) Abort the change             |
|   ( ) Other (type your own)        |
";
    assert_eq!(
        locate(screen, &deploy_rows()),
        Ok(SelectorView { highlighted: 1 })
    );
}
