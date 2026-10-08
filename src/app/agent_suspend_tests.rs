//! Agent identity across foreground changes: real exits, relaunches, and
//! an agent suspended behind the shell.

use std::time::{Duration, Instant};

use crate::agent_resume::AgentSessionRef;
use crate::app::state::AppState;
use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;
use crate::terminal::TerminalId;

fn adversarial_pane() -> (AppState, PaneId, TerminalId) {
    let app = AppState::test_with_adversarial_identity_state();
    let ws = &app.workspaces[0];
    let pane_id = ws.tabs[ws.active_tab].root_pane;
    let terminal_id = ws.panes[&pane_id].attached_terminal_id.clone();
    (app, pane_id, terminal_id)
}

fn pi_session() -> AgentSessionRef {
    AgentSessionRef::path(
        std::env::current_dir()
            .expect("cwd")
            .join("takeover-pi.jsonl")
            .display()
            .to_string(),
    )
    .expect("pi session path")
}

fn claude_session() -> AgentSessionRef {
    AgentSessionRef::id("2f0c0c2e-1647-4a4b-9a3e-0000000c1a0d").expect("claude session id")
}

fn detect(app: &mut AppState, pane_id: PaneId, agent: Agent, at: Instant) {
    app.handle_app_event(AppEvent::AgentProcessDetected {
        pane_id,
        agent,
        observed_at: at,
    });
}

fn detection_state(
    app: &mut AppState,
    pane_id: PaneId,
    agent: Option<Agent>,
    process_exited: bool,
    at: Instant,
) {
    app.handle_app_event(AppEvent::StateChanged {
        pane_id,
        agent,
        state: AgentState::Idle,
        visible_blocker: false,
        visible_working: false,
        process_exited,
        observed_at: at,
    });
}

/// The two events the detection loop publishes when the pane shell takes the
/// foreground back from an agent whose process is gone.
fn real_exit(app: &mut AppState, pane_id: PaneId, agent: Agent, at: Instant) {
    detection_state(app, pane_id, Some(agent), true, at);
    detection_state(app, pane_id, None, false, at + Duration::from_millis(1));
}

fn pi_report(app: &mut AppState, pane_id: PaneId, state: AgentState, seq: u64) {
    app.handle_app_event(AppEvent::HookStateReported {
        pane_id,
        source: "herdr:pi".into(),
        agent_label: "pi".into(),
        state,
        message: None,
        seq: Some(seq),
        session_ref: Some(pi_session()),
    });
}

fn pi_session_report(
    app: &mut AppState,
    pane_id: PaneId,
    session_start_source: Option<&str>,
    seq: u64,
) {
    app.handle_app_event(AppEvent::AgentSessionReported {
        pane_id,
        source: "herdr:pi".into(),
        agent_label: "pi".into(),
        seq: Some(seq),
        session_ref: Some(pi_session()),
        session_start_source: session_start_source.map(str::to_string),
    });
}

fn claude_session_report(app: &mut AppState, pane_id: PaneId, seq: u64) {
    app.handle_app_event(AppEvent::AgentSessionReported {
        pane_id,
        source: "herdr:claude".into(),
        agent_label: "claude".into(),
        seq: Some(seq),
        session_ref: Some(claude_session()),
        session_start_source: Some("startup".into()),
    });
}

fn session_value(app: &AppState, terminal_id: &TerminalId) -> Option<String> {
    let terminal = &app.terminals[terminal_id];
    terminal
        .hook_authority
        .as_ref()
        .and_then(|authority| authority.session_ref.as_ref())
        .or_else(|| {
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref)
        })
        .map(|session_ref| session_ref.value.clone())
}

fn name_agent(app: &mut AppState, terminal_id: &TerminalId, name: &str) {
    app.terminals
        .get_mut(terminal_id)
        .expect("terminal")
        .set_agent_name(name.into());
}

