use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::{App, AppState};
use crate::api::schema::PaneTarget;
use crate::layout::PaneId;

const REAP_LOG_FILE_NAME: &str = "reap-log.jsonl";
const STRICT_THRESHOLD_STEP: Duration = Duration::from_nanos(1);

#[derive(Debug, Clone)]
struct DonePaneTarget {
    pane_id: PaneId,
    ws_idx: usize,
    done_for: Duration,
    workspace: String,
    workspace_id: String,
    agent_kind: String,
    worktree: Option<crate::workspace::WorktreeSpaceMembership>,
    closes_workspace: bool,
}

#[derive(Debug, Serialize)]
struct ReapLogRecord {
    timestamp: u64,
    pane_id: String,
    workspace: String,
    workspace_id: String,
    agent_kind: String,
    done_for_seconds: u64,
    worktree: Option<PathBuf>,
    worktree_removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl AppState {
    pub(crate) fn next_done_hide_deadline(&self, now: Instant) -> Option<Instant> {
        self.done_panes()
            .filter_map(|(_, _, _, pane, terminal)| {
                pane_is_done(pane, terminal)
                    .then_some(pane.done_since)
                    .flatten()
            })
            .filter_map(|done_since| strict_deadline(done_since, self.hide_done_after))
            .filter(|deadline| *deadline > now)
            .min()
    }

    pub(crate) fn done_hide_transition_due(&self, now: Instant) -> bool {
        self.done_panes().any(|(_, _, _, pane, terminal)| {
            if !pane_is_done(pane, terminal) {
                return false;
            }
            pane.done_since
                .and_then(|done_since| strict_deadline(done_since, self.hide_done_after))
                .is_some_and(|deadline| self.view_observed_at < deadline && now >= deadline)
        })
    }

    pub(crate) fn next_done_reap_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.reap_done_panes {
            return None;
        }
        self.done_panes()
            .filter_map(|(ws_idx, tab_idx, pane_id, pane, terminal)| {
                (!self.is_active_pane(ws_idx, tab_idx, pane_id)
                    && !self.workspaces[ws_idx].tabs[tab_idx].pinned)
                    .then(|| pane_reap_age(pane, terminal, now))
                    .flatten()
            })
            .filter_map(|reap_age| strict_age_deadline(now, reap_age, self.reap_done_after))
            .min()
    }

    /// When the next quiet pane becomes eligible for the settle trigger. The
    /// settlement pass itself runs on every tick, so without this the wake that
    /// carries it would depend on unrelated timers.
    pub(crate) fn next_done_settle_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.auto_settle_done {
            return None;
        }
        self.done_panes()
            .filter(|(ws_idx, tab_idx, pane_id, pane, terminal)| {
                pane.settled_at.is_none()
                    && pane.snoozed_until().is_none()
                    && pane_is_quiet(pane, terminal)
                    && !self.is_active_pane(*ws_idx, *tab_idx, *pane_id)
                    && !self.workspaces[*ws_idx].tabs[*tab_idx].pinned
            })
            .filter_map(|(_, _, _, pane, _)| {
                pane.activity.quiet_deadline_after(self.settle_done_after)
            })
            .map(|deadline| deadline.max(now))
            .min()
    }

    fn due_done_pane_ids(&self, now: Instant) -> Vec<PaneId> {
        if !self.reap_done_panes {
            return Vec::new();
        }
        self.done_panes()
            .filter_map(|(ws_idx, tab_idx, pane_id, pane, terminal)| {
                let reap_age = pane_reap_age(pane, terminal, now)?;
                (reap_age > self.reap_done_after
                    && !self.is_active_pane(ws_idx, tab_idx, pane_id)
                    && !self.workspaces[ws_idx].tabs[tab_idx].pinned)
                    .then_some(pane_id)
            })
            .collect()
    }

    fn done_pane_target_at(&self, pane_id: PaneId, now: Instant) -> Option<DonePaneTarget> {
        let (ws_idx, workspace) = self
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, workspace)| workspace.pane_state(pane_id).is_some())?;
        let tab_idx = workspace.find_tab_index_for_pane(pane_id)?;
        let tab = workspace.tabs.get(tab_idx)?;
        let pane = tab.panes.get(&pane_id)?;
        let terminal = self.terminals.get(&pane.attached_terminal_id)?;
        let done_for = pane_reap_age(pane, terminal, now)?;
        if done_for <= self.reap_done_after
            || self.is_active_pane(ws_idx, tab_idx, pane_id)
            || tab.pinned
        {
            return None;
        }

        Some(DonePaneTarget {
            pane_id,
            ws_idx,
            done_for,
            workspace: workspace.display_name_from_terminals(&self.terminals),
            workspace_id: workspace.id.clone(),
            agent_kind: terminal
                .effective_agent_label()
                .unwrap_or("unknown")
                .to_string(),
            worktree: workspace.worktree_space().cloned(),
            closes_workspace: self.close_pane_would_close_workspace(ws_idx, pane_id),
        })
    }

    fn done_panes(
        &self,
    ) -> impl Iterator<
        Item = (
            usize,
            usize,
            PaneId,
            &crate::pane::PaneState,
            &crate::terminal::TerminalState,
        ),
    > {
        let terminals = &self.terminals;
        self.workspaces
            .iter()
            .enumerate()
            .flat_map(move |(ws_idx, workspace)| {
                workspace
                    .tabs
                    .iter()
                    .enumerate()
                    .flat_map(move |(tab_idx, tab)| {
                        tab.panes.iter().filter_map(move |(pane_id, pane)| {
                            terminals
                                .get(&pane.attached_terminal_id)
                                .map(|terminal| (ws_idx, tab_idx, *pane_id, pane, terminal))
                        })
                    })
            })
    }
}

