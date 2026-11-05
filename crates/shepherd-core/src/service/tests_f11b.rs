//! F11b: automatic snapshots (state transitions, periodic, pre-kill/pre-clean, recovery).
use super::tests::{test_service, TestDir};
use super::*;
use crate::snapshot::{self, SnapshotInfo, SnapshotReason};
use crate::terminals::{InMemoryTerminals, TerminalAgentStatus};
use std::process::Command;
use std::sync::Once;

static ISOLATE: Once = Once::new();

fn git(dir: &Path, args: &[&str]) -> String {
    ISOLATE.call_once(|| {
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    });
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo(tmp: &TestDir, name: &str) -> PathBuf {
    let dir = tmp.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

fn status(terminals: &InMemoryTerminals, id: &str, st: &str) {
    let s = TerminalAgentStatus {
        state: "live".into(),
        status: st.into(),
        pane: format!("p-{id}"),
        tab: String::new(),
        title: String::new(),
        seq: 1,
    };
    terminals.set_status(id, s.clone());
    terminals.set_status(&format!("p-{id}"), s);
}

/// Register an MCP agent working in `worktree`; returns its id.
fn add_agent(service: &OrchestrationService, root: &Path, worktree: &Path, state: &str) -> String {
    let mut reg = service.load_registry().unwrap();
    let agent = Agent {
        role: "coder".into(),
        vendor: "codex".into(),
        root: root.to_string_lossy().into(),
        worktree: worktree.to_string_lossy().into(),
        mcp: true,
        ..Default::default()
    };
    let mut created = reg.create(agent, "tok".into(), "2026-04-20T00:00:00Z");
    created.pane = format!("p-{}", created.id);
    created.state = state.into();
    let id = created.id.clone();
    *reg.agents.iter_mut().find(|a| a.id == id).unwrap() = created;
    service.save_registry(&reg).unwrap();
    id
}

fn snaps(root: &Path, id: &str) -> Vec<SnapshotInfo> {
    snapshot::list(root, Some(id), true).unwrap()
}

fn reasons(root: &Path, id: &str) -> Vec<SnapshotReason> {
    snaps(root, id).iter().map(|s| s.reason).collect()
}

#[test]
fn idle_and_done_transitions_capture_and_unchanged_tree_does_not() {
    let tmp = TestDir::new("f11b-idle");
    let repo = init_repo(&tmp, "repo");
    let (service, terminals, _clock) = test_service(tmp.path());
    let id = add_agent(&service, &repo, &repo, "working");
    status(&terminals, &id, "working");
    service.reconcile_once().unwrap(); // first sight: nothing to compare with
    service.wait_snapshots_idle();
    assert!(snaps(&repo, &id).is_empty());

    std::fs::write(repo.join("b.txt"), "work\n").unwrap();
    status(&terminals, &id, "idle");
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(reasons(&repo, &id), vec![SnapshotReason::Idle]);

    // idle -> working -> idle with the same tree: nothing new.
    status(&terminals, &id, "working");
    service.reconcile_once().unwrap();
    status(&terminals, &id, "idle");
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(snaps(&repo, &id).len(), 1);

    // idle -> done with new work: reason done.
    std::fs::write(repo.join("b.txt"), "more work\n").unwrap();
    let mut reg = service.load_registry().unwrap();
    reg.submit_result(&id, "finished", "2026-04-20T00:00:05Z")
        .unwrap();
    service.save_registry(&reg).unwrap();
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(
        reasons(&repo, &id),
        vec![SnapshotReason::Idle, SnapshotReason::Done]
    );
}

#[test]
fn periodic_capture_after_interval_and_zero_disables() {
    let tmp = TestDir::new("f11b-periodic");
    let repo = init_repo(&tmp, "repo");
    let (mut service, terminals, clock) = test_service(tmp.path());
    service.shepherd_config.snapshots.interval_secs = 10;
    let id = add_agent(&service, &repo, &repo, "working");
    status(&terminals, &id, "working");
    service.reconcile_once().unwrap();
    std::fs::write(repo.join("b.txt"), "1\n").unwrap();
    clock.advance(Duration::from_secs(9));
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert!(snaps(&repo, &id).is_empty(), "before the interval");

    clock.advance(Duration::from_secs(2));
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(reasons(&repo, &id), vec![SnapshotReason::Periodic]);

    // The timer restarted: a new change is not captured until another interval passes.
    std::fs::write(repo.join("b.txt"), "2\n").unwrap();
    clock.advance(Duration::from_secs(5));
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(snaps(&repo, &id).len(), 1);
    clock.advance(Duration::from_secs(6));
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(snaps(&repo, &id).len(), 2);

    service.shepherd_config.snapshots.interval_secs = 0;
    std::fs::write(repo.join("b.txt"), "3\n").unwrap();
    clock.advance(Duration::from_secs(3600));
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(snaps(&repo, &id).len(), 2, "0 disables periodic captures");
}

#[test]
fn captures_never_overlap_and_a_stuck_one_expires() {
    let tmp = TestDir::new("f11b-overlap");
    let repo = init_repo(&tmp, "repo");
    let (service, terminals, _clock) = test_service(tmp.path());
    let id = add_agent(&service, &repo, &repo, "working");
    status(&terminals, &id, "working");
    service.reconcile_once().unwrap();
    std::fs::write(repo.join("b.txt"), "x\n").unwrap();

    service.mark_snapshot_inflight_for_test(&id, Duration::ZERO);
    status(&terminals, &id, "idle");
    service.reconcile_once().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        snaps(&repo, &id).is_empty(),
        "an in-flight capture blocks a second one"
    );

    // A capture older than the timeout is considered dead.
    status(&terminals, &id, "working");
    service.reconcile_once().unwrap();
    service.mark_snapshot_inflight_for_test(&id, Duration::from_secs(3600));
    status(&terminals, &id, "idle");
    service.reconcile_once().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(snaps(&repo, &id).len(), 1);
}

#[test]
fn kill_takes_pre_kill_snapshot_and_a_failing_capture_does_not_block_it() {
    let tmp = TestDir::new("f11b-kill");
    let repo = init_repo(&tmp, "repo");
    let (service, terminals, _clock) = test_service(tmp.path());
    let id = add_agent(&service, &repo, &repo, "working");
    status(&terminals, &id, "working");
    std::fs::write(repo.join("wip.txt"), "unsaved\n").unwrap();
    let killed = service.kill(&id).unwrap();
    assert_eq!(killed.state, "killed");
    assert!(killed.notice.is_empty(), "{}", killed.notice);
    assert_eq!(reasons(&repo, &id), vec![SnapshotReason::PreKill]);

    // Worktree that is not a git repo: the capture fails, the kill still happens, with a warning.
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let id2 = add_agent(&service, &repo, &plain, "working");
    status(&terminals, &id2, "working");
    let killed = service.kill(&id2).unwrap();
    assert_eq!(killed.state, "killed");
    assert!(killed.notice.contains("pre-kill"), "{}", killed.notice);
    assert!(terminals.closed().iter().any(|c| c.contains(&id2)));
    assert!(std::fs::read_to_string(service.service_config.log_path())
        .unwrap()
        .contains("snapshot"));
}

#[test]
fn clean_takes_pre_clean_snapshot_and_dead_agents_stay_listed() {
    let tmp = TestDir::new("f11b-clean");
    let repo = init_repo(&tmp, "repo");
    let linked = tmp.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent",
            linked.to_str().unwrap(),
        ],
    );
    let (service, terminals, _clock) = test_service(tmp.path());
    let id = add_agent(&service, &repo, &linked, "done");
    status(&terminals, &id, "idle");
    std::fs::write(linked.join("result.txt"), "final\n").unwrap();
    assert_eq!(
        service.clean(&[id.clone()], None).unwrap(),
        vec![id.clone()]
    );
    assert_eq!(reasons(&repo, &id), vec![SnapshotReason::PreClean]);

    // The worktree is gone but the snapshots live in the base repo.
    git(
        &repo,
        &["worktree", "remove", "--force", linked.to_str().unwrap()],
    );
    let all = snapshot::list(&repo, None, true).unwrap();
    assert!(all.iter().any(|s| s.agent == id), "{all:?}");
}