fn pi_with_live_authority(start: Instant) -> (AppState, PaneId, TerminalId) {
    let (mut app, pane_id, terminal_id) = adversarial_pane();
    detect(&mut app, pane_id, Agent::Pi, start);
    pi_session_report(&mut app, pane_id, Some("startup"), 99);
    pi_report(&mut app, pane_id, AgentState::Working, 100);
    name_agent(&mut app, &terminal_id, "worker");
    assert!(app.terminals[&terminal_id].full_lifecycle_hook_authority_active());
    assert_eq!(session_value(&app, &terminal_id), Some(pi_session().value));
    (app, pane_id, terminal_id)
}

fn claude_with_session(start: Instant) -> (AppState, PaneId, TerminalId) {
    let (mut app, pane_id, terminal_id) = adversarial_pane();
    detect(&mut app, pane_id, Agent::Claude, start);
    claude_session_report(&mut app, pane_id, 1);
    name_agent(&mut app, &terminal_id, "c1");
    assert_eq!(
        session_value(&app, &terminal_id),
        Some(claude_session().value)
    );
    (app, pane_id, terminal_id)
}

#[test]
fn real_exit_clears_claude_session_and_name() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = claude_with_session(start);

    real_exit(
        &mut app,
        pane_id,
        Agent::Claude,
        start + Duration::from_secs(1),
    );

    let terminal = &app.terminals[&terminal_id];
    assert_eq!(session_value(&app, &terminal_id), None);
    assert_eq!(terminal.agent_name, None);
    assert_eq!(terminal.effective_agent_label(), None);
    app.assert_invariants_for_test();
}

#[test]
fn real_exit_clears_pi_session_name_and_authority() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(start);

    real_exit(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(1));

    let terminal = &app.terminals[&terminal_id];
    assert_eq!(session_value(&app, &terminal_id), None);
    assert!(terminal.hook_authority.is_none());
    assert_eq!(terminal.agent_name, None);
    assert_eq!(terminal.effective_agent_label(), None);
    app.assert_invariants_for_test();
}

fn captured_agent_session(
    app: &AppState,
    pane_id: PaneId,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    let snapshot = crate::persist::capture(
        &app.workspaces,
        &app.terminals,
        &crate::terminal::TerminalRuntimeRegistry::new(),
        app.active,
        app.selected,
    );
    // Exercise the on-disk shape, not just the in-memory fallback accessor.
    let saved: crate::persist::SessionSnapshot =
        serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
    saved
        .workspaces
        .into_iter()
        .flat_map(|ws| ws.tabs)
        .find_map(|mut tab| tab.panes.remove(&pane_id.raw()))
        .and_then(|pane| pane.agent_session)
        .map(|session| crate::agent_resume::PersistedAgentSession {
            source: session.source,
            agent: session.agent,
            session_ref: AgentSessionRef {
                kind: session.kind,
                value: session.value,
            },
        })
}

#[test]
fn interrupted_native_sessions_survive_exit_and_autosave_without_live_authority() {
    for (agent, label) in [(Agent::Pi, "pi"), (Agent::Omp, "omp")] {
        for with_hook in [false, true] {
            let start = Instant::now();
            let (mut app, pane_id, terminal_id) = adversarial_pane();
            let source = format!("herdr:{label}");
            let session = pi_session();
            detect(&mut app, pane_id, agent, start);
            app.handle_app_event(AppEvent::AgentSessionReported {
                pane_id,
                source: source.clone(),
                agent_label: label.into(),
                seq: Some(1),
                session_ref: Some(session.clone()),
                session_start_source: Some("startup".into()),
            });
            if with_hook {
                app.handle_app_event(AppEvent::HookStateReported {
                    pane_id,
                    source: source.clone(),
                    agent_label: label.into(),
                    state: AgentState::Working,
                    message: None,
                    seq: Some(2),
                    session_ref: Some(session.clone()),
                });
            }
            name_agent(&mut app, &terminal_id, "worker");
            assert!(app
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .report_agent_interruption(&source, label, session.clone(), 3));
            real_exit(&mut app, pane_id, agent, start + Duration::from_secs(1));
            // A duplicate exit (including the pane death checkpoint path) and
            // later shell detections must not erase the retained reference.
            real_exit(&mut app, pane_id, agent, start + Duration::from_secs(2));
            let terminal = &app.terminals[&terminal_id];
            assert_eq!(terminal.effective_agent_label(), None);
            assert!(terminal.hook_authority.is_none());
            assert!(terminal.persisted_agent_session.is_none());
            assert!(terminal.agent_name.is_none());
            let saved = captured_agent_session(&app, pane_id).expect("recoverable session");
            assert_eq!(saved.session_ref, session);
            let plan = crate::agent_resume::plan(&saved.source, &saved.agent, &session).unwrap();
            let expected = if label == "pi" {
                vec![
                    "pi".to_string(),
                    "--session".to_string(),
                    session.value.clone(),
                ]
            } else {
                vec!["omp".to_string(), format!("--resume={}", session.value)]
            };
            assert_eq!(plan.argv, expected);
            app.assert_invariants_for_test();
        }
    }
}

