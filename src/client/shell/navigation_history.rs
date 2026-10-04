use super::*;
use crate::config::NavigationHistoryScope;

const MAX_VISITS: usize = 256;
const MAX_QUEUED_MOVES: usize = 32;
const CONFIRMATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Stable client-only identity; labels, pane focus and tab order are not history.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Location {
    workspace_id: String,
    tab_id: String,
}

impl Location {
    fn focused(snapshot: &ClientShellSnapshot) -> Option<Self> {
        let tab_id = snapshot.focused_tab_id.as_ref()?;
        let workspace_id = snapshot.focused_workspace_id.as_ref()?;
        snapshot
            .tabs
            .iter()
            .any(|tab| &tab.tab_id == tab_id && &tab.workspace_id == workspace_id)
            .then(|| Self {
                workspace_id: workspace_id.clone(),
                tab_id: tab_id.clone(),
            })
    }
}

#[derive(Default)]
struct Visits {
    entries: Vec<Location>,
    cursor: usize,
}

impl Visits {
    fn record(&mut self, location: &Location) {
        if self.entries.get(self.cursor) == Some(location) {
            return;
        }
        self.entries.truncate(self.cursor.saturating_add(1));
        self.entries.push(location.clone());
        if self.entries.len() > MAX_VISITS {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() - 1;
    }

    fn reconcile(&mut self, tabs: &HashMap<&str, &str>, workspace: Option<&str>) {
        let mut cursor = 0;
        let mut retained = 0;
        let mut index = 0;
        self.entries.retain_mut(|entry| {
            let original_index = index;
            index += 1;
            let Some(&current_workspace) = tabs.get(entry.tab_id.as_str()) else {
                return false;
            };
            if workspace.is_some_and(|workspace| workspace != current_workspace) {
                return false;
            }
            if entry.workspace_id != current_workspace {
                entry.workspace_id = current_workspace.to_owned();
            }
            if original_index <= self.cursor {
                cursor = retained;
            }
            retained += 1;
            true
        });
        self.cursor = cursor.min(self.entries.len().saturating_sub(1));
    }

    fn destination(&self, back: bool) -> Option<(usize, Location)> {
        let current = self.entries.get(self.cursor)?;
        let mut index = self.cursor;
        loop {
            index = if back {
                index.checked_sub(1)?
            } else {
                index.checked_add(1)?
            };
            let destination = self.entries.get(index)?;
            // Closing intervening tabs can make separate visits adjacent again.
            // A same-tab focus cannot produce fresh confirmation evidence.
            if destination != current {
                return Some((index, destination.clone()));
            }
        }
    }
}

struct PendingMove {
    serial: u64,
    workspace: Option<String>,
    index: usize,
    destination: Location,
    expires_at: std::time::Instant,
}

#[derive(Default)]
struct EndpointHistory {
    boot_id: String,
    last_location: Option<Location>,
    across_spaces: Visits,
    spaces: HashMap<String, Visits>,
    pending: Option<PendingMove>,
    queued: VecDeque<bool>,
}

impl EndpointHistory {
    fn cancel(&mut self) {
        self.pending = None;
        self.queued.clear();
    }