#[test]
fn restart_recovers_changes_and_notes_missing_worktrees() {
    let tmp = TestDir::new("f11b-resume");
    let repo = init_repo(&tmp, "repo");
    let (service, terminals, _clock) = test_service(tmp.path());
    let working = add_agent(&service, &repo, &repo, "working");
    status(&terminals, &working, "idle");
    std::fs::write(repo.join("lost.txt"), "before restart\n").unwrap();

    let done_repo = init_repo(&tmp, "repo-done");
    let done = add_agent(&service, &done_repo, &done_repo, "done");
    status(&terminals, &done, "idle");
    std::fs::write(done_repo.join("r.txt"), "r\n").unwrap();

    // An agent whose worktree vanished, with an older snapshot kept in the base repo.
    let gone_wt = tmp.path().join("vanished");
    let gone = add_agent(&service, &repo, &gone_wt, "working");
    status(&terminals, &gone, "idle");
    snapshot::capture(&repo, &gone, SnapshotReason::Manual, &Default::default()).unwrap();

    service.resume_on_startup().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(reasons(&repo, &working), vec![SnapshotReason::Idle]);
    assert_eq!(reasons(&done_repo, &done), vec![SnapshotReason::Done]);
    let log = std::fs::read_to_string(service.service_config.log_path()).unwrap();
    assert!(
        log.contains(&gone) && log.contains("worktree") && log.contains("1 snapshot"),
        "{log}"
    );
    assert_eq!(snaps(&repo, &gone).len(), 1, "old snapshots survive");

    // Restarting again with nothing changed adds nothing.
    service.resume_on_startup().unwrap();
    service.wait_snapshots_idle();
    assert_eq!(snaps(&repo, &working).len(), 1);
}