#[test]
fn interruption_rejects_stale_foreign_and_unbound_reports() {
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(Instant::now());
    let terminal = app.terminals.get_mut(&terminal_id).unwrap();
    assert!(!terminal.report_agent_interruption("herdr:pi", "pi", pi_session(), 99));
    assert!(!terminal.report_agent_interruption("custom:pi", "pi", pi_session(), 101));
    assert!(!terminal.report_agent_interruption("herdr:omp", "omp", pi_session(), 101));
    assert!(!terminal.report_agent_interruption(
        "herdr:pi",
        "pi",
        AgentSessionRef::path(
            std::env::current_dir()
                .unwrap()
                .join("foreign.jsonl")
                .display()
                .to_string(),
        )
        .unwrap(),
        101
    ));
    assert!(terminal.interrupted_agent_session().is_none());
    real_exit(&mut app, pane_id, Agent::Pi, Instant::now());
    assert!(!app
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .report_agent_interruption("herdr:pi", "pi", pi_session(), 101));
    assert!(captured_agent_session(&app, pane_id).is_none());
    app.assert_invariants_for_test();
}

#[test]
fn interrupted_session_switch_requires_explicit_selection_before_recovery() {
    for (agent, label) in [(Agent::Pi, "pi"), (Agent::Omp, "omp")] {
        let (mut app, pane_id, terminal_id) = adversarial_pane();
        let source = format!("herdr:{label}");
        let next_session = AgentSessionRef::path(
            std::env::current_dir()
                .unwrap()
                .join("switched-session.jsonl")
                .display()
                .to_string(),
        )
        .unwrap();
        detect(&mut app, pane_id, agent, Instant::now());
        app.handle_app_event(AppEvent::AgentSessionReported {
            pane_id,
            source: source.clone(),
            agent_label: label.into(),
            seq: Some(99),
            session_ref: Some(pi_session()),
            session_start_source: Some("startup".into()),
        });
        app.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: source.clone(),
            agent_label: label.into(),
            state: AgentState::Working,
            message: None,
            seq: Some(100),
            session_ref: Some(pi_session()),
        });
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                source.clone(),
                label.into(),
                Some(next_session.clone()),
                Some(101),
                None,
            )
            .is_none());
        assert!(!terminal.report_agent_interruption(&source, label, next_session.clone(), 102));
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                source.clone(),
                label.into(),
                Some(next_session.clone()),
                Some(103),
                Some("resume".into()),
            )
            .is_some());
        assert!(terminal.report_agent_interruption(&source, label, next_session.clone(), 104));
        real_exit(&mut app, pane_id, agent, Instant::now());
        assert_eq!(
            captured_agent_session(&app, pane_id).unwrap().session_ref,
            next_session
        );
        app.assert_invariants_for_test();
    }
}