    fn observe(&mut self, snapshot: &ClientShellSnapshot) {
        if self.boot_id != snapshot.boot_id {
            *self = Self {
                boot_id: snapshot.boot_id.clone(),
                ..Default::default()
            };
        }
        if self.pending.as_ref().is_some_and(|pending| {
            snapshot
                .tabs
                .iter()
                .find(|tab| tab.tab_id == pending.destination.tab_id)
                .is_none_or(|tab| {
                    pending
                        .workspace
                        .as_deref()
                        .is_some_and(|workspace| workspace != tab.workspace_id)
                })
        }) {
            self.cancel();
        }
        if self.last_location.as_ref().is_some_and(|location| {
            snapshot.focused_tab_id.as_deref() == Some(location.tab_id.as_str())
                && snapshot.focused_workspace_id.as_deref() == Some(location.workspace_id.as_str())
        }) {
            return;
        }
        let Some(location) = Location::focused(snapshot) else {
            return;
        };
        let replay = self.pending.take().filter(|pending| {
            // A tab can move while tab.focus is in flight. Across-space history
            // follows stable tab identity; a space-local intent must stay in scope.
            pending.destination.tab_id == location.tab_id
                && pending
                    .workspace
                    .as_deref()
                    .is_none_or(|workspace| workspace == location.workspace_id)
                && std::time::Instant::now() < pending.expires_at
        });
        if replay.is_none() {
            self.queued.clear();
        }
        let local = self
            .spaces
            .entry(location.workspace_id.clone())
            .or_default();
        match replay {
            Some(PendingMove {
                workspace: None,
                index,
                ..
            }) => {
                if let Some(entry) = self.across_spaces.entries.get_mut(index) {
                    entry.workspace_id.clone_from(&location.workspace_id);
                }
                self.across_spaces.cursor = index;
                local.record(&location);
            }
            Some(PendingMove {
                workspace: Some(_),
                index,
                ..
            }) => {
                local.cursor = index;
                self.across_spaces.record(&location);
            }
            None => {
                self.across_spaces.record(&location);
                local.record(&location);
            }
        }
        self.last_location = Some(location);
    }

    fn prepare(&mut self, snapshot: &ClientShellSnapshot) {
        let tabs = snapshot
            .tabs
            .iter()
            .map(|tab| (tab.tab_id.as_str(), tab.workspace_id.as_str()))
            .collect::<HashMap<_, _>>();
        self.across_spaces.reconcile(&tabs, None);
        self.spaces.retain(|workspace, visits| {
            visits.reconcile(&tabs, Some(workspace));
            !visits.entries.is_empty()
        });
    }
}

/// Each attached client has its own histories, partitioned by machine and boot.
#[derive(Default)]
pub(super) struct NavigationHistory {
    endpoints: HashMap<ClientEndpointId, EndpointHistory>,
    next_serial: u64,
}

impl NavigationHistory {
    pub(super) fn observe(&mut self, endpoint: &ClientEndpointId, snapshot: &ClientShellSnapshot) {
        if let Some(history) = self.endpoints.get_mut(endpoint) {
            history.observe(snapshot);
        } else {
            let mut history = EndpointHistory::default();
            history.observe(snapshot);
            self.endpoints.insert(endpoint.clone(), history);
        }
    }

    pub(super) fn cancel(&mut self, endpoint: &ClientEndpointId) {
        if let Some(history) = self.endpoints.get_mut(endpoint) {
            history.cancel();
        }
    }

    pub(super) fn fail(&mut self, endpoint: &ClientEndpointId, serial: u64) {
        if let Some(history) = self.endpoints.get_mut(endpoint) {
            if history
                .pending
                .as_ref()
                .is_some_and(|pending| pending.serial == serial)
            {
                history.cancel();
            }
        }
    }

    fn begin(
        &mut self,
        endpoint: &ClientEndpointId,
        snapshot: &ClientShellSnapshot,
        scope: NavigationHistoryScope,
        back: bool,
        now: std::time::Instant,
        from_queue: bool,
    ) -> Option<(u64, String)> {
        self.observe(endpoint, snapshot);
        let history = self.endpoints.get_mut(endpoint)?;
        if history.pending.is_some() || (!from_queue && !history.queued.is_empty()) {
            if history.queued.len() < MAX_QUEUED_MOVES {
                history.queued.push_back(back);
            }
            return None;
        }
        history.prepare(snapshot);
        let workspace = match scope {
            NavigationHistoryScope::AcrossSpaces => None,
            NavigationHistoryScope::CurrentSpace => Some(snapshot.focused_workspace_id.clone()?),
        };
        let visits = match workspace.as_ref() {
            Some(workspace) => history.spaces.get(workspace)?,
            None => &history.across_spaces,
        };
        let (index, destination) = visits.destination(back)?;
        self.next_serial = self.next_serial.saturating_add(1);
        let serial = self.next_serial;
        let tab_id = destination.tab_id.clone();
        history.pending = Some(PendingMove {
            serial,
            workspace,
            index,
            destination,
            expires_at: now + CONFIRMATION_TIMEOUT,
        });
        Some((serial, tab_id))
    }
}

impl ClientShellState {
    pub(super) fn navigate_history(&mut self, back: bool, outcome: &mut ClientShellInput) {
        self.dispatch_history(back, false, outcome);
    }

