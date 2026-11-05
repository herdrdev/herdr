use super::*;
use crate::bus::Message;
use crate::terminals::{InMemoryTerminals, TerminalAgentStatus};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(1);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "shep-service-test-{name}-{}-{n}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_service(dir: &Path) -> (OrchestrationService, InMemoryTerminals, ManualClock) {
    let state_dir = dir.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let terminals = InMemoryTerminals::new();
    let clock = ManualClock::new();
    let svc_cfg = ServiceConfig::for_state_dir(&state_dir);
    let shep_cfg = Config::default();
    let service = OrchestrationService::new(
        svc_cfg,
        shep_cfg,
        Arc::new(terminals.clone()),
        Arc::new(clock.clone()),
    )
    .with_active_vendors(vec!["claude".into(), "codex".into(), "gemini".into()]);
    (service, terminals, clock)
}

#[test]
fn req1_durable_registry_permissions_legacy_and_monotonic_counters() {
    let tmp = TestDir::new("test");
    let reg_path = tmp.path().join("agents.json");

    // 1. Load legacy bare array format
    let legacy_json = r#"[
            {
                "id": "coder-3",
                "seq": 7,
                "role": "coder",
                "vendor": "codex",
                "root": "/repo",
                "state": "done",
                "notice": "old notice",
                "token": "tok-3",
                "started": "2026-04-20T00:00:00Z",
                "updated": "2026-04-20T00:00:00Z"
            }
        ]"#;
    std::fs::write(&reg_path, legacy_json).unwrap();

    let mut reg = load_agent_registry(&reg_path).unwrap();
    assert_eq!(reg.seq, 7);
    assert_eq!(reg.next.get("coder"), Some(&3));
    assert_eq!(reg.agents[0].notice, "old notice");
    assert!(reg.agents[0].terminal());
    assert!(!reg.agents[0].gone());

    // 2. Remove coder-3, save, and reload: counters must NEVER go backwards
    assert!(reg.remove("coder-3"));
    save_agent_registry(&reg_path, &reg).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&reg_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "agents.json must be written with 0600 permissions"
        );
    }

    let mut reloaded = load_agent_registry(&reg_path).unwrap();
    assert!(reloaded.agents.is_empty());
    assert_eq!(reloaded.seq, 7);
    assert_eq!(reloaded.next.get("coder"), Some(&3));

    let next_coder = reloaded
        .create_secure(
            Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                root: "/repo".into(),
                ..Agent::default()
            },
            "2026-04-20T00:01:00Z",
        )
        .unwrap();
    assert_eq!(next_coder.id, "coder-4");
    assert_eq!(next_coder.seq, 8);
    assert_eq!(next_coder.state, "starting");
}

#[test]
fn req2_first_prompt_delivery_blocked_notice_only_for_blocked_and_clears_on_delivery() {
    let tmp = TestDir::new("test");
    let (service, terminals, clock) = test_service(tmp.path());

    // Spawn without auto-delivering so we can step through readiness states
    let spawned = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "plan feature".into(),
            deliver_prompt: false,
            ..ServiceSpawnRequest::default()
        })
        .unwrap();

    // Initially set status to "not_ready" for 5 seconds, then "blocked" for 4 seconds, then "idle"
    terminals.set_status(
        &spawned.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "not_ready".into(),
            pane: spawned.pane.clone(),
            tab: spawned.tab.clone(),
            title: spawned.label.clone(),
            seq: 1,
        },
    );

    let service_ref_path = service.service_config().registry_path();
    let terminals_hook = terminals.clone();
    let agent_id = spawned.id.clone();
    let pane_id = spawned.pane.clone();
    let tab_id = spawned.tab.clone();
    let label = spawned.label.clone();

    let not_ready_notice_seen = Arc::new(Mutex::new(None));
    let blocked_notice_seen = Arc::new(Mutex::new(None));
    let nr_clone = not_ready_notice_seen.clone();
    let bl_clone = blocked_notice_seen.clone();

    clock.set_on_sleep(move |now_ms| {
        if now_ms == 4_000 {
            // After 4s of "not_ready" (> 3s notice_after), notice MUST still be empty!
            let reg = load_agent_registry(&service_ref_path).unwrap();
            let a = reg.result(&agent_id).unwrap();
            *nr_clone.lock().unwrap() = Some(a.notice.clone());
            // Now switch to "blocked"
            terminals_hook.set_status(
                &agent_id,
                TerminalAgentStatus {
                    state: "live".into(),
                    status: "blocked".into(),
                    pane: pane_id.clone(),
                    tab: tab_id.clone(),
                    title: label.clone(),
                    seq: 1,
                },
            );
        } else if now_ms == 7_400 {
            // 3.4s after becoming "blocked" (>= 3s notice_after): notice MUST be set!
            let reg = load_agent_registry(&service_ref_path).unwrap();
            let a = reg.result(&agent_id).unwrap();
            *bl_clone.lock().unwrap() = Some(a.notice.clone());
            // Human resolves dialog -> agent becomes "idle"
            terminals_hook.set_status(
                &agent_id,
                TerminalAgentStatus {
                    state: "live".into(),
                    status: "idle".into(),
                    pane: pane_id.clone(),
                    tab: tab_id.clone(),
                    title: label.clone(),
                    seq: 2,
                },
            );
        }
    });

    service
        .deliver_first_prompt(&spawned.id, "hello world")
        .unwrap();

    assert_eq!(
        not_ready_notice_seen.lock().unwrap().as_deref(),
        Some(""),
        "notice must NEVER be set for not_ready"
    );
    assert_eq!(
        blocked_notice_seen.lock().unwrap().as_deref(),
        Some(BLOCKED_HUMAN_NOTICE),
        "notice must be set after notice_after in blocked state"
    );
    // Never answered any dialog on behalf of the human
    assert!(terminals.sent_keys().is_empty());
    assert!(terminals.sent_text().is_empty());

    // Notice must be cleared and state must be "working" after delivery
    let final_reg = service.load_registry().unwrap();
    let final_agent = final_reg.result(&spawned.id).unwrap();
    assert_eq!(final_agent.state, "working");
    assert!(final_agent.notice.is_empty());
}

