//! F11c: snapshot timeline — pure render (cells), hit-testing, state machine and extreme data.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{buffer::Buffer, layout::Rect};
use shepherd_core::snapshot::{DiffFile, DiffResult, ShowResult, SnapshotInfo, SnapshotReason};

use super::snapshots::*;
use super::snapshots_view::rel_time;
use super::*;
use crate::app::state::Palette;
use crate::shepherd::panel::ShepherdAgentRow;

pub(super) const SIZES: [(u16, u16); 4] = [(200, 50), (120, 40), (80, 24), (55, 20)];
pub(super) const NOW: u64 = 1_800_000_000;

pub(super) fn snap(agent: &str, n: u64, reason: SnapshotReason) -> SnapshotInfo {
    SnapshotInfo {
        id: format!("{agent}/{n}"),
        agent: agent.into(),
        n,
        commit: format!("{n:040x}"),
        reason,
        created: NOW - 3600 + n * 10,
        worktree: "/tmp/wt1".into(),
        prev: (n > 1).then(|| format!("{agent}/{}", n - 1)),
        files: 3,
        added: 12,
        removed: 4,
        omitted: Vec::new(),
    }
}

pub(super) fn file(path: &str, status: &str) -> DiffFile {
    DiffFile {
        path: path.into(),
        status: status.into(),
        added: 5,
        removed: 1,
        binary: false,
    }
}

pub(super) fn diff(files: Vec<DiffFile>, patch: Option<String>) -> DiffResult {
    DiffResult {
        from: "coder-1/1".into(),
        to: "current".into(),
        added: files.iter().map(|f| f.added).sum(),
        removed: files.iter().map(|f| f.removed).sum(),
        files,
        patch,
    }
}

pub(super) fn agent_row(id: &str) -> ShepherdAgentRow {
    ShepherdAgentRow {
        id: id.into(),
        role: "coder".into(),
        vendor: "claude".into(),
        state: "working".into(),
        pane: "p1".into(),
        worktree: "/tmp/wt1".into(),
        snapshots: 3,
        last_snapshot: format!("{id}/3"),
        ..Default::default()
    }
}

