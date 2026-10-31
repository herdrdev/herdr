//! Per-client back/forward history of focus jumps.
//!
//! A "jump" is a focus change that crosses a tab or workspace boundary. Moving
//! between panes inside one tab is not recorded, but the entry for the current
//! tab is updated to the pane that was focused when the jump happened, so
//! stepping back returns to the exact pane that was left.
//!
//! The history is observed from snapshot transitions rather than recorded at
//! each server call site, so every source of focus change (keys, mouse,
//! CLI/API, notifications) is captured. It lives only in client memory.

use crate::protocol::ClientShellSnapshot;

/// Maximum number of entries kept; the oldest entry is dropped beyond this.
pub(super) const NAVIGATION_HISTORY_LIMIT: usize = 50;

#[derive(Debug, Default)]
pub(super) struct NavigationHistory {
    /// Pane ids, oldest first. Adjacent entries are always distinct.
    entries: Vec<String>,
    /// Index of the entry for the current location. Meaningless when empty.
    cursor: usize,
    /// Back/forward steps whose focus change has not been observed yet.
    pending: Option<PendingStep>,
}

#[derive(Debug)]
struct PendingStep {
    /// Cursor before the first in-flight step, restored if the steps never land.
    origin: usize,
}

impl NavigationHistory {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Observes the focus change, if any, between two snapshots.
    pub(super) fn observe(&mut self, previous: &ClientShellSnapshot, next: &ClientShellSnapshot) {
        let (Some(from), Some(to)) = (
            previous.focused_pane_id.as_deref(),
            next.focused_pane_id.as_deref(),
        ) else {
            return;
        };
        if from == to {
            return;
        }
        let crossed = previous.focused_workspace_id != next.focused_workspace_id
            || previous.focused_tab_id != next.focused_tab_id;
        self.observe_focus(from, to, crossed);
    }

    /// Observes focus moving from one pane to another; `crossed` is whether it
    /// crossed a tab or workspace boundary. Every focus change is seen here, not
    /// only jumps, because a step between two entries in the same tab lands
    /// without crossing one. Changes caused by our own back/forward steps are
    /// recognized and do not alter the history.
    fn observe_focus(&mut self, from: &str, to: &str, crossed: bool) {
        if let Some(pending) = self.pending.take() {
            if self.entries.get(self.cursor).is_some_and(|e| e == to) {
                // The last in-flight step landed.
                return;
            }
            if self.on_step_path(pending.origin, to) {
                // An intermediate step from rapid repeated presses; wait for
                // the last one.
                self.pending = Some(pending);
                return;
            }
            // Focus went somewhere the steps were not heading, so they never
            // landed: focus is still where they started, so put the cursor
            // back there before recording.
            self.cursor = pending.origin.min(self.entries.len().saturating_sub(1));
        }
        if crossed {
            self.record_jump(from, to);
        }
    }

    /// Whether `pane` is an entry the in-flight steps from `origin` to the
    /// cursor pass through.
    fn on_step_path(&self, origin: usize, pane: &str) -> bool {
        let (low, high) = (origin.min(self.cursor), origin.max(self.cursor));
        self.entries
            .get(low..=high)
            .is_some_and(|path| path.iter().any(|e| e == pane))
    }

    fn record_jump(&mut self, from: &str, to: &str) {
        if self.entries.get(self.cursor).is_some_and(|e| e == to) {
            return;
        }

        if self.entries.is_empty() {
            self.entries.push(from.to_owned());
            self.cursor = 0;
        } else {
            // Browser-style: navigating somewhere new discards forward entries.
            self.entries.truncate(self.cursor + 1);
            // Remember the exact pane we left, which may differ from the one we
            // arrived at if focus moved within the tab since.
            self.entries[self.cursor] = from.to_owned();
        }
        self.entries.push(to.to_owned());
        self.cursor = self.entries.len() - 1;

        if self.entries.len() > NAVIGATION_HISTORY_LIMIT {
            self.entries.remove(0);
            self.cursor -= 1;
        }
    }

    /// Steps back, returning the pane to focus. `current` is the focused pane
    /// and `is_live` reports whether a pane still exists.
    pub(super) fn back(&mut self, current: &str, is_live: impl Fn(&str) -> bool) -> Option<String> {
        self.prepare_step(current, is_live);
        if self.entries.is_empty() || self.cursor == 0 {
            return None;
        }
        Some(self.begin_step(self.cursor - 1))
    }

    /// Steps forward, returning the pane to focus.
    pub(super) fn forward(
        &mut self,
        current: &str,
        is_live: impl Fn(&str) -> bool,
    ) -> Option<String> {
        self.prepare_step(current, is_live);
        if self.cursor + 1 >= self.entries.len() {
            return None;
        }
        Some(self.begin_step(self.cursor + 1))
    }