#[test]
fn req2_first_prompt_timeout_sets_never_ready_error_and_clears_notice() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());

    let spawned = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "plan feature".into(),
            deliver_prompt: false,
            ..ServiceSpawnRequest::default()
        })
        .unwrap();

    terminals.set_status(
        &spawned.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "blocked".into(),
            pane: spawned.pane.clone(),
            tab: spawned.tab.clone(),
            title: spawned.label.clone(),
            seq: 1,
        },
    );

    let err = service
        .deliver_first_prompt(&spawned.id, "hello")
        .unwrap_err();
    assert_eq!(err, NEVER_READY_ERROR);

    let reg = service.load_registry().unwrap();
    let agent = reg.result(&spawned.id).unwrap();
    assert_eq!(agent.state, "failed");
    assert_eq!(agent.error, NEVER_READY_ERROR);
    assert!(
        agent.notice.is_empty(),
        "notice must be cleared upon failure"
    );
    assert!(terminals.sent_keys().is_empty());
}

#[test]
fn req2_first_prompt_resends_up_to_3_times_when_agent_does_not_react() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());
    terminals.set_auto_react_on_prompt(false);

    let spawned = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "plan feature".into(),
            deliver_prompt: false,
            ..ServiceSpawnRequest::default()
        })
        .unwrap();

    let err = service
        .deliver_first_prompt(&spawned.id, "do work")
        .unwrap_err();
    assert!(
        err.contains("no reaccionó al primer prompt tras 3 reenvíos"),
        "unexpected error: {err}"
    );
    // 1 initial send + 3 resends = 4 prompts sent
    assert_eq!(terminals.prompts().len(), 4);

    let reg = service.load_registry().unwrap();
    let agent = reg.result(&spawned.id).unwrap();
    assert_eq!(agent.state, "failed");
    assert!(agent.notice.is_empty());

    // Now test succeeding on the 2nd resend
    let spawned2 = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "plan second feature".into(),
            deliver_prompt: false,
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();
    let terminals_clone = terminals.clone();
    let id2 = spawned2.id.clone();
    let pane2 = spawned2.pane.clone();
    let tab2 = spawned2.tab.clone();
    let label2 = spawned2.label.clone();
    terminals.on_prompt(move |_target, _text| {
        let n = count_clone.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 3 {
            // React on the 2nd resend (3rd send) by advancing seq
            terminals_clone.set_status(
                &id2,
                TerminalAgentStatus {
                    state: "live".into(),
                    status: "idle".into(),
                    pane: pane2.clone(),
                    tab: tab2.clone(),
                    title: label2.clone(),
                    seq: 99,
                },
            );
        }
    });
    service
        .deliver_first_prompt(&spawned2.id, "do second work")
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
    let reg2 = service.load_registry().unwrap();
    assert_eq!(reg2.result(&spawned2.id).unwrap().state, "working");
}

#[test]
fn req3_periodic_reconciliation_blocked_once_and_non_mcp_quiet_screen_done() {
    let tmp = TestDir::new("test");
    let (service, terminals, clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:4317/mcp".into()));

    // 1. MCP agent (brain -> claude) and non-MCP agent (docs -> gemini/agy)
    let mcp_agent = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "coordinate".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    assert!(mcp_agent.mcp);

    let non_mcp_agent = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "docs".into(),
            task: "summarize".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    assert!(!non_mcp_agent.mcp);

    // 2. Put MCP agent in "blocked" across two reconcile ticks -> bus message emitted ONLY ONCE
    terminals.set_status(
        &mcp_agent.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "blocked".into(),
            pane: mcp_agent.pane.clone(),
            tab: mcp_agent.tab.clone(),
            title: mcp_agent.label.clone(),
            seq: 3,
        },
    );
    service.reconcile_once().unwrap();
    service.reconcile_once().unwrap();

    let timeline = service.bus().unwrap().since(0).unwrap();
    let blocked_msgs: Vec<_> = timeline
        .iter()
        .filter(|m| m.content.contains(&mcp_agent.id) && m.content.contains("blocked"))
        .collect();
    assert_eq!(
        blocked_msgs.len(),
        1,
        "blocked state must be announced only once per episode on the bus"
    );
    let notices = terminals.notifications();
    assert_eq!(
        notices.len(),
        1,
        "notification must be emitted only once per blocked episode"
    );
    assert!(notices[0].0.contains(&mcp_agent.id));

    // MCP agent becomes "idle" -> stays "idle" (not "done", because MCP agents finish via submit_result)
    terminals.set_status(
        &mcp_agent.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: mcp_agent.pane.clone(),
            tab: mcp_agent.tab.clone(),
            title: mcp_agent.label.clone(),
            seq: 4,
        },
    );
    let reg = service.reconcile_once().unwrap();
    assert_eq!(reg.result(&mcp_agent.id).unwrap().state, "idle");

    // 3. Non-MCP agent (agy): Shepherd reports "done" prematurely while screen is still changing!
    terminals.set_status(
        &non_mcp_agent.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "done".into(),
            pane: non_mcp_agent.pane.clone(),
            tab: non_mcp_agent.tab.clone(),
            title: non_mcp_agent.label.clone(),
            seq: 5,
        },
    );
    terminals.set_screen(&non_mcp_agent.pane, "Thinking step 1...");
    let reg = service.reconcile_once().unwrap();
    assert_ne!(
        reg.result(&non_mcp_agent.id).unwrap().state,
        "done",
        "non-MCP agent must not be marked done before screen is quiet for 60s"
    );

    // Advance 30s, screen changes -> timer resets!
    clock.advance(Duration::from_secs(30));
    terminals.set_screen(&non_mcp_agent.pane, "Final summary output");
    let reg = service.reconcile_once().unwrap();
    assert_ne!(reg.result(&non_mcp_agent.id).unwrap().state, "done");

    // Advance 59s -> still < 60s since last screen change
    clock.advance(Duration::from_secs(59));
    let reg = service.reconcile_once().unwrap();
    assert_ne!(reg.result(&non_mcp_agent.id).unwrap().state, "done");

    // Advance 1 more second (total 60s unchanged) -> now marked "done" with screen captured as result!
    clock.advance(Duration::from_secs(1));
    let reg = service.reconcile_once().unwrap();
    let done_agent = reg.result(&non_mcp_agent.id).unwrap();
    assert_eq!(done_agent.state, "done");
    assert!(done_agent.has_result);
    assert_eq!(done_agent.result, "Final summary output");

    // 4. Missing pane -> transitions to "exited"
    terminals.remove_status(&mcp_agent.id);
    let reg = service.reconcile_once().unwrap();
    assert_eq!(reg.result(&mcp_agent.id).unwrap().state, "exited");
}

