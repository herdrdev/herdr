//! Client glue of the snapshot timeline (F11c): turns timeline actions into typed
//! `shepherd.snapshot.*` requests and feeds the typed results back into the pure state.
//! Every request is asynchronous (`push_endpoint_method_with_kind`), so the UI never waits on git.

use super::*;
use crate::api::schema::{
    Method, ResponseResult, ShepherdSnapshotCaptureParams, ShepherdSnapshotDeleteParams,
    ShepherdSnapshotDiffParams, ShepherdSnapshotListParams, ShepherdSnapshotPruneParams,
    ShepherdSnapshotRestoreParams, ShepherdSnapshotShowParams,
};
use crate::shepherd::ui::snapshots::{DiffMode, ShepherdSnapshotsAction as Act};

fn opt(root: String) -> Option<String> {
    Some(root).filter(|r| !r.is_empty())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Request + the `op` tag that lets the result be matched to what was asked.
fn to_request(action: Act) -> (Method, String) {
    match action {
        Act::List { root, agent } => {
            let tag = format!("snapshot_list:{}", agent.as_deref().unwrap_or("*"));
            let all = agent.is_none();
            (
                Method::ShepherdSnapshotList(ShepherdSnapshotListParams { root, agent, all }),
                tag,
            )
        }
        Act::Show { root, id } => (
            Method::ShepherdSnapshotShow(ShepherdSnapshotShowParams {
                root: opt(root),
                id,
                full: false,
            }),
            "snapshot_show".into(),
        ),
        Act::Capture { root, agent } => (
            Method::ShepherdSnapshotCapture(ShepherdSnapshotCaptureParams {
                root,
                agent,
                reason: "manual".into(),
            }),
            "snapshot_capture".into(),
        ),
        Act::DiffPrevious { root, id } => (
            Method::ShepherdSnapshotShow(ShepherdSnapshotShowParams {
                root: opt(root),
                id: id.clone(),
                full: true,
            }),
            format!("snapshot_diff_prev:{id}"),
        ),
        Act::DiffCurrent { root, id } => (
            Method::ShepherdSnapshotDiff(ShepherdSnapshotDiffParams {
                root: opt(root),
                from: id.clone(),
                to: None,
                full: true,
            }),
            format!("snapshot_diff_cur:{id}"),
        ),
        Act::FilesVsCurrent { root, id } => (
            Method::ShepherdSnapshotDiff(ShepherdSnapshotDiffParams {
                root: opt(root),
                from: id.clone(),
                to: None,
                full: false,
            }),
            format!("snapshot_files:{id}"),
        ),
        Act::Restore { root, id, paths } => {
            let confirm = paths.is_empty();
            (
                Method::ShepherdSnapshotRestore(ShepherdSnapshotRestoreParams {
                    root: opt(root),
                    id,
                    paths,
                    confirm,
                    delete_extra: false,
                }),
                "snapshot_restore".into(),
            )
        }
        Act::Delete { root, id } => (
            Method::ShepherdSnapshotDelete(ShepherdSnapshotDeleteParams {
                root: opt(root),
                id,
            }),
            "snapshot_delete".into(),
        ),
        Act::Prune { root, agent, keep } => (
            Method::ShepherdSnapshotPrune(ShepherdSnapshotPruneParams {
                root,
                agent: Some(agent),
                keep: Some(keep),
            }),
            "snapshot_prune".into(),
        ),
    }
}

impl ClientShellState {
    pub(super) fn dispatch_shepherd_snapshots_action(
        &mut self,
        action: Act,
        outcome: &mut ClientShellInput,
    ) {
        let (method, op) = to_request(action);
        self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::ShepherdCall { op },
            outcome,
        );
    }

    /// Background refresh while the timeline is open (list + missing summary).
    pub(super) fn refresh_shepherd_snapshots(&mut self, outcome: &mut ClientShellInput) {
        let requests = match self.shepherd_section.as_mut() {
            Some(s) if s.active_tab == crate::shepherd::ui::state::ShepherdSectionTab::Agents => {
                s.agents.snapshots.refresh_requests()
            }
            _ => Vec::new(),
        };
        for r in requests {
            self.dispatch_shepherd_snapshots_action(r, outcome);
        }
    }

    pub(super) fn is_snapshot_result(result: &ResponseResult) -> bool {
        matches!(
            result,
            ResponseResult::ShepherdSnapshotCaptured { .. }
                | ResponseResult::ShepherdSnapshotList { .. }
                | ResponseResult::ShepherdSnapshotShown { .. }
                | ResponseResult::ShepherdSnapshotDiffed { .. }
                | ResponseResult::ShepherdSnapshotRestored { .. }
                | ResponseResult::ShepherdSnapshotDeleted { .. }
                | ResponseResult::ShepherdSnapshotPruned { .. }
        )
    }

    pub(super) fn handle_shepherd_snapshot_error(&mut self, op: &str, message: &str) {
        if let Some(section) = self.shepherd_section.as_mut() {
            if op == "snapshot_show" {
                section.agents.snapshots.ingest_summary_error();
            } else {
                section.agents.snapshots.ingest_error(message);
            }
        }
    }

    pub(super) fn handle_shepherd_snapshot_result(
        &mut self,
        op: &str,
        result: ResponseResult,
        outcome: &mut ClientShellInput,
    ) {
        let mut refresh = false;
        let Some(section) = self.shepherd_section.as_mut() else {
            return;
        };
        let snaps = &mut section.agents.snapshots;
        match result {
            ResponseResult::ShepherdSnapshotList { snapshots } => {
                let agent = op.strip_prefix("snapshot_list:").filter(|a| *a != "*");
                snaps.ingest_list(agent, snapshots, now_secs());
                // new selection may lack a summary
                if let Some(a) = snaps.summary_request() {
                    self.dispatch_shepherd_snapshots_action(a, outcome);
                }
            }
            ResponseResult::ShepherdSnapshotShown { result } => {
                if let Some(id) = op.strip_prefix("snapshot_diff_prev:") {
                    let id = id.to_string();
                    snaps.ingest_diff(&id, DiffMode::Previous, &result.diff);
                    snaps.ingest_show(result);
                } else {
                    snaps.ingest_show(result);
                }
            }
            ResponseResult::ShepherdSnapshotDiffed { diff } => {
                if let Some(id) = op.strip_prefix("snapshot_diff_cur:") {
                    snaps.ingest_diff(id, DiffMode::Current, &diff);
                } else if let Some(id) = op.strip_prefix("snapshot_files:") {
                    snaps.ingest_files(id, &diff);
                }
            }
            ResponseResult::ShepherdSnapshotCaptured { result } => {
                snaps.ingest_capture(&result);
                refresh = true;
            }
            ResponseResult::ShepherdSnapshotRestored { result } => {
                snaps.ingest_restore(&result);
                refresh = true;
            }
            ResponseResult::ShepherdSnapshotDeleted { result } => {
                snaps.ingest_delete(&result);
                refresh = true;
            }
            ResponseResult::ShepherdSnapshotPruned { result } => {
                snaps.ingest_prune(&result);
                refresh = true;
            }
            _ => {}
        }
        if refresh {
            self.refresh_shepherd_views(outcome);
        }
        outcome.repaint = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shepherd::panel::ShepherdAgentRow;
    use crate::shepherd::ui::snapshots::SnapSub;
    use shepherd_core::service::{OrchestrationService, ServiceConfig, SystemClock};
    use std::sync::Arc;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    struct Rig {
        dir: std::path::PathBuf,
        core: OrchestrationService,
        state: ClientShellState,
        outcome: ClientShellInput,
    }

    impl Rig {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("shep-ui-snap-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
            std::fs::write(dir.join("g.txt"), "g\n").unwrap();
            git(&dir, &["add", "."]);
            git(&dir, &["commit", "-q", "-m", "init"]);
            let core = OrchestrationService::new(
                ServiceConfig::for_state_dir(dir.join("state")),
                shepherd_core::config::Config::default(),
                Arc::new(shepherd_core::terminals::InMemoryTerminals::new()),
                Arc::new(SystemClock::new()),
            );
            let mut config = ClientShellConfig::from_config(&crate::config::Config::default());
            config.shepherd_enabled = true;
            let mut state = ClientShellState::new(config);
            let row = ShepherdAgentRow {
                id: "coder-1".into(),
                state: "working".into(),
                worktree: dir.display().to_string(),
                ..Default::default()
            };
            let sec = state.ensure_shepherd_section();
            sec.agents.agents = vec![row];
            sec.agents.snapshots.active_agents = vec!["coder-1".into()];
            Rig {
                dir,
                core,
                state,
                outcome: ClientShellInput::default(),
            }
        }

        fn snaps(&mut self) -> &mut crate::shepherd::ui::snapshots::ShepherdSnapshotsState {
            &mut self.state.ensure_shepherd_section().agents.snapshots
        }

        /// Send `action` to the real service and feed the answer back, like the endpoint would.
        fn drive(&mut self, action: Act) {
            let (method, op) = to_request(action);
            let raw = crate::shepherd::service::execute_shepherd_method(&self.core, "t", method);
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if let Some(err) = v.get("error") {
                let msg = err["message"].as_str().unwrap().to_string();
                self.state.handle_shepherd_snapshot_error(&op, &msg);
                return;
            }
            let result: ResponseResult = serde_json::from_value(v["result"].clone()).unwrap();
            assert!(ClientShellState::is_snapshot_result(&result), "{raw}");
            self.state
                .handle_shepherd_snapshot_result(&op, result, &mut self.outcome);
        }

        fn list(&mut self) {
            let a = self.snaps().list_request().unwrap();
            self.drive(a);
        }

        fn ids(&mut self) -> Vec<String> {
            self.snaps()
                .agent_snaps
                .iter()
                .map(|s| s.id.clone())
                .collect()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn requests_are_typed_and_tagged() {
        let (m, op) = to_request(Act::List {
            root: Some("/r".into()),
            agent: None,
        });
        assert_eq!(op, "snapshot_list:*");
        assert!(matches!(m, Method::ShepherdSnapshotList(ref p) if p.all && p.agent.is_none()));
        let (m, _) = to_request(Act::List {
            root: None,
            agent: Some("a".into()),
        });
        assert!(
            matches!(m, Method::ShepherdSnapshotList(ref p) if !p.all && p.agent.as_deref() == Some("a"))
        );
        let (m, op) = to_request(Act::DiffPrevious {
            root: "/r".into(),
            id: "a/2".into(),
        });
        assert_eq!(op, "snapshot_diff_prev:a/2");
        assert!(
            matches!(m, Method::ShepherdSnapshotShow(ref p) if p.full && p.root.as_deref() == Some("/r"))
        );
        let (m, _) = to_request(Act::DiffCurrent {
            root: String::new(),
            id: "a/2".into(),
        });
        assert!(
            matches!(m, Method::ShepherdSnapshotDiff(ref p) if p.full && p.to.is_none() && p.root.is_none())
        );
        // a full restore (no paths) is the only one that asks for --confirm
        let (m, _) = to_request(Act::Restore {
            root: "/r".into(),
            id: "a/1".into(),
            paths: vec![],
        });
        assert!(
            matches!(m, Method::ShepherdSnapshotRestore(ref p) if p.confirm && !p.delete_extra)
        );
        let (m, _) = to_request(Act::Restore {
            root: "/r".into(),
            id: "a/1".into(),
            paths: vec!["x".into()],
        });
        assert!(
            matches!(m, Method::ShepherdSnapshotRestore(ref p) if !p.confirm && p.paths == ["x"])
        );
        let (m, _) = to_request(Act::Prune {
            root: None,
            agent: "a".into(),
            keep: 3,
        });
        assert!(
            matches!(m, Method::ShepherdSnapshotPrune(ref p) if p.keep == Some(3) && p.agent.as_deref() == Some("a"))
        );
    }

    #[test]
    fn timeline_end_to_end_against_the_real_core_on_a_temp_repo() {
        let mut rig = Rig::new("e2e");
        let wt = rig.dir.display().to_string();

        // open: empty timeline, explained
        let sec = rig.state.ensure_shepherd_section();
        let open = sec.agents.open_timeline().unwrap();
        let crate::shepherd::ui::agents::ShepherdAgentsAction::Snapshots(open) = open else {
            panic!()
        };
        rig.drive(open);
        assert!(rig.ids().is_empty());

        // capture twice (second after an edit)
        let a = rig.snaps().start_capture().unwrap();
        assert_eq!(rig.snaps().busy.as_deref(), Some("capturando…"));
        rig.drive(a);
        assert!(!rig.snaps().is_busy());
        assert!(rig.snaps().notice.as_deref().unwrap().contains("coder-1/1"));
        // identical tree: nothing created, the panel says so
        let a = rig.snaps().start_capture().unwrap();
        rig.drive(a);
        assert!(rig
            .snaps()
            .notice
            .as_deref()
            .unwrap()
            .contains("Sin cambios"));
        std::fs::write(rig.dir.join("f.txt"), "v2\n").unwrap();
        let a = rig.snaps().start_capture().unwrap();
        rig.drive(a);
        rig.list();
        assert_eq!(rig.ids(), ["coder-1/2", "coder-1/1"], "newest first");
        assert_eq!(rig.snaps().selected, 0);

        // the list answer already asked (asynchronously) for the selection's summary
        assert_eq!(rig.snaps().summary_inflight.as_deref(), Some("coder-1/2"));
        assert!(
            rig.snaps().summary_request().is_none(),
            "not asked twice while in flight"
        );
        rig.snaps().summary_inflight = None;
        let a = rig.snaps().summary_request().unwrap();
        rig.drive(a);
        let sum = rig.snaps().summary_for_selected().unwrap().clone();
        assert_eq!(sum.files.len(), 1);
        assert_eq!(sum.files[0].path, "f.txt");
        let a = rig.snaps().start_diff(DiffMode::Previous).unwrap();
        assert_eq!(rig.snaps().busy.as_deref(), Some("cargando diff…"));
        rig.drive(a);
        let d = rig.snaps().diff_for_selected().unwrap().clone();
        assert!(d.lines.iter().any(|l| l == "+v2"), "{:?}", d.lines);
        assert!(d.lines.iter().any(|l| l == "-v1"));

        // diff against the current worktree
        std::fs::write(rig.dir.join("f.txt"), "v3\n").unwrap();
        std::fs::write(rig.dir.join("new.txt"), "n\n").unwrap();
        let a = rig.snaps().start_diff(DiffMode::Current).unwrap();
        rig.drive(a);
        let d = rig.snaps().diff_for_selected().unwrap().clone();
        assert_eq!(d.mode, DiffMode::Current);
        assert!(d.lines.iter().any(|l| l == "+v3"));

        // restore ONE file from #1 through the selector: g.txt/new.txt untouched, pre-restore saved
        rig.snaps().select(1);
        let a = rig.snaps().start_restore_file().unwrap();
        rig.drive(a);
        let SnapSub::Files(sel) = rig.snaps().sub.clone() else {
            panic!("selector expected: {:?}", rig.snaps().error)
        };
        let names: Vec<_> = sel.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, ["f.txt"], "new.txt exists only now: not offered");
        assert_eq!(sel.not_restorable, 1);
        if let SnapSub::Files(s) = &mut rig.snaps().sub {
            s.files[0].checked = true;
        }
        rig.snaps().submit_files();
        let a = rig.snaps().confirm().unwrap();
        rig.drive(a);
        assert_eq!(
            std::fs::read_to_string(rig.dir.join("f.txt")).unwrap(),
            "v1\n"
        );
        assert!(rig.dir.join("new.txt").exists());
        assert!(
            rig.snaps()
                .notice
                .as_deref()
                .unwrap()
                .contains("pre-restore")
                || rig.snaps().notice.as_deref().unwrap().contains("coder-1/")
        );
        rig.list();
        assert_eq!(rig.ids().len(), 3, "{:?}", rig.ids());
        assert_eq!(rig.snaps().agent_snaps[0].reason.as_str(), "pre-restore");

        // an invalid path comes back as an error shown in the panel, not a dialog
        rig.snaps().selected = 0;
        rig.drive(Act::Restore {
            root: wt.clone(),
            id: "coder-1/1".into(),
            paths: vec!["nope.txt".into()],
        });
        assert!(rig.snaps().error.as_deref().unwrap().contains("nope.txt"));
        assert!(!rig.snaps().is_busy());

        // restore everything (confirm) from #1
        rig.snaps().select(2);
        rig.snaps().start_restore_all();
        let a = rig.snaps().confirm().unwrap();
        rig.drive(a);
        assert_eq!(
            std::fs::read_to_string(rig.dir.join("f.txt")).unwrap(),
            "v1\n"
        );
        assert!(rig.dir.join("new.txt").exists(), "extra files are kept");
        rig.list();
        let n_before = rig.ids().len();

        // delete the selected one, then prune to the 1 newest
        rig.snaps().selected = 0;
        let victim = rig.snaps().selected_snapshot().unwrap().id.clone();
        rig.snaps().start_delete();
        let a = rig.snaps().confirm().unwrap();
        rig.drive(a);
        rig.list();
        assert_eq!(rig.ids().len(), n_before - 1);
        assert!(!rig.ids().contains(&victim));
        rig.snaps().start_prune();
        if let SnapSub::Prune(f) = &mut rig.snaps().sub {
            f.fields[0].value = "1".into();
        }
        let a = rig.snaps().submit_prune().unwrap();
        rig.drive(a);
        rig.list();
        assert_eq!(rig.ids().len(), 1);
        assert!(rig.snaps().notice.as_deref().unwrap().contains("Podados"));

        // finished-agents view: list the whole repo, the live agent is excluded, others appear
        std::fs::write(rig.dir.join("f.txt"), "old\n").unwrap();
        let (m, _) = to_request(Act::Capture {
            root: Some(wt.clone()),
            agent: "gone-7".into(),
        });
        let raw = crate::shepherd::service::execute_shepherd_method(&rig.core, "t", m);
        assert!(raw.contains("shepherd_snapshot_captured"), "{raw}");
        let a = rig
            .snaps()
            .set_source(crate::shepherd::ui::snapshots::SnapshotSource::Finished)
            .unwrap();
        rig.drive(a);
        let fin: Vec<_> = rig
            .snaps()
            .finished_snaps
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(fin, ["gone-7/1"]);
    }

    #[test]
    fn errors_for_snapshot_ops_land_in_the_panel_only() {
        let mut rig = Rig::new("err");
        let sec = rig.state.ensure_shepherd_section();
        sec.agents.open_timeline();
        // not a git worktree: the server error is shown in the timeline, the global status stays clean
        rig.drive(Act::List {
            root: Some("/definitely/not/here".into()),
            agent: Some("coder-1".into()),
        });
        assert!(rig.snaps().error.is_some());
        assert!(rig.state.ensure_shepherd_section().status_message.is_none());
    }
}