#[test]
fn fresh_process_retires_interrupted_recovery_and_marks_session_dirty() {
    for agent in [Agent::Pi, Agent::Omp, Agent::Codex] {
        let start = Instant::now();
        let (mut app, pane_id, terminal_id) = pi_with_live_authority(start);
        assert!(app
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .report_agent_interruption("herdr:pi", "pi", pi_session(), 101));
        real_exit(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(1));
        app.session_dirty = false;
        detect(&mut app, pane_id, agent, start + Duration::from_secs(2));
        assert!(app.session_dirty);
        assert!(app.terminals[&terminal_id]
            .interrupted_agent_session()
            .is_none());
        assert!(captured_agent_session(&app, pane_id).is_none());
        pi_report(&mut app, pane_id, AgentState::Working, 102);
        assert!(captured_agent_session(&app, pane_id).is_none());
        app.assert_invariants_for_test();
    }
}

#[test]
fn explicit_release_discards_only_matching_interrupted_recovery() {
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(Instant::now());
    assert!(app
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .report_agent_interruption("herdr:pi", "pi", pi_session(), 101));
    real_exit(&mut app, pane_id, Agent::Pi, Instant::now());
    let terminal = app.terminals.get_mut(&terminal_id).unwrap();
    assert!(terminal
        .release_agent_with_mutation("foreign", "pi", Some(102))
        .is_none());
    assert!(terminal.interrupted_agent_session().is_some());
    assert!(terminal
        .release_agent_with_mutation("herdr:pi", "pi", Some(102))
        .is_some());
    assert!(captured_agent_session(&app, pane_id).is_none());
    app.assert_invariants_for_test();
}

#[test]
fn stale_process_observation_cannot_discard_interrupted_recovery() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(start);
    assert!(app
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .report_agent_interruption("herdr:pi", "pi", pi_session(), 101));
    real_exit(&mut app, pane_id, Agent::Pi, Instant::now());
    detect(&mut app, pane_id, Agent::Pi, start);
    assert!(app.terminals[&terminal_id]
        .interrupted_agent_session()
        .is_some());
    assert!(captured_agent_session(&app, pane_id).is_some());
    app.assert_invariants_for_test();
}

#[test]
fn buffered_resume_retires_interrupted_recovery_before_the_next_clean_exit() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(start);
    assert!(app
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .report_agent_interruption("herdr:pi", "pi", pi_session(), 101));
    real_exit(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(1));
    // Native startup can report its explicit selection before process detection.
    pi_session_report(&mut app, pane_id, Some("resume"), 102);
    detect(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(2));
    pi_report(&mut app, pane_id, AgentState::Idle, 103);
    assert!(app.terminals[&terminal_id]
        .interrupted_agent_session()
        .is_none());
    assert!(captured_agent_session(&app, pane_id).is_some());
    real_exit(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(3));
    assert!(captured_agent_session(&app, pane_id).is_none());
    app.assert_invariants_for_test();
}

#[test]
fn same_session_reports_preserve_interruption_until_a_new_session_starts() {
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(Instant::now());
    assert!(app
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .report_agent_interruption("herdr:pi", "pi", pi_session(), 101));
    pi_report(&mut app, pane_id, AgentState::Idle, 102);
    pi_session_report(&mut app, pane_id, None, 103);
    assert!(app.terminals[&terminal_id]
        .interrupted_agent_session()
        .is_some());
    pi_session_report(&mut app, pane_id, Some("resume"), 104);
    assert!(app.terminals[&terminal_id]
        .interrupted_agent_session()
        .is_none());
    real_exit(&mut app, pane_id, Agent::Pi, Instant::now());
    assert!(captured_agent_session(&app, pane_id).is_none());
    app.assert_invariants_for_test();
}