#[test]
fn req4_resume_on_startup_reconciles_starting_and_preserves_tokens() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());

    let mut reg = AgentRegistryFile::default();
    // 1. starting without pane -> exited
    let mut a1 = reg.create(
        Agent {
            role: "coder".into(),
            vendor: "codex".into(),
            pane: "pane-missing".into(),
            notice: "stale notice".into(),
            ..Agent::default()
        },
        "token-1".into(),
        "2026-04-20T00:00:00Z",
    );
    a1.pane = "pane-missing".into();
    reg.agents[0] = a1;

    // 2. starting that is already working -> working
    let mut a2 = reg.create(
        Agent {
            role: "coder".into(),
            vendor: "codex".into(),
            pane: "pane-working".into(),
            notice: "stale notice 2".into(),
            ..Agent::default()
        },
        "token-2".into(),
        "2026-04-20T00:00:00Z",
    );
    a2.pane = "pane-working".into();
    reg.agents[1] = a2.clone();
    terminals.set_status(
        &a2.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "working".into(),
            pane: "pane-working".into(),
            tab: "tab-working".into(),
            title: "coder·codex·2".into(),
            seq: 2,
        },
    );

    // 3. starting that never received its first prompt (still idle) -> failed with relaunch instructions
    let mut a3 = reg.create(
        Agent {
            role: "reviewer".into(),
            vendor: "gemini".into(),
            pane: "pane-idle".into(),
            notice: "stale notice 3".into(),
            ..Agent::default()
        },
        "token-3".into(),
        "2026-04-20T00:00:00Z",
    );
    a3.pane = "pane-idle".into();
    reg.agents[2] = a3.clone();
    terminals.set_status(
        &a3.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: "pane-idle".into(),
            tab: "tab-idle".into(),
            title: "reviewer·gemini·1".into(),
            seq: 1,
        },
    );

    service.save_registry(&reg).unwrap();
    let resumed = service.resume_on_startup().unwrap();

    let r1 = resumed.result("coder-1").unwrap();
    assert_eq!(r1.state, "exited");
    assert!(r1.notice.is_empty());
    assert_eq!(r1.token, "token-1");

    let r2 = resumed.result("coder-2").unwrap();
    assert_eq!(r2.state, "working");
    assert!(r2.notice.is_empty());
    assert_eq!(r2.token, "token-2");

    let r3 = resumed.result("reviewer-1").unwrap();
    assert_eq!(r3.state, "failed");
    assert!(r3.notice.is_empty());
    assert!(
        r3.error.contains("relánzalo"),
        "failed starting agent must explain how to relaunch: {}",
        r3.error
    );
    assert_eq!(r3.token, "token-3");
}

#[test]
fn req5_kill_clean_focus_and_limits_confirmation() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());

    let brain = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "lead".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();

    // Focus selects native pane/tab
    service.focus(&brain.id).unwrap();
    assert!(!terminals.focused().is_empty());

    // Mark brain as "done" -> kill must accept "done" and succeed even if pane is already gone
    let mut reg = service.load_registry().unwrap();
    reg.submit_result(&brain.id, "done planning", "2026-04-20T00:01:00Z")
        .unwrap();
    service.save_registry(&reg).unwrap();
    terminals.remove_status(&brain.id);

    let killed = service.kill(&brain.id).unwrap();
    assert_eq!(killed.state, "killed");

    // Killing an already-definitive agent must fail
    assert!(service.kill(&brain.id).is_err());

    // Spawn another agent, leave it in "working" -> clean must reject running agent
    let coder = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "coder".into(),
            task: "implement".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    assert!(service
        .clean(std::slice::from_ref(&coder.id), None)
        .is_err());

    // Clean path traversal validation
    assert!(service.clean(&["../evil".into()], None).is_err());
    assert!(service.clean(&["foo/bar".into()], None).is_err());

    // Mark coder as "done" -> clean by explicit ID kills it first and removes its session dir
    let mut reg = service.load_registry().unwrap();
    reg.submit_result(&coder.id, "implemented", "2026-04-20T00:02:00Z")
        .unwrap();
    service.save_registry(&reg).unwrap();
    let coder_session_dir = service.service_config().agent_session_dir(&coder.id);
    assert!(coder_session_dir.exists());

    let cleaned = service
        .clean(std::slice::from_ref(&coder.id), None)
        .unwrap();
    assert_eq!(cleaned, vec![coder.id.clone()]);
    assert!(!coder_session_dir.exists());
    assert!(terminals.closed().contains(&coder.tab));

    // Clean without explicit IDs removes all definitive agents (brain-1) while keeping counters
    let cleaned_all = service.clean(&[], None).unwrap();
    assert_eq!(cleaned_all, vec![brain.id.clone()]);
    let reg_after = service.load_registry().unwrap();
    assert!(reg_after.agents.is_empty());
    assert_eq!(reg_after.next.get("brain"), Some(&1));
    assert_eq!(reg_after.next.get("coder"), Some(&1));
}

