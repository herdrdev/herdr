//! F8g: reopen quiesced runs on publish; no phantom memberless consumer groups.
use super::patterns_run::PatternRunState;
use super::tests::{test_service, TestDir};
use super::*;
use crate::terminals::TerminalAgentStatus;
use std::time::Duration;

fn quiesced_run(
    service: &OrchestrationService,
    terminals: &crate::terminals::InMemoryTerminals,
    clock: &ManualClock,
    dir: &Path,
) -> patterns_run::PatternRun {
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = service
        .start_pattern_run(
            dir,
            Some("orchestrator-worker"),
            None,
            Some("t"),
            None,
            true,
        )
        .unwrap();
    // F12: a run where nothing was ever published is `stalled`, not `done`.
    service
        .publish_topic("orders", "go", None, Some(0), "human")
        .unwrap();
    set_idle(service, terminals);
    service.reconcile_once().unwrap(); // wakes the unread consumers
    set_idle(service, terminals);
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(dir, &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Done);
    run
}

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

fn ack_all(s: &OrchestrationService) {
    let b = s.bus().unwrap();
    for g in b.consumer_groups(None).unwrap() {
        for m in g.members {
            let _ = b.ack(&m, &g.topic, Some(0), Some(10));
        }
    }
}

#[test]
fn publish_reopens_quiesced_run_even_if_agents_stay_idle_and_acked() {
    let tmp = TestDir::new("f8g-reopen");
    let (service, terminals, clock) = test_service(tmp.path());
    let run = quiesced_run(&service, &terminals, &clock, tmp.path());

    // Foreign publisher; no reconcile in between (agents may be idle again before one runs).
    service
        .publish_topic("results", "more", None, Some(0), "human")
        .unwrap();
    set_idle(&service, &terminals);
    ack_all(&service); // fast agent already read + acked it
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Running);
    assert!(!r.quiesced && r.finished_at.is_none());

    // The reopened run calms down and quiesces again.
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    ack_all(&service);
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Done);
}

#[test]
fn publish_does_not_reopen_stopped_or_unrelated_topic_runs() {
    let tmp = TestDir::new("f8g-no-reopen");
    let (service, terminals, clock) = test_service(tmp.path());
    let run = quiesced_run(&service, &terminals, &clock, tmp.path());
    service
        .publish_topic("unrelated", "x", None, Some(0), "human")
        .unwrap();
    assert_eq!(
        service.get_pattern_run(tmp.path(), &run.id).unwrap().state,
        PatternRunState::Done
    );

    let tmp2 = TestDir::new("f8g-stopped");
    let (s2, t2, c2) = test_service(tmp2.path());
    let run2 = quiesced_run(&s2, &t2, &c2, tmp2.path());
    s2.stop_pattern_run(tmp2.path(), &run2.id, true).unwrap();
    s2.publish_topic("results", "x", None, Some(0), "human")
        .unwrap();
    assert_eq!(
        s2.get_pattern_run(tmp2.path(), &run2.id).unwrap().state,
        PatternRunState::Stopped
    );
}

#[test]
fn moving_a_consumer_to_another_group_drops_the_never_used_empty_group() {
    let tmp = TestDir::new("f8g-ghost");
    let (service, _t, _c) = test_service(tmp.path());
    let bus = service.bus().unwrap();
    bus.topic_create("results", 1).unwrap();
    bus.subscribe("brain-1", "results", Some("brain")).unwrap(); // topology
    bus.subscribe("brain-1", "results", None).unwrap(); // agent's own subscribe
    bus.publish("results", "m", None, Some(0), "x").unwrap();
    let groups = service.consumer_groups(Some("results")).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].group, "brain-1");

    // A group that already committed offsets is real state and survives.
    bus.subscribe("a", "results", Some("g")).unwrap();
    bus.ack("a", "results", Some(0), Some(0)).unwrap();
    bus.subscribe("a", "results", Some("h")).unwrap();
    let names: Vec<_> = service
        .consumer_groups(Some("results"))
        .unwrap()
        .into_iter()
        .map(|g| g.group)
        .collect();
    assert!(names.contains(&"g".to_string()), "{names:?}");
}

#[test]
fn agent_mcp_publish_also_reopens_quiesced_run() {
    let tmp = TestDir::new("f8g-mcp");
    let (service, terminals, clock) = test_service(tmp.path());
    let run = quiesced_run(&service, &terminals, &clock, tmp.path());
    let reg = service.load_registry().unwrap();
    let agent = reg.agents[0].clone();
    let state = crate::mcp::State::new(tmp.path().join("state").join("agents.json"));
    crate::mcp::events::handle_event_tool(
        &state,
        &agent,
        "publish",
        &serde_json::json!({"topic": "results", "content": "x"}),
    )
    .unwrap();
    set_idle(&service, &terminals);
    ack_all(&service);
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Running);
}
