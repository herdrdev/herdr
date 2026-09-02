use super::*;
use crate::protocol::ClientPaneInputEvent;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

const LOCAL_INPUT_SOURCE: u8 = 0;

fn is_retained_selection_copy_key(key: &crate::input::TerminalKey) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C'))
        && matches!(key.modifiers, KeyModifiers::CONTROL | KeyModifiers::SUPER)
}

pub(super) fn is_modal_paste_shortcut_for_platform(
    key: &crate::input::TerminalKey,
    macos: bool,
) -> bool {
    matches!(key.code, KeyCode::Char('v' | 'V'))
        && (key.modifiers.contains(KeyModifiers::CONTROL)
            || macos && key.modifiers.contains(KeyModifiers::SUPER))
}

fn is_modal_paste_shortcut(key: &crate::input::TerminalKey) -> bool {
    is_modal_paste_shortcut_for_platform(key, cfg!(target_os = "macos"))
}

fn host_theme_update(event: &RawInputEvent) -> Option<crate::protocol::ClientHostThemeUpdate> {
    use crate::protocol::{
        ClientHostAppearance, ClientHostDefaultColorKind, ClientHostThemeUpdate,
    };

    match event {
        RawInputEvent::HostDefaultColor { kind, color } => {
            Some(ClientHostThemeUpdate::DefaultColor {
                kind: match kind {
                    crate::terminal_theme::DefaultColorKind::Foreground => {
                        ClientHostDefaultColorKind::Foreground
                    }
                    crate::terminal_theme::DefaultColorKind::Background => {
                        ClientHostDefaultColorKind::Background
                    }
                },
                color: (*color).into(),
            })
        }
        RawInputEvent::HostPaletteColors { colors } => Some(ClientHostThemeUpdate::PaletteColors(
            colors
                .iter()
                .map(|(index, color)| (*index, (*color).into()))
                .collect(),
        )),
        RawInputEvent::HostColorSchemeChanged(appearance) => {
            Some(ClientHostThemeUpdate::Appearance(match appearance {
                crate::terminal_theme::HostAppearance::Dark => ClientHostAppearance::Dark,
                crate::terminal_theme::HostAppearance::Light => ClientHostAppearance::Light,
            }))
        }
        _ => None,
    }
}

impl ClientShellState {
    pub(crate) fn host_keyboard_report_all_requested(&self) -> bool {
        matches!(
            self.mode,
            ClientShellMode::Prefix | ClientShellMode::Navigate
        )
    }

    #[cfg(any(unix, test))]
    pub(crate) fn handle_input_bytes(&mut self, data: &[u8]) -> ClientShellInput {
        self.handle_raw_events(crate::raw_input::parse_raw_input_bytes_sync(data))
    }

    #[cfg(any(unix, test))]
    pub(crate) fn handle_pixel_mouse(
        &mut self,
        data: &[u8],
        geometry: crate::input::mouse::HostGeometry,
    ) -> ClientShellInput {
        let Some((x, y)) = crate::input::mouse::parse_report(data) else {
            return ClientShellInput::default();
        };
        let Some((column, row)) = geometry.cell(x, y) else {
            return ClientShellInput::default();
        };
        let Some(cell_report) = crate::input::mouse::report_at_cell(data, column, row) else {
            return ClientShellInput::default();
        };
        let events = crate::raw_input::parse_raw_input_bytes_sync(&cell_report);
        if events.len() != 1 || !matches!(events[0], RawInputEvent::Mouse(_)) {
            return ClientShellInput::default();
        }
        self.host_mouse_pixels = Some(crate::input::mouse::HostPixels { x, y, geometry });
        let outcome = self.handle_raw_events(events);
        self.host_mouse_pixels = None;
        outcome
    }

    #[cfg(windows)]
    pub(crate) fn handle_client_events(
        &mut self,
        events: &[crate::protocol::ClientInputEvent],
    ) -> ClientShellInput {
        self.handle_raw_events(
            events
                .iter()
                .map(crate::protocol::ClientInputEvent::to_raw_input_event)
                .collect(),
        )
    }

    fn prepare_committed_text(&mut self, text: &str, outcome: &mut ClientShellInput) -> bool {
        if self.insert_copy_search_text(text) {
            outcome.repaint = true;
            return true;
        }
        self.pending_word_selection = None;
        if self.copy_or_terminal_mode() != ClientShellMode::Copy && self.selection.take().is_some()
        {
            self.stop_selection_autoscroll();
            self.selection_highlight_clear_deadline = None;
            outcome.repaint = true;
        }
        false
    }