#[cfg(unix)]
#[test]
fn interrupted_recovery_handoff_does_not_revive_authority_or_hide_a_new_process() {
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(Instant::now());
    assert!(app
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .report_agent_interruption("herdr:pi", "pi", pi_session(), 101));
    real_exit(&mut app, pane_id, Agent::Pi, Instant::now());
    let encoded = serde_json::to_string(
        &app.terminals[&terminal_id]
            .handoff_interrupted_agent_session()
            .unwrap(),
    )
    .unwrap();
    let imported = serde_json::from_str(&encoded).unwrap();
    let queued_process_observation = Instant::now();
    let terminal = app.terminals.get_mut(&terminal_id).unwrap();
    terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
        source: "herdr:pi".into(),
        agent: "pi".into(),
        session_ref: pi_session(),
    });
    terminal.restore_interrupted_agent_session(imported);
    assert!(terminal.persisted_agent_session.is_none());
    assert!(terminal.effective_agent_label().is_none());
    pi_report(&mut app, pane_id, AgentState::Working, 102);
    assert!(app.terminals[&terminal_id].hook_authority.is_none());
    assert!(captured_agent_session(&app, pane_id).is_some());
    detect(&mut app, pane_id, Agent::Pi, queued_process_observation);
    assert!(app.terminals[&terminal_id]
        .interrupted_agent_session()
        .is_none());
    app.assert_invariants_for_test();
}

#[test]
fn exit_then_different_agent_relaunch_drops_previous_identity() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = claude_with_session(start);

    real_exit(
        &mut app,
        pane_id,
        Agent::Claude,
        start + Duration::from_secs(1),
    );
    detect(
        &mut app,
        pane_id,
        Agent::Codex,
        start + Duration::from_secs(2),
    );

    let terminal = &app.terminals[&terminal_id];
    assert_eq!(session_value(&app, &terminal_id), None);
    assert_eq!(terminal.agent_name, None);
    assert_eq!(terminal.effective_agent_label(), Some("codex"));
    app.assert_invariants_for_test();
}

#[test]
fn exited_pi_session_is_not_replayed_into_a_relaunched_pi() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(start);

    real_exit(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(1));
    detect(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(2));
    // A late report from the exited process still carries its old session.
    pi_report(&mut app, pane_id, AgentState::Working, 101);

    let terminal = &app.terminals[&terminal_id];
    assert!(!terminal.full_lifecycle_hook_authority_active());
    assert_eq!(session_value(&app, &terminal_id), None);
    app.assert_invariants_for_test();
}

#[test]
fn suspended_pi_keeps_session_name_and_authority_for_its_next_reports() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(start);

    // While Pi's job is stopped behind the shell, detection publishes nothing.
    // After `fg`, Pi re-announces its session on agent_start and reports state.
    pi_session_report(&mut app, pane_id, None, 101);
    pi_report(&mut app, pane_id, AgentState::Idle, 102);

    let terminal = &app.terminals[&terminal_id];
    assert!(terminal.full_lifecycle_hook_authority_active());
    assert_eq!(terminal.state, AgentState::Idle);
    assert_eq!(terminal.agent_name.as_deref(), Some("worker"));
    assert_eq!(session_value(&app, &terminal_id), Some(pi_session().value));
    app.assert_invariants_for_test();
}

/// The events detection publishes when a new process of the same agent kind
/// replaces the previous one without the shell in between.
fn same_kind_replacement(app: &mut AppState, pane_id: PaneId, agent: Agent, at: Instant) {
    detection_state(app, pane_id, Some(agent), true, at);
    detect(app, pane_id, agent, at + Duration::from_millis(1));
}

#[test]
fn same_kind_replacement_drops_the_previous_claude_session() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = claude_with_session(start);

    same_kind_replacement(
        &mut app,
        pane_id,
        Agent::Claude,
        start + Duration::from_secs(1),
    );

    assert_eq!(session_value(&app, &terminal_id), None);
    assert_eq!(
        app.terminals[&terminal_id].effective_agent_label(),
        Some("claude")
    );
    app.assert_invariants_for_test();
}

#[test]
fn same_kind_replacement_drops_the_previous_pi_session_and_authority() {
    let start = Instant::now();
    let (mut app, pane_id, terminal_id) = pi_with_live_authority(start);

    same_kind_replacement(&mut app, pane_id, Agent::Pi, start + Duration::from_secs(1));
    pi_report(&mut app, pane_id, AgentState::Working, 101);

    let terminal = &app.terminals[&terminal_id];
    assert!(!terminal.full_lifecycle_hook_authority_active());
    assert_eq!(session_value(&app, &terminal_id), None);
    app.assert_invariants_for_test();
}