#[test]
fn req6_unique_agent_names_reclaims_shepherd_leftover_and_protects_user_panes() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());

    // 1. Pre-populate a Shepherd leftover pane for "coder-1" (`coder·codex·old-task`, not in registry)
    terminals.put_open_pane(
        "coder-1",
        "pane-leftover",
        "tab-leftover",
        "coder·codex·old-task",
    );

    let coder1 = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "coder".into(),
            task: "new task".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    assert_eq!(coder1.id, "coder-1");
    assert!(
        terminals.closed().contains(&"tab-leftover".to_owned()),
        "Shepherd leftover tab must be closed before retrying open_agent"
    );

    // 2. Pre-populate a user/non-Shepherd pane for "coder-2" (label "vim-notes")
    terminals.put_open_pane("coder-2", "pane-user", "tab-user", "vim-notes");
    let err = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "coder".into(),
            task: "another task".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap_err();
    assert!(
        err.contains("pane-user") && err.contains("vim-notes"),
        "error must explain which pane/label occupies the name: {err}"
    );
    assert!(
        !terminals.closed().contains(&"tab-user".to_owned()),
        "non-leftover pane must NEVER be closed"
    );

    // 3. Req 8: Verify failed request was logged in memory and on disk
    let failures = service.failed_requests();
    assert!(!failures.is_empty());
    assert_eq!(failures.last().unwrap().method, "spawn");
    let log_on_disk = std::fs::read_to_string(service.service_config().log_path()).unwrap();
    assert!(log_on_disk.contains("method=spawn"));
}

#[test]
fn test_handle_call_tasks_runs_skills() {
    let tmp = TestDir::new("call-ops");
    let (service, _terminals, _clock) = test_service(tmp.path());

    // 1. Task create
    let create_args = serde_json::json!({
        "root": tmp.path().to_str().unwrap(),
        "title": "Test task F3b",
        "description": "Task description for testing",
        "criteria": ["- [ ] first criteria"],
        "attachments": []
    });
    let created = service.handle_call("task_create", create_args).unwrap();
    let task_id = created["status"]["id"].as_u64().unwrap();
    assert_eq!(task_id, 1);

    // 2. Tasks list
    let list_args = serde_json::json!({
        "root": tmp.path().to_str().unwrap()
    });
    let tasks = service.handle_call("tasks", list_args.clone()).unwrap();
    let arr = tasks.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["status"]["title"], "Test task F3b");

    // 3. Runs list
    let runs = service.handle_call("runs", list_args.clone()).unwrap();
    let runs_arr = runs.as_array().unwrap();
    assert_eq!(runs_arr.len(), 1);
    assert_eq!(runs_arr[0]["task_id"], 1);
    assert_eq!(runs_arr[0]["subtasks_progress"], serde_json::json!([0, 0]));
    assert!(runs_arr[0]["subtasks"].is_array());

    // 4. Skills list
    let skills = service.handle_call("skills", list_args).unwrap();
    assert!(skills.is_array());

    // 5. Task archive
    let archive_args = serde_json::json!({
        "root": tmp.path().to_str().unwrap(),
        "id": 1
    });
    let archived = service.handle_call("task_archive", archive_args).unwrap();
    assert_eq!(archived["archived"], true);

    // 6. Git status
    let _ = std::process::Command::new("git")
        .args(["init", "-b", "master"])
        .current_dir(tmp.path())
        .output();
    let git_status = service
        .handle_call(
            "git_status",
            serde_json::json!({
                "root": tmp.path().to_str().unwrap()
            }),
        )
        .unwrap();
    assert!(git_status["worktrees"].is_array());
}

#[test]
fn test_runs_pending_gate_and_subtasks_real() {
    let tmp = TestDir::new("runs-gates");
    let (service, _terminals, _clock) = test_service(tmp.path());

    // Create task 1: waiting for plan
    let create_args = serde_json::json!({
        "root": tmp.path().to_str().unwrap(),
        "title": "Task with plan gate",
        "description": "Waiting for plan",
        "criteria": ["- [ ] criteria"],
        "attachments": []
    });
    service.handle_call("task_create", create_args).unwrap();
    let mut t1 = crate::find_task(tmp.path(), "1").unwrap();
    t1.status.waiting = "plan".into();
    crate::save_task_status(t1.directory.join("status.json"), &t1.status).unwrap();

    // Create task 2: subtasks with awaiting-approval
    let create_args2 = serde_json::json!({
        "root": tmp.path().to_str().unwrap(),
        "title": "Task with push_pr gate",
        "description": "Subtask ready for PR",
        "criteria": ["- [ ] criteria 1", "- [ ] criteria 2"],
        "attachments": []
    });
    service.handle_call("task_create", create_args2).unwrap();
    let mut t2 = crate::find_task(tmp.path(), "2").unwrap();
    t2.status.subtasks = vec![
        crate::Subtask {
            number: 1,
            title: "Subtask 1".into(),
            body: "Body 1".into(),
            depends_on: vec![],
            verify: vec![],
            state: "done".into(),
            note: Some("done note".into()),
            coder: "coder-1".into(),
            reviewer: "reviewer-1".into(),
            round: 1,
            branch: "shep/1-s1".into(),
            pr: "https://pr/1".into(),
        },
        crate::Subtask {
            number: 2,
            title: "Subtask 2".into(),
            body: "Body 2".into(),
            depends_on: vec![1],
            verify: vec![],
            state: "awaiting-approval".into(),
            note: Some("waiting note".into()),
            coder: "coder-2".into(),
            reviewer: "reviewer-2".into(),
            round: 1,
            branch: "shep/2-s2".into(),
            pr: "".into(),
        },
    ];
    crate::save_task_status(t2.directory.join("status.json"), &t2.status).unwrap();

    let runs_val = service
        .handle_call(
            "runs",
            serde_json::json!({ "root": tmp.path().to_str().unwrap() }),
        )
        .unwrap();
    let runs = runs_val.as_array().unwrap();
    assert_eq!(runs.len(), 2);

    // Task 1: pending_gate == "plan"
    let r1 = runs.iter().find(|r| r["task_id"] == 1).unwrap();
    assert_eq!(r1["pending_gate"], serde_json::json!("plan"));
    assert_eq!(r1["subtasks_progress"], serde_json::json!([0, 0]));

    // Task 2: pending_gate == {"kind":"push_pr", "subtask": 2}
    let r2 = runs.iter().find(|r| r["task_id"] == 2).unwrap();
    assert_eq!(
        r2["pending_gate"],
        serde_json::json!({ "kind": "push_pr", "subtask": 2 })
    );
    assert_eq!(r2["subtasks_progress"], serde_json::json!([1, 2]));
    let subs = r2["subtasks"].as_array().unwrap();
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0]["n"], 1);
    assert_eq!(subs[0]["state"], "done");
    assert_eq!(subs[1]["n"], 2);
    assert_eq!(subs[1]["state"], "awaiting-approval");
}