    fn history_navigation_blocked(&self) -> bool {
        self.overlay.is_some()
            || self.popup_terminal_id.is_some()
            || self.popup_pending
            || self.chrome_drag.is_some()
            || self.pane_mouse_gesture.is_some()
            || self.workspace_press.is_some()
            || self.tab_press.is_some()
            || self.has_active_mouse_selection()
    }

    fn dispatch_history(&mut self, back: bool, from_queue: bool, outcome: &mut ClientShellInput) {
        if self.history_navigation_blocked() {
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some((serial, tab_id)) = self.navigation_history.begin(
            &self.active_endpoint_id,
            snapshot,
            self.config.navigation_history_scope,
            back,
            std::time::Instant::now(),
            from_queue,
        ) else {
            return;
        };
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget { tab_id }),
            PendingEndpointKind::NavigationHistory { serial },
            outcome,
        ) {
            self.navigation_history
                .fail(&self.active_endpoint_id, serial);
        } else {
            if self.copy_mode.is_some() {
                self.exit_copy_mode(false, outcome);
            }
            outcome.repaint |=
                self.mode != ClientShellMode::Terminal || self.navigate_workspace_id.is_some();
            self.mode = ClientShellMode::Terminal;
            self.navigate_workspace_id = None;
        }
    }

    pub(crate) fn tick_navigation_history(&mut self, now: std::time::Instant) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        let blocked = self.history_navigation_blocked();
        let Some(history) = self
            .navigation_history
            .endpoints
            .get_mut(&self.active_endpoint_id)
        else {
            return outcome;
        };
        if history
            .pending
            .as_ref()
            .is_some_and(|pending| now >= pending.expires_at)
        {
            history.cancel();
        }
        if !blocked && history.pending.is_none() {
            if let Some(back) = history.queued.pop_front() {
                self.dispatch_history(back, true, &mut outcome);
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection() -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        for (workspace, tab) in [("ws_1", "tab_2"), ("ws_2", "tab_3"), ("ws_2", "tab_4")] {
            let mut entry = snapshot.tabs[0].clone();
            entry.tab_id = tab.into();
            entry.workspace_id = workspace.into();
            snapshot.tabs.push(entry);
        }
        let mut workspace = snapshot.workspaces[0].clone();
        workspace.workspace_id = "ws_2".into();
        workspace.active_tab_id = "tab_3".into();
        snapshot.workspaces.push(workspace);
        snapshot
    }

    fn visit(snapshot: &mut ClientShellSnapshot, tab: &str) {
        snapshot.revision += 1;
        snapshot.focused_tab_id = Some(tab.into());
        snapshot.focused_workspace_id = Some(
            snapshot
                .tabs
                .iter()
                .find(|entry| entry.tab_id == tab)
                .unwrap()
                .workspace_id
                .clone(),
        );
    }

    fn state_at(tabs: &[&str]) -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let mut snapshot = projection();
        for tab in tabs {
            visit(&mut snapshot, tab);
            state.set_snapshot(Box::new(snapshot.clone()));
        }
        state
    }

    fn request(outcome: &ClientShellInput) -> (&str, &str) {
        let [ClientShellAction::Endpoint { request, .. }] = outcome.actions.as_slice() else {
            panic!("expected one focus request");
        };
        let crate::api::schema::Method::TabFocus(target) = &request.method else {
            panic!("expected tab.focus");
        };
        (&request.id, &target.tab_id)
    }