impl App {
    pub(crate) fn reap_due_done_panes(&mut self, now: Instant) -> bool {
        let pane_ids = self.state.due_done_pane_ids(now);
        let mut reaped = false;
        for pane_id in pane_ids {
            let Some(target) = self.state.done_pane_target_at(pane_id, now) else {
                continue;
            };
            let Some(public_pane_id) = self.public_pane_id(target.ws_idx, target.pane_id) else {
                continue;
            };
            let done_for_seconds = target.done_for.as_secs();
            let target_param = PaneTarget {
                pane_id: public_pane_id.clone(),
            };
            if let Err(error) =
                self.close_pane_for_reap("pane_lifecycle.reap".into(), &target_param)
            {
                tracing::warn!(pane = %public_pane_id, %error, "failed to reap Done pane");
                continue;
            }

            let (worktree, worktree_removed, reason) =
                cleanup_reaped_worktree(target.worktree.as_ref(), target.closes_workspace);
            let record = ReapLogRecord {
                timestamp: unix_timestamp_seconds(),
                pane_id: public_pane_id.clone(),
                workspace: target.workspace,
                workspace_id: target.workspace_id,
                agent_kind: target.agent_kind,
                done_for_seconds,
                worktree,
                worktree_removed,
                reason,
            };
            if let Err(error) = append_reap_log(&reap_log_path(), &record) {
                tracing::warn!(pane = %public_pane_id, %error, "failed to append pane reap log");
            }
            reaped = true;
        }
        reaped
    }
}

pub(crate) fn pane_is_done(
    pane: &crate::pane::PaneState,
    terminal: &crate::terminal::TerminalState,
) -> bool {
    if pane.settled_at.is_some() || pane.snoozed_until().is_some() {
        return false;
    }
    let (state, seen) = terminal.sidebar_projection(pane.seen);
    if seen
        || terminal.supervisor_stale
        || terminal.raw_agent_state() == crate::detect::AgentState::Working
    {
        return false;
    }
    // Idle is not enough to close a pane. A latched human-action closing item,
    // a live sub-agent or a shell still running below the agent all mean the
    // pane has something left in it. Share the sidebar's definition so a pane
    // painted as blocking can never be reaped underneath it.
    crate::terminal::state::session_is_quiet(
        state,
        terminal.has_pending_human_input() || terminal.usage_limited,
        terminal.effective_active_subagents(),
        terminal.holds_shell,
    )
}

pub(crate) fn pane_is_quiet(
    pane: &crate::pane::PaneState,
    terminal: &crate::terminal::TerminalState,
) -> bool {
    if terminal.raw_agent_state() == crate::detect::AgentState::Working
        || (terminal.supervisor_stale && terminal.stale_resolution.is_none())
    {
        return false;
    }
    let state = terminal.sidebar_projection(pane.seen).0;
    crate::terminal::state::session_is_quiet(
        state,
        terminal.has_pending_human_input() || terminal.usage_limited,
        terminal.effective_active_subagents(),
        terminal.holds_shell,
    )
}

