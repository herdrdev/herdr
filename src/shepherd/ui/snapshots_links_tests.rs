//! F11c: snapshot timeline — mouse hit-testing and links from the Agents/Git tabs.

use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};

use super::snapshots::*;
use super::snapshots_tests::*;
use super::snapshots_view::FileButton;
use super::*;
use crate::shepherd::git::ShepherdGitRow;

// ---------- mouse ----------

#[test]
fn every_timeline_button_is_clickable() {
    for (w, h) in [(200, 50), (120, 40), (80, 24)] {
        // list: capture button, rows, source tabs
        let mut st = timeline(4);
        load_summary(&mut st);
        let (_, hits) = render(&mut st, w, h);
        let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
        let o = handle_shepherd_section_mouse(
            &mut st,
            &hits,
            click(sh.list_hits.action_button.unwrap()),
        );
        assert!(
            matches!(
                snap_action(o),
                Some(ShepherdSnapshotsAction::Capture { .. })
            ),
            "{w}x{h}"
        );
        st.agents.snapshots.busy = None;

        let (_, hits) = render(&mut st, w, h);
        let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
        let (row, idx) = sh.list_hits.rows[2];
        let o = handle_shepherd_section_mouse(&mut st, &hits, click(row));
        assert_eq!(st.agents.snapshots.selected, idx);
        assert!(
            snap_action(o).is_some(),
            "selecting asks for the missing summary"
        );

        let (_, hits) = render(&mut st, w, h);
        let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
        let (tab, _) = sh.list_hits.filter_tabs[1];
        let o = handle_shepherd_section_mouse(&mut st, &hits, click(tab));
        assert!(matches!(
            snap_action(o),
            Some(ShepherdSnapshotsAction::List { agent: None, .. })
        ));
        let (_, hits) = render(&mut st, w, h);
        let (tab, _) = hits
            .agents_hits
            .clone()
            .unwrap()
            .snapshots
            .unwrap()
            .list_hits
            .filter_tabs[0];
        handle_shepherd_section_mouse(&mut st, &hits, click(tab));
        assert_eq!(st.agents.snapshots.source, SnapshotSource::Agent);

        // detail buttons, one by one
        for (i, b) in SnapshotButton::ALL.iter().enumerate() {
            let mut st = timeline(4);
            load_summary(&mut st);
            let (_, hits) = render(&mut st, w, h);
            let d = hits
                .agents_hits
                .clone()
                .unwrap()
                .snapshots
                .unwrap()
                .detail_hits
                .unwrap();
            let (rect, _) = *d.bottom_actions.iter().find(|(_, k)| *k == i).unwrap();
            let o = handle_shepherd_section_mouse(&mut st, &hits, click(rect));
            let s = &st.agents.snapshots;
            match b {
                SnapshotButton::DiffPrevious => assert!(matches!(
                    snap_action(o),
                    Some(ShepherdSnapshotsAction::DiffPrevious { .. })
                )),
                SnapshotButton::DiffCurrent => assert!(matches!(
                    snap_action(o),
                    Some(ShepherdSnapshotsAction::DiffCurrent { .. })
                )),
                SnapshotButton::RestoreFile => assert!(matches!(
                    snap_action(o),
                    Some(ShepherdSnapshotsAction::FilesVsCurrent { .. })
                )),
                SnapshotButton::RestoreAll => assert!(matches!(s.sub, SnapSub::ConfirmAll(_))),
                SnapshotButton::Delete => assert!(matches!(s.sub, SnapSub::ConfirmDelete(_))),
                SnapshotButton::Prune => assert!(matches!(s.sub, SnapSub::Prune(_))),
                SnapshotButton::Back => assert!(!s.open),
            }
        }
    }
}