#[test]
fn test_service_approve_and_reject_ops() {
    let tmp = TestDir::new("service-approve-reject");
    let (service, _terminals, _clock) = test_service(tmp.path());

    // Create task with plan
    let create_args = serde_json::json!({
        "root": tmp.path().to_str().unwrap(),
        "title": "Task for approve reject test",
        "description": "Description",
        "criteria": ["- [ ] criteria 1"],
        "attachments": []
    });
    service.handle_call("task_create", create_args).unwrap();
    let mut t1 = crate::find_task(tmp.path(), "1").unwrap();
    t1.status.waiting = "plan".into();
    std::fs::write(
        t1.directory.join("plan.md"),
        "# Plan\n\n### 1. Subtask 1\nBody\n",
    )
    .unwrap();
    crate::save_task_status(t1.directory.join("status.json"), &t1.status).unwrap();

    // 1. Reject plan
    let rej_res = service
        .handle_call(
            "reject",
            serde_json::json!({
                "root": tmp.path().to_str().unwrap(),
                "task": "1",
                "target": "plan",
                "reason": "plan needs more details"
            }),
        )
        .unwrap();
    assert_eq!(rej_res["ok"], true);
    assert_eq!(rej_res["rejected"], "plan");
    let t1_reloaded = crate::find_task(tmp.path(), "1").unwrap();
    assert_eq!(t1_reloaded.status.waiting, "plan");
    assert!(t1_reloaded.status.note.contains("plan needs more details"));

    // 2. Approve plan
    let app_res = service
        .handle_call(
            "approve",
            serde_json::json!({
                "root": tmp.path().to_str().unwrap(),
                "task": "1",
                "target": "plan"
            }),
        )
        .unwrap();
    assert_eq!(app_res["ok"], true);
    assert_eq!(app_res["approved"], "plan");
    let mut t1_reloaded2 = crate::find_task(tmp.path(), "1").unwrap();
    assert!(t1_reloaded2.status.waiting.is_empty());

    // 3. Reject subtask in awaiting-approval
    t1_reloaded2.status.subtasks[0].state = "awaiting-approval".into();
    crate::save_task_status(
        t1_reloaded2.directory.join("status.json"),
        &t1_reloaded2.status,
    )
    .unwrap();

    let rej_sub = service
        .handle_call(
            "reject",
            serde_json::json!({
                "root": tmp.path().to_str().unwrap(),
                "task": "1",
                "target": "1",
                "reason": "flaky test"
            }),
        )
        .unwrap();
    assert_eq!(rej_sub["ok"], true);
    assert_eq!(rej_sub["rejected"], 1);

    let t1_reloaded3 = crate::find_task(tmp.path(), "1").unwrap();
    // Subtask must have transitioned to "pending" (non-terminal, retryable)
    assert_eq!(t1_reloaded3.status.subtasks[0].state, "pending");
    assert!(t1_reloaded3.status.subtasks[0]
        .note
        .as_ref()
        .unwrap()
        .contains("flaky test"));
}

#[test]
fn test_concurrent_run_does_not_block_agents() {
    let tmp = TestDir::new("concurrent-run-agents");
    let (service, _terminals, _clock) = test_service(tmp.path());
    service.set_mcp_url(Some("http://127.0.0.1:9999/mcp".into()));

    // Pre-create task 1
    let create_args = serde_json::json!({
        "root": tmp.path().to_str().unwrap(),
        "title": "Long running task",
        "description": "Will take time",
        "criteria": ["- [ ] work done"],
        "attachments": []
    });
    service.handle_call("task_create", create_args).unwrap();

    // Configure a running agent in registry so "agents" has data
    service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "brain".into(),
            task: "existing agent".into(),
            deliver_prompt: false,
            ..ServiceSpawnRequest::default()
        })
        .unwrap();

    // 1. Dispatch "run" via handle_call
    let run_res = service
        .handle_call(
            "run",
            serde_json::json!({
                "root": tmp.path().to_str().unwrap(),
                "task": "1",
            }),
        )
        .unwrap();

    // run op must return immediately with accepted: true and job id
    assert_eq!(run_res["accepted"], true);
    assert_eq!(run_res["task"], "1");
    assert_eq!(run_res["job"], "run-1");

    // 2. Concurrently call "agents" - it must respond immediately without being blocked by run!
    let start = std::time::Instant::now();
    let agents_res = service
        .handle_call("agents", serde_json::json!({}))
        .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "agents call must not be blocked by running workflow; took {:?}",
        elapsed
    );
    let agents = agents_res.as_array().unwrap();
    assert!(!agents.is_empty());
    assert_eq!(agents[0]["id"], "brain-1");
}

#[test]
fn format_exact_sender_and_content_truncation() {
    let mut registry = AgentRegistryFile::default();
    let a1 = registry.create(
        Agent {
            role: "coder".into(),
            ..Agent::default()
        },
        "token".into(),
        "2026-04-20T00:00:00Z",
    );
    assert_eq!(a1.id, "coder-1");
    assert_eq!(format_shep_sender("coder-1", &registry), "coder#1");
    assert_eq!(format_shep_sender("brain-20", &registry), "brain#20");
    assert_eq!(format_shep_sender("user", &registry), "user");

    let custom = Agent {
        id: "special_agent".into(),
        seq: 7,
        role: "reviewer".into(),
        ..Agent::default()
    };
    registry.agents.push(custom);
    assert_eq!(format_shep_sender("special_agent", &registry), "reviewer#7");

    let newlines = "first\nsecond\r\nthird\rfourth";
    assert_eq!(format_shep_content(newlines), "first second third fourth");

    let short_content = "short content";
    assert_eq!(format_shep_content(short_content), "short content");

    let long_content = "a".repeat(520);
    let truncated = format_shep_content(&long_content);
    assert_eq!(truncated.chars().count(), 501);
    assert!(truncated.ends_with('…'));
    assert_eq!(&truncated[..500], &"a".repeat(500));

    let formatted = format_shep_message("coder-1", "hello\nworld", &registry);
    assert_eq!(formatted, "[shep] from coder#1: hello world");
}