fn pane_reap_age(
    pane: &crate::pane::PaneState,
    terminal: &crate::terminal::TerminalState,
    now: Instant,
) -> Option<Duration> {
    if pane_is_done(pane, terminal) {
        return pane
            .done_since
            .map(|done_since| now.saturating_duration_since(done_since));
    }
    (pane.settled_at.is_some()
        && !crate::app::settled::pane_has_resume_plan(terminal)
        && pane_is_quiet(pane, terminal))
    .then(|| pane.activity.quiet_for(now))
    .flatten()
}

fn strict_deadline(done_since: Instant, threshold: Duration) -> Option<Instant> {
    done_since
        .checked_add(threshold)
        .and_then(|deadline| deadline.checked_add(STRICT_THRESHOLD_STEP))
}

fn strict_age_deadline(now: Instant, age: Duration, threshold: Duration) -> Option<Instant> {
    if age > threshold {
        return Some(now);
    }
    now.checked_add(threshold.saturating_sub(age))?
        .checked_add(STRICT_THRESHOLD_STEP)
}

fn cleanup_reaped_worktree(
    membership: Option<&crate::workspace::WorktreeSpaceMembership>,
    closes_workspace: bool,
) -> (Option<PathBuf>, bool, Option<String>) {
    let Some(membership) = membership.filter(|membership| membership.is_linked_worktree) else {
        return (None, false, Some("no_worktree".into()));
    };
    let path = membership.checkout_path.clone();
    if !closes_workspace {
        return (Some(path), false, Some("skipped_in_use".into()));
    }
    if !matches!(
        crate::worktree::checkout_is_clean_and_pushed(&path),
        Ok(true)
    ) {
        return (Some(path), false, Some("skipped_dirty".into()));
    }

    let command = crate::worktree::build_worktree_remove_command(
        &membership.repo_root,
        &membership.checkout_path,
        false,
        false,
    );
    match crate::worktree::run_worktree_remove_command_with_recovery(
        &command,
        &membership.repo_root,
        &membership.checkout_path,
        false,
        false,
    ) {
        Ok(()) => (Some(path), true, None),
        Err(error) => {
            tracing::warn!(worktree = %path.display(), %error, "failed to remove reaped pane worktree");
            (Some(path), false, Some("remove_failed".into()))
        }
    }
}

fn reap_log_path() -> PathBuf {
    crate::config::state_dir().join(REAP_LOG_FILE_NAME)
}

fn append_reap_log(path: &Path, record: &ReapLogRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    line.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)
}

