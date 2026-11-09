//! F8f: run quiescence, self-publisher wake exclusion.
use super::patterns_run::PatternRunState;
use super::tests::{test_service, TestDir};
use super::*;
use crate::terminals::TerminalAgentStatus;
use std::time::Duration;

fn set_all_status(
    service: &OrchestrationService,
    terminals: &crate::terminals::InMemoryTerminals,
    status: &str,
) {
    let mut reg = service.load_registry().unwrap();
    for ag in &mut reg.agents {
        ag.state = status.into();
        let st = TerminalAgentStatus {
            state: "live".into(),
            status: status.into(),
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

fn start_run(service: &OrchestrationService, dir: &Path) -> patterns_run::PatternRun {
    // Real runs use MCP agents: they poll/ack themselves and are never auto-marked done.
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    service
        .start_pattern_run(
            dir,
            Some("orchestrator-worker"),
            None,
            Some("t"),
            None,
            true,
        )
        .unwrap()
}

#[test]
fn quiescent_run_goes_done_after_period_and_revives_on_new_message() {
    let tmp = TestDir::new("f8f-quiescence");
    let (service, terminals, clock) = test_service(tmp.path());
    let run = start_run(&service, tmp.path());
    // F12: a run where nothing was ever published is `stalled`, not `done`.
    service
        .publish_topic("orders", "go", None, Some(0), "human")
        .unwrap();
    set_all_status(&service, &terminals, "idle");

    service.reconcile_once().unwrap(); // wakes the unread consumers
    set_all_status(&service, &terminals, "idle");
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(
        r.state,
        PatternRunState::Running,
        "too early for quiescence"
    );

    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Done);
    assert!(r.finished_at.is_some());

    // A new message from someone else revives the run.
    service
        .bus()
        .unwrap()
        .publish("orders", "more work", None, Some(0), "human")
        .unwrap();
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Running);
    assert!(r.finished_at.is_none());
}

#[test]
fn run_with_unread_messages_or_working_agent_is_not_quiescent() {
    let tmp = TestDir::new("f8f-not-quiescent");
    let (service, terminals, clock) = test_service(tmp.path());
    let run = start_run(&service, tmp.path());
    set_all_status(&service, &terminals, "idle");
    // Unread message for the worker; its pane stays "blocked" so no wake resolves it.
    service
        .bus()
        .unwrap()
        .publish("orders", "pending", None, Some(0), "orchestrator-1")
        .unwrap();
    let mut reg = service.load_registry().unwrap();
    for ag in &mut reg.agents {
        ag.notice = BLOCKED_HUMAN_NOTICE.into();
    }
    service.save_registry(&reg).unwrap();
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(120));
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Running);
}

#[test]
fn paused_run_is_not_quiesced() {
    let tmp = TestDir::new("f8f-paused");
    let (service, terminals, clock) = test_service(tmp.path());
    let run = start_run(&service, tmp.path());
    service.pause_pattern_run(tmp.path(), &run.id).unwrap();
    set_all_status(&service, &terminals, "idle");
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(120));
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Paused);
}

#[test]
fn quiescence_is_reconciled_after_server_restart() {
    let tmp = TestDir::new("f8f-restart");
    let (service, terminals, _clock) = test_service(tmp.path());
    let run = start_run(&service, tmp.path());
    service
        .publish_topic("orders", "go", None, Some(0), "human")
        .unwrap();
    set_all_status(&service, &terminals, "idle");

    // Restart: fresh service (empty in-memory timers) over the same state, panes still live.
    let (service2, terminals2, clock2) = test_service(tmp.path());
    service2.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    set_all_status(&service2, &terminals2, "idle");
    service2.resume_on_startup().unwrap();
    service2.reconcile_once().unwrap(); // wakes the unread consumers
    set_all_status(&service2, &terminals2, "idle");
    service2.reconcile_once().unwrap();
    clock2.advance(Duration::from_secs(31));
    service2.reconcile_once().unwrap();
    let r = service2.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Done);
}

#[test]
fn wake_ignores_messages_published_by_the_consumer_itself() {
    let tmp = TestDir::new("f8f-self-wake");
    let (service, terminals, clock) = test_service(tmp.path());
    let agent = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "t".into(),
            mcp_url: Some("http://127.0.0.1:9099".into()),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    set_all_status(&service, &terminals, "idle");
    let bus = service.bus().unwrap();
    bus.subscribe(&agent.id, "results", None).unwrap();
    let before = terminals.prompts().len();

    bus.publish("results", "my own summary", None, Some(0), &agent.id)
        .unwrap();
    service.reconcile_once().unwrap();
    assert_eq!(terminals.prompts().len(), before, "woken by own message");

    // Own + foreign: count only the foreign one.
    clock.advance(Duration::from_secs(5));
    bus.publish("results", "foreign", None, Some(0), "coder-1")
        .unwrap();
    service.reconcile_once().unwrap();
    let prompts = terminals.prompts();
    assert_eq!(prompts.len(), before + 1);
    assert!(prompts[before].1.contains("1 new message on 'results'"));
}

#[test]
fn consumer_groups_report_total_messages_and_lag_per_partition() {
    let tmp = TestDir::new("f8f-groups");
    let (service, _terminals, _clock) = test_service(tmp.path());
    let bus = service.bus().unwrap();
    bus.topic_create("results", 2).unwrap();
    bus.subscribe("brain-1", "results", None).unwrap();
    bus.subscribe("coder-1", "results", None).unwrap();
    for _ in 0..3 {
        bus.publish("results", "m", None, Some(0), "x").unwrap();
    }
    bus.publish("results", "m", None, Some(1), "x").unwrap();
    bus.ack("coder-1", "results", Some(0), Some(1)).unwrap();

    let groups = service.consumer_groups(Some("results")).unwrap();
    let json = serde_json::to_value(&groups).unwrap();
    assert_eq!(json[0]["group"], "brain-1");
    assert_eq!(json[0]["total_messages"], 4);
    assert_eq!(
        json[0]["lag_by_partition"],
        serde_json::json!({"0": 3, "1": 1})
    );
    assert_eq!(json[1]["group"], "coder-1");
    assert_eq!(json[1]["total_messages"], 4);
    assert_eq!(
        json[1]["lag_by_partition"],
        serde_json::json!({"0": 1, "1": 1})
    );
    assert_eq!(json[1]["committed_offsets"], serde_json::json!({"0": 1}));
}
