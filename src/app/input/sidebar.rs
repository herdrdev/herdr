use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{
    state::{AppState, InputOwner, ServerInputOwner, ViewLayout},
    App,
};

use super::ScrollbarClickTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettledMenuAction {
    Unsettle(crate::app::state::PaneFocusTarget),
    Resume(crate::app::state::PaneFocusTarget),
    FocusLive(crate::app::state::PaneFocusTarget),
    NewThread {
        directory: std::path::PathBuf,
        workspace: crate::app::home::HomeWorkspace,
    },
    Delete(crate::app::state::PaneFocusTarget),
}

pub(crate) enum SidebarWorkGroupKeyAction {
    Ignored,
    Consumed,
    Dispatch(Box<crate::app::home::HomeDispatchPlan>),
    RenamePod(crate::groups::GroupRecord),
    DeletePod(crate::groups::GroupRecord),
}

fn settled_sidebar_neighbor(
    rows: &[crate::app::state::PaneFocusTarget],
    selected: Option<&crate::app::state::PaneFocusTarget>,
    previous: bool,
) -> Option<crate::app::state::PaneFocusTarget> {
    if rows.is_empty() {
        return None;
    }
    let index = selected.and_then(|target| rows.iter().position(|row| row == target));
    let next = match index {
        Some(index) if previous => index.saturating_sub(1),
        Some(index) => index.saturating_add(1).min(rows.len() - 1),
        None if previous => rows.len() - 1,
        None => 0,
    };
    rows.get(next).cloned()
}

fn pod_record_for_key(state: &AppState, key: &str) -> Option<crate::groups::GroupRecord> {
    state
        .local_group_snapshot
        .iter()
        .chain(
            state
                .fleet_snapshot
                .group_catalogs
                .iter()
                .filter_map(|catalog| catalog.snapshot.as_ref()),
        )
        .flat_map(|snapshot| snapshot.groups.iter())
        .find(|record| format!("pod:{}:{}", record.id.owner, record.id.local) == key)
        .cloned()
}

fn sidebar_snooze_params(
    pane_id: String,
    preset: crate::app::state::SidebarSnoozePreset,
    tomorrow_morning: Option<u64>,
) -> Option<crate::api::schema::PaneSnoozeParams> {
    let (duration_s, snoozed_until) = match preset {
        crate::app::state::SidebarSnoozePreset::Duration(duration_s) => (Some(duration_s), None),
        crate::app::state::SidebarSnoozePreset::TomorrowMorning => (None, Some(tomorrow_morning?)),
    };
    Some(crate::api::schema::PaneSnoozeParams {
        pane_id,
        duration_s,
        snoozed_until,
    })
}

impl AppState {
    pub(crate) fn open_agent_finder(&mut self) {
        if self.agent_finder_saved_query.is_none() {
            self.agent_finder_saved_query = Some(self.sidebar_work_filter.query.clone());
        }
        self.sidebar_work_filter.query.clear();
        self.agent_finder_history = self.sidebar_work_filter.agent_finder_history.clone();
        self.sidebar_search_active = true;
        self.agent_finder_history_index = None;
        self.agent_finder_results.clear();
        self.agent_finder_partial = false;
        self.agent_finder_selected = 0;
        self.agent_finder_deadline = None;
        self.agent_finder_generation = self.agent_finder_generation.wrapping_add(1);
        self.agent_finder_side_pane = false;
        self.schedule_agent_finder_search();
    }

    fn close_agent_finder(&mut self) {
        if let Some(query) = self.agent_finder_saved_query.take() {
            let mut filter = self.sidebar_work_filter.clone();
            filter.query = query;
            self.set_sidebar_work_filter(filter);
        }
        self.sidebar_search_active = false;
        self.agent_finder_history_index = None;
        self.agent_finder_deadline = None;
        self.agent_finder_results.clear();
        self.agent_finder_partial = false;
        self.agent_finder_generation = self.agent_finder_generation.wrapping_add(1);
        self.agent_finder_side_pane = false;
    }

    pub(crate) fn accept_agent_finder_query(&mut self) {
        let query = self.sidebar_work_filter.query.trim().to_owned();
        if !query.is_empty() && self.agent_finder_history.last() != Some(&query) {
            self.agent_finder_history.push(query);
            if self.agent_finder_history.len() > 64 {
                self.agent_finder_history.remove(0);
            }
            let mut filter = self.sidebar_work_filter.clone();
            filter.agent_finder_history = self.agent_finder_history.clone();
            self.set_sidebar_work_filter(filter);
        }
        if let Some(saved_query) = self.agent_finder_saved_query.take() {
            let mut filter = self.sidebar_work_filter.clone();
            filter.query = saved_query;
            self.set_sidebar_work_filter(filter);
        }
        self.agent_finder_history_index = None;
        self.sidebar_search_active = false;
        self.agent_finder_deadline = None;
        self.agent_finder_generation = self.agent_finder_generation.wrapping_add(1);
        self.agent_finder_side_pane = false;
    }

