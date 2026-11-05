use super::*;
use crate::shepherd::overlay::ShepherdActionDispatcher;
use std::path::Path;

fn shepherd_op_to_method(op: &str, args: serde_json::Value) -> crate::api::schema::Method {
    use crate::api::schema::*;
    match op {
        "agents" => Method::ShepherdAgentList(ShepherdAgentListParams {}),
        "spawn" => {
            let params: ShepherdAgentSpawnParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdAgentSpawn(params)
        }
        "kill" => {
            let id = args
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Method::ShepherdAgentKill(ShepherdAgentTargetParams { id })
        }
        "clean" => {
            let params: ShepherdAgentCleanParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdAgentClean(params)
        }
        "focus" => {
            let id = args
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Method::ShepherdAgentFocus(ShepherdAgentTargetParams { id })
        }
        "reconcile" => Method::ShepherdAgentReconcile(ShepherdEmptyParams {}),
        "resume" => Method::ShepherdAgentResume(ShepherdEmptyParams {}),
        "tasks" => {
            let params: ShepherdTaskListParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTaskList(params)
        }
        "task_create" | "create_task" => {
            let params: ShepherdTaskCreateParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTaskCreate(params)
        }
        "task_archive" | "archive_task" => {
            let params: ShepherdTaskArchiveParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTaskArchive(params)
        }
        "runs" => {
            let params: ShepherdRunListParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdRunList(params)
        }
        "run" => {
            let params: ShepherdRunStartParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdRunStart(params)
        }
        "approve" => {
            let params: ShepherdGateActionParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdRunApprove(params)
        }
        "reject" | "reject_gate" => {
            let params: ShepherdGateRejectParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdRunReject(params)
        }
        "skills" => {
            let params: ShepherdSkillListParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdSkillList(params)
        }
        "git_status" => {
            let params: ShepherdGitStatusParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdGitStatus(params)
        }
        "patterns" => {
            let params: ShepherdPatternListParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternList(params)
        }
        "pattern_get" => {
            let params: ShepherdPatternGetParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternGet(params)
        }
        "pattern_save" => {
            let params: ShepherdPatternSaveParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternSave(params)
        }
        "pattern_delete" => {
            let params: ShepherdPatternDeleteParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternDelete(params)
        }
        "pattern_validate" => {
            let params: ShepherdPatternValidateParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternValidate(params)
        }
        "topics" => Method::ShepherdTopicList(ShepherdEmptyParams {}),
        "topic_create" => {
            let params: ShepherdTopicCreateParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTopicCreate(params)
        }
        "publish" => {
            let params: ShepherdTopicPublishParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTopicPublish(params)
        }
        "topic_tail" => {
            let params: ShepherdTopicTailParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTopicTail(params)
        }
        "topic_replay" => {
            let params: ShepherdTopicReplayParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTopicReplay(params)
        }
        "consumer_groups" => {
            let params: ShepherdTopicGroupsParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdTopicGroups(params)
        }
        _ => Method::ShepherdCall(ShepherdCallParams {
            op: op.to_string(),
            args,
        }),
    }
}

struct ClientActionDispatcher<'a> {
    state: &'a mut ClientShellState,
    outcome: &'a mut ClientShellInput,
}

impl<'a> crate::shepherd::ShepherdActionDispatcher for ClientActionDispatcher<'a> {
    fn dispatch_call(&mut self, op: &str, args: serde_json::Value) {
        let method = shepherd_op_to_method(op, args);
        self.state.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::ShepherdCall { op: op.to_string() },
            self.outcome,
        );
    }

    fn focus_pane(&mut self, pane_id: &str, _agent_id: &str) {
        self.state.push_endpoint_method(
            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                pane_id: pane_id.to_string(),
            }),
            self.outcome,
        );
    }

    fn open_editor(&mut self, _task_id: u32, file_path: &Path) {
        let cwd = file_path.parent().map(|p| p.to_string_lossy().to_string());
        self.state.push_endpoint_method(
            crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd,
                focus: true,
                right_click: Default::default(),
                env: Default::default(),
            }),
            self.outcome,
        );
    }
}