#[test]
fn req7_deliver_bus_messages_order_once_non_mcp_and_persists_cursor() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());

    // Spawn non-MCP agent (docs) and MCP agent (coder)
    let non_mcp = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "docs".into(),
            task: "doc-task".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    assert!(!non_mcp.mcp);

    let mcp = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "coder".into(),
            task: "code-task".into(),
            mcp_url: Some("http://127.0.0.1:9999/mcp".into()),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    assert!(mcp.mcp);

    // Set non-MCP agent to idle and give it a screen
    terminals.set_status(
        &non_mcp.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: non_mcp.pane.clone(),
            tab: non_mcp.tab.clone(),
            title: non_mcp.label.clone(),
            seq: 10,
        },
    );
    terminals.set_screen(&non_mcp.pane, "docs-shell$ ");

    // Set MCP agent to idle
    terminals.set_status(
        &mcp.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: mcp.pane.clone(),
            tab: mcp.tab.clone(),
            title: mcp.label.clone(),
            seq: 10,
        },
    );
    terminals.set_screen(&mcp.pane, "mcp-shell$ ");

    // Send messages to the bus
    let bus = service.bus().unwrap();
    let _m1 = bus
        .send(Message::directed(
            "brain-1",
            non_mcp.id.clone(),
            "msg",
            "first instruction",
        ))
        .unwrap();
    let m2 = bus
        .send(Message::directed(
            "brain-1",
            non_mcp.id.clone(),
            "msg",
            "second instruction\nmultiline",
        ))
        .unwrap();
    let _m3 = bus
        .send(Message::directed(
            "brain-1",
            mcp.id.clone(),
            "msg",
            "for mcp agent",
        ))
        .unwrap();
    let _m4 = bus
        .send(Message::directed("brain-1", "user", "msg", "for user"))
        .unwrap();

    // Run reconcile tick
    let reg = service.reconcile_once().unwrap();

    // 1. Check delivery to non-MCP agent: exact format, newlines as spaces, order
    let bus_prompts: Vec<_> = terminals
        .prompts()
        .into_iter()
        .filter(|(_, text)| text.starts_with("[shep]"))
        .collect();
    let non_mcp_sent: Vec<_> = bus_prompts
        .iter()
        .filter(|(target, _)| target == &non_mcp.pane || target == &non_mcp.id)
        .map(|(_, text)| text.as_str())
        .collect();

    assert_eq!(
        non_mcp_sent,
        vec![
            "[shep] from brain#1: first instruction",
            "[shep] from brain#1: second instruction multiline"
        ]
    );

    // 2. Neither MCP agent nor user received prompts
    assert!(
        !bus_prompts
            .iter()
            .any(|(t, _)| t == &mcp.pane || t == &mcp.id),
        "MCP agents must NOT receive prompts in terminal"
    );
    assert!(
        !bus_prompts.iter().any(|(t, _)| t == "user"),
        "user messages must NOT be prompted"
    );

    // 3. Cursor advanced to m2.id
    let updated_non_mcp = reg.result(&non_mcp.id).unwrap();
    assert_eq!(updated_non_mcp.delivery_cursor, m2.id);

    // 4. Second reconcile tick does not re-deliver
    let bus_prompts_len_before = terminals
        .prompts()
        .into_iter()
        .filter(|(_, text)| text.starts_with("[shep]"))
        .count();
    service.reconcile_once().unwrap();
    let bus_prompts_len_after = terminals
        .prompts()
        .into_iter()
        .filter(|(_, text)| text.starts_with("[shep]"))
        .count();
    assert_eq!(
        bus_prompts_len_after, bus_prompts_len_before,
        "messages must be delivered only once"
    );

    // 5. Restart service: cursor persists from disk, previously delivered messages are not resent
    let (service2, terminals2, _clock2) = test_service(tmp.path());
    terminals2.set_status(
        &non_mcp.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: non_mcp.pane.clone(),
            tab: non_mcp.tab.clone(),
            title: non_mcp.label.clone(),
            seq: 11,
        },
    );
    terminals2.set_screen(&non_mcp.pane, "docs-shell$ ");

    service2.reconcile_once().unwrap();
    assert!(
        terminals2.prompts().is_empty(),
        "cursor must persist across service restart"
    );

    // Send a new message after restart -> only the new message is delivered
    let m5 = service2
        .bus()
        .unwrap()
        .send(Message::directed(
            "coder-1",
            non_mcp.id.clone(),
            "msg",
            "third instruction",
        ))
        .unwrap();

    let reg2 = service2.reconcile_once().unwrap();
    let sent2 = terminals2.prompts();
    assert_eq!(sent2.len(), 1);
    assert_eq!(sent2[0].1, "[shep] from coder#1: third instruction");
    assert_eq!(reg2.result(&non_mcp.id).unwrap().delivery_cursor, m5.id);
}