#[test]
fn automatic_captures_respect_max_per_agent_and_leave_the_user_repo_alone() {
    let tmp = TestDir::new("f11b-limits");
    let repo = init_repo(&tmp, "repo");
    let (mut service, terminals, _clock) = test_service(tmp.path());
    service.shepherd_config.snapshots.max_per_agent = 2;
    let id = add_agent(&service, &repo, &repo, "working");
    status(&terminals, &id, "working");
    service.reconcile_once().unwrap();
    std::fs::write(repo.join("staged.txt"), "s\n").unwrap();
    git(&repo, &["add", "staged.txt"]);
    std::fs::write(repo.join("b.txt"), "start\n").unwrap();
    let before = (
        git(&repo, &["status", "--porcelain"]),
        git(&repo, &["rev-parse", "HEAD"]),
        git(&repo, &["branch", "--list"]),
        std::fs::read(repo.join(".git/index")).unwrap(),
    );
    for i in 0..4 {
        std::fs::write(repo.join("b.txt"), format!("{i}\n")).unwrap();
        status(&terminals, &id, "idle");
        service.reconcile_once().unwrap();
        service.wait_snapshots_idle();
        status(&terminals, &id, "working");
        service.reconcile_once().unwrap();
    }
    let after = (
        git(&repo, &["status", "--porcelain"]),
        git(&repo, &["rev-parse", "HEAD"]),
        git(&repo, &["branch", "--list"]),
        std::fs::read(repo.join(".git/index")).unwrap(),
    );
    assert_eq!(before, after);
    let ids: Vec<_> = snaps(&repo, &id).into_iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![format!("{id}/3"), format!("{id}/4")]);
}

#[test]
fn agent_list_reports_snapshot_count_and_last_and_omits_them_when_zero() {
    let tmp = TestDir::new("f11b-list");
    let repo = init_repo(&tmp, "repo");
    let (service, _terminals, _clock) = test_service(tmp.path());
    let with = add_agent(&service, &repo, &repo, "idle");
    let without = add_agent(&service, &repo, &repo, "idle");
    let limits = Default::default();
    snapshot::capture(&repo, &with, SnapshotReason::Manual, &limits).unwrap();
    std::fs::write(repo.join("b.txt"), "x\n").unwrap();
    snapshot::capture(&repo, &with, SnapshotReason::Manual, &limits).unwrap();
    let agents = service.list_public_agents().unwrap();
    let a = agents.iter().find(|a| a.id == with).unwrap();
    assert_eq!(
        (a.snapshots, a.last_snapshot.as_str()),
        (2, format!("{with}/2").as_str())
    );
    let b = agents.iter().find(|a| a.id == without).unwrap();
    assert_eq!(b.snapshots, 0);
    let json = serde_json::to_value(b).unwrap();
    assert!(json.get("snapshots").is_none() && json.get("last_snapshot").is_none());
}
