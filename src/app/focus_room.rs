use std::path::PathBuf;
use std::time::Instant;

use crate::layout::PaneId;
use crate::workspace::Workspace;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FocusRoomUiState {
    pub(crate) leave_dialog_open: bool,
    pub(crate) leave_selection: usize,
    pub(crate) drawer: Option<FocusRoomDrawerState>,
    pub(crate) geometry: FocusRoomGeometry,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FocusRoomGeometry {
    pub(crate) bar: ratatui::layout::Rect,
    pub(crate) pane: ratatui::layout::Rect,
    pub(crate) find_hit_area: ratatui::layout::Rect,
    pub(crate) ops_hit_area: ratatui::layout::Rect,
    pub(crate) return_hit_area: ratatui::layout::Rect,
    pub(crate) dialog_rect: ratatui::layout::Rect,
    pub(crate) finder_rect: ratatui::layout::Rect,
    pub(crate) drawer_rect: ratatui::layout::Rect,
    pub(crate) dialog_row_hit_areas: Vec<ratatui::layout::Rect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusRoomDrawerState {
    pub(crate) pane_id: Option<String>,
    pub(crate) agent_ref: Option<crate::api::schema::AgentRef>,
    pub(crate) title: String,
    pub(crate) agent_label: String,
    pub(crate) host: String,
    pub(crate) remote: bool,
    pub(crate) lines: Vec<String>,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) output_revision: u64,
    pub(crate) refreshed_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusEnterResult {
    pub(crate) tab_id: String,
    pub(crate) pane_id: String,
    pub(crate) created: bool,
}

fn resolve_vellum(value: &str) -> Option<PathBuf> {
    let expanded = crate::worktree::expand_tilde_path(value);
    if expanded.is_absolute() || expanded.components().count() > 1 {
        let candidate = if expanded.is_absolute() {
            expanded
        } else {
            std::env::current_dir().ok()?.join(expanded)
        };
        return candidate.is_file().then_some(candidate);
    }
    let current_dir = std::env::current_dir().ok();
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(&expanded);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            current_dir.as_ref()?.join(candidate)
        };
        candidate.is_file().then_some(candidate)
    })
}

pub(crate) fn restore_focus_room_launch_argv(
    vellum: &str,
    focus_dir: &std::path::Path,
) -> Option<Vec<String>> {
    let vellum = resolve_vellum(vellum)?;
    Some(vec![
        vellum.to_string_lossy().into_owned(),
        "--focus".into(),
        "--theme".into(),
        "auto".into(),
        focus_dir.to_string_lossy().into_owned(),
    ])
}

fn current_public_tab_id(state: &crate::app::state::AppState) -> Option<String> {
    let workspace = state.workspaces.get(state.active?)?;
    let tab = workspace.tabs.get(workspace.active_tab_index())?;
    Some(crate::workspace::public_tab_id_for_number(
        &workspace.id,
        tab.number,
    ))
}

pub(crate) fn find_tab(
    state: &crate::app::state::AppState,
    tab_id: &str,
) -> Option<(usize, usize, PaneId)> {
    state
        .workspaces
        .iter()
        .enumerate()
        .find_map(|(ws_idx, workspace)| {
            workspace
                .tabs
                .iter()
                .enumerate()
                .find(|(_, tab)| {
                    crate::workspace::public_tab_id_for_number(&workspace.id, tab.number) == tab_id
                })
                .map(|(tab_idx, tab)| (ws_idx, tab_idx, tab.root_pane))
        })
}

impl crate::app::App {
    pub(crate) fn append_focus_room_drawer_text(
        &mut self,
        owner: crate::app::state::InputOwner,
        text: &str,
    ) -> bool {
        if !self.state.focus_room.active
            || self.state.capture.is_some()
            || owner == crate::app::state::InputOwner::Pomodoro
        {
            return false;
        }
        if self.state.focus_room_ui.leave_dialog_open {
            return true;
        }
        if self.state.focus_room_ui.drawer.is_none() {
            return false;
        }
        if let Some(drawer) = self.state.focus_room_ui.drawer.as_mut() {
            if !drawer.remote {
                drawer
                    .input
                    .extend(text.chars().filter(|character| !character.is_control()));
                drawer.cursor = drawer.input.chars().count();
            }
        }
        true
    }

    pub(crate) fn focus_room_tab_will_disappear(&mut self, tab_id: &str) {
        if self.state.focus_room.tab_id.as_deref() != Some(tab_id) {
            return;
        }
        if self.state.focus_room.active {
            self.leave_focus_room();
        }
        self.state.forget_focus_room_tab(tab_id);
        self.schedule_session_save();
    }