#[test]
fn stripe_form_and_selector_buttons_are_clickable() {
    let (w, h) = (100, 30);
    // restore all: confirm
    let mut st = timeline(3);
    load_summary(&mut st);
    st.agents.snapshots.start_restore_all();
    let (_, hits) = render(&mut st, w, h);
    let stripe = hits
        .agents_hits
        .clone()
        .unwrap()
        .snapshots
        .unwrap()
        .stripe_hits
        .unwrap();
    // cancel first: no action
    let o = handle_shepherd_section_mouse(&mut st, &hits, click(stripe.cancel_button.unwrap()));
    assert_eq!(snap_action(o), None);
    assert_eq!(st.agents.snapshots.sub, SnapSub::None);
    assert!(!st.agents.snapshots.is_busy());
    st.agents.snapshots.start_restore_all();
    let (_, hits) = render(&mut st, w, h);
    let stripe = hits
        .agents_hits
        .clone()
        .unwrap()
        .snapshots
        .unwrap()
        .stripe_hits
        .unwrap();
    let o = handle_shepherd_section_mouse(&mut st, &hits, click(stripe.confirm_button.unwrap()));
    assert!(matches!(
        snap_action(o),
        Some(ShepherdSnapshotsAction::Restore { .. })
    ));

    // prune form buttons
    let mut st = timeline(3);
    st.agents.snapshots.start_prune();
    let (_, hits) = render(&mut st, w, h);
    let f = hits
        .agents_hits
        .clone()
        .unwrap()
        .snapshots
        .unwrap()
        .form_hits
        .unwrap();
    let o = handle_shepherd_section_mouse(&mut st, &hits, click(f.submit_button.unwrap()));
    assert!(matches!(
        snap_action(o),
        Some(ShepherdSnapshotsAction::Prune { keep: 10, .. })
    ));
    let mut st = timeline(3);
    st.agents.snapshots.start_prune();
    let (_, hits) = render(&mut st, w, h);
    let f = hits
        .agents_hits
        .clone()
        .unwrap()
        .snapshots
        .unwrap()
        .form_hits
        .unwrap();
    handle_shepherd_section_mouse(&mut st, &hits, click(f.cancel_button.unwrap()));
    assert_eq!(st.agents.snapshots.sub, SnapSub::None);

    // file selector: row click toggles, buttons work at 80x24 too
    for (w, h) in [(100, 30), (80, 24)] {
        let mut st = timeline(3);
        st.handle_key(&ch('f'));
        st.agents.snapshots.ingest_files(
            "coder-1/3",
            &diff(
                (0..30)
                    .map(|i| file(&format!("f{i}.rs"), "modified"))
                    .collect(),
                None,
            ),
        );
        let (buf, hits) = render(&mut st, w, h);
        let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
        assert!(
            text(&buf).contains("RESTORE FILES FROM coder-1/3"),
            "{w}x{h}"
        );
        assert_eq!(sh.file_buttons.len(), 3, "{w}x{h}");
        let (r0, i0) = sh.file_rows[0];
        handle_shepherd_section_mouse(&mut st, &hits, click(r0));
        let SnapSub::Files(sel) = &st.agents.snapshots.sub else {
            panic!()
        };
        assert!(sel.files[i0].checked);
        let (_, hits) = render(&mut st, w, h);
        let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
        let btn = |b| sh.file_buttons.iter().find(|(_, x)| *x == b).unwrap().0;
        handle_shepherd_section_mouse(&mut st, &hits, click(btn(FileButton::ToggleAll)));
        let SnapSub::Files(sel) = &st.agents.snapshots.sub else {
            panic!()
        };
        assert_eq!(
            sel.checked_paths().len(),
            30,
            "toggle-all on a partial selection checks everything"
        );
        handle_shepherd_section_mouse(&mut st, &hits, click(btn(FileButton::Restore)));
        assert!(matches!(
            st.agents.snapshots.sub,
            SnapSub::ConfirmFiles { .. }
        ));
        // cancel from the stripe returns to normal without sending anything
        let (_, hits) = render(&mut st, w, h);
        let stripe = hits
            .agents_hits
            .clone()
            .unwrap()
            .snapshots
            .unwrap()
            .stripe_hits
            .unwrap();
        handle_shepherd_section_mouse(&mut st, &hits, click(stripe.cancel_button.unwrap()));
        assert_eq!(st.agents.snapshots.sub, SnapSub::None);
        assert!(!st.agents.snapshots.is_busy());
    }
}