    fn prepare_step(&mut self, current: &str, is_live: impl Fn(&str) -> bool) {
        // With a step in flight the snapshot's focus is stale, so only sync the
        // current entry once the previous step has landed.
        if self.pending.is_none() {
            if let Some(entry) = self.entries.get_mut(self.cursor) {
                current.clone_into(entry);
            }
        }
        self.retain_live(is_live);
    }

    fn begin_step(&mut self, target: usize) -> String {
        let origin = self.pending.as_ref().map_or(self.cursor, |p| p.origin);
        self.pending = Some(PendingStep { origin });
        self.cursor = target;
        self.entries[target].clone()
    }

    /// Drops entries for panes that no longer exist and collapses the adjacent
    /// duplicates that leaves behind. The cursor and any pending origin move to
    /// the nearest surviving entry at or before their old position, or to the
    /// first entry when none survives before it.
    fn retain_live(&mut self, is_live: impl Fn(&str) -> bool) {
        let mut kept: Vec<String> = Vec::with_capacity(self.entries.len());
        // How many entries were kept up to and including each old index.
        let mut kept_through = Vec::with_capacity(self.entries.len());
        for entry in std::mem::take(&mut self.entries) {
            if is_live(&entry) && kept.last() != Some(&entry) {
                kept.push(entry);
            }
            kept_through.push(kept.len());
        }
        let rebase = |index: usize| {
            kept_through
                .get(index)
                .copied()
                .unwrap_or(kept.len())
                .saturating_sub(1)
        };
        self.cursor = rebase(self.cursor);
        if let Some(pending) = &mut self.pending {
            pending.origin = rebase(pending.origin);
        }
        self.entries = kept;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(_: &str) -> bool {
        true
    }

    fn history(jumps: &[(&str, &str)]) -> NavigationHistory {
        let mut history = NavigationHistory::default();
        for (from, to) in jumps {
            history.observe_focus(from, to, true);
        }
        history
    }

    /// Steps and simulates the server applying the focus change.
    fn step_back(history: &mut NavigationHistory, current: &mut String) -> Option<String> {
        let target = history.back(current, live)?;
        history.observe_focus(current, &target, true);
        *current = target.clone();
        Some(target)
    }

    fn step_forward(history: &mut NavigationHistory, current: &mut String) -> Option<String> {
        let target = history.forward(current, live)?;
        history.observe_focus(current, &target, true);
        *current = target.clone();
        Some(target)
    }

    #[test]
    fn empty_history_has_nowhere_to_go() {
        let mut history = NavigationHistory::default();
        assert_eq!(history.back("a", live), None);
        assert_eq!(history.forward("a", live), None);
    }

    #[test]
    fn back_and_forward_walk_the_recorded_jumps() {
        let mut history = history(&[("a", "b"), ("b", "c")]);
        let mut current = "c".to_owned();
        assert_eq!(step_back(&mut history, &mut current).as_deref(), Some("b"));
        assert_eq!(step_back(&mut history, &mut current).as_deref(), Some("a"));
        assert_eq!(step_back(&mut history, &mut current), None);
        assert_eq!(
            step_forward(&mut history, &mut current).as_deref(),
            Some("b")
        );
        assert_eq!(
            step_forward(&mut history, &mut current).as_deref(),
            Some("c")
        );
        assert_eq!(step_forward(&mut history, &mut current), None);
    }

    #[test]
    fn new_jump_after_going_back_discards_forward_entries() {
        let mut history = history(&[("a", "b"), ("b", "c")]);
        let mut current = "c".to_owned();
        step_back(&mut history, &mut current);
        history.observe_focus("b", "d", true);
        let mut current = "d".to_owned();
        assert_eq!(history.forward(&current, live), None);
        assert_eq!(step_back(&mut history, &mut current).as_deref(), Some("b"));
        assert_eq!(step_back(&mut history, &mut current).as_deref(), Some("a"));
    }

    #[test]
    fn back_returns_to_the_pane_that_was_left_within_a_tab() {
        // Arrived at b, moved to sibling b2 in the same tab (not recorded), jumped to c.
        let mut history = history(&[("a", "b"), ("b2", "c")]);
        assert_eq!(history.back("c", live).as_deref(), Some("b2"));
    }

    #[test]
    fn forward_returns_to_the_pane_focused_before_stepping_back() {
        let mut history = history(&[("a", "b")]);
        // Moved within b's tab to b2, then stepped back.
        let target = history.back("b2", live);
        assert_eq!(target.as_deref(), Some("a"));
        history.observe_focus("b2", "a", true);
        assert_eq!(history.forward("a", live).as_deref(), Some("b2"));
    }

    #[test]
    fn rapid_steps_before_focus_lands_are_not_recorded_as_jumps() {
        let mut history = history(&[("a", "b"), ("b", "c"), ("c", "d")]);
        // Two presses before any snapshot arrives; current is still d.
        assert_eq!(history.back("d", live).as_deref(), Some("c"));
        assert_eq!(history.back("d", live).as_deref(), Some("b"));
        // The server applies both; snapshots show d -> c, then c -> b.
        history.observe_focus("d", "c", true);
        history.observe_focus("c", "b", true);
        assert_eq!(history.forward("b", live).as_deref(), Some("c"));
    }

    #[test]
    fn steps_skip_panes_that_no_longer_exist() {
        let mut history = history(&[("a", "b"), ("b", "c"), ("c", "d")]);
        let alive = |pane: &str| pane != "b" && pane != "c";
        assert_eq!(history.back("d", alive).as_deref(), Some("a"));
    }

    #[test]
    fn removing_a_pane_collapses_neighbors_that_become_equal() {
        // a -> b -> a -> c, then b closes: a, a would be adjacent.
        let mut history = history(&[("a", "b"), ("b", "a"), ("a", "c")]);
        let alive = |pane: &str| pane != "b";
        assert_eq!(history.back("c", alive).as_deref(), Some("a"));
        history.observe_focus("c", "a", true);
        assert_eq!(history.back("a", alive), None);
    }

    #[test]
    fn history_is_capped_by_dropping_the_oldest_entries() {
        let names: Vec<String> = (0..=NAVIGATION_HISTORY_LIMIT + 5)
            .map(|n| format!("p{n}"))
            .collect();
        let mut history = NavigationHistory::default();
        for pair in names.windows(2) {
            history.observe_focus(&pair[0], &pair[1], true);
        }
        assert_eq!(history.entries.len(), NAVIGATION_HISTORY_LIMIT);
        let mut current = names.last().unwrap().clone();
        let mut steps = 0;
        while step_back(&mut history, &mut current).is_some() {
            steps += 1;
        }
        assert_eq!(steps, NAVIGATION_HISTORY_LIMIT - 1);
        assert_eq!(current, names[names.len() - NAVIGATION_HISTORY_LIMIT]);
    }

    #[test]
    fn a_failed_step_does_not_block_later_recording() {
        let mut history = history(&[("a", "b")]);
        // Step back is issued but the server never applies it.
        assert_eq!(history.back("b", live).as_deref(), Some("a"));
        // The user then jumps somewhere new; a must survive.
        history.observe_focus("b", "c", true);
        let mut current = "c".to_owned();
        assert_eq!(step_back(&mut history, &mut current).as_deref(), Some("b"));
        assert_eq!(step_back(&mut history, &mut current).as_deref(), Some("a"));
    }

    #[test]
    fn a_step_that_lands_within_a_tab_is_not_left_in_flight() {
        // x and a are in different tabs; a2 was later moved into a's tab, so
        // stepping back from a2 to a crosses no tab boundary.
        let mut history = history(&[("x", "a"), ("a", "a2")]);
        assert_eq!(history.back("a2", live).as_deref(), Some("a"));
        history.observe_focus("a2", "a", false);
        // A later jump to a pane already in the history is still recorded.
        history.observe_focus("a", "x", true);
        assert_eq!(history.back("x", live).as_deref(), Some("a"));
    }

    #[test]
    fn a_jump_off_the_step_path_while_a_step_is_in_flight_is_recorded() {
        let mut history = history(&[("a", "b"), ("b", "c"), ("c", "d")]);
        // Step back toward c is issued but never lands; the user jumps to a,
        // which is in the history but not where the step was heading.
        assert_eq!(history.back("d", live).as_deref(), Some("c"));
        history.observe_focus("d", "a", true);
        assert_eq!(history.back("a", live).as_deref(), Some("d"));
    }

    #[test]
    fn a_failed_step_restores_its_origin_after_entries_are_dropped() {
        let mut history = history(&[("a", "b"), ("b", "c"), ("c", "d"), ("d", "e")]);
        let mut current = "e".to_owned();
        step_back(&mut history, &mut current);
        // Two rapid presses from d; b closes between them.
        assert_eq!(history.back("d", live).as_deref(), Some("c"));
        let alive = |pane: &str| pane != "b";
        assert_eq!(history.back("d", alive).as_deref(), Some("a"));
        // Neither step lands; focus moves within d's tab instead.
        history.observe_focus("d", "d2", false);
        assert_eq!(history.back("d2", alive).as_deref(), Some("c"));
    }
}
