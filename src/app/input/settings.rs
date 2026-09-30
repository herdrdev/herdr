use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{AppState, SettingsSection, THEME_NAMES},
        App, Mode,
    },
    config::{StatusIndicatorStyle, ToastDelivery},
};

#[derive(Debug, Clone, PartialEq, Eq)]
// The shared `Save` verb is semantic: these actions persist settings.
#[allow(clippy::enum_variant_names)]
pub(super) enum SettingsAction {
    SaveTheme(String),
    SaveStatusIndicators(StatusIndicatorStyle),
    SaveSound(bool),
    SaveToastDelivery(ToastDelivery),
    SaveAgentBorderLabels(bool),
    SaveConfigEdit(crate::app::settings_general::ConfigEdit),
    RestoreArchived(crate::app::state::PaneFocusTarget),
    DeleteArchived(crate::app::state::PaneFocusTarget),
    InstallRecommendedIntegrations,
    SaveKeybinding {
        target: crate::app::settings_keybindings::KeybindTarget,
        key: String,
    },
}

impl App {
    /// The one place a settings action is applied, whether it came from the
    /// keyboard or the mouse.
    pub(super) fn apply_settings_action(&mut self, action: SettingsAction) {
        match action {
            SettingsAction::SaveTheme(name) => self.save_theme(&name),
            SettingsAction::SaveStatusIndicators(style) => self.save_status_indicators(style),
            SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
            SettingsAction::SaveToastDelivery(delivery) => self.save_toast_delivery(delivery),
            SettingsAction::SaveAgentBorderLabels(enabled) => {
                self.save_agent_border_labels(enabled)
            }
            SettingsAction::SaveConfigEdit(edit) => self.save_config_edit(edit),
            SettingsAction::RestoreArchived(target) => {
                self.restore_archived_pane(&target);
            }
            SettingsAction::DeleteArchived(target) => {
                self.delete_archived_pane(&target);
            }
            SettingsAction::InstallRecommendedIntegrations => {
                self.install_recommended_integrations()
            }
            SettingsAction::SaveKeybinding { target, key } => {
                match self.save_keybinding(&target, &key) {
                    Ok(()) => self.state.settings.keybind_capture = None,
                    Err(error) => {
                        if let Some(capture) = self.state.settings.keybind_capture.as_mut() {
                            capture.error = Some(error);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn handle_settings_key(&mut self, key: KeyEvent) {
        let previous_section = self.state.settings.section;
        if let Some(action) = update_settings_state(&mut self.state, key) {
            self.apply_settings_action(action);
        }
        if previous_section != SettingsSection::Integrations
            && self.state.settings.section == SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
        self.start_requested_tool_probes();
    }
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn current_theme_index(theme_name: &str) -> usize {
    let normalized = normalize_theme_name(theme_name);
    THEME_NAMES
        .iter()
        .position(|name| normalize_theme_name(name) == normalized)
        .unwrap_or(0)
}

fn status_indicator_index(style: StatusIndicatorStyle) -> usize {
    match style {
        StatusIndicatorStyle::Dots => 0,
        StatusIndicatorStyle::Symbols => 1,
    }
}

fn status_indicator_for_index(idx: usize) -> StatusIndicatorStyle {
    if idx == 0 {
        StatusIndicatorStyle::Dots
    } else {
        StatusIndicatorStyle::Symbols
    }
}

fn toast_delivery_index(delivery: ToastDelivery) -> usize {
    match delivery {
        ToastDelivery::Off => 0,
        ToastDelivery::Herdr => 1,
        ToastDelivery::Terminal => 2,
        ToastDelivery::System => 3,
    }
}

fn toast_delivery_for_index(idx: usize) -> ToastDelivery {
    match idx {
        0 => ToastDelivery::Off,
        1 => ToastDelivery::Herdr,
        2 => ToastDelivery::Terminal,
        _ => ToastDelivery::System,
    }
}

fn preview_selected_theme(state: &mut AppState) {
    use crate::app::state::Palette;

    let name = THEME_NAMES[state.settings.list.selected];
    if let Some(mut palette) = Palette::from_name(name) {
        if let Some(custom) = &state.theme_runtime.custom {
            palette = palette.with_overrides(custom);
        }
        if let Some(accent) = &state.theme_runtime.legacy_accent {
            palette.accent = crate::config::parse_color(accent);
        }
        state.palette = palette;
        state.theme_name = name.to_string();
    }
}

fn cancel_settings(state: &mut AppState) {
    if let Some(palette) = state.settings.original_palette.take() {
        state.palette = palette;
    }
    if let Some(theme_name) = state.settings.original_theme.take() {
        state.theme_name = theme_name;
    }
    super::modal::leave_modal(state);
}

fn integrations_need_install(state: &AppState) -> bool {
    state
        .integration_recommendations
        .iter()
        .any(crate::integration::IntegrationRecommendation::needs_install)
}

fn apply_settings(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Theme => {
            let theme_name = state.theme_name.clone();
            state.settings.original_palette = None;
            state.settings.original_theme = None;
            super::modal::leave_modal(state);
            Some(SettingsAction::SaveTheme(theme_name))
        }
        SettingsSection::Integrations if integrations_need_install(state) => {
            Some(SettingsAction::InstallRecommendedIntegrations)
        }
        SettingsSection::Integrations => None,
        _ => {
            super::modal::leave_modal(state);
            None
        }
    }
}

/// How many selectable rows the section's content column has. Sections that
/// only display facts have none, so `up`/`down` are inert there rather than
/// moving an invisible cursor.
pub(crate) fn settings_section_item_count(state: &AppState, section: SettingsSection) -> usize {
    match section {
        SettingsSection::General => crate::app::settings_general::GeneralRow::ALL.len(),
        SettingsSection::Sidebar => 2,
        SettingsSection::Theme => THEME_NAMES.len(),
        SettingsSection::Indicators | SettingsSection::Sound | SettingsSection::PaneLabels => 2,
        SettingsSection::SidebarPanels => {
            crate::app::state::settings_sidebar_panel_items(state).len()
        }
        SettingsSection::Toast => 4,
        SettingsSection::Archive => state.archive_entries().len(),
        SettingsSection::Keybindings => {
            crate::app::settings_keybindings::settings_keybinding_rows(state).len()
        }
        SettingsSection::Providers
        | SettingsSection::Integrations
        | SettingsSection::SourceControl
        | SettingsSection::About => 0,
    }
}

/// Where the cursor lands when a section is opened: on the current value, so
/// the screen tells the operator what is set without them having to read it.
fn default_selected_index(state: &AppState, section: SettingsSection) -> usize {
    match section {
        SettingsSection::Sidebar => usize::from(!state.sidebar_show_ask_subtitles),
        SettingsSection::Theme => current_theme_index(&state.theme_name),
        SettingsSection::Indicators => status_indicator_index(state.status_indicators),
        SettingsSection::Sound => usize::from(!state.sound_enabled()),
        SettingsSection::Toast => toast_delivery_index(state.toast_delivery()),
        SettingsSection::PaneLabels => usize::from(!state.agent_border_labels_enabled()),
        _ => 0,
    }
}

/// The one way a settings section is entered, from the keyboard, the mouse, or
/// `open_settings_at`. Sections backed by CLI probes request them here so no
/// entry path can reach them unprobed.
fn select_section(state: &mut AppState, section: SettingsSection) {
    state.settings.section = section;
    state.settings.keybind_capture = None;
    state.settings.list.selected = default_selected_index(state, section);
    state.settings.archive_delete_armed = false;
    if matches!(
        section,
        SettingsSection::Providers | SettingsSection::Integrations
    ) {
        state.request_tool_probes = true;
    }
}

/// The next/previous section in the filtered nav column. With no filter this
/// is exactly `SettingsSection::next`/`prev`.
fn adjacent_section(state: &AppState, delta: isize) -> SettingsSection {
    let sections = crate::app::state::settings_sections_matching(&state.settings.search);
    if sections.is_empty() {
        return state.settings.section;
    }
    let current = sections
        .iter()
        .position(|section| *section == state.settings.section)
        .unwrap_or(0);
    let len = sections.len();
    let next = if delta < 0 {
        (current + len - 1) % len
    } else {
        (current + 1) % len
    };
    sections[next]
}

/// Typing in the filter line. Returns true when the key was consumed.
fn update_settings_search(state: &mut AppState, key: KeyEvent) -> bool {
    if !state.settings.search_active {
        return false;
    }
    match key.code {
        KeyCode::Char(character)
            if key.modifiers.is_empty()
                || key.modifiers == crossterm::event::KeyModifiers::SHIFT =>
        {
            state.settings.search.push(character);
        }
        KeyCode::Backspace => {
            state.settings.search.pop();
        }
        KeyCode::Enter | KeyCode::Down | KeyCode::Up | KeyCode::Tab | KeyCode::BackTab => {
            state.settings.search_active = false;
            return key.code == KeyCode::Enter;
        }
        KeyCode::Esc => {
            state.settings.search.clear();
            state.settings.search_active = false;
            return true;
        }
        _ => return false,
    }
    // The filtered list may no longer contain the active section; land on the
    // first match so the body always matches the visible nav.
    let sections = crate::app::state::settings_sections_matching(&state.settings.search);
    if !sections.is_empty() && !sections.contains(&state.settings.section) {
        select_section(state, sections[0]);
    }
    true
}

fn move_selection(state: &mut AppState, delta: isize) {
    let count = settings_section_item_count(state, state.settings.section);
    if count == 0 {
        return;
    }
    let previous = state.settings.list.selected;
    if delta < 0 {
        state.settings.list.move_prev();
    } else {
        state.settings.list.move_next(count);
    }
    if state.settings.list.selected != previous {
        state.settings.archive_delete_armed = false;
        state.settings.keybind_capture = None;
        if state.settings.section == SettingsSection::Theme {
            preview_selected_theme(state);
        }
    }
}

/// `enter` / `space` on the selected row.
fn activate_selection(state: &mut AppState) -> Option<SettingsAction> {
    let idx = state.settings.list.selected;
    match state.settings.section {
        SettingsSection::General => {
            let row = *crate::app::settings_general::GeneralRow::ALL.get(idx)?;
            crate::app::settings_general::cycle_general_row(state, row)
                .map(SettingsAction::SaveConfigEdit)
        }
        SettingsSection::Sidebar => Some(SettingsAction::SaveConfigEdit(
            crate::app::settings_general::ConfigEdit::Bool {
                section: "ui.sidebar",
                key: "show_ask_subtitles",
                value: idx == 0,
            },
        )),
        SettingsSection::Indicators => Some(SettingsAction::SaveStatusIndicators(
            status_indicator_for_index(idx),
        )),
        SettingsSection::Sound => Some(SettingsAction::SaveSound(idx == 0)),
        SettingsSection::Toast => Some(SettingsAction::SaveToastDelivery(
            toast_delivery_for_index(idx),
        )),
        SettingsSection::PaneLabels => Some(SettingsAction::SaveAgentBorderLabels(idx == 0)),
        SettingsSection::SidebarPanels => {
            let item = crate::app::state::settings_sidebar_panel_items(state)
                .into_iter()
                .nth(idx)?;
            match item.target {
                crate::app::state::SidebarPanelSettingTarget::Note(name) => {
                    let visible = !item.visible;
                    let mut notepad = state.notepad.clone();
                    notepad.set_note_tab_visible(&name, visible);
                    Some(SettingsAction::SaveConfigEdit(
                        crate::app::settings_general::ConfigEdit::NotepadTabVisibility {
                            visible_tabs: notepad.visible_tabs,
                            enable_notepad: visible,
                            ensure_note: visible.then_some(name),
                        },
                    ))
                }
                crate::app::state::SidebarPanelSettingTarget::NotepadTab(target) => {
                    let mut notepad = state.notepad.clone();
                    notepad.set_tab_visible(target, !item.visible);
                    Some(SettingsAction::SaveConfigEdit(
                        crate::app::settings_general::ConfigEdit::StringList {
                            section: "notepad",
                            key: "visible_tabs",
                            value: notepad.visible_tabs,
                        },
                    ))
                }
                crate::app::state::SidebarPanelSettingTarget::Goals => {
                    Some(SettingsAction::SaveConfigEdit(
                        crate::app::settings_general::ConfigEdit::Bool {
                            section: "goals_panel",
                            key: "enabled",
                            value: !item.visible,
                        },
                    ))
                }
                crate::app::state::SidebarPanelSettingTarget::Pomodoro => {
                    Some(SettingsAction::SaveConfigEdit(
                        crate::app::settings_general::ConfigEdit::PomodoroSidebarVisible {
                            visible: !item.visible,
                        },
                    ))
                }
                crate::app::state::SidebarPanelSettingTarget::Animation => {
                    Some(SettingsAction::SaveConfigEdit(
                        crate::app::settings_general::ConfigEdit::Bool {
                            section: "ui",
                            key: "sidebar_animation",
                            value: !item.visible,
                        },
                    ))
                }
            }
        }
        SettingsSection::Archive => {
            let entry = state.archive_entries().into_iter().nth(idx)?;
            state.settings.archive_delete_armed = false;
            Some(SettingsAction::RestoreArchived(entry.target))
        }
        SettingsSection::Integrations if integrations_need_install(state) => {
            Some(SettingsAction::InstallRecommendedIntegrations)
        }
        SettingsSection::Keybindings => {
            crate::app::settings_keybindings::keybinding_target(state, idx)?;
            state.settings.keybind_capture = Some(crate::app::state::KeybindCapture {
                row: idx,
                error: None,
            });
            None
        }
        _ => None,
    }
}

/// The chord the operator pressed while a keybindings row is capturing.
///
/// The chord is validated with the same function the 6b add-action modal uses,
/// so a chord that is already bound is refused here for exactly the reason it
/// would be refused there, and the refusal is shown on the row.
fn capture_keybinding(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    let row = state.settings.keybind_capture.as_ref()?.row;
    if key.code == KeyCode::Esc {
        state.settings.keybind_capture = None;
        return None;
    }
    if matches!(key.code, KeyCode::Modifier(_)) {
        return None;
    }
    let target = crate::app::settings_keybindings::keybinding_target(state, row)?;
    let label = crate::config::format_key_combo((key.code, key.modifiers));
    let mut config = crate::config::Config::load().config;
    match crate::config::validate_user_action_key(&mut config, &label) {
        Ok(label) => Some(SettingsAction::SaveKeybinding { target, key: label }),
        Err(diagnostic) => {
            // The validator phrases its refusal in terms of the temporary
            // action it probes with, which means nothing on this row.
            tracing::debug!(message = %diagnostic, "keybinding capture refused");
            if let Some(capture) = state.settings.keybind_capture.as_mut() {
                capture.error = Some(format!("{label} is already bound; press another chord"));
            }
            None
        }
    }
}

/// `d` on an archive row. Confirms once first while `ui.confirm_close` is on,
/// the same rule the sidebar's settled menu follows.
fn delete_archive_selection(state: &mut AppState) -> Option<SettingsAction> {
    if state.settings.section != SettingsSection::Archive {
        return None;
    }
    let entry = state
        .archive_entries()
        .into_iter()
        .nth(state.settings.list.selected)?;
    if state.confirm_close && !std::mem::take(&mut state.settings.archive_delete_armed) {
        state.settings.archive_delete_armed = true;
        return None;
    }
    state.settings.archive_delete_armed = false;
    Some(SettingsAction::DeleteArchived(entry.target))
}

pub(super) fn update_settings_state(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    if update_settings_search(state, key) {
        return None;
    }
    if state.settings.keybind_capture.is_some() {
        return capture_keybinding(state, key);
    }
    match key.code {
        KeyCode::Char('/') => {
            state.settings.search_active = true;
            return None;
        }
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
            select_section(state, adjacent_section(state, 1));
            return None;
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
            select_section(state, adjacent_section(state, -1));
            return None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            move_selection(state, -1);
            return None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            move_selection(state, 1);
            return None;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(action) = activate_selection(state) {
                return Some(action);
            }
            if state.settings.section == SettingsSection::Theme {
                return apply_settings(state);
            }
            return None;
        }
        KeyCode::Char('d') | KeyCode::Delete => {
            if let Some(action) = delete_archive_selection(state) {
                return Some(action);
            }
            if key.code == KeyCode::Char('d') {
                return None;
            }
        }
        _ => {}
    }

    match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
        Some(super::modal::ModalAction::Apply) => apply_settings(state),
        Some(super::modal::ModalAction::Close) => {
            if !state.settings.search.is_empty() {
                state.settings.search.clear();
                return None;
            }
            cancel_settings(state);
            None
        }
        _ => None,
    }
}

pub(crate) fn open_settings(state: &mut AppState) {
    open_settings_at(state, SettingsSection::General);
}

pub(crate) fn open_settings_at(state: &mut AppState, section: SettingsSection) {
    state.integration_install_messages.clear();
    state.settings.original_palette = Some(state.palette.clone());
    state.settings.original_theme = Some(state.theme_name.clone());
    state.settings.search.clear();
    state.settings.search_active = false;
    select_section(state, section);
    state.set_server_mode(Mode::Settings);
}

impl AppState {
    fn settings_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(
            self.screen_rect(),
            crate::ui::SETTINGS_POPUP_WIDTH,
            crate::ui::settings_popup_height(self),
        )
        .unwrap_or_default()
    }

    fn settings_inner_rect(&self) -> Rect {
        let popup = self.settings_popup_rect();
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    }

    /// The section under a click in the left nav column.
    fn settings_tab_at(&self, col: u16, row: u16) -> Option<SettingsSection> {
        let nav = crate::ui::settings_areas(self.settings_inner_rect()).nav;
        if col < nav.x || col >= nav.x + nav.width || row < nav.y || row >= nav.y + nav.height {
            return None;
        }
        let index = crate::ui::settings_nav_scroll(self, nav) + (row - nav.y) as usize;
        crate::app::state::settings_sections_matching(&self.settings.search)
            .get(index)
            .copied()
    }

    /// A click on the filter line focuses it rather than closing the screen.
    fn settings_search_hit(&self, col: u16, row: u16) -> bool {
        let search = crate::ui::settings_areas(self.settings_inner_rect()).search;
        col >= search.x
            && col < search.x + search.width
            && row >= search.y
            && row < search.y + search.height
    }

    pub(crate) fn settings_content_rect(&self) -> Rect {
        crate::ui::settings_areas(self.settings_inner_rect()).content
    }

    fn settings_list_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.settings_content_rect();
        if row < area.y || row >= area.y + area.height || col < area.x || col >= area.x + area.width
        {
            return None;
        }

        match self.settings.section {
            SettingsSection::General => {
                let list_y = area.y + 2;
                let offset = row.checked_sub(list_y)?;
                let list_height = area.height.saturating_sub(2);
                let scroll =
                    crate::ui::general_row_scroll(self.settings.list.selected, list_height);
                crate::ui::general_row_offsets()
                    .into_iter()
                    .position(|(start, height)| {
                        let offset = offset.saturating_add(scroll);
                        offset >= start && offset < start + height
                    })
            }
            SettingsSection::Theme => {
                let max_visible = area.height as usize;
                let scroll = if self.settings.list.selected >= max_visible {
                    self.settings.list.selected - max_visible + 1
                } else {
                    0
                };
                let idx = scroll + (row - area.y) as usize;
                (idx < THEME_NAMES.len()).then_some(idx)
            }
            SettingsSection::Indicators
            | SettingsSection::Sidebar
            | SettingsSection::Sound
            | SettingsSection::PaneLabels => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::SidebarPanels => {
                let list_y = area.y + 3;
                let visible = area.height.saturating_sub(3) as usize;
                let offset = row.checked_sub(list_y)? as usize;
                let count = settings_section_item_count(self, self.settings.section);
                let scroll = self
                    .settings
                    .list
                    .selected
                    .saturating_sub(visible.saturating_sub(1));
                let index = scroll + offset;
                (index < count).then_some(index)
            }
            SettingsSection::Toast => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 8 {
                    Some(((row - list_y) / 2) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Archive | SettingsSection::Keybindings => {
                let list_y = area.y + 2;
                let visible = area.height.saturating_sub(2) as usize;
                let offset = row.checked_sub(list_y)? as usize;
                let count = settings_section_item_count(self, self.settings.section);
                let scroll = self
                    .settings
                    .list
                    .selected
                    .saturating_sub(visible.saturating_sub(1));
                let idx = scroll + offset;
                (idx < count).then_some(idx)
            }
            SettingsSection::Providers
            | SettingsSection::Integrations
            | SettingsSection::SourceControl
            | SettingsSection::About => None,
        }
    }

    pub(super) fn handle_settings_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.settings_search_hit(mouse.column, mouse.row) {
                    self.settings.search_active = true;
                    return None;
                }
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    select_section(self, section);
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.column, mouse.row) {
                    self.settings.list.select(idx);
                    if self.settings.section == SettingsSection::Theme {
                        preview_selected_theme(self);
                        return None;
                    }
                    return activate_selection(self);
                }

                let inner = self.settings_inner_rect();
                let show_primary = crate::ui::settings_show_primary_action(self);
                let (apply, close) =
                    crate::ui::settings_button_rects(inner, self.settings.section, show_primary);
                let mut buttons = vec![(close, super::modal::ModalAction::Close)];
                if let Some(apply) = apply {
                    buttons.insert(0, (apply, super::modal::ModalAction::Apply));
                }
                match super::modal::modal_action_from_buttons(mouse.column, mouse.row, &buttons) {
                    Some(super::modal::ModalAction::Apply) => apply_settings(self),
                    Some(super::modal::ModalAction::Close) => {
                        cancel_settings(self);
                        None
                    }
                    _ => {
                        cancel_settings(self);
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, mouse, state_with_workspaces};
    use super::*;

    fn press(state: &mut AppState, code: KeyCode) -> Option<SettingsAction> {
        update_settings_state(state, KeyEvent::new(code, KeyModifiers::empty()))
    }

    fn type_search(state: &mut AppState, text: &str) {
        for character in text.chars() {
            press(state, KeyCode::Char(character));
        }
    }

    #[test]
    fn the_section_filter_narrows_the_nav_and_esc_clears_it_before_closing() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        assert_eq!(state.settings.section, SettingsSection::General);

        press(&mut state, KeyCode::Char('/'));
        assert!(state.settings.search_active);
        type_search(&mut state, "arch");
        assert_eq!(state.settings.search, "arch");
        assert_eq!(
            crate::app::state::settings_sections_matching(&state.settings.search),
            vec![SettingsSection::Archive]
        );
        // The body follows the filter instead of showing a hidden section.
        assert_eq!(state.settings.section, SettingsSection::Archive);

        // Esc leaves the filter line but keeps the query and the screen.
        press(&mut state, KeyCode::Esc);
        assert!(!state.settings.search_active);
        assert!(state.settings.search.is_empty());
        assert_eq!(state.server_mode(), Mode::Settings);

        // With no query left, Esc closes.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn an_esc_with_a_query_but_an_unfocused_filter_clears_before_closing() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.search = "keys".into();

        press(&mut state, KeyCode::Esc);
        assert!(state.settings.search.is_empty());
        assert_eq!(state.server_mode(), Mode::Settings);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn section_cycling_stays_inside_the_filtered_list() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.search = "so".into();
        let visible = crate::app::state::settings_sections_matching(&state.settings.search);
        assert_eq!(
            visible,
            vec![SettingsSection::Sound, SettingsSection::SourceControl]
        );

        select_section(&mut state, SettingsSection::Sound);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.settings.section, SettingsSection::SourceControl);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.settings.section, SettingsSection::Sound);
        press(&mut state, KeyCode::BackTab);
        assert_eq!(state.settings.section, SettingsSection::SourceControl);
    }

    #[test]
    fn a_query_that_matches_nothing_leaves_the_section_alone() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        press(&mut state, KeyCode::Char('/'));
        type_search(&mut state, "zzz");
        assert!(crate::app::state::settings_sections_matching(&state.settings.search).is_empty());
        assert_eq!(state.settings.section, SettingsSection::General);
    }

