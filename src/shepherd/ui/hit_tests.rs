//! Mouse hit-testing and extreme-data tests for the whole Shepherd section (F10d).
//! They drive `render_shepherd_section` + `handle_shepherd_section_mouse` like the TUI does.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{buffer::Buffer, layout::Rect};

use super::*;
use crate::app::state::Palette;
use crate::shepherd::git::ShepherdGitRow;
use crate::shepherd::panel::ShepherdAgentRow;
use crate::shepherd::runs::ShepherdRunRow;
use crate::shepherd::tasks::ShepherdTaskRow;
use events::{BusMessage, PatternRun, PatternRunState, PatternRunSummary, ShepherdTopicRow};

const SIZES: [(u16, u16); 4] = [(200, 50), (120, 40), (80, 24), (55, 20)];

fn ev(kind: MouseEventKind, rect: Rect) -> MouseEvent {
    MouseEvent {
        kind,
        column: rect.x + rect.width / 2,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    }
}

fn click(rect: Rect) -> MouseEvent {
    ev(MouseEventKind::Down(MouseButton::Left), rect)
}

fn render(state: &mut ShepherdSectionState, w: u16, h: u16) -> (Buffer, ShepherdSectionHits) {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let hits = render_shepherd_section(&mut buf, area, state, &Palette::catppuccin());
    (buf, hits)
}

fn agent(i: usize) -> ShepherdAgentRow {
    ShepherdAgentRow {
        id: format!("agent-{i}-{}", "x".repeat(40)),
        role: format!("rol-日本語-{i}"),
        vendor: "claude".into(),
        model: "m".into(),
        state: "working".into(),
        needs_you: i % 7 == 0,
        notice: "avisó\nvarias líneas\n日本語".into(),
        pane: "p1".into(),
        task_text: "tarea\nmultilínea".repeat(5),
        ..Default::default()
    }
}

fn task(i: u32) -> ShepherdTaskRow {
    ShepherdTaskRow {
        id: i,
        title: format!("Tarea larga 日本語 {}", "y".repeat(80)),
        state: "open".into(),
        description: "línea1\nlínea2 ".repeat(30),
        criteria: vec!["- [ ] criterio muy largo ".repeat(10)],
        attachments: vec!["src/".to_string() + &"a/".repeat(40)],
        ..Default::default()
    }
}

fn run_row(i: u32) -> ShepherdRunRow {
    ShepherdRunRow {
        task_id: i,
        task_title: format!("Run 日本語 {}", "z".repeat(60)),
        status: crate::shepherd::runs::RunStatus::Running,
        current_stage: "impl".into(),
        has_saved_plan: false,
        pending_gate: None,
        active_agents: vec!["a".into()],
        subtasks_progress: (1, 3),
    }
}

fn git_row(i: usize) -> ShepherdGitRow {
    ShepherdGitRow {
        path: format!("/tmp/wt/{i}/{}", "d/".repeat(30)),
        branch: format!("shep/{i}-{}", "r".repeat(50)),
        is_clean: i % 2 == 0,
        dirty_count: i,
        ..Default::default()
    }
}