    pub(crate) fn replay_mouse_events(
        &mut self,
        events: Vec<crossterm::event::MouseEvent>,
    ) -> ClientShellInput {
        self.replaying_url_click = true;
        let outcome =
            self.handle_raw_events(events.into_iter().map(RawInputEvent::Mouse).collect());
        self.replaying_url_click = false;
        outcome
    }

    pub(super) fn handle_raw_events(&mut self, events: Vec<RawInputEvent>) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        if !events.is_empty() && self.endpoint_error.take().is_some() {
            outcome.repaint = true;
        }
        for event in events {
            if let Some(update) = host_theme_update(&event) {
                outcome
                    .requests
                    .push(ClientMessage::ClientShellHostTheme { update });
            }
            match event {
                RawInputEvent::Key(key) => self.handle_key(key, &mut outcome),
                RawInputEvent::Text(text) => {
                    let text = text.into_string();
                    if matches!(
                        self.overlay,
                        Some(
                            ClientShellOverlay::Onboarding
                                | ClientShellOverlay::ProductAnnouncement(_)
                                | ClientShellOverlay::ReleaseNotes(_)
                        )
                    ) {
                        self.reconcile_input_source();
                        continue;
                    }
                    if self.prepare_committed_text(&text, &mut outcome) {
                        self.reconcile_input_source();
                        continue;
                    }
                    if let Some(target) = self.popup_input_target() {
                        super::push_target_event(
                            target,
                            ClientPaneInputEvent::TextCommit(text),
                            &mut outcome,
                        );
                    } else if !self.popup_pending {
                        if self.insert_overlay_text(&text) {
                            outcome.repaint = true;
                        } else if self.overlay.is_none() && self.mode == ClientShellMode::Terminal {
                            self.push_focused_pane_event(
                                ClientPaneInputEvent::TextCommit(text),
                                &mut outcome,
                            );
                        }
                    }
                }
                RawInputEvent::Paste(text) => {
                    if matches!(
                        self.overlay,
                        Some(
                            ClientShellOverlay::Onboarding
                                | ClientShellOverlay::ProductAnnouncement(_)
                                | ClientShellOverlay::ReleaseNotes(_)
                        )
                    ) {
                        self.reconcile_input_source();
                        continue;
                    }
                    if self.prepare_committed_text(&text, &mut outcome) {
                        self.reconcile_input_source();
                        continue;
                    }
                    if let Some(target) = self.popup_input_target() {
                        super::push_target_event(
                            target,
                            ClientPaneInputEvent::Paste(text),
                            &mut outcome,
                        );
                    } else if !self.popup_pending {
                        if self.insert_overlay_text(&text) {
                            outcome.repaint = true;
                        } else if self.overlay.is_none() && self.mode == ClientShellMode::Terminal {
                            self.push_focused_pane_event(
                                ClientPaneInputEvent::Paste(text),
                                &mut outcome,
                            );
                        }
                    }
                }
                RawInputEvent::Mouse(mouse) => self.handle_mouse(mouse, &mut outcome),
                RawInputEvent::OuterFocusGained => {
                    self.outer_focused = Some(true);
                    outcome.query_host_appearance = true;
                    outcome.repaint |= self.config.redraw_on_focus_gained;
                    outcome
                        .requests
                        .push(ClientMessage::ClientShellFocus { focused: true });
                }
                RawInputEvent::OuterFocusLost => {
                    self.outer_focused = Some(false);
                    self.release_input_leases(&mut outcome);
                    outcome
                        .requests
                        .push(ClientMessage::ClientShellFocus { focused: false });
                }
                RawInputEvent::HostColorSchemeChanged(appearance) => {
                    self.host_appearance = Some(appearance);
                    self.host_appearance_explicit = true;
                    outcome.query_host_theme = true;
                    if self.config.theme_runtime.auto_switch {
                        self.config.palette = crate::app::client_palette_for_appearance(
                            &self.config.theme_runtime,
                            appearance,
                        );
                        outcome.repaint = true;
                    }
                }
                RawInputEvent::HostDefaultColor {
                    kind: crate::terminal_theme::DefaultColorKind::Background,
                    color,
                } if !self.host_appearance_explicit => {
                    let appearance = color.inferred_appearance();
                    self.host_appearance = Some(appearance);
                    if self.config.theme_runtime.auto_switch {
                        self.config.palette = crate::app::client_palette_for_appearance(
                            &self.config.theme_runtime,
                            appearance,
                        );
                        outcome.repaint = true;
                    }
                }
                RawInputEvent::HostDefaultColor { .. }
                | RawInputEvent::HostPaletteColors { .. }
                | RawInputEvent::HostCellSizeReport { .. }
                | RawInputEvent::Unsupported => {}
            }
            self.reconcile_input_source();
        }
        outcome.repaint |= self.resume_mobile_switcher_if_ready();
        outcome
    }

    pub(super) fn handle_key(
        &mut self,
        key: crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        if self.copy_operation_in_flight {
            self.copy_input_queue.push_back(key);
            return;
        }
        let lease_key = crate::input::InputLeaseKey::new(LOCAL_INPUT_SOURCE, &key);
        let key = self.input_leases.normalize_press(&lease_key, key);
        match key.kind {
            KeyEventKind::Press => {
                let initial_context = self.input_context();
                let target = self.route_key_press(&key, outcome);
                if let Some(target) = target.as_ref() {
                    self.push_pane_key(target.clone(), key.clone(), outcome);
                }
                let resulting_context = self.input_context();
                let plan = self.input_leases.complete_press(
                    lease_key,
                    &key,
                    Some(&initial_context),
                    Some(&resulting_context),
                    target,
                );
                self.execute_repeat_plan(lease_key, key, plan, outcome);
            }
            KeyEventKind::Repeat => {
                let context = self.input_context();
                let plan = self
                    .input_leases
                    .plan_repeat(lease_key, &key, Some(&context));
                self.execute_repeat_plan(lease_key, key, plan, outcome);
            }
            KeyEventKind::Release => {
                if let Some(lease) = self.input_leases.remove_forwarded(&lease_key) {
                    let release = lease
                        .key
                        .with_modifiers(key.modifiers)
                        .with_kind(KeyEventKind::Release);
                    self.push_pane_key(lease.target, release, outcome);
                } else {
                    let _ = self.input_leases.remove(&lease_key);
                }
            }
        }
    }

    fn release_input_leases(&mut self, outcome: &mut ClientShellInput) {
        for lease in self.input_leases.remove_source(LOCAL_INPUT_SOURCE) {
            self.push_pane_key(
                lease.target,
                lease.key.with_kind(KeyEventKind::Release),
                outcome,
            );
        }
        if let Some(gesture) = self.pane_mouse_gesture.take() {
            let modifiers = gesture
                .last_event
                .modifiers
                .difference(gesture.stripped_modifiers);
            let geometry = matches!(
                gesture.last_position,
                crate::protocol::ClientMousePosition::Pixels { .. }
            )
            .then_some(crate::protocol::ClientMouseGeometry {
                cols: gesture.hit.inner_rect.width,
                rows: gesture.hit.inner_rect.height,
                width_px: gesture.hit.pixel_width,
                height_px: gesture.hit.pixel_height,
            });
            let target = if gesture.hit.popup {
                ClientInputTarget::Popup(gesture.hit.pane_id)
            } else {
                ClientInputTarget::Pane(gesture.hit.pane_id)
            };
            super::push_target_event(
                target,
                ClientPaneInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Up(
                        crate::protocol::ClientMouseButton::from_crossterm(gesture.button),
                    ),
                    position: gesture.last_position,
                    geometry,
                    modifiers: modifiers.bits(),
                    lines: self.config.mouse_scroll_lines.min(u16::MAX as usize) as u16,
                },
                outcome,
            );
        }
        self.copy_input_queue.clear();
    }

    fn execute_repeat_plan(
        &mut self,
        lease_key: crate::input::InputLeaseKey<u8>,
        key: crate::input::TerminalKey,
        plan: crate::input::RepeatPlan<ClientInputContext, ClientInputTarget>,
        outcome: &mut ClientShellInput,
    ) {
        match plan {
            crate::input::RepeatPlan::Forwarded(target) => {
                let pane_blocked_by_popup = matches!(&target, ClientInputTarget::Pane(_))
                    && (self.popup_pending || self.popup_terminal_id.is_some());
                if !pane_blocked_by_popup {
                    self.push_pane_key(target, key, outcome);
                }
            }
            crate::input::RepeatPlan::Reprocess {
                context,
                repetitions,
                tracked,
            } => {
                for _ in 0..repetitions {
                    let current = self.input_context();
                    if !self.input_leases.reprocess_allowed(
                        lease_key,
                        &context,
                        Some(&current),
                        tracked,
                    ) {
                        break;
                    }
                    let repeated = key
                        .clone()
                        .with_repeat_count(1)
                        .with_kind(KeyEventKind::Repeat);
                    if let Some(target) = self.route_key_press(&repeated, outcome) {
                        self.push_pane_key(target, repeated, outcome);
                    }
                }
            }
            crate::input::RepeatPlan::Ignore => {}
        }
    }

    pub(super) fn modal_paste_target_active(&self) -> bool {
        if self.popup_pending || self.popup_input_target().is_some() {
            return false;
        }
        if self
            .copy_mode
            .as_ref()
            .is_some_and(|copy_mode| copy_mode.search_prompt.is_some())
        {
            return self.overlay.is_none();
        }
        matches!(
            self.overlay.as_ref(),
            Some(ClientShellOverlay::Rename(_))
                | Some(ClientShellOverlay::WorktreeCreate(
                    ClientWorktreeCreateOverlay {
                        creating: false,
                        ..
                    }
                ))
                | Some(ClientShellOverlay::WorktreeOpen(
                    ClientWorktreeOpenOverlay {
                        search_focused: true,
                        opening: false,
                        ..
                    }
                ))
                | Some(ClientShellOverlay::Navigator(ClientNavigatorOverlay {
                    search_focused: true,
                    ..
                }))
                | Some(ClientShellOverlay::Help(ClientHelpOverlay {
                    search_focused: true,
                    ..
                }))
        )
    }

    pub(super) fn handle_modal_paste_shortcut_with(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
        read_clipboard_text: impl FnOnce() -> Option<String>,
    ) -> bool {
        if !is_modal_paste_shortcut(key) || !self.modal_paste_target_active() {
            return false;
        }
        if let Some(text) = read_clipboard_text() {
            let inserted = self.insert_copy_search_text(&text) || self.insert_overlay_text(&text);
            outcome.repaint |= inserted;
        }
        true
    }

    fn route_key_press(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> Option<ClientInputTarget> {
        if self.handle_modal_paste_shortcut_with(key, outcome, crate::platform::read_clipboard_text)
        {
            return None;
        }
        if matches!(
            self.overlay,
            Some(
                ClientShellOverlay::Onboarding
                    | ClientShellOverlay::ProductAnnouncement(_)
                    | ClientShellOverlay::ReleaseNotes(_)
            )
        ) {
            if key.kind == KeyEventKind::Press {
                self.route_overlay_key(key, outcome);
            }
            return None;
        }
        if let Some(target) = self.popup_input_target() {
            return Some(target);
        }
        if self.popup_pending {
            return None;
        }
        if self.overlay.is_some() {
            self.route_overlay_key(key, outcome);
            return None;
        }
        if matches!(key.code, KeyCode::Modifier(_)) {
            return None;
        }
        self.pending_word_selection = None;
        if self.mode != ClientShellMode::Copy
            && self.copy_or_terminal_mode() != ClientShellMode::Copy
            && !self.config.copy_on_select
            && is_retained_selection_copy_key(key)
            && self
                .selection
                .as_ref()
                .is_some_and(crate::selection::Selection::is_visible)
        {
            self.request_selection_copy_with_fallback(outcome, Some(key.clone()));
            self.selection = None;
            self.stop_selection_autoscroll();
            self.selection_highlight_clear_deadline = None;
            outcome.repaint = true;
            return None;
        }
        if self.mode != ClientShellMode::Copy
            && self.copy_or_terminal_mode() != ClientShellMode::Copy
            && self.selection.take().is_some()
        {
            self.stop_selection_autoscroll();
            self.selection_highlight_clear_deadline = None;
            outcome.repaint = true;
        }

        match self.mode {
            ClientShellMode::Terminal => {
                if let Some(binding) =
                    crate::input::resolve_direct_binding(&self.config.keybinds.keybinds, key)
                {
                    self.record_binding(binding, outcome);
                    return None;
                }
                if crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix) {
                    self.mode = ClientShellMode::Prefix;
                    outcome.repaint = true;
                    return None;
                }
                self.focused_pane_id().map(ClientInputTarget::Pane)
            }
            ClientShellMode::Prefix => {
                let return_mode = if self.copy_mode.as_ref().is_some_and(|copy_mode| {
                    self.focused_pane_id().as_deref() == Some(copy_mode.pane_id.as_str())
                }) {
                    ClientShellMode::Copy
                } else {
                    ClientShellMode::Terminal
                };
                if crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix) {
                    self.mode = return_mode;
                    outcome.repaint = true;
                    return self.focused_pane_id().map(ClientInputTarget::Pane);
                }
                if key.code == KeyCode::Esc {
                    self.mode = return_mode;
                    outcome.repaint = true;
                    return None;
                }
                if let Some(binding) =
                    crate::input::resolve_prefix_binding(&self.config.keybinds.keybinds, key)
                {
                    self.mode = return_mode;
                    outcome.repaint = true;
                    self.record_binding(binding, outcome);
                    return None;
                }
                self.mode = return_mode;
                outcome.repaint = true;
                None
            }
            ClientShellMode::Navigate => {
                self.route_navigate_key(key, outcome);
                None
            }
            ClientShellMode::Resize => {
                self.route_resize_key(key, outcome);
                None
            }
            ClientShellMode::Copy => {
                if crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix) {
                    self.mode = ClientShellMode::Prefix;
                    outcome.repaint = true;
                } else {
                    self.route_copy_mode_key(key, outcome);
                }
                None
            }
        }
    }

    pub(super) fn copy_or_terminal_mode(&self) -> ClientShellMode {
        if self.copy_mode.as_ref().is_some_and(|copy_mode| {
            self.focused_pane_id().as_deref() == Some(copy_mode.pane_id.as_str())
        }) {
            ClientShellMode::Copy
        } else {
            ClientShellMode::Terminal
        }
    }

    fn route_navigate_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::{KeybindAction, KeybindDispatch, KeybindMatch};

        if key.code == KeyCode::Esc
            || crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix)
        {
            self.mode = self.copy_or_terminal_mode();
            self.navigate_workspace_id = None;
            outcome.repaint = true;
            return;
        }

        if self
            .config
            .keybinds
            .keybinds
            .navigate
            .workspace_up
            .matches_direct_key(key)
        {
            self.move_navigate_workspace(-1);
            outcome.repaint = true;
            return;
        }
        if self
            .config
            .keybinds
            .keybinds
            .navigate
            .workspace_down
            .matches_direct_key(key)
        {
            self.move_navigate_workspace(1);
            outcome.repaint = true;
            return;
        }

        if let Some(index) = ('1'..='9').position(|digit| {
            crate::config::terminal_key_matches_combo(
                key,
                (KeyCode::Char(digit), KeyModifiers::empty()),
            )
        }) {
            let valid = self.snapshot.as_deref().is_some_and(|snapshot| {
                self.navigation_workspace_entries(snapshot)
                    .get(index)
                    .is_some()
            });
            if valid {
                self.mode = ClientShellMode::Terminal;
                self.navigate_workspace_id = None;
                self.record_binding(
                    KeybindMatch::Action(KeybindAction::SwitchWorkspace(index)),
                    outcome,
                );
                outcome.repaint = true;
            }
            return;
        }

        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        if modifiers.is_empty() {
            match code {
                KeyCode::Enter => {
                    let selected = self.navigate_workspace_id.clone();
                    self.mode = ClientShellMode::Terminal;
                    self.navigate_workspace_id = None;
                    if let Some(workspace_id) = selected {
                        self.push_endpoint_method(
                            crate::api::schema::Method::WorkspaceFocus(
                                crate::api::schema::WorkspaceTarget { workspace_id },
                            ),
                            outcome,
                        );
                    }
                    outcome.repaint = true;
                    return;
                }
                KeyCode::Tab => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::CyclePaneNext),
                        false,
                        outcome,
                    );
                    return;
                }
                KeyCode::BackTab => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::CyclePanePrevious),
                        false,
                        outcome,
                    );
                    return;
                }
                KeyCode::Left => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::FocusPaneLeft),
                        true,
                        outcome,
                    );
                    return;
                }
                KeyCode::Right => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::FocusPaneRight),
                        true,
                        outcome,
                    );
                    return;
                }
                _ => {}
            }
        }

        let pane_action = [
            (
                &self.config.keybinds.keybinds.navigate.pane_left,
                KeybindAction::FocusPaneLeft,
            ),
            (
                &self.config.keybinds.keybinds.navigate.pane_down,
                KeybindAction::FocusPaneDown,
            ),
            (
                &self.config.keybinds.keybinds.navigate.pane_up,
                KeybindAction::FocusPaneUp,
            ),
            (
                &self.config.keybinds.keybinds.navigate.pane_right,
                KeybindAction::FocusPaneRight,
            ),
        ]
        .into_iter()
        .find_map(|(bindings, action)| bindings.matches_direct_key(key).then_some(action));
        if let Some(action) = pane_action {
            self.record_navigate_binding(KeybindMatch::Action(action), true, outcome);
            return;
        }

        let binding = crate::input::resolve_non_indexed_action(
            &self.config.keybinds.keybinds,
            key,
            KeybindDispatch::Prefix,
        )
        .filter(|action| {
            !matches!(
                action,
                KeybindAction::FocusPaneLeft
                    | KeybindAction::FocusPaneDown
                    | KeybindAction::FocusPaneUp
                    | KeybindAction::FocusPaneRight
            )
        })
        .map(KeybindMatch::Action)
        .or_else(|| {
            crate::input::resolve_custom_command(
                &self.config.keybinds.keybinds,
                key,
                KeybindDispatch::Prefix,
            )
            .map(KeybindMatch::Command)
        })
        .or_else(|| {
            crate::input::resolve_indexed_action(
                &self.config.keybinds.keybinds,
                key,
                KeybindDispatch::Prefix,
            )
            .map(KeybindMatch::Action)
        });
        if let Some(binding) = binding {
            self.record_navigate_binding(binding, false, outcome);
        }
    }

    fn record_navigate_binding(
        &mut self,
        binding: crate::input::KeybindMatch,
        preserve_navigate: bool,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::{KeybindAction, KeybindMatch};

        let indexed_target_exists = match &binding {
            KeybindMatch::Action(KeybindAction::SwitchWorkspace(index)) => {
                self.snapshot.as_deref().is_some_and(|snapshot| {
                    self.navigation_workspace_entries(snapshot)
                        .get(*index)
                        .is_some()
                })
            }
            KeybindMatch::Action(KeybindAction::SwitchTab(index)) => self
                .snapshot
                .as_deref()
                .and_then(|snapshot| {
                    let workspace_id = snapshot.focused_workspace_id.as_deref()?;
                    snapshot
                        .tabs
                        .iter()
                        .filter(|tab| tab.workspace_id == workspace_id)
                        .nth(*index)
                })
                .is_some(),
            KeybindMatch::Action(KeybindAction::FocusAgent(index)) => {
                self.snapshot.as_deref().is_some_and(|snapshot| {
                    super::agent_sidebar::ordered_agent_pane_ids(
                        snapshot,
                        self.config.agent_panel_sort,
                    )
                    .get(*index)
                    .is_some()
                })
            }
            _ => true,
        };
        if !indexed_target_exists {
            return;
        }

        if let KeybindMatch::Action(KeybindAction::CyclePaneNext) = binding {
            self.cycle_pane(false, outcome);
        } else if let KeybindMatch::Action(KeybindAction::CyclePanePrevious) = binding {
            self.cycle_pane(true, outcome);
        } else {
            if !preserve_navigate {
                self.mode = ClientShellMode::Terminal;
            }
            self.record_binding(binding, outcome);
        }
        if !preserve_navigate {
            if self.mode == ClientShellMode::Navigate {
                self.mode = self.copy_or_terminal_mode();
            }
            self.navigate_workspace_id = None;
        }
        outcome.repaint = true;
    }

    fn move_navigate_workspace(&mut self, delta: isize) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let mobile = self.mobile_layout_active();
        let entries = self.navigation_workspace_entries(snapshot);
        if entries.is_empty() {
            return;
        }
        let current = self
            .navigate_workspace_id
            .as_deref()
            .and_then(|selected| {
                entries
                    .iter()
                    .position(|entry| snapshot.workspaces[entry.index].workspace_id == selected)
            })
            .unwrap_or(0);
        let next = if mobile {
            (current as isize + delta).clamp(0, entries.len().saturating_sub(1) as isize) as usize
        } else {
            (current as isize + delta).rem_euclid(entries.len() as isize) as usize
        };
        let workspace_id = snapshot.workspaces[entries[next].index]
            .workspace_id
            .clone();
        self.navigate_workspace_id = Some(workspace_id.clone());
        self.reveal_mobile_workspace = mobile;
        if !mobile {
            self.reveal_workspace(&workspace_id);
        }
    }

    fn cycle_pane(&mut self, reverse: bool, outcome: &mut ClientShellInput) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(surface) = self.pane_surface.as_ref() else {
            return;
        };
        if surface.panes.is_empty() {
            return;
        }
        let current = snapshot
            .focused_pane_id
            .as_deref()
            .and_then(|focused| {
                surface
                    .panes
                    .iter()
                    .position(|pane| pane.pane_id == focused)
            })
            .unwrap_or(0);
        let next = if reverse {
            (current + surface.panes.len() - 1) % surface.panes.len()
        } else {
            (current + 1) % surface.panes.len()
        };
        let pane_id = surface.panes[next].pane_id.clone();
        self.push_endpoint_method(
            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
            outcome,
        );
    }

    fn route_resize_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        let resize_bindings = &self.config.keybinds.keybinds.resize_mode;
        if key.code == KeyCode::Esc
            || key.code == KeyCode::Enter
            || resize_bindings.matches_prefix_key(key)
            || resize_bindings.matches_direct_key(key)
        {
            self.mode = self.copy_or_terminal_mode();
            outcome.repaint = true;
            return;
        }

        let action = match key.code {
            KeyCode::Char('h') | KeyCode::Left => Some(crate::input::KeybindAction::ResizePaneLeft),
            KeyCode::Char('j') | KeyCode::Down => Some(crate::input::KeybindAction::ResizePaneDown),
            KeyCode::Char('k') | KeyCode::Up => Some(crate::input::KeybindAction::ResizePaneUp),
            KeyCode::Char('l') | KeyCode::Right => {
                Some(crate::input::KeybindAction::ResizePaneRight)
            }
            _ => None,
        };
        if let Some(action) = action {
            self.record_binding(crate::input::KeybindMatch::Action(action), outcome);
        }
    }

    fn input_context(&self) -> ClientInputContext {
        ClientInputContext {
            mode: self.mode,
            overlay: self.overlay.as_ref().map(ClientShellOverlay::kind),
            popup_terminal_id: self.popup_input_target().and_then(|target| match target {
                ClientInputTarget::Popup(terminal_id) => Some(terminal_id),
                ClientInputTarget::Pane(_) => None,
            }),
            popup_pending: self.popup_pending,
            retained_selection: self
                .selection
                .as_ref()
                .is_some_and(crate::selection::Selection::is_visible),
        }
    }

    pub(super) fn focused_pane_id(&self) -> Option<String> {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_pane_id.clone())
    }

    pub(crate) fn clipboard_image_target(
        &self,
    ) -> Option<crate::protocol::ClientClipboardImageTarget> {
        if matches!(
            self.overlay,
            Some(
                ClientShellOverlay::Onboarding
                    | ClientShellOverlay::ProductAnnouncement(_)
                    | ClientShellOverlay::ReleaseNotes(_)
            )
        ) || self
            .copy_mode
            .as_ref()
            .is_some_and(|copy_mode| copy_mode.search_prompt.is_some())
        {
            return None;
        }
        if let Some(terminal_id) = self.popup_input_target().and_then(|target| match target {
            ClientInputTarget::Popup(terminal_id) => Some(terminal_id),
            ClientInputTarget::Pane(_) => None,
        }) {
            return Some(crate::protocol::ClientClipboardImageTarget::Popup(
                terminal_id,
            ));
        }
        if self.popup_pending || self.overlay.is_some() || self.mode != ClientShellMode::Terminal {
            return None;
        }
        self.focused_pane_id()
            .map(crate::protocol::ClientClipboardImageTarget::Pane)
    }

    fn popup_input_target(&self) -> Option<ClientInputTarget> {
        self.popup_terminal_id
            .as_ref()
            .map(|terminal_id| ClientInputTarget::Popup(terminal_id.clone()))
    }

    fn push_pane_key(
        &self,
        target: ClientInputTarget,
        key: crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(event) = ClientPaneInputEvent::from_terminal_key(key) {
            super::push_target_event(target, event, outcome);
        }
    }

    fn push_focused_pane_event(&self, event: ClientPaneInputEvent, outcome: &mut ClientShellInput) {
        if let Some(pane_id) = self.focused_pane_id() {
            super::push_target_event(ClientInputTarget::Pane(pane_id), event, outcome);
        }
    }
}