    #[test]
    fn desktop_clamped_settings_hit_geometry_includes_status_row() {
        let mut state = state_with_workspaces(&["test"]);
        state.view.status_bar_rect = Rect::new(0, 0, 106, 1);
        state.view.sidebar_rect = Rect::new(0, 1, 26, 19);
        state.view.terminal_area = Rect::new(26, 2, 80, 18);
        open_settings(&mut state);

        assert_eq!(state.screen_rect(), Rect::new(0, 0, 106, 20));
        assert_eq!(state.settings_popup_rect(), Rect::new(5, 1, 96, 18));
        let inner = state.settings_inner_rect();
        let (_, close) = crate::ui::settings_button_rects(
            inner,
            state.settings.section,
            crate::ui::settings_show_primary_action(&state),
        );
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close.x,
            row: close.y,
            modifiers: KeyModifiers::empty(),
        };

        assert_eq!(state.handle_settings_mouse(click), None);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn settings_cancel_restores_previewed_theme_from_other_sections() {
        let mut state = state_with_workspaces(&["test"]);
        let original_palette = state.palette.clone();
        let original_theme = state.theme_name.clone();

        open_settings_at(&mut state, SettingsSection::Theme);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_ne!(state.theme_name, original_theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(
            state.settings.section,
            crate::app::state::SettingsSection::Indicators
        );

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);
        assert_eq!(state.theme_name, original_theme);
        assert_eq!(state.palette.accent, original_palette.accent);
        assert_eq!(state.palette.panel_bg, original_palette.panel_bg);
    }