    pub(crate) fn settled_target_has_resume_plan(
        &self,
        target: &crate::app::state::PaneFocusTarget,
    ) -> bool {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == target.workspace_id)
            .and_then(|workspace| workspace.pane_state(target.pane_id))
            .and_then(|pane| self.terminals.get(&pane.attached_terminal_id))
            .is_some_and(crate::app::settled::pane_has_resume_plan)
    }

    pub(crate) fn open_sidebar_new_menu(&mut self) {
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_search_active = false;
        self.sidebar_new_thread = None;
        self.sidebar_new_menu = Some(Default::default());
    }

    pub(crate) fn sidebar_new_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_new_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn dispatch_sidebar_new_menu_action(
        &mut self,
        action: crate::app::state::SidebarNewMenuAction,
    ) {
        self.sidebar_new_menu = None;
        match action {
            crate::app::state::SidebarNewMenuAction::NewSpace => {
                self.request_new_workspace = true;
            }
            crate::app::state::SidebarNewMenuAction::AddProject => {
                self.open_add_project_from_sidebar();
            }
            crate::app::state::SidebarNewMenuAction::NewThread => {
                self.open_sidebar_new_thread();
            }
            crate::app::state::SidebarNewMenuAction::OpenFolder => {
                self.open_folder_from_sidebar();
            }
        }
    }

    pub(crate) fn select_sidebar_new_menu_item(&mut self, index: usize) -> bool {
        let Some(action) = crate::app::state::SidebarNewMenuAction::ALL
            .get(index)
            .copied()
        else {
            return false;
        };
        self.dispatch_sidebar_new_menu_action(action);
        true
    }

    pub(crate) fn handle_sidebar_new_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut menu) = self.sidebar_new_menu.take() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                menu.selected = menu.selected.saturating_sub(1);
                self.sidebar_new_menu = Some(menu);
            }
            KeyCode::Down => {
                menu.selected = menu
                    .selected
                    .saturating_add(1)
                    .min(crate::app::state::SidebarNewMenuAction::ALL.len() - 1);
                self.sidebar_new_menu = Some(menu);
            }
            KeyCode::Enter => {
                self.select_sidebar_new_menu_item(menu.selected);
            }
            _ => self.sidebar_new_menu = Some(menu),
        }
        true
    }

    pub(crate) fn open_sidebar_new_thread(&mut self) {
        self.sidebar_new_menu = None;
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_search_active = false;
        self.sidebar_new_thread = Some(Default::default());
    }

    pub(crate) fn sidebar_new_thread_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_new_thread_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn accept_sidebar_new_thread(&mut self, position: usize) -> bool {
        let Some((path_index, _)) = crate::ui::sidebar_new_thread_matches(self)
            .get(position)
            .cloned()
        else {
            return false;
        };
        let Some(directory) = self
            .new_thread_options()
            .get(path_index)
            .map(|option| option.path.clone())
        else {
            return false;
        };
        self.sidebar_new_thread = None;
        self.open_home_composer_in_directory(directory, self.default_home_workspace());
        true
    }

    pub(crate) fn handle_sidebar_new_thread_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut picker) = self.sidebar_new_thread.take() else {
            return false;
        };
        let match_count = {
            self.sidebar_new_thread = Some(picker.clone());
            let count = crate::ui::sidebar_new_thread_matches(self).len();
            self.sidebar_new_thread = None;
            count
        };
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up => picker.filter.move_selection(-1, match_count),
            KeyCode::Down => picker.filter.move_selection(1, match_count),
            KeyCode::Backspace => picker.filter.pop(),
            KeyCode::Enter => {
                let selected = picker.filter.selected;
                self.sidebar_new_thread = Some(picker);
                self.accept_sidebar_new_thread(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() && ('1'..='9').contains(&character) =>
            {
                let selected = character.to_digit(10).unwrap_or(1) as usize - 1;
                self.sidebar_new_thread = Some(picker);
                self.accept_sidebar_new_thread(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                picker.filter.push(character);
            }
            _ => {}
        }
        self.sidebar_new_thread = Some(picker);
        true
    }

    pub(crate) fn open_sidebar_project_menu(&mut self) {
        self.sidebar_new_menu = None;
        self.sidebar_new_thread = None;
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_search_active = false;
        self.sidebar_project_menu = Some(Default::default());
    }

    pub(crate) fn sidebar_project_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_project_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    /// Apply the row at `position` in the filtered list. Row 0 of the unfiltered
    /// list clears the scope; every other row names a project.
    pub(crate) fn accept_sidebar_project_menu(&mut self, position: usize) -> bool {
        let Some((index, _)) = crate::ui::sidebar_project_menu_matches(self)
            .get(position)
            .cloned()
        else {
            return false;
        };
        let project = match index.checked_sub(1) {
            Some(project) => match self.projects.get(project) {
                Some(project) => Some(project.id.clone()),
                None => return false,
            },
            None => None,
        };
        self.sidebar_project_menu = None;
        let mut filter = self.sidebar_work_filter.clone();
        filter.project = project;
        self.set_sidebar_work_filter(filter);
        true
    }

    pub(crate) fn handle_sidebar_project_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut menu) = self.sidebar_project_menu.take() else {
            return false;
        };
        let match_count = {
            self.sidebar_project_menu = Some(menu.clone());
            let count = crate::ui::sidebar_project_menu_matches(self).len();
            self.sidebar_project_menu = None;
            count
        };
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up => menu.filter.move_selection(-1, match_count),
            KeyCode::Down => menu.filter.move_selection(1, match_count),
            KeyCode::Backspace => menu.filter.pop(),
            KeyCode::Enter => {
                let selected = menu.filter.selected;
                self.sidebar_project_menu = Some(menu);
                self.accept_sidebar_project_menu(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() && ('1'..='9').contains(&character) =>
            {
                let selected = character.to_digit(10).unwrap_or(1) as usize - 1;
                self.sidebar_project_menu = Some(menu);
                self.accept_sidebar_project_menu(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                menu.filter.push(character);
            }
            _ => {}
        }
        self.sidebar_project_menu = Some(menu);
        true
    }

    pub(crate) fn handle_sidebar_search_key(&mut self, key: KeyEvent) -> bool {
        if !self.sidebar_search_active {
            return false;
        }
        match key.code {
            KeyCode::Esc if self.agent_finder_saved_query.is_some() => self.close_agent_finder(),
            KeyCode::Enter
                if self.agent_finder_saved_query.is_some()
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.agent_finder_side_pane = true;
            }
            KeyCode::Enter if self.agent_finder_saved_query.is_some() => {
                self.accept_agent_finder_query();
            }
            KeyCode::Esc | KeyCode::Enter => self.sidebar_search_active = false,
            KeyCode::Up if key.modifiers == KeyModifiers::CONTROL => {
                self.step_agent_finder_history(-1);
            }
            KeyCode::Down if key.modifiers == KeyModifiers::CONTROL => {
                self.step_agent_finder_history(1);
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                self.agent_finder_selected = self.agent_finder_selected.saturating_sub(1);
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.agent_finder_selected = self
                    .agent_finder_selected
                    .saturating_add(1)
                    .min(self.agent_finder_results.len().saturating_sub(1));
            }
            KeyCode::Backspace => {
                let mut filter = self.sidebar_work_filter.clone();
                filter.query.pop();
                self.set_sidebar_work_filter(filter);
                self.agent_finder_history_index = None;
                self.schedule_agent_finder_search();
                self.sidebar_search_active = true;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut filter = self.sidebar_work_filter.clone();
                filter.query.clear();
                self.set_sidebar_work_filter(filter);
                self.schedule_agent_finder_search();
                self.sidebar_search_active = true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                let mut filter = self.sidebar_work_filter.clone();
                filter.query.push(character);
                self.set_sidebar_work_filter(filter);
                self.agent_finder_history_index = None;
                self.schedule_agent_finder_search();
                self.sidebar_search_active = true;
            }
            _ => {}
        }
        true
    }

    fn step_agent_finder_history(&mut self, direction: isize) {
        if self.agent_finder_history.is_empty() {
            return;
        }
        let current = self
            .agent_finder_history_index
            .unwrap_or(self.agent_finder_history.len());
        let next = if direction < 0 {
            current.saturating_sub(1)
        } else {
            current
                .saturating_add(1)
                .min(self.agent_finder_history.len())
        };
        self.agent_finder_history_index = (next < self.agent_finder_history.len()).then_some(next);
        let mut filter = self.sidebar_work_filter.clone();
        filter.query = self
            .agent_finder_history_index
            .and_then(|index| self.agent_finder_history.get(index))
            .cloned()
            .unwrap_or_default();
        self.set_sidebar_work_filter(filter);
        self.schedule_agent_finder_search();
    }

    pub(crate) fn insert_agent_finder_text(&mut self, text: &str) -> bool {
        if !self.sidebar_search_active || self.agent_finder_saved_query.is_none() {
            return false;
        }
        let mut filter = self.sidebar_work_filter.clone();
        filter
            .query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.set_sidebar_work_filter(filter);
        self.agent_finder_history_index = None;
        self.schedule_agent_finder_search();
        true
    }

    fn schedule_agent_finder_search(&mut self) {
        if self.agent_finder_saved_query.is_some() {
            self.agent_finder_generation = self.agent_finder_generation.wrapping_add(1);
            self.agent_finder_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(180));
            self.agent_finder_results.clear();
            self.agent_finder_partial = false;
            self.agent_finder_selected = 0;
        }
    }

    pub(crate) fn open_sidebar_object_menu(&mut self, target: String) {
        if self.dock_pending_write.is_some() {
            return;
        }
        self.sidebar_selected_work_group = Some(target.clone());
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_settled_menu_target = None;
        self.sidebar_object_menu = Some(crate::app::state::SidebarObjectMenuState {
            target,
            anchor_row: None,
            page: crate::app::state::SidebarObjectMenuPage::Actions,
            selected: 0,
        });
    }

    pub(crate) fn open_sidebar_sort_menu(
        &mut self,
        target: String,
        current: crate::app::state::SidebarSortMode,
        anchor: (u16, u16),
    ) {
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_settled_menu_target = None;
        self.sidebar_sort_menu = Some(crate::app::state::SidebarSortMenuState {
            target,
            current,
            anchor,
            selected: current.index(),
        });
    }

    pub(crate) fn sidebar_sort_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        crate::ui::sidebar::sidebar_sort_menu_item_at(self, self.screen_rect(), col, row)
    }

    pub(crate) fn handle_sidebar_sort_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(menu) = self.sidebar_sort_menu.as_mut() else {
            return false;
        };
        let count = crate::app::state::SidebarSortMode::ALL.len();
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => self.sidebar_sort_menu = None,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                menu.selected = menu.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                menu.selected = menu.selected.saturating_add(1).min(count - 1);
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let target = menu.target.clone();
                let mode = crate::app::state::SidebarSortMode::ALL
                    .get(menu.selected)
                    .copied()
                    .unwrap_or_default();
                self.set_sidebar_group_sort(target, mode);
            }
            _ => {}
        }
        true
    }

    pub(crate) fn apply_sidebar_sort_menu_selection(&mut self, index: usize) {
        let Some(menu) = self.sidebar_sort_menu.as_ref() else {
            return;
        };
        let Some(mode) = crate::app::state::SidebarSortMode::ALL.get(index).copied() else {
            self.sidebar_sort_menu = None;
            return;
        };
        let target = menu.target.clone();
        self.set_sidebar_group_sort(target, mode);
    }

    pub(crate) fn sidebar_subgroup_picker_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar::sidebar_subgroup_picker_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn handle_sidebar_subgroup_picker_key(&mut self, key: KeyEvent) -> bool {
        if self.sidebar_subgroup_picker.is_none() {
            return false;
        }
        let row_count = crate::ui::sidebar::sidebar_subgroup_picker_choices(self).len();
        let Some(mut picker) = self.sidebar_subgroup_picker.take() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up => picker.filter.move_selection(-1, row_count),
            KeyCode::Down => picker.filter.move_selection(1, row_count),
            KeyCode::Backspace => picker.filter.pop(),
            KeyCode::Enter => {
                let selected = picker.filter.selected;
                self.sidebar_subgroup_picker = Some(picker);
                self.accept_sidebar_subgroup_picker(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                picker.filter.push(character);
            }
            _ => {}
        }
        self.sidebar_subgroup_picker = Some(picker);
        true
    }

    pub(crate) fn accept_sidebar_subgroup_picker(&mut self, index: usize) {
        let choices = crate::ui::sidebar::sidebar_subgroup_picker_choices(self);
        let Some(picker) = self.sidebar_subgroup_picker.take() else {
            return;
        };
        let Some(name) = choices.get(index).map(|choice| choice.name().to_string()) else {
            return;
        };
        if let Some(tab) = self
            .workspaces
            .get_mut(picker.ws_idx)
            .and_then(|workspace| workspace.tabs.get_mut(picker.tab_idx))
        {
            tab.set_subgroup(Some(name));
            self.mark_session_dirty();
        }
    }

    pub(crate) fn sidebar_pod_picker_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar::sidebar_pod_picker_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    /// Remove the window's subgroup outright, from the tab context menu.
    pub(crate) fn clear_tab_subgroup(&mut self, ws_idx: usize, tab_idx: usize) {
        if let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|workspace| workspace.tabs.get_mut(tab_idx))
        {
            if tab.subgroup.is_some() {
                tab.set_subgroup(None);
                self.mark_session_dirty();
            }
        }
    }

    pub(crate) fn sidebar_settled_target_at(
        &self,
        row: u16,
    ) -> Option<crate::app::state::PaneFocusTarget> {
        let (ws_idx, _, pane_id) = self.sidebar_local_pane_at(row)?;
        self.pane_is_settled(ws_idx, pane_id)
            .then(|| crate::app::state::PaneFocusTarget {
                workspace_id: self.workspaces[ws_idx].id.clone(),
                pane_id,
            })
    }

    pub(crate) fn sidebar_local_pane_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        crate::ui::compute_tab_card_areas(self, self.view.sidebar_rect)
            .into_iter()
            .find(|card| row >= card.rect.y && row < card.rect.bottom())
            .map(|card| (card.ws_idx, card.tab_idx, card.pane_id))
            .or_else(|| self.agent_detail_target_at(row))
            .or_else(|| {
                self.sidebar_settled_workspace_target_at(row)
                    .and_then(|(ws_idx, pane_id)| {
                        let tab_idx = self.workspaces[ws_idx]
                            .tabs
                            .iter()
                            .position(|tab| tab.panes.contains_key(&pane_id))?;
                        Some((ws_idx, tab_idx, pane_id))
                    })
            })
    }

    fn sidebar_settled_workspace_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let (cards, _) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);
        let card = cards
            .iter()
            .find(|card| row >= card.rect.y && row < card.rect.bottom())?;
        let pane_id = card.settled_pane_id?;
        Some((card.ws_idx, pane_id))
    }

    pub(crate) fn sidebar_settled_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_settled_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn sidebar_snooze_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_snooze_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    /// Resolve a settled-menu row press into an action.
    ///
    /// The delete row asks once first while `ui.confirm_close` is on: the first
    /// press arms the row and returns nothing, the second press deletes. Every
    /// other row disarms it, so an armed delete cannot fire from a later press
    /// on a different row.
    pub(crate) fn select_settled_menu_action(&mut self, index: usize) -> Option<SettledMenuAction> {
        let delete_armed = std::mem::take(&mut self.sidebar_settled_menu_delete_armed);
        if index == 3 && self.confirm_close && !delete_armed {
            self.sidebar_settled_menu_delete_armed = true;
            return None;
        }
        let target = self.sidebar_settled_menu_target.take()?;
        self.sidebar_selected_settled = None;
        let ws_idx = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)?;
        let pane = self.workspaces[ws_idx].pane_state(target.pane_id)?;
        let terminal = self.terminals.get(&pane.attached_terminal_id)?;
        match index {
            0 if self.settled_target_has_resume_plan(&target) => {
                Some(SettledMenuAction::Resume(target))
            }
            0 => Some(SettledMenuAction::FocusLive(target)),
            1 => Some(SettledMenuAction::NewThread {
                directory: terminal.cwd.clone(),
                workspace: crate::app::home::HomeWorkspace::CurrentCheckout,
            }),
            2 => Some(SettledMenuAction::NewThread {
                directory: terminal.cwd.clone(),
                workspace: crate::app::home::HomeWorkspace::NewWorktree,
            }),
            3 => Some(SettledMenuAction::Delete(target)),
            4 => Some(SettledMenuAction::Unsettle(target)),
            _ => None,
        }
    }

    /// Whether this point belongs to the sidebar for keyboard purposes.
    ///
    /// The sidebar body plus whatever menu it currently has open, which the
    /// renderer floats outside the sidebar rectangle. A collapsed sidebar
    /// claims nothing: its only control expands it, and expanding is not the
    /// operator asking to drive it from the keyboard.
    pub(crate) fn sidebar_claims_pointer(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        if self.point_in_rect(self.view.sidebar_rect, col, row) {
            return true;
        }
        self.sidebar_new_menu_item_at(col, row).is_some()
            || self.sidebar_new_thread_item_at(col, row).is_some()
            || self.sidebar_project_menu_item_at(col, row).is_some()
            || self.sidebar_group_menu_item_at(col, row).is_some()
            || self.sidebar_filter_menu_item_at(col, row).is_some()
            || self.sidebar_sort_menu_item_at(col, row).is_some()
            || self.sidebar_subgroup_picker_item_at(col, row).is_some()
            || crate::ui::sidebar_object_menu_item_at(self, self.screen_rect(), col, row).is_some()
    }

    pub(crate) fn sidebar_group_mode_anchor_rect(&self) -> Rect {
        crate::ui::sidebar_group_mode_anchor_rect_for_app(self, self.view.sidebar_rect)
    }

    pub(crate) fn sidebar_group_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_group_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn open_sidebar_group_menu(&mut self) {
        self.sidebar_group_menu_selected = self.sidebar_group_mode.view_index();
        self.sidebar_group_menu_open = true;
    }

    pub(crate) fn sidebar_filter_anchor_rect(&self) -> Rect {
        crate::ui::sidebar_filter_anchor_rect(self, self.view.sidebar_rect)
    }

    pub(crate) fn sidebar_filter_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_filter_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn open_sidebar_filter_menu(&mut self) {
        self.sidebar_filter_menu_selected = 0;
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = true;
    }

    /// Apply the option at `index` to the stored filter. A team option replaces
    /// the team and leaves the assignee alone, and vice versa, so the two
    /// narrowings compose instead of resetting each other.
    pub(crate) fn select_sidebar_filter_option(&mut self, index: usize) {
        let Some(option) = crate::ui::sidebar_filter_options(self)
            .into_iter()
            .nth(index)
        else {
            self.sidebar_filter_menu_open = false;
            return;
        };
        let mut filter = self.sidebar_work_filter.clone();
        let mut keep_open = false;
        match option {
            crate::ui::SidebarFilterOption::OnlyThisMachine(enabled) => {
                filter.only_this_machine = !enabled;
                keep_open = true;
            }
            crate::ui::SidebarFilterOption::LinearTeam(team) => filter.team = team,
            crate::ui::SidebarFilterOption::LinearOwnership(ownership) => {
                filter.linear_ownership = ownership;
            }
            crate::ui::SidebarFilterOption::LinearAssignee(assignee) => {
                filter.assignee = assignee;
            }
            crate::ui::SidebarFilterOption::LinearStatus(status, selected) => {
                if selected {
                    filter.linear_statuses.remove(&status);
                } else {
                    filter.linear_statuses.insert(status);
                }
                keep_open = true;
            }
            crate::ui::SidebarFilterOption::GithubAssignee(assignee) => {
                filter.github.assignee = assignee;
            }
            crate::ui::SidebarFilterOption::GithubOwnership(ownership) => {
                filter.github.ownership = ownership;
            }
            crate::ui::SidebarFilterOption::GithubDrafts(shown) => {
                filter.github.show_drafts = !shown;
                keep_open = true;
            }
            crate::ui::SidebarFilterOption::GithubState(state) => {
                filter.github.state = state;
            }
            crate::ui::SidebarFilterOption::MissiveTeam(team) => {
                filter.missive.team = team;
            }
            crate::ui::SidebarFilterOption::MissiveAssignee(assignee) => {
                filter.missive.assignee = assignee;
            }
            crate::ui::SidebarFilterOption::MissiveClosed(shown) => {
                filter.missive.show_closed = !shown;
                keep_open = true;
            }
        }
        self.set_sidebar_work_filter(filter);
        if self.agent_finder_saved_query.is_some() {
            self.schedule_agent_finder_search();
        }
        if keep_open {
            self.sidebar_filter_menu_open = true;
            self.sidebar_filter_menu_selected = index;
        }
    }

    pub(crate) fn handle_sidebar_filter_menu_key(&mut self, key: KeyEvent) -> bool {
        if !self.sidebar_filter_menu_open {
            return false;
        }
        let count = crate::ui::sidebar_filter_options(self).len();
        match key.code {
            KeyCode::Esc => self.sidebar_filter_menu_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.sidebar_filter_menu_selected =
                    self.sidebar_filter_menu_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sidebar_filter_menu_selected = self
                    .sidebar_filter_menu_selected
                    .saturating_add(1)
                    .min(count.saturating_sub(1));
            }
            KeyCode::Enter => self.select_sidebar_filter_option(self.sidebar_filter_menu_selected),
            _ => {}
        }
        true
    }

    /// `Enter` opens a selected provider object without creating a pane; `n`
    /// keeps the direct thread-start shortcut. The selection only exists while
    /// one row is selected, so this never swallows input meant for a pane.
    pub(crate) fn handle_sidebar_work_group_key(
        &mut self,
        key: KeyEvent,
    ) -> SidebarWorkGroupKeyAction {
        let Some(selected) = self.sidebar_selected_work_group.clone() else {
            return SidebarWorkGroupKeyAction::Ignored;
        };
        if (selected == "inbox"
            || selected == format!("section:{}", crate::ui::sidebar::INBOX_SECTION_TITLE))
            && key.code == KeyCode::Enter
            && key.modifiers.is_empty()
        {
            self.toggle_sidebar_group(crate::ui::sidebar::INBOX_SECTION_TITLE);
            return SidebarWorkGroupKeyAction::Consumed;
        }
        if selected == crate::ui::sidebar::PODS_SECTION_TITLE {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.toggle_sidebar_group(crate::ui::sidebar::PODS_SECTION_TITLE);
                return SidebarWorkGroupKeyAction::Consumed;
            }
        } else if selected.starts_with("pod:") {
            match key.code {
                KeyCode::Enter if key.modifiers.is_empty() => {
                    self.toggle_sidebar_group(&selected);
                    return SidebarWorkGroupKeyAction::Consumed;
                }
                KeyCode::Char('r') if key.modifiers.is_empty() => {
                    return pod_record_for_key(self, &selected).map_or(
                        SidebarWorkGroupKeyAction::Consumed,
                        SidebarWorkGroupKeyAction::RenamePod,
                    );
                }
                KeyCode::Char('d') if key.modifiers.is_empty() => {
                    return pod_record_for_key(self, &selected).map_or(
                        SidebarWorkGroupKeyAction::Consumed,
                        SidebarWorkGroupKeyAction::DeletePod,
                    );
                }
                _ => {}
            }
        }
        // Aloops rows that are not findings have their own verbs (MAT-159):
        // loop headers open the run-history table (AC8), run lines and the
        // clean-run fold toggle, and a clean run opens its recorded log (AC7).
        // None of them dispatch, so `n` is ignored on purpose.
        use crate::ui::sidebar::aloops as aloop_rows;
        if let Some(name) = selected.strip_prefix(aloop_rows::ALOOP_LOOP_KEY_PREFIX) {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                self.open_aloop_loop_history(name);
                return SidebarWorkGroupKeyAction::Consumed;
            }
            if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                return SidebarWorkGroupKeyAction::Consumed;
            }
        } else if selected.starts_with(aloop_rows::ALOOP_RUN_KEY_PREFIX)
            || selected.starts_with(aloop_rows::ALOOP_CLEAN_KEY_PREFIX)
        {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.toggle_sidebar_group(&selected);
                return SidebarWorkGroupKeyAction::Consumed;
            }
            if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                return SidebarWorkGroupKeyAction::Consumed;
            }
        } else if let Some(rest) = selected.strip_prefix(aloop_rows::ALOOP_CLEAN_RUN_KEY_PREFIX) {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                if let Some((loop_name, at)) = rest.split_once(':') {
                    self.open_aloop_run_log(loop_name, at);
                }
                return SidebarWorkGroupKeyAction::Consumed;
            }
            if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                return SidebarWorkGroupKeyAction::Consumed;
            }
        }
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.sidebar_selected_work_group = None;
                if selected == crate::ui::sidebar_show_more_key(self.sidebar_group_mode) {
                    self.sidebar_unassigned_expanded_views
                        .insert(self.sidebar_group_mode);
                    self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                        self,
                        self.view.sidebar_rect,
                        self.workspace_scroll,
                    );
                } else if !self.open_sidebar_unassigned_object(&selected) {
                    match self.sidebar_unassigned_dispatch_plan(&selected) {
                        Ok(plan) => return SidebarWorkGroupKeyAction::Dispatch(Box::new(plan)),
                        Err(error) => self.config_diagnostic = Some(error),
                    }
                }
                SidebarWorkGroupKeyAction::Consumed
            }
            KeyCode::Char('n') if key.modifiers.is_empty() => {
                self.sidebar_selected_work_group = None;
                match self.sidebar_unassigned_dispatch_plan(&selected) {
                    Ok(plan) => SidebarWorkGroupKeyAction::Dispatch(Box::new(plan)),
                    Err(error) => {
                        self.config_diagnostic = Some(error);
                        SidebarWorkGroupKeyAction::Consumed
                    }
                }
            }
            KeyCode::Esc => {
                self.sidebar_selected_work_group = None;
                SidebarWorkGroupKeyAction::Consumed
            }
            _ => SidebarWorkGroupKeyAction::Ignored,
        }
    }

    pub(crate) fn handle_sidebar_group_menu_key(&mut self, key: KeyEvent) -> bool {
        if !self.sidebar_group_menu_open {
            return false;
        }
        match key.code {
            KeyCode::Esc => self.sidebar_group_menu_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.sidebar_group_menu_selected =
                    self.sidebar_group_menu_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sidebar_group_menu_selected =
                    self.sidebar_group_menu_selected.saturating_add(1).min(
                        crate::app::state::SidebarGroupMode::VIEWS
                            .len()
                            .saturating_sub(1),
                    );
            }
            KeyCode::Enter => {
                if let Some(mode) = crate::app::state::SidebarGroupMode::VIEWS
                    .get(self.sidebar_group_menu_selected)
                    .copied()
                {
                    self.set_sidebar_group_mode(mode);
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn workspace_list_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        crate::ui::workspace_list_rect_for_app(self, sidebar)
    }

    pub(super) fn workspace_list_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn workspace_list_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_workspace_list_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn scroll_workspace_list(&mut self, delta: i16) {
        if delta.is_negative() {
            self.workspace_scroll = self
                .workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
            );
            return;
        }

        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = self
            .workspace_scroll
            .saturating_add(delta as usize)
            .min(metrics.max_offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn scroll_collapsed_sidebar(&mut self, delta: i16) -> bool {
        if !self.sidebar_collapsed {
            return false;
        }
        let (ws_area, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() {
            return false;
        }
        let max_scroll = crate::ui::sidebar_rows(self)
            .len()
            .saturating_sub(ws_area.height as usize);
        if max_scroll == 0 {
            return false;
        }
        let next = if delta.is_negative() {
            self.workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.workspace_scroll.saturating_add(delta as usize)
        };
        self.workspace_scroll = next.min(max_scroll);
        true
    }

    pub(crate) fn sidebar_footer_rect(&self) -> Rect {
        let ws_area = self.workspace_list_rect();
        if ws_area == Rect::default() {
            return Rect::default();
        }
        let y = ws_area.y + ws_area.height;
        Rect::new(ws_area.x, y, ws_area.width, 0)
    }

    pub(crate) fn global_launcher_rect(&self) -> Rect {
        if self.view.layout == ViewLayout::Mobile {
            return self.view.mobile_menu_hit_area;
        }

        crate::ui::sidebar_header_overflow_rect(self.view.sidebar_rect)
    }

    pub(crate) fn global_menu_labels(&self) -> Vec<&'static str> {
        let mut labels = vec!["settings", "keybinds", "reload config"];
        if self.update_available.is_some() {
            labels.push("update ready");
        } else if self.latest_release_notes_available {
            labels.push("what's new");
        }
        labels.push("detach");
        labels
    }

    pub(crate) fn global_menu_rect(&self) -> Rect {
        let screen = self.screen_rect();
        let launcher = self.global_launcher_rect();
        let labels = self.global_menu_labels();
        let content_width = labels
            .iter()
            .map(|label| {
                let badge_width = if self.global_menu_item_has_badge(label) {
                    2
                } else {
                    0
                };
                label.chars().count() as u16 + badge_width
            })
            .max()
            .unwrap_or(8)
            .saturating_add(2);
        let menu_w = content_width.saturating_add(2).min(screen.width.max(1));
        let menu_h = (labels.len() as u16 + 2).min(screen.height.max(1));
        let max_x = screen.x + screen.width.saturating_sub(menu_w);
        let desired_x = launcher.x + launcher.width.saturating_sub(menu_w);
        let x = desired_x.min(max_x);
        let y = launcher
            .y
            .saturating_add(1)
            .min(screen.y + screen.height.saturating_sub(menu_h));
        Rect::new(x, y, menu_w, menu_h)
    }

    pub(super) fn on_sidebar_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let sidebar = self.view.sidebar_rect;
        let toggle = crate::ui::expanded_sidebar_toggle_rect(sidebar);
        let on_toggle = toggle.width > 0
            && col >= toggle.x
            && col < toggle.x + toggle.width
            && row >= toggle.y
            && row < toggle.y + toggle.height;
        crate::ui::sidebar_separator_col(sidebar).is_some_and(|separator_col| col == separator_col)
            && !on_toggle
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
    }

    pub(super) fn on_sidebar_toggle(&self, col: u16, row: u16) -> bool {
        let rect = if self.sidebar_collapsed {
            crate::ui::collapsed_sidebar_toggle_rect(self.view.sidebar_rect)
        } else {
            crate::ui::expanded_sidebar_toggle_rect(self.view.sidebar_rect)
        };
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_manual_sidebar_width(&mut self, divider_col: u16) {
        let sidebar = self.view.sidebar_rect;
        let width = divider_col.saturating_sub(sidebar.x).saturating_add(1);
        self.sidebar_width = width.clamp(self.sidebar_min_width, self.sidebar_max_width);
        self.sidebar_width_source = crate::app::state::SidebarWidthSource::Manual;
        self.mark_session_dirty();
    }

    pub(super) fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let footer = self.sidebar_footer_rect();
        if footer == Rect::default() {
            return None;
        }

        let (cards, _) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);

        cards.iter().find_map(|card| {
            (row >= card.rect.y && row < card.rect.y + card.rect.height).then_some(card.ws_idx)
        })
    }

    pub(super) fn sidebar_section_header_at(&self, row: u16) -> Option<&'static str> {
        crate::ui::compute_sidebar_section_header_areas(self, self.view.sidebar_rect)
            .into_iter()
            .find(|header| row >= header.rect.y && row < header.rect.y + header.rect.height)
            .map(|header| header.title)
    }

    /// Folding a group is view state, not session state, so it needs no API
    /// round trip -- but it does change the row count, so the sidebar's own
    /// scroll clamp has to run afterwards.
    pub(crate) fn toggle_sidebar_group(&mut self, title: &str) {
        if let Some(key_tail) = title.strip_prefix("device:") {
            let Some((section, host)) = key_tail.split_once('/') else {
                return;
            };
            let key = crate::ui::sidebar::devices::group_key(section, host);
            let local = host == self.agent_host_name;
            let reachable = crate::ui::sidebar::devices::host_reachable(self, host);
            let collapsed = crate::ui::sidebar::devices::group_is_collapsed(
                self, section, host, local, reachable,
            );
            self.collapsed_sidebar_groups.remove(&key);
            self.collapsed_sidebar_groups
                .remove(&format!("expanded:{key}"));
            let next_collapsed = !collapsed;
            if next_collapsed {
                self.collapsed_sidebar_groups.insert(key.clone());
            } else {
                self.collapsed_sidebar_groups
                    .insert(format!("expanded:{key}"));
            }
            self.sidebar_group_collapsed_persistence_request = Some((key, next_collapsed));
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
            );
            return;
        }
        if self.sidebar_sections_layout {
            let key = format!("sections:{title}");
            if title == crate::ui::sidebar::SETTLED_SECTION_TITLE
                || title == crate::ui::sidebar::NEEDS_YOU_SECTION_TITLE
            {
                if !self.collapsed_sidebar_groups.remove(&key) {
                    self.collapsed_sidebar_groups.insert(key);
                }
            } else if !self.collapsed_sidebar_groups.remove(&key) {
                self.collapsed_sidebar_groups.insert(key);
            }
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
            );
            return;
        }
        let key = format!("{}:{title}", self.sidebar_group_mode.collapse_namespace());
        if title.starts_with(crate::ui::sidebar::aloops::ALOOP_CLEAN_KEY_PREFIX) {
            // Clean runs are folded by default, so membership represents the
            // inverse state for this one row kind: an explicit expansion.
            if !self.collapsed_sidebar_groups.remove(&key) {
                self.collapsed_sidebar_groups.insert(key);
            }
        } else {
            let collapsed = !self.collapsed_sidebar_groups.contains(&key);
            self.set_sidebar_group_collapsed(title, collapsed);
        }
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(crate) fn set_sidebar_group_collapsed(&mut self, title: &str, collapsed: bool) {
        let key = format!("{}:{title}", self.sidebar_group_mode.collapse_namespace());
        if collapsed {
            self.collapsed_sidebar_groups.insert(key.clone());
        } else {
            self.collapsed_sidebar_groups.remove(&key);
        }
        self.sidebar_group_collapsed_persistence_request = Some((key, collapsed));
    }

    /// Collapses every repo group except the one owning the focused pane.
    ///
    /// This is a transient lens, not a preference: it writes no persistence
    /// overrides, so a restart re-seeds the defaults and a group the operator
    /// never toggled by hand keeps following them.
    pub(crate) fn focus_owning_repo_group(&mut self) -> bool {
        if self.sidebar_group_mode != crate::app::state::SidebarGroupMode::Repo {
            return false;
        }
        let Some(plan) = crate::ui::repo_group_focus_plan(self) else {
            return false;
        };
        let namespace = self.sidebar_group_mode.collapse_namespace();
        for key in &plan.group_keys {
            let full_key = format!("{namespace}:{key}");
            if *key == plan.owner_key {
                self.collapsed_sidebar_groups.remove(&full_key);
            } else {
                self.collapsed_sidebar_groups.insert(full_key);
            }
        }
        // The owning group is the one thing on screen, so its card must not
        // stay folded by an earlier per-Space disclosure click.
        if let Some(workspace) = self.workspaces.get(plan.owner_ws_idx) {
            self.sidebar_presentation
                .expanded_workspace_ids
                .insert(workspace.id.clone());
        }
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        true
    }

    /// Select a fleet agent, then keep its row in view.
    pub(crate) fn select_remote_agent_row(&mut self, agent_ref: crate::api::schema::AgentRef) {
        self.sidebar_selected_remote_agent = Some(agent_ref.clone());
        self.mark_sidebar_projection_changed();
        if let Some(target_row) = crate::ui::sidebar_rows(self).iter().position(|row| {
            matches!(
                row,
                crate::ui::SidebarRow::RemoteAgent { entry, .. }
                    if entry.agent_ref == agent_ref
            )
        }) {
            self.workspace_scroll = crate::ui::sidebar_row_scroll_for_target(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
                target_row,
            );
        }
    }

    pub(super) fn collapsed_workspace_at_row(&self, row: u16) -> Option<usize> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (ws_area, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() || row < ws_area.y || row >= ws_area.y + ws_area.height {
            return None;
        }

        let idx =
            (row - ws_area.y) as usize + crate::ui::collapsed_sidebar_row_scroll(self, ws_area);
        crate::ui::sidebar_rows(self)
            .get(idx)
            .and_then(|entry| match entry {
                crate::ui::SidebarRow::Workspace { ws_idx, .. } => Some(*ws_idx),
                crate::ui::SidebarRow::Agent { .. }
                | crate::ui::SidebarRow::PodHeader { .. }
                | crate::ui::SidebarRow::PodMember { .. }
                | crate::ui::SidebarRow::RemoteAgent { .. }
                | crate::ui::SidebarRow::Tab { .. }
                | crate::ui::SidebarRow::SectionHeader { .. }
                | crate::ui::SidebarRow::NestedHeader { .. }
                | crate::ui::SidebarRow::SymphonyJob { .. }
                | crate::ui::SidebarRow::SymphonyEmpty
                | crate::ui::SidebarRow::Divider
                | crate::ui::SidebarRow::ShelfDivider
                | crate::ui::SidebarRow::AloopLoop { .. }
                | crate::ui::SidebarRow::AloopRemoteLoop { .. }
                | crate::ui::SidebarRow::AloopRunLine { .. }
                | crate::ui::SidebarRow::AloopFinding { .. }
                | crate::ui::SidebarRow::AloopCleanRuns { .. }
                | crate::ui::SidebarRow::AloopCleanRun { .. }
                | crate::ui::SidebarRow::AloopUnreachable { .. }
                | crate::ui::SidebarRow::AloopEmpty
                | crate::ui::SidebarRow::Notes(_)
                | crate::ui::SidebarRow::Inbox(_)
                | crate::ui::SidebarRow::NeedsYou { .. }
                | crate::ui::SidebarRow::NeedsYouMore { .. }
                | crate::ui::SidebarRow::AgentRun { .. } => None,
            })
    }

    pub(super) fn collapsed_agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, crate::layout::PaneId)> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (content, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if content == Rect::default() || row < content.y || row >= content.y + content.height {
            return None;
        }
        let row_idx =
            (row - content.y) as usize + crate::ui::collapsed_sidebar_row_scroll(self, content);
        crate::ui::sidebar_rows(self)
            .get(row_idx)
            .and_then(|entry| match entry {
                crate::ui::SidebarRow::Agent { entry, .. } => entry
                    .local_target()
                    .map(|target| (target.ws_idx, target.pane_id)),
                crate::ui::SidebarRow::Workspace { .. }
                | crate::ui::SidebarRow::PodHeader { .. }
                | crate::ui::SidebarRow::PodMember { .. }
                | crate::ui::SidebarRow::RemoteAgent { .. }
                | crate::ui::SidebarRow::SectionHeader { .. }
                | crate::ui::SidebarRow::NestedHeader { .. }
                | crate::ui::SidebarRow::SymphonyJob { .. }
                | crate::ui::SidebarRow::SymphonyEmpty
                | crate::ui::SidebarRow::Divider
                | crate::ui::SidebarRow::ShelfDivider
                | crate::ui::SidebarRow::AloopLoop { .. }
                | crate::ui::SidebarRow::AloopRemoteLoop { .. }
                | crate::ui::SidebarRow::AloopRunLine { .. }
                | crate::ui::SidebarRow::AloopFinding { .. }
                | crate::ui::SidebarRow::AloopCleanRuns { .. }
                | crate::ui::SidebarRow::AloopCleanRun { .. }
                | crate::ui::SidebarRow::AloopUnreachable { .. }
                | crate::ui::SidebarRow::AloopEmpty
                | crate::ui::SidebarRow::Notes(_)
                | crate::ui::SidebarRow::Inbox(_)
                | crate::ui::SidebarRow::NeedsYou { .. }
                | crate::ui::SidebarRow::NeedsYouMore { .. }
                | crate::ui::SidebarRow::AgentRun { .. } => None,
                crate::ui::SidebarRow::Tab { entry, .. } => entry
                    .local_target()
                    .map(|target| (target.ws_idx, target.pane_id)),
            })
    }

    pub(super) fn collapsed_remote_agent_target_at(
        &self,
        row: u16,
    ) -> Option<crate::api::schema::AgentRef> {
        if !self.sidebar_collapsed {
            return None;
        }
        let (content, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if content == Rect::default() || row < content.y || row >= content.bottom() {
            return None;
        }
        let row_idx =
            usize::from(row - content.y) + crate::ui::collapsed_sidebar_row_scroll(self, content);
        crate::ui::sidebar_rows(self)
            .get(row_idx)
            .and_then(|entry| match entry {
                crate::ui::SidebarRow::RemoteAgent { entry, .. } => Some(entry.agent_ref.clone()),
                _ => None,
            })
    }

    pub(super) fn workspace_drop_target_at_row(
        &self,
        row: u16,
    ) -> Option<crate::app::state::WorkspaceDropTarget> {
        let area = self.workspace_list_rect();
        let footer = self.sidebar_footer_rect();
        if area == Rect::default() || row < area.y || row >= footer.y {
            return None;
        }

        let (cards, _) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);
        let slots = crate::ui::workspace_drop_slots(self, &cards, area);
        if slots.last().is_some_and(|(_, slot_row)| row <= *slot_row) {
            slots
                .into_iter()
                .enumerate()
                .min_by_key(|(slot_idx, (_, slot_row))| (row.abs_diff(*slot_row), *slot_idx))
                .map(|(_, (target, _))| target)
        } else {
            None
        }
    }

    pub(super) fn workspace_move_block_params(
        &self,
        source_ws_idx: usize,
        drop_target: crate::app::state::WorkspaceDropTarget,
    ) -> Option<crate::api::schema::WorkspaceMoveBlockParams> {
        let source = self.workspaces.get(source_ws_idx)?;
        if source
            .worktree_space()
            .is_some_and(|space| space.is_linked_worktree)
        {
            return None;
        }

        let roots = crate::ui::workspace_list_entries_expanded(self)
            .into_iter()
            .filter_map(|entry| match entry {
                crate::ui::WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                } => Some(ws_idx),
                crate::ui::WorkspaceListEntry::Workspace { .. } => None,
                crate::ui::WorkspaceListEntry::NestedHeader { .. } => None,
            })
            .collect::<Vec<_>>();
        let source_pos = roots.iter().position(|ws_idx| *ws_idx == source_ws_idx)?;
        let remaining_roots = roots
            .iter()
            .copied()
            .filter(|ws_idx| *ws_idx != source_ws_idx)
            .collect::<Vec<_>>();
        let insert_pos = match drop_target {
            crate::app::state::WorkspaceDropTarget::Before(target_ws_idx) => remaining_roots
                .iter()
                .position(|ws_idx| *ws_idx == target_ws_idx)?,
            crate::app::state::WorkspaceDropTarget::End => remaining_roots.len(),
        };
        if insert_pos == source_pos {
            return None;
        }

        let workspace_ids = match source.worktree_space() {
            Some(source_space) => {
                let mut ids = vec![source.id.clone()];
                ids.extend(
                    self.workspaces
                        .iter()
                        .filter(|workspace| workspace.id != source.id)
                        .filter(|workspace| {
                            workspace
                                .worktree_space()
                                .is_some_and(|space| space.key == source_space.key)
                        })
                        .map(|workspace| workspace.id.clone()),
                );
                ids
            }
            None => vec![source.id.clone()],
        };
        let before_workspace_id = match drop_target {
            crate::app::state::WorkspaceDropTarget::Before(target_ws_idx) => {
                let target = self.workspaces.get(target_ws_idx)?;
                let anchor = match crate::ui::workspace_parent_group_state(self, target_ws_idx)
                    .and_then(|_| target.worktree_space())
                {
                    Some(target_space) => self
                        .workspaces
                        .iter()
                        .find(|workspace| {
                            workspace
                                .worktree_space()
                                .is_some_and(|space| space.key == target_space.key)
                        })
                        .unwrap_or(target),
                    None => target,
                };
                Some(anchor.id.clone())
            }
            crate::app::state::WorkspaceDropTarget::End => None,
        };

        Some(crate::api::schema::WorkspaceMoveBlockParams {
            workspace_ids,
            before_workspace_id,
        })
    }

    pub(super) fn agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        if self.sidebar_collapsed {
            return None;
        }

        let (_, cards) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);
        cards.iter().find_map(|card| {
            (row >= card.rect.y && row < card.rect.y + card.rect.height).then_some((
                card.ws_idx,
                card.tab_idx,
                card.pane_id,
            ))
        })
    }

    pub(super) fn tab_target_at(&self, row: u16) -> Option<(usize, usize)> {
        if self.sidebar_collapsed {
            return None;
        }
        crate::ui::compute_tab_card_areas(self, self.view.sidebar_rect)
            .into_iter()
            .find(|card| row >= card.rect.y && row < card.rect.y + card.rect.height)
            .map(|card| (card.ws_idx, card.tab_idx))
    }
}

