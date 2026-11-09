//! F8h: run quiescence ignores consumer lag; it needs idle agents + no publish for T seconds.
use super::patterns_run::PatternRunState;
use super::tests::{test_service, TestDir};
use super::*;
use crate::terminals::TerminalAgentStatus;
use std::time::Duration;

fn set_idle(service: &OrchestrationService, terminals: &crate::terminals::InMemoryTerminals) {
    let mut reg = service.load_registry().unwrap();
    for ag in &mut reg.agents {
        ag.state = "idle".into();
        let st = TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: ag.pane.clone(),
            tab: ag.tab.clone(),
            title: ag.label.clone(),
            seq: 1,
        };
        terminals.set_status(&ag.id, st.clone());
        terminals.set_status(&ag.pane, st);
    }
    service.save_registry(&reg).unwrap();
}

#[test]
fn run_quiesces_despite_unacked_lag_and_extra_group() {
    let tmp = TestDir::new("f8h-lag");
    let (service, terminals, clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = service
        .start_pattern_run(
            tmp.path(),
            Some("orchestrator-worker"),
            None,
            Some("t"),
            None,
            true,
        )
        .unwrap();
    let reg = service.load_registry().unwrap();
    let brain = reg.agents[0].id.clone();
    let worker = reg.agents[1].id.clone();
    let bus = service.bus().unwrap();
    // Real-agent shape: orchestrator's own summary stays unacked; worker joins `results`
    // (not in its consumes) in a group of its own and never acks.
    bus.subscribe(&worker, "results", None).unwrap();
    service
        .publish_topic("results", "summary", None, Some(0), &brain)
        .unwrap();
    set_idle(&service, &terminals);

    service.reconcile_once().unwrap(); // wakes the worker for the unread summary
    set_idle(&service, &terminals); // ...it processes it and goes idle without acking
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(
        r.state,
        PatternRunState::Done,
        "lag must not block quiescence"
    );
    assert!(r.quiesced);
}

#[test]
fn recent_publish_restarts_the_quiet_period_and_reopen_still_works() {
    let tmp = TestDir::new("f8h-publish");
    let (service, terminals, clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = service
        .start_pattern_run(
            tmp.path(),
            Some("orchestrator-worker"),
            None,
            Some("t"),
            None,
            true,
        )
        .unwrap();
    set_idle(&service, &terminals);
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(20));
    service
        .publish_topic("results", "late", None, Some(0), "human")
        .unwrap();
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(20)); // 40s since first idle, only 20s since publish
    service.reconcile_once().unwrap();
    assert_eq!(
        service.get_pattern_run(tmp.path(), &run.id).unwrap().state,
        PatternRunState::Running
    );
    clock.advance(Duration::from_secs(11));
    service.reconcile_once().unwrap();
    assert_eq!(
        service.get_pattern_run(tmp.path(), &run.id).unwrap().state,
        PatternRunState::Done
    );
    service
        .publish_topic("results", "again", None, Some(0), "human")
        .unwrap();
    assert_eq!(
        service.get_pattern_run(tmp.path(), &run.id).unwrap().state,
        PatternRunState::Running
    );
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    assert_eq!(
        service.get_pattern_run(tmp.path(), &run.id).unwrap().state,
        PatternRunState::Done
    );
}
