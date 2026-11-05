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
        let Some(ClientShellOverlay::Shepherd(ref mut overlay)) = self.overlay else {
            return;
        };

        match result {
            Ok(crate::api::schema::ResponseResult::ShepherdTaskList { tasks }) => {
                overlay.tasks.tasks = tasks
                    .iter()
                    .map(crate::shepherd::tasks::ShepherdTaskRow::from_created_task)
                    .collect();
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdRunList { runs }) => {
                overlay.runs.runs = runs
                    .iter()
                    .filter_map(|r| {
                        serde_json::to_value(r)
                            .ok()
                            .and_then(|v| crate::shepherd::runs::ShepherdRunRow::from_value(&v))
                    })
                    .collect();
                overlay.status_message = None;
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdSkillList { skills }) => {
                overlay.skills.catalog_skills = skills
                    .iter()
                    .map(crate::shepherd::skills::CatalogSkillItem::from)
                    .collect();
                overlay.status_message = None;
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdGitStatus { status }) => {
                overlay.git.update_from_status(&status);
                overlay.status_message = None;
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdTaskCreated { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdTaskArchived { .. }) => {
                overlay.status_message = Some(format!("Tarea actualizada exitosamente ({op})"));
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdRunStarted { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdGateApproved { .. })
            | Ok(crate::api::schema::ResponseResult::ShepherdGateRejected { .. }) => {
                overlay.status_message = Some(format!("Operación completada ({op})"));
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
                overlay.status_message = Some(format!("Operación de agente completada ({op})"));
                self.refresh_shepherd_views(outcome);
                outcome.repaint = true;
            }
            Ok(crate::api::schema::ResponseResult::ShepherdCall { data }) => {
                match op {
                    "tasks" => {
                        if let Ok(tasks) =
                            serde_json::from_value::<Vec<shepherd_core::CreatedTask>>(data.clone())
                        {
                            overlay.tasks.tasks = tasks
                                .iter()
                                .map(crate::shepherd::tasks::ShepherdTaskRow::from_created_task)
                                .collect();
                        }
                    }
                    "runs" => {
                        if let Some(arr) = data.as_array() {
                            overlay.runs.runs = arr
                                .iter()
                                .filter_map(crate::shepherd::runs::ShepherdRunRow::from_value)
                                .collect();
                        }
                        overlay.status_message = None;
                    }
                    "skills" => {
                        // If skills JSON is returned, update skills view
                        if let Ok(skills) = serde_json::from_value::<
                            Vec<shepherd_core::skills::Skill>,
                        >(data.clone())
                        {
                            overlay.skills.catalog_skills = skills
                                .iter()
                                .map(crate::shepherd::skills::CatalogSkillItem::from)
                                .collect();
                        }
                        overlay.status_message = None;
                    }
                    "git_status" => {
                        if let Ok(status) = serde_json::from_value::<
                            shepherd_core::git::GitRepoStatus,
                        >(data.clone())
                        {
                            overlay.git.update_from_status(&status);
                        }
                        overlay.status_message = None;
                    }
                    "archive_task" | "create_task" | "import_task" | "task_create"
                    | "task_archive" => {
                        overlay.status_message =
                            Some(format!("Tarea actualizada exitosamente ({op})"));
                        self.refresh_shepherd_views(outcome);
                    }
                    "run" | "approve" | "cancel_run" | "reject_gate" => {
                        overlay.status_message = Some(format!("Operación completada ({op})"));
                        self.refresh_shepherd_views(outcome);
                    }
                    "skills_toggle" | "skills_restore" | "skills_create" | "skills_delete" => {
                        overlay.status_message =
                            Some(format!("Configuración de skills actualizada ({op})"));
                        self.refresh_shepherd_views(outcome);
                    }
                    _ => {
                        overlay.status_message = Some("Operación completada".to_string());
                    }
                }
                outcome.repaint = true;
            }
            Ok(_) => {
                overlay.status_message = Some("Respuesta inesperada del servidor".to_string());
                outcome.repaint = true;
            }
            Err(err) => {
                overlay.status_message = Some(format!("Error: {}", err.message));
                outcome.repaint = true;
            }
        }
    }
}