#[test]
fn req7_deliver_bus_messages_blocked_working_and_terminal_states() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());

    let non_mcp = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "docs".into(),
            task: "doc-task".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    terminals.set_screen(&non_mcp.pane, "shell$ ");

    let bus = service.bus().unwrap();
    let m1 = bus
        .send(Message::directed(
            "brain-1",
            non_mcp.id.clone(),
            "msg",
            "do not type yet",
        ))
        .unwrap();

    let initial_prompts_len = terminals.prompts().len();

    // 1. Working: pane is busy -> no delivery
    terminals.set_status(
        &non_mcp.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "working".into(),
            pane: non_mcp.pane.clone(),
            tab: non_mcp.tab.clone(),
            title: non_mcp.label.clone(),
            seq: 1,
        },
    );
    let reg = service.reconcile_once().unwrap();
    assert_eq!(terminals.prompts().len(), initial_prompts_len);
    assert_eq!(reg.result(&non_mcp.id).unwrap().delivery_cursor, 0);

    // 2. Blocked: trust dialog -> no delivery
    terminals.set_status(
        &non_mcp.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "blocked".into(),
            pane: non_mcp.pane.clone(),
            tab: non_mcp.tab.clone(),
            title: non_mcp.label.clone(),
            seq: 2,
        },
    );
    let reg = service.reconcile_once().unwrap();
    assert_eq!(terminals.prompts().len(), initial_prompts_len);
    assert_eq!(reg.result(&non_mcp.id).unwrap().delivery_cursor, 0);

    // 3. Released to idle -> delivery succeeds!
    terminals.set_status(
        &non_mcp.id,
        TerminalAgentStatus {
            state: "live".into(),
            status: "idle".into(),
            pane: non_mcp.pane.clone(),
            tab: non_mcp.tab.clone(),
            title: non_mcp.label.clone(),
            seq: 3,
        },
    );
    let reg = service.reconcile_once().unwrap();
    assert_eq!(terminals.prompts().len(), initial_prompts_len + 1);
    assert_eq!(reg.result(&non_mcp.id).unwrap().delivery_cursor, m1.id);

    // 4. Agent transitions to "done" -> further messages are NOT delivered
    let mut reg_file = service.load_registry().unwrap();
    reg_file.agents[0].state = "done".into();
    service.save_registry(&reg_file).unwrap();

    let _m2 = bus
        .send(Message::directed(
            "brain-1",
            non_mcp.id.clone(),
            "msg",
            "agent is already done",
        ))
        .unwrap();

    let sent_before = terminals.prompts().len();
    service.reconcile_once().unwrap();
    assert_eq!(
        terminals.prompts().len(),
        sent_before,
        "done agents must NOT receive messages"
    );

    // 5. Agent transitions to "exited" (gone) -> no delivery
    let mut reg_file = service.load_registry().unwrap();
    reg_file.agents[0].state = "exited".into();
    service.save_registry(&reg_file).unwrap();

    let _m3 = bus
        .send(Message::directed(
            "brain-1",
            non_mcp.id.clone(),
            "msg",
            "agent is exited",
        ))
        .unwrap();

    service.reconcile_once().unwrap();
    assert_eq!(
        terminals.prompts().len(),
        sent_before,
        "gone agents must NOT receive messages"
    );
}

#[test]
fn req7_deliver_bus_messages_retry_on_verification_failure() {
    let tmp = TestDir::new("test");
    let (service, terminals, _clock) = test_service(tmp.path());

    let non_mcp = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "docs".into(),
            task: "doc-task".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    let st = TerminalAgentStatus {
        state: "live".into(),
        status: "idle".into(),
        pane: non_mcp.pane.clone(),
        tab: non_mcp.tab.clone(),
        title: non_mcp.label.clone(),
        seq: 1,
    };
    terminals.set_status(&non_mcp.id, st.clone());
    terminals.set_status(&non_mcp.pane, st);
    terminals.set_screen(&non_mcp.pane, "shell$ ");

    let bus = service.bus().unwrap();
    let m1 = bus
        .send(Message::directed(
            "brain-1",
            non_mcp.id.clone(),
            "msg",
            "needs verification",
        ))
        .unwrap();

    // Force agent.prompt to report agent_not_ready so it falls back to raw pane typing + screen verification
    terminals.set_prompt_error(&non_mcp.pane, "agent_not_ready", "agent not ready");
    terminals.set_prompt_error(&non_mcp.id, "agent_not_ready", "agent not ready");

    // Disable echo on send_text so verification fails
    terminals.set_echo_on_send_text(false);

    // Reconcile tick runs, retries 3 times, records failure, does NOT fail tick
    let reg = service.reconcile_once().unwrap();
    assert_eq!(
        reg.result(&non_mcp.id).unwrap().delivery_cursor,
        0,
        "cursor must not advance if verification fails"
    );

    // Check service log has the failure recorded
    let log_content =
        std::fs::read_to_string(tmp.path().join("state").join("service.log")).unwrap_or_default();
    assert!(log_content.contains("was not verified on screen"));

    // Now enable echo on send_text and run next tick -> succeeds and advances cursor!
    terminals.set_echo_on_send_text(true);
    let reg = service.reconcile_once().unwrap();
    assert_eq!(
        reg.result(&non_mcp.id).unwrap().delivery_cursor,
        m1.id,
        "cursor must advance when verification succeeds"
    );
}