    pub(crate) fn handle_focus_room_key(&mut self, raw: crate::input::TerminalKey) -> bool {
        use crate::app::Mode;
        use crossterm::event::{KeyCode, KeyModifiers};

        let key = raw.as_key_event();
        if self.state.server_mode() == Mode::Prefix {
            return self.handle_focus_room_prefix_key(raw);
        }
        if self.state.is_prefix_key(&raw) {
            self.state.set_server_mode(Mode::Prefix);
            return true;
        }

        if self.state.focus_room_ui.leave_dialog_open {
            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.state.focus_room_ui.leave_dialog_open = false;
                }
                KeyCode::Up if key.modifiers.is_empty() => {
                    self.state.focus_room_ui.leave_selection =
                        self.state.focus_room_ui.leave_selection.saturating_sub(1);
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    self.state.focus_room_ui.leave_selection =
                        (self.state.focus_room_ui.leave_selection + 1).min(2);
                }
                KeyCode::Char('c') if key.modifiers.is_empty() => {
                    self.activate_focus_room_leave_choice(0);
                }
                KeyCode::Char('f') if key.modifiers.is_empty() => {
                    self.activate_focus_room_leave_choice(1);
                }
                KeyCode::Char('o') if key.modifiers.is_empty() => {
                    self.activate_focus_room_leave_choice(2);
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    self.activate_focus_room_leave_choice(self.state.focus_room_ui.leave_selection);
                }
                _ => {}
            }
            return true;
        }

        if self.state.agent_finder_saved_query.is_some() {
            if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
                return true;
            }
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                if let Some(hit) = self
                    .state
                    .agent_finder_results
                    .get(self.state.agent_finder_selected)
                    .cloned()
                {
                    self.open_focus_room_drawer(&hit);
                }
                return true;
            }
            let _ = self.state.handle_sidebar_search_key(key);
            return true;
        }

        let direct_action =
            crate::app::input::terminal_direct_non_indexed_navigation_action(&self.state, &raw)
                .or_else(|| {
                    crate::app::input::terminal_direct_indexed_navigation_action(&self.state, &raw)
                });
        if let Some(action) =
            direct_action.filter(|action| crate::app::input::focus_room_action_allowed(*action))
        {
            self.execute_tui_navigate_action(action, crate::app::input::ActionContext::Direct);
            return true;
        }

        if self.state.focus_room_ui.drawer.is_some() {
            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.state.focus_room_ui.drawer = None;
                    return true;
                }
                KeyCode::Char('q')
                    if key.modifiers.is_empty()
                        && self
                            .state
                            .focus_room_ui
                            .drawer
                            .as_ref()
                            .is_some_and(|drawer| drawer.input.is_empty()) =>
                {
                    self.state.focus_room_ui.drawer = None;
                    return true;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    let Some(drawer) = self.state.focus_room_ui.drawer.as_ref() else {
                        return true;
                    };
                    if drawer.remote || drawer.input.is_empty() {
                        return true;
                    }
                    let public_pane_id = drawer.pane_id.clone();
                    let text = drawer.input.clone();
                    let sent = public_pane_id
                        .and_then(|id| self.parse_pane_id(&id))
                        .is_some_and(|(ws_idx, pane_id)| {
                            self.send_text_to_agent_pane(ws_idx, pane_id, &text)
                        });
                    if sent {
                        if let Some(drawer) = self.state.focus_room_ui.drawer.as_mut() {
                            drawer.input.clear();
                            drawer.cursor = 0;
                        }
                    }
                    return true;
                }
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    if let Some(drawer) = self.state.focus_room_ui.drawer.as_mut() {
                        drawer.input.pop();
                        drawer.cursor = drawer.input.chars().count();
                    }
                    return true;
                }
                KeyCode::Char(character)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) && !character.is_control() =>
                {
                    if let Some(drawer) = self.state.focus_room_ui.drawer.as_mut() {
                        if !drawer.remote {
                            drawer.input.push(character);
                            drawer.cursor = drawer.input.chars().count();
                        }
                    }
                    return true;
                }
                _ => return true,
            }
        }

        if self.state.keybinds.focus_leave.matches_direct_key(&raw) {
            self.open_focus_room_leave_dialog();
            return true;
        }
        let direct_action =
            crate::app::input::terminal_direct_non_indexed_navigation_action(&self.state, &raw)
                .or_else(|| {
                    crate::app::input::terminal_direct_indexed_navigation_action(&self.state, &raw)
                });
        if let Some(action) = direct_action {
            if crate::app::input::focus_room_action_allowed(action) {
                self.execute_tui_navigate_action(action, crate::app::input::ActionContext::Direct);
            }
            return true;
        }
        if crate::app::input::focus_room_direct_other_action(&self.state, &raw) {
            return true;
        }
        false
    }

    pub(crate) fn activate_focus_room_leave_choice(&mut self, choice: usize) {
        self.state.focus_room_ui.leave_dialog_open = false;
        match choice {
            0 => self.state.open_capture(),
            1 => {
                self.state.focus_room_ui.drawer = None;
                self.state.open_agent_finder();
            }
            _ => self.leave_focus_room(),
        }
    }

    pub(crate) fn handle_focus_room_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return false;
        }
        let geometry = self.state.focus_room_ui.geometry.clone();
        let point = ratatui::layout::Rect::new(mouse.column, mouse.row, 1, 1);
        if self.state.focus_room_ui.leave_dialog_open {
            if !geometry.dialog_rect.intersects(point) {
                self.state.focus_room_ui.leave_dialog_open = false;
                return true;
            }
            if let Some(choice) = geometry
                .dialog_row_hit_areas
                .iter()
                .position(|row| row.intersects(point))
            {
                self.activate_focus_room_leave_choice(choice);
            }
            return true;
        }
        if self.state.focus_room.active && geometry.find_hit_area.intersects(point) {
            self.state.focus_room_ui.drawer = None;
            self.state.open_agent_finder();
            return true;
        }
        if self.state.focus_room.active && geometry.ops_hit_area.intersects(point) {
            self.open_focus_room_leave_dialog();
            return true;
        }
        if !self.state.focus_room.active && geometry.return_hit_area.intersects(point) {
            let _ = self.enter_focus_room();
            return true;
        }
        false
    }

    pub(crate) fn open_focus_room_leave_dialog(&mut self) {
        self.state.focus_room_ui.leave_dialog_open = true;
        self.state.focus_room_ui.leave_selection = 2;
    }

    pub(crate) fn open_focus_room_drawer(&mut self, hit: &crate::api::schema::AgentSearchHit) {
        let agent_ref = hit.agent.agent_ref.clone();
        let host = agent_ref
            .as_ref()
            .map(|agent_ref| agent_ref.host.clone())
            .unwrap_or_else(|| self.state.agent_host_name.clone());
        let remote = host != self.state.agent_host_name;
        let display = hit
            .agent
            .display_agent
            .clone()
            .or_else(|| hit.agent.agent.clone())
            .unwrap_or_else(|| "agent".into());
        let agent_label = crate::ui::sidebar::short_agent_label(&display).unwrap_or(display);
        self.state.focus_room_ui.drawer = Some(FocusRoomDrawerState {
            pane_id: (!remote).then(|| hit.agent.pane_id.clone()),
            agent_ref,
            title: hit.title.clone(),
            agent_label,
            host,
            remote,
            lines: if remote {
                hit.preview.clone()
            } else {
                Vec::new()
            },
            input: String::new(),
            cursor: 0,
            output_revision: 0,
            refreshed_at: None,
        });
        if self.state.agent_finder_saved_query.is_some() {
            self.state.close_agent_finder();
        }
        self.state.sidebar_search_active = false;
    }

    pub(crate) fn refresh_focus_room_drawer(&mut self) -> bool {
        let Some(drawer) = self.state.focus_room_ui.drawer.as_ref() else {
            return false;
        };
        if drawer.remote {
            return false;
        }
        if drawer
            .refreshed_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(250))
        {
            return false;
        }
        let Some(public_pane_id) = drawer.pane_id.clone() else {
            self.state.focus_room_ui.drawer = None;
            return true;
        };
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&public_pane_id) else {
            self.state.focus_room_ui.drawer = None;
            return true;
        };
        let previous_revision = drawer.output_revision;
        let had_lines = !drawer.lines.is_empty();
        let snapshot = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .map(|runtime| {
                let revision = runtime.content_revision();
                let text =
                    (revision != previous_revision || !had_lines).then(|| runtime.visible_text());
                (revision, text)
            });
        let Some((revision, text)) = snapshot else {
            self.state.focus_room_ui.drawer = None;
            return true;
        };
        let now = std::time::Instant::now();
        let Some(drawer) = self.state.focus_room_ui.drawer.as_mut() else {
            return false;
        };
        drawer.refreshed_at = Some(now);
        if let Some(text) = text {
            drawer.lines = text.lines().map(str::to_owned).collect();
            while drawer
                .lines
                .last()
                .is_some_and(|line| line.trim().is_empty())
            {
                drawer.lines.pop();
            }
            drawer.output_revision = revision;
            true
        } else {
            false
        }
    }

    /// Enter the persistent Focus Room, creating its single Vellum pane once.
    pub(crate) fn enter_focus_room(&mut self) -> Result<FocusEnterResult, String> {
        let now = crate::app::settled::unix_seconds(std::time::SystemTime::now());
        if self.state.planning_lock.is_locked(now)
            && self
                .state
                .focus_room
                .tab_id
                .as_deref()
                .is_none_or(|tab_id| !self.state.planning_lock.permits_tab(tab_id, now))
        {
            return Err("planning lock is on".into());
        }

        let mut created = false;
        let existing = self
            .state
            .focus_room
            .tab_id
            .as_deref()
            .and_then(|tab_id| find_tab(&self.state, tab_id))
            .filter(|(ws_idx, _tab_idx, pane_id)| {
                self.state.workspaces[*ws_idx]
                    .pane_state(*pane_id)
                    .is_some_and(|pane| {
                        self.state
                            .terminals
                            .contains_key(&pane.attached_terminal_id)
                            && self
                                .terminal_runtimes
                                .get(&pane.attached_terminal_id)
                                .is_some_and(|runtime| !runtime.child_has_exited())
                    })
            });
        let (ws_idx, tab_idx, pane_id) = if let Some(existing) = existing {
            existing
        } else {
            let vellum = resolve_vellum(&self.state.focus_vellum)
                .ok_or_else(|| format!("vellum not found: {}", self.state.focus_vellum))?;
            let focus_dir = self.state.focus_dir.clone();
            crate::platform::create_private_dir_all(&focus_dir)
                .map_err(|error| format!("could not create focus directory: {error}"))?;
            let argv = vec![
                vellum.to_string_lossy().into_owned(),
                "--focus".into(),
                "--theme".into(),
                "auto".into(),
                focus_dir.to_string_lossy().into_owned(),
            ];
            let (rows, cols) = self.state.estimate_pane_size();
            let (mut workspace, terminal, runtime) = Workspace::new_argv_command(
                focus_dir.clone(),
                rows.max(1),
                cols.max(1),
                &argv,
                self.state.pane_scrollback_limit_bytes,
                self.state.pane_terminal_theme(),
                Some(self.state.pane_terminal_appearance()),
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            )
            .map_err(|error| format!("could not launch vellum: {error}"))?;
            workspace.custom_name = Some("focus".into());
            workspace.tabs[0].custom_name = Some("focus".into());
            workspace.tabs[0].name_origin = crate::workspace::TabNameOrigin::User;
            let pane_id = workspace.tabs[0].root_pane;
            let tab_id = crate::workspace::public_tab_id_for_number(&workspace.id, 1);
            self.terminal_runtimes.insert(terminal.id.clone(), runtime);
            self.state.terminals.insert(terminal.id.clone(), terminal);
            self.state.workspaces.push(workspace);
            let ws_idx = self.state.workspaces.len() - 1;
            self.pending_first_frame_pane = Some(pane_id);
            self.state.remove_alias_shadowed_by_new_pane(pane_id);
            self.state.focus_room.tab_id = Some(tab_id);
            self.emit_workspace_open_events(ws_idx);
            created = true;
            (ws_idx, 0, pane_id)
        };

        if !self.state.focus_room.active {
            self.state
                .focus_room
                .enter(current_public_tab_id(&self.state));
        }
        self.state.focus_room.active = true;
        let tab = self.state.workspaces[ws_idx].tabs[tab_idx].number;
        self.state.focus_room.tab_id = Some(crate::workspace::public_tab_id_for_number(
            &self.state.workspaces[ws_idx].id,
            tab,
        ));
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        if let Some(index) = self.state.workspaces[ws_idx].tabs[tab_idx]
            .layout
            .pane_ids()
            .iter()
            .position(|candidate| *candidate == pane_id)
        {
            self.state.selected = index;
        }
        if self.state.scratch.open {
            self.tick_scratch(std::time::Instant::now(), true);
        }
        self.state.close_top_bar_views();
        self.state.navigator = crate::app::state::NavigatorState::default();
        self.state.command_palette = crate::app::state::CommandPaletteState::default();
        self.state.set_server_mode(crate::app::Mode::Terminal);
        self.close_popup_pane();
        if self.state.agent_finder_saved_query.is_some() {
            self.state.close_agent_finder();
        }
        if self.state.capture.is_some() {
            self.persist_capture_draft();
            self.state.capture = None;
        }
        if let Some(dock) = self.state.spawn_dock.take() {
            if let Some(client_id) = self
                .state
                .sidebar_presentation
                .spawn_dock_client_id
                .as_deref()
            {
                crate::client::presentation::save_spawn_dock_draft(client_id, Some(dock.draft()));
            }
        }
        self.state.close_client_overlay();
        self.state.client_overlay = crate::app::state::ClientOverlay::None;
        self.state.context_menu = None;
        self.state.sidebar_snooze = None;
        self.state.rename_target = None;
        self.state.confirm_close_workspace_id = None;
        self.state.confirm_close_remote_agent_ref = None;
        self.state.name_input.clear();
        self.state.name_input_caret = 0;
        self.state.name_input_replace_on_type = false;
        self.state.creating_new_tab = false;
        self.state.pending_workspace_create_cwd = None;
        self.state.rename_tab_prefill = None;
        self.state.worktree_create = None;
        self.state.worktree_open = None;
        self.state.worktree_remove = None;
        self.state.status_more_open = false;
        self.state.window_cycle_menu_open = false;
        self.state.sidebar_group_menu_open = false;
        self.state.sidebar_filter_menu_open = false;
        self.state.sidebar_blocker_scope_menu_open = false;
        self.state.sidebar_search_active = false;
        self.state.sidebar_new_menu = None;
        self.state.sidebar_new_thread = None;
        self.state.sidebar_areas_menu_selected = None;
        self.state.sidebar_project_menu = None;
        self.state.sidebar_object_menu = None;
        self.state.sidebar_sort_menu = None;
        self.state.sidebar_subgroup_picker = None;
        self.state.sidebar_pod_picker = None;
        self.state.sidebar_settled_menu_target = None;
        self.state.sidebar_settled_menu_selected = 0;
        self.state.sidebar_settled_menu_delete_armed = false;
        self.state.agent_picker = None;
        self.state.dock_surface_menu = None;
        self.state.dock_pr_checkout_menu = None;
        self.state.dock_pr_action_menu = None;
        self.state.dock_ticket_start_menu = None;
        self.state.dock_ticket_action_menu = None;
        self.state.focus_room_ui.leave_dialog_open = false;
        self.state.focus_room_ui.drawer = None;
        self.focus_client_on_pane();
        self.state.mark_session_dirty();
        self.schedule_session_save();
        let workspace = &self.state.workspaces[ws_idx];
        if workspace.pane_state(pane_id).is_none() {
            return Err("focus pane disappeared".into());
        }
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace.id,
            workspace.tabs[tab_idx].number,
        );
        let pane_id = self
            .public_pane_id(ws_idx, pane_id)
            .ok_or("focus pane has no public identity")?;
        Ok(FocusEnterResult {
            tab_id,
            pane_id,
            created,
        })
    }

    pub(crate) fn leave_focus_room(&mut self) {
        let Some(room_tab_id) = self.state.focus_room.tab_id.clone() else {
            self.state.focus_room.active = false;
            self.state.focus_room_ui = FocusRoomUiState::default();
            if self.state.agent_finder_saved_query.is_some() {
                self.state.close_agent_finder();
            }
            return;
        };
        let return_to = self.state.focus_room.leave().map(str::to_owned);
        if self.state.agent_finder_saved_query.is_some() {
            self.state.close_agent_finder();
        }
        let return_tab = return_to
            .as_deref()
            .and_then(|id| find_tab(&self.state, id))
            .or_else(|| {
                self.state
                    .workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(ws_idx, workspace)| {
                        workspace
                            .tabs
                            .iter()
                            .enumerate()
                            .find_map(|(tab_idx, tab)| {
                                (crate::workspace::public_tab_id_for_number(
                                    &workspace.id,
                                    tab.number,
                                ) != room_tab_id)
                                    .then_some((ws_idx, tab_idx, tab.root_pane))
                            })
                    })
            });
        self.state.focus_room_ui = FocusRoomUiState::default();
        if let Some((ws_idx, tab_idx, pane_id)) = return_tab {
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            if let Some(index) = self.state.workspaces[ws_idx].tabs[tab_idx]
                .layout
                .pane_ids()
                .iter()
                .position(|candidate| *candidate == pane_id)
            {
                self.state.selected = index;
            }
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
    }

    pub(crate) fn restore_focus_room(&mut self) {
        self.restore_focus_room_inner(true);
    }

    #[cfg(unix)]
    pub(crate) fn restore_focus_room_without_relaunch(&mut self) {
        self.restore_focus_room_inner(false);
    }

    fn restore_focus_room_inner(&mut self, recreate_missing: bool) {
        if !self.state.focus_room.active {
            let stale_tab_id = self
                .state
                .focus_room
                .tab_id
                .as_deref()
                .is_some_and(|tab_id| find_tab(&self.state, tab_id).is_none());
            if stale_tab_id {
                self.state.focus_room.tab_disappeared();
                self.state.mark_session_dirty();
                self.schedule_session_save();
            }
            return;
        }
        let restored = self
            .state
            .focus_room
            .tab_id
            .as_deref()
            .and_then(|tab_id| find_tab(&self.state, tab_id))
            .filter(|(ws_idx, _tab_idx, pane_id)| {
                self.state.workspaces[*ws_idx]
                    .pane_state(*pane_id)
                    .is_some_and(|pane| {
                        self.state
                            .terminals
                            .contains_key(&pane.attached_terminal_id)
                            && self
                                .terminal_runtimes
                                .get(&pane.attached_terminal_id)
                                .is_some_and(|runtime| !runtime.child_has_exited())
                    })
            });
        if let Some((ws_idx, tab_idx, pane_id)) = restored {
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            if let Some(index) = self.state.workspaces[ws_idx].tabs[tab_idx]
                .layout
                .pane_ids()
                .iter()
                .position(|candidate| *candidate == pane_id)
            {
                self.state.selected = index;
            }
        } else {
            self.state.focus_room.tab_id = None;
            if recreate_missing {
                if let Err(error) = self.enter_focus_room() {
                    tracing::warn!(%error, "could not recreate restored focus room");
                    self.state.focus_room.tab_disappeared();
                    self.state.mark_session_dirty();
                }
            } else {
                self.state.focus_room.tab_disappeared();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn missing_vellum_refuses_before_creating_a_workspace() {
        let mut app = test_app();
        app.state.focus_vellum = format!("herdr-missing-vellum-{}", std::process::id());
        let error = app.enter_focus_room().unwrap_err();
        assert!(error.starts_with("vellum not found:"));
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn planning_lock_refuses_focus_entry_before_resolving_vellum() {
        let mut app = test_app();
        app.state
            .planning_lock
            .apply_remote_snapshot(Some(crate::planning_lock::Snapshot {
                locked: true,
                discussion_tab_id: "work:tab-1".into(),
                unlock_until_unix_s: None,
            }));
        app.state.focus_vellum = "missing-vellum-for-lock-test".into();
        assert_eq!(app.enter_focus_room().unwrap_err(), "planning lock is on");
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn focus_room_leave_dialog_swallow_pasted_text_before_pane_fallback() {
        let mut app = test_app();
        app.state.focus_room.active = true;
        app.state.focus_room_ui.leave_dialog_open = true;

        assert!(
            app.append_focus_room_drawer_text(crate::app::state::InputOwner::Pane, "pasted text")
        );
        assert!(!app.append_focus_room_drawer_text(
            crate::app::state::InputOwner::Pomodoro,
            "break prompt"
        ));
        assert!(app.state.focus_room_ui.drawer.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn focus_room_launches_once_and_reuses_the_same_vellum_process() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "herdr-focus-room-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let vellum = dir.join("vellum");
        std::fs::write(&vellum, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
        std::fs::set_permissions(&vellum, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut app = test_app();
        app.state.focus_vellum = vellum.to_string_lossy().into_owned();
        app.state.focus_dir = dir.join("focus");
        let first = app.enter_focus_room().unwrap();
        assert!(first.created);
        let (first_ws, _, first_pane) = find_tab(&app.state, &first.tab_id).unwrap();
        let first_terminal_id = app.state.workspaces[first_ws]
            .terminal_id(first_pane)
            .expect("focus terminal ID")
            .clone();
        let first_runtime = app
            .terminal_runtimes
            .get(&first_terminal_id)
            .expect("focus pane runtime");
        let first_pid = first_runtime.child_pid().expect("Vellum process ID");
        let terminal = app
            .state
            .terminals
            .get(&first_terminal_id)
            .expect("focus terminal state");
        assert_eq!(
            terminal.launch_argv.as_deref(),
            Some(
                [
                    vellum.to_string_lossy().into_owned(),
                    "--focus".into(),
                    "--theme".into(),
                    "auto".into(),
                    app.state.focus_dir.to_string_lossy().into_owned(),
                ]
                .as_slice()
            )
        );
        let expected_argv = terminal.launch_argv.clone().expect("Vellum argv");
        assert_eq!(
            restore_focus_room_launch_argv(&app.state.focus_vellum, &app.state.focus_dir),
            Some(expected_argv.clone()),
            "cold restore argv must follow the current Focus config"
        );
        let mut snapshot = crate::persist::capture(
            &app.state.workspaces,
            &app.state.terminals,
            &app.terminal_runtimes,
            app.state.active,
            app.state.selected,
            app.state.sidebar_width,
            app.state.sidebar_section_split,
            app.state.collapsed_space_keys.clone(),
            app.state.prio_panel_collapsed,
            app.state.window_cycle_mode,
            app.state.skip_collapsed_cycle,
        );
        snapshot.focus_room = app.state.focus_room.snapshot();
        let saved_room_tab = snapshot.workspaces[first_ws].tabs[0]
            .root_pane
            .expect("focus room snapshot root pane");
        assert_eq!(
            snapshot.workspaces[first_ws].tabs[0].panes[&saved_room_tab]
                .launch_argv
                .as_deref(),
            Some(expected_argv.as_slice()),
            "session snapshot must retain the room pane launch command"
        );

        app.leave_focus_room();
        let second = app.enter_focus_room().unwrap();
        assert!(!second.created);
        assert_eq!(second.tab_id, first.tab_id);
        assert_eq!(second.pane_id, first.pane_id);
        let (second_ws, _, second_pane) = find_tab(&app.state, &second.tab_id).unwrap();
        let second_terminal_id = app.state.workspaces[second_ws]
            .terminal_id(second_pane)
            .expect("reused focus terminal ID");
        assert_eq!(second_terminal_id, &first_terminal_id);
        let second_pid = app
            .terminal_runtimes
            .get(second_terminal_id)
            .and_then(|runtime| runtime.child_pid())
            .expect("reused Vellum process ID");
        assert_eq!(second_pid, first_pid);

        let restore_events = app.event_tx.clone();
        let restore_notify = app.render_notify.clone();
        let restore_dirty = app.render_dirty.clone();
        let fallback_events = restore_events.clone();
        let fallback_notify = restore_notify.clone();
        let fallback_dirty = restore_dirty.clone();
        let restore_focus_dir = app.state.focus_dir.clone();
        drop(app);

        let (_, restored_terminals, restored_runtimes) = crate::persist::restore_with_focus_room(
            &snapshot,
            None,
            24,
            80,
            1024 * 1024,
            "sh",
            crate::config::ShellModeConfig::default(),
            false,
            Some(crate::persist::FocusRoomRestore {
                tab_id: &first.tab_id,
                cwd: Some(&restore_focus_dir),
                launch_argv: Some(&expected_argv),
            }),
            restore_events,
            restore_notify,
            restore_dirty,
        );
        let restored_terminal = restored_terminals
            .values()
            .next()
            .expect("restored room pane");
        assert_eq!(
            restored_terminal.launch_argv.as_deref(),
            Some(expected_argv.as_slice())
        );
        assert!(
            restored_runtimes
                .get(&restored_terminal.id)
                .and_then(|runtime| runtime.child_pid())
                .is_some(),
            "cold restore must spawn the configured Vellum process"
        );
        drop(restored_runtimes);

        let (_, shell_terminals, shell_runtimes) = crate::persist::restore_with_focus_room(
            &snapshot,
            None,
            24,
            80,
            1024 * 1024,
            "sh",
            crate::config::ShellModeConfig::default(),
            false,
            Some(crate::persist::FocusRoomRestore {
                tab_id: &first.tab_id,
                cwd: Some(&restore_focus_dir),
                launch_argv: None,
            }),
            fallback_events,
            fallback_notify,
            fallback_dirty,
        );
        let shell_terminal = shell_terminals.values().next().expect("room shell");
        assert!(shell_terminal.launch_argv.is_none());
        assert!(
            shell_runtimes
                .get(&shell_terminal.id)
                .and_then(|runtime| runtime.child_pid())
                .is_some(),
            "unresolved Vellum must leave a running room shell"
        );
        drop(shell_runtimes);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn closing_the_focus_workspace_clears_room_and_finder_state() {
        let mut app = test_app();
        let workspace = Workspace::test_new("focus");
        let tab_id =
            crate::workspace::public_tab_id_for_number(&workspace.id, workspace.tabs[0].number);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.focus_room.tab_id = Some(tab_id);
        app.state.focus_room.active = true;
        app.state.agent_finder_saved_query = Some(String::new());
        app.state.focus_room_ui.leave_dialog_open = true;

        app.state.close_selected_workspace();

        assert!(!app.state.focus_room.active);
        assert!(app.state.focus_room.tab_id.is_none());
        assert!(app.state.agent_finder_saved_query.is_none());
        assert!(!app.state.focus_room_ui.leave_dialog_open);
    }

    fn drawer(remote: bool) -> FocusRoomDrawerState {
        FocusRoomDrawerState {
            pane_id: (!remote).then(|| "work:pane-1".into()),
            agent_ref: None,
            title: "review".into(),
            agent_label: "cc".into(),
            host: if remote { "ub1" } else { "local" }.into(),
            remote,
            lines: vec!["last output".into()],
            input: String::new(),
            cursor: 0,
            output_revision: 0,
            refreshed_at: None,
        }
    }

    #[test]
    fn drawer_input_edits_text_and_q_only_closes_when_empty() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = test_app();
        app.state.focus_room.active = true;
        app.state.focus_room_ui.drawer = Some(drawer(false));
        app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        ));
        assert!(app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Char('q'),
            KeyModifiers::empty(),
        )));
        assert_eq!(
            app.state
                .focus_room_ui
                .drawer
                .as_ref()
                .map(|drawer| drawer.input.as_str()),
            Some("xq")
        );
        app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Backspace,
            KeyModifiers::empty(),
        ));
        app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Backspace,
            KeyModifiers::empty(),
        ));
        assert_eq!(app.state.focus_room_ui.drawer.as_ref().unwrap().input, "");
        app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Char('q'),
            KeyModifiers::empty(),
        ));
        assert!(app.state.focus_room_ui.drawer.is_none());
    }

    #[tokio::test]
    async fn drawer_enter_queues_the_prompt_text_and_clears_the_input() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut app = test_app();
        let workspace = Workspace::test_new("drawer-send");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 8,
            );
        app.terminal_runtimes.insert(terminal_id, runtime);
        let mut drawer = drawer(false);
        drawer.pane_id = app.public_pane_id(0, pane_id);
        drawer.input = "answer".into();
        drawer.cursor = drawer.input.chars().count();
        app.state.focus_room.active = true;
        app.state.focus_room_ui.drawer = Some(drawer);

        app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        ));

        assert_eq!(input_rx.try_recv().unwrap().as_ref(), b"answer");
        assert_eq!(app.state.focus_room_ui.drawer.as_ref().unwrap().input, "");
    }

    #[test]
    fn capture_opens_over_the_room_drawer() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut app = test_app();
        app.state.focus_room.active = true;
        app.state.keybinds.capture = crate::config::ActionKeybinds::prefix("t");
        app.state.focus_room_ui.drawer = Some(drawer(false));

        assert!(
            app.handle_focus_room_prefix_key(crate::input::TerminalKey::new(
                KeyCode::Char('t'),
                KeyModifiers::empty(),
            ))
        );

        assert!(app.state.capture.is_some());
        assert!(app.state.focus_room_ui.drawer.is_some());
    }

    #[test]
    fn direct_capture_binding_opens_in_focus_room_and_over_drawer() {
        use crossterm::event::{KeyCode, KeyModifiers};

        for drawer_open in [false, true] {
            let mut app = test_app();
            app.state.keybinds = crate::config::Config::default().keybinds();
            app.state.focus_room.active = true;
            if drawer_open {
                app.state.focus_room_ui.drawer = Some(drawer(false));
            }

            assert!(app.handle_focus_room_key(crate::input::TerminalKey::new(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            )));

            assert!(app.state.capture.is_some());
            assert_eq!(app.state.focus_room_ui.drawer.is_some(), drawer_open);
        }
    }

    #[test]
    fn remote_drawer_is_read_only_and_escape_closes_it() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = test_app();
        app.state.focus_room.active = true;
        app.state.focus_room_ui.drawer = Some(drawer(true));
        app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        ));
        assert_eq!(
            app.state
                .focus_room_ui
                .drawer
                .as_ref()
                .map(|drawer| drawer.input.as_str()),
            Some("")
        );
        app.handle_focus_room_key(crate::input::TerminalKey::new(
            KeyCode::Esc,
            KeyModifiers::empty(),
        ));
        assert!(app.state.focus_room_ui.drawer.is_none());
    }

    #[test]
    fn headless_text_commit_appends_to_the_drawer_instead_of_the_room_pane() {
        fn no_op_terminal_input(_: &crate::app::TerminalInputTarget) {}

        let mut app = test_app();
        app.state.focus_room.active = true;
        app.state.focus_room_ui.drawer = Some(drawer(false));
        let owner = app.state.input_owner();
        let mut before_terminal_input = no_op_terminal_input;

        app.handle_text_commit_headless_for_owner_with_hook(
            owner,
            "pasted\ntext",
            &mut before_terminal_input,
            None,
        );

        let drawer = app.state.focus_room_ui.drawer.as_ref().unwrap();
        assert_eq!(drawer.input, "pastedtext");
        assert_eq!(drawer.cursor, "pastedtext".chars().count());
    }
}