impl App {
    pub(crate) fn handle_window_cycle_menu_key(
        &mut self,
        key: KeyEvent,
        owner: InputOwner,
    ) -> bool {
        if !self.state.window_cycle_menu_open || !sidebar_areas_menu_accepts_input_owner(owner) {
            return false;
        }
        if crate::ui::sidebar::window_cycle_mode_menu_layout(&self.state, self.state.screen_rect())
            .is_none()
        {
            self.state.window_cycle_menu_open = false;
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.window_cycle_menu_selected =
                    self.state.window_cycle_menu_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.state.window_cycle_menu_selected = self
                    .state
                    .window_cycle_menu_selected
                    .saturating_add(1)
                    .min(2);
            }
            KeyCode::Home => self.state.window_cycle_menu_selected = 0,
            KeyCode::End => self.state.window_cycle_menu_selected = 2,
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.apply_window_cycle_menu_choice(self.state.window_cycle_menu_selected);
            }
            KeyCode::Esc | KeyCode::Char('q') => self.state.window_cycle_menu_open = false,
            _ => {}
        }
        true
    }

    pub(super) fn handle_window_cycle_menu_mouse(
        &mut self,
        mouse: MouseEvent,
        owner: InputOwner,
    ) -> bool {
        if !sidebar_areas_menu_accepts_input_owner(owner) {
            return false;
        }
        if self.state.window_cycle_menu_open {
            if let Some(index) = crate::ui::sidebar::window_cycle_mode_menu_index_at(
                &self.state,
                self.state.screen_rect(),
                mouse.column,
                mouse.row,
            ) {
                if matches!(mouse.kind, MouseEventKind::Moved) {
                    self.state.window_cycle_menu_selected = index;
                    return true;
                }
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.state.window_cycle_menu_selected = index;
                    self.apply_window_cycle_menu_choice(index);
                    return true;
                }
            }
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.state.window_cycle_menu_open = false;
            }
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.point_in_rect(
                self.state.view.window_cycle_mode_hit_area,
                mouse.column,
                mouse.row,
            )
        {
            self.state.sidebar_areas_menu_selected = None;
            self.state.window_cycle_menu_selected = match self.state.window_cycle_mode {
                crate::config::WindowCycleModeConfig::ThisMachine => 0,
                crate::config::WindowCycleModeConfig::ThisMachineAndFleet => 1,
            };
            self.state.window_cycle_menu_open = true;
            return true;
        }
        false
    }

    fn apply_window_cycle_menu_choice(&mut self, index: usize) {
        match index {
            0 => self.set_window_cycle_mode(crate::config::WindowCycleModeConfig::ThisMachine),
            1 => self
                .set_window_cycle_mode(crate::config::WindowCycleModeConfig::ThisMachineAndFleet),
            2 => {
                self.state.skip_collapsed_cycle = !self.state.skip_collapsed_cycle;
                self.schedule_session_save();
                self.save_config_edit(crate::app::settings_general::ConfigEdit::Bool {
                    section: "ui",
                    key: "skip_collapsed_cycle",
                    value: self.state.skip_collapsed_cycle,
                });
                self.state.window_cycle_menu_open = true;
            }
            _ => {}
        }
    }

    fn set_window_cycle_mode(&mut self, mode: crate::config::WindowCycleModeConfig) {
        self.state.window_cycle_mode = mode;
        self.state.window_cycle_menu_open = false;
        self.schedule_session_save();
        let value = match mode {
            crate::config::WindowCycleModeConfig::ThisMachine => "this-machine",
            crate::config::WindowCycleModeConfig::ThisMachineAndFleet => "this-machine-and-fleet",
        };
        self.save_config_edit(crate::app::settings_general::ConfigEdit::Text {
            section: "ui",
            key: "window_cycle_mode",
            value: value.into(),
        });
    }

    pub(crate) fn handle_sidebar_areas_menu_key(
        &mut self,
        key: KeyEvent,
        owner: InputOwner,
    ) -> bool {
        if self.state.sidebar_areas_menu_selected.is_none()
            || !sidebar_areas_menu_accepts_input_owner(owner)
        {
            return false;
        }
        if crate::ui::sidebar::sidebar_areas_menu_layout(&self.state, self.state.screen_rect())
            .is_none()
        {
            self.state.sidebar_areas_menu_selected = None;
            return false;
        }
        const LAST_AREA_INDEX: usize = 8;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(selected) = &mut self.state.sidebar_areas_menu_selected {
                    *selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(selected) = &mut self.state.sidebar_areas_menu_selected {
                    *selected = selected.saturating_add(1).min(LAST_AREA_INDEX);
                }
            }
            KeyCode::Home => self.state.sidebar_areas_menu_selected = Some(0),
            KeyCode::End => self.state.sidebar_areas_menu_selected = Some(LAST_AREA_INDEX),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(area) = self
                    .state
                    .sidebar_areas_menu_selected
                    .and_then(crate::ui::sidebar::sidebar_area_at_menu_index)
                {
                    self.toggle_sidebar_area(area);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.state.sidebar_areas_menu_selected = None;
            }
            _ => {}
        }
        true
    }

    pub(super) fn handle_sidebar_areas_menu_mouse(
        &mut self,
        mouse: MouseEvent,
        owner: InputOwner,
    ) -> bool {
        if !sidebar_areas_menu_accepts_input_owner(owner) {
            return false;
        }
        if self.state.sidebar_areas_menu_selected.is_some() {
            if let Some(index) = crate::ui::sidebar::sidebar_areas_menu_index_at(
                &self.state,
                self.state.screen_rect(),
                mouse.column,
                mouse.row,
            ) {
                if matches!(mouse.kind, MouseEventKind::Moved) {
                    self.state.sidebar_areas_menu_selected = Some(index);
                    return true;
                }
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.state.sidebar_areas_menu_selected = Some(index);
                    if let Some(area) = crate::ui::sidebar::sidebar_area_at_menu_index(index) {
                        self.toggle_sidebar_area(area);
                    }
                    return true;
                }
            }
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.state.sidebar_areas_menu_selected = None;
            }
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.point_in_rect(
                self.state.view.sidebar_areas_hit_area,
                mouse.column,
                mouse.row,
            )
        {
            self.state.window_cycle_menu_open = false;
            self.state.sidebar_areas_menu_selected = Some(0);
            return true;
        }
        false
    }

    /// Open the pod picker for the focused local pane. The action is safe to
    /// expose in the palette even when no pod exists because a typed name can
    /// create the first local pod.
    pub(crate) fn open_sidebar_pod_picker_for_focused_pane(&mut self) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
        else {
            return false;
        };
        let anchor =
            crate::ui::sidebar::compute_agent_card_areas(&self.state, self.state.view.sidebar_rect)
                .into_iter()
                .find(|card| card.ws_idx == ws_idx && card.pane_id == pane_id)
                .map(|card| (card.rect.x.saturating_add(2), card.rect.y))
                .unwrap_or((
                    self.state.view.sidebar_rect.x.saturating_add(2),
                    self.state.view.sidebar_rect.y.saturating_add(1),
                ));
        self.state.sidebar_pod_picker = Some(crate::app::state::SidebarPodPickerState {
            ws_idx,
            pane_id,
            anchor,
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });
        self.state.focus_client_on_sidebar();
        true
    }

    pub(crate) fn handle_sidebar_pod_picker_key(&mut self, key: KeyEvent) -> bool {
        if self.state.sidebar_pod_picker.is_none() {
            return false;
        }
        let choices = crate::ui::sidebar::sidebar_pod_picker_choices(&self.state);
        let Some(mut picker) = self.state.sidebar_pod_picker.take() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up | KeyCode::Down => {
                let len = choices.len();
                if len > 0 {
                    let current = picker.filter.selected.min(len - 1);
                    for distance in 1..=len {
                        let candidate = if key.code == KeyCode::Up {
                            (current + len - (distance % len)) % len
                        } else {
                            (current + distance) % len
                        };
                        if choices
                            .get(candidate)
                            .is_some_and(crate::ui::sidebar::SidebarPodChoice::enabled)
                        {
                            picker.filter.selected = candidate;
                            break;
                        }
                    }
                }
            }
            KeyCode::Backspace => picker.filter.pop(),
            KeyCode::Enter => {
                let selected = picker.filter.selected;
                self.state.sidebar_pod_picker = Some(picker);
                self.accept_sidebar_pod_picker(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                picker.filter.push(character);
            }
            _ => {}
        }
        self.state.sidebar_pod_picker = Some(picker);
        true
    }

    pub(crate) fn accept_sidebar_pod_picker(&mut self, index: usize) {
        let choices = crate::ui::sidebar::sidebar_pod_picker_choices(&self.state);
        let Some(picker) = self.state.sidebar_pod_picker.take() else {
            return;
        };
        let Some(choice) = choices.get(index).cloned() else {
            return;
        };
        if !choice.enabled() {
            self.state.sidebar_pod_picker = Some(picker);
            return;
        }
        let target = match choice {
            crate::ui::sidebar::SidebarPodChoice::Existing { id, .. } => Some(id),
            crate::ui::sidebar::SidebarPodChoice::NoPod => None,
            crate::ui::sidebar::SidebarPodChoice::Create(name) => {
                let Some(expected_revision) = self
                    .state
                    .local_group_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.revision)
                else {
                    return;
                };
                let response = self.dispatch_runtime_mutation(
                    "sidebar-pod-create",
                    crate::api::schema::Method::GroupCreate(
                        crate::api::schema::GroupCreateParams {
                            name,
                            expected_revision,
                        },
                    ),
                );
                let Ok(success) =
                    serde_json::from_str::<crate::api::schema::SuccessResponse>(&response)
                else {
                    self.show_pod_mutation_error("Pod not created", &response);
                    return;
                };
                let crate::api::schema::ResponseResult::GroupMutation { record, .. } =
                    success.result
                else {
                    return;
                };
                Some(record.id)
            }
        };
        let Some(public_pane_id) = self.public_pane_id(picker.ws_idx, picker.pane_id) else {
            return;
        };
        let Some(pane) = self
            .state
            .workspaces
            .get(picker.ws_idx)
            .and_then(|workspace| workspace.pane_state(picker.pane_id))
        else {
            return;
        };
        self.runtime_pane_group_set(crate::api::schema::PaneGroupSetParams {
            pane_id: public_pane_id,
            group_id: target,
            expected_revision: pane.group_membership.revision,
            expected_pane_authority: self
                .state
                .local_group_snapshot
                .as_ref()
                .map(|snapshot| snapshot.authority_id.clone()),
            expected_pane_incarnation: Some(pane.attached_terminal_id.to_string()),
        });
    }
}

fn sidebar_areas_menu_accepts_input_owner(owner: InputOwner) -> bool {
    match owner {
        InputOwner::Pomodoro
        | InputOwner::Client(_)
        | InputOwner::AddProject
        | InputOwner::Popup => false,
        InputOwner::Server(
            ServerInputOwner::Navigate
            | ServerInputOwner::Prefix
            | ServerInputOwner::Copy
            | ServerInputOwner::Resize,
        ) => true,
        InputOwner::Server(_) => false,
        _ => true,
    }
}

