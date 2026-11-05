use super::*;
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
        "pattern_run_start" => {
            let params: ShepherdPatternRunStartParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternRunStart(params)
        }
        "pattern_runs" | "pattern_run_list" => {
            let params: ShepherdPatternRunListParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternRunList(params)
        }
        "pattern_run_show" => {
            let params: ShepherdPatternRunShowParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternRunShow(params)
        }
        "pattern_run_pause" => {
            let params: ShepherdPatternRunPauseParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternRunPause(params)
        }
        "pattern_run_resume" => {
            let params: ShepherdPatternRunResumeParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternRunResume(params)
        }
        "pattern_run_stop" => {
            let params: ShepherdPatternRunStopParams =
                serde_json::from_value(args.clone()).unwrap_or_default();
            Method::ShepherdPatternRunStop(params)
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

impl<'a> ClientActionDispatcher<'a> {
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
    pub(super) fn is_shepherd_tab_active(&self) -> bool {
        if !self.config.shepherd_enabled {
            return false;
        }
        let Some(ws_id) = self
            .snapshot
            .as_deref()
            .and_then(|s| s.focused_workspace_id.as_deref())
        else {
            return true;
        };
        self.shepherd_tab_active.get(ws_id).copied().unwrap_or(true)
    }

    pub(super) fn ensure_shepherd_section(
        &mut self,
    ) -> &mut crate::shepherd::ui::state::ShepherdSectionState {
        if self.shepherd_section.is_none() {
            let sec = crate::shepherd::ui::state::ShepherdSectionState::default();
            self.shepherd_section = Some(sec);
        }
        self.shepherd_section.as_mut().unwrap()
    }

    pub(super) fn select_shepherd_tab(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.ensure_shepherd_section();
        self.refresh_shepherd_views(outcome);
        if let Some(ws_id) = self
            .snapshot
            .as_deref()
            .and_then(|s| s.focused_workspace_id.as_deref())
        {
            self.shepherd_tab_active.insert(ws_id.to_string(), true);
        }
        self.reveal_focused_tab = true;
        outcome.repaint = true;
        self.persist_chrome_preferences(outcome);
    }

    pub(super) fn set_shepherd_tab_active(
        &mut self,
        active: bool,
        outcome: &mut ClientShellInput,
    ) {
        if !self.config.shepherd_enabled {
            return;
        }
        if active {
            self.ensure_shepherd_section();
            self.refresh_shepherd_views(outcome);
        }
        if let Some(ws_id) = self
            .snapshot
            .as_deref()
            .and_then(|s| s.focused_workspace_id.as_deref())
        {
            self.shepherd_tab_active.insert(ws_id.to_string(), active);
        }
        self.reveal_focused_tab = true;
        outcome.repaint = true;
        self.persist_chrome_preferences(outcome);
    }

    pub(super) fn toggle_shepherd_section(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        let is_active = self.is_shepherd_tab_active();
        if is_active {
            self.set_shepherd_tab_active(false, outcome);
        } else {
            self.select_shepherd_tab(outcome);
        }
    }

    pub(super) fn handle_tab_navigation(
        &mut self,
        action: crate::input::KeybindAction,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.config.shepherd_enabled {
            return false;
        }
        let (real_tab_ids, current_index) = {
            let Some(snapshot) = self.snapshot.as_deref() else {
                return false;
            };
            let Some(focused_workspace) = snapshot.focused_workspace_id.as_deref() else {
                return false;
            };
            let real_tab_ids = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == focused_workspace)
                .map(|tab| tab.tab_id.clone())
                .collect::<Vec<_>>();
            if real_tab_ids.is_empty() {
                return false;
            }
            let current_focused = snapshot.focused_tab_id.as_deref();
            let current_index = current_focused
                .and_then(|id| real_tab_ids.iter().position(|tab_id| tab_id == id))
                .unwrap_or(0);
            (real_tab_ids, current_index)
        };

        match action {
            crate::input::KeybindAction::PreviousTab | crate::input::KeybindAction::NextTab => {
                let is_shepherd_active = self.is_shepherd_tab_active();
                if is_shepherd_active {
                    if action == crate::input::KeybindAction::NextTab {
                        self.set_shepherd_tab_active(false, outcome);
                        self.push_endpoint_method(
                            crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                                tab_id: real_tab_ids[0].clone(),
                            }),
                            outcome,
                        );
                    } else {
                        let last_tab_id = real_tab_ids.last().unwrap().clone();
                        self.set_shepherd_tab_active(false, outcome);
                        self.push_endpoint_method(
                            crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                                tab_id: last_tab_id,
                            }),
                            outcome,
                        );
                    }
                    true
                } else {
                    if action == crate::input::KeybindAction::NextTab
                        && current_index + 1 >= real_tab_ids.len()
                    {
                        self.select_shepherd_tab(outcome);
                        true
                    } else if action == crate::input::KeybindAction::PreviousTab
                        && current_index == 0
                    {
                        self.select_shepherd_tab(outcome);
                        true
                    } else {
                        false
                    }
                }
            }
            crate::input::KeybindAction::SwitchTab(index) => {
                if let Some(tab_id) = real_tab_ids.get(index).cloned() {
                    self.set_shepherd_tab_active(false, outcome);
                    self.push_endpoint_method(
                        crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                            tab_id,
                        }),
                        outcome,
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    pub(super) fn route_shepherd_section_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let section = self.ensure_shepherd_section();
        let prev_tab = section.active_tab;
        let key_event = crossterm::event::KeyEvent::new(key.code, key.modifiers);
        let section_outcome = section.handle_key(&key_event);
        if self.shepherd_section.as_ref().map(|s| s.active_tab) != Some(prev_tab) {
            self.refresh_shepherd_views(outcome);
        }
        self.dispatch_shepherd_section_outcome(section_outcome, outcome);
        true
    }

    pub(super) fn route_shepherd_section_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
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
        // If click is on the tab bar, do not intercept
        if self
            .hits
            .tabs
            .iter()
            .any(|(rect, _)| super::contains(*rect, point))
            || super::contains(self.hits.new_tab, point)
            || super::contains(self.hits.tab_scroll_left, point)
            || super::contains(self.hits.tab_scroll_right, point)
        {
            return false;
        }
        let section = self.ensure_shepherd_section();
        let prev_tab = section.active_tab;
        let section_outcome =
            crate::shepherd::ui::handle_shepherd_section_mouse(section, &hits, mouse);
        if self.shepherd_section.as_ref().map(|s| s.active_tab) != Some(prev_tab) {
            self.refresh_shepherd_views(outcome);
        }
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
                self.set_shepherd_tab_active(false, outcome);
                outcome.repaint = true;
            }
            ShepherdSectionOutcome::Handled => {
                outcome.repaint = true;
            }
            ShepherdSectionOutcome::Action(action) => {
                match action {
                    ShepherdSectionAction::Agents(agents_action) => {
                        self.dispatch_shepherd_agents_action(agents_action, outcome);
                    }
                    ShepherdSectionAction::Tasks(tasks_action) => {
                        self.dispatch_shepherd_tasks_action(tasks_action, outcome);
                    }
                    ShepherdSectionAction::Runs(runs_action) => {
                        self.dispatch_shepherd_runs_action(runs_action, outcome);
                    }
                    ShepherdSectionAction::Patterns(patterns_action) => {
                        self.dispatch_shepherd_patterns_action(patterns_action, outcome);
                    }
                    ShepherdSectionAction::Skills(skills_action) => {
                        self.dispatch_shepherd_skills_action(skills_action, outcome);
                    }
                    ShepherdSectionAction::Git(git_action) => {
                        self.dispatch_shepherd_git_action(git_action, outcome);
                    }
                    ShepherdSectionAction::Events(events_action) => {
                        self.dispatch_shepherd_events_action(events_action, outcome);
                    }
                }
                outcome.repaint = true;
            }
            ShepherdSectionOutcome::Ignored => {}
        }
    }

    pub(super) fn dispatch_shepherd_agents_action(
        &mut self,
        action: crate::shepherd::ui::agents::ShepherdAgentsAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::ui::agents::ShepherdAgentsAction;
        match action {
            ShepherdAgentsAction::FocusPane { pane_id, .. } => {
                self.set_shepherd_tab_active(false, outcome);
                self.push_endpoint_method(
                    crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                        pane_id,
                    }),
                    outcome,
                );
            }
            ShepherdAgentsAction::Spawn {
                role,
                task,
                vendor,
                model,
                extra_skills,
            } => {
                let method = shepherd_op_to_method(
                    "spawn",
                    serde_json::json!({
                        "role": role,
                        "task": task,
                        "vendor": vendor,
                        "model": model,
                        "extra_skills": extra_skills,
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "spawn".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdAgentsAction::Kill { agent_id } => {
                let method = shepherd_op_to_method("kill", serde_json::json!({ "id": agent_id }));
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "kill".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdAgentsAction::Clean { agent_id } => {
                let method =
                    shepherd_op_to_method("clean", serde_json::json!({ "ids": [agent_id] }));
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "clean".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdAgentsAction::CleanAllFinished { workspace, .. } => {
                let method = shepherd_op_to_method(
                    "clean",
                    serde_json::json!({ "all": true, "workspace": workspace }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "clean".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdAgentsAction::Refresh => {
                self.refresh_shepherd_views(outcome);
            }
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

    pub(super) fn dispatch_shepherd_patterns_action(
        &mut self,
        action: crate::shepherd::ui::patterns::ShepherdPatternsAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::ui::patterns::ShepherdPatternsAction;
        match action {
            ShepherdPatternsAction::List => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternList(
                        crate::api::schema::ShepherdPatternListParams::default(),
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "patterns".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdPatternsAction::Get { name } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternGet(
                        crate::api::schema::ShepherdPatternGetParams {
                            root: None,
                            name,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_get".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdPatternsAction::Save { pattern, global } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternSave(
                        crate::api::schema::ShepherdPatternSaveParams {
                            root: None,
                            pattern,
                            global,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_save".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdPatternsAction::Delete { name } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternDelete(
                        crate::api::schema::ShepherdPatternDeleteParams {
                            root: None,
                            name,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_delete".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdPatternsAction::Validate { name, pattern } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternValidate(
                        crate::api::schema::ShepherdPatternValidateParams {
                            root: None,
                            name,
                            pattern,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_validate".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdPatternsAction::RunPattern {
                pattern,
                task,
                worktree,
            } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternRunStart(
                        crate::api::schema::ShepherdPatternRunStartParams {
                            root: None,
                            pattern,
                            task,
                            worktree,
                            confirm_over_limit: false,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_run_start".to_string(),
                    },
                    outcome,
                );
            }
        }
    }

    pub(super) fn dispatch_shepherd_skills_action(
        &mut self,
        action: crate::shepherd::ui::skills::ShepherdSkillsAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::ui::skills::ShepherdSkillsAction;
        match action {
            ShepherdSkillsAction::ToggleAgentSkill {
                agent_id,
                skill_name,
                enable,
            } => {
                let method = shepherd_op_to_method(
                    "skills_toggle",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "skill": skill_name,
                        "enable": enable,
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "skills_toggle".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdSkillsAction::RestoreEditedSkill {
                agent_id,
                skill_name,
            } => {
                let method = shepherd_op_to_method(
                    "skills_restore",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "skill": skill_name,
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "skills_restore".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdSkillsAction::CreateCatalogSkill {
                name,
                description,
                scope,
                is_global,
            } => {
                let method = shepherd_op_to_method(
                    "skills_create",
                    serde_json::json!({
                        "name": name,
                        "description": description,
                        "scope": format!("{scope:?}").to_lowercase(),
                        "global": is_global,
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "skills_create".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdSkillsAction::ToggleCatalogGlobal { name, make_global } => {
                let method = shepherd_op_to_method(
                    "skills_global",
                    serde_json::json!({
                        "name": name,
                        "global": make_global,
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "skills_global".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdSkillsAction::DeleteCatalogSkill { name, scope } => {
                let method = shepherd_op_to_method(
                    "skills_delete",
                    serde_json::json!({
                        "name": name,
                        "scope": format!("{scope:?}").to_lowercase(),
                    }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "skills_delete".to_string(),
                    },
                    outcome,
                );
            }
        }
    }

    pub(super) fn dispatch_shepherd_git_action(
        &mut self,
        action: crate::shepherd::ui::git::ShepherdGitAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::ui::git::ShepherdGitAction;
        match action {
            ShepherdGitAction::Refresh => {
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
            ShepherdGitAction::CopyOrShowPath { path } => {
                if let Some(ref mut sec) = self.shepherd_section {
                    sec.status_message = Some(format!("Ruta: {}", path.display()));
                }
            }
            ShepherdGitAction::CreateWorktree { path, branch } => {
                let method = shepherd_op_to_method(
                    "create_worktree",
                    serde_json::json!({ "path": path, "branch": branch }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "create_worktree".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdGitAction::CreateBranch { branch, base } => {
                let method = shepherd_op_to_method(
                    "create_branch",
                    serde_json::json!({ "branch": branch, "base": base }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "create_branch".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdGitAction::PublishBranch { branch } => {
                let method = shepherd_op_to_method(
                    "publish_branch",
                    serde_json::json!({ "branch": branch }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "publish_branch".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdGitAction::RemoveWorktree { path } => {
                let method = shepherd_op_to_method(
                    "remove_worktree",
                    serde_json::json!({ "path": path }),
                );
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::ShepherdCall {
                        op: "remove_worktree".to_string(),
                    },
                    outcome,
                );
            }
        }
    }

    pub(super) fn dispatch_shepherd_events_action(
        &mut self,
        action: crate::shepherd::ui::events::ShepherdEventsAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::shepherd::ui::events::ShepherdEventsAction;
        match action {
            ShepherdEventsAction::RefreshTopics => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdTopicList(
                        crate::api::schema::ShepherdEmptyParams::default(),
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "topic_list".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdEventsAction::RefreshRuns => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternRunList(
                        crate::api::schema::ShepherdPatternRunListParams::default(),
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_run_list".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdEventsAction::ShowRun { id } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternRunShow(
                        crate::api::schema::ShepherdPatternRunShowParams {
                            root: None,
                            id,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_run_show".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdEventsAction::TailTopic { topic, partition, limit } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdTopicTail(
                        crate::api::schema::ShepherdTopicTailParams {
                            topic,
                            partition,
                            limit,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "topic_tail".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdEventsAction::ReplayTopic { topic, partition, from_offset, limit } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdTopicReplay(
                        crate::api::schema::ShepherdTopicReplayParams {
                            topic,
                            partition,
                            from_offset,
                            limit,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "topic_replay".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdEventsAction::PauseRun { id } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternRunPause(
                        crate::api::schema::ShepherdPatternRunPauseParams {
                            root: None,
                            id,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_run_pause".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdEventsAction::ResumeRun { id } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternRunResume(
                        crate::api::schema::ShepherdPatternRunResumeParams {
                            root: None,
                            id,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_run_resume".to_string(),
                    },
                    outcome,
                );
            }
            ShepherdEventsAction::StopRun { id, confirm } => {
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternRunStop(
                        crate::api::schema::ShepherdPatternRunStopParams {
                            root: None,
                            id,
                            confirm,
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_run_stop".to_string(),
                    },
                    outcome,
                );
            }
        }
    }

    pub(super) fn open_shepherd_spawn_form(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.select_shepherd_tab(outcome);
        let section = self.ensure_shepherd_section();
        section.select_tab(crate::shepherd::ui::state::ShepherdSectionTab::Agents);
        section.agents.start_spawn();
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
        self.select_shepherd_tab(outcome);
        let section = self.ensure_shepherd_section();
        section.select_tab(crate::shepherd::ui::state::ShepherdSectionTab::Agents);
        section.agents.confirm_kill = Some(agent_id.to_string());
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
        self.select_shepherd_tab(outcome);
        let section = self.ensure_shepherd_section();
        section.select_tab(crate::shepherd::ui::state::ShepherdSectionTab::Agents);
        section.agents.confirm_clean = Some(agent_id.to_string());
        outcome.repaint = true;
    }

    pub(super) fn open_shepherd_clean_all_confirm(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        self.select_shepherd_tab(outcome);
        let section = self.ensure_shepherd_section();
        section.select_tab(crate::shepherd::ui::state::ShepherdSectionTab::Agents);
        section.agents.start_clean_all();
        outcome.repaint = true;
    }

    pub(super) fn refresh_shepherd_views(&mut self, outcome: &mut ClientShellInput) {
        if !self.config.shepherd_enabled {
            return;
        }
        // Fetch agents
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdAgentList(
                crate::api::schema::ShepherdAgentListParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "agents".to_string(),
            },
            outcome,
        );
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
        // Fetch git status
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdGitStatus(
                crate::api::schema::ShepherdGitStatusParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "git_status".to_string(),
            },
            outcome,
        );
        // Fetch patterns
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdPatternList(
                crate::api::schema::ShepherdPatternListParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "patterns".to_string(),
            },
            outcome,
        );
        // Fetch topics
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdTopicList(
                crate::api::schema::ShepherdEmptyParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "topic_list".to_string(),
            },
            outcome,
        );
        // Fetch pattern runs
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ShepherdPatternRunList(
                crate::api::schema::ShepherdPatternRunListParams::default(),
            ),
            PendingEndpointKind::ShepherdCall {
                op: "pattern_run_list".to_string(),
            },
            outcome,
        );
    }

    pub(super) fn handle_shepherd_endpoint_result(
        &mut self,
        op: &str,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) {
        if self.shepherd_section.is_none() {
            return;
        }

        match result {
            Ok(crate::api::schema::ResponseResult::ShepherdAgentList { agents }) => {
                if let Some(ref mut section) = self.shepherd_section {
                    section.agents.update_from_agents(&agents);
                }
                outcome.repaint = true;
            }
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
                if let Some(ref mut section) = self.shepherd_section {
                    section.skills.update_from_skills(&skills);
                    section.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdGitStatus { status }) => {
                if let Some(ref mut section) = self.shepherd_section {
                    section.git.update_from_status(&status);
                    section.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternList { patterns }) => {
                if let Some(ref mut section) = self.shepherd_section {
                    section.patterns.update_from_summaries(patterns);
                    section.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternDetail { pattern }) => {
                if let Some(ref mut section) = self.shepherd_section {
                    section.patterns.update_detail(pattern);
                    section.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdTopicList { topics }) => {
                let mut auto_tail_topic = None;
                if let Some(ref mut section) = self.shepherd_section {
                    section.events.topics = topics
                        .into_iter()
                        .map(|t| crate::shepherd::ui::events::ShepherdTopicRow {
                            name: t.name,
                            partitions: t.partitions,
                            messages_count: t.messages_count,
                            groups_count: 0,
                        })
                        .collect();
                    if section.events.messages.is_empty() {
                        if let Some(t) = section.events.selected_topic_row() {
                            auto_tail_topic = Some(t.name.clone());
                        }
                    }
                    section.status_message = None;
                }
                if let Some(topic) = auto_tail_topic {
                    self.push_endpoint_method_with_kind(
                        crate::api::schema::Method::ShepherdTopicTail(
                            crate::api::schema::ShepherdTopicTailParams {
                                topic,
                                partition: None,
                                limit: Some(50),
                            },
                        ),
                        PendingEndpointKind::ShepherdCall {
                            op: "topic_tail".to_string(),
                        },
                        outcome,
                    );
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdTopicMessages { messages }) => {
                if let Some(ref mut section) = self.shepherd_section {
                    if !section.events.view_paused {
                        section.events.messages = messages;
                    }
                    section.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternRunList { runs }) => {
                let mut auto_show_run = None;
                if let Some(ref mut section) = self.shepherd_section {
                    section.events.runs = runs;
                    if section.events.selected_run_detail.is_none() {
                        if let Some(r) = section.events.selected_run_row() {
                            auto_show_run = Some(r.id.clone());
                        }
                    }
                    section.status_message = None;
                }
                if let Some(id) = auto_show_run {
                    self.push_endpoint_method_with_kind(
                        crate::api::schema::Method::ShepherdPatternRunShow(
                            crate::api::schema::ShepherdPatternRunShowParams {
                                root: None,
                                id,
                            },
                        ),
                        PendingEndpointKind::ShepherdCall {
                            op: "pattern_run_show".to_string(),
                        },
                        outcome,
                    );
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternRunDetail { run }) => {
                if let Some(ref mut section) = self.shepherd_section {
                    section.events.selected_run_detail = Some(run);
                    section.status_message = None;
                }
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternRunStarted { run }) => {
                let msg = format!("Ejecución de patrón iniciada: {}", run.id);
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternRunPaused { run }) => {
                let msg = format!("Ejecución de patrón pausada: {}", run.id);
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternRunResumed { run }) => {
                let msg = format!("Ejecución de patrón reanudada: {}", run.id);
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternRunStopped { run }) => {
                let msg = format!("Ejecución de patrón detenida: {}", run.id);
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdTaskCreated { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdTaskArchived { .. }) => {
                let msg = format!("Tarea actualizada exitosamente ({op})");
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdRunStarted { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdGateApproved { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdGateRejected { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdPatternDeleted { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdPatternValidated { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdTopicCreated { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdMessagePublished { .. }) => {
                let msg = format!("Operación completada ({op})");
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdPatternSaved { ref name, .. }) => {
                let msg = format!("Patrón '{name}' guardado exitosamente");
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::ShepherdPatternGet(
                        crate::api::schema::ShepherdPatternGetParams {
                            root: None,
                            name: name.clone(),
                        },
                    ),
                    PendingEndpointKind::ShepherdCall {
                        op: "pattern_get".to_string(),
                    },
                    outcome,
                );
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdAgentSpawned { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentKilled { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentCleaned { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentFocused { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentReconciled { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdAgentResumed { .. }) => {
                let msg = format!("Operación de agente completada ({op})");
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdCall { data }) => {
                match op {
                    "agents" => {
                        if let Ok(agents) =
                            serde_json::from_value::<Vec<shepherd_core::Agent>>(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.agents.update_from_agents(&agents);
                            }
                        }
                    }
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
                        if let Ok(skills) = serde_json::from_value::<
                            Vec<shepherd_core::skills::Skill>,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.skills.update_from_skills(&skills);
                                section.status_message = None;
                            }
                        }
                    }
                    "git_status" => {
                        if let Ok(status) = serde_json::from_value::<
                            shepherd_core::git::GitRepoStatus,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.git.update_from_status(&status);
                                section.status_message = None;
                            }
                        }
                    }
                    "archive_task" | "create_task" | "import_task" | "task_create"
                    | "task_archive" => {
                        let msg = format!("Tarea actualizada exitosamente ({op})");
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    "run" | "approve" | "cancel_run" | "reject_gate" => {
                        let msg = format!("Operación completada ({op})");
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    "skills_toggle" | "skills_restore" | "skills_create" | "skills_delete"
                    | "skills_global" => {
                        let msg = format!("Configuración de skills actualizada ({op})");
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    "patterns" => {
                        if let Ok(patterns) = serde_json::from_value::<
                            Vec<shepherd_core::patterns::PatternSummary>,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.patterns.update_from_summaries(patterns);
                                section.status_message = None;
                            }
                        }
                    }
                    "pattern_get" => {
                        if let Ok(detail) = serde_json::from_value::<
                            shepherd_core::patterns::PatternDetail,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.patterns.update_detail(detail);
                                section.status_message = None;
                            }
                        }
                    }
                    "pattern_save" | "pattern_delete" | "pattern_validate" => {
                        let msg = format!("Operación de patrón completada ({op})");
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    "pattern_run_start" | "pattern_run_pause" | "pattern_run_resume" | "pattern_run_stop" => {
                        let msg = format!("Operación de ejecución completada ({op})");
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some(msg);
                        }
                        self.refresh_shepherd_views(outcome);
                    }
                    "pattern_run_list" => {
                        if let Ok(runs) = serde_json::from_value::<
                            Vec<shepherd_core::service::patterns_run::PatternRunSummary>,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.events.runs = runs;
                                section.status_message = None;
                            }
                        }
                    }
                    "pattern_run_show" => {
                        if let Ok(run) = serde_json::from_value::<
                            shepherd_core::service::patterns_run::PatternRun,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.events.selected_run_detail = Some(run);
                                section.status_message = None;
                            }
                        }
                    }
                    "topic_list" => {
                        if let Ok(topics) = serde_json::from_value::<
                            Vec<shepherd_core::bus::TopicSummary>,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                section.events.topics = topics
                                    .into_iter()
                                    .map(|t| crate::shepherd::ui::events::ShepherdTopicRow {
                                        name: t.name,
                                        partitions: t.partitions,
                                        messages_count: t.messages_count,
                                        groups_count: 0,
                                    })
                                    .collect();
                                section.status_message = None;
                            }
                        }
                    }
                    "topic_tail" | "topic_replay" => {
                        if let Ok(messages) = serde_json::from_value::<
                            Vec<shepherd_core::bus::Message>,
                        >(data.clone())
                        {
                            if let Some(ref mut section) = self.shepherd_section {
                                if !section.events.view_paused {
                                    section.events.messages = messages;
                                }
                                section.status_message = None;
                            }
                        }
                    }
                    _ => {
                        if let Some(ref mut section) = self.shepherd_section {
                            section.status_message = Some("Operación completada".to_string());
                        }
                    }
                }
                outcome.repaint = true;
            }
            Ok(_) => {
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some("Respuesta inesperada del servidor".to_string());
                }
                outcome.repaint = true;
            }
            Err(err) => {
                let msg = format!("Error: {}", err.message);
                if let Some(ref mut section) = self.shepherd_section {
                    section.status_message = Some(msg);
                }
                outcome.repaint = true;
            }
        }
    }

    pub(crate) fn tick_shepherd(
        &mut self,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.config.shepherd_enabled || !self.is_shepherd_tab_active() {
            return false;
        }

        let should_refresh = self.pending_shepherd_refresh
            || self
                .last_shepherd_refresh
                .is_none_or(|last| now.saturating_duration_since(last) >= std::time::Duration::from_millis(1500));

        if should_refresh {
            self.refresh_shepherd_views(outcome);
            self.last_shepherd_refresh = Some(now);
            self.pending_shepherd_refresh = false;
            true
        } else {
            false
        }
    }
}