#[test]
fn compact_layout_opens_detail_on_row_click_and_wheel_scrolls() {
    let mut st = timeline(4);
    load_summary(&mut st);
    let (_, hits) = render(&mut st, 55, 20);
    let sh = hits.agents_hits.clone().unwrap().snapshots.unwrap();
    assert!(sh.detail_hits.is_none());
    let (row, _) = sh.list_hits.rows[1];
    handle_shepherd_section_mouse(&mut st, &hits, click(row));
    assert!(st.agents.snapshots.detail_open);
    load_summary(&mut st);
    let (buf, hits) = render(&mut st, 55, 20);
    let t = text(&buf);
    assert!(t.contains("SNAPSHOT: coder-1/3"), "{t}");
    assert!(hits
        .agents_hits
        .clone()
        .unwrap()
        .snapshots
        .unwrap()
        .detail_hits
        .is_some());
    // wheel scrolls the detail in single-column mode
    let before = st.agents.snapshots.detail_scroll;
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    let (_, hits) = render(&mut st, 55, 20);
    handle_shepherd_section_mouse(&mut st, &hits, wheel);
    assert!(st.agents.snapshots.detail_scroll > before);
    // Esc goes back to the list, then closes
    press(&mut st, KeyCode::Esc);
    assert!(!st.agents.snapshots.detail_open);
}

// ---------- Agents detail + Git tab links ----------

#[test]
fn agent_detail_shows_snapshot_summary_and_button_opens_timeline() {
    let mut st = ShepherdSectionState::default();
    st.agents.agents = vec![agent_row("coder-1")];
    let (buf, hits) = render(&mut st, 120, 40);
    let t = text(&buf);
    assert!(t.contains("3 snapshot(s); latest: coder-1/3"), "{t}");
    let d = hits.agents_hits.clone().unwrap().detail_hits.unwrap();
    let (rect, _) = *d.bottom_actions.iter().find(|(_, i)| *i == 3).unwrap();
    let o = handle_shepherd_section_mouse(&mut st, &hits, click(rect));
    assert!(matches!(
        snap_action(o),
        Some(ShepherdSnapshotsAction::List { agent: Some(_), .. })
    ));
    assert!(st.agents.snapshots.open);
    // agent without snapshots explains how they appear
    let mut st = ShepherdSectionState::default();
    let mut a = agent_row("coder-2");
    a.snapshots = 0;
    a.last_snapshot.clear();
    st.agents.agents = vec![a];
    let (buf, _) = render(&mut st, 120, 40);
    assert!(text(&buf).contains("No snapshots yet"));
}

#[test]
fn empty_agents_tab_offers_snapshots_of_finished_agents() {
    let mut st = ShepherdSectionState::default();
    let (buf, hits) = render(&mut st, 120, 40);
    assert!(text(&buf).contains("Snapshots (t)"));
    let e = hits.agents_hits.clone().unwrap().empty_hits.unwrap();
    let (rect, _) = *e.actions.iter().find(|(_, i)| *i == 2).unwrap();
    let o = handle_shepherd_section_mouse(&mut st, &hits, click(rect));
    assert!(snap_action(o).is_some());
    assert_eq!(st.agents.snapshots.source, SnapshotSource::Finished);
}

fn git_row(path: &str, agent: Option<&str>) -> ShepherdGitRow {
    ShepherdGitRow {
        path: path.into(),
        branch: "shep/1".into(),
        is_clean: true,
        agent_id: agent.map(String::from),
        pr_display: "—".into(),
        ..Default::default()
    }
}