impl ClientShellState {
    pub(super) fn toggle_shepherd_section(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        if self.shepherd_section.is_some() {
            self.shepherd_section = None;
            self.invalidate_pane_surface();
        } else {
            self.shepherd_section =
                Some(crate::shepherd::ui::state::ShepherdSectionState::default());
            self.refresh_shepherd_views(outcome);
        }
        outcome.repaint = true;
    }

    pub(super) fn open_shepherd_tab_overlay(
        &mut self,
        tab: crate::shepherd::overlay::ShepherdTab,
        outcome: &mut ClientShellInput,
    ) {
        if !self.config.shepherd_enabled {
            return;
        }
        let mut overlay = crate::shepherd::ClientShepherdOverlay::default();
        overlay.select_tab(tab);
        self.overlay = Some(ClientShellOverlay::Shepherd(overlay));
        self.refresh_shepherd_views(outcome);
        outcome.repaint = true;
    }

    pub(super) fn route_shepherd_section_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ref mut section) = self.shepherd_section else {
            return false;
        };
        let key_event = crossterm::event::KeyEvent::new(key.code, key.modifiers);
        let section_outcome = section.handle_key(&key_event);
        self.dispatch_shepherd_section_outcome(section_outcome, outcome);
        true
    }

    pub(super) fn route_shepherd_section_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ref mut section) = self.shepherd_section else {
            return false;
        };
        let Some(hits) = self.hits.shepherd_section_hits.clone() else {
            return false;
        };
        // If sidebar is visible and click is on the sidebar, do not intercept
        if !self.sidebar_collapsed && point.0 < self.sidebar_width {
            return false;
        }
        if super::contains(self.hits.sidebar_toggle, point) {
            return false;
        }
        let section_outcome =
            crate::shepherd::ui::handle_shepherd_section_mouse(section, &hits, mouse);
        self.dispatch_shepherd_section_outcome(section_outcome, outcome);
        true
    }

    pub(super) fn dispatch_shepherd_section_outcome(
        &mut self,
        section_outcome: crate::shepherd::ui::state::ShepherdSectionOutcome,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::ui::state::{ShepherdSectionAction, ShepherdSectionOutcome};
        match section_outcome {
            ShepherdSectionOutcome::Close => {
                self.shepherd_section = None;
                self.invalidate_pane_surface();
                outcome.repaint = true;
            }
            ShepherdSectionOutcome::Handled => {
                outcome.repaint = true;
            }
            ShepherdSectionOutcome::Action(action) => {
                match action {
                    ShepherdSectionAction::Tasks(tasks_action) => {
                        self.dispatch_shepherd_tasks_action(tasks_action, outcome);
                    }
                    ShepherdSectionAction::Runs(runs_action) => {
                        self.dispatch_shepherd_runs_action(runs_action, outcome);
                    }
                    ShepherdSectionAction::OpenLegacyOverlay(tab) => {
                        self.open_shepherd_tab_overlay(tab, outcome);
                    }
                }
                outcome.repaint = true;
            }
            ShepherdSectionOutcome::Ignored => {}
        }
    }

    pub(super) fn dispatch_shepherd_tasks_action(
        &mut self,
        action: crate::shepherd::tasks::ShepherdTasksAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::tasks::ShepherdTasksAction;
        match action {
            ShepherdTasksAction::OpenEditor { task_id, file_path } => {
                let mut d = ClientActionDispatcher {
                    state: self,
                    outcome,
                };
                d.open_editor(task_id, &file_path);
            }
            ShepherdTasksAction::CreateTask {
                title,
                description,
                criteria,
                attachments,
            } => {
                let attach_strings: Vec<String> =
                    attachments.into_iter().map(|p| p.display().to_string()).collect();
                let method = shepherd_op_to_method(
                    "create_task",
                    serde_json::json!({
                        "title": title,
                        "description": description,
                        "criteria": criteria,
                        "attachments": attach_strings,
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "create_task".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdTasksAction::ImportGhIssue { issue_ref } => {
                let method = shepherd_op_to_method(
                    "import_task",
                    serde_json::json!({ "issue_ref": issue_ref }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "import_task".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdTasksAction::ArchiveTask { task_id } => {
                let method = shepherd_op_to_method(
                    "archive_task",
                    serde_json::json!({ "id": task_id }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "archive_task".to_string(),
                    },
                    outcome,
                );
            }
        }
    }

    pub(super) fn dispatch_shepherd_runs_action(
        &mut self,
        action: crate::shepherd::runs::ShepherdRunsAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::runs::{GateKind, ShepherdRunsAction};
        match action {
            ShepherdRunsAction::LaunchOrResume { task_id, options } => {
                let method = shepherd_op_to_method(
                    "run",
                    serde_json::json!({
                        "task": task_id.to_string(),
                        "auto_push": options.auto_push,
                        "replan": options.replan,
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "run".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdRunsAction::CancelRun { task_id } => {
                let method = shepherd_op_to_method(
                    "cancel_run",
                    serde_json::json!({ "task": task_id.to_string() }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "cancel_run".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdRunsAction::ApproveGate { task_id, gate } => {
                let target = match gate {
                    GateKind::PlanApproval => "plan".to_string(),
                    GateKind::PushAndPr { subtask } => format!("subtask:{subtask}"),
                };
                let method = shepherd_op_to_method(
                    "approve",
                    serde_json::json!({ "task": task_id.to_string(), "target": target }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "approve".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdRunsAction::RejectGate { task_id, gate } => {
                let target = match gate {
                    GateKind::PlanApproval => "plan".to_string(),
                    GateKind::PushAndPr { subtask } => format!("subtask:{subtask}"),
                };
                let method = shepherd_op_to_method(
                    "reject_gate",
                    serde_json::json!({ "task": task_id.to_string(), "target": target }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "reject_gate".to_string(),
                    },
                    outcome,
                );
            }
        }
    }

    #[allow(dead_code)]
    pub(super) fn open_shepherd_overlay(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.overlay = Some(ClientShellOverlay::Shepherd(
            crate::shepherd::ClientShepherdOverlay::default(),
        ));
        self.refresh_shepherd_views(outcome);
        outcome.repaint = true;
    }

    pub(super) fn open_shepherd_spawn_form(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.overlay = Some(ClientShellOverlay::ShepherdAgentModal(
            crate::shepherd::ClientShepherdAgentModal::new(
                crate::shepherd::panel::AgentsSubModal::Spawn(
                    crate::shepherd::panel::SpawnFormState::default(),
                ),
            ),
        ));
        outcome.repaint = true;
    }

    pub(super) fn open_shepherd_kill_confirm(
        &mut self,
        agent_id: &str,
        outcome: &mut ClientShellInput,
    ) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.overlay = Some(ClientShellOverlay::ShepherdAgentModal(
            crate::shepherd::ClientShepherdAgentModal::new(
                crate::shepherd::panel::AgentsSubModal::ConfirmKill {
                    agent_id: agent_id.to_string(),
                    state: "working".to_string(),
                    message: format!(
                        "¿Matar al agente '{agent_id}'? Esta acción enviará SIGKILL a su proceso."
                    ),
                },
            ),
        ));
        outcome.repaint = true;
    }

    pub(super) fn open_shepherd_clean_confirm(
        &mut self,
        agent_id: &str,
        outcome: &mut ClientShellInput,
    ) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.overlay = Some(ClientShellOverlay::ShepherdAgentModal(
            crate::shepherd::ClientShepherdAgentModal::new(
                crate::shepherd::panel::AgentsSubModal::ConfirmClean {
                    agent_id: agent_id.to_string(),
                    state: "done".to_string(),
                    message: format!("¿Limpiar el agente '{agent_id}'? Se cerrará su terminal y se eliminarán sus archivos de sesión."),
                },
            ),
        ));
        outcome.repaint = true;
    }

    pub(super) fn open_shepherd_clean_all_confirm(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.overlay = Some(ClientShellOverlay::ShepherdAgentModal(
            crate::shepherd::ClientShepherdAgentModal::new(
                crate::shepherd::panel::AgentsSubModal::ConfirmCleanAll {
                    count: 0,
                    workspace: None,
                    message: "¿Limpiar todos los agentes definitivos? Se cerrarán sus terminales y se eliminarán sus archivos de sesión.".to_string(),
                },
            ),
        ));
        outcome.repaint = true;
    }

    pub(super) fn refresh_shepherd_views(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        // Fetch tasks
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdTaskList(
                crate::api::schema::ShepherdTaskListParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "tasks".to_string(),
            },
            outcome,
        );
        // Fetch runs
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdRunList(
                crate::api::schema::ShepherdRunListParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "runs".to_string(),
            },
            outcome,
        );
        // Fetch skills
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdSkillList(
                crate::api::schema::ShepherdSkillListParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "skills".to_string(),
            },
            outcome,
        );
        // Fetch git status if on Git tab
        if let Some(ClientShellOverlay::Shepherd(ref overlay)) = self.overlay {
            if overlay.active_tab == crate::shepherd::ShepherdTab::Git {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdGitStatus(
                        crate::api::schema::ShepherdGitStatusParams::default(),
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "git_status".to_string(),
                    },
                    outcome,
                );
            }
        }
    }

    pub(super) fn route_shepherd_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        match self.overlay {
            Some(ClientShellOverlay::Shepherd(ref mut overlay)) => {
                let mut fake_dispatcher = crate::shepherd::FakeShepherdDispatcher::default();
                let key_outcome = crate::shepherd::route_shepherd_key(
                    overlay,
                    key.code,
                    key.modifiers,
                    &mut fake_dispatcher,
                );

                for (op, args) in fake_dispatcher.calls {
                    let method = shepherd_op_to_method(&op, args);
                    self.push_endpoint_method_with_kind(
                        method,
                        PendingEndpointKind::ShepherdCall { op },
                        outcome,
                    );
                }
                for (pane_id, _) in fake_dispatcher.focused_panes {
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        }),
                        outcome,
                    );
                }
                for (task_id, file_path) in fake_dispatcher.opened_editors {
                    let mut d = ClientActionDispatcher {
                        state: self,
                        outcome,
                    };
                    d.open_editor(task_id, &file_path);
                }

                match key_outcome {
                    crate::shepherd::ShepherdKeyOutcome::CloseOverlay => {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                    crate::shepherd::ShepherdKeyOutcome::FocusPane { pane_id, .. } => {
                        self.overlay = None;
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id,
                            }),
                            outcome,
                        );
                        outcome.repaint = true;
                    }
                    crate::shepherd::ShepherdKeyOutcome::OpenEditor { task_id, file_path } => {
                        self.overlay = None;
                        let mut d = ClientActionDispatcher {
                            state: self,
                            outcome,
                        };
                        d.open_editor(task_id, &file_path);
                        outcome.repaint = true;
                    }
                    crate::shepherd::ShepherdKeyOutcome::Handled => {
                        outcome.repaint = true;
                    }
                    crate::shepherd::ShepherdKeyOutcome::Ignored => {}
                }
                true
            }
            Some(ClientShellOverlay::ShepherdAgentModal(ref mut modal)) => {
                let mut fake_dispatcher = crate::shepherd::FakeShepherdDispatcher::default();
                let key_outcome = crate::shepherd::route_agent_modal_key(
                    modal,
                    key.code,
                    key.modifiers,
                    &mut fake_dispatcher,
                );

                for (op, args) in fake_dispatcher.calls {
                    let method = shepherd_op_to_method(&op, args);
                    self.push_endpoint_method_with_kind(
                        method,
                        PendingEndpointKind::ShepherdCall { op },
                        outcome,
                    );
                }
                for (pane_id, _) in fake_dispatcher.focused_panes {
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        }),
                        outcome,
                    );
                }

                match key_outcome {
                    crate::shepherd::ShepherdKeyOutcome::CloseOverlay => {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                    crate::shepherd::ShepherdKeyOutcome::FocusPane { pane_id, .. } => {
                        self.overlay = None;
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id,
                            }),
                            outcome,
                        );
                        outcome.repaint = true;
                    }
                    crate::shepherd::ShepherdKeyOutcome::Handled => {
                        outcome.repaint = true;
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn route_shepherd_mouse(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) {
        if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
            let Some(hits) = self.hits.shepherd_hits.clone() else {
                return;
            };
            let mut fake_dispatcher = crate::shepherd::FakeShepherdDispatcher::default();
            let click_outcome = crate::shepherd::handle_shepherd_mouse_click(
                overlay,
                point,
                &hits,
                &mut fake_dispatcher,
            );

            for (op, args) in fake_dispatcher.calls {
                let method = shepherd_op_to_method(&op, args);
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall { op },
                    outcome,
                );
            }
            for (pane_id, _) in fake_dispatcher.focused_panes {
                self.push_endpoint_method(
                    crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                        pane_id,
                    }),
                    outcome,
                );
            }
            for (task_id, file_path) in fake_dispatcher.opened_editors {
                let mut d = ClientActionDispatcher {
                    state: self,
                    outcome,
                };
                d.open_editor(task_id, &file_path);
            }

            match click_outcome {
                crate::shepherd::ShepherdClickOutcome::CloseOverlay => {
                    self.overlay = None;
                    outcome.repaint = true;
                }
                crate::shepherd::ShepherdClickOutcome::FocusPane { pane_id, .. } => {
                    self.overlay = None;
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        }),
                        outcome,
                    );
                    outcome.repaint = true;
                }
                crate::shepherd::ShepherdClickOutcome::OpenEditor { task_id, file_path } => {
                    self.overlay = None;
                    let mut d = ClientActionDispatcher {
                        state: self,
                        outcome,
                    };
                    d.open_editor(task_id, &file_path);
                    outcome.repaint = true;
                }
                crate::shepherd::ShepherdClickOutcome::Handled => {
                    outcome.repaint = true;
                }
                crate::shepherd::ShepherdClickOutcome::Ignored => {}
            }
        }
    }

    pub(super) fn handle_shepherd_endpoint_result(
        &mut self,
        op: &str,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) {
        if self.overlay.is_none() && self.shepherd_section.is_none() {
            return;
        }

        match result {
            Ok(crate::api::schema::ResponseResult::ShepherdTaskList { tasks }) => {
                let rows: Vec<crate::shepherd::tasks::ShepherdTaskRow> = tasks
                    .iter()
                    .map(crate::shepherd::tasks::ShepherdTaskRow::from_created_task)
                    .collect();
                if let Some(ref mut section) = self.shepherd_section {
                    section.tasks.tasks = rows;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdRunList { runs }) => {
                let rows: Vec<crate::shepherd::runs::ShepherdRunRow> = runs
                    .iter()
                    .filter_map(|r| {
                        serde_json::to_value(r)
                            .ok()
                            .and_then(|v| crate::shepherd::runs::ShepherdRunRow::from_value(&v))
                    })
                    .collect();
                if let Some(ref mut section) = self.shepherd_section {
                    section.runs.runs = rows;
                    section.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdSkillList { skills }) => {
                if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                    overlay.skills.catalog_skills = skills
                        .iter()
                        .map(crate::shepherd::skills::CatalogSkillItem::from)
                        .collect();
                    overlay.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdGitStatus { status }) => {
                if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                    overlay.git.update_from_status(&status);
                    overlay.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdTaskCreated { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdTaskArchived { .. }) => {
                let msg = format!("Tarea actualizada exitosamente ({op})");
                if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                    overlay.status_message = Some(msg.clone());
                }
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdRunStarted { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdGateApproved { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdGateRejected { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdPatternSaved { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdPatternDeleted { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdPatternValidated { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdTopicCreated { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdMessagePublished { .. }) => {
                let msg = format!("Operación completada ({op})");
                if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                    overlay.status_message = Some(msg.clone());
                }
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdAgentSpawned { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentKilled { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentCleaned { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentFocused { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentReconciled { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentResumed { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentList { .. }) => {
                let msg = format!("Operación de agente completada ({op})");
                if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                    overlay.status_message = Some(msg.clone());
                }
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdCall { data }) => {
                match op {
                    "tasks" => {
                        if let Ok(tasks) =
                            serde_json::from_value::<Vec<shepherd_core::CreatedTask>>(data.clone())
                        {
                            let rows: Vec<crate::shepherd::tasks::ShepherdTaskRow> = tasks
                                .iter()
                                .map(crate::shepherd::tasks::ShepherdTaskRow::from_created_task)
                                .collect();
                            if let Some(ref mut section) = self.shepherd_section {
                                section.tasks.tasks = rows;
                            }
                        }
                    }
                    "runs" => {
                        if let Some(arr) = data.as_array() {
                            let rows: Vec<crate::shepherd::runs::ShepherdRunRow> = arr
                                .iter()
                                .filter_map(crate::shepherd::runs::ShepherdRunRow::from_value)
                                .collect();
                            if let Some(ref mut section) = self.shepherd_section {
                                section.runs.runs = rows;
                                section.status_message = None;
                            }
                        }
                    }
                    "skills" => {
                        // If skills JSON is returned, update skills view
                        if let Ok(skills) = serde_json::from_value::<
                            Vec<shepherd_core::skills::Skill>,
                        >(data.clone())
                        {
                            if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) =
                                self.overlay
                            {
                                overlay.skills.catalog_skills = skills
                                    .iter()
                                    .map(crate::shepherd::skills::CatalogSkillItem::from)
                                    .collect();
                                overlay.status_message = None;
                            }
                        }
                    }
                    "git_status" => {
                        if let Ok(status) = serde_json::from_value::<
                            shepherd_core::git::GitRepoStatus,
                        >(data.clone())
                        {
                            if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) =
                                self.overlay
                            {
                                overlay.git.update_from_status(&status);
                                overlay.status_message = None;
                            }
                        }
                    }
                    "archive_task" | "create_task" | "import_task" | "task_create"
                    | "task_archive" => {
                        let msg = format!("Tarea actualizada exitosamente ({op})");
                        if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                            overlay.status_message = Some(msg.clone());
                        }
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    "run" | "approve" | "cancel_run" | "reject_gate" => {
                        let msg = format!("Operación completada ({op})");
                        if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                            overlay.status_message = Some(msg.clone());
                        }
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    "skills_toggle" | "skills_restore" | "skills_create" | "skills_delete" => {
                        let msg = format!("Configuración de skills actualizada ({op})");
                        if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                            overlay.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    _ => {
                        if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                            overlay.status_message = Some("Operación completada".to_string());
                        }
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some("Operación completada".to_string());
                        }
                    }
                }
                outcome.repaint = true;
            }
            Ok(_) => {
                if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                    overlay.status_message = Some("Respuesta inesperada del servidor".to_string());
                }
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some("Respuesta inesperada del servidor".to_string());
                }
                outcome.repaint = true;
            }
            Err(err) => {
                let msg = format!("Error: {}", err.message);
                if let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay {
                    overlay.status_message = Some(msg.clone());
                }
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                outcome.repaint = true;
            }
        }
    }
}