fn unix_timestamp_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Agent;
    use crate::detect::AgentState;
    use crate::workspace::{Workspace, WorktreeSpaceMembership};

    fn lifecycle_state(state: AgentState, seen: bool, done_since: Instant) -> (AppState, PaneId) {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("lifecycle")];
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_raw_agent_state_for_test(state);
        let pane = app.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        pane.seen = seen;
        pane.done_since = Some(done_since);
        app.reap_done_after = Duration::from_secs(4 * 60 * 60);
        (app, pane_id)
    }

    fn elapsed_test_clock(elapsed: Duration) -> (Instant, Instant) {
        let since = Instant::now();
        (since, since + elapsed)
    }

    #[test]
    fn never_reaps_a_pane_that_still_has_live_work() {
        for label in ["subagents", "shell", "usage"] {
            let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
            let (mut app, pane_id) = lifecycle_state(AgentState::Idle, false, done_since);
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            match label {
                "subagents" => terminal.active_subagents = Some(3),
                "shell" => terminal.holds_shell = true,
                _ => terminal.usage_limited = true,
            }
            let due = app.due_done_pane_ids(now);
            assert!(due.is_empty(), "{label}: still-live pane reaped: {due:?}");
        }
    }

    #[test]
    fn never_reaps_stale_working_even_when_stale_resolution_says_idle() {
        let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        let (mut app, pane_id) = lifecycle_state(AgentState::Working, false, done_since);
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal");
        terminal.supervisor_stale = true;
        terminal.stale_resolution = Some((AgentState::Idle, true));
        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("pane")
            .settled_at = Some(1);

        let pane = app.workspaces[0].pane_state(pane_id).expect("pane state");
        assert!(!pane_is_quiet(pane, terminal));
        assert!(app.due_done_pane_ids(now).is_empty());
    }

    #[test]
    fn never_reaps_a_pane_holding_an_unanswered_gate() {
        let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        let (mut app, pane_id) = lifecycle_state(AgentState::Idle, false, done_since);
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.closing_gates = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "Gate".into(),
            text: "Choose the release path".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let pane = app.workspaces[0].pane_state(pane_id).expect("pane state");
        assert!(pane.agent_projection(terminal).counts_as_blocked());
        let due = app.due_done_pane_ids(now);
        assert!(
            due.is_empty(),
            "a pane waiting on a human gate must not be reaped: {due:?}"
        );
    }

    #[test]
    fn never_reaps_a_pane_holding_an_unanswered_closing_item() {
        let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        let (mut app, pane_id) = lifecycle_state(AgentState::Idle, false, done_since);
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "Answer".into(),
            text: "Choose the release path".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let pane = &app.workspaces[0].tabs[0].panes[&pane_id];
        assert!(!pane_is_done(pane, terminal));
        assert!(!pane_is_quiet(pane, terminal));
        let due = app.due_done_pane_ids(now);
        assert!(
            due.is_empty(),
            "a pane waiting on a human answer must not be reaped: {due:?}"
        );
    }

    #[test]
    fn settled_pane_is_not_done_or_reaped() {
        let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        let (mut app, pane_id) = lifecycle_state(AgentState::Idle, false, done_since);
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("settled-session")
                    .expect("valid session id"),
            });
        let pane = app.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        pane.settled_at = Some(1_725_000_021);

        assert!(!pane_is_done(pane, &app.terminals[&terminal_id]));
        assert!(app.due_done_pane_ids(now).is_empty());
    }

    #[test]
    fn settled_unresumable_quiet_pane_stays_on_the_reap_clock() {
        let (quiet_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        let (mut app, pane_id) = lifecycle_state(AgentState::Idle, true, quiet_since);
        let pane = app.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.done_since = None;
        pane.settled_at = Some(1_725_000_022);
        pane.activity.set_last_at(quiet_since);
        pane.activity.observe_quiet(true, quiet_since);

        assert_eq!(app.due_done_pane_ids(now), vec![pane_id]);
    }

    #[test]
    fn restored_unresumable_settled_pane_reaps_with_an_unrepresentable_quiet_age() {
        let now = Instant::now();
        let (mut app, pane_id) = lifecycle_state(AgentState::Idle, true, now);
        let pane = app.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.done_since = None;
        pane.settled_at = Some(1_725_000_023);
        pane.activity.restore_unix_timestamp_at(0, now, u64::MAX);
        pane.activity
            .restore_quiet_unix_timestamp_at(0, now, u64::MAX);

        assert_eq!(app.next_done_reap_deadline(now), Some(now));
        assert_eq!(app.due_done_pane_ids(now), vec![pane_id]);
    }

    #[test]
    fn reap_threshold_is_strictly_greater_than_four_hours() {
        let done_since = Instant::now();
        let (app, pane_id) = lifecycle_state(AgentState::Idle, false, done_since);
        assert!(app
            .due_done_pane_ids(done_since + Duration::from_secs(4 * 60 * 60))
            .is_empty());
        assert_eq!(
            app.due_done_pane_ids(
                done_since + Duration::from_secs(4 * 60 * 60) + Duration::from_nanos(1)
            ),
            vec![pane_id]
        );
    }

    #[test]
    fn never_reaps_working_blocked_or_unknown_statuses() {
        let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        for state in [
            AgentState::Working,
            AgentState::Blocked,
            AgentState::Unknown,
        ] {
            let (app, _) = lifecycle_state(state, false, done_since);
            assert!(app.due_done_pane_ids(now).is_empty(), "{state:?}");
        }
    }

    #[test]
    fn never_reaps_the_currently_focused_pane() {
        let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        let (mut app, _) = lifecycle_state(AgentState::Idle, false, done_since);
        app.active = Some(0);
        assert!(app.due_done_pane_ids(now).is_empty());
    }

    #[test]
    fn never_reaps_a_pane_in_a_pinned_tab() {
        let (done_since, now) = elapsed_test_clock(Duration::from_secs(5 * 60 * 60));
        let (mut app, _) = lifecycle_state(AgentState::Idle, false, done_since);
        app.workspaces[0].tabs[0].pinned = true;
        assert!(app.due_done_pane_ids(now).is_empty());
    }

    #[test]
    fn clean_pushed_worktree_is_removed() {
        let fixture = pushed_worktree("clean");
        let (path, removed, reason) = cleanup_reaped_worktree(Some(&fixture.membership), true);
        assert_eq!(path.as_deref(), Some(fixture.checkout.as_path()));
        assert!(removed);
        assert_eq!(reason, None);
        assert!(!fixture.checkout.exists());
        fixture.cleanup();
    }

    #[test]
    fn dirty_worktree_is_kept_and_logged_as_skipped_dirty() {
        let fixture = pushed_worktree("dirty");
        std::fs::write(fixture.checkout.join("untracked.txt"), "dirty\n").unwrap();
        let (path, removed, reason) = cleanup_reaped_worktree(Some(&fixture.membership), true);
        assert_eq!(path.as_deref(), Some(fixture.checkout.as_path()));
        assert!(!removed);
        assert_eq!(reason.as_deref(), Some("skipped_dirty"));
        assert!(fixture.checkout.exists());
        fixture.cleanup();
    }

    #[test]
    fn unpushed_worktree_is_kept_and_logged_as_skipped_dirty() {
        let fixture = pushed_worktree("unpushed");
        run_git(
            &fixture.checkout,
            &["config", "user.email", "test@example.com"],
        );
        run_git(&fixture.checkout, &["config", "user.name", "Herdr Test"]);
        std::fs::write(fixture.checkout.join("unpushed.txt"), "unpushed\n").unwrap();
        run_git(&fixture.checkout, &["add", "unpushed.txt"]);
        run_git(&fixture.checkout, &["commit", "-m", "unpushed"]);

        let (path, removed, reason) = cleanup_reaped_worktree(Some(&fixture.membership), true);
        assert_eq!(path.as_deref(), Some(fixture.checkout.as_path()));
        assert!(!removed);
        assert_eq!(reason.as_deref(), Some("skipped_dirty"));
        assert!(fixture.checkout.exists());
        fixture.cleanup();
    }

    #[test]
    fn reap_log_appends_one_well_formed_json_object_per_line() {
        let root = unique_temp_path("reap-log");
        let path = root.join(REAP_LOG_FILE_NAME);
        let record = ReapLogRecord {
            timestamp: 1_788_000_000,
            pane_id: "w1:p2".into(),
            workspace: "herdr".into(),
            workspace_id: "w1".into(),
            agent_kind: "codex".into(),
            done_for_seconds: 14_401,
            worktree: Some(PathBuf::from("/tmp/herdr-worktree")),
            worktree_removed: false,
            reason: Some("skipped_dirty".into()),
        };

        append_reap_log(&path, &record).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(contents.trim_end()).unwrap();
        assert_eq!(parsed["pane_id"], "w1:p2");
        assert_eq!(parsed["worktree_removed"], false);
        assert_eq!(parsed["reason"], "skipped_dirty");
        std::fs::remove_dir_all(root).unwrap();
    }

    struct WorktreeFixture {
        root: PathBuf,
        repo: PathBuf,
        checkout: PathBuf,
        membership: WorktreeSpaceMembership,
    }

    impl WorktreeFixture {
        fn cleanup(self) {
            if self.checkout.exists() {
                let remove = crate::worktree::build_worktree_remove_command(
                    &self.repo,
                    &self.checkout,
                    true,
                    false,
                );
                let _ = crate::worktree::run_worktree_command(&remove);
            }
            std::fs::remove_dir_all(self.root).unwrap();
        }
    }

    fn pushed_worktree(label: &str) -> WorktreeFixture {
        let root = unique_temp_path(&format!("reap-worktree-{label}"));
        let remote = root.join("remote.git");
        let repo = root.join("repo");
        let checkout = root.join("checkout");
        std::fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "--bare", remote.to_str().unwrap()]);
        run_git(
            &root,
            &["clone", remote.to_str().unwrap(), repo.to_str().unwrap()],
        );
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Herdr Test"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-m", "fixture"]);
        let branch = format!("worktree/{label}");
        run_git(&repo, &["branch", &branch]);
        run_git(&repo, &["push", "-u", "origin", &branch]);
        run_git(
            &repo,
            &["worktree", "add", checkout.to_str().unwrap(), &branch],
        );
        let membership = WorktreeSpaceMembership {
            key: "fixture".into(),
            label: "fixture".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        };
        WorktreeFixture {
            root,
            repo,
            checkout,
            membership,
        }
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("herdr-{label}-{}-{nonce}", std::process::id()))
    }
}
