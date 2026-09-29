use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabCreateParams, TabListParams,
    TabMoveParams, TabPinMode, TabPinParams, TabPrioMode, TabPrioParams, TabPrioResult,
    TabRenameParams, TabStarMode, TabStarParams, TabTarget,
};
use crate::app::App;
use crate::workspace::TabPrioAction as StateTabPrioAction;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_tab_list(&mut self, id: String, params: TabListParams) -> String {
        let tabs = if let Some(workspace_id) = params.workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            let Some(_) = self.state.workspaces.get(ws_idx) else {
                return workspace_not_found(id, &workspace_id);
            };
            self.tab_list_info(ws_idx)
        } else {
            let mut tabs = Vec::new();
            for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                for tab_idx in 0..ws.tabs.len() {
                    if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                        tabs.push(tab);
                    }
                }
            }
            tabs
        };

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_get(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_create(&mut self, id: String, params: TabCreateParams) -> String {
        let TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
            env,
            work_context,
        } = params;
        let work_context = match self.prepare_spawn_work_context(work_context) {
            Ok(context) => context,
            Err(message) => return encode_error(id, "invalid_work_context", message),
        };
        let ws_idx = if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
            self.resolve_new_terminal_cwd(self.focused_pane_cwd_in_workspace(ws_idx))
        });
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.pane_terminal_theme();
        let host_terminal_appearance = Some(self.state.pane_terminal_appearance());
        let extra_env = match super::env::normalize_launch_env(env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("workspace disappeared"))
            .and_then(|ws| {
                ws.create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                )
            });
        match result {
            Ok((tab_idx, mut terminal, runtime)) => {
                Self::bind_spawn_work_context(&mut terminal, work_context);
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[tab_idx].root_pane,
                );
                if let Some(label) = label {
                    let workspace_id = self.state.workspaces[ws_idx].id.clone();
                    let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
                        crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
                    });
                    if let Some(tab) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.tabs.get_mut(tab_idx))
                    {
                        tab.set_custom_name(label);
                        crate::logging::tab_renamed(&workspace_id, &tab_id);
                    }
                }
                if focus {
                    self.state.switch_workspace_tab(ws_idx, tab_idx);
                    self.focus_client_on_pane();
                }
                self.schedule_session_save();
                self.emit_tab_created_events(ws_idx, tab_idx);
                encode_success(
                    id,
                    self.tab_created_result(ws_idx, tab_idx)
                        .expect("new tab should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "tab_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_tab_focus(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.focus_client_on_pane();
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_rename(&mut self, id: String, params: TabRenameParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
            crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
        });
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        match params.label.clone() {
            Some(label) => tab.set_user_custom_name(label),
            None => tab.clear_custom_name(),
        }
        crate::logging::tab_renamed(&workspace_id, &tab_id);
        if self.state.active == Some(ws_idx) {
            // Reflow the tab bar so the new label width takes effect immediately.
            // The tab bar renders into cached hit areas; without this refresh the
            // old geometry lingers until the next refresh (e.g. a tab switch),
            // leaving the visible label stale. Mirrors handle_tab_move.
            self.state.refresh_tab_bar_view();
        }
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                workspace_id: self.public_workspace_id(ws_idx),
                label: params.label,
            },
        });
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_prio(&mut self, id: String, params: TabPrioParams) -> String {
        let requested_tab_id = params.tab_id.clone();
        let Some((ws_idx, tab_idx)) = self.resolve_optional_tab(requested_tab_id.as_deref()) else {
            return tab_not_found(id, requested_tab_id.as_deref().unwrap_or("active"));
        };
        let action = match params.mode {
            TabPrioMode::Toggle => StateTabPrioAction::Toggle,
            TabPrioMode::On => StateTabPrioAction::Set(true),
            TabPrioMode::Off => StateTabPrioAction::Set(false),
        };
        let Some(changed) = self.state.apply_tab_prio(ws_idx, tab_idx, action) else {
            return tab_not_found(id, requested_tab_id.as_deref().unwrap_or("active"));
        };
        if changed {
            self.schedule_session_save();
            if self.no_session {
                self.state.mark_session_dirty();
            }
        }
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return tab_not_found(id, requested_tab_id.as_deref().unwrap_or("active"));
        };
        let prio = self.state.workspaces[ws_idx].tabs[tab_idx].prio;

        encode_success(
            id,
            ResponseResult::TabPrio {
                prio: TabPrioResult {
                    changed,
                    tab_id,
                    prio,
                },
            },
        )
    }

    pub(super) fn handle_tab_pin(&mut self, id: String, params: TabPinParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        let pinned = match params.mode {
            TabPinMode::Pin => true,
            TabPinMode::Unpin => false,
            TabPinMode::Toggle => !tab.pinned,
        };
        let changed = tab.pinned != pinned;
        tab.pinned = pinned;
        self.schedule_session_save();
        if changed {
            self.state.mark_sidebar_projection_changed();
        }
        tab_info_response(id, &params.tab_id, self.tab_info(ws_idx, tab_idx))
    }

    pub(super) fn handle_tab_star(&mut self, id: String, params: TabStarParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.starred = match params.mode {
            TabStarMode::Star => true,
            TabStarMode::Unstar => false,
            TabStarMode::Toggle => !tab.starred,
        };
        self.schedule_session_save();
        tab_info_response(id, &params.tab_id, self.tab_info(ws_idx, tab_idx))
    }

    pub(super) fn handle_tab_move(&mut self, id: String, params: TabMoveParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &params.tab_id);
        };
        if params.insert_index > ws.tabs.len() {
            return encode_error(
                id,
                "tab_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let tab_id = self
            .public_tab_id(ws_idx, tab_idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&ws.id, tab_idx + 1));
        let workspace_id = self.public_workspace_id(ws_idx);
        let insert_index = params.insert_index;
        let moved = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|ws| ws.move_tab(tab_idx, insert_index));
        let tabs = self.tab_list_info(ws_idx);
        if moved {
            self.schedule_session_save();
            if self.state.active == Some(ws_idx) {
                self.state.tab_scroll_follow_active = true;
                self.state.refresh_tab_bar_view();
            }
            self.emit_event(EventEnvelope {
                event: EventKind::TabMoved,
                data: EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index,
                    tabs: tabs.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_close(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        match self.close_tab_preserving_workspace(ws_idx, tab_idx, false) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(message) => encode_error(id, "tab_close_failed", message),
        }
    }

    pub(crate) fn close_tab_preserving_workspace(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
        emit_pane_closed: bool,
    ) -> Result<(), String> {
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return Err("tab not found".into());
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        let Some(workspace) = self.state.workspaces.get(ws_idx) else {
            return Err("workspace not found".into());
        };
        if tab_idx >= workspace.tabs.len() {
            return Err("tab not found".into());
        }
        let replace_last_tab = workspace.tabs.len() == 1;
        let was_active_workspace = self.state.active == Some(ws_idx);
        let replacement_cwd = replace_last_tab.then(|| workspace.identity_cwd.clone());
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let pane_ids = self.state.pane_ids_for_tab(ws_idx, tab_idx);
        for pane_id in &pane_ids {
            self.fleet_attach_agents.remove(pane_id);
        }
        let public_pane_ids = emit_pane_closed.then(|| {
            pane_ids
                .iter()
                .filter_map(|pane_id| self.public_pane_id(ws_idx, *pane_id))
                .collect::<Vec<_>>()
        });
        let replacement_created = replacement_cwd.is_some();

        if let Some(cwd) = replacement_cwd {
            let (rows, cols) = self.state.estimate_pane_size();
            let default_shell = self.state.default_shell.clone();
            let shell_mode = self.state.shell_mode;
            let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
            let theme = self.state.pane_terminal_theme();
            let appearance = Some(self.state.pane_terminal_appearance());
            let (new_tab_idx, terminal, runtime) = self
                .state
                .workspaces
                .get_mut(ws_idx)
                .ok_or_else(|| "workspace not found".to_string())?
                .create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    theme,
                    appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, shell_mode),
                    Vec::new(),
                )
                .map_err(|err| format!("could not create replacement tab: {err}"))?;
            let root = self.state.workspaces[ws_idx].tabs[new_tab_idx].root_pane;
            self.terminal_runtimes.insert(terminal.id.clone(), runtime);
            self.state.terminals.insert(terminal.id.clone(), terminal);
            self.state.remove_alias_shadowed_by_new_pane(root);
        }

        let closed = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|workspace| workspace.close_tab(tab_idx));
        if !closed {
            return Err(format!("tab {tab_id} could not be closed"));
        }
        self.state.remove_plugin_pane_records(pane_ids);
        self.state.remove_unattached_terminal_ids(terminal_ids);
        self.shutdown_detached_terminal_runtimes();
        self.schedule_session_save();
        if let Some(public_pane_ids) = public_pane_ids {
            for pane_id in public_pane_ids {
                self.emit_event(EventEnvelope {
                    event: EventKind::PaneClosed,
                    data: EventData::PaneClosed {
                        pane_id,
                        workspace_id: workspace_id.clone(),
                    },
                });
            }
        }
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id,
                workspace_id,
            },
        });
        if replacement_created {
            let new_tab_idx = 0;
            if was_active_workspace {
                self.state
                    .switch_workspace_tab_preserving_workspace_selection(ws_idx, new_tab_idx);
                if matches!(
                    self.state.server_mode(),
                    crate::app::Mode::Terminal
                        | crate::app::Mode::Navigate
                        | crate::app::Mode::Prefix
                ) {
                    self.focus_client_on_pane();
                }
            }
            self.emit_tab_created_events(ws_idx, new_tab_idx);
        }
        Ok(())
    }

    fn tab_list_info(&self, ws_idx: usize) -> Vec<crate::api::schema::TabInfo> {
        self.state
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                (0..ws.tabs.len())
                    .filter_map(|idx| self.tab_info(ws_idx, idx))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn resolve_optional_tab(&self, tab_id: Option<&str>) -> Option<(usize, usize)> {
        match tab_id {
            Some(tab_id) => self.parse_tab_id(tab_id),
            None => {
                let ws_idx = self.state.active?;
                let tab_idx = self.state.workspaces.get(ws_idx)?.active_tab_index();
                self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
                Some((ws_idx, tab_idx))
            }
        }
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

fn tab_not_found(id: String, tab_id: &str) -> String {
    encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
}

fn tab_info_response(id: String, tab_id: &str, tab: Option<crate::api::schema::TabInfo>) -> String {
    let Some(tab) = tab else {
        return tab_not_found(id, tab_id);
    };
    encode_success(id, ResponseResult::TabInfo { tab })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
    use super::*;
    use crate::{
        api::schema::{ErrorResponse, SuccessResponse},
        config::{Config, ShellModeConfig},
        workspace::Workspace,
    };

    #[test]
    fn api_tab_prio_sets_clears_toggles_and_reports_changes() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.session_dirty = false;
        app.state.session_dirty_revision = 0;
        let tab_id = app.public_tab_id(0, 0).unwrap();

        let set_response = app.handle_tab_prio(
            "set".into(),
            TabPrioParams {
                tab_id: Some(tab_id.clone()),
                mode: TabPrioMode::On,
            },
        );
        let set: SuccessResponse = serde_json::from_str(&set_response).unwrap();
        assert_eq!(
            set.result,
            ResponseResult::TabPrio {
                prio: TabPrioResult {
                    changed: true,
                    tab_id: tab_id.clone(),
                    prio: true,
                },
            }
        );
        assert!(app.state.session_dirty);
        assert_eq!(app.state.session_dirty_revision, 1);

        let get_response = app.handle_tab_get(
            "get".into(),
            TabTarget {
                tab_id: tab_id.clone(),
            },
        );
        let get: SuccessResponse = serde_json::from_str(&get_response).unwrap();
        let ResponseResult::TabInfo { tab } = get.result else {
            panic!("expected tab info");
        };
        assert!(tab.prio);

        let unchanged_response = app.handle_tab_prio(
            "unchanged".into(),
            TabPrioParams {
                tab_id: None,
                mode: TabPrioMode::On,
            },
        );
        let unchanged: SuccessResponse = serde_json::from_str(&unchanged_response).unwrap();
        assert_eq!(
            unchanged.result,
            ResponseResult::TabPrio {
                prio: TabPrioResult {
                    changed: false,
                    tab_id: tab_id.clone(),
                    prio: true,
                },
            }
        );
        assert_eq!(app.state.session_dirty_revision, 1);

        let toggle_response = app.handle_tab_prio(
            "toggle".into(),
            TabPrioParams {
                tab_id: Some(tab_id.clone()),
                mode: TabPrioMode::Toggle,
            },
        );
        let toggle: SuccessResponse = serde_json::from_str(&toggle_response).unwrap();
        assert!(matches!(
            toggle.result,
            ResponseResult::TabPrio {
                prio: TabPrioResult {
                    changed: true,
                    prio: false,
                    ..
                }
            }
        ));

        let clear_response = app.handle_tab_prio(
            "clear".into(),
            TabPrioParams {
                tab_id: Some(tab_id.clone()),
                mode: TabPrioMode::Off,
            },
        );
        let clear: SuccessResponse = serde_json::from_str(&clear_response).unwrap();
        assert!(matches!(
            clear.result,
            ResponseResult::TabPrio {
                prio: TabPrioResult {
                    changed: false,
                    prio: false,
                    ..
                }
            }
        ));
    }

    #[test]
    fn api_tab_prio_unknown_tab_uses_tab_not_found_error() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        let response = app.handle_tab_prio(
            "req".into(),
            TabPrioParams {
                tab_id: Some("w_missing:t1".into()),
                mode: TabPrioMode::On,
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "tab_not_found");
        assert_eq!(error.error.message, "tab w_missing:t1 not found");
    }

    #[tokio::test]
    async fn api_tab_close_last_tab_replaces_tab_without_closing_workspace() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let workspace_id = app.public_workspace_id(0);
        let workspace_cwd = app.state.workspaces[0].identity_cwd.clone();
        let old_root = app.state.workspaces[0].tabs[0].root_pane;

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: tab_id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].id, workspace_id);
        assert_eq!(app.state.workspaces[0].identity_cwd, workspace_cwd);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_ne!(app.state.workspaces[0].tabs[0].root_pane, old_root);
        assert_eq!(app.state.active, Some(0));
        let replacement_terminal = app
            .state
            .terminal_id_for_pane(0, app.state.workspaces[0].tabs[0].root_pane)
            .unwrap();
        assert_eq!(
            app.state.terminals[&replacement_terminal].cwd,
            workspace_cwd
        );
        let events = event_hub.events_after(0);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            [
                EventKind::TabClosed,
                EventKind::TabCreated,
                EventKind::PaneCreated,
                EventKind::LayoutUpdated,
            ]
        );
        assert!(matches!(
            &events[0].1.data,
            EventData::TabClosed {
                tab_id: closed_tab_id,
                workspace_id: closed_workspace_id,
            } if closed_tab_id == &tab_id && closed_workspace_id == &workspace_id
        ));
        for (_sequence, event) in events {
            assert_ne!(event.event, EventKind::WorkspaceClosed);
        }
        for (_terminal_id, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[test]
    fn api_tab_move_reorders_tabs_in_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("two"));
        workspace.test_add_tab(Some("three"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_root = app.state.workspaces[0].tabs[0].root_pane;
        let moved_id = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_tab_move(
            "req".into(),
            TabMoveParams {
                tab_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabList { tabs } = success.result else {
            panic!("expected tab list");
        };
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(tabs[2].tab_id, app.public_tab_id(0, 2).unwrap());
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index: 3,
                    tabs,
                } if tab_id == &moved_id
                    && workspace_id == &app.public_workspace_id(0)
                    && tabs[2].tab_id == moved_id
            )
        }));
    }

    #[test]
    fn api_tab_rename_reflows_active_tab_bar() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        let workspace = Workspace::test_new("tabs");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.view.tab_bar_rect = ratatui::layout::Rect::new(0, 0, 60, 1);
        app.state.refresh_tab_bar_view();

        let tab_id = app.public_tab_id(0, 0).unwrap();
        let width_before = app.state.view.tab_hit_areas[0].width;

        app.handle_tab_rename(
            "req".into(),
            TabRenameParams {
                tab_id,
                label: Some("a much longer custom tab label".into()),
            },
        );

        let width_after = app.state.view.tab_hit_areas[0].width;
        assert!(
            width_after > width_before,
            "tab bar should reflow to the new label width immediately: \
             before={width_before}, after={width_after}"
        );
    }

    #[test]
    fn api_tab_rename_clear_restores_the_derived_label() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        let workspace = Workspace::test_new("tabs");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.handle_tab_rename(
            "req".into(),
            TabRenameParams {
                tab_id: tab_id.clone(),
                label: Some("pinned by hand".into()),
            },
        );
        assert!(!app.state.workspaces[0].tabs[0].is_auto_named());

        app.handle_tab_rename(
            "req".into(),
            TabRenameParams {
                tab_id,
                label: None,
            },
        );

        // A user name outranks every derived source, so clearing it is the only
        // way a renamed tab can follow its agent's session name again.
        let tab = &app.state.workspaces[0].tabs[0];
        assert!(tab.is_auto_named());
        assert_eq!(tab.name_origin, crate::workspace::TabNameOrigin::Structural);
    }

    #[test]
    fn api_tab_pin_toggle_flips_pinned_flag() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        let workspace = Workspace::test_new("tabs");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        let tab_id = app.public_tab_id(0, 0).unwrap();
        assert!(!app.state.workspaces[0].tabs[0].pinned);

        let response = app.handle_tab_pin(
            "req".into(),
            TabPinParams {
                tab_id: tab_id.clone(),
                mode: TabPinMode::Toggle,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::TabInfo { .. }));
        assert!(app.state.workspaces[0].tabs[0].pinned);

        app.handle_tab_pin(
            "req2".into(),
            TabPinParams {
                tab_id: tab_id.clone(),
                mode: TabPinMode::Toggle,
            },
        );
        assert!(!app.state.workspaces[0].tabs[0].pinned);

        app.handle_tab_pin(
            "req3".into(),
            TabPinParams {
                tab_id,
                mode: TabPinMode::Pin,
            },
        );
        assert!(app.state.workspaces[0].tabs[0].pinned);
    }

    #[test]
    fn sidebar_pin_api_invalidates_the_sidebar_projection() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        let tab_id = app.public_tab_id(0, 0).expect("tab id");
        let revision = app.state.sidebar_projection_revision;

        app.handle_tab_pin(
            "req".into(),
            TabPinParams {
                tab_id,
                mode: TabPinMode::Pin,
            },
        );

        assert!(app.state.sidebar_projection_revision > revision);
    }

    #[test]
    fn api_tab_star_toggles_sets_and_clears_the_focus_star() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        app.state.active = Some(0);
        app.state.selected = 0;

        let tab_id = app.public_tab_id(0, 0).unwrap();
        assert!(!app.state.workspaces[0].tabs[0].starred);

        let response = app.handle_tab_star(
            "req".into(),
            TabStarParams {
                tab_id: tab_id.clone(),
                mode: TabStarMode::Toggle,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        match success.result {
            ResponseResult::TabInfo { tab } => assert!(tab.starred, "response reports the star"),
            other => panic!("expected tab info, got {other:?}"),
        }
        assert!(app.state.workspaces[0].tabs[0].starred);

        app.handle_tab_star(
            "req2".into(),
            TabStarParams {
                tab_id: tab_id.clone(),
                mode: TabStarMode::Toggle,
            },
        );
        assert!(!app.state.workspaces[0].tabs[0].starred);

        app.handle_tab_star(
            "req3".into(),
            TabStarParams {
                tab_id: tab_id.clone(),
                mode: TabStarMode::Star,
            },
        );
        assert!(app.state.workspaces[0].tabs[0].starred);

        app.handle_tab_star(
            "req4".into(),
            TabStarParams {
                tab_id,
                mode: TabStarMode::Unstar,
            },
        );
        assert!(!app.state.workspaces[0].tabs[0].starred);
    }

    #[test]
    fn api_tab_star_on_an_unknown_tab_returns_structured_error() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.workspaces = vec![Workspace::test_new("tabs")];

        let response = app.handle_tab_star(
            "req".into(),
            TabStarParams {
                tab_id: "w9_tabs:t9".into(),
                mode: TabStarMode::Toggle,
            },
        );
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "tab_not_found");
    }

    #[test]
    fn api_tab_pin_projection_divergence_returns_structured_error() {
        let response = tab_info_response("req".into(), "w1_tabs:t1", None);
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "tab_not_found");
    }

    #[tokio::test]
    async fn tab_create_follows_cached_focused_pane_cwd_without_runtime() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let workspace = Workspace::test_new("tabs");
        let focused_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let cached_cwd = std::env::temp_dir();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .cloned()
            .unwrap();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cached_cwd.clone();

        let response = app.handle_tab_create(
            "req".into(),
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
                work_context: None,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::TabCreated { .. }));
        let created = &app.state.workspaces[0].tabs[1];
        let created_terminal_id = created.terminal_id(created.root_pane).unwrap();
        let created_cwd = &app.state.terminals.get(created_terminal_id).unwrap().cwd;
        assert_eq!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&cached_cwd)
        );
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn tab_create_returns_second_pr_run_as_history() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        app.state.workspaces = vec![Workspace::test_new("owner")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let owner_pane = app.state.workspaces[0].tabs[0].root_pane;
        let owner_terminal_id = app.state.workspaces[0]
            .terminal_id(owner_pane)
            .cloned()
            .unwrap();
        let owner = crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/o/r/pull/42".into()],
            role: Some(crate::work_context::PaneWorkRole::Ship),
            active_owner: true,
            ..Default::default()
        }
        .normalized_spawn_binding()
        .unwrap();
        app.state
            .terminals
            .get_mut(&owner_terminal_id)
            .unwrap()
            .replace_prevalidated_manual_work_context(owner);

        let response = app.handle_tab_create(
            "req".into(),
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
                work_context: Some(crate::work_context::PaneWorkContext {
                    pr_urls: vec!["https://github.com/o/r/pull/42".into()],
                    role: Some(crate::work_context::PaneWorkRole::Review),
                    active_owner: true,
                    ..Default::default()
                }),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabCreated { root_pane, .. } = success.result else {
            panic!("expected tab_created response");
        };
        assert_eq!(
            root_pane.work_context.role,
            Some(crate::work_context::PaneWorkRole::Review)
        );
        assert!(!root_pane.work_context.active_owner);
        assert!(
            app.state.terminals[&owner_terminal_id]
                .effective_work_context()
                .active_owner
        );
        shutdown_test_runtimes(&mut app);
    }
}