pub(super) fn click_at(x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

pub(super) fn click(rect: Rect) -> MouseEvent {
    click_at(rect.x + rect.width / 2, rect.y)
}

pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(super) fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

pub(super) fn render(
    state: &mut ShepherdSectionState,
    w: u16,
    h: u16,
) -> (Buffer, ShepherdSectionHits) {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let hits = render_shepherd_section(&mut buf, area, state, &Palette::catppuccin());
    (buf, hits)
}

pub(super) fn text(buf: &Buffer) -> String {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

/// Section with one live agent and `n` snapshots, timeline open and summary loaded.
pub(super) fn timeline(n: u64) -> ShepherdSectionState {
    let mut st = ShepherdSectionState::default();
    st.agents.agents = vec![agent_row("coder-1")];
    st.agents.snapshots.active_agents = vec!["coder-1".into()];
    let reasons = [
        SnapshotReason::Manual,
        SnapshotReason::Idle,
        SnapshotReason::Done,
        SnapshotReason::Periodic,
        SnapshotReason::PreRestore,
        SnapshotReason::PreKill,
    ];
    let snaps: Vec<_> = (1..=n)
        .map(|i| snap("coder-1", i, reasons[i as usize % reasons.len()]))
        .collect();
    let act = st.agents.open_timeline();
    assert!(matches!(
        act,
        Some(ShepherdAgentsAction::Snapshots(
            ShepherdSnapshotsAction::List { agent: Some(_), .. }
        ))
    ));
    st.agents.snapshots.ingest_list(Some("coder-1"), snaps, NOW);
    st
}

pub(super) fn load_summary(st: &mut ShepherdSectionState) {
    let s = st.agents.snapshots.selected_snapshot().unwrap().clone();
    st.agents.snapshots.ingest_show(ShowResult {
        diff: diff(
            vec![file("src/a.rs", "modified"), file("b.txt", "added")],
            None,
        ),
        snapshot: s,
    });
}

pub(super) fn press(st: &mut ShepherdSectionState, c: KeyCode) -> ShepherdSectionOutcome {
    st.handle_key(&key(c))
}

pub(super) fn snap_action(o: ShepherdSectionOutcome) -> Option<ShepherdSnapshotsAction> {
    match o {
        ShepherdSectionOutcome::Action(ShepherdSectionAction::Agents(
            ShepherdAgentsAction::Snapshots(a),
        )) => Some(a),
        _ => None,
    }
}

// ---------- render (cells) ----------

#[test]
fn list_and_detail_render_in_all_sizes() {
    for (w, h) in SIZES {
        let mut st = timeline(5);
        load_summary(&mut st);
        let (buf, hits) = render(&mut st, w, h);
        let t = text(&buf);
        let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
        assert_eq!(sh.list_hits.rows.len(), 5, "{w}x{h}");
        assert!(sh.list_hits.action_button.is_some(), "{w}x{h}");
        assert_eq!(sh.list_hits.filter_tabs.len(), 2, "{w}x{h}");
        assert!(t.contains("Capture now"), "{w}x{h}");
        assert!(t.contains("Finalizados"), "{w}x{h}");
        // newest first, relative time and reason badge
        assert!(t.contains("#5"), "{w}x{h}");
        assert!(t.contains(" ago"), "{w}x{h}");
        if w >= 70 {
            let d = sh.detail_hits.as_ref().unwrap();
            assert_eq!(d.bottom_actions.len(), SnapshotButton::ALL.len(), "{w}x{h}");
            assert!(t.contains("SNAPSHOT: coder-1/5"), "{w}x{h}");
            assert!(t.contains("src/a.rs"), "{w}x{h}");
            assert!(t.contains("Restore file"), "{w}x{h}");
            assert!(t.contains("Prune"), "{w}x{h}");
        } else {
            assert!(sh.detail_hits.is_none(), "compact shows the list only");
        }
    }
}

#[test]
fn reason_colours_follow_the_palette_and_newest_is_first() {
    let mut st = timeline(6);
    load_summary(&mut st);
    let (buf, hits) = render(&mut st, 120, 40);
    let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
    let (row0, _) = sh.list_hits.rows[0];
    let line: String = (row0.x..row0.right())
        .map(|x| buf[(x, row0.y)].symbol())
        .collect();
    assert!(line.contains("#6"), "{line}");
    let p = Palette::catppuccin();
    // coder-1/6 is Manual (accent); coder-1/5 PreKill (red); coder-1/4 Periodic
    let badge_x = (row0.x..row0.right())
        .find(|x| buf[(*x, row0.y)].symbol() == "[")
        .unwrap();
    assert_eq!(buf[(badge_x + 1, row0.y)].fg, p.accent);
    let (row1, _) = sh.list_hits.rows[1];
    let bx = (row1.x..row1.right())
        .find(|x| buf[(*x, row1.y)].symbol() == "[")
        .unwrap();
    assert_eq!(buf[(bx + 1, row1.y)].fg, p.red);
}

#[test]
fn empty_state_explains_what_to_do_and_80x24_fits() {
    for (w, h) in SIZES {
        let mut st = timeline(0);
        let (buf, hits) = render(&mut st, w, h);
        let t = text(&buf);
        let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
        assert!(
            t.contains("No snapshots yet") || t.contains("NO SNAPSHOTS YET"),
            "{w}x{h}\n{t}"
        );
        assert!(sh.empty_hits.is_some(), "{w}x{h}");
        assert!(sh.list_hits.action_button.is_some(), "{w}x{h}");
    }
    // finished tab, empty
    let mut st = timeline(0);
    let _ = st.agents.snapshots.set_source(SnapshotSource::Finished);
    let (buf, _) = render(&mut st, 120, 40);
    assert!(text(&buf).contains("NO SNAPSHOTS OF FINISHED AGENTS"));
}

#[test]
fn busy_error_and_notice_are_shown_inside_the_panel() {
    let mut st = timeline(3);
    load_summary(&mut st);
    st.agents.snapshots.busy = Some("loading diff…".into());
    let (buf, _) = render(&mut st, 120, 40);
    assert!(text(&buf).contains("Working… (loading diff…)"));
    st.agents.snapshots.ingest_error("git exploded");
    let (buf, _) = render(&mut st, 120, 40);
    let t = text(&buf);
    assert!(t.contains("Error: git exploded"));
    assert!(!t.contains("Trabajando…"));
}

#[test]
fn confirmation_stripes_and_prune_form_render_with_buttons() {
    let mut st = timeline(3);
    load_summary(&mut st);
    for (setup, expect) in [(b'R', "pre-restore"), (b'x', "Delete snapshot coder-1/3?")] {
        st.agents.snapshots.sub = SnapSub::None;
        press(&mut st, KeyCode::Char(setup as char));
        let (buf, hits) = render(&mut st, 100, 30);
        let t = text(&buf);
        assert!(t.contains("CONFIRMATION"), "{t}");
        assert!(t.contains(expect), "{expect}\n{t}");
        let stripe = hits
            .agents_hits
            .clone()
            .unwrap()
            .snapshots
            .unwrap()
            .stripe_hits
            .unwrap();
        assert!(stripe.confirm_button.is_some() && stripe.cancel_button.is_some());
    }
    st.agents.snapshots.sub = SnapSub::None;
    press(&mut st, KeyCode::Char('p'));
    let (buf, hits) = render(&mut st, 100, 30);
    assert!(text(&buf).contains("PRUNE SNAPSHOTS OF coder-1"));
    let f = hits
        .agents_hits
        .clone()
        .unwrap()
        .snapshots
        .unwrap()
        .form_hits
        .unwrap();
    assert!(f.submit_button.is_some() && f.cancel_button.is_some());
}

#[test]
fn diff_is_coloured_and_truncated_with_notice() {
    let mut st = timeline(2);
    load_summary(&mut st);
    let mut patch = String::from("diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+new\n");
    for i in 0..5000 {
        patch.push_str(&format!("+línea {i} 日本語\n"));
    }
    let id = st.agents.snapshots.selected_snapshot().unwrap().id.clone();
    st.agents.snapshots.ingest_diff(
        &id,
        DiffMode::Current,
        &diff(vec![file("x", "modified")], Some(patch)),
    );
    let d = st.agents.snapshots.diff_for_selected().unwrap();
    assert_eq!(d.lines.len(), MAX_DIFF_LINES);
    assert_eq!(d.hidden, 5004 - MAX_DIFF_LINES);
    let (buf, hits) = render(&mut st, 120, 40);
    let t = text(&buf);
    assert!(t.contains("DIFF vs estado actual"));
    let p = Palette::catppuccin();
    // find the "-old" and "+new" cells and check colours
    let mut found = (false, false);
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        if let Some(x) = row.find("-old") {
            found.0 = buf[(x as u16, y)].fg == p.red;
        }
        if let Some(x) = row.find("+new") {
            found.1 = buf[(x as u16, y)].fg == p.green;
        }
    }
    assert_eq!(found, (true, true));
    assert!(hits
        .agents_hits
        .clone()
        .unwrap()
        .snapshots
        .unwrap()
        .detail_hits
        .unwrap()
        .scrollbar
        .is_some());
    // the truncation notice sits above the (long) patch, visible without scrolling
    assert!(text(&buf).contains("Diff truncado"));
}

#[test]
fn extreme_data_never_panics_and_keeps_selection_visible() {
    for (w, h) in SIZES
        .into_iter()
        .chain([(30, 8), (31, 9), (20, 5), (0, 0), (1, 1)])
    {
        let mut st = timeline(200);
        st.agents.snapshots.selected = 199;
        let id = st.agents.snapshots.selected_snapshot().unwrap().id.clone();
        let mut s = st.agents.snapshots.selected_snapshot().unwrap().clone();
        s.worktree = format!("/tmp/{}", "d/".repeat(60));
        s.omitted = vec![shepherd_core::snapshot::OmittedFile {
            path: format!("日本語/{}", "x".repeat(300)),
            reason: "too-large".into(),
            size: 9,
        }];
        st.agents.snapshots.agent_snaps[199] = s.clone();
        st.agents.snapshots.ingest_show(ShowResult {
            snapshot: s,
            diff: diff(
                (0..600)
                    .map(|i| {
                        file(
                            &format!("dir-日本語-{i}/{}.rs", "n".repeat(120)),
                            "modified",
                        )
                    })
                    .collect(),
                None,
            ),
        });
        let _ = id;
        let (buf, hits) = render(&mut st, w, h);
        if w >= 70 && h >= 8 {
            let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
            // the selected row (the oldest, last in newest-first order) is on screen
            assert!(sh.list_hits.rows.iter().any(|(_, i)| *i == 199), "{w}x{h}");
            let _ = &buf;
        }
    }
}

#[test]
fn rel_time_formats() {
    assert_eq!(rel_time(100, 100), "now");
    assert_eq!(rel_time(100, 500), "now"); // clock skew
    assert_eq!(rel_time(100, 70), "30s ago");
    assert_eq!(rel_time(1000, 100), "15m ago");
    assert_eq!(rel_time(50_000, 100), "13h ago");
    assert_eq!(rel_time(1_000_000, 100), "11d ago");
}

// ---------- state machine ----------

#[test]
fn selection_requests_missing_summary_once() {
    let mut st = timeline(3);
    let s = &mut st.agents.snapshots;
    // first summary request (selected = newest = coder-1/3)
    let a = s.summary_request().unwrap();
    assert_eq!(
        a,
        ShepherdSnapshotsAction::Show {
            root: "/tmp/wt1".into(),
            id: "coder-1/3".into()
        }
    );
    assert!(s.summary_request().is_none(), "in flight: not repeated");
    let a = s.select_next().unwrap();
    assert!(matches!(a, ShepherdSnapshotsAction::Show { ref id, .. } if id == "coder-1/2"));
    assert!(
        s.select_prev().is_some(),
        "back on 3 whose summary is not in flight any more"
    );
}

#[test]
fn busy_blocks_slow_actions_but_not_navigation() {
    let mut st = timeline(3);
    let s = &mut st.agents.snapshots;
    assert!(s.start_capture().is_some());
    assert!(s.is_busy());
    assert!(s.start_capture().is_none());
    assert!(s.start_diff(DiffMode::Previous).is_none());
    assert!(s.start_restore_file().is_none());
    s.start_delete();
    assert_eq!(s.sub, SnapSub::None);
    assert!(
        s.refresh_requests().is_empty(),
        "no background polling while busy"
    );
    s.select_next();
    assert_eq!(s.selected, 1);
}

#[test]
fn confirm_and_cancel_have_no_effect_until_confirmed() {
    let mut st = timeline(3);
    // Esc on every confirmation: no action, back to normal
    for k in ['R', 'x'] {
        assert_eq!(snap_action(st.handle_key(&ch(k))), None);
        assert!(st.agents.snapshots.has_sub_action());
        assert!(st.has_active_sub_modal_or_stripe());
        assert_eq!(
            press(&mut st, KeyCode::Esc),
            ShepherdSectionOutcome::Handled
        );
        assert!(!st.agents.snapshots.has_sub_action());
        assert!(!st.agents.snapshots.is_busy());
        assert!(st.agents.snapshots.open, "Esc only cancelled the stripe");
    }
    // other keys are swallowed by the stripe
    st.handle_key(&ch('R'));
    assert_eq!(
        press(&mut st, KeyCode::Down),
        ShepherdSectionOutcome::Handled
    );
    assert_eq!(st.agents.snapshots.selected, 0);
    // Enter confirms → full restore (empty paths), busy
    let a = snap_action(press(&mut st, KeyCode::Enter)).unwrap();
    assert_eq!(
        a,
        ShepherdSnapshotsAction::Restore {
            root: "/tmp/wt1".into(),
            id: "coder-1/3".into(),
            paths: vec![]
        }
    );
    assert_eq!(st.agents.snapshots.busy.as_deref(), Some("restoring…"));
    // delete
    st.agents.snapshots.busy = None;
    st.handle_key(&ch('x'));
    let a = snap_action(press(&mut st, KeyCode::Enter)).unwrap();
    assert!(matches!(a, ShepherdSnapshotsAction::Delete { ref id, .. } if id == "coder-1/3"));
}

#[test]
fn file_selector_toggle_all_none_and_confirmation() {
    let mut st = timeline(3);
    let id = "coder-1/3".to_string();
    let a = snap_action(st.handle_key(&ch('f'))).unwrap();
    assert!(matches!(a, ShepherdSnapshotsAction::FilesVsCurrent { .. }));
    assert_eq!(
        st.agents.snapshots.pending_files.as_deref(),
        Some("coder-1/3")
    );
    // stale answer for another snapshot is ignored
    st.agents
        .snapshots
        .ingest_files("coder-1/1", &diff(vec![file("z", "modified")], None));
    assert_eq!(st.agents.snapshots.sub, SnapSub::None);
    st.agents.snapshots.ingest_files(
        &id,
        &diff(
            vec![
                file("a.rs", "modified"),
                file("b.rs", "deleted"),
                file("new.rs", "added"),
            ],
            None,
        ),
    );
    let SnapSub::Files(sel) = &st.agents.snapshots.sub else {
        panic!("selector expected");
    };
    assert_eq!(sel.files.len(), 2, "files only present now are not offered");
    assert_eq!(sel.not_restorable, 1);
    // Enter with nothing checked: error, stays
    press(&mut st, KeyCode::Enter);
    assert!(st
        .agents
        .snapshots
        .error
        .as_deref()
        .unwrap()
        .contains("at least one file"));
    assert!(matches!(st.agents.snapshots.sub, SnapSub::Files(_)));
    // space toggles under the cursor, j moves
    st.handle_key(&ch(' '));
    st.handle_key(&ch('j'));
    st.handle_key(&ch(' '));
    st.handle_key(&ch(' '));
    let SnapSub::Files(sel) = &st.agents.snapshots.sub else {
        panic!()
    };
    assert_eq!(sel.checked_paths(), vec!["a.rs".to_string()]);
    // 'a' marks all, again clears
    st.handle_key(&ch('a'));
    let SnapSub::Files(sel) = &st.agents.snapshots.sub else {
        panic!()
    };
    assert_eq!(sel.checked_paths().len(), 2);
    st.handle_key(&ch('a'));
    st.handle_key(&ch('a'));
    // Enter → stripe, Esc → nothing sent, everything back to normal
    press(&mut st, KeyCode::Enter);
    assert!(matches!(
        st.agents.snapshots.sub,
        SnapSub::ConfirmFiles { .. }
    ));
    press(&mut st, KeyCode::Esc);
    assert_eq!(st.agents.snapshots.sub, SnapSub::None);
    assert!(!st.agents.snapshots.is_busy());
}

#[test]
fn file_restore_sends_only_checked_paths_and_empty_diff_reports() {
    let mut st = timeline(3);
    st.handle_key(&ch('f'));
    st.agents
        .snapshots
        .ingest_files("coder-1/3", &diff(vec![], None));
    assert!(st
        .agents
        .snapshots
        .notice
        .as_deref()
        .unwrap()
        .contains("nothing to restore"));
    assert_eq!(st.agents.snapshots.sub, SnapSub::None);

    st.handle_key(&ch('f'));
    st.agents.snapshots.ingest_files(
        "coder-1/3",
        &diff(
            vec![file("a b.rs", "modified"), file("c.rs", "deleted")],
            None,
        ),
    );
    st.handle_key(&ch('j'));
    st.handle_key(&ch(' '));
    press(&mut st, KeyCode::Enter);
    let a = snap_action(press(&mut st, KeyCode::Enter)).unwrap();
    assert_eq!(
        a,
        ShepherdSnapshotsAction::Restore {
            root: "/tmp/wt1".into(),
            id: "coder-1/3".into(),
            paths: vec!["c.rs".into()]
        }
    );
}

#[test]
fn prune_form_validates_keep() {
    let mut st = timeline(3);
    st.handle_key(&ch('p'));
    let SnapSub::Prune(f) = &st.agents.snapshots.sub else {
        panic!()
    };
    assert_eq!(f.fields[0].value, DEFAULT_PRUNE_KEEP);
    for bad in ["", "0", "-1", "abc", "1.5"] {
        if let SnapSub::Prune(f) = &mut st.agents.snapshots.sub {
            f.fields[0].value = bad.into();
        }
        assert_eq!(snap_action(press(&mut st, KeyCode::Enter)), None, "{bad:?}");
        let SnapSub::Prune(f) = &st.agents.snapshots.sub else {
            panic!("form must stay open for {bad:?}")
        };
        assert!(f.error.as_deref().unwrap().contains("≥ 1"));
    }
    if let SnapSub::Prune(f) = &mut st.agents.snapshots.sub {
        f.fields[0].value = "2".into();
    }
    let a = snap_action(press(&mut st, KeyCode::Enter)).unwrap();
    assert_eq!(
        a,
        ShepherdSnapshotsAction::Prune {
            root: Some("/tmp/wt1".into()),
            agent: "coder-1".into(),
            keep: 2
        }
    );
    // typing then Esc cancels with no action
    st.agents.snapshots.busy = None;
    st.handle_key(&ch('p'));
    st.handle_key(&key(KeyCode::Backspace));
    st.handle_key(&ch('7'));
    assert_eq!(
        press(&mut st, KeyCode::Esc),
        ShepherdSectionOutcome::Handled
    );
    assert_eq!(st.agents.snapshots.sub, SnapSub::None);
}

#[test]
fn prune_is_refused_on_the_finished_tab() {
    let mut st = timeline(2);
    st.agents.snapshots.set_source(SnapshotSource::Finished);
    st.handle_key(&ch('p'));
    assert_eq!(st.agents.snapshots.sub, SnapSub::None);
    assert!(st.agents.snapshots.error.is_some());
}

#[test]
fn esc_hierarchy_and_tab_shortcuts() {
    let mut st = timeline(2);
    // tab digits work while the timeline has no sub-action
    assert!(!st.has_active_sub_modal_or_stripe());
    st.agents.snapshots.detail_open = true;
    assert_eq!(
        press(&mut st, KeyCode::Esc),
        ShepherdSectionOutcome::Handled
    );
    assert!(!st.agents.snapshots.detail_open && st.agents.snapshots.open);
    assert_eq!(
        press(&mut st, KeyCode::Esc),
        ShepherdSectionOutcome::Handled
    );
    assert!(!st.agents.snapshots.open, "second Esc closes the timeline");
    assert_eq!(press(&mut st, KeyCode::Esc), ShepherdSectionOutcome::Close);
}

#[test]
fn results_update_state_and_stale_list_is_ignored() {
    let mut st = timeline(2);
    let s = &mut st.agents.snapshots;
    // list for another agent: ignored
    s.ingest_list(
        Some("other"),
        vec![snap("other", 1, SnapshotReason::Idle)],
        NOW,
    );
    assert_eq!(s.agent_snaps.len(), 2);
    // finished list drops live agents
    s.ingest_list(
        None,
        vec![
            snap("coder-1", 1, SnapshotReason::Idle),
            snap("old-9", 4, SnapshotReason::Done),
        ],
        NOW,
    );
    assert_eq!(s.finished_snaps.len(), 1);
    assert_eq!(s.finished_snaps[0].id, "old-9/4");
    // the same snapshot stays selected when a newer one arrives
    s.selected = 1; // coder-1/1
    let mut v = s.agent_snaps.clone();
    v.push(snap("coder-1", 3, SnapshotReason::Manual));
    s.ingest_list(Some("coder-1"), v, NOW);
    assert_eq!(s.selected_snapshot().unwrap().id, "coder-1/1");
    // errors keep the message and clear busy
    s.busy = Some("x".into());
    s.ingest_error("boom");
    assert!(!s.is_busy());
    assert_eq!(s.error.as_deref(), Some("boom"));
    // new request clears the old error
    s.start_capture();
    assert!(s.error.is_none());
}

#[test]
fn failed_background_summary_is_silent_and_not_retried_until_selection_changes() {
    let mut st = timeline(3);
    let s = &mut st.agents.snapshots;
    assert!(s.summary_request().is_some());
    s.ingest_summary_error(); // e.g. the snapshot was deleted while the request was in flight
    assert!(s.error.is_none(), "no banner for a background summary");
    assert!(
        s.summary_request().is_none(),
        "no retry loop every refresh tick"
    );
    assert!(s.select_next().is_some(), "another snapshot asks again");
    // a real (user-triggered) failure still shows up
    s.ingest_error("boom");
    assert_eq!(s.error.as_deref(), Some("boom"));
}

#[test]
fn capture_and_restore_select_the_snapshot_they_created() {
    let mut st = timeline(2);
    let s = &mut st.agents.snapshots;
    s.selected = 1; // looking at #1
    let mut newest = snap("coder-1", 3, SnapshotReason::PreRestore);
    newest.created = NOW;
    s.pending_select = Some(newest.id.clone());
    let mut v = s.agent_snaps.clone();
    v.push(newest);
    s.ingest_list(Some("coder-1"), v, NOW);
    assert_eq!(s.selected_snapshot().unwrap().id, "coder-1/3");
    assert!(s.pending_select.is_none(), "consumed by the first list");
}

#[test]
fn source_switch_and_finished_list_use_repo_wide_request() {
    let mut st = timeline(2);
    let a = snap_action(press(&mut st, KeyCode::Tab)).unwrap();
    assert_eq!(
        a,
        ShepherdSnapshotsAction::List {
            root: Some("/tmp/wt1".into()),
            agent: None
        }
    );
    assert_eq!(st.agents.snapshots.source, SnapshotSource::Finished);
    let a = snap_action(press(&mut st, KeyCode::Tab)).unwrap();
    assert!(
        matches!(a, ShepherdSnapshotsAction::List { agent: Some(ref x), .. } if x == "coder-1")
    );
}

#[test]
fn t_without_agents_opens_finished_timeline() {
    let mut st = ShepherdSectionState::default();
    let a = snap_action(press(&mut st, KeyCode::Char('t'))).unwrap();
    assert_eq!(
        a,
        ShepherdSnapshotsAction::List {
            root: None,
            agent: None
        }
    );
    assert_eq!(st.agents.snapshots.source, SnapshotSource::Finished);
    assert!(st.agents.snapshots.agent_id.is_empty());
    // capturing needs an agent
    st.agents.snapshots.start_capture();
    assert!(st.agents.snapshots.error.is_some());
}
