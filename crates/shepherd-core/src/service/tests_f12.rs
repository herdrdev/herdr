//! F12: pattern agents obey the pattern (own startup prompt, explicit starter, run context,
//! `stalled`, concurrency that ignores inactive agents of finished runs).
use super::patterns_run::PatternRunState;
use super::tests::{test_service, TestDir};
use super::*;
use crate::terminals::TerminalAgentStatus;
use std::time::Duration;

fn prompt_of(terminals: &crate::terminals::InMemoryTerminals, agent: &str) -> String {
    // Delivery may run on a helper thread when the pane is not ready yet.
    for _ in 0..100 {
        if let Some((_, p)) = terminals.prompts().iter().find(|(id, _)| id == agent) {
            return p.clone();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no prompt delivered to {agent}: {:?}", terminals.prompts());
}

fn set_state(
    service: &OrchestrationService,
    terminals: &crate::terminals::InMemoryTerminals,
    state: &str,
) {
    let mut reg = service.load_registry().unwrap();
    for ag in &mut reg.agents {
        ag.state = state.into();
        let st = TerminalAgentStatus {
            state: "live".into(),
            status: state.into(),
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

/// Idle agents, a reconcile that wakes whoever has unread messages, idle again, then T+1 s.
fn settle(
    service: &OrchestrationService,
    terminals: &crate::terminals::InMemoryTerminals,
    clock: &ManualClock,
) {
    set_state(service, terminals, "idle");
    service.reconcile_once().unwrap();
    set_state(service, terminals, "idle");
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
}

fn start(service: &OrchestrationService, dir: &std::path::Path, name: &str) -> PatternRun {
    service
        .start_pattern_run(dir, Some(name), None, Some("build X"), None, true)
        .unwrap()
}
use super::patterns_run::PatternRun;

#[test]
fn pattern_prompt_replaces_the_role_script_and_names_the_pattern() {
    let tmp = TestDir::new("f12-prompt");
    let (service, terminals, _clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = start(&service, tmp.path(), "hierarchical");
    let manager = run.agents.iter().find(|a| a.role == "reviewer").unwrap();
    let p = prompt_of(&terminals, &manager.id);
    assert!(
        p.contains("You are in the pattern `hierarchical` (`hierarchical`)"),
        "{p}"
    );
    assert!(p.contains(&format!("your id is `{}`", manager.id)), "{p}");
    assert!(p.contains("engineering manager"), "persona missing: {p}");
    assert!(p.contains("build X"), "objective missing: {p}");
    // The reviewer role script (diff review, get_task_context) must not leak in.
    assert!(!p.contains("get_diff"), "{p}");
    assert!(!p.contains("independent **reviewer**"), "{p}");
    assert!(!p.contains("## Your task"), "{p}");
}

#[test]
fn only_starters_get_the_kickoff_order() {
    let tmp = TestDir::new("f12-starter");
    let (service, terminals, _clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    for name in [
        "market",
        "hierarchical",
        "orchestrator-worker",
        "blackboard",
    ] {
        let pat = crate::patterns::get_builtin_pattern(name).unwrap();
        let starters = pat.starter_ids();
        let run = start(&service, tmp.path(), name);
        for a in &run.agents {
            let def = pat.agents.iter().find(|d| d.prompt == a.prompt).unwrap();
            let p = prompt_of(&terminals, &a.id);
            let starts = p.contains("Start now");
            let waits = p.contains("Wait for messages on");
            assert!(starts != waits, "{name}/{}: {p}", a.id);
            assert_eq!(starts, starters.contains(&def.id), "{name}/{}: {p}", a.id);
            assert_eq!(a.start, starts, "{name}/{}", a.id);
        }
    }
}

#[test]
fn explicit_start_overrides_the_topology_default() {
    let mut pat = crate::patterns::get_builtin_pattern("blackboard").unwrap();
    assert_eq!(pat.starter_ids(), vec!["researcher"]);
    pat.agents[0].start = false;
    pat.agents[2].start = true;
    assert_eq!(pat.starter_ids(), vec!["synthesizer"]);
    let market = crate::patterns::get_builtin_pattern("market").unwrap();
    assert_eq!(market.starter_ids(), vec!["requester"]);
    let hier = crate::patterns::get_builtin_pattern("hierarchical").unwrap();
    assert_eq!(hier.starter_ids(), vec!["director"]);
}

#[test]
fn get_task_context_for_a_pattern_run_returns_objective_topology_and_persona() {
    let tmp = TestDir::new("f12-context");
    let (service, _terminals, _clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = start(&service, tmp.path(), "market");
    let reg = service.load_registry().unwrap();
    let solver_id = run
        .agents
        .iter()
        .find(|a| a.role == "coder")
        .unwrap()
        .id
        .clone();
    let solver = reg
        .agents
        .iter()
        .find(|a| a.id == solver_id)
        .unwrap()
        .clone();
    let state = crate::mcp::State::new(tmp.path().join("state").join("agents.json"));
    let out = crate::mcp::tools::call_tool(
        &state,
        &solver,
        &serde_json::json!({"name":"get_task_context","arguments":{}}),
    )
    .expect("get_task_context must work for a pattern run");
    let text = out["structuredContent"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("build X"), "{text}");
    assert!(text.contains(&run.id), "{text}");
    assert!(
        text.contains("brain-1") && text.contains("coder-3"),
        "topology: {text}"
    );
    assert!(text.contains("publish one bid"), "persona: {text}");
}

#[test]
fn run_without_any_message_stalls_instead_of_done_and_revives_on_publish() {
    let tmp = TestDir::new("f12-stalled-silent");
    let (service, terminals, clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = start(&service, tmp.path(), "orchestrator-worker");
    settle(&service, &terminals, &clock);
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Stalled);
    assert!(r.stalled_reason.as_deref().unwrap().contains("no activity"));
    service
        .publish_topic("orders", "go", None, Some(0), "human")
        .unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Running);
    assert!(r.stalled_reason.is_none());
}

#[test]
fn blocked_agent_makes_the_run_stalled_not_done() {
    let tmp = TestDir::new("f12-stalled-blocked");
    let (service, terminals, clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = start(&service, tmp.path(), "orchestrator-worker");
    service
        .publish_topic("orders", "go", None, Some(0), "human")
        .unwrap();
    set_state(&service, &terminals, "idle");
    let mut reg = service.load_registry().unwrap();
    reg.agents[1].state = "blocked".into();
    let st = TerminalAgentStatus {
        state: "live".into(),
        status: "blocked".into(),
        pane: reg.agents[1].pane.clone(),
        tab: reg.agents[1].tab.clone(),
        title: reg.agents[1].label.clone(),
        seq: 2,
    };
    terminals.set_status(&reg.agents[1].id, st.clone());
    terminals.set_status(&reg.agents[1].pane, st);
    service.save_registry(&reg).unwrap();
    service.reconcile_once().unwrap();
    clock.advance(Duration::from_secs(31));
    service.reconcile_once().unwrap();
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Stalled);
    assert!(r.stalled_reason.as_deref().unwrap().contains("blocked"));
}

#[test]
fn healthy_run_still_reaches_done() {
    let tmp = TestDir::new("f12-healthy");
    let (service, terminals, clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    let run = start(&service, tmp.path(), "orchestrator-worker");
    service
        .publish_topic("orders", "go", None, Some(0), "human")
        .unwrap();
    settle(&service, &terminals, &clock);
    let r = service.get_pattern_run(tmp.path(), &run.id).unwrap();
    assert_eq!(r.state, PatternRunState::Done);
}

#[test]
fn inactive_agents_of_finished_runs_do_not_count_for_concurrency() {
    let tmp = TestDir::new("f12-concurrency");
    let (service, terminals, clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9099/mcp".into()));
    // 4 agents (default max_concurrent_agents = 4) from a run that finished.
    let run = start(&service, tmp.path(), "orchestrator-worker");
    service
        .publish_topic("orders", "go", None, Some(0), "human")
        .unwrap();
    settle(&service, &terminals, &clock);
    assert_eq!(
        service.get_pattern_run(tmp.path(), &run.id).unwrap().state,
        PatternRunState::Done
    );
    let again = service.start_pattern_run(
        tmp.path(),
        Some("orchestrator-worker"),
        None,
        Some("second"),
        None,
        false,
    );
    assert!(
        again.is_ok(),
        "idle agents of a done run must not block: {again:?}"
    );
}