impl super::super::App {
    fn sidebar_pane_lifecycle_state(
        &self,
        target: &crate::app::state::SidebarPaneLifecycleTarget,
    ) -> Option<(bool, Option<u64>)> {
        match &target {
            crate::app::state::SidebarPaneLifecycleTarget::Local(target) => {
                let ws_idx = self
                    .state
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == target.workspace_id)?;
                let pane = self
                    .state
                    .workspaces
                    .get(ws_idx)?
                    .pane_state(target.pane_id)?;
                let snoozed_until = pane.snoozed_until();
                (snoozed_until.is_some() || self.state.pane_can_snooze(ws_idx, target.pane_id))
                    .then_some((snoozed_until.is_some(), snoozed_until))
            }
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) => self
                .state
                .remote_agent_panel_entries
                .iter()
                .find(|entry| {
                    entry.agent_ref == *agent_ref
                        && entry.host_fresh
                        && !entry.settled
                        && !crate::ui::sidebar::entry_needs_human_attention(entry)
                })
                .map(|entry| (entry.snoozed_until.is_some(), entry.snoozed_until)),
        }
    }

    pub(crate) fn local_sidebar_pane_lifecycle_target(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::app::state::SidebarPaneLifecycleTarget> {
        Some(crate::app::state::SidebarPaneLifecycleTarget::Local(
            crate::app::state::PaneFocusTarget {
                workspace_id: self.state.workspaces.get(ws_idx)?.id.clone(),
                pane_id,
            },
        ))
    }

    pub(crate) fn sidebar_pane_lifecycle_public_id(
        &self,
        target: &crate::app::state::SidebarPaneLifecycleTarget,
    ) -> Option<String> {
        match &target {
            crate::app::state::SidebarPaneLifecycleTarget::Local(target) => {
                let ws_idx = self
                    .state
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == target.workspace_id)?;
                self.public_pane_id(ws_idx, target.pane_id)
            }
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) => {
                Some(agent_ref.agent.clone())
            }
        }
    }

    pub(crate) fn show_remote_pane_lifecycle_error(
        &mut self,
        agent_ref: &crate::api::schema::AgentRef,
        message: String,
    ) {
        self.show_pane_lifecycle_error(format!("{} pane action failed", agent_ref.host), message);
    }

    fn show_pane_lifecycle_error(&mut self, title: String, message: String) {
        let previous_toast = self.state.toast.clone();
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title,
            context: message,
            position: None,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
    }

    fn show_sidebar_shelf_toast(
        &mut self,
        target: &crate::app::state::SidebarPaneLifecycleTarget,
        shelf: &str,
    ) {
        let previous_toast = self.state.toast.clone();
        let name = match target {
            crate::app::state::SidebarPaneLifecycleTarget::Local(pane_target) => self
                .state
                .workspaces
                .iter()
                .find(|workspace| workspace.id == pane_target.workspace_id)
                .and_then(|workspace| {
                    workspace
                        .tabs
                        .iter()
                        .enumerate()
                        .find_map(|(tab_idx, tab)| {
                            tab.panes.contains_key(&pane_target.pane_id).then(|| {
                                workspace.tab_display_name_from(&self.state.terminals, tab_idx)
                            })
                        })
                })
                .flatten()
                .unwrap_or_else(|| "Tab".to_string()),
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) => self
                .state
                .remote_agent_panel_entries
                .iter()
                .find(|entry| entry.agent_ref == *agent_ref)
                .map(|entry| entry.render_title.clone())
                .unwrap_or_else(|| agent_ref.agent.clone()),
        };
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: name,
            context: format!("Moved to {shelf}"),
            position: None,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
    }

    fn show_local_pane_lifecycle_error(&mut self, action: &str, response: &str) -> bool {
        let Ok(error) = serde_json::from_str::<crate::api::schema::ErrorResponse>(response) else {
            return false;
        };
        self.show_pane_lifecycle_error(format!("Pane {action} failed"), error.error.message);
        true
    }

    pub(crate) fn remote_pane_lifecycle_refused(
        &mut self,
        target: &crate::app::state::SidebarPaneLifecycleTarget,
    ) {
        let crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) = target else {
            return;
        };
        let message = self
            .state
            .remote_agent_panel_entries
            .iter()
            .find(|entry| entry.agent_ref == *agent_ref)
            .map_or_else(
                || format!("pane {} is no longer available", agent_ref.agent),
                |entry| {
                    if !entry.host_fresh {
                        format!("owner {} is not available for pane actions", agent_ref.host)
                    } else if entry.settled {
                        format!("pane {} is already settled", agent_ref.agent)
                    } else if crate::ui::sidebar::entry_needs_human_attention(entry) {
                        format!("pane {} needs attention", agent_ref.agent)
                    } else {
                        format!("pane {} lifecycle state changed", agent_ref.agent)
                    }
                },
            );
        self.show_remote_pane_lifecycle_error(agent_ref, message);
    }

    pub(crate) fn dispatch_sidebar_pane_snooze(
        &mut self,
        target: crate::app::state::SidebarPaneLifecycleTarget,
        params: crate::api::schema::PaneSnoozeParams,
    ) {
        match &target {
            crate::app::state::SidebarPaneLifecycleTarget::Local(_) => {
                let response = self.runtime_pane_snooze("tui.sidebar.snooze", params);
                if !self.show_local_pane_lifecycle_error("snooze", &response) {
                    self.show_sidebar_shelf_toast(&target, "Snoozed");
                }
            }
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) => {
                if let Err(error) = self.remote_pane_snooze(agent_ref.clone(), params) {
                    self.show_remote_pane_lifecycle_error(agent_ref, error);
                } else {
                    self.show_sidebar_shelf_toast(&target, "Snoozed");
                }
            }
        }
    }

    fn dispatch_sidebar_pane_unsnooze(
        &mut self,
        target: crate::app::state::SidebarPaneLifecycleTarget,
    ) {
        match &target {
            crate::app::state::SidebarPaneLifecycleTarget::Local(_) => {
                if let Some(pane_id) = self.sidebar_pane_lifecycle_public_id(&target) {
                    let response = self.runtime_pane_unsnooze("tui.sidebar.unsnooze", pane_id);
                    if !self.show_local_pane_lifecycle_error("unsnooze", &response) {
                        self.show_sidebar_shelf_toast(&target, "Active");
                    }
                }
            }
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) => {
                if let Err(error) = self.remote_pane_unsnooze(agent_ref.clone()) {
                    self.show_remote_pane_lifecycle_error(agent_ref, error);
                } else {
                    self.show_sidebar_shelf_toast(&target, "Active");
                }
            }
        }
    }

    pub(crate) fn open_sidebar_snooze_menu(
        &mut self,
        target: crate::app::state::SidebarPaneLifecycleTarget,
        column: u16,
        row: u16,
    ) {
        let Some((snoozed, _)) = self.sidebar_pane_lifecycle_state(&target) else {
            return;
        };
        self.state.sidebar_snooze = Some(crate::app::state::SidebarSnoozeUiState {
            target,
            anchor: (column, row),
            selected: crate::app::state::sidebar_snooze_menu_items(snoozed)[0].1,
            time_draft: None,
            error: None,
        });
    }

    pub(crate) fn open_snooze_time_input(
        &mut self,
        target: crate::app::state::SidebarPaneLifecycleTarget,
    ) {
        let Some((_, snoozed_until)) = self.sidebar_pane_lifecycle_state(&target) else {
            return;
        };
        let prefill = snoozed_until
            .and_then(crate::platform::local_datetime_at)
            .map(|deadline| format!("{:02}:{:02}", deadline.hour(), deadline.minute()))
            .unwrap_or_default();
        let anchor = self
            .state
            .sidebar_snooze
            .as_ref()
            .filter(|snooze| snooze.target == target)
            .map_or(
                (
                    self.state.view.sidebar_rect.x,
                    self.state.view.sidebar_rect.y,
                ),
                |snooze| snooze.anchor,
            );
        self.state.sidebar_snooze = Some(crate::app::state::SidebarSnoozeUiState {
            target,
            anchor,
            selected: crate::app::state::SidebarSnoozeMenuAction::SetTime,
            time_draft: Some(prefill),
            error: None,
        });
    }

    pub(crate) fn apply_sidebar_snooze_menu_action(
        &mut self,
        selected: crate::app::state::SidebarSnoozeMenuAction,
    ) {
        let Some(snooze) = self.state.sidebar_snooze.as_ref() else {
            return;
        };
        if snooze.time_draft.is_some() {
            return;
        }
        let target = snooze.target.clone();
        let Some((snoozed, _)) = self.sidebar_pane_lifecycle_state(&target) else {
            self.state.sidebar_snooze = None;
            self.remote_pane_lifecycle_refused(&target);
            return;
        };
        let actions = crate::app::state::sidebar_snooze_menu_items(snoozed);
        let Some((_, action)) = actions.iter().find(|(_, action)| *action == selected) else {
            self.state.sidebar_snooze = None;
            return;
        };
        match action {
            crate::app::state::SidebarSnoozeMenuAction::Preset(preset) => {
                let tomorrow_morning = matches!(
                    preset,
                    crate::app::state::SidebarSnoozePreset::TomorrowMorning
                )
                .then(crate::platform::tomorrow_morning_unix)
                .flatten();
                let Some(public_pane_id) = self.sidebar_pane_lifecycle_public_id(&target) else {
                    return;
                };
                let Some(params) = sidebar_snooze_params(public_pane_id, *preset, tomorrow_morning)
                else {
                    return;
                };
                self.state.sidebar_snooze = None;
                self.dispatch_sidebar_pane_snooze(target, params);
            }
            crate::app::state::SidebarSnoozeMenuAction::SetTime => {
                self.open_snooze_time_input(target);
            }
            crate::app::state::SidebarSnoozeMenuAction::Unsnooze => {
                self.state.sidebar_snooze = None;
                self.dispatch_sidebar_pane_unsnooze(target);
            }
        }
    }

    pub(crate) fn handle_sidebar_snooze_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(snooze) = self.state.sidebar_snooze.as_ref() else {
            return false;
        };
        if snooze.time_draft.is_some() {
            return false;
        }
        let Some((snoozed, _)) = self.sidebar_pane_lifecycle_state(&snooze.target) else {
            self.state.sidebar_snooze = None;
            return true;
        };
        let item_count = crate::app::state::sidebar_snooze_menu_items(snoozed);
        match key.code {
            KeyCode::Esc => self.state.sidebar_snooze = None,
            KeyCode::Up | KeyCode::Char('k') => {
                let selected = item_count
                    .iter()
                    .position(|(_, action)| *action == snooze.selected)
                    .unwrap_or(0)
                    .saturating_sub(1);
                if let (Some(snooze), Some((_, action))) =
                    (self.state.sidebar_snooze.as_mut(), item_count.get(selected))
                {
                    snooze.selected = *action;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let selected = item_count
                    .iter()
                    .position(|(_, action)| *action == snooze.selected)
                    .unwrap_or(0)
                    .saturating_add(1)
                    .min(item_count.len().saturating_sub(1));
                if let (Some(snooze), Some((_, action))) =
                    (self.state.sidebar_snooze.as_mut(), item_count.get(selected))
                {
                    snooze.selected = *action;
                }
            }
            KeyCode::Enter => {
                let action = self
                    .state
                    .sidebar_snooze
                    .as_ref()
                    .map(|snooze| snooze.selected);
                if let Some(action) = action {
                    self.apply_sidebar_snooze_menu_action(action);
                }
            }
            _ => {}
        }
        true
    }

    pub(crate) fn handle_sidebar_session_action_key(&mut self, key: KeyEvent) -> bool {
        if !self.state.sidebar_focused
            || !matches!(
                self.state.effective_interaction_mode(),
                crate::app::Mode::Terminal | crate::app::Mode::Navigate
            )
            || !key.modifiers.is_empty()
            || self.state.sidebar_settled_menu_target.is_some()
            || self.state.sidebar_selected_settled.is_some()
            || self.state.sidebar_object_menu.is_some()
            || self.state.sidebar_sort_menu.is_some()
            || self.state.sidebar_group_menu_open
            || self.state.sidebar_filter_menu_open
            || self.state.sidebar_subgroup_picker.is_some()
            || self.state.sidebar_new_menu.is_some()
            || self.state.sidebar_new_thread.is_some()
            || self.state.sidebar_project_menu.is_some()
            || self.state.sidebar_search_active
            || self.state.sidebar_selected_work_group.is_some()
        {
            return false;
        }
        let target = if let Some(agent_ref) = self.state.sidebar_selected_remote_agent.clone() {
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref)
        } else {
            let Some(ws_idx) = self.state.active else {
                return false;
            };
            let Some(pane_id) = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(crate::workspace::Workspace::focused_pane_id)
            else {
                return false;
            };
            let Some(target) = self.local_sidebar_pane_lifecycle_target(ws_idx, pane_id) else {
                return false;
            };
            target
        };
        match key.code {
            KeyCode::Char('z') => {
                if self.sidebar_pane_lifecycle_state(&target).is_none() {
                    return false;
                }
                let anchor = match &target {
                    crate::app::state::SidebarPaneLifecycleTarget::Local(target) => self
                        .state
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.id == target.workspace_id)
                        .and_then(|ws_idx| {
                            crate::ui::compute_tab_card_areas(
                                &self.state,
                                self.state.view.sidebar_rect,
                            )
                            .into_iter()
                            .find(|card| card.ws_idx == ws_idx && card.pane_id == target.pane_id)
                        })
                        .map(|card| (card.rect.right().saturating_sub(4), card.rect.y))
                        .unwrap_or((
                            self.state.view.sidebar_rect.x,
                            self.state.view.sidebar_rect.y,
                        )),
                    crate::app::state::SidebarPaneLifecycleTarget::Remote(_) => (
                        self.state.view.sidebar_rect.right().saturating_sub(4),
                        self.state.view.sidebar_rect.y,
                    ),
                };
                self.open_sidebar_snooze_menu(target, anchor.0, anchor.1);
                self.state.sidebar_snooze.is_some()
            }
            KeyCode::Char('s') => match &target {
                crate::app::state::SidebarPaneLifecycleTarget::Local(_) => {
                    self.settle_sidebar_pane(target);
                    true
                }
                crate::app::state::SidebarPaneLifecycleTarget::Remote(_) => {
                    let Some((snoozed, _)) = self.sidebar_pane_lifecycle_state(&target) else {
                        return false;
                    };
                    if snoozed {
                        false
                    } else {
                        self.settle_sidebar_pane(target);
                        true
                    }
                }
            },
            _ => false,
        }
    }

    pub(crate) fn handle_sidebar_object_menu_key(&mut self, key: KeyEvent) -> bool {
        if self.state.sidebar_object_menu.is_none() {
            if key.code != KeyCode::Char('m') || !key.modifiers.is_empty() {
                return false;
            }
            let Some(target) = self.state.sidebar_selected_work_group.clone() else {
                return false;
            };
            self.state.open_sidebar_object_menu(target);
            if crate::ui::sidebar_object_menu_items(&self.state).is_empty() {
                self.state.sidebar_object_menu = None;
                return false;
            }
            return true;
        }

        let page = self
            .state
            .sidebar_object_menu
            .as_ref()
            .map(|menu| menu.page)
            .unwrap_or_default();
        if page == crate::app::state::SidebarObjectMenuPage::Confirmation {
            match key.code {
                KeyCode::Char('y' | 'Y') if key.modifiers.is_empty() => {
                    self.handle_pending_dock_write_key(key);
                    self.state.sidebar_object_menu = None;
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') if key.modifiers.is_empty() => {
                    self.handle_pending_dock_write_key(key);
                    self.state.sidebar_object_menu = None;
                }
                _ => {}
            }
            return true;
        }

        let count = crate::ui::sidebar_object_menu_items(&self.state).len();
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => self.state.sidebar_object_menu = None,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
                    menu.selected = menu.selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
                    menu.selected = menu.selected.saturating_add(1).min(count.saturating_sub(1));
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let index = self
                    .state
                    .sidebar_object_menu
                    .as_ref()
                    .map_or(0, |menu| menu.selected);
                self.apply_sidebar_object_menu_action(index);
            }
            _ => {}
        }
        true
    }

    pub(crate) fn apply_sidebar_object_menu_action(&mut self, index: usize) {
        let Some(item) = crate::ui::sidebar_object_menu_items(&self.state)
            .get(index)
            .copied()
        else {
            return;
        };
        if let crate::ui::SidebarObjectMenuItem::Ticket(action) = item {
            let enabled = crate::ui::sidebar_ticket_action_entries(&self.state)
                .into_iter()
                .find(|entry| entry.action == action)
                .is_some_and(|entry| entry.enabled());
            if enabled {
                self.apply_sidebar_ticket_action(action);
            }
            return;
        }
        match item {
            crate::ui::SidebarObjectMenuItem::PullRequest(action) => {
                let action_enabled = crate::ui::sidebar_pull_request_actions(&self.state)
                    .get(index)
                    .is_some_and(|action| action.enabled());
                if !action_enabled {
                    return;
                }
                let Some(key) = crate::ui::sidebar_pull_request_key(&self.state) else {
                    self.state.config_diagnostic =
                        Some("pull request is no longer available".to_string());
                    self.state.sidebar_object_menu = None;
                    return;
                };
                self.state.sidebar_object_menu = None;
                self.activate_pr_action(key, action);
            }
            crate::ui::SidebarObjectMenuItem::StartThread => {
                let target = self
                    .state
                    .sidebar_object_menu
                    .as_ref()
                    .map(|menu| menu.target.clone());
                self.state.sidebar_object_menu = None;
                let Some(target) = target else { return };
                match self.state.sidebar_unassigned_dispatch_plan(&target) {
                    Ok(plan) => self.dispatch_sidebar_work_group_plan(plan),
                    Err(error) => self.state.config_diagnostic = Some(error),
                }
            }
            crate::ui::SidebarObjectMenuItem::CopyMissiveUrl => {
                let url = crate::ui::sidebar_missive_copy_url(&self.state);
                self.state.sidebar_object_menu = None;
                if let Some(url) = url {
                    self.state.request_clipboard_write = Some(url.into_bytes());
                } else {
                    self.state.config_diagnostic =
                        Some("conversation is no longer available".to_string());
                }
            }
            crate::ui::SidebarObjectMenuItem::Ticket(_) => {}
        }
    }

    fn sidebar_ticket_parts(
        &self,
    ) -> Option<(
        crate::app::state::WorkItemKey,
        crate::work_index::WorkTicket,
        String,
    )> {
        let identifier = crate::ui::sidebar_ticket_target(&self.state)?;
        let source = self
            .state
            .work_index_snapshot
            .as_ref()?
            .items
            .iter()
            .find(|item| {
                item.ticket_details
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
            })?;
        let ticket = source
            .ticket_details
            .iter()
            .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))?
            .clone();
        Some((
            crate::app::state::WorkItemKey {
                repo: String::new(),
                pr_number: None,
                pr_url: None,
                ticket_id: Some(identifier),
            },
            ticket,
            source.repo.clone(),
        ))
    }

    fn stage_sidebar_ticket_write(&mut self, write: crate::work_index::WorkItemWrite) {
        self.state.dock_pending_write = Some(write);
        self.state.dock_write_notice = None;
        if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
            menu.page = crate::app::state::SidebarObjectMenuPage::Confirmation;
            menu.selected = 0;
        }
    }

    fn apply_sidebar_ticket_action(&mut self, action: crate::ui::ticket_actions::TicketAction) {
        use crate::ui::ticket_actions::TicketAction;
        let Some((key, ticket, repo)) = self.sidebar_ticket_parts() else {
            self.state.config_diagnostic = Some("ticket is no longer available".into());
            self.state.sidebar_object_menu = None;
            return;
        };
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &ticket,
            self.state.work_item_detail_cache.get(&key),
            self.state.work_index_session.linear.viewer.as_deref(),
            self.state.work_index_session.linear_viewer_identity(),
            crate::ui::dock::pr::focused_pr_key(&self.state).is_some(),
        );
        match action {
            TicketAction::TransitionMenu | TicketAction::PriorityMenu => {
                if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
                    menu.page = if action == TicketAction::TransitionMenu {
                        crate::app::state::SidebarObjectMenuPage::TicketTransitions
                    } else {
                        crate::app::state::SidebarObjectMenuPage::TicketPriorities
                    };
                    menu.selected = 0;
                }
            }
            TicketAction::Refresh => {
                self.next_work_index_refresh = std::time::Instant::now();
                self.state.sidebar_object_menu = None;
            }
            TicketAction::AskQuestion => {
                self.state.sidebar_object_menu = None;
                self.open_ticket_home(
                    ticket,
                    repo,
                    format!("About {} ({}): ", context.identifier, context.title),
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::Explain => {
                self.state.sidebar_object_menu = None;
                self.open_ticket_home(
                    ticket,
                    repo,
                    format!(
                        "Summarise {} ({}). Include the ticket, linked pull requests, and the acceptance criteria block.",
                        context.identifier, context.title
                    ),
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::WorkInThread => {
                let prompt = Self::ticket_work_prompt(&ticket);
                self.state.sidebar_object_menu = None;
                self.open_ticket_home(
                    ticket,
                    repo,
                    prompt,
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::Transition(choice) => self.stage_sidebar_ticket_write(
                crate::work_index::WorkItemWrite::TransitionTicket {
                    identifier: ticket.identifier,
                    state: choice.label().to_string(),
                },
            ),
            TicketAction::AssignToMe => {
                let Some(viewer) = context.viewer_id else {
                    return;
                };
                self.stage_sidebar_ticket_write(crate::work_index::WorkItemWrite::AssignTicket {
                    identifier: ticket.identifier,
                    assignee: viewer,
                });
            }
            TicketAction::SetPriority(priority) => self.stage_sidebar_ticket_write(
                crate::work_index::WorkItemWrite::SetTicketPriority {
                    identifier: ticket.identifier,
                    priority,
                },
            ),
            TicketAction::LinkPr => {
                let Some(pr) = crate::ui::dock::pr::focused_pr_key(&self.state) else {
                    return;
                };
                let (Some(number), Some(url)) = (pr.pr_number, pr.pr_url) else {
                    return;
                };
                self.stage_sidebar_ticket_write(
                    crate::work_index::WorkItemWrite::LinkTicketPullRequest {
                        identifier: ticket.identifier,
                        title: format!("{}#{number}", pr.repo),
                        url,
                    },
                );
            }
            TicketAction::Comment => {
                let mut view = crate::app::state::WorkViewState::new(
                    true,
                    self.state.work_index_snapshot.clone(),
                );
                view.projection = crate::app::state::WorkProjection::Tickets;
                view.selected = Some(key);
                view.ticket_comment_draft = Some(String::new());
                self.state.sidebar_object_menu = None;
                self.state.work_view = Some(view);
            }
            TicketAction::OpenInLinear => {
                self.state.sidebar_object_menu = None;
                if let Some(url) = context.url {
                    if let Err(error) = crate::platform::open_url(&url) {
                        self.state.config_diagnostic =
                            Some(format!("could not open ticket: {error}"));
                    }
                }
            }
            TicketAction::CopyLink => {
                self.state.sidebar_object_menu = None;
                if let Some(url) = context.url {
                    self.state.request_clipboard_write = Some(url.into_bytes());
                }
            }
            TicketAction::CopyIdentifier => {
                self.state.sidebar_object_menu = None;
                self.state.request_clipboard_write = Some(ticket.identifier.into_bytes());
            }
            TicketAction::Cancel => self.stage_sidebar_ticket_write(
                crate::work_index::WorkItemWrite::TransitionTicket {
                    identifier: ticket.identifier,
                    state: "Canceled".into(),
                },
            ),
        }
    }

    pub(crate) fn handle_sidebar_settled_key(&mut self, key: KeyEvent) -> bool {
        if self.state.sidebar_settled_menu_target.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.state.sidebar_settled_menu_target = None;
                    self.state.sidebar_settled_menu_delete_armed = false;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.sidebar_settled_menu_delete_armed = false;
                    self.state.sidebar_settled_menu_selected =
                        self.state.sidebar_settled_menu_selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.sidebar_settled_menu_delete_armed = false;
                    self.state.sidebar_settled_menu_selected = self
                        .state
                        .sidebar_settled_menu_selected
                        .saturating_add(1)
                        .min(crate::ui::SETTLED_MENU_LABELS.len() - 1);
                }
                KeyCode::Enter => {
                    let index = self.state.sidebar_settled_menu_selected;
                    self.apply_sidebar_settled_menu_action(index);
                }
                _ => {}
            }
            return true;
        }
        let rows = crate::ui::compute_sidebar_row_areas(&self.state, self.state.view.sidebar_rect)
            .0
            .into_iter()
            .filter_map(|card| {
                let pane_id = card.settled_pane_id?;
                let ws = self.state.workspaces.get(card.ws_idx)?;
                Some(crate::app::state::PaneFocusTarget {
                    workspace_id: ws.id.clone(),
                    pane_id,
                })
            })
            .collect::<Vec<_>>();
        let Some(target) = self.state.sidebar_selected_settled.clone() else {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.sidebar_selected_settled =
                        settled_sidebar_neighbor(&rows, None, true)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.sidebar_selected_settled =
                        settled_sidebar_neighbor(&rows, None, false)
                }
                _ => return false,
            }
            return self.state.sidebar_selected_settled.is_some();
        };
        match key.code {
            KeyCode::Enter => {
                self.focus_settled_pane(target);
                true
            }
            KeyCode::Char('m') => {
                self.state.sidebar_settled_menu_target = Some(target);
                self.state.sidebar_settled_menu_selected = 0;
                self.state.sidebar_settled_menu_delete_armed = false;
                true
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                self.state.sidebar_selected_settled = settled_sidebar_neighbor(
                    &rows,
                    Some(&target),
                    matches!(key.code, KeyCode::Up | KeyCode::Char('k')),
                );
                true
            }
            KeyCode::Esc => {
                self.state.sidebar_selected_settled = None;
                true
            }
            _ => {
                self.state.sidebar_selected_settled = None;
                false
            }
        }
    }

    pub(crate) fn apply_sidebar_settled_menu_action(&mut self, index: usize) {
        let Some(action) = self.state.select_settled_menu_action(index) else {
            return;
        };
        match action {
            SettledMenuAction::Unsettle(target) => self.unsettle_sidebar_pane(
                crate::app::state::SidebarPaneLifecycleTarget::Local(target),
            ),
            SettledMenuAction::Resume(target) => self.resume_settled_pane(target),
            SettledMenuAction::FocusLive(target) => self.focus_settled_pane(target),
            SettledMenuAction::NewThread {
                directory,
                workspace,
            } => self
                .state
                .open_home_composer_in_directory(directory, workspace),
            SettledMenuAction::Delete(target) => {
                let Some(ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == target.workspace_id)
                else {
                    return;
                };
                if let Some(pane_id) = self.public_pane_id(ws_idx, target.pane_id) {
                    self.runtime_pane_close("tui.sidebar.settled.delete", pane_id);
                }
            }
        }
        self.flush_pane_settlement_events();
    }

    pub(crate) fn focus_settled_pane(&mut self, target: crate::app::state::PaneFocusTarget) {
        let now = std::time::Instant::now();
        let now_unix = crate::app::settled::unix_seconds(std::time::SystemTime::now());
        self.state.refresh_settled_panes_at(None, now, now_unix);
        self.flush_pane_settlement_events();
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        if let Some(terminal_id) = self.state.workspaces[ws_idx]
            .terminal_id(target.pane_id)
            .cloned()
        {
            let is_locked = self.state.workspaces[ws_idx]
                .pane_state(target.pane_id)
                .is_some_and(|pane| pane.settled_locked);
            if is_locked {
                if let Some(terminal) = self.state.terminals.get(&terminal_id) {
                    if let Some(plan) = terminal.pending_agent_resume_plan.as_ref() {
                        let transcript =
                            terminal
                                .persisted_agent_session
                                .as_ref()
                                .and_then(|session| {
                                    crate::app::settled_view::load_transcript(
                                        session,
                                        &terminal.cwd,
                                    )
                                });
                        let command = crate::app::agent_resume::shell_command_from_argv(&plan.argv)
                            .unwrap_or_default();
                        self.state.settled_view =
                            Some(crate::app::settled_view::SettledViewState {
                                pane_id: target.pane_id,
                                transcript,
                                command,
                                scroll: 0,
                                search: String::new(),
                                searching: false,
                                editing: false,
                            });
                    }
                }
            }
        }
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
    }

    pub(crate) fn resume_settled_pane(&mut self, target: crate::app::state::PaneFocusTarget) {
        if self
            .state
            .settled_view
            .as_ref()
            .is_some_and(|view| view.pane_id == target.pane_id)
        {
            self.state.settled_view = None;
        }
        let toast_target = crate::app::state::SidebarPaneLifecycleTarget::Local(target.clone());
        let now = std::time::Instant::now();
        let tab_panes = self
            .state
            .sidebar_sections_layout
            .then(|| {
                self.state
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == target.workspace_id)
                    .and_then(|workspace| {
                        workspace
                            .tabs
                            .iter()
                            .find(|tab| tab.panes.contains_key(&target.pane_id))
                            .map(|tab| tab.panes.keys().copied().collect::<Vec<_>>())
                    })
            })
            .flatten();
        let resumed = if let Some(pane_ids) = tab_panes {
            let mut changed = false;
            for pane_id in pane_ids {
                changed |= self.state.note_human_pane_activity_at(pane_id, now);
            }
            changed
        } else {
            self.state.note_human_pane_activity_at(target.pane_id, now)
        };
        self.focus_settled_pane(target);
        self.flush_pane_settlement_events();
        if resumed {
            self.show_sidebar_shelf_toast(&toast_target, "Active");
        }
    }

    pub(crate) fn settle_sidebar_pane(
        &mut self,
        target: crate::app::state::SidebarPaneLifecycleTarget,
    ) {
        match &target {
            crate::app::state::SidebarPaneLifecycleTarget::Local(pane_target) => {
                // Keep the pre-remote-control keyboard behavior: local `s`
                // always reaches pane.settle, whose API owns snoozed refusal.
                self.settle_local_pane_or_tab(pane_target.clone(), "tui.sidebar.settle");
            }
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) => {
                let Some((snoozed, _)) = self.sidebar_pane_lifecycle_state(&target) else {
                    self.remote_pane_lifecycle_refused(&target);
                    return;
                };
                if snoozed {
                    return;
                }
                if let Err(error) = self.remote_pane_settle(agent_ref.clone()) {
                    self.show_remote_pane_lifecycle_error(agent_ref, error);
                } else {
                    self.show_sidebar_shelf_toast(&target, "Settled");
                }
            }
        }
    }

    pub(crate) fn unsettle_sidebar_pane(
        &mut self,
        target: crate::app::state::SidebarPaneLifecycleTarget,
    ) {
        match &target {
            crate::app::state::SidebarPaneLifecycleTarget::Local(_) => {
                if let Some(pane_id) = self.sidebar_pane_lifecycle_public_id(&target) {
                    let response = self.runtime_pane_unsettle("tui.sidebar.unsettle", pane_id);
                    if !self.show_local_pane_lifecycle_error("unsettle", &response) {
                        self.show_sidebar_shelf_toast(&target, "Active");
                    }
                }
            }
            crate::app::state::SidebarPaneLifecycleTarget::Remote(agent_ref) => {
                if let Err(error) = self.remote_pane_unsettle(agent_ref.clone()) {
                    self.show_remote_pane_lifecycle_error(agent_ref, error);
                } else {
                    self.show_sidebar_shelf_toast(&target, "Active");
                }
            }
        }
        self.flush_pane_settlement_events();
    }

    pub(crate) fn settle_local_pane_or_tab(
        &mut self,
        target: crate::app::state::PaneFocusTarget,
        source: &'static str,
    ) {
        let toast_target = crate::app::state::SidebarPaneLifecycleTarget::Local(target.clone());
        let pane_ids = self
            .state
            .sidebar_sections_layout
            .then(|| {
                self.state
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == target.workspace_id)
                    .and_then(|workspace| {
                        workspace
                            .tabs
                            .iter()
                            .find(|tab| tab.panes.contains_key(&target.pane_id))
                            .map(|tab| tab.panes.keys().copied().collect::<Vec<_>>())
                    })
            })
            .flatten();
        if let Some(pane_ids) = pane_ids {
            let Some(ws_idx) = self
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == target.workspace_id)
            else {
                return;
            };
            let mut settled = false;
            for pane_id in pane_ids {
                let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
                    continue;
                };
                if self.state.pane_is_snoozed(ws_idx, pane_id) {
                    let response = self.runtime_pane_unsnooze(
                        "tui.pane.unsnooze-before-settle",
                        public_pane_id.clone(),
                    );
                    if self.show_local_pane_lifecycle_error("unsnooze", &response) {
                        continue;
                    }
                }
                let response = self.runtime_pane_settle(source, public_pane_id);
                settled |= !self.show_local_pane_lifecycle_error("settle", &response);
            }
            if settled {
                self.show_sidebar_shelf_toast(&toast_target, "Settled");
            }
            return;
        }

        let target = crate::app::state::SidebarPaneLifecycleTarget::Local(target);
        if let Some(public_pane_id) = self.sidebar_pane_lifecycle_public_id(&target) {
            let response = self.runtime_pane_settle(source, public_pane_id);
            if !self.show_local_pane_lifecycle_error("settle", &response) {
                self.show_sidebar_shelf_toast(&toast_target, "Settled");
            }
        }
    }

    pub(crate) fn resume_settled_pane_before_input(&mut self, pane_id: crate::layout::PaneId) {
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.pane_state(pane_id).is_some())
        else {
            return;
        };
        if !self.state.pane_is_settled(ws_idx, pane_id) {
            return;
        }
        self.state
            .note_human_pane_activity_at(pane_id, std::time::Instant::now());
        self.flush_pane_settlement_events();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::{Direction, Rect};

    use super::super::{app_for_mouse_test, capture_snapshot, mouse, unique_temp_path};
    use crate::{
        app::state::{AgentPanelSort, DragTarget, Mode, SidebarGroupMode},
        config::SidebarCollapsedModeConfig,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    #[tokio::test]
    async fn agent_finder_text_and_paste_stay_with_the_sidebar_input_owner() {
        let mut app = app_for_mouse_test();
        app.state.open_agent_finder();
        assert_eq!(
            app.state.input_owner(),
            crate::app::state::InputOwner::Sidebar
        );

        app.handle_text_commit_for_input_owner(
            crate::app::state::InputOwner::Sidebar,
            "needle".into(),
        )
        .await;
        app.handle_paste_for_input_owner(crate::app::state::InputOwner::Sidebar, " pasted".into())
            .await;

        assert_eq!(app.state.sidebar_work_filter.query, "needle pasted");
        assert!(app.state.agent_finder_deadline.is_some());
        assert!(
            app.state.workspaces.is_empty(),
            "finder input must not be sent to a PTY"
        );
        app.state
            .handle_sidebar_search_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(app.state.agent_finder_side_pane);
        app.state
            .handle_sidebar_search_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(!app.state.sidebar_search_active);
        assert!(app.state.agent_finder_saved_query.is_none());
        assert!(!app.state.agent_finder_side_pane);
        assert!(
            !app.state.sidebar_focused,
            "closing the finder restores the focus that was active before it opened"
        );
    }

    #[test]
    fn sidebar_areas_menu_accepts_nonmodal_surfaces_and_excludes_modal_owners() {
        use crate::app::state::{InputOwner, SurfaceInputOwner};

        for surface in [
            SurfaceInputOwner::Symphony,
            SurfaceInputOwner::LoopRunHistory,
            SurfaceInputOwner::AloopRunLog,
            SurfaceInputOwner::EditorPreview,
            SurfaceInputOwner::DockObjectPreview,
        ] {
            assert!(
                super::sidebar_areas_menu_accepts_input_owner(InputOwner::Surface(surface)),
                "nonmodal surface {surface:?} must allow the area checklist click"
            );
        }

        for owner in [
            InputOwner::Server(crate::app::state::ServerInputOwner::Navigate),
            InputOwner::Server(crate::app::state::ServerInputOwner::Prefix),
            InputOwner::Server(crate::app::state::ServerInputOwner::Copy),
            InputOwner::Server(crate::app::state::ServerInputOwner::Resize),
        ] {
            assert!(super::sidebar_areas_menu_accepts_input_owner(owner));
        }

        for owner in [
            InputOwner::Pomodoro,
            InputOwner::Client(crate::app::state::ClientInputOwner::DockSurfaceMenu),
            InputOwner::AddProject,
            InputOwner::Server(crate::app::state::ServerInputOwner::Settings),
            InputOwner::Popup,
        ] {
            assert!(
                !super::sidebar_areas_menu_accepts_input_owner(owner),
                "modal owner {owner:?} must keep the checklist click blocked"
            );
        }
    }

    #[test]
    fn fleet_workspace_ac6_cycle_dropdown_saves_radio_and_checkbox_choices() {
        use crate::app::state::InputOwner;

        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let config_path = unique_temp_path("fleet-cycle-config");
        fs::write(&config_path, "[ui]\n").expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &config_path);

        let mut app = app_for_mouse_test();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let anchor = app.state.view.window_cycle_mode_hit_area;
        assert!(anchor.width > 0);
        let owner = InputOwner::Pane;
        assert!(app.handle_window_cycle_menu_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), anchor.x, anchor.y),
            owner,
        ));
        let menu =
            crate::ui::sidebar::window_cycle_mode_menu_layout(&app.state, app.state.screen_rect())
                .expect("cycle settings dropdown");
        app.handle_window_cycle_menu_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                menu.list_rect.x,
                menu.list_rect.y + 1,
            ),
            owner,
        );
        assert_eq!(
            app.state.window_cycle_mode,
            crate::config::WindowCycleModeConfig::ThisMachineAndFleet
        );
        assert!(!app.state.window_cycle_menu_open);
        assert_eq!(
            crate::config::Config::load().config.ui.window_cycle_mode,
            crate::config::WindowCycleModeConfig::ThisMachineAndFleet
        );

        app.handle_window_cycle_menu_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), anchor.x, anchor.y),
            owner,
        );
        let menu =
            crate::ui::sidebar::window_cycle_mode_menu_layout(&app.state, app.state.screen_rect())
                .expect("reopened cycle settings dropdown");
        app.handle_window_cycle_menu_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                menu.list_rect.x,
                menu.list_rect.y + 2,
            ),
            owner,
        );
        assert!(app.state.skip_collapsed_cycle);
        assert!(app.state.window_cycle_menu_open);
        assert!(crate::config::Config::load().config.ui.skip_collapsed_cycle);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        fs::remove_file(config_path).ok();
    }

    fn sidebar_order_app(settled: bool) -> crate::app::App {
        let mut app = app_for_mouse_test();
        app.state.workspaces = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                let mut workspace = Workspace::test_new(name);
                workspace.test_add_tab(Some("two"));
                workspace.test_add_tab(Some("three"));
                workspace
            })
            .collect();
        app.state.ensure_test_terminals();
        for ws_idx in 0..app.state.workspaces.len() {
            for tab_idx in 0..app.state.workspaces[ws_idx].tabs.len() {
                let pane_id = app.state.workspaces[ws_idx].tabs[tab_idx].root_pane;
                if settled {
                    app.state.workspaces[ws_idx].tabs[tab_idx]
                        .panes
                        .get_mut(&pane_id)
                        .expect("root pane")
                        .settled_at = Some(1_725_000_000);
                }
                let terminal_id = app.state.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
                    .attached_terminal_id
                    .clone();
                app.state
                    .terminals
                    .get_mut(&terminal_id)
                    .expect("fixture terminal")
                    .replace_prevalidated_manual_work_context(
                        crate::work_context::PaneWorkContext {
                            repo: Some(format!("acme/{}", ["alpha", "beta"][ws_idx])),
                            branch: Some(format!("branch-{ws_idx}-{tab_idx}")),
                            pr_urls: vec![format!(
                                "https://github.com/acme/project/pull/{}",
                                ws_idx * 10 + tab_idx + 1
                            )],
                            ticket_ids: vec![format!("SCA-{}", 100 + ws_idx * 10 + tab_idx)],
                            missive_urls: vec![format!(
                                "https://mail.missiveapp.com/#inbox/conversations/{ws_idx}{tab_idx}"
                            )],
                            work_title: Some(format!("work-{ws_idx}-{tab_idx}")),
                            ..Default::default()
                        },
                    );
                if settled {
                    app.state
                        .terminals
                        .get_mut(&terminal_id)
                        .expect("fixture terminal")
                        .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                            source: "herdr:codex".into(),
                            agent: "codex".into(),
                            session_ref: crate::agent_resume::AgentSessionRef::id(format!(
                                "settled-{ws_idx}-{tab_idx}"
                            ))
                            .expect("valid session id"),
                        });
                }
            }
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        app.state.collapsed_sidebar_groups.clear();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        app
    }

    #[test]
    fn section_toggle_queues_explicit_expand_and_collapse_for_persistence() {
        let mut app = crate::app::state::AppState::test_new();
        assert!(app.collapsed_sidebar_groups.contains("repo:Runs"));

        app.toggle_sidebar_group(crate::ui::sidebar::RUNS_SECTION_TITLE);
        assert!(!app.collapsed_sidebar_groups.contains("repo:Runs"));
        assert_eq!(
            app.take_sidebar_group_collapsed_persistence_request(),
            Some(("repo:Runs".to_string(), false))
        );

        app.toggle_sidebar_group(crate::ui::sidebar::RUNS_SECTION_TITLE);
        assert!(app.collapsed_sidebar_groups.contains("repo:Runs"));
        assert_eq!(
            app.take_sidebar_group_collapsed_persistence_request(),
            Some(("repo:Runs".to_string(), true))
        );
    }

    fn pod_keyboard_app() -> (crate::app::App, crate::groups::GroupRecord) {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("pods")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let owner = crate::groups::AuthorityId::from_random_bytes([61; 16]);
        let record = crate::groups::GroupRecord {
            id: crate::groups::GroupId {
                owner: owner.clone(),
                local: 1,
            },
            revision: 3,
            state: crate::groups::GroupState::Active {
                name: "release".into(),
            },
        };
        app.state.local_group_snapshot = Some(crate::groups::GroupAuthoritySnapshot {
            authority_id: owner,
            revision: 3,
            groups: vec![record.clone()],
            memberships: Vec::new(),
        });
        app.state.sidebar_pod_picker = Some(crate::app::state::SidebarPodPickerState {
            ws_idx: 0,
            pane_id,
            anchor: (1, 1),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });
        (app, record)
    }

    #[test]
    fn pod_picker_keeps_stale_rows_visible_but_disabled() {
        let (mut app, _) = pod_keyboard_app();
        let remote = crate::groups::AuthorityId::from_random_bytes([62; 16]);
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "ub2".into(),
            target: "ub2".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Stale,
            observed_authority_id: Some(remote.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: remote.clone(),
                revision: 1,
                groups: vec![crate::groups::GroupRecord {
                    id: crate::groups::GroupId {
                        owner: remote,
                        local: 9,
                    },
                    revision: 1,
                    state: crate::groups::GroupState::Active {
                        name: "offline".into(),
                    },
                }],
                memberships: Vec::new(),
            }),
            error: Some("offline".into()),
        }];
        let choices = crate::ui::sidebar::sidebar_pod_picker_choices(&app.state);
        assert!(matches!(
            choices.as_slice(),
            [
                crate::ui::sidebar::SidebarPodChoice::Existing { fresh: true, .. },
                crate::ui::sidebar::SidebarPodChoice::Existing { fresh: false, .. }
            ]
        ));

        assert!(
            app.handle_sidebar_pod_picker_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty(),))
        );
        assert_eq!(
            app.state
                .sidebar_pod_picker
                .as_ref()
                .expect("picker remains open")
                .filter
                .selected,
            0,
            "keyboard selection skips the disabled stale row"
        );
        app.accept_sidebar_pod_picker(1);
        assert!(app.state.sidebar_pod_picker.is_some());
        assert!(
            app.state.workspaces[0].tabs[0].panes[&app.state.workspaces[0].tabs[0].root_pane]
                .group_membership
                .group_id
                .is_none()
        );
    }

    #[test]
    fn pod_picker_create_chains_group_create_and_pane_assignment() {
        let mut app = app_for_mouse_test();
        let dir = std::env::temp_dir().join(format!(
            "herdr-pod-picker-create-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        app.group_runtime = crate::groups::Runtime::load(&dir.join("groups"));
        app.group_session_paths_override =
            Some((dir.join("session.json"), dir.join("history.json")));
        app.no_session = false;
        app.state.workspaces = vec![Workspace::test_new("pods")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app.state.refresh_local_agent_panel_identities();
        app.rebuild_group_membership_projection();

        assert!(app.open_sidebar_pod_picker_for_focused_pane());
        for character in "new-pod".chars() {
            assert!(app.handle_sidebar_pod_picker_key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            )));
        }
        let choices = crate::ui::sidebar::sidebar_pod_picker_choices(&app.state);
        let create = choices
            .iter()
            .position(|choice| matches!(choice, crate::ui::sidebar::SidebarPodChoice::Create(name) if name == "new-pod"))
            .expect("create choice");
        app.accept_sidebar_pod_picker(create);

        let snapshot = app
            .state
            .local_group_snapshot
            .as_ref()
            .expect("local snapshot after create");
        let created_id = snapshot
            .groups
            .iter()
            .find_map(|record| match &record.state {
                crate::groups::GroupState::Active { name } if name == "new-pod" => {
                    Some(record.id.clone())
                }
                _ => None,
            })
            .expect("created pod");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&pane_id]
                .group_membership
                .group_id,
            Some(created_id.clone())
        );
        assert!(app.open_sidebar_pod_picker_for_focused_pane());
        let choices = crate::ui::sidebar::sidebar_pod_picker_choices(&app.state);
        let no_pod = choices
            .iter()
            .position(|choice| matches!(choice, crate::ui::sidebar::SidebarPodChoice::NoPod))
            .expect("no-pod choice");
        app.accept_sidebar_pod_picker(no_pod);
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&pane_id]
                .group_membership
                .group_id,
            None
        );
        assert!(app.open_sidebar_pod_picker_for_focused_pane());
        let choices = crate::ui::sidebar::sidebar_pod_picker_choices(&app.state);
        let existing = choices
            .iter()
            .position(|choice| matches!(choice, crate::ui::sidebar::SidebarPodChoice::Existing { id, fresh: true, .. } if id == &created_id))
            .expect("existing pod choice");
        app.accept_sidebar_pod_picker(existing);
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&pane_id]
                .group_membership
                .group_id,
            Some(created_id)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pod_header_keyboard_actions_toggle_rename_and_delete() {
        let (mut app, record) = pod_keyboard_app();
        let key = format!("pod:{}:{}", record.id.owner, record.id.local);
        app.state.sidebar_selected_work_group = Some(key.clone());
        assert!(matches!(
            app.state.handle_sidebar_work_group_key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::empty(),
            )),
            super::SidebarWorkGroupKeyAction::Consumed
        ));
        assert!(app.state.collapsed_sidebar_groups.contains(&format!(
            "{}:{key}",
            app.state.sidebar_group_mode.collapse_namespace()
        )));

        assert!(matches!(
            app.state.handle_sidebar_work_group_key(KeyEvent::new(
                KeyCode::Char('r'),
                KeyModifiers::empty(),
            )),
            super::SidebarWorkGroupKeyAction::RenamePod(candidate) if candidate == record
        ));
        assert!(matches!(
            app.state.handle_sidebar_work_group_key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::empty(),
            )),
            super::SidebarWorkGroupKeyAction::DeletePod(candidate) if candidate == record
        ));
    }

    fn sidebar_order_signature(app: &crate::app::state::AppState) -> Vec<String> {
        crate::ui::sidebar_rows(app)
            .into_iter()
            .map(|row| match row {
                crate::ui::SidebarRow::Workspace {
                    ws_idx, indented, ..
                } => {
                    format!("workspace:{ws_idx}:{indented}")
                }
                crate::ui::SidebarRow::Tab { entry, .. } => {
                    let target = entry.local_target().unwrap();
                    format!("tab:{}:{}", target.ws_idx, target.tab_idx)
                }
                crate::ui::SidebarRow::Agent { entry, .. } => {
                    let target = entry.local_target().unwrap();
                    format!(
                        "pane:{}:{}:{}",
                        target.ws_idx,
                        target.tab_idx,
                        target.pane_id.raw()
                    )
                }
                crate::ui::SidebarRow::RemoteAgent { entry, .. } => {
                    format!("remote:{}", entry.agent_ref)
                }
                crate::ui::SidebarRow::PodHeader { group_id, .. } => {
                    format!("pod:{}:{}", group_id.owner, group_id.local)
                }
                crate::ui::SidebarRow::PodMember { title, .. } => {
                    format!("pod-member:{title}")
                }
                crate::ui::SidebarRow::SectionHeader { title, .. } => {
                    format!("section:{title}")
                }
                crate::ui::SidebarRow::Divider | crate::ui::SidebarRow::ShelfDivider => {
                    "divider".to_string()
                }
                crate::ui::SidebarRow::NeedsYou { title, .. } => format!("needs-you:{title}"),
                crate::ui::SidebarRow::NeedsYouMore { .. } => "needs-you-more".to_string(),
                crate::ui::SidebarRow::NestedHeader { key, .. } => format!("group:{key}"),
                crate::ui::SidebarRow::SymphonyJob { name, .. } => format!("symphony:{name}"),
                crate::ui::SidebarRow::SymphonyEmpty => "symphony:empty".to_string(),
                crate::ui::SidebarRow::AgentRun { host, summary } => summary.map_or_else(
                    || format!("run:{host}:empty"),
                    |summary| format!("run:{host}:{}", summary.run_id),
                ),
                crate::ui::SidebarRow::AloopLoop { name, .. } => format!("aloop:{name}"),
                crate::ui::SidebarRow::AloopRemoteLoop { key, .. } => {
                    format!("aloop:remote:{key}")
                }
                crate::ui::SidebarRow::AloopRunLine { key, .. }
                | crate::ui::SidebarRow::AloopFinding { key, .. }
                | crate::ui::SidebarRow::AloopCleanRuns { key, .. }
                | crate::ui::SidebarRow::AloopCleanRun { key, .. } => format!("aloop:row:{key}"),
                crate::ui::SidebarRow::AloopUnreachable { .. } => "aloop:unreachable".to_string(),
                crate::ui::SidebarRow::AloopEmpty => "aloop:empty".to_string(),
                crate::ui::SidebarRow::Inbox(_) => "inbox".to_string(),
                crate::ui::SidebarRow::Notes(_) => "notes".to_string(),
            })
            .collect()
    }

    fn assert_sidebar_click_preserves_order(mode: SidebarGroupMode, settled: bool) {
        let mut app = sidebar_order_app(settled);
        app.state.set_sidebar_group_mode(mode);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let before = sidebar_order_signature(&app.state);
        let before_storage = app
            .state
            .workspaces
            .iter()
            .map(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .map(|tab| tab.root_pane)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .nth(2)
            .expect("third sidebar tab row");
        let target_rect = target.rect;
        let target_ws_idx = target.ws_idx;
        let target_tab_idx = target.tab_idx;
        let target_pane_id = target.pane_id;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target_rect.x + 1,
            target_rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target_rect.x + 1,
            target_rect.y,
        ));

        if settled {
            assert_eq!(app.state.active, Some(target_ws_idx));
            assert_eq!(
                app.state.workspaces[target_ws_idx].focused_pane_id(),
                Some(target_pane_id)
            );
            assert!(app.state.pane_is_settled(target_ws_idx, target_pane_id));
            assert!(app.state.sidebar_selected_settled.is_none());
        } else {
            assert_eq!(app.state.active, Some(target_ws_idx));
            assert_eq!(
                app.state.workspaces[target_ws_idx].active_tab,
                target_tab_idx
            );
        }
        if !settled {
            assert_eq!(
                sidebar_order_signature(&app.state),
                before,
                "{mode:?}, settled={settled}"
            );
        }
        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| {
                    workspace
                        .tabs
                        .iter()
                        .map(|tab| tab.root_pane)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
            before_storage,
            "{mode:?}, settled={settled}"
        );
    }

    #[test]
    fn sidebar_click_preserves_order_in_every_group_mode_and_settled() {
        for mode in SidebarGroupMode::ALL {
            assert_sidebar_click_preserves_order(mode, false);
            assert_sidebar_click_preserves_order(mode, true);
        }
    }

    #[test]
    fn clicking_resumable_settled_row_focuses_without_unsettling_its_pane() {
        for mode in SidebarGroupMode::ALL {
            let mut app = sidebar_order_app(true);
            app.state.set_sidebar_group_mode(mode);
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
                .into_iter()
                .nth(2)
                .expect("third settled sidebar tab row");

            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                target.rect.x + 1,
                target.rect.y,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                target.rect.x + 1,
                target.rect.y,
            ));

            assert_eq!(app.state.active, Some(target.ws_idx), "{mode:?}");
            assert_eq!(
                app.state.workspaces[target.ws_idx].focused_pane_id(),
                Some(target.pane_id),
                "{mode:?}"
            );
            assert!(
                app.state.pane_is_settled(target.ws_idx, target.pane_id),
                "{mode:?}"
            );
            assert!(app.state.sidebar_selected_settled.is_none(), "{mode:?}");
        }
    }

    #[test]
    fn clicking_sidebar_settle_icon_settles_the_exact_pane() {
        let mut app = sidebar_order_app(false);
        // Nested rows need room for both lifecycle controls. This tab belongs
        // to the inactive workspace, so its controls must appear on hover.
        app.state.sidebar_width = 40;
        let area = Rect::new(0, 0, 120, 40);
        crate::ui::compute_view(&mut app.state, area);
        let row = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .find(|row| row.ws_idx == 1 && row.tab_idx == 1)
            .expect("inactive workspace tab");
        app.handle_mouse(mouse(
            MouseEventKind::Moved,
            row.rect.right() - 1,
            row.rect.y,
        ));
        crate::ui::compute_view(&mut app.state, area);
        let target = app
            .state
            .view
            .sidebar_hover_targets
            .iter()
            .find(|target| {
                target.rect.y == row.rect.y
                    && matches!(
                        target.action.as_ref(),
                        Some(crate::app::state::SidebarHoverAction::Settle {
                            target: crate::app::state::SidebarPaneLifecycleTarget::Local(pane),
                        }) if pane.pane_id == row.pane_id
                    )
            })
            .cloned()
            .expect("settle icon target for the inactive tab");
        let crate::app::state::SidebarHoverAction::Settle {
            target: lifecycle_target,
        } = target.action.expect("settle action")
        else {
            panic!("settle target carried a different action");
        };
        let crate::app::state::SidebarPaneLifecycleTarget::Local(target_pane) = lifecycle_target
        else {
            panic!("local row carried a remote target");
        };
        let ws_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target_pane.workspace_id)
            .expect("target workspace");
        let pane_id = target_pane.pane_id;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x,
            target.rect.y,
        ));

        assert_eq!(
            app.state.active,
            Some(0),
            "the other workspace stays active"
        );
        assert!(app.state.pane_is_settled(ws_idx, pane_id));
    }

    #[test]
    fn sidebar_settle_tab_uses_pane_settle_for_every_tab_pane() {
        let mut app = sidebar_order_app(false);
        let workspace = &mut app.state.workspaces[0];
        let root_pane = workspace.tabs[0].root_pane;
        let sibling_pane = workspace.test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.sidebar_sections_layout = true;
        let workspace_id = app.state.workspaces[0].id.clone();

        app.settle_sidebar_pane(crate::app::state::SidebarPaneLifecycleTarget::Local(
            crate::app::state::PaneFocusTarget {
                workspace_id,
                pane_id: root_pane,
            },
        ));

        assert!(app.state.pane_is_settled(0, root_pane));
        assert!(app.state.pane_is_settled(0, sibling_pane));
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("Moved to Settled")
        );
        let deadline = app.toast_deadline.expect("settle toast deadline");
        assert!(app.handle_scheduled_tasks(deadline, false));
        assert!(app.state.toast.is_none(), "settle toast expires");
    }

    #[test]
    fn sections_settle_tab_unsnoozes_and_settles_snoozed_sibling() {
        let mut sidebar = sidebar_order_app(false);
        sidebar.state.active = Some(0);
        let (root_pane, sibling_pane) = {
            let workspace = &mut sidebar.state.workspaces[0];
            let root_pane = workspace.tabs[0].root_pane;
            let sibling_pane = workspace.test_split(Direction::Horizontal);
            workspace.tabs[0].layout.focus_pane(root_pane);
            (root_pane, sibling_pane)
        };
        sidebar.state.ensure_test_terminals();
        sidebar.state.sidebar_sections_layout = true;
        assert!(sidebar.state.snooze_pane_at(0, sibling_pane, u64::MAX));
        sidebar.state.sidebar_focused = true;

        assert!(sidebar.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));

        for pane_id in [root_pane, sibling_pane] {
            assert!(sidebar.state.pane_is_settled(0, pane_id));
            assert!(!sidebar.state.pane_is_snoozed(0, pane_id));
        }

        let mut context_menu = sidebar_order_app(false);
        let (workspace_id, tab_id, root_pane, sibling_pane) = {
            let workspace = &mut context_menu.state.workspaces[0];
            let root_pane = workspace.tabs[0].root_pane;
            let sibling_pane = workspace.test_split(Direction::Horizontal);
            workspace.tabs[0].layout.focus_pane(root_pane);
            (
                workspace.id.clone(),
                crate::workspace::public_tab_id_for_number(&workspace.id, workspace.tabs[0].number),
                root_pane,
                sibling_pane,
            )
        };
        context_menu.state.ensure_test_terminals();
        context_menu.state.sidebar_sections_layout = true;
        assert!(context_menu.state.snooze_pane_at(0, sibling_pane, u64::MAX));
        let menu = crate::app::state::ContextMenuState {
            kind: crate::app::state::ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: Some(root_pane),
                snooze_target: Some(root_pane),
            },
            x: 0,
            y: 0,
            selected: crate::app::state::ContextMenuAction::Settle,
        };

        context_menu
            .apply_context_menu_action_via_api(menu, crate::app::state::ContextMenuAction::Settle);

        for pane_id in [root_pane, sibling_pane] {
            assert!(context_menu.state.pane_is_settled(0, pane_id));
            assert!(!context_menu.state.pane_is_snoozed(0, pane_id));
        }
    }

    #[test]
    fn sidebar_areas_menu_opens_and_toggles_from_surface_and_dock_owners() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let config_path = unique_temp_path("sidebar-areas-owners");
        let config = "[ui.sidebar]\nlayout = \"sections\"\nheader = \"sky\"\n[ui.sidebar.areas]\nsky_header = true\nview_bar = true\n";
        fs::write(&config_path, config).expect("seed sidebar config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &config_path);

        let owners = [
            crate::app::state::InputOwner::Surface(crate::app::state::SurfaceInputOwner::Home),
            crate::app::state::InputOwner::Surface(crate::app::state::SurfaceInputOwner::Inbox),
            crate::app::state::InputOwner::Surface(crate::app::state::SurfaceInputOwner::Work),
            crate::app::state::InputOwner::Surface(crate::app::state::SurfaceInputOwner::Usage),
            crate::app::state::InputOwner::Dock(crate::app::state::DockInputOwner::Home),
        ];

        for owner in owners {
            fs::write(&config_path, config).expect("reset sidebar config");
            let mut app = app_for_mouse_test();
            app.state.sidebar_sections_layout = true;
            app.state.sidebar_width = 40;
            app.state.sidebar_areas.sky_header = true;
            app.state.sidebar_areas.view_bar = true;
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let anchor = app.state.view.sidebar_areas_hit_area;
            assert!(anchor.width > 0, "checklist control must be visible");

            app.handle_mouse_for_input_owner(
                1,
                mouse(MouseEventKind::Down(MouseButton::Left), anchor.x, anchor.y),
                crate::ui::pomodoro::InputPresentation::default(),
                owner,
            );
            assert_eq!(app.state.sidebar_areas_menu_selected, Some(0));
            assert!(app.handle_sidebar_areas_menu_key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
                owner,
            ));
            assert_eq!(app.state.sidebar_areas_menu_selected, Some(1));
            assert!(app.handle_sidebar_areas_menu_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
                owner,
            ));
            assert!(!app.state.sidebar_areas.view_bar);

            fs::write(&config_path, config).expect("reset sidebar config for mouse toggle");
            let mut app = app_for_mouse_test();
            app.state.sidebar_sections_layout = true;
            app.state.sidebar_width = 40;
            app.state.sidebar_areas.sky_header = true;
            app.state.sidebar_areas.view_bar = true;
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let anchor = app.state.view.sidebar_areas_hit_area;
            app.handle_mouse_for_input_owner(
                1,
                mouse(MouseEventKind::Down(MouseButton::Left), anchor.x, anchor.y),
                crate::ui::pomodoro::InputPresentation::default(),
                owner,
            );
            let menu =
                crate::ui::sidebar::sidebar_areas_menu_layout(&app.state, Rect::new(0, 0, 120, 40))
                    .expect("checklist menu layout");
            app.handle_mouse_for_input_owner(
                1,
                mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    menu.rect.x.saturating_add(1),
                    menu.rect.y,
                ),
                crate::ui::pomodoro::InputPresentation::default(),
                owner,
            );
            assert!(!app.state.sidebar_areas.sky_header);
        }

        let mut app = app_for_mouse_test();
        app.state.sidebar_sections_layout = true;
        app.state.view.sidebar_areas_hit_area = Rect::new(10, 19, 1, 1);
        let modal = crate::app::state::InputOwner::Pomodoro;
        assert!(!app.handle_sidebar_areas_menu_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 10, 19),
            modal,
        ));
        assert!(!app.handle_sidebar_areas_menu_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            modal,
        ));
        assert_eq!(app.state.sidebar_areas_menu_selected, None);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = fs::remove_file(config_path);
    }

    fn assert_hidden_areas_menu_releases_pane(layout_off: bool) {
        let mut app = app_for_mouse_test();
        let mut workspace = Workspace::test_new("areas-menu-pane");
        let pane_id = workspace.tabs[0].root_pane;
        let (runtime, mut pane_input) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 4,
            );
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.sidebar_sections_layout = true;
        app.state.sidebar_width = 40;
        app.state.sidebar_areas.sky_header = true;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let anchor = app.state.view.sidebar_areas_hit_area;
        assert!(anchor.width > 0);
        assert!(app.handle_sidebar_areas_menu_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), anchor.x, anchor.y),
            crate::app::state::InputOwner::Pane,
        ));
        assert_eq!(app.state.sidebar_areas_menu_selected, Some(0));

        let area = if layout_off {
            app.state.sidebar_sections_layout = false;
            Rect::new(0, 0, 120, 40)
        } else {
            Rect::new(0, 0, 40, 40)
        };
        crate::ui::compute_view(&mut app.state, area);
        assert_eq!(app.state.view.sidebar_areas_hit_area.width, 0);
        assert_eq!(app.state.sidebar_areas_menu_selected, None);

        // A stale selection can arrive before the next view pass. Input must
        // still go to the pane and must not toggle the hidden area.
        app.state.sidebar_areas_menu_selected = Some(0);
        assert_eq!(app.state.input_owner(), crate::app::state::InputOwner::Pane);
        let target = app.handle_terminal_key_headless(crate::input::TerminalKey::new(
            KeyCode::Char(' '),
            KeyModifiers::empty(),
        ));
        assert!(target.is_some(), "space must reach the focused pane");
        assert_eq!(pane_input.try_recv().expect("pane input").as_ref(), b" ");
        assert!(app.state.sidebar_areas.sky_header);
        assert_eq!(app.state.sidebar_areas_menu_selected, None);
    }

    #[tokio::test]
    async fn area_menu_closes_when_sidebar_becomes_too_narrow() {
        assert_hidden_areas_menu_releases_pane(false);
    }

    #[tokio::test]
    async fn area_menu_closes_when_sections_layout_is_disabled() {
        assert_hidden_areas_menu_releases_pane(true);
    }

    #[test]
    fn clicking_sidebar_snooze_opens_durations_and_dispatches_the_api() {
        let mut app = sidebar_order_app(false);
        app.state.sidebar_width = 40;
        let area = Rect::new(0, 0, 120, 40);
        crate::ui::compute_view(&mut app.state, area);
        let row = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .find(|row| row.ws_idx == 1 && row.tab_idx == 2)
            .expect("second inactive workspace tab");
        app.handle_mouse(mouse(
            MouseEventKind::Moved,
            row.rect.right() - 1,
            row.rect.y,
        ));
        crate::ui::compute_view(&mut app.state, area);
        let target = app
            .state
            .view
            .sidebar_hover_targets
            .iter()
            .find(|target| {
                target.rect.y == row.rect.y
                    && matches!(
                        target.action.as_ref(),
                        Some(crate::app::state::SidebarHoverAction::Snooze {
                            target: crate::app::state::SidebarPaneLifecycleTarget::Local(pane),
                        }) if pane.pane_id == row.pane_id
                    )
            })
            .cloned()
            .expect("snooze control target for the inactive tab");
        let crate::app::state::SidebarHoverAction::Snooze {
            target: lifecycle_target,
        } = target.action.expect("snooze action")
        else {
            panic!("snooze target carried a different action");
        };
        let crate::app::state::SidebarPaneLifecycleTarget::Local(target_pane) = lifecycle_target
        else {
            panic!("local row carried a remote target");
        };
        let ws_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target_pane.workspace_id)
            .expect("target workspace");
        let pane_id = target_pane.pane_id;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x,
            target.rect.y,
        ));
        assert!(app.state.sidebar_snooze.is_some());
        let before = crate::app::settled::unix_seconds(std::time::SystemTime::now());
        app.handle_sidebar_snooze_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        let deadline = app.state.workspaces[ws_idx]
            .pane_state(pane_id)
            .and_then(crate::pane::PaneState::snoozed_until)
            .expect("pane snoozed through runtime API");
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("Moved to Snoozed")
        );
        assert!((before + 15 * 60..=before + 15 * 60 + 1).contains(&deadline));
        assert!(crate::ui::sidebar_rows(&app.state)
            .iter()
            .any(|row| matches!(
                row,
                crate::ui::SidebarRow::SectionHeader {
                    title: crate::ui::sidebar::SNOOZED_SECTION_TITLE,
                    ..
                }
            )));
    }

    #[test]
    fn snoozed_section_timer_dropdown_unsnoozes_the_exact_pane() {
        let mut app = sidebar_order_app(false);
        app.state.sidebar_width = 40;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 900;
        assert!(app.state.snooze_pane_at(0, pane_id, deadline));
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let target = app
            .state
            .view
            .sidebar_hover_targets
            .iter()
            .find(|target| {
                target.label.starts_with("Unsnoozes")
                    && matches!(
                        target.action.as_ref(),
                        Some(crate::app::state::SidebarHoverAction::Snooze {
                            target: crate::app::state::SidebarPaneLifecycleTarget::Local(target_pane),
                        }) if target_pane.pane_id == pane_id
                    )
            })
            .cloned()
            .expect("timer control in Snoozed section");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x,
            target.rect.y,
        ));
        assert!(app.state.sidebar_snooze.as_ref().is_some_and(|snooze| {
            snooze.time_draft.is_none()
                && matches!(
                    &snooze.target,
                    crate::app::state::SidebarPaneLifecycleTarget::Local(target)
                        if app.state.pane_is_snoozed(0, target.pane_id)
                )
        }));
        app.handle_sidebar_snooze_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.state.pane_is_snoozed(0, pane_id));
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("Moved to Active")
        );
    }

    #[test]
    fn snooze_presets_map_to_bounded_api_requests() {
        let expected = [
            ("Snooze for 15 minutes", Some(15 * 60), None),
            ("Snooze for 1 hour", Some(60 * 60), None),
            ("Snooze for 4 hours", Some(4 * 60 * 60), None),
            ("Snooze until tomorrow at 09:00", None, Some(1_725_033_600)),
        ];

        for ((label, action), (expected_label, duration_s, snoozed_until)) in
            crate::app::state::SNOOZE_MENU_ITEMS
                .into_iter()
                .take(4)
                .zip(expected)
        {
            let crate::app::state::SidebarSnoozeMenuAction::Preset(preset) = action else {
                panic!("duration row must dispatch a preset");
            };
            let params = super::sidebar_snooze_params(
                "workspace:pane".to_string(),
                preset,
                Some(1_725_033_600),
            )
            .expect("bounded snooze request");
            assert_eq!(label, expected_label);
            assert_eq!(params.pane_id, "workspace:pane");
            assert_eq!(params.duration_s, duration_s);
            assert_eq!(params.snoozed_until, snoozed_until);
        }
    }

    #[test]
    fn keyboard_s_settles_a_local_pane_that_needs_attention() {
        let mut app = sidebar_order_app(false);
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("focused pane state")
            .attached_terminal_id
            .clone();
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("focused pane terminal");
        terminal.set_raw_agent_state_for_test(AgentState::Blocked);
        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "Answer".into(),
            text: "Choose one".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        app.state.sidebar_focused = true;
        assert!(!app.state.pane_can_snooze(0, pane_id));

        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));

        assert!(app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn keyboard_s_keeps_the_local_pane_api_refusal_when_already_snoozed() {
        let mut app = sidebar_order_app(false);
        assert!(!app.state.sidebar_sections_layout);
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        assert!(app.state.snooze_pane_at(0, pane_id, u64::MAX));
        app.state.sidebar_focused = true;

        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));

        assert!(app.state.pane_is_snoozed(0, pane_id));
        assert!(!app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn keyboard_z_anchors_a_local_snooze_menu_to_the_focused_card() {
        let mut app = sidebar_order_app(false);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let card = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .find(|card| card.ws_idx == 0 && card.pane_id == pane_id)
            .expect("focused pane card");
        let expected_anchor = (card.rect.right().saturating_sub(4), card.rect.y);
        app.state.sidebar_focused = true;

        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::empty(),
        )));

        assert_eq!(
            app.state
                .sidebar_snooze
                .as_ref()
                .expect("local snooze menu")
                .anchor,
            expected_anchor
        );
    }

    #[tokio::test]
    async fn sidebar_keyboard_reaches_snooze_and_settle_controls_through_input_routing() {
        let mut snooze = sidebar_order_app(false);
        crate::ui::compute_view(&mut snooze.state, Rect::new(0, 0, 120, 40));
        snooze.state.sidebar_focused = true;
        let _ = snooze
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('z'),
                KeyModifiers::empty(),
            ))
            .await;
        assert!(snooze.state.sidebar_snooze.is_some());
        let _ = snooze
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Enter,
                KeyModifiers::empty(),
            ))
            .await;
        let snoozed_pane = snooze.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        assert!(snooze.state.pane_is_snoozed(0, snoozed_pane));

        let mut settle = sidebar_order_app(false);
        settle.state.sidebar_focused = true;
        let _ = settle
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('s'),
                KeyModifiers::empty(),
            ))
            .await;
        let settled_pane = settle.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        assert!(settle.state.pane_is_settled(0, settled_pane));
    }

    #[tokio::test]
    async fn sidebar_focus_and_snooze_editor_are_isolated_between_clients() {
        let mut app = sidebar_order_app(false);
        let editor_pane = app.state.workspaces[0].tabs[0].root_pane;
        let client_pane = app.state.workspaces[0].tabs[1].root_pane;
        let mut client_a = crate::app::state::SidebarPresentationState::default();
        let mut client_b = crate::app::state::SidebarPresentationState::default();

        app.state.swap_sidebar_presentation(&mut client_a);
        app.state.focus_client_on_sidebar();
        let target = app
            .local_sidebar_pane_lifecycle_target(0, editor_pane)
            .expect("local lifecycle target");
        app.open_snooze_time_input(target);
        if let Some(snooze) = app.state.sidebar_snooze.as_mut() {
            snooze.time_draft = Some("14:30".into());
            snooze.error = Some("client A only".into());
        }
        app.state.swap_sidebar_presentation(&mut client_a);

        app.state.swap_sidebar_presentation(&mut client_b);
        assert!(!app.state.sidebar_focused);
        assert!(app.state.sidebar_snooze.is_none());
        app.state.sidebar_focused = true;
        app.state.workspaces[0].active_tab = 1;
        for key in ['z', '\n', 'z', '\n', 's'] {
            let code = if key == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(key)
            };
            let _ = app
                .handle_key_inner(crate::input::TerminalKey::new(code, KeyModifiers::empty()))
                .await;
        }
        assert!(app.state.pane_is_settled(0, client_pane));
        assert!(!app.state.pane_is_settled(0, editor_pane));
        assert!(!app.state.pane_is_snoozed(0, editor_pane));
        app.state.swap_sidebar_presentation(&mut client_b);

        app.state.swap_sidebar_presentation(&mut client_a);
        assert!(app.state.sidebar_focused);
        let editor = app.state.sidebar_snooze.as_ref().expect("client A editor");
        assert_eq!(
            editor.target.local().map(|target| target.pane_id),
            Some(editor_pane)
        );
        assert_eq!(editor.time_draft.as_deref(), Some("14:30"));
        assert_eq!(editor.error.as_deref(), Some("client A only"));
    }

    #[test]
    fn c7_unfresh_remote_selection_keeps_snooze_and_settle_shortcuts_inert() {
        let mut app = sidebar_order_app(false);
        let local_pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.sidebar_focused = true;
        app.state.sidebar_selected_remote_agent = Some(
            crate::api::schema::AgentRef::new("offline", "remote-pane")
                .expect("valid remote reference"),
        );

        for key in ['z', 's'] {
            assert!(!app.handle_sidebar_session_action_key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::empty(),
            )));
        }
        assert!(app.state.sidebar_snooze.is_none());
        assert!(!app.state.pane_is_settled(0, local_pane));
    }

    #[test]
    fn c7_f1_fresh_remote_shortcuts_resolve_remote_target_and_revalidate_before_enqueue() {
        let mut app = sidebar_order_app(false);
        let (remote_state, entry) = crate::ui::sidebar::tests::remote_control_fixture(
            crate::fleet::HostState::Reachable,
            crate::api::schema::AgentStatus::Working,
            false,
            false,
            false,
        );
        app.state.fleet_snapshot = remote_state.fleet_snapshot;
        app.state.remote_agent_panel_entries = remote_state.remote_agent_panel_entries;
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        app.state.sidebar_focused = true;
        app.state.sidebar_selected_remote_agent = Some(entry.agent_ref.clone());
        let before = app.state.remote_agent_panel_entries[0].clone();

        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::empty(),
        )));
        assert!(matches!(
            app.state.sidebar_snooze.as_ref().map(|menu| &menu.target),
            Some(crate::app::state::SidebarPaneLifecycleTarget::Remote(target))
                if target == &entry.agent_ref
        ));
        app.state.sidebar_snooze = None;
        app.state.fleet_snapshot.hosts[0].state = crate::fleet::HostState::Unreachable;
        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));
        assert!(app
            .state
            .toast
            .as_ref()
            .is_some_and(|toast| toast.context.contains("owner remote is unreachable")));
        assert!(!app.authority_mutation_router.worker_started_for_test());
        let after = &app.state.remote_agent_panel_entries[0];
        assert_eq!(after.settled, before.settled);
        assert_eq!(after.snoozed_until, before.snoozed_until);
    }

    #[test]
    fn remote_lifecycle_refusal_names_settled_and_attention_states() {
        for (status, gated, settled, expected) in [
            (
                crate::api::schema::AgentStatus::Done,
                false,
                true,
                "already settled",
            ),
            (
                crate::api::schema::AgentStatus::Blocked,
                true,
                false,
                "needs attention",
            ),
        ] {
            let mut app = sidebar_order_app(false);
            let (remote_state, entry) = crate::ui::sidebar::tests::remote_control_fixture(
                crate::fleet::HostState::Reachable,
                status,
                gated,
                settled,
                false,
            );
            app.state.remote_agent_panel_entries = remote_state.remote_agent_panel_entries;
            let target =
                crate::app::state::SidebarPaneLifecycleTarget::Remote(entry.agent_ref.clone());

            app.remote_pane_lifecycle_refused(&target);

            assert!(app
                .state
                .toast
                .as_ref()
                .is_some_and(|toast| toast.context.contains(expected)));
        }
    }

    #[test]
    fn n3_remote_snooze_menu_draft_and_close_leave_cached_lifecycle_unchanged() {
        let mut app = sidebar_order_app(false);
        let (remote_state, entry) = crate::ui::sidebar::tests::remote_control_fixture(
            crate::fleet::HostState::Reachable,
            crate::api::schema::AgentStatus::Working,
            false,
            false,
            false,
        );
        app.state.remote_agent_panel_entries = remote_state.remote_agent_panel_entries;
        let target = crate::app::state::SidebarPaneLifecycleTarget::Remote(entry.agent_ref.clone());
        let before = (entry.settled, entry.snoozed_until);

        app.open_sidebar_snooze_menu(target.clone(), 3, 2);
        app.open_snooze_time_input(target);
        if let Some(menu) = app.state.sidebar_snooze.as_mut() {
            menu.time_draft = Some("14:30".into());
        }
        app.state.sidebar_snooze = None;

        let after = &app.state.remote_agent_panel_entries[0];
        assert_eq!((after.settled, after.snoozed_until), before);
    }

    #[test]
    fn failed_remote_selection_does_not_block_another_clients_session_shortcuts() {
        let mut app = sidebar_order_app(false);
        let local_pane = app.state.workspaces[0].tabs[0].root_pane;
        let mut client_a = crate::app::state::SidebarPresentationState::default();
        let mut client_b = crate::app::state::SidebarPresentationState::default();

        app.state.swap_sidebar_presentation(&mut client_a);
        app.state.sidebar_focused = true;
        app.state.sidebar_selected_remote_agent = Some(
            crate::api::schema::AgentRef::new("offline", "failed-attach")
                .expect("valid remote reference"),
        );
        app.state.swap_sidebar_presentation(&mut client_a);

        app.state.swap_sidebar_presentation(&mut client_b);
        app.state.sidebar_focused = true;
        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.sidebar_snooze.is_some());
        app.state.sidebar_snooze = None;
        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.pane_is_settled(0, local_pane));
        app.state.swap_sidebar_presentation(&mut client_b);

        app.state.swap_sidebar_presentation(&mut client_a);
        assert!(app.state.sidebar_selected_remote_agent.is_some());
    }

    #[test]
    fn sidebar_session_shortcuts_do_not_fire_through_an_open_menu() {
        let mut app = sidebar_order_app(false);
        app.state.sidebar_focused = true;
        app.state.sidebar_group_menu_open = true;
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");

        assert!(!app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::empty(),
        )));
        assert!(!app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.sidebar_snooze.is_none());
        assert!(!app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn clicking_settled_workspace_header_focuses_without_unsettling_its_pane() {
        for mode in [
            SidebarGroupMode::Repo,
            SidebarGroupMode::RepoWorktree,
            SidebarGroupMode::Spaces,
        ] {
            let mut app = sidebar_order_app(true);
            let target_ws_idx = 1;
            let hidden_sibling =
                app.state.workspaces[target_ws_idx].test_split(Direction::Horizontal);
            app.state.workspaces[target_ws_idx].tabs[0]
                .panes
                .get_mut(&hidden_sibling)
                .expect("hidden sibling")
                .settled_at = Some(1_725_000_001);
            app.state.ensure_test_terminals();
            app.state.set_sidebar_group_mode(mode);
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let target =
                crate::ui::compute_workspace_card_areas(&app.state, app.state.view.sidebar_rect)
                    .into_iter()
                    .find(|card| card.ws_idx == target_ws_idx && card.settled_pane_id.is_some())
                    .expect("settled workspace header");
            let target_pane_id = target.settled_pane_id.expect("represented settled pane");

            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                target.rect.x + 8,
                target.rect.y,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                target.rect.x + 8,
                target.rect.y,
            ));

            assert_eq!(app.state.active, Some(target_ws_idx), "{mode:?}");
            assert_eq!(
                app.state.workspaces[target_ws_idx].focused_pane_id(),
                Some(target_pane_id),
                "{mode:?}"
            );
            assert!(
                app.state.pane_is_settled(target_ws_idx, target_pane_id),
                "{mode:?}"
            );
            assert!(
                app.state.pane_is_settled(target_ws_idx, hidden_sibling),
                "{mode:?}: group header resumed an unrelated hidden sibling"
            );
            assert!(app.state.sidebar_selected_settled.is_none(), "{mode:?}");
        }
    }

    #[test]
    fn settled_workspace_header_targets_its_settled_pane_in_a_mixed_workspace() {
        let mut app = sidebar_order_app(false);
        app.state.set_sidebar_group_mode(SidebarGroupMode::Repo);
        let target_ws_idx = 0;
        let target_tab_idx = 2;
        let target_pane_id = app.state.workspaces[target_ws_idx].tabs[target_tab_idx].root_pane;
        app.state.workspaces[target_ws_idx].tabs[target_tab_idx]
            .panes
            .get_mut(&target_pane_id)
            .expect("target pane")
            .settled_at = Some(1_725_000_000);
        let terminal_id = app.state.workspaces[target_ws_idx].tabs[target_tab_idx].panes
            [&target_pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("target terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("mixed-settled")
                    .expect("valid session id"),
            });
        app.state.reconcile_sidebar_presentation();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        assert_ne!(
            app.state.workspaces[target_ws_idx].focused_pane_id(),
            Some(target_pane_id)
        );
        let cards =
            crate::ui::compute_workspace_card_areas(&app.state, app.state.view.sidebar_rect);
        assert!(cards
            .iter()
            .any(|card| { card.ws_idx == target_ws_idx && card.settled_pane_id.is_none() }));
        let target = cards
            .into_iter()
            .find(|card| {
                card.ws_idx == target_ws_idx && card.settled_pane_id == Some(target_pane_id)
            })
            .expect("mixed workspace settled header");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x + 8,
            target.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target.rect.x + 8,
            target.rect.y,
        ));

        assert_eq!(app.state.active, Some(target_ws_idx));
        assert_eq!(
            app.state.workspaces[target_ws_idx].active_tab_index(),
            target_tab_idx
        );
        assert_eq!(
            app.state.workspaces[target_ws_idx].focused_pane_id(),
            Some(target_pane_id)
        );
        assert!(app.state.pane_is_settled(target_ws_idx, target_pane_id));
        assert!(app.state.sidebar_selected_settled.is_none());
    }

    #[test]
    fn dragging_a_settled_row_does_not_resume_it() {
        let mut app = sidebar_order_app(true);
        app.state.set_sidebar_group_mode(SidebarGroupMode::Repo);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let cards =
            crate::ui::compute_workspace_card_areas(&app.state, app.state.view.sidebar_rect);
        let source = cards
            .iter()
            .find(|card| card.ws_idx == 0 && card.settled_pane_id.is_some())
            .expect("settled source workspace row");
        let destination = cards
            .iter()
            .find(|card| card.ws_idx == 1 && card.settled_pane_id.is_some())
            .expect("settled destination workspace row");
        let workspace_id = app.state.workspaces[source.ws_idx].id.clone();
        let pane_id = source.settled_pane_id.expect("settled source pane");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source.rect.x + 8,
            source.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            destination.rect.x + 8,
            destination.rect.y,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder { .. })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            destination.rect.x + 8,
            destination.rect.y,
        ));

        let ws_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .expect("moved workspace");
        assert!(app.state.pane_is_settled(ws_idx, pane_id));
        assert!(app.state.sidebar_selected_settled.is_none());
    }

    /// One workspace with three tabs in a single flat repo group, named so the
    /// canonical order and the alphabetical order disagree.
    fn sidebar_sort_mouse_app() -> crate::app::App {
        let mut app = app_for_mouse_test();
        let mut workspace = Workspace::test_new("ws");
        workspace.test_add_tab(Some("zeta"));
        workspace.test_add_tab(Some("alpha"));
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].custom_name = Some("mike".to_string());
        for tab_idx in 0..3 {
            let pane_id = app.state.workspaces[0].tabs[tab_idx].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("fixture terminal");
            terminal.replace_prevalidated_manual_work_context(
                crate::work_context::PaneWorkContext {
                    repo: Some("acme/one".into()),
                    ..Default::default()
                },
            );
            terminal.detected_agent = Some(Agent::Pi);
            terminal.set_raw_agent_state_for_test(AgentState::Working);
            terminal.last_agent_state_change_seq = Some(tab_idx as u64 + 1);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        app
    }

    #[test]
    fn clicking_the_sort_glyph_opens_the_dropdown_and_picking_applies_the_sort() {
        let mut app = sidebar_order_app(false);
        app.state.set_sidebar_group_mode(SidebarGroupMode::Repo);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let sidebar_area = Rect::new(
            app.state.view.sidebar_rect.x,
            app.state.view.sidebar_rect.y,
            app.state.view.sidebar_rect.width.saturating_add(1),
            app.state.view.sidebar_rect.height,
        );
        let card = crate::ui::compute_workspace_card_areas(&app.state, sidebar_area)
            .first()
            .expect("repo group card")
            .rect;
        let (glyph_col, glyph_row) = (card.right() - 1, card.y);
        assert_eq!(
            crate::ui::sidebar::sidebar_group_sort_at(&app.state, glyph_col, glyph_row),
            Some(("repo:acme/alpha".to_string(), glyph_col)),
            "the repo group header's last cell is its sort control"
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            glyph_col,
            glyph_row,
        ));
        let menu = app
            .state
            .sidebar_sort_menu
            .as_ref()
            .expect("clicking the glyph opens the sort dropdown");
        assert_eq!(menu.target, "repo:acme/alpha");
        assert_eq!(menu.current, crate::app::state::SidebarSortMode::Default);
        assert_eq!(menu.selected, 0, "the cursor starts on the current choice");

        let layout =
            crate::ui::sidebar::sidebar_sort_menu_layout(&app.state, app.state.screen_rect())
                .expect("sort dropdown layout");
        let name_row = layout.list_rect.y + 1;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            layout.list_rect.x + 1,
            name_row,
        ));
        assert_eq!(
            app.state.sidebar_group_sort("repo:acme/alpha"),
            crate::app::state::SidebarSortMode::Name,
            "picking a row applies the sort to exactly that group"
        );
        assert!(app.state.sidebar_sort_menu.is_none(), "the menu closes");
        assert_eq!(
            app.state.take_sidebar_group_sort_persistence_request(),
            Some((
                "repo:acme/alpha".to_string(),
                crate::app::state::SidebarSortMode::Name
            )),
            "the choice is queued for client-local persistence"
        );
    }

    #[test]
    fn clicking_a_sorted_row_focuses_that_rows_pane() {
        let mut app = sidebar_sort_mouse_app();
        app.state.set_sidebar_group_sort(
            "repo:acme/one".to_string(),
            crate::app::state::SidebarSortMode::Name,
        );
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let first = cards.first().expect("first sorted tab row");
        assert_eq!(first.tab_idx, 2, "alpha sorts above mike and zeta");
        let (target_ws, target_tab, target_pane) = (first.ws_idx, first.tab_idx, first.pane_id);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first.rect.x + 1,
            first.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            first.rect.x + 1,
            first.rect.y,
        ));

        assert_eq!(app.state.active, Some(target_ws));
        assert_eq!(app.state.workspaces[target_ws].active_tab, target_tab);
        assert_eq!(
            app.state.workspaces[target_ws].tabs[target_tab]
                .layout
                .focused(),
            target_pane,
            "the click lands on the row drawn there after sorting, not the row that used to be there"
        );
    }

    #[test]
    fn subgroup_picker_creates_a_name_then_offers_it_to_the_next_window() {
        let mut app = sidebar_sort_mouse_app();
        app.state.sidebar_subgroup_picker = Some(crate::app::state::SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 0,
            anchor: (5, 5),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });
        for character in "api".chars() {
            assert!(app.state.handle_sidebar_subgroup_picker_key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            )));
        }
        let choices = crate::ui::sidebar::sidebar_subgroup_picker_choices(&app.state);
        assert_eq!(
            choices,
            vec![crate::ui::sidebar::SidebarSubgroupChoice::Create(
                "api".to_string()
            )],
            "an unknown name is offered as a create row"
        );
        assert!(app.state.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )));
        assert_eq!(
            app.state.workspaces[0].tabs[0].subgroup(),
            Some("api"),
            "Enter assigns the typed name"
        );
        assert!(app.state.sidebar_subgroup_picker.is_none());

        // The next window's picker offers the name the group already has.
        app.state.sidebar_subgroup_picker = Some(crate::app::state::SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 1,
            anchor: (5, 5),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });
        let choices = crate::ui::sidebar::sidebar_subgroup_picker_choices(&app.state);
        assert_eq!(
            choices,
            vec![crate::ui::sidebar::SidebarSubgroupChoice::Existing(
                "api".to_string()
            )],
            "an existing subgroup name is a pick, not a retype"
        );
        assert!(app.state.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )));
        assert_eq!(app.state.workspaces[0].tabs[1].subgroup(), Some("api"));

        app.state.clear_tab_subgroup(0, 1);
        assert_eq!(app.state.workspaces[0].tabs[1].subgroup(), None);
    }

    fn settled_target(app: &mut crate::app::App) -> crate::app::state::PaneFocusTarget {
        if app.state.workspaces.is_empty() {
            app.state.workspaces.push(Workspace::test_new("settled"));
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.ensure_test_terminals();
        }
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .settled_at = Some(1_725_000_000);
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("settled-menu")
                    .expect("valid session id"),
            });
        crate::app::state::PaneFocusTarget {
            workspace_id: app.state.workspaces[0].id.clone(),
            pane_id,
        }
    }

    #[tokio::test]
    async fn a_pane_keeps_its_own_keys_while_a_sidebar_row_stays_selected() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.set_server_mode(crate::app::Mode::Terminal);
        app.state.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.state.sidebar_selected_work_group = Some("linear:SCA-3102".into());
        app.state.sidebar_focused = false;

        // `m` opens the sidebar object menu, but only for the sidebar. Typed
        // into a pane it is just a letter.
        let consumed = app
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('m'),
                KeyModifiers::empty(),
            ))
            .await;
        assert!(
            app.state.sidebar_object_menu.is_none(),
            "a selected sidebar row must not claim keys the pane owns"
        );
        drop(consumed);

        app.state.focus_client_on_sidebar();
        assert_eq!(
            app.state.input_owner(),
            crate::app::state::InputOwner::Sidebar
        );
        assert_eq!(
            app.state.sidebar_selected_work_group.as_deref(),
            Some("linear:SCA-3102")
        );
        let _ = app
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('m'),
                KeyModifiers::empty(),
            ))
            .await;
        assert_eq!(
            app.state
                .sidebar_object_menu
                .as_ref()
                .map(|menu| menu.target.as_str()),
            Some("linear:SCA-3102"),
            "the same key still works once the sidebar owns the keyboard"
        );
    }

    #[test]
    fn focusing_a_pane_releases_the_sidebar_keyboard() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_focused = true;
        app.state.release_dock_focus_to_pane();
        assert!(!app.state.sidebar_focused);
    }

    #[test]
    fn sidebar_object_menu_keyboard_opens_and_escape_closes() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.state.sidebar_selected_work_group = Some("linear:SCA-3102".into());

        assert!(app.handle_sidebar_object_menu_key(KeyEvent::new(
            KeyCode::Char('m'),
            KeyModifiers::empty(),
        )));
        assert_eq!(
            app.state
                .sidebar_object_menu
                .as_ref()
                .map(|menu| menu.target.as_str()),
            Some("linear:SCA-3102")
        );
        assert!(
            app.handle_sidebar_object_menu_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty(),))
        );
        assert!(app.state.sidebar_object_menu.is_none());
    }

    #[test]
    fn sidebar_ellipsis_click_anchors_the_object_menu_to_that_row() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 80, 24));
        let (col, row) = (app.state.view.sidebar_rect.y..app.state.view.sidebar_rect.bottom())
            .find_map(|row| {
                (app.state.view.sidebar_rect.x..app.state.view.sidebar_rect.right())
                    .find(|col| {
                        crate::ui::sidebar_object_action_at(&app.state, *col, row).is_some()
                    })
                    .map(|col| (col, row))
            })
            .expect("Linear object action row");

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert_eq!(
            app.state
                .sidebar_object_menu
                .as_ref()
                .and_then(|menu| menu.anchor_row),
            Some(row)
        );
        let layout =
            crate::ui::sidebar_object_menu_layout_for_test(&app.state, Rect::new(0, 0, 80, 24))
                .expect("sidebar action menu");
        assert_eq!(layout.rect.y, row + 1);
    }

    #[test]
    fn f27_sidebar_row_click_opens_linked_home_without_starting_a_pane() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.sidebar_group_mode = SidebarGroupMode::RepoPr;
        app.state.collapsed_sidebar_groups.clear();
        app.state.sidebar_work_filter.github.assignee = None;
        app.state.dock_collapsed = false;
        app.state
            .work_index_snapshot
            .as_mut()
            .and_then(|snapshot| {
                snapshot
                    .items
                    .iter_mut()
                    .find(|item| item.pr_number == Some(159))
            })
            .expect("pull request fixture")
            .source
            .github = true;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let target = "github:https://github.com/scalable-so/herdr/pull/159";
        let row = (app.state.view.sidebar_rect.y..app.state.view.sidebar_rect.bottom())
            .find(|row| {
                crate::ui::sidebar_dim_header_at(&app.state, *row).as_deref() == Some(target)
            })
            .expect("No agent yet PR row");
        let pane_count = app.state.workspaces[0].tabs[0].panes.len();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.state.view.sidebar_rect.x.saturating_add(2),
            row,
        ));

        assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), pane_count);
        assert!(app.state.dock_object_preview.is_none());
        assert_eq!(
            app.state
                .home
                .as_ref()
                .and_then(|home| home.pr.as_ref())
                .map(|pr| (pr.repo.as_str(), pr.number)),
            Some(("scalable-so/herdr", 159))
        );
    }

    #[test]
    fn sidebar_pr_writes_use_the_shared_modal_confirmation_gate() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.work_index_gh_program_override = Some(std::path::PathBuf::from("/usr/bin/false"));
        app.state.open_sidebar_object_menu(
            "github:https://github.com/scalable-so/herdr/pull/159".into(),
        );

        app.apply_sidebar_object_menu_action(11);
        assert!(matches!(
            app.state.pr_action_confirmation,
            Some(crate::app::state::PrActionConfirmation {
                ref key,
                action: crate::ui::work_list_detail::PrActionKind::Close,
            }) if key.repo == "scalable-so/herdr" && key.pr_number == Some(159)
        ));
        assert!(app.state.sidebar_object_menu.is_none());
        assert!(app.state.dock_pending_write.is_none());
        assert!(app.state.dock_write_notice.is_none());

        assert!(app.handle_pr_action_confirmation_key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.dock_pending_write.is_none());
        assert!(app.state.sidebar_object_menu.is_none());
        assert!(app
            .state
            .dock_write_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("failed")));
    }

    #[test]
    fn sidebar_ticket_transition_stages_without_running() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.work_index_linearis_program_override = Some(std::path::PathBuf::from("/usr/bin/false"));
        app.state.open_sidebar_object_menu("linear:SCA-3102".into());

        app.apply_sidebar_object_menu_action(4);
        assert_eq!(
            app.state.sidebar_object_menu.as_ref().map(|menu| menu.page),
            Some(crate::app::state::SidebarObjectMenuPage::TicketTransitions)
        );
        app.apply_sidebar_object_menu_action(3);
        assert_eq!(
            app.state.dock_pending_write,
            Some(crate::work_index::WorkItemWrite::TransitionTicket {
                identifier: "SCA-3102".into(),
                state: "Done".into(),
            })
        );
        assert!(app.state.dock_write_notice.is_none());
        assert!(
            app.handle_sidebar_object_menu_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty(),))
        );
        assert!(app.state.dock_pending_write.is_none());
    }

    #[test]
    fn sidebar_missive_action_only_requests_a_clipboard_copy() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        let web_url = "https://mail.missiveapp.com/#inbox/conversations/sample";
        let app_url = "missive://mail.missiveapp.com/#inbox/conversations/sample";
        app.state
            .work_index_snapshot
            .as_mut()
            .expect("work snapshot")
            .conversations
            .push(crate::work_index::MissiveConversation {
                id: "sample".into(),
                subject: "Billing question".into(),
                app_url: app_url.into(),
                web_url: web_url.into(),
                team: None,
                assignees: Vec::new(),
                last_activity_at: None,
                closed: false,
                labels: Vec::new(),
                pane_bound: false,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            });
        app.state
            .open_sidebar_object_menu(format!("missive:{web_url}"));

        app.apply_sidebar_object_menu_action(0);

        assert_eq!(
            app.state.request_clipboard_write,
            Some(app_url.as_bytes().to_vec())
        );
        assert!(app.state.sidebar_object_menu.is_none());
        assert!(app.state.dock_pending_write.is_none());
    }

    #[test]
    fn settled_menu_items_yield_resume_delete_and_home_dispatch_plans() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);

        app.state.sidebar_settled_menu_target = Some(target.clone());
        assert_eq!(
            app.state.select_settled_menu_action(4),
            Some(super::SettledMenuAction::Unsettle(target.clone()))
        );

        app.state.sidebar_settled_menu_target = Some(target.clone());
        assert_eq!(
            app.state.select_settled_menu_action(0),
            Some(super::SettledMenuAction::Resume(target.clone()))
        );
        // Delete asks once while ui.confirm_close is on.
        app.state.sidebar_settled_menu_target = Some(target.clone());
        assert_eq!(app.state.select_settled_menu_action(3), None);
        assert!(app.state.sidebar_settled_menu_delete_armed);
        assert_eq!(
            app.state.select_settled_menu_action(3),
            Some(super::SettledMenuAction::Delete(target.clone()))
        );
        assert!(!app.state.sidebar_settled_menu_delete_armed);

        for (index, expected_workspace) in [
            (1, crate::app::home::HomeWorkspace::CurrentCheckout),
            (2, crate::app::home::HomeWorkspace::NewWorktree),
        ] {
            app.state.sidebar_settled_menu_target = Some(target.clone());
            let Some(super::SettledMenuAction::NewThread {
                directory,
                workspace,
            }) = app.state.select_settled_menu_action(index)
            else {
                panic!("menu item {index} should start a new thread");
            };
            assert_eq!(workspace, expected_workspace);
            app.state
                .open_home_composer_in_directory(directory.clone(), workspace);
            let home = app.state.home.as_mut().expect("home composer");
            home.prompt = "continue this work".into();
            let plan = home.dispatch_plan().expect("dispatch plan");
            assert_eq!(plan.directory, directory);
            assert_eq!(plan.workspace, expected_workspace);
        }
    }

    #[test]
    fn settled_menu_delete_is_immediate_when_confirmation_is_off() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.confirm_close = false;
        app.state.sidebar_settled_menu_target = Some(target.clone());

        assert_eq!(
            app.state.select_settled_menu_action(3),
            Some(super::SettledMenuAction::Delete(target))
        );
    }

    #[test]
    fn settled_menu_delete_disarms_when_another_row_is_pressed() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.sidebar_settled_menu_target = Some(target.clone());

        assert_eq!(app.state.select_settled_menu_action(3), None);
        assert_eq!(
            app.state.select_settled_menu_action(0),
            Some(super::SettledMenuAction::Resume(target.clone()))
        );
        assert!(!app.state.sidebar_settled_menu_delete_armed);

        app.state.sidebar_settled_menu_target = Some(target);
        assert_eq!(app.state.select_settled_menu_action(3), None);
    }

    #[test]
    fn settled_menu_resume_clears_settled_at() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.set_server_mode(Mode::Settings);
        app.state.sidebar_settled_menu_target = Some(target.clone());

        app.apply_sidebar_settled_menu_action(0);

        assert!(!app.state.pane_is_settled(0, target.pane_id));
        assert_eq!(app.state.active, Some(0));
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(target.pane_id)
        );
        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    #[test]
    fn sections_resume_unsettles_every_pane_in_split_tab() {
        let mut app = app_for_mouse_test();
        if app.state.workspaces.is_empty() {
            app.state.workspaces.push(Workspace::test_new("sections"));
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.ensure_test_terminals();
        }
        let root_pane = app.state.workspaces[0].tabs[0].root_pane;
        let sibling_pane = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.sidebar_sections_layout = true;
        app.state.set_server_mode(Mode::Settings);
        for pane_id in [root_pane, sibling_pane] {
            assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_000));
        }
        let target = crate::app::state::PaneFocusTarget {
            workspace_id: app.state.workspaces[0].id.clone(),
            pane_id: sibling_pane,
        };

        app.resume_settled_pane(target);

        assert!(!app.state.pane_is_settled(0, root_pane));
        assert!(!app.state.pane_is_settled(0, sibling_pane));
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("Moved to Active")
        );
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(sibling_pane)
        );
    }

    #[test]
    fn clicking_unresumable_settled_row_focuses_its_live_pane_without_unsettling() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("settled-live"),
            Workspace::test_new("currently-focused"),
        ];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_000));
        app.state.collapsed_sidebar_groups.clear();
        let area = Rect::new(0, 0, 120, 40);
        crate::ui::compute_view(&mut app.state, area);
        let row = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .find(|card| card.ws_idx == 0 && card.pane_id == pane_id)
            .expect("settled row")
            .rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            row.x + 1,
            row.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            row.x + 1,
            row.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));
        assert!(app.state.pane_is_settled(0, pane_id));
        assert!(app.state.sidebar_settled_menu_target.is_none());
    }

    #[test]
    fn keyboard_enter_focuses_settled_pane_and_m_opens_its_menu() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        let terminal_id = app.state.workspaces[0]
            .terminal_id(target.pane_id)
            .unwrap()
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["codex".into(), "resume".into(), "session".into()],
            dedupe_key: "session".into(),
        });
        app.state.sidebar_selected_settled = Some(target.clone());

        assert!(
            app.handle_sidebar_settled_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),))
        );

        assert_eq!(app.state.sidebar_settled_menu_target, None);
        let ws_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
            .expect("settled target workspace");
        assert!(app.state.pane_is_settled(ws_idx, target.pane_id));
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_some());
        assert_eq!(app.state.active, Some(ws_idx));
        assert_eq!(
            app.state.workspaces[ws_idx].focused_pane_id(),
            Some(target.pane_id)
        );
        app.state.sidebar_selected_settled = Some(target.clone());
        assert!(app
            .handle_sidebar_settled_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::empty())));
        assert_eq!(app.state.sidebar_settled_menu_target, Some(target));
    }

    #[test]
    fn settled_sidebar_keyboard_moves_into_and_through_visible_rows() {
        let mut workspace = Workspace::test_new("settled rows");
        let first_pane = workspace.tabs[0].root_pane;
        let second_pane = workspace.test_split(Direction::Horizontal);
        let rows = [first_pane, second_pane].map(|pane_id| crate::app::state::PaneFocusTarget {
            workspace_id: workspace.id.clone(),
            pane_id,
        });
        assert_eq!(
            super::settled_sidebar_neighbor(&rows, None, false),
            Some(rows[0].clone())
        );
        assert_eq!(
            super::settled_sidebar_neighbor(&rows, None, true),
            Some(rows[1].clone())
        );
        assert_eq!(
            super::settled_sidebar_neighbor(&rows, Some(&rows[0]), false),
            Some(rows[1].clone())
        );
        assert_eq!(
            super::settled_sidebar_neighbor(&rows, Some(&rows[1]), true),
            Some(rows[0].clone())
        );
        assert_eq!(
            super::settled_sidebar_neighbor(&rows, Some(&rows[0]), true),
            Some(rows[0].clone())
        );
    }

    #[test]
    fn settled_menu_opens_below_its_sidebar_row() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.collapsed_sidebar_groups.clear();
        let area = Rect::new(0, 0, 120, 40);
        crate::ui::compute_view(&mut app.state, area);
        app.state.sidebar_settled_menu_target = Some(target.clone());
        let anchor = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .find(|card| card.pane_id == target.pane_id)
            .expect("settled row")
            .rect;
        let layout =
            crate::ui::sidebar_settled_menu_layout(&app.state, area).expect("settled dropdown");
        assert_eq!(layout.rect.y, anchor.bottom());
    }

    #[test]
    fn clicking_launcher_opens_global_menu() {
        let mut app = app_for_mouse_test();
        let rect = app.state.global_launcher_rect();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + rect.width.saturating_sub(1),
            rect.y,
        ));

        assert_eq!(app.state.server_mode(), Mode::GlobalMenu);
    }

    #[test]
    fn launcher_global_menu_owns_input_renders_and_releases_settings() {
        use crate::app::state::{InputOwner, ServerInputOwner};
        let mut app = app_for_mouse_test();
        let area = Rect::new(0, 0, 106, 30);
        crate::ui::compute_view(&mut app.state, area);
        let rect = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x,
            rect.y,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), rect.x, rect.y));
        assert_eq!(app.state.server_mode(), Mode::GlobalMenu);
        assert_eq!(
            app.state.input_owner(),
            InputOwner::Server(ServerInputOwner::GlobalMenu)
        );

        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app.state, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let menu = app.state.global_menu_rect();
        let text: String = (menu.y..menu.bottom())
            .flat_map(|y| (menu.x..menu.right()).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        for label in app.state.global_menu_labels() {
            assert!(text.contains(label), "menu should draw {label:?}: {text}");
        }
        for (idx, label) in app.state.global_menu_labels().iter().enumerate() {
            let row = menu.y + 1 + idx as u16;
            assert!(
                app.state.global_menu_item_at(menu.x + 1, row).is_some(),
                "row for {label:?} resolves an action"
            );
        }

        crate::app::input::handle_global_menu_key(
            &mut app.state,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        assert!(!matches!(app.state.server_mode(), Mode::GlobalMenu));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x,
            rect.y,
        ));
        assert_eq!(app.state.server_mode(), Mode::GlobalMenu);
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 90, 20));
        assert!(!matches!(app.state.server_mode(), Mode::GlobalMenu));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x,
            rect.y,
        ));
        let settings_row = app.state.global_menu_rect().y + 1;
        let menu_x = app.state.global_menu_rect().x + 1;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu_x,
            settings_row,
        ));
        assert_eq!(app.state.server_mode(), Mode::Settings);
        assert_eq!(
            app.state.input_owner(),
            InputOwner::Server(ServerInputOwner::Settings)
        );
    }

    #[test]
    fn clicking_sidebar_mode_header_opens_and_selects_dropdown() {
        let mut app = app_for_mouse_test();
        let anchor = app.state.sidebar_group_mode_anchor_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            anchor.x,
            anchor.y,
        ));
        assert!(app.state.sidebar_group_menu_open);

        let menu = crate::ui::sidebar_group_menu_layout(&app.state, Rect::new(0, 0, 106, 20))
            .expect("mode menu");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.list_rect.x,
            menu.list_rect.y + 2,
        ));
        assert_eq!(
            app.state.sidebar_group_mode,
            crate::app::state::SidebarGroupMode::LinearTeam
        );
        assert!(!app.state.sidebar_group_menu_open);
    }

    #[test]
    fn clicking_the_filter_chip_opens_a_downward_dropdown() {
        let mut app = app_for_mouse_test();
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::LinearTeam);
        let area = Rect::new(0, 0, 106, 40);
        crate::ui::compute_view(&mut app.state, area);
        let anchor = app.state.sidebar_filter_anchor_rect();
        assert!(anchor.width > 0, "the filter chip needs a hit area");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            anchor.x,
            anchor.y,
        ));
        assert!(app.state.sidebar_filter_menu_open);
        assert!(!app.state.sidebar_group_menu_open);

        let layout =
            crate::ui::sidebar_filter_menu_layout(&app.state, area).expect("filter dropdown");
        assert_eq!(layout.rect.y, anchor.bottom(), "dropdowns open downward");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            layout.list_rect.x,
            layout.list_rect.y + 1,
        ));
        assert!(!app.state.sidebar_filter_menu_open);
    }

    #[test]
    fn only_this_machine_filter_toggles_through_mouse_input_and_is_persisted() {
        let mut app = app_for_mouse_test();
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::LinearTeam);
        let area = Rect::new(0, 0, 106, 40);
        crate::ui::compute_view(&mut app.state, area);
        let anchor = app.state.sidebar_filter_anchor_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            anchor.x,
            anchor.y,
        ));
        let layout =
            crate::ui::sidebar_filter_menu_layout(&app.state, area).expect("filter dropdown");
        assert_eq!(
            crate::ui::sidebar_filter_options(&app.state)[0].label(),
            "[x] Only this machine"
        );
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            layout.list_rect.x,
            layout.list_rect.y,
        ));
        assert!(!app.state.sidebar_work_filter.only_this_machine);
        assert!(app.state.sidebar_filter_menu_open);
        assert!(
            !app.state
                .take_sidebar_work_filter_persistence_request()
                .expect("toggle requests persistence")
                .only_this_machine
        );

        let layout =
            crate::ui::sidebar_filter_menu_layout(&app.state, area).expect("filter stays open");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            layout.list_rect.x,
            layout.list_rect.y,
        ));
        assert!(app.state.sidebar_work_filter.only_this_machine);
    }

    #[test]
    fn hovering_global_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.global_menu.highlighted, 1);
    }

    #[test]
    fn clicking_keybinds_menu_item_opens_help() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.server_mode(), Mode::KeybindHelp);
    }

    #[test]
    fn clicking_settings_menu_item_opens_settings() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1,
        ));

        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    #[test]
    fn clicking_reload_config_menu_item_requests_reload() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 3,
        ));

        assert!(app.state.request_reload_config);
        assert_eq!(app.state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn update_pending_menu_surfaces_update_ready_entry() {
        let mut app = app_for_mouse_test();
        app.state.update_available = Some("0.3.2".into());
        app.state.latest_release_notes_available = true;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "update ready",
                "detach"
            ]
        );
        assert!(!app.state.should_quit);
    }

    #[test]
    fn persistence_mode_menu_surfaces_detach_action() {
        let mut app = app_for_mouse_test();
        app.state.detach_exits = false;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec!["settings", "keybinds", "reload config", "detach"]
        );

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 4,
        ));

        assert!(app.state.detach_requested);
        assert!(!app.state.should_quit);
        assert_ne!(app.state.server_mode(), Mode::GlobalMenu);
    }

    #[test]
    fn whats_new_remains_in_menu_for_latest_installed_release_notes() {
        let mut app = app_for_mouse_test();
        app.state.latest_release_notes_available = true;

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "what's new",
                "detach"
            ]
        );
    }

    #[test]
    fn ac4_clicking_tab_row_preserves_that_tabs_focused_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("main".into());
        let first_pane = ws.tabs[0].root_pane;
        let first_tab = ws.test_add_tab(Some("logs"));
        ws.active_tab = first_tab;
        let second_pane = ws.test_split(Direction::Horizontal);
        assert_eq!(ws.tabs[first_tab].layout.focused(), second_pane);
        ws.active_tab = 0;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[first_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .iter()
            .find(|card| card.tab_idx == first_tab)
            .unwrap()
            .rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.workspaces[0].active_tab, first_tab);
        assert_eq!(
            snapshot.workspaces[0].tabs[first_tab].focused,
            Some(second_pane.raw())
        );
    }

    #[test]
    fn legacy_agent_row_layout_does_not_change_single_line_tab_targets() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let second = Workspace::test_new("two");
        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        app.state.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![
                vec![crate::config::AgentSidebarToken::Agent],
                vec![crate::config::AgentSidebarToken::Workspace],
            ],
        );
        app.state.sidebar_agents.row_gap = 1;
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.reconcile_sidebar_presentation();

        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        assert_eq!(cards.len(), 2);
        assert_eq!(app.state.tab_target_at(cards[0].rect.y), Some((0, 0)));
        assert_eq!(app.state.tab_target_at(cards[1].rect.y), Some((1, 0)));
        assert!(
            crate::ui::compute_agent_card_areas(&app.state, app.state.view.sidebar_rect).is_empty()
        );
    }

    #[test]
    fn sidebar_hit_testing_recomputes_client_local_scrolled_row_geometry() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.state.sidebar_spaces.rows = vec![
            vec![crate::config::SpaceSidebarToken::Workspace],
            vec![crate::config::SpaceSidebarToken::StateText],
        ];
        app.state.sidebar_spaces.row_gap = 1;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 8);
        app.state.ensure_test_terminals();
        app.state.reconcile_sidebar_presentation();
        app.state.workspace_scroll =
            crate::ui::sidebar_row_index_for_workspace(&app.state, 1).expect("second Space row");

        let (cards, _) =
            crate::ui::compute_sidebar_row_areas(&app.state, app.state.view.sidebar_rect);
        assert_eq!(cards[0].ws_idx, 1);
        let target_row = cards[0].rect.y;
        app.state.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: cards[0].rect,
            indented: false,
            repo_header: false,
            settled_pane_id: None,
        }];

        assert_eq!(app.state.workspace_at_row(target_row), Some(1));
        assert_eq!(
            app.state.workspace_drop_target_at_row(target_row),
            Some(crate::app::state::WorkspaceDropTarget::Before(1))
        );
    }

    #[test]
    fn tab_hit_testing_ignores_legacy_agent_geometry_cached_for_another_client() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        for (ws_idx, pane_id) in [(0, first_pane), (1, second_pane)] {
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(Agent::Claude);
        }
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.sidebar_presentation.expanded_workspace_ids = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let first_row = cards[0].rect;
        app.state.view.agent_card_areas = vec![crate::app::state::AgentCardArea {
            ws_idx: 1,
            tab_idx: 0,
            pane_id: second_pane,
            rect: first_row,
            row_idx: 0,
        }];

        assert_eq!(app.state.tab_target_at(first_row.y), Some((0, 0)));
    }

    #[test]
    fn tab_hit_testing_uses_current_hierarchy_after_legacy_filter_change() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        for (ws_idx, pane_id) in [(0, first_pane), (1, second_pane)] {
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(Agent::Claude);
        }
        app.state.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(crate::api::schema::AgentViewFilter::Eq {
                field: crate::api::schema::AgentViewField::Builtin(
                    crate::api::schema::AgentViewBuiltinField::WorkspaceId,
                ),
                value: crate::api::schema::AgentViewValue::Context {
                    context: crate::api::schema::AgentViewContext::CurrentWorkspaceId,
                },
            }),
            sort: Vec::new(),
        });
        app.state.sidebar_presentation.expanded_workspace_ids = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        app.state.workspace_scroll = 10;
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let card = cards
            .first()
            .expect("expanded hierarchy should expose a visible tab card");

        assert_eq!(
            app.state.tab_target_at(card.rect.y),
            Some((card.ws_idx, card.tab_idx))
        );
    }

    #[test]
    fn clicking_all_workspaces_tab_row_switches_to_correct_workspace() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;

        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        assert!(app.state.workspace_agents_expanded(1));
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .iter()
            .find(|card| card.ws_idx == 1 && card.tab_idx == 0)
            .unwrap()
            .rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces[1].active_tab, 0);
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            second_pane
        );
    }

    #[test]
    fn scrolling_flat_agent_projection_with_wheel_updates_shared_scroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;

        let mut tabs = Vec::new();
        for (tab_name, agent) in [
            ("logs", Agent::Claude),
            ("review", Agent::Codex),
            ("ops", Agent::Gemini),
            ("deploy", Agent::Claude),
        ] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        for (tab_idx, pane_id, agent) in tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 5);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 5);

        let detail_area = app.state.workspace_list_rect();
        assert!(crate::ui::should_show_scrollbar(
            crate::ui::workspace_list_scroll_metrics(&app.state, detail_area)
        ));

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            detail_area.x + 1,
            detail_area.y + 4,
        ));

        assert_eq!(app.state.workspace_scroll, 1);
        assert_eq!(app.state.selected, 0);
    }

    #[test]
    fn clicking_scrolled_tab_row_switches_to_correct_tab_and_preserves_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        let mut extra_tabs = Vec::new();
        for (tab_name, agent) in [("review", Agent::Codex), ("ops", Agent::Gemini)] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            extra_tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        for (tab_idx, pane_id, agent) in extra_tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![
                vec![crate::config::AgentSidebarToken::Agent],
                vec![crate::config::AgentSidebarToken::Workspace],
            ],
        );
        app.state.reconcile_sidebar_presentation();
        app.state.workspace_scroll = 1;
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .iter()
            .find(|card| card.tab_idx == second_tab)
            .unwrap()
            .rect;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, second_tab);
        assert_eq!(
            app.state.workspaces[0].tabs[second_tab].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn clicking_collapsed_agent_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);
        app.state.reconcile_sidebar_presentation();

        let row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    crate::ui::SidebarRow::Tab { entry, .. }
                        if entry.local_target().is_some_and(|target| target.tab_idx == second_tab)
                )
            })
            .unwrap() as u16;
        let (content, _, _) = crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x,
            content.y + row,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn clicking_collapsed_snoozed_row_focuses_its_exact_pane() {
        let mut app = app_for_mouse_test();
        let mut workspace = Workspace::test_new("split");
        let active_pane = workspace.tabs[0].root_pane;
        let snoozed_pane = workspace.test_split(Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(active_pane);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        for pane_id in [active_pane, snoozed_pane] {
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal")
                .detected_agent = Some(Agent::Pi);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);
        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 900;
        assert!(app.state.snooze_pane_at(0, snoozed_pane, deadline));
        app.state.collapsed_sidebar_groups.clear();
        app.state.refresh_local_agent_panel_identities();
        app.state.reconcile_sidebar_presentation();

        let row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    crate::ui::SidebarRow::Agent { entry, .. }
                        | crate::ui::SidebarRow::Tab { entry, .. }
                        if entry.local_target().is_some_and(|target| target.pane_id == snoozed_pane)
                )
            })
            .expect("snoozed pane row") as u16;
        let (content, _, _) = crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x,
            content.y + row,
        ));

        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            snoozed_pane
        );
    }

    #[test]
    fn clicking_collapsed_priority_projection_keeps_workspace_order() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_collapsed = true;
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let set_state = |app: &mut crate::app::App, ws_idx: usize, pane_id, state| {
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.set_raw_agent_state_for_test(state);
        };
        set_state(&mut app, 0, first_pane, AgentState::Working);
        set_state(&mut app, 1, second_pane, AgentState::Blocked);

        let (detail_area, _, _) =
            crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        let row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|row| matches!(row, crate::ui::SidebarRow::Workspace { ws_idx: 1, .. }))
            .unwrap() as u16;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            detail_area.x,
            detail_area.y + row,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            second_pane
        );
    }

    #[test]
    fn clicking_collapsed_sidebar_toggle_expands_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let toggle = crate::ui::collapsed_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(!app.state.sidebar_collapsed);
    }

    #[test]
    fn hidden_collapsed_sidebar_has_no_mouse_expand_hotspot() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
        app.state.view.sidebar_rect = Rect::new(0, 0, 0, 20);
        app.state.view.terminal_area = Rect::new(0, 0, 80, 20);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 19));

        assert!(app.state.sidebar_collapsed);
    }

    #[test]
    fn clicking_expanded_sidebar_toggle_collapses_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = false;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 20);

        let toggle = crate::ui::expanded_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(app.state.sidebar_collapsed);
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn clicking_workspace_switches_on_mouse_up() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let target_row = app.state.view.workspace_card_areas[1].rect.y;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            target_row,
        ));
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspace_presses.len(), 1);

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert!(app.state.workspace_presses.is_empty());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.active, Some(1));
        assert_eq!(snapshot.selected, 1);
    }

    #[test]
    fn clicking_the_spaces_header_folds_and_unfolds_the_tree() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let header = crate::ui::compute_sidebar_section_header_areas(
            &app.state,
            app.state.view.sidebar_rect,
        )
        .into_iter()
        .find(|header| header.title == crate::ui::SPACES_SECTION_TITLE)
        .expect("the Spaces header is always present");
        let rows_open = crate::ui::sidebar_rows(&app.state).len();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            header.rect.x + 1,
            header.rect.y,
        ));
        assert!(app.state.collapsed_sidebar_groups.contains("repo:Spaces"));
        assert!(crate::ui::sidebar_rows(&app.state).len() < rows_open);

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            header.rect.x + 1,
            header.rect.y,
        ));
        assert!(!app.state.collapsed_sidebar_groups.contains("repo:Spaces"));
        assert_eq!(crate::ui::sidebar_rows(&app.state).len(), rows_open);
    }

    #[test]
    fn clicking_a_blocked_worklist_row_focuses_its_own_pane() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("blocked")];
        app.state.ensure_test_terminals();
        let working_pane = app.state.workspaces[0].tabs[0].root_pane;
        let blocked_pane = app.state.workspaces[1].tabs[0].root_pane;
        for (ws_idx, pane_id, state) in [
            (0, working_pane, AgentState::Working),
            (1, blocked_pane, AgentState::Blocked),
        ] {
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .unwrap()
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.set_raw_agent_state_for_test(state);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let screen = Rect::new(0, 0, 106, 40);
        crate::ui::compute_view(&mut app.state, screen);
        let area = app.state.view.sidebar_rect;
        let card = crate::ui::compute_tab_card_areas(&app.state, area)
            .into_iter()
            .find(|card| card.pane_id == blocked_pane)
            .expect("blocked worklist row should be clickable");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            card.rect.x + 1,
            card.rect.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            blocked_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn clicking_worktree_parent_row_focuses_workspace_without_toggling() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.set_server_mode(Mode::Terminal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0].rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn clicking_space_chevron_toggles_direct_window_rows() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.set_server_mode(Mode::Terminal);
        app.state.ensure_test_terminals();
        app.state.reconcile_sidebar_presentation();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0];
        let chevron = crate::ui::workspace_agent_chevron_rect(&app.state, &parent, true);
        assert!(app.state.workspace_agents_expanded(0));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chevron.x,
            chevron.y,
        ));

        assert_eq!(app.state.active, None);
        assert!(app.state.workspace_presses.is_empty());
        assert!(!app.state.workspace_agents_expanded(0));
        assert!(!app.state.collapsed_space_keys.contains("repo-key"));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chevron.x,
            chevron.y,
        ));

        assert!(app.state.workspace_agents_expanded(0));
    }

    #[test]
    fn wheel_workspace_selection_follows_grouped_visual_order_without_scrollbar() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("main"),
            Workspace::test_new("normal"),
            Workspace::test_new("issue"),
        ];
        for (idx, checkout_path) in [(0, "/repo/herdr"), (2, "/repo/herdr-issue")] {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx != 0,
                });
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 30));
        let list = app.state.workspace_list_rect();
        assert!(!crate::ui::should_show_scrollbar(
            crate::ui::workspace_list_scroll_metrics(&app.state, list)
        ));

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, list.x + 1, list.y + 1));

        assert_eq!(app.state.selected, 2);
    }

    #[test]
    fn dragging_workspace_reorders_without_changing_identity() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        app.state.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.state.sidebar_spaces.row_gap = 0;
        let active_id = app.state.workspaces[1].id.clone();
        let selected_id = app.state.workspaces[2].id.clone();
        app.state.active = Some(1);
        app.state.selected = 2;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let packed_boundary_row = app.state.view.workspace_card_areas[1].rect.y;
        assert_eq!(
            app.state.workspace_drop_target_at_row(packed_boundary_row),
            Some(crate::app::state::WorkspaceDropTarget::Before(2))
        );

        let source_row = app.state.view.workspace_card_areas[1].rect.y;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::Before(0),
        )
        .unwrap();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            source_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 1,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::Before(0)),
                ..
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        let names: Vec<_> = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.selected, 2);
        assert_eq!(app.state.workspaces[0].id, active_id);
        assert_eq!(app.state.workspaces[2].id, selected_id);
        let events = app.event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::WorkspaceMoved { .. }
        )));
        assert!(!events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::WorkspaceReordered { .. }
        )));
        let snapshot = capture_snapshot(&app.state);
        let captured_names: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.custom_name.clone().unwrap())
            .collect();
        assert_eq!(captured_names, vec!["b", "a", "c"]);
    }

    #[test]
    fn clicking_tab_scroll_button_reveals_hidden_tabs_without_renaming() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));
        ws.test_add_tab(Some("review"));
        ws.test_add_tab(Some("ops"));
        ws.test_add_tab(Some("notes"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 66, 20));

        let right = app.state.view.tab_scroll_right_hit_area;
        assert!(right.width > 0);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right.x + 1,
            right.y,
        ));

        assert_eq!(app.state.tab_scroll, 1);
        assert!(!app.state.tab_scroll_follow_active);
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(app.state.view.tab_hit_areas[0].width, 0);
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.state.workspaces[0].tabs[1].custom_name.as_deref(),
            Some("logs")
        );
    }

    #[test]
    fn clicking_last_visible_tab_at_right_edge_does_not_overscroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.tab_scroll = usize::MAX;
        app.state.tab_scroll_follow_active = false;
        // 91 columns leaves the same tab strip as 65 did before the tab row
        // gained the action, git, and pane-toggle buttons at its right end.
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 91, 20));

        let last_idx = app.state.workspaces[0].tabs.len() - 1;
        let target = app.state.view.tab_hit_areas[last_idx];
        let clamped_scroll = app.state.tab_scroll;
        assert!(target.width > 0, "last tab should already be visible");
        let target_col = target.x + target.width - 1;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target_col,
            target.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target_col,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, last_idx);
        assert_eq!(app.state.tab_scroll, clamped_scroll);
        assert!(app.state.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn dragging_tab_reorders_auto_and_custom_names_without_materializing_numbers() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("foo"));
        ws.test_add_tab(None);
        let moved_root = ws.tabs[0].root_pane;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let source = app.state.view.tab_hit_areas[0];
        let last = app.state.view.tab_hit_areas[2];
        let drop_col = last.x + last.width;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source.x + 1,
            source.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drop_col,
            source.y,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::TabReorder {
                ws_idx: 0,
                source_tab_idx: 0,
                insert_idx: Some(3),
                ..
            })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drop_col,
            source.y,
        ));

        let labels: Vec<_> = app.state.workspaces[0]
            .tabs
            .iter()
            .enumerate()
            .map(|(tab_idx, _)| {
                app.state.workspaces[0]
                    .tab_display_name_from(&app.state.terminals, tab_idx)
                    .unwrap_or_else(|| (tab_idx + 1).to_string())
            })
            .collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("foo")
        );
        assert!(app.state.workspaces[0].tabs[1].custom_name.is_none());
        assert!(app.state.workspaces[0].tabs[2].custom_name.is_none());
        assert_eq!(app.state.workspaces[0].tabs[0].number, 2);
        assert_eq!(app.state.workspaces[0].tabs[1].number, 3);
        assert_eq!(app.state.workspaces[0].tabs[2].number, 1);
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let repo = unique_temp_path("sidebar-drop-slot-repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .unwrap();
        repo
    }

    fn workspace_with_space(name: &str, key: &str) -> Workspace {
        let mut ws = Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/{name}").into(),
            is_linked_worktree: name != "main",
        });
        ws
    }

    #[test]
    fn top_drop_slot_is_distinct_from_gap_below_first_workspace() {
        let mut app = app_for_mouse_test();
        let first_repo = temp_git_repo("main");
        let second_repo = temp_git_repo("main");

        let mut first = Workspace::test_new("a");
        let first_root = first.tabs[0].root_pane;
        first.identity_cwd = first_repo.clone();
        first.refresh_git_ahead_behind();

        let mut second = Workspace::test_new("b");
        let second_root = second.tabs[0].root_pane;
        second.identity_cwd = second_repo.clone();
        second.refresh_git_ahead_behind();

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_root]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&first_terminal_id).unwrap().cwd = first_repo.clone();
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_root]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .cwd = second_repo.clone();
        app.state.sidebar_spaces.row_gap = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        // Status bar occupies row 0; sidebar list starts at y=1.
        assert_eq!(app.state.workspace_drop_target_at_row(0), None);
        let slots = crate::ui::workspace_drop_slots(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
        );
        let first_slot = slots
            .iter()
            .find(|(target, _)| *target == crate::app::state::WorkspaceDropTarget::Before(0))
            .unwrap()
            .1;
        let second_slot = slots
            .iter()
            .find(|(target, _)| *target == crate::app::state::WorkspaceDropTarget::Before(1))
            .unwrap()
            .1;
        assert!(second_slot > first_slot);
        assert_eq!(
            app.state.workspace_drop_target_at_row(first_slot),
            Some(crate::app::state::WorkspaceDropTarget::Before(0))
        );
        assert_eq!(
            app.state.workspace_drop_target_at_row(second_slot),
            Some(crate::app::state::WorkspaceDropTarget::Before(1))
        );

        let _ = fs::remove_dir_all(first_repo);
        let _ = fs::remove_dir_all(second_repo);
    }

    #[test]
    fn bottom_drop_slot_stays_below_last_workspace_not_footer() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));

        let cards = &app.state.view.workspace_card_areas;
        let bottom_slot = crate::ui::workspace_drop_indicator_row(
            &app.state,
            cards,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();

        let last = cards.last().unwrap().rect;
        assert_eq!(bottom_slot, last.y + last.height);
        assert!(bottom_slot < app.state.sidebar_footer_rect().y.saturating_sub(1));
    }

    #[test]
    fn grouped_sidebar_drop_slots_do_not_land_inside_compact_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(1);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let cards = &app.state.view.workspace_card_areas;
        let order = cards.iter().map(|card| card.ws_idx).collect::<Vec<_>>();
        assert_eq!(order, vec![0, 1]);
        let normal = cards.iter().find(|card| card.ws_idx == 1).unwrap();

        assert_eq!(
            app.state.workspace_drop_target_at_row(cards[0].rect.y),
            Some(crate::app::state::WorkspaceDropTarget::Before(0))
        );
        assert_eq!(
            crate::ui::workspace_drop_indicator_row(
                &app.state,
                cards,
                app.state.workspace_list_rect(),
                crate::app::state::WorkspaceDropTarget::End,
            ),
            Some(normal.rect.y + normal.rect.height)
        );
    }

    #[test]
    fn plain_drag_anchors_to_the_selected_parentless_linked_workspace() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("one", "repo-key"),
            workspace_with_space("two", "repo-key"),
            Workspace::test_new("normal"),
        ];
        let target_id = app.state.workspaces[1].id.clone();

        let params = app
            .state
            .workspace_move_block_params(2, crate::app::state::WorkspaceDropTarget::Before(1))
            .unwrap();

        assert_eq!(params.workspace_ids, [app.state.workspaces[2].id.clone()]);
        assert_eq!(
            params.before_workspace_id.as_deref(),
            Some(target_id.as_str())
        );
    }

    #[test]
    fn dragging_worktree_parent_reorders_the_complete_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(2);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let parent = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 0)
            .unwrap()
            .rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();
        let active_id = app.state.workspaces[2].id.clone();
        let selected_id = app.state.workspaces[1].id.clone();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, parent.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 0,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::End),
                ..
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "main", "issue"]
        );
        assert_eq!(
            app.state.workspaces[app.state.active.unwrap()].id,
            active_id
        );
        assert_eq!(app.state.workspaces[app.state.selected].id, selected_id);
    }

    #[test]
    fn dragging_collapsed_worktree_parent_still_moves_hidden_children() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("issue", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("main", "repo-key"),
            workspace_with_space("review", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.collapsed_space_keys.insert("repo-key".into());
        let active_id = app.state.workspaces[0].id.clone();
        let selected_id = app.state.workspaces[1].id.clone();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));
        assert_eq!(app.state.view.workspace_card_areas.len(), 2);

        let parent = app.state.view.workspace_card_areas[0].rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, parent.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "main", "issue", "review"]
        );
        assert_eq!(
            app.state.workspaces[app.state.active.unwrap()].id,
            active_id
        );
        assert_eq!(app.state.workspaces[app.state.selected].id, selected_id);
    }

    #[test]
    fn linked_worktree_has_no_intermediate_draggable_space_row() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let source = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 2)
            .copied();
        assert!(source.is_none());

        let names = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["main", "normal", "issue"]);
    }

    #[test]
    fn dragging_sidebar_divider_sets_manual_width() {
        let mut app = app_for_mouse_test();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30, 5));

        assert_eq!(app.state.sidebar_width, 31);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(31));
    }

    #[test]
    fn drag_hit_column_matches_rendered_sidebar_separator() {
        let app = app_for_mouse_test();
        let sidebar = app.state.view.sidebar_rect;
        let separator_col = crate::ui::sidebar_separator_col(sidebar).unwrap();
        let row = sidebar.y + 2;

        assert!(app.state.on_sidebar_divider(separator_col, row));
        assert!(!app
            .state
            .on_sidebar_divider(separator_col.saturating_sub(1), row));
    }

    #[test]
    fn dragging_sidebar_bottom_divider_still_sets_manual_width() {
        let mut app = app_for_mouse_test();
        let divider_col = app.state.view.sidebar_rect.x + app.state.view.sidebar_rect.width - 1;
        let bottom_row = app.state.view.sidebar_rect.y + app.state.view.sidebar_rect.height - 1;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            divider_col,
            bottom_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            divider_col + 5,
            bottom_row,
        ));

        assert_eq!(app.state.sidebar_width, 31);
    }

    #[test]
    fn dragging_past_max_clamps_to_configured_max() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_max_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5));

        assert_eq!(app.state.sidebar_width, 30);
    }

    #[test]
    fn dragging_below_min_clamps_to_configured_min() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_min_width = 22;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 5));

        assert_eq!(app.state.sidebar_width, 22);
    }

    #[test]
    fn removed_sidebar_section_divider_is_not_interactive() {
        let mut app = app_for_mouse_test();
        let original = app.state.sidebar_section_split;
        let old_divider_row =
            app.state.view.sidebar_rect.y + app.state.view.sidebar_rect.height / 2;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.state.view.sidebar_rect.x + 1,
            old_divider_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            app.state.view.sidebar_rect.x + 1,
            old_divider_row + 4,
        ));

        assert_eq!(app.state.sidebar_section_split, original);
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn double_clicking_sidebar_divider_resets_default_width() {
        let mut app = app_for_mouse_test();
        app.state.default_sidebar_width = 26;
        app.state.sidebar_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));

        assert_eq!(app.state.sidebar_width, 26);
        assert!(app.state.drag.is_none());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(26));
    }

    fn aloop_key_fixture() -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        app.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            aloop: Some(crate::aloop::ProducerSnapshot::read(
                "ub2".to_string(),
                crate::aloop::HostData {
                    findings: vec![std::sync::Arc::new(crate::aloop::Finding {
                        loop_name: "nightly".to_string(),
                        source: "sentry".to_string(),
                        stable_id: "abc-123".to_string(),
                        title: "worker crashed".to_string(),
                        url: None,
                        evidence: "stacktrace line".to_string(),
                        prompt: "fix the crash".to_string(),
                        created_at: "2026-09-18T09:50:00Z".to_string(),
                        created_at_unix_s: crate::fleet::parse_utc_timestamp(
                            "2026-09-18T09:50:00Z",
                        )
                        .expect("timestamp"),
                        status: crate::aloop::FindingStatus::Pending,
                    })],
                    loops: vec![crate::aloop::LoopRuns {
                        loop_name: "nightly".to_string(),
                        runs: vec![
                            std::sync::Arc::new(crate::aloop::RunRecord {
                                at: "2026-09-18T09:59:00Z".to_string(),
                                at_unix_s: crate::fleet::parse_utc_timestamp(
                                    "2026-09-18T09:59:00Z",
                                )
                                .expect("timestamp"),
                                duration_ms: 1_200,
                                exit: 0,
                                findings: 0,
                                stable_ids: Vec::new(),
                                log_excerpt: "clean log tail".to_string(),
                            }),
                            std::sync::Arc::new(crate::aloop::RunRecord {
                                at: "2026-09-18T09:58:00Z".to_string(),
                                at_unix_s: crate::fleet::parse_utc_timestamp(
                                    "2026-09-18T09:58:00Z",
                                )
                                .expect("timestamp"),
                                duration_ms: 2_400,
                                exit: 0,
                                findings: 2,
                                stable_ids: vec!["abc-123".to_string()],
                                log_excerpt: "hit log tail".to_string(),
                            }),
                        ],
                        skipped_lines: 0,
                    }],
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        app
    }

    #[test]
    fn ac5_enter_on_an_aloop_finding_opens_the_composer_without_a_spawn() {
        let mut app = aloop_key_fixture();
        app.machines = crate::app::machines::resolve(&crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub2".to_string(),
                target: "ub2".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.sidebar_selected_work_group = Some("aloop:finding:nightly:abc-123".into());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        let home = app.home.as_ref().expect("composer opened");
        assert_eq!(home.prompt, "fix the crash\n\nEvidence:\nstacktrace line");
        // AC4: the composer targets the finding's producer host.
        assert_eq!(
            home.machine().map(|machine| machine.name.as_str()),
            Some("ub2")
        );
        // AC5: opening the composer never creates a pane.
        assert!(app.workspaces.is_empty());
        assert_eq!(app.sidebar_selected_work_group, None);
    }

    #[test]
    fn ac4_n_on_an_aloop_finding_dispatches_to_the_producer_host() {
        let mut app = aloop_key_fixture();
        app.machines = crate::app::machines::resolve(&crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub2".to_string(),
                target: "ub2".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.sidebar_selected_work_group = Some("aloop:finding:nightly:abc-123".into());

        let crate::app::SidebarWorkGroupKeyAction::Dispatch(plan) = app
            .handle_sidebar_work_group_key(KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::empty(),
            ))
        else {
            panic!("n should dispatch the selected finding");
        };

        assert_eq!(plan.prompt, "fix the crash\n\nEvidence:\nstacktrace line");
        let remote = plan.remote.expect("producer host machine");
        assert_eq!(remote.name, "ub2");
    }

    #[test]
    fn ac8_enter_on_an_aloop_loop_header_opens_its_run_history() {
        let mut app = aloop_key_fixture();
        app.sidebar_selected_work_group = Some("aloop:loop:nightly".into());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        let detail = app
            .loop_run_history_detail
            .as_ref()
            .expect("run history opened");
        assert_eq!(detail.loop_id, "nightly");
        let rendered = crate::ui::loop_runs::project_loop_run_history(
            &detail.history,
            &detail.loop_id,
            std::time::UNIX_EPOCH,
        );
        assert_eq!(rendered.rows.len(), 2);
        assert_eq!(rendered.rows[0].run_id, "2026-09-18T09:59:00Z");
        assert_eq!(rendered.rows[1].run_id, "2026-09-18T09:58:00Z");
        assert_eq!(rendered.rows[1].duration, "2s");
        assert_eq!(app.sidebar_selected_work_group, None);
        // A loop header never dispatches.
        assert!(matches!(
            {
                app.sidebar_selected_work_group = Some("aloop:loop:nightly".into());
                app.handle_sidebar_work_group_key(KeyEvent::new(
                    KeyCode::Char('n'),
                    KeyModifiers::empty(),
                ))
            },
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        assert!(app.workspaces.is_empty());
    }

    #[test]
    fn ac7_enter_on_a_clean_run_opens_its_run_log() {
        let mut app = aloop_key_fixture();
        app.sidebar_selected_work_group =
            Some("aloop:cleanrun:nightly:2026-09-18T09:59:00Z".into());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        let detail = app.aloop_run_detail.expect("run log opened");
        assert_eq!(detail.loop_name, "nightly");
        assert_eq!(detail.host, "ub2");
        assert_eq!(detail.run.log_excerpt, "clean log tail");
    }

    #[test]
    fn ac4_dispatch_fails_loudly_when_the_producer_host_is_not_a_machine() {
        let mut app = aloop_key_fixture();
        app.sidebar_selected_work_group = Some("aloop:finding:nightly:abc-123".into());

        let action = app.handle_sidebar_work_group_key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::empty(),
        ));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        assert_eq!(
            app.config_diagnostic.as_deref(),
            Some("aloop producer host `ub2` is not a configured machine")
        );
        assert!(app.workspaces.is_empty());
    }

    #[test]
    fn ac7_enter_on_a_run_line_toggles_its_fold_without_dispatching() {
        let mut app = aloop_key_fixture();
        let key = "aloop:run:nightly:2026-09-18T09:59:00Z".to_string();
        app.sidebar_selected_work_group = Some(key.clone());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        assert!(app
            .collapsed_sidebar_groups
            .iter()
            .any(|entry| entry.ends_with(&key)));
        // The fold keeps the row selected so Enter toggles it back open.
        assert_eq!(
            app.sidebar_selected_work_group.as_deref(),
            Some(key.as_str())
        );
    }

    #[test]
    fn lifecycle_toasts_name_the_tab_and_destination_shelf() {
        let mut app = sidebar_order_app(false);
        let workspace = &app.state.workspaces[0];
        let pane = workspace.tabs[0].root_pane;
        let name = workspace
            .tab_display_name_from(&app.state.terminals, 0)
            .expect("tab name");
        let target = crate::app::state::SidebarPaneLifecycleTarget::Local(
            crate::app::state::PaneFocusTarget {
                workspace_id: workspace.id.clone(),
                pane_id: pane,
            },
        );
        for shelf in ["Snoozed", "Active", "Settled"] {
            app.show_sidebar_shelf_toast(&target, shelf);
            let toast = app.state.toast.as_ref().expect("lifecycle toast");
            assert_eq!(toast.title, name);
            assert_eq!(toast.context, format!("Moved to {shelf}"));
        }
    }
}