#[test]
fn git_tab_summarises_snapshots_and_links_to_the_timeline() {
    for key_instead_of_click in [false, true] {
        let mut st = ShepherdSectionState::default();
        st.agents.agents = vec![agent_row("coder-1"), agent_row("coder-2")];
        st.git.rows = vec![git_row("/tmp/other", None), git_row("/tmp/wt1/", None)];
        st.git.selected = 1; // matched by worktree path, trailing slash tolerated
        st.select_tab(ShepherdSectionTab::Git);
        let (buf, hits) = render(&mut st, 120, 40);
        let t = text(&buf);
        assert!(
            t.contains("Agent coder-1: 3 snapshot(s); latest coder-1/3"),
            "{t}"
        );
        let o = if key_instead_of_click {
            st.handle_key(&ch('t'))
        } else {
            let rect = hits.git_hits.clone().unwrap().timeline_button.unwrap();
            handle_shepherd_section_mouse(&mut st, &hits, click(rect))
        };
        assert_eq!(st.active_tab, ShepherdSectionTab::Agents);
        assert_eq!(st.agents.selected, 0);
        assert_eq!(st.agents.snapshots.agent_id, "coder-1");
        assert!(
            matches!(snap_action(o), Some(ShepherdSnapshotsAction::List { agent: Some(ref a), .. }) if a == "coder-1")
        );
    }
}

#[test]
fn git_tab_without_agent_opens_finished_timeline_for_that_worktree() {
    let mut st = ShepherdSectionState::default();
    st.git.rows = vec![git_row("/tmp/lonely", None)];
    st.select_tab(ShepherdSectionTab::Git);
    let (buf, hits) = render(&mut st, 120, 40);
    assert!(text(&buf).contains("No active agent here"));
    let rect = hits.git_hits.clone().unwrap().timeline_button.unwrap();
    let o = handle_shepherd_section_mouse(&mut st, &hits, click(rect));
    assert_eq!(
        snap_action(o),
        Some(ShepherdSnapshotsAction::List {
            root: Some("/tmp/lonely".into()),
            agent: None
        })
    );
    assert_eq!(st.agents.snapshots.source, SnapshotSource::Finished);
}

#[test]
fn git_timeline_button_visible_at_80x24_and_in_main_worktree() {
    for (w, h) in [(80, 24), (120, 40)] {
        let mut st = ShepherdSectionState::default();
        let mut r = git_row("/repo", None);
        r.is_main = true;
        st.git.rows = vec![r];
        st.select_tab(ShepherdSectionTab::Git);
        let (_, hits) = render(&mut st, w, h);
        assert!(
            hits.git_hits.clone().unwrap().timeline_button.is_some(),
            "{w}x{h}"
        );
    }
}

#[test]
fn status_bar_and_summary_follow_the_timeline() {
    let mut st = timeline(2);
    let (buf, _) = render(&mut st, 160, 40);
    let t = text(&buf);
    assert!(t.contains("Snapshots of coder-1 (2)"), "{t}");
    assert!(t.contains("Esc: volver"));
}

// ---------- integration with the default render helpers ----------

#[test]
fn update_from_agents_tracks_active_agents() {
    let mut st = ShepherdAgentsSectionState::default();
    let live = shepherd_core::Agent {
        id: "a".into(),
        state: "working".into(),
        snapshots: 4,
        last_snapshot: "a/4".into(),
        worktree: "/w".into(),
        ..Default::default()
    };
    let dead = shepherd_core::Agent {
        id: "b".into(),
        state: "killed".into(),
        ..Default::default()
    };
    st.update_from_agents(&[live, dead]);
    assert_eq!(st.snapshots.active_agents, vec!["a".to_string()]);
    assert_eq!(st.agents[0].snapshots, 4);
    assert_eq!(st.agents[0].worktree, "/w");
}