fn big_state(n: usize) -> ShepherdSectionState {
    let mut s = ShepherdSectionState::default();
    s.agents = ShepherdAgentsSectionState::new((0..n).map(agent).collect());
    s.tasks.tasks = (0..n as u32).map(task).collect();
    s.runs.runs = (0..n as u32).map(run_row).collect();
    s.git = ShepherdGitSectionState::new((0..n).map(git_row).collect());
    s
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn tab_bar_shows_and_clicks_all_seven_tabs_at_every_size() {
    for (w, h) in SIZES {
        let mut state = ShepherdSectionState::default();
        let (_, hits) = render(&mut state, w, h);
        assert_eq!(hits.tabs.len(), 7, "{w}x{h}: tabs must all fit");
        let close = hits.close_button.expect("close button");
        for &(rect, _) in &hits.tabs {
            assert!(rect.right() <= close.x, "{w}x{h}: tab overlaps close");
        }
        for &(rect, tab) in &hits.tabs.clone() {
            let mut st = ShepherdSectionState::default();
            let out = handle_shepherd_section_mouse(&mut st, &hits, click(rect));
            assert_eq!(out, ShepherdSectionOutcome::Handled);
            assert_eq!(st.active_tab, tab, "{w}x{h}");
        }
        let out = handle_shepherd_section_mouse(&mut state, &hits, click(close));
        assert_eq!(out, ShepherdSectionOutcome::Close);
    }
}

#[test]
fn too_small_terminal_explains_instead_of_blank() {
    let mut state = ShepherdSectionState::default();
    let (buf, hits) = render(&mut state, 24, 6);
    assert!(hits.tabs.is_empty());
    let text: String = (0..6)
        .flat_map(|y| (0..24).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect();
    assert!(text.contains("Enlarge"), "{text}");
    // never panics on absurd sizes either
    for (w, h) in [(0, 0), (1, 1), (5, 2), (29, 100), (200, 7)] {
        let mut st = ShepherdSectionState::default();
        render(&mut st, w, h);
    }
}

#[test]
fn list_rows_action_button_and_wheel_work_on_every_list_tab() {
    for (w, h) in [(200, 50), (120, 40), (80, 24)] {
        for tab in [
            ShepherdSectionTab::Agents,
            ShepherdSectionTab::Tasks,
            ShepherdSectionTab::Runs,
            ShepherdSectionTab::Git,
        ] {
            let mut state = big_state(30);
            state.select_tab(tab);
            let (_, hits) = render(&mut state, w, h);
            let list = match tab {
                ShepherdSectionTab::Agents => &hits.agents_hits.as_ref().unwrap().list_hits,
                ShepherdSectionTab::Tasks => &hits.tasks_hits.as_ref().unwrap().list_hits,
                ShepherdSectionTab::Runs => &hits.runs_hits.as_ref().unwrap().list_hits,
                _ => &hits.git_hits.as_ref().unwrap().list_hits,
            };
            let (row_rect, idx) = list.rows[2];
            assert_eq!(idx, 2);
            assert!(list.action_button.is_some(), "{tab:?} {w}x{h}");
            // click a row selects it
            handle_shepherd_section_mouse(&mut state, &hits, click(row_rect));
            let selected = match tab {
                ShepherdSectionTab::Agents => state.agents.selected,
                ShepherdSectionTab::Tasks => state.tasks.selected,
                ShepherdSectionTab::Runs => state.runs.selected,
                _ => state.git.selected,
            };
            assert_eq!(selected, 2, "{tab:?} {w}x{h}");
            // wheel down moves, wheel up moves back
            handle_shepherd_section_mouse(
                &mut state,
                &hits,
                ev(MouseEventKind::ScrollDown, row_rect),
            );
            handle_shepherd_section_mouse(
                &mut state,
                &hits,
                ev(MouseEventKind::ScrollUp, row_rect),
            );
            let after = match tab {
                ShepherdSectionTab::Agents => state.agents.selected,
                ShepherdSectionTab::Tasks => state.tasks.selected,
                ShepherdSectionTab::Runs => state.runs.selected,
                _ => state.git.selected,
            };
            assert_eq!(after, 2, "{tab:?} wheel round trip");
        }
    }
}

#[test]
fn new_buttons_open_inline_forms_and_cancel_closes_them_even_in_compact() {
    for (w, h) in [(120, 40), (80, 24)] {
        for tab in [
            ShepherdSectionTab::Agents,
            ShepherdSectionTab::Tasks,
            ShepherdSectionTab::Git,
            ShepherdSectionTab::Patterns,
            ShepherdSectionTab::Skills,
        ] {
            let mut state = big_state(5);
            state.select_tab(tab);
            let (_, hits) = render(&mut state, w, h);
            let btn = match tab {
                ShepherdSectionTab::Agents => {
                    hits.agents_hits.as_ref().unwrap().list_hits.action_button
                }
                ShepherdSectionTab::Tasks => {
                    hits.tasks_hits.as_ref().unwrap().list_hits.action_button
                }
                ShepherdSectionTab::Git => hits.git_hits.as_ref().unwrap().list_hits.action_button,
                ShepherdSectionTab::Patterns => {
                    hits.patterns_hits.as_ref().unwrap().list_hits.action_button
                }
                _ => hits.skills_hits.as_ref().unwrap().list_hits.action_button,
            }
            .unwrap_or_else(|| panic!("{tab:?} {w}x{h}: no action button"));
            handle_shepherd_section_mouse(&mut state, &hits, click(btn));
            assert!(
                state.has_active_sub_modal_or_stripe(),
                "{tab:?} {w}x{h}: + button opened nothing"
            );
            // the open form must be visible: it draws its own hit rects (cancel button)
            let (_, hits2) = render(&mut state, w, h);
            let cancel = match tab {
                ShepherdSectionTab::Agents => hits2
                    .agents_hits
                    .as_ref()
                    .and_then(|h| h.form_hits.as_ref())
                    .and_then(|f| f.cancel_button),
                ShepherdSectionTab::Tasks => hits2
                    .tasks_hits
                    .as_ref()
                    .and_then(|h| h.form_hits.as_ref())
                    .and_then(|f| f.cancel_button),
                ShepherdSectionTab::Git => hits2
                    .git_hits
                    .as_ref()
                    .and_then(|h| h.form_hits.as_ref())
                    .and_then(|f| f.cancel_button),
                _ => None,
            };
            if let Some(c) = cancel {
                handle_shepherd_section_mouse(&mut state, &hits2, click(c));
                assert!(
                    !state.has_active_sub_modal_or_stripe(),
                    "{tab:?} {w}x{h}: cancel"
                );
            } else if !matches!(
                tab,
                ShepherdSectionTab::Patterns | ShepherdSectionTab::Skills
            ) {
                panic!("{tab:?} {w}x{h}: form not rendered");
            }
        }
    }
}

#[test]
fn empty_states_are_visible_and_their_buttons_are_clickable_in_compact_widths() {
    for (w, h) in [(120, 40), (80, 24), (55, 20)] {
        for tab in [
            ShepherdSectionTab::Tasks,
            ShepherdSectionTab::Runs,
            ShepherdSectionTab::Git,
        ] {
            let mut state = ShepherdSectionState::default();
            state.select_tab(tab);
            let (_, hits) = render(&mut state, w, h);
            let empty = match tab {
                ShepherdSectionTab::Tasks => hits.tasks_hits.unwrap().empty_hits,
                ShepherdSectionTab::Runs => hits.runs_hits.unwrap().empty_hits,
                _ => hits.git_hits.unwrap().empty_hits,
            };
            if tab != ShepherdSectionTab::Git {
                let e = empty.unwrap_or_else(|| panic!("{tab:?} {w}x{h}: empty state hidden"));
                assert!(!e.actions.is_empty(), "{tab:?} {w}x{h}: no CTA");
            }
        }
    }
}

fn events_with_run() -> ShepherdSectionState {
    let mut state = ShepherdSectionState::default();
    state.select_tab(ShepherdSectionTab::Events);
    state.events.active_subtab = events::ShepherdEventsSubTab::PatternRuns;
    state.events.runs.push(PatternRunSummary {
        id: "run-1".into(),
        pattern: "market".into(),
        kind: "market".into(),
        state: PatternRunState::Running,
        agents_count: 3,
        live_agents: 3,
        topics_count: 1,
        created_at: "t".into(),
        updated_at: "t".into(),
    });
    state.events.selected_run_detail = Some(PatternRun {
        id: "run-1".into(),
        pattern: "market".into(),
        kind: "market".into(),
        task: "t".into(),
        worktree: None,
        topics: vec![],
        agents: vec![],
        state: PatternRunState::Running,
        created_at: "t".into(),
        updated_at: "t".into(),
        finished_at: None,
        error: None,
        quiesced: false,
        stalled_reason: None,
    });
    state
}

#[test]
fn events_run_controls_pause_resume_stop_with_confirmation_stripe() {
    let mut state = events_with_run();
    let (_, hits) = render(&mut state, 160, 40);
    let ev_hits = hits.events_hits.as_ref().unwrap();
    let pause = ev_hits.pause_run_button.expect("pause rect");
    let stop = ev_hits.stop_run_button.expect("stop rect");
    // pause -> Action(PauseRun)
    let out = handle_shepherd_section_mouse(&mut state, &hits, click(pause));
    assert!(
        matches!(
            out,
            ShepherdSectionOutcome::Action(ShepherdSectionAction::Events(
                events::ShepherdEventsAction::PauseRun { .. }
            ))
        ),
        "{out:?}"
    );
    // stop -> confirmation stripe, nothing executed yet
    let out = handle_shepherd_section_mouse(&mut state, &hits, click(stop));
    assert_eq!(out, ShepherdSectionOutcome::Handled);
    assert_eq!(state.events.confirm_stop_run.as_deref(), Some("run-1"));
    // cancel button closes the stripe
    let (_, hits2) = render(&mut state, 160, 40);
    let sh = hits2
        .events_hits
        .as_ref()
        .unwrap()
        .confirm_stripe_hits
        .as_ref()
        .unwrap();
    let cancel = sh.cancel_button.expect("cancel");
    let confirm = sh.confirm_button.expect("confirm");
    handle_shepherd_section_mouse(&mut state, &hits2, click(cancel));
    assert!(state.events.confirm_stop_run.is_none());
    // stop + confirm -> Action(StopRun)
    handle_shepherd_section_mouse(&mut state, &hits, click(stop));
    let out = handle_shepherd_section_mouse(&mut state, &hits2, click(confirm));
    assert!(
        matches!(
            out,
            ShepherdSectionOutcome::Action(ShepherdSectionAction::Events(
                events::ShepherdEventsAction::StopRun { .. }
            ))
        ),
        "{out:?}"
    );
}

#[test]
fn events_topic_controls_pause_replay_filter_and_subtabs() {
    let mut state = ShepherdSectionState::default();
    state.select_tab(ShepherdSectionTab::Events);
    state.events.topics.push(ShepherdTopicRow {
        name: "t1".into(),
        partitions: 1,
        messages_count: 0,
        groups_count: 0,
    });
    for (w, h) in [(160, 40), (120, 30)] {
        let (_, hits) = render(&mut state, w, h);
        let e = hits.events_hits.as_ref().unwrap();
        // pause view toggles
        let was = state.events.view_paused;
        handle_shepherd_section_mouse(&mut state, &hits, click(e.pause_view_button.unwrap()));
        assert_ne!(state.events.view_paused, was, "{w}x{h}");
        handle_shepherd_section_mouse(&mut state, &hits, click(e.pause_view_button.unwrap()));
        // replay offset input -> edit mode
        handle_shepherd_section_mouse(&mut state, &hits, click(e.replay_input_rect.unwrap()));
        assert!(state.events.editing_replay_offset);
        state.events.editing_replay_offset = false;
        // replay button -> action or handled, never ignored
        let out = handle_shepherd_section_mouse(&mut state, &hits, click(e.replay_button.unwrap()));
        assert_ne!(out, ShepherdSectionOutcome::Ignored);
        // filter input -> edit mode
        handle_shepherd_section_mouse(&mut state, &hits, click(e.filter_input_rect.unwrap()));
        assert!(state.events.editing_agent_filter);
        state.events.editing_agent_filter = false;
        // sub-tab bar
        let (rect, sub) = e.subtab_rects[1];
        handle_shepherd_section_mouse(&mut state, &hits, click(rect));
        assert_eq!(state.events.active_subtab, sub);
        state.events.active_subtab = events::ShepherdEventsSubTab::Topics;
    }
}

#[test]
fn events_wheel_over_messages_scrolls_the_stream_and_tail_follows() {
    let mut state = ShepherdSectionState::default();
    state.select_tab(ShepherdSectionTab::Events);
    state.events.topics.push(ShepherdTopicRow {
        name: "t1".into(),
        partitions: 1,
        messages_count: 600,
        groups_count: 0,
    });
    state.events.messages = (0..600u64)
        .map(|i| BusMessage {
            id: i as i64,
            ts: "t".into(),
            from: "planner".into(),
            to: "".into(),
            kind: "msg".into(),
            topic: "t1".into(),
            partition: 0,
            offset: i,
            key: "k".into(),
            content: format!("message {i} 日本語 with\nline break"),
        })
        .collect();
    let (buf, hits) = render(&mut state, 160, 40);
    let screen: String = (0..40)
        .flat_map(|y| (0..160).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect();
    assert!(
        screen.contains("message 599"),
        "live tail must show the newest message"
    );
    assert!(
        !screen.contains("message 0 "),
        "oldest message must not be pinned on screen"
    );
    let r = hits.events_hits.as_ref().unwrap().messages_rect.unwrap();
    handle_shepherd_section_mouse(&mut state, &hits, ev(MouseEventKind::ScrollUp, r));
    assert_eq!(state.events.message_scroll, 3);
    handle_shepherd_section_mouse(&mut state, &hits, ev(MouseEventKind::ScrollDown, r));
    assert_eq!(state.events.message_scroll, 0);
    // PageUp far beyond the end stays bounded and one PageDown-run returns to the tail
    for _ in 0..500 {
        state.events.handle_key(key(KeyCode::PageUp));
    }
    assert!(state.events.message_scroll <= 600);
    for _ in 0..200 {
        state.events.handle_key(key(KeyCode::PageDown));
    }
    assert_eq!(state.events.message_scroll, 0);
    render(&mut state, 160, 40);
}

#[test]
fn events_stream_render_with_hundreds_of_messages_is_cheap() {
    let mut state = ShepherdSectionState::default();
    state.select_tab(ShepherdSectionTab::Events);
    state.events.topics.push(ShepherdTopicRow {
        name: "t1".into(),
        partitions: 1,
        messages_count: 500,
        groups_count: 0,
    });
    state.events.messages = (0..500u64)
        .map(|i| BusMessage {
            id: i as i64,
            ts: "t".into(),
            from: "a".into(),
            to: "".into(),
            kind: "msg".into(),
            topic: "t1".into(),
            partition: 0,
            offset: i,
            key: "k".into(),
            content: "x".repeat(300),
        })
        .collect();
    let start = std::time::Instant::now();
    for _ in 0..50 {
        render(&mut state, 200, 50);
    }
    let per_frame = start.elapsed() / 50;
    // deterministic bound is the O(visible rows) draw; this only guards against O(n^2) regressions
    assert!(
        per_frame < std::time::Duration::from_millis(250),
        "{per_frame:?}"
    );
}

#[test]
fn extreme_data_never_panics_and_selection_stays_visible() {
    for (w, h) in SIZES {
        for tab in ShepherdSectionTab::ALL {
            let mut state = big_state(150);
            state.select_tab(tab);
            // walk the selection to the end and back with the keyboard
            for _ in 0..170 {
                state.handle_key(&key(KeyCode::Down));
                render(&mut state, w, h);
            }
            for _ in 0..170 {
                state.handle_key(&key(KeyCode::Up));
            }
            let (_, hits) = render(&mut state, w, h);
            // the selected row is always among the drawn rows when a list is drawn
            let (rows, sel) = match tab {
                ShepherdSectionTab::Agents => (
                    hits.agents_hits.as_ref().map(|h| h.list_hits.rows.clone()),
                    state.agents.selected,
                ),
                ShepherdSectionTab::Tasks => (
                    hits.tasks_hits.as_ref().map(|h| h.list_hits.rows.clone()),
                    state.tasks.selected,
                ),
                ShepherdSectionTab::Runs => (
                    hits.runs_hits.as_ref().map(|h| h.list_hits.rows.clone()),
                    state.runs.selected,
                ),
                ShepherdSectionTab::Git => (
                    hits.git_hits.as_ref().map(|h| h.list_hits.rows.clone()),
                    state.git.selected,
                ),
                _ => (None, 0),
            };
            if let Some(rows) = rows {
                assert!(
                    rows.iter().all(|&(r, _)| r.bottom() <= h),
                    "{tab:?} {w}x{h}"
                );
                if !rows.is_empty() {
                    assert!(
                        rows.iter().any(|&(_, i)| i == sel),
                        "{tab:?} {w}x{h}: selection {sel} off-screen"
                    );
                }
            }
        }
    }
}