    #[test]
    fn settings_indicator_choice_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Indicators);
        state.settings.list.selected = 1;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveStatusIndicators(
                StatusIndicatorStyle::Symbols
            ))
        );
        assert_eq!(state.status_indicators, StatusIndicatorStyle::Dots);
        assert_eq!(state.server_mode(), Mode::Settings);
    }

    #[test]
    fn settings_sound_toggle_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.section = crate::app::state::SettingsSection::Sound;
        state.settings.list.selected = 0;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SaveSound(true)));
        assert!(!state.sound.enabled);
        assert_eq!(state.server_mode(), Mode::Settings);
    }

    #[test]
    fn settings_tab_cycle_wraps_around_the_section_list() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::About);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::General);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::About);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Archive);
    }

    #[test]
    fn integrations_enter_does_nothing_when_nothing_needs_install() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Integrations);

        let enter_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(enter_action, None);

        let space_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );
        assert_eq!(space_action, None);
    }

    #[test]
    fn settings_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        app.state.settings.list.select(0);

        let area = app.state.settings_content_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, area.x + 2, area.y + 2));

        assert_eq!(app.state.settings.list.selected, 0);
    }

    #[test]
    fn integration_update_badge_only_tracks_outdated_recommendations() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            false,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Current,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        assert!(state.integration_updates_available());
    }

    /// The nav-row coordinate for `section`, or `None` when it is scrolled out.
    fn nav_row_for(state: &AppState, section: SettingsSection) -> Option<(u16, u16)> {
        let nav = crate::ui::settings_areas(state.settings_inner_rect()).nav;
        let scroll = crate::ui::settings_nav_scroll(state, nav);
        let index = SettingsSection::ALL.iter().position(|s| *s == section)?;
        let offset = index.checked_sub(scroll)?;
        if offset >= nav.height as usize {
            return None;
        }
        Some((nav.x, nav.y + offset as u16))
    }

    #[test]
    fn clicking_the_providers_nav_row_starts_the_tool_probes() {
        let mut app = app_for_mouse_test();
        // Keep Providers visible in the scrolled nav without entering a probed section.
        open_settings_at(&mut app.state, SettingsSection::SourceControl);
        // Opening on an unprobed section leaves nothing requested.
        assert!(!app.state.request_tool_probes);

        let (col, row) =
            nav_row_for(&app.state, SettingsSection::Providers).expect("providers nav row");
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert_eq!(app.state.settings.section, SettingsSection::Providers);
        assert!(
            app.state.tool_probes_pending(),
            "the probes are pending until the worker answers"
        );
        assert!(
            matches!(
                app.state.tool_probes,
                crate::app::probes::ToolProbeState::Running
            ),
            "the mouse path starts the probes: {:?}",
            app.state.tool_probes
        );
        assert!(
            !app.state.request_tool_probes,
            "the request is drained once it is served"
        );
    }

    #[test]
    fn entering_a_probed_section_requests_the_probes_from_every_path() {
        let mut state = app_for_mouse_test().state;

        open_settings_at(&mut state, SettingsSection::Providers);
        assert!(state.request_tool_probes, "open_settings_at requests");

        state.request_tool_probes = false;
        open_settings_at(&mut state, SettingsSection::General);
        assert!(!state.request_tool_probes, "an unprobed section does not");

        // Keyboard: tab through to Integrations.
        while state.settings.section != SettingsSection::Integrations {
            update_settings_state(
                &mut state,
                KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            );
        }
        assert!(state.request_tool_probes, "the keyboard path requests");

        state.request_tool_probes = false;
        open_settings_at(&mut state, SettingsSection::SourceControl);
        let (col, row) =
            nav_row_for(&state, SettingsSection::Providers).expect("providers nav row");
        state.handle_settings_mouse(mouse_down(col, row));
        assert_eq!(state.settings.section, SettingsSection::Providers);
        assert!(state.request_tool_probes, "the mouse path requests");
    }

    #[test]
    fn every_nav_row_maps_back_to_its_section() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        open_settings(&mut state);

        let nav = crate::ui::settings_areas(state.settings_inner_rect()).nav;
        let scroll = crate::ui::settings_nav_scroll(&state, nav);
        for (offset, section) in SettingsSection::ALL
            .iter()
            .enumerate()
            .skip(scroll)
            .take(nav.height as usize)
        {
            let row = nav.y + (offset - scroll) as u16;
            assert_eq!(state.settings_tab_at(nav.x, row), Some(*section));
            assert_eq!(
                state.settings_tab_at(nav.x + nav.width - 1, row),
                Some(*section)
            );
        }
        assert_eq!(state.settings_tab_at(nav.x + nav.width, nav.y), None);
    }

    #[test]
    fn general_enter_returns_the_config_edit_for_the_selected_row() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::General);
        assert_eq!(state.settings.section, SettingsSection::General);

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(
            action,
            Some(SettingsAction::SaveConfigEdit(
                crate::app::settings_general::ConfigEdit::Bool {
                    section: "ui",
                    key: "combine_repos_across_hosts",
                    value: true,
                }
            ))
        );

        // The read-only path row writes nothing.
        state.settings.list.select(
            crate::app::settings_general::GeneralRow::ALL
                .iter()
                .position(|row| {
                    *row == crate::app::settings_general::GeneralRow::AddProjectStartDir
                })
                .expect("row"),
        );
        assert_eq!(
            update_settings_state(
                &mut state,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            ),
            None
        );
    }

    #[test]
    fn clicking_a_general_row_selects_it_and_returns_its_edit() {
        let mut app = app_for_mouse_test();
        // Tall enough for every General row: the mapping this test checks only
        // holds for rows the screen actually shows.
        app.state.view.terminal_area = ratatui::layout::Rect::new(26, 0, 80, 44);
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 26, 44);
        open_settings_at(&mut app.state, SettingsSection::General);

        let area = app.state.settings_content_rect();
        // Every row that fits maps back to itself, including the hinted first
        // row whose second line belongs to the same row.
        for (index, (offset, height)) in crate::ui::general_row_offsets().into_iter().enumerate() {
            for line in 0..height {
                let row = area.y + 2 + offset + line;
                if row >= area.y + area.height {
                    continue;
                }
                assert_eq!(
                    app.state.settings_list_index_at(area.x + 1, row),
                    Some(index),
                    "row {index} line {line}"
                );
            }
        }

        let hide_whitespace = crate::app::settings_general::GeneralRow::ALL
            .iter()
            .position(|row| *row == crate::app::settings_general::GeneralRow::HideWhitespace)
            .expect("row");
        let (offset, _) = crate::ui::general_row_offsets()[hide_whitespace];
        app.state.settings.list.selected = hide_whitespace + 1;
        let scroll = crate::ui::general_row_scroll(
            app.state.settings.list.selected,
            area.height.saturating_sub(2),
        );
        let action = app.state.handle_settings_mouse(mouse_down(
            area.x + 1,
            area.y + 2 + offset.saturating_sub(scroll),
        ));

        assert_eq!(app.state.settings.list.selected, hide_whitespace);
        assert_eq!(
            action,
            Some(SettingsAction::SaveConfigEdit(
                crate::app::settings_general::ConfigEdit::Bool {
                    section: "ui",
                    key: "hide_whitespace_in_diff",
                    value: true,
                }
            ))
        );

        let auto_nudge = crate::app::settings_general::GeneralRow::ALL
            .iter()
            .position(|row| {
                *row == crate::app::settings_general::GeneralRow::AutoNudgeStalledAgents
            })
            .expect("row");
        let (offset, _) = crate::ui::general_row_offsets()[auto_nudge];
        app.state.settings.list.selected = auto_nudge - 1;
        let scroll = crate::ui::general_row_scroll(
            app.state.settings.list.selected,
            area.height.saturating_sub(2),
        );
        let action = app.state.handle_settings_mouse(mouse_down(
            area.x + 1,
            area.y + 2 + offset.saturating_sub(scroll),
        ));

        assert_eq!(app.state.settings.list.selected, auto_nudge);
        assert_eq!(
            action,
            Some(SettingsAction::SaveConfigEdit(
                crate::app::settings_general::ConfigEdit::Bool {
                    section: "session",
                    key: "auto_nudge_stalled_agents",
                    value: true,
                }
            ))
        );
    }

    #[test]
    fn archive_delete_asks_once_while_confirm_close_is_on() {
        let mut state = state_with_workspaces(&["test"]);
        let ws_idx = state.active.expect("active workspace");
        let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
        assert!(state.settle_pane_at(ws_idx, pane_id, 1_700_000_000));
        open_settings_at(&mut state, SettingsSection::Archive);
        assert_eq!(state.archive_entries().len(), 1);

        assert!(state.confirm_close);
        let armed = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
        );
        assert_eq!(armed, None);
        assert!(state.settings.archive_delete_armed);

        let deleted = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
        );
        assert!(matches!(deleted, Some(SettingsAction::DeleteArchived(_))));
        assert!(!state.settings.archive_delete_armed);
    }

    #[test]
    fn archive_delete_is_immediate_when_confirmation_is_off() {
        let mut state = state_with_workspaces(&["test"]);
        state.confirm_close = false;
        let ws_idx = state.active.expect("active workspace");
        let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
        assert!(state.settle_pane_at(ws_idx, pane_id, 1_700_000_000));
        open_settings_at(&mut state, SettingsSection::Archive);

        let deleted = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
        );
        assert!(matches!(deleted, Some(SettingsAction::DeleteArchived(_))));
    }

    #[test]
    fn archive_enter_restores_the_selected_thread() {
        let mut state = state_with_workspaces(&["test"]);
        let ws_idx = state.active.expect("active workspace");
        let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
        assert!(state.settle_pane_at(ws_idx, pane_id, 1_700_000_000));
        open_settings_at(&mut state, SettingsSection::Archive);

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        match action {
            Some(SettingsAction::RestoreArchived(target)) => {
                assert_eq!(target.pane_id, pane_id);
            }
            other => panic!("expected a restore action, got {other:?}"),
        }
    }

    #[test]
    fn keybindings_lists_actions_with_their_keys() {
        let state = state_with_workspaces(&["test"]);
        let rows = crate::app::settings_keybindings::settings_keybinding_rows(&state);

        assert!(rows.iter().any(|row| row.heading && row.label == "global"));
        assert!(rows
            .iter()
            .any(|row| !row.heading && row.label == "settings" && !row.key.is_empty()));
    }

    #[test]
    fn sidebar_panel_defaults_show_only_the_usage_tab() {
        let mut state = AppState::test_new();
        let config = crate::config::Config::default();
        state.notepad = crate::notepad::NotepadState::from_config(&config.notepad);
        state.goals = crate::goals::GoalsPanelState::from_config(&config.goals_panel);
        state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &config.pomodoro,
            std::time::Instant::now(),
        );
        let items = crate::app::state::settings_sidebar_panel_items(&state);
        let visible = items
            .iter()
            .filter(|item| item.visible)
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(state.notepad.visible_tabs, vec!["usage".to_string()]);
        assert_eq!(visible, vec!["Usage tab"]);
        assert!(items
            .iter()
            .any(|item| { item.label == "Note · notes" && !item.visible }));
        assert!(items
            .iter()
            .any(|item| { item.label == "Note · pomodoro-log" && !item.visible }));
        assert!(!state.goals.enabled);
        assert!(!state.pomodoro.sidebar_visible);
    }

    #[test]
    fn sidebar_panel_note_rows_include_configured_and_log_notes_before_discovery() {
        let mut state = AppState::test_new();
        state.sidebar_note_names = crate::app::state::sidebar_panel_note_names(
            &["journal.md".to_string(), "weekly".to_string()],
            "break-notes.md",
        );

        let rows = crate::app::state::settings_sidebar_panel_items(&state);

        assert!(state.notepad.files.is_empty());
        for name in ["notes", "journal", "weekly", "break-notes"] {
            assert!(rows.iter().any(|row| row.label == format!("Note · {name}")));
        }
    }

    #[test]
    fn sidebar_panel_tabs_can_be_hidden_and_shown_again() {
        let mut state = AppState::test_new();
        state.notepad =
            crate::notepad::NotepadState::from_config(&crate::config::NotepadConfig::default());
        let usage = crate::notepad::NotepadTabTarget::Usage;
        assert_eq!(state.notepad.active_tab_target(), Some(usage));

        assert!(state.notepad.set_tab_visible(usage, false));
        assert!(!state.notepad.is_tab_visible(usage));
        assert_eq!(state.notepad.active_tab_target(), None);
        assert!(
            !state.notepad.select_context(),
            "a hidden static tab cannot be selected"
        );

        assert!(state.notepad.set_tab_visible(usage, true));
        assert!(state.notepad.is_tab_visible(usage));
        assert_eq!(state.notepad.active_tab_target(), Some(usage));
    }

    #[test]
    fn hiding_the_active_note_falls_back_to_the_first_visible_tab() {
        let mut state = AppState::test_new();
        state.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/pomodoro-log.md".into(),
                name: "pomodoro-log".into(),
            },
        ]);
        state.notepad.set_visible_tabs(vec![
            "note:todo".to_string(),
            "note:pomodoro-log".to_string(),
            "usage".to_string(),
        ]);
        assert_eq!(
            state.notepad.active_tab_target(),
            Some(crate::notepad::NotepadTabTarget::Note(0))
        );

        state
            .notepad
            .set_visible_tabs(vec!["note:pomodoro-log".to_string(), "usage".to_string()]);

        assert_eq!(
            state.notepad.active_tab_target(),
            Some(crate::notepad::NotepadTabTarget::Note(1))
        );
        assert!(!state.notepad.select(0), "a hidden note cannot be selected");
    }

    #[test]
    fn sidebar_panel_settings_toggle_from_keyboard_and_mouse() {
        let mut keyboard = AppState::test_new();
        open_settings_at(&mut keyboard, SettingsSection::SidebarPanels);
        let usage_index = crate::app::state::settings_sidebar_panel_items(&keyboard)
            .iter()
            .position(|item| {
                item.target
                    == crate::app::state::SidebarPanelSettingTarget::NotepadTab(
                        crate::notepad::NotepadTabTarget::Usage,
                    )
            })
            .expect("usage row");
        keyboard.settings.list.selected = usage_index;
        assert_eq!(
            update_settings_state(
                &mut keyboard,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            ),
            Some(SettingsAction::SaveConfigEdit(
                crate::app::settings_general::ConfigEdit::StringList {
                    section: "notepad",
                    key: "visible_tabs",
                    value: vec!["usage".to_string()],
                }
            ))
        );

        let mut mouse = AppState::test_new();
        mouse.view.sidebar_rect = Rect::new(0, 0, 26, 40);
        mouse.view.terminal_area = Rect::new(26, 0, 80, 40);
        open_settings_at(&mut mouse, SettingsSection::SidebarPanels);
        let area = mouse.settings_content_rect();
        let pomodoro_index = crate::app::state::settings_sidebar_panel_items(&mouse)
            .iter()
            .position(|item| item.target == crate::app::state::SidebarPanelSettingTarget::Pomodoro)
            .expect("pomodoro row");
        assert_eq!(
            mouse.settings_list_index_at(area.x, area.y + 3 + pomodoro_index as u16),
            Some(pomodoro_index)
        );
        assert_eq!(
            mouse.handle_settings_mouse(mouse_down(area.x, area.y + 3 + pomodoro_index as u16,)),
            Some(SettingsAction::SaveConfigEdit(
                crate::app::settings_general::ConfigEdit::PomodoroSidebarVisible { visible: true }
            ))
        );
    }

    #[test]
    fn sidebar_panels_animation_row_hides_and_shows_the_animation() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-sidebar-animation-setting-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("temp dir");
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "").expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &config_path);
        let config = crate::config::Config::load().config;
        assert!(config.ui.sidebar_animation, "default stays on");
        let mut app = crate::app::App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        assert!(app.state.hyperspace.enabled);
        app.state.view.hyperspace_rect = Rect::new(0, 20, 12, 4);
        app.state.view.hyperspace_pause_hit_area = Rect::new(0, 23, 12, 1);

        open_settings_at(&mut app.state, SettingsSection::SidebarPanels);
        let index = crate::app::state::settings_sidebar_panel_items(&app.state)
            .iter()
            .position(|item| item.target == crate::app::state::SidebarPanelSettingTarget::Animation)
            .expect("animation row");
        app.state.settings.list.selected = index;
        let action = update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        )
        .expect("animation hide action");
        app.apply_settings_action(action);

        assert!(!app.state.hyperspace.enabled);
        assert_eq!(app.state.view.hyperspace_rect, Rect::default());
        assert_eq!(app.state.view.hyperspace_pause_hit_area, Rect::default());
        let written = std::fs::read_to_string(&config_path).expect("config written");
        let parsed: toml::Value = toml::from_str(&written).expect("valid toml");
        assert_eq!(
            parsed.get("ui").and_then(|ui| ui.get("sidebar_animation")),
            Some(&toml::Value::Boolean(false))
        );

        app.state.settings.list.selected = index;
        let action = update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        )
        .expect("animation show action");
        app.apply_settings_action(action);
        assert!(app.state.hyperspace.enabled);
        let written = std::fs::read_to_string(&config_path).expect("config written");
        assert!(written.contains("sidebar_animation = true"));

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn enabling_a_note_from_sidebar_panels_enables_and_creates_it() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-sidebar-note-setting-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("scratch directory");
        let config_path = directory.join("config.toml");
        let notes_dir = directory.join("notes");
        std::fs::write(
            &config_path,
            format!("[notepad]\ndir = {:?}\n", notes_dir.display().to_string()),
        )
        .expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &config_path);
        let config = crate::config::Config::load().config;
        let mut app = crate::app::App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        open_settings_at(&mut app.state, SettingsSection::SidebarPanels);
        let note_index = crate::app::state::settings_sidebar_panel_items(&app.state)
            .iter()
            .position(|item| {
                item.target
                    == crate::app::state::SidebarPanelSettingTarget::Note("notes".to_string())
            })
            .expect("default note row");
        app.state.settings.list.selected = note_index;
        let action = update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        )
        .expect("note toggle action");

        app.apply_settings_action(action);

        assert!(app.state.notepad.enabled);
        assert!(app
            .state
            .notepad
            .visible_tabs
            .contains(&"note:notes".to_string()));
        assert!(notes_dir.join("notes.md").is_file());
        assert!(app
            .state
            .notepad
            .files
            .iter()
            .any(|file| file.name == "notes"));

        let pomodoro_index = crate::app::state::settings_sidebar_panel_items(&app.state)
            .iter()
            .position(|item| item.target == crate::app::state::SidebarPanelSettingTarget::Pomodoro)
            .expect("pomodoro row");
        app.state.settings.list.selected = pomodoro_index;
        let action = update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        )
        .expect("pomodoro toggle action");
        app.apply_settings_action(action);
        assert!(app.state.pomodoro.enabled);
        assert!(app.state.pomodoro.sidebar_visible);

        let action = update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        )
        .expect("pomodoro hide action");
        app.apply_settings_action(action);
        assert!(app.state.pomodoro.enabled);
        assert!(!app.state.pomodoro.sidebar_visible);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(&directory).ok();
    }

    fn mouse_down(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn integration_recommendation(
        state: crate::integration::IntegrationStatusKind,
        available: bool,
    ) -> crate::integration::IntegrationRecommendation {
        crate::integration::IntegrationRecommendation {
            target: crate::api::schema::IntegrationTarget::Claude,
            label: "claude",
            command: "claude",
            available,
            path: std::path::PathBuf::from("/tmp/herdr-test-integration"),
            state,
        }
    }
}