    fn navigate(state: &mut ClientShellState, back: bool) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        state.navigate_history(back, &mut outcome);
        outcome
    }

    fn confirm(state: &mut ClientShellState, tab: &str) {
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        visit(&mut snapshot, tab);
        state.set_snapshot(Box::new(snapshot));
    }

    #[test]
    fn browser_history_preserves_repeated_occurrences_and_truncates_new_branches() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_1", "tab_3"]);
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
        confirm(&mut state, "tab_1");
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        confirm(&mut state, "tab_2");
        assert_eq!(request(&navigate(&mut state, false)).1, "tab_1");
        confirm(&mut state, "tab_1");
        confirm(&mut state, "tab_4");
        assert!(navigate(&mut state, false).actions.is_empty());
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
    }

    #[test]
    fn current_space_histories_resume_and_scope_switch_keeps_both_histories() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_3", "tab_4", "tab_2"]);
        state.config.navigation_history_scope = NavigationHistoryScope::CurrentSpace;
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
        confirm(&mut state, "tab_1");
        assert!(navigate(&mut state, true).actions.is_empty());
        state.config.navigation_history_scope = NavigationHistoryScope::AcrossSpaces;
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        confirm(&mut state, "tab_2");
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_4");
        confirm(&mut state, "tab_4");
        state.config.navigation_history_scope = NavigationHistoryScope::CurrentSpace;
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_3");
    }

    #[test]
    fn rapid_moves_are_queued_until_confirmed_even_when_final_location_repeats() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_1"]);
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        assert!(navigate(&mut state, true).actions.is_empty());
        assert!(state
            .tick_navigation_history(std::time::Instant::now())
            .actions
            .is_empty());
        assert_eq!(
            state.navigation_history.endpoints[&ClientEndpointId::Local]
                .across_spaces
                .cursor,
            2
        );
        confirm(&mut state, "tab_2");
        let outcome = state.tick_navigation_history(std::time::Instant::now());
        assert_eq!(request(&outcome).1, "tab_1");
        confirm(&mut state, "tab_1");
        let history = &state.navigation_history.endpoints[&ClientEndpointId::Local];
        assert_eq!(history.across_spaces.cursor, 0);
        assert_eq!(history.across_spaces.entries.len(), 3);
    }

    #[test]
    fn mixed_moves_keep_fifo_order_when_input_arrives_between_confirmation_and_tick() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_3"]);
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        assert!(navigate(&mut state, true).actions.is_empty());
        confirm(&mut state, "tab_2");
        assert!(navigate(&mut state, false).actions.is_empty());
        assert_eq!(
            request(&state.tick_navigation_history(std::time::Instant::now())).1,
            "tab_1"
        );
        confirm(&mut state, "tab_1");
        assert_eq!(
            request(&state.tick_navigation_history(std::time::Instant::now())).1,
            "tab_2"
        );
        confirm(&mut state, "tab_2");
        assert_eq!(
            state.navigation_history.endpoints[&ClientEndpointId::Local]
                .across_spaces
                .cursor,
            1
        );
        assert_eq!(
            state.navigation_history.endpoints[&ClientEndpointId::Local]
                .across_spaces
                .entries
                .len(),
            3
        );
    }

    #[test]
    fn queued_moves_wait_for_dialogs_and_drags_without_losing_fifo_order() {
        for dialog in [true, false] {
            let mut state = state_at(&["tab_1", "tab_2", "tab_3", "tab_4"]);
            assert_eq!(request(&navigate(&mut state, true)).1, "tab_3");
            navigate(&mut state, true);
            navigate(&mut state, false);
            if dialog {
                state.open_settings_overlay();
            } else {
                state.chrome_drag = Some(ClientChromeDrag::SidebarWidth);
            }
            confirm(&mut state, "tab_3");
            for _ in 0..3 {
                assert!(state
                    .tick_navigation_history(std::time::Instant::now())
                    .actions
                    .is_empty());
            }
            state.overlay = None;
            state.chrome_drag = None;
            assert_eq!(
                request(&state.tick_navigation_history(std::time::Instant::now())).1,
                "tab_2"
            );
            confirm(&mut state, "tab_2");
            assert_eq!(
                request(&state.tick_navigation_history(std::time::Instant::now())).1,
                "tab_3"
            );
            confirm(&mut state, "tab_3");
            assert!(state
                .tick_navigation_history(std::time::Instant::now())
                .actions
                .is_empty());
        }
    }

    #[test]
    fn history_navigation_exits_keyboard_copy_selections() {
        for selection in [b"v", b"V"] {
            for mouse in [false, true] {
                let mut state = state_at(&["tab_1", "tab_2"]);
                let mut surface = super::super::tests::surface();
                surface.projection_revision = state.snapshot.as_deref().unwrap().revision;
                surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
                    offset_from_bottom: 0,
                    max_offset_from_bottom: 0,
                    viewport_rows: 2,
                });
                state.set_pane_surface(surface);
                state.compose(106, 20).unwrap();
                state.handle_input_bytes(b"\x02[");
                state.handle_input_bytes(selection);
                assert!(state.copy_mode.as_ref().unwrap().selection.is_some());
                assert!(state.selection.as_ref().unwrap().is_in_progress());
                let outcome = state.handle_input_bytes(if mouse {
                    b"\x1b[<128;5;3M"
                } else {
                    b"\x02\x1b[D"
                });
                assert!(outcome.actions.iter().any(|action| matches!(
                    action,
                    ClientShellAction::Endpoint { request, .. }
                        if matches!(&request.method, crate::api::schema::Method::TabFocus(target)
                            if target.tab_id == "tab_1")
                )));
                assert!(state.copy_mode.is_none());
                assert!(state.selection.is_none());
                assert_eq!(state.mode, ClientShellMode::Terminal);
            }
        }
    }

    #[test]
    fn rejection_timeout_cancellation_and_missing_method_do_not_advance_cursor() {
        for code in ["invalid_target", "endpoint_timeout", "endpoint_cancelled"] {
            let mut state = state_at(&["tab_1", "tab_2", "tab_3"]);
            let outcome = navigate(&mut state, true);
            let id = request(&outcome).0.to_owned();
            assert!(navigate(&mut state, true).actions.is_empty());
            state.handle_endpoint_result(
                "boot-1",
                &id,
                Err(ClientShellEndpointError {
                    code: Some(code.into()),
                    message: "test rejection".into(),
                }),
            );
            assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        }
        let mut state = state_at(&["tab_1", "tab_2", "tab_3"]);
        state.set_endpoint_methods_for(&ClientEndpointId::Local, Some(Vec::new()));
        assert!(navigate(&mut state, true).actions.is_empty());
        state.set_endpoint_methods_for(&ClientEndpointId::Local, None);
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        let now = std::time::Instant::now() + CONFIRMATION_TIMEOUT;
        assert!(state.tick_navigation_history(now).actions.is_empty());
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
    }

    #[test]
    fn ordinary_selection_supersedes_history_and_late_failure_does_not_cancel_new_request() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_3"]);
        let first = navigate(&mut state, true);
        let id = request(&first).0.to_owned();
        navigate(&mut state, true);
        state.focus_endpoint_target(ClientEndpointFocusTarget::Tab("tab_4".into()));
        confirm(&mut state, "tab_4");
        assert!(state
            .tick_navigation_history(std::time::Instant::now())
            .actions
            .is_empty());
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_3");
        state.handle_endpoint_result(
            "boot-1",
            &id,
            Err(ClientShellEndpointError {
                code: Some("endpoint_timeout".into()),
                message: "late".into(),
            }),
        );
        assert!(state.navigation_history.endpoints[&ClientEndpointId::Local]
            .pending
            .is_some());
    }

    #[test]
    fn deleted_tabs_are_skipped_and_transferred_tabs_follow_current_membership() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_3"]);
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        snapshot.tabs.retain(|tab| tab.tab_id != "tab_2");
        snapshot.tabs[0].workspace_id = "ws_2".into();
        snapshot.revision += 1;
        state.set_snapshot(Box::new(snapshot));
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
        confirm(&mut state, "tab_1");
        assert!(navigate(&mut state, true).actions.is_empty());
    }

    #[test]
    fn deleting_visits_before_a_middle_cursor_preserves_both_directions() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_3", "tab_4"]);
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_3");
        confirm(&mut state, "tab_3");
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        snapshot.tabs.retain(|tab| tab.tab_id != "tab_1");
        snapshot.revision += 1;
        state.set_snapshot(Box::new(snapshot));
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        confirm(&mut state, "tab_2");
        assert_eq!(request(&navigate(&mut state, false)).1, "tab_3");
        confirm(&mut state, "tab_3");
        assert_eq!(request(&navigate(&mut state, false)).1, "tab_4");
    }

    #[test]
    fn closing_intervening_tabs_does_not_dispatch_same_tab_focus() {
        let mut state = state_at(&["tab_3", "tab_1", "tab_2", "tab_1"]);
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        snapshot.tabs.retain(|tab| tab.tab_id != "tab_2");
        snapshot.revision += 1;
        state.set_snapshot(Box::new(snapshot));
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_3");
        confirm(&mut state, "tab_3");
        assert_eq!(request(&navigate(&mut state, false)).1, "tab_1");
        confirm(&mut state, "tab_1");
        assert!(navigate(&mut state, false).actions.is_empty());
    }

    #[test]
    fn in_flight_cross_space_transfer_preserves_requested_occurrence_and_queue() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_3"]);
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        assert!(navigate(&mut state, true).actions.is_empty());
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        snapshot
            .tabs
            .iter_mut()
            .find(|tab| tab.tab_id == "tab_2")
            .unwrap()
            .workspace_id = "ws_2".into();
        visit(&mut snapshot, "tab_2");
        state.set_snapshot(Box::new(snapshot));
        let history = &state.navigation_history.endpoints[&ClientEndpointId::Local];
        assert_eq!(history.across_spaces.cursor, 1);
        assert_eq!(history.across_spaces.entries.len(), 3);
        assert_eq!(history.across_spaces.entries[1].workspace_id, "ws_2");
        assert_eq!(
            request(&state.tick_navigation_history(std::time::Instant::now())).1,
            "tab_1"
        );
    }

    #[test]
    fn in_flight_space_local_transfer_cancels_intent_that_leaves_scope() {
        let mut state = state_at(&["tab_1", "tab_2"]);
        state.config.navigation_history_scope = NavigationHistoryScope::CurrentSpace;
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
        assert!(navigate(&mut state, false).actions.is_empty());
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        snapshot.tabs[0].workspace_id = "ws_2".into();
        visit(&mut snapshot, "tab_1");
        state.set_snapshot(Box::new(snapshot));
        let history = &state.navigation_history.endpoints[&ClientEndpointId::Local];
        assert!(history.pending.is_none());
        assert!(history.queued.is_empty());
        assert_eq!(history.spaces["ws_1"].cursor, 1);
        assert!(state
            .tick_navigation_history(std::time::Instant::now())
            .actions
            .is_empty());
        assert!(navigate(&mut state, true).actions.is_empty());
    }

    #[test]
    fn deleted_in_flight_destination_releases_queue_without_waiting_for_timeout() {
        let mut state = state_at(&["tab_1", "tab_2", "tab_3"]);
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_2");
        navigate(&mut state, true);
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        snapshot.tabs.retain(|tab| tab.tab_id != "tab_2");
        snapshot.revision += 1;
        state.set_snapshot(Box::new(snapshot));
        assert!(state
            .tick_navigation_history(std::time::Instant::now())
            .actions
            .is_empty());
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
    }

    #[test]
    fn pane_focus_is_ignored_and_history_memory_is_bounded() {
        let mut history = NavigationHistory::default();
        let mut snapshot = projection();
        for index in 0..600 {
            visit(
                &mut snapshot,
                if index % 2 == 0 { "tab_1" } else { "tab_2" },
            );
            history.observe(&ClientEndpointId::Local, &snapshot);
            snapshot.focused_pane_id = Some(format!("pane_{index}"));
            history.observe(&ClientEndpointId::Local, &snapshot);
        }
        let endpoint = &history.endpoints[&ClientEndpointId::Local];
        assert_eq!(endpoint.across_spaces.entries.len(), MAX_VISITS);
        assert_eq!(endpoint.spaces["ws_1"].entries.len(), MAX_VISITS);
        assert_eq!(endpoint.across_spaces.cursor, MAX_VISITS - 1);
    }

    #[test]
    fn histories_are_per_machine_and_survive_reconnect_but_not_server_restart() {
        let mut state = state_at(&["tab_1", "tab_2"]);
        let local_snapshot = state.snapshot.as_deref().unwrap().clone();
        let profile = SavedSshEndpoint {
            id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef")
                .unwrap(),
            label: "test".into(),
            target: "test@host".into(),
            session: "test".into(),
            enabled: true,
        };
        let remote = ClientEndpointId::Ssh(profile.id.clone());
        state.set_endpoint_catalog(&[profile]);
        state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
        let mut snapshot = projection();
        snapshot.boot_id = "remote-boot".into();
        state.set_endpoint_snapshot_for_generation(&remote, 1, Box::new(snapshot.clone()));
        assert!(state.activate_endpoint_projection(&remote));
        assert!(navigate(&mut state, true).actions.is_empty());
        visit(&mut snapshot, "tab_4");
        state.set_endpoint_snapshot_for_generation(&remote, 1, Box::new(snapshot));
        assert!(state.activate_endpoint_projection(&ClientEndpointId::Local));
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
        state.navigation_history.cancel(&ClientEndpointId::Local);
        state.apply_active_snapshot(Box::new(local_snapshot.clone()), Some(2));
        assert_eq!(request(&navigate(&mut state, true)).1, "tab_1");
        let mut restarted = local_snapshot;
        restarted.boot_id = "new-boot".into();
        state.apply_active_snapshot(Box::new(restarted), Some(3));
        assert!(navigate(&mut state, true).actions.is_empty());
    }

    #[test]
    fn prefix_keyboard_and_pixel_side_buttons_use_the_same_history() {
        let mut state = state_at(&["tab_1", "tab_2"]);
        assert!(state.handle_input_bytes(b"\x02").actions.is_empty());
        assert_eq!(request(&state.handle_input_bytes(b"\x1b[D")).1, "tab_1");
        confirm(&mut state, "tab_1");
        let geometry = crate::input::mouse::HostGeometry {
            cols: 100,
            rows: 40,
            width_px: 1000,
            height_px: 800,
        };
        assert_eq!(
            request(&state.handle_pixel_mouse(b"\x1b[<129;50;30M", geometry)).1,
            "tab_2"
        );
    }

    #[test]
    fn side_buttons_respect_disabled_setting_modal_and_active_drag() {
        let mut state = state_at(&["tab_1", "tab_2"]);
        state.config.mouse_history_navigation = false;
        assert!(state
            .handle_input_bytes(b"\x1b[<128;5;3M")
            .actions
            .is_empty());
        state.config.mouse_history_navigation = true;
        state.open_settings_overlay();
        assert!(state
            .handle_input_bytes(b"\x1b[<128;5;3M")
            .actions
            .is_empty());
        state.overlay = None;
        state.chrome_drag = Some(ClientChromeDrag::SidebarWidth);
        assert!(state
            .handle_input_bytes(b"\x1b[<128;5;3M")
            .actions
            .is_empty());
        state.chrome_drag = None;
        assert_eq!(
            request(&state.handle_input_bytes(b"\x1b[<128;5;3M")).1,
            "tab_1"
        );
        assert!(state
            .handle_input_bytes(b"\x1b[<128;5;3m")
            .actions
            .is_empty());
    }
}