#[test]
fn test_handle_call_pattern_operations() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-test-pat-svc-{}-{nonce}",
        std::process::id()
    ));
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir_all(root.join(".shepherd")).unwrap();

    let terminals = InMemoryTerminals::new();
    let clock = ManualClock::new();
    let svc_cfg = ServiceConfig::for_state_dir(&state_dir);
    let shep_cfg = Config::default();
    let service =
        OrchestrationService::new(svc_cfg, shep_cfg, Arc::new(terminals), Arc::new(clock));

    let root_str = root.display().to_string();

    // 1. List patterns
    let list_val = service
        .handle_call("patterns", serde_json::json!({ "root": root_str }))
        .unwrap();
    let list = list_val.as_array().expect("array of patterns");
    assert_eq!(list.len(), 4);

    // 2. Pattern get
    let get_val = service
        .handle_call(
            "pattern_get",
            serde_json::json!({ "name": "orchestrator-worker", "root": root_str }),
        )
        .unwrap();
    assert_eq!(get_val["name"], "orchestrator-worker");
    assert_eq!(get_val["source"], "builtin");
    assert!(get_val["problems"].as_array().unwrap().is_empty());

    // 3. Pattern validate on object
    let valid_val = service
        .handle_call(
            "pattern_validate",
            serde_json::json!({ "name": "orchestrator-worker", "root": root_str }),
        )
        .unwrap();
    assert_eq!(valid_val["valid"], true);

    // 4. Pattern save (project)
    let mut custom_pat = get_val.clone();
    custom_pat["name"] = serde_json::json!("custom-test-orch");
    let save_val = service
        .handle_call(
            "pattern_save",
            serde_json::json!({ "pattern": custom_pat, "root": root_str }),
        )
        .unwrap();
    assert_eq!(save_val["ok"], true);
    assert_eq!(save_val["name"], "custom-test-orch");
    assert_eq!(save_val["source"], "project");

    // Now list should have 5 patterns
    let list_val2 = service
        .handle_call("patterns", serde_json::json!({ "root": root_str }))
        .unwrap();
    assert_eq!(list_val2.as_array().unwrap().len(), 5);

    // 5. Pattern delete
    let del_val = service
        .handle_call(
            "pattern_delete",
            serde_json::json!({ "name": "custom-test-orch", "root": root_str }),
        )
        .unwrap();
    assert_eq!(del_val["ok"], true);
    assert_eq!(del_val["deleted_source"], "project");

    // 6. Delete builtin should fail
    let del_builtin_err = service
        .handle_call(
            "pattern_delete",
            serde_json::json!({ "name": "orchestrator-worker", "root": root_str }),
        )
        .unwrap_err();
    assert!(del_builtin_err.contains("cannot delete built-in"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn test_format_shep_topic_message() {
    let registry = AgentRegistryFile {
        seq: 2,
        next: std::collections::HashMap::new(),
        agents: vec![Agent {
            id: "coder-1".into(),
            seq: 1,
            role: "coder".into(),
            ..Agent::default()
        }],
    };
    let msg = Message {
        id: 1,
        ts: "2026-11-05T00:00:00Z".into(),
        from: "coder-1".into(),
        to: String::new(),
        kind: "msg".into(),
        content: "feature implementation done\nall tests passing".into(),
        topic: "results".into(),
        key: "task-42".into(),
        partition: 2,
        offset: 5,
    };
    let formatted = format_shep_topic_message(&msg, &registry);
    assert_eq!(
        formatted,
        "[shep] topic results[2]#5 from coder#1: feature implementation done all tests passing"
    );
}

#[test]
fn test_handle_call_topic_operations() {
    let tmp = TestDir::new("test-topic-ops");
    let (service, _terminals, _clock) = test_service(tmp.path());

    // 1. topic_create
    let create_val = service
        .handle_call(
            "topic_create",
            serde_json::json!({
                "name": "orders",
                "partitions": 3
            }),
        )
        .unwrap();
    assert_eq!(create_val["name"], "orders");
    assert_eq!(create_val["partitions"], 3);

    // 2. topics list
    let topics_val = service
        .handle_call("topics", serde_json::json!({}))
        .unwrap();
    let topics = topics_val.as_array().expect("array of topics");
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0]["name"], "orders");
    assert_eq!(topics[0]["partitions"], 3);

    // 3. publish
    let pub_val = service
        .handle_call(
            "publish",
            serde_json::json!({
                "topic": "orders",
                "content": "process payment",
                "key": "user-99"
            }),
        )
        .unwrap();
    assert_eq!(pub_val["topic"], "orders");
    assert_eq!(pub_val["content"], "process payment");
    assert_eq!(pub_val["offset"], 0);

    // 4. topic_tail
    let tail_val = service
        .handle_call(
            "topic_tail",
            serde_json::json!({
                "topic": "orders"
            }),
        )
        .unwrap();
    let msgs = tail_val.as_array().expect("array of messages");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"], "process payment");

    // 5. consumer_groups
    let bus = service.bus().unwrap();
    bus.subscribe("coder-1", "orders", Some("workers")).unwrap();
    let groups_val = service
        .handle_call(
            "consumer_groups",
            serde_json::json!({
                "topic": "orders"
            }),
        )
        .unwrap();
    let groups = groups_val.as_array().expect("array of groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["group"], "workers");
    assert_eq!(groups[0]["members"], serde_json::json!(["coder-1"]));
}

#[test]
fn test_deliver_topic_messages_to_non_mcp_agent() {
    let tmp = TestDir::new("test-non-mcp-topics");
    let (service, terminals, _clock) = test_service(tmp.path());

    // Spawn non-MCP agent (docs)
    let non_mcp = service
        .spawn(ServiceSpawnRequest {
            root: tmp.path().to_path_buf(),
            role: "docs".into(),
            task: "docs-review".into(),
            ..ServiceSpawnRequest::default()
        })
        .unwrap();
    assert!(!non_mcp.mcp);

    let st = TerminalAgentStatus {
        state: "live".into(),
        status: "idle".into(),
        pane: non_mcp.pane.clone(),
        tab: non_mcp.tab.clone(),
        title: non_mcp.label.clone(),
        seq: 1,
    };
    terminals.set_status(&non_mcp.id, st.clone());
    terminals.set_status(&non_mcp.pane, st);
    terminals.set_screen(&non_mcp.pane, "shell$ ");

    // Set state to idle in registry
    let mut reg = service.load_registry().unwrap();
    reg.agents[0].state = "idle".into();
    service.save_registry(&reg).unwrap();

    // Initial prompt was sent during spawn
    let prompts_init = terminals.prompts();
    assert_eq!(prompts_init.len(), 1);

    // Subscribe docs-1 to topic "docs-tasks" in group "docs-pool"
    let bus = service.bus().unwrap();
    bus.subscribe(&non_mcp.id, "docs-tasks", Some("docs-pool"))
        .unwrap();

    // Publish event to "docs-tasks"
    bus.publish("docs-tasks", "document new API", None, Some(0), "brain-1")
        .unwrap();

    // Reconcile once -> delivers topic event via agent.prompt
    let _reg_after = service.reconcile_once().unwrap();

    let prompts = terminals.prompts();
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        prompts[1].1,
        "[shep] topic docs-tasks[0]#0 from brain#1: document new API"
    );

    // Next reconcile does NOT re-deliver because offset 0 was acked upon successful delivery
    service.reconcile_once().unwrap();
    assert_eq!(terminals.prompts().len(), 2);
}
