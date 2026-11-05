//! Mouse routing of the snapshot timeline (F11c).

use super::agents::ShepherdAgentsAction;
use super::snapshots::{ShepherdSnapshotsAction, SnapSub, SnapshotButton, SnapshotSource};
use super::snapshots_view::{FileButton, ShepherdSnapshotsHits};
use super::state::{ShepherdSectionAction, ShepherdSectionOutcome, ShepherdSectionState};
use ratatui::layout::Rect;

fn contains(r: Rect, p: (u16, u16)) -> bool {
    p.0 >= r.x && p.0 < r.x + r.width && p.1 >= r.y && p.1 < r.y + r.height
}

fn outcome(act: Option<ShepherdSnapshotsAction>) -> ShepherdSectionOutcome {
    match act {
        Some(a) => ShepherdSectionOutcome::Action(ShepherdSectionAction::Agents(
            ShepherdAgentsAction::Snapshots(a),
        )),
        None => ShepherdSectionOutcome::Handled,
    }
}

/// Left click inside the timeline; `None` = the click hit nothing interactive.
pub fn click(
    state: &mut ShepherdSectionState,
    hits: &ShepherdSnapshotsHits,
    point: (u16, u16),
) -> Option<ShepherdSectionOutcome> {
    let snaps = &mut state.agents.snapshots;

    // File selector (inline checkboxes)
    if let SnapSub::Files(_) = snaps.sub {
        for &(rect, idx) in &hits.file_rows {
            if contains(rect, point) {
                if let SnapSub::Files(sel) = &mut snaps.sub {
                    sel.cursor = idx;
                    if let Some(f) = sel.files.get_mut(idx) {
                        f.checked = !f.checked;
                    }
                }
                return Some(ShepherdSectionOutcome::Handled);
            }
        }
        for &(rect, b) in &hits.file_buttons {
            if contains(rect, point) {
                match b {
                    FileButton::Restore => snaps.submit_files(),
                    FileButton::Cancel => {
                        snaps.cancel();
                    }
                    FileButton::ToggleAll => {
                        if let SnapSub::Files(sel) = &mut snaps.sub {
                            let all = sel.files.iter().all(|f| f.checked);
                            sel.files.iter_mut().for_each(|f| f.checked = !all);
                        }
                    }
                }
                return Some(ShepherdSectionOutcome::Handled);
            }
        }
    }

    if let Some(form) = &hits.form_hits {
        if form.submit_button.is_some_and(|r| contains(r, point)) {
            return Some(outcome(snaps.submit_prune()));
        }
        if form.cancel_button.is_some_and(|r| contains(r, point)) {
            snaps.cancel();
            return Some(ShepherdSectionOutcome::Handled);
        }
    }
    if let Some(stripe) = &hits.stripe_hits {
        if stripe.confirm_button.is_some_and(|r| contains(r, point)) {
            return Some(outcome(snaps.confirm()));
        }
        if stripe.cancel_button.is_some_and(|r| contains(r, point)) {
            snaps.cancel();
            return Some(ShepherdSectionOutcome::Handled);
        }
    }
    if snaps.has_sub_action() {
        return None;
    }

    // List: capture button, source tabs, rows
    if hits
        .list_hits
        .action_button
        .is_some_and(|r| contains(r, point))
    {
        return Some(outcome(snaps.start_capture()));
    }
    for &(rect, idx) in &hits.list_hits.filter_tabs {
        if contains(rect, point) {
            let source = if idx == 0 {
                SnapshotSource::Agent
            } else {
                SnapshotSource::Finished
            };
            return Some(outcome(snaps.set_source(source)));
        }
    }
    for &(rect, idx) in &hits.list_hits.rows {
        if contains(rect, point) {
            let act = snaps.select(idx);
            if hits.detail_rect.is_none() {
                snaps.detail_open = true; // single-column layout: show the detail
            }
            return Some(outcome(act));
        }
    }

    // Detail: header capture, buttons, empty-state CTAs
    if hits.header_capture.is_some_and(|r| contains(r, point)) {
        return Some(outcome(snaps.start_capture()));
    }
    if let Some(d) = &hits.detail_hits {
        for &(rect, idx) in &d.bottom_actions {
            if contains(rect, point) {
                let act = SnapshotButton::ALL.get(idx).and_then(|b| snaps.press(*b));
                return Some(outcome(act));
            }
        }
    }
    if let Some(e) = &hits.empty_hits {
        for &(rect, idx) in &e.actions {
            if contains(rect, point) {
                let capture = snaps.source == SnapshotSource::Agent && idx == 0;
                return Some(if capture {
                    outcome(snaps.start_capture())
                } else {
                    snaps.cancel();
                    ShepherdSectionOutcome::Handled
                });
            }
        }
    }
    None
}

/// Wheel: over the detail it scrolls the diff, over the list it moves the selection.
pub fn wheel(
    state: &mut ShepherdSectionState,
    hits: &ShepherdSnapshotsHits,
    point: (u16, u16),
    up: bool,
) -> ShepherdSectionOutcome {
    let snaps = &mut state.agents.snapshots;
    if let SnapSub::Files(sel) = &mut snaps.sub {
        if up {
            sel.cursor = sel.cursor.saturating_sub(1);
        } else if sel.cursor + 1 < sel.files.len() {
            sel.cursor += 1;
        }
        return ShepherdSectionOutcome::Handled;
    }
    let over_detail = hits.detail_rect.is_some_and(|r| contains(r, point))
        || (hits.detail_rect.is_none() && snaps.detail_open);
    if over_detail {
        snaps.scroll_detail(if up { -3 } else { 3 });
        return ShepherdSectionOutcome::Handled;
    }
    outcome(if up {
        snaps.select_prev()
    } else {
        snaps.select_next()
    })
}
