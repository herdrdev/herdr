//! Bottom-docked agent launcher. Dispatch planning stays in `HomeState`; this
//! module owns only the dock's field focus and picker presentation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{
    home::{HomePicker, HomeState, HomeWorkspace},
    state::AppState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnDockField {
    Project,
    Host,
    Profile,
    Model,
    Effort,
    Worktree,
    Prompt,
}

impl SpawnDockField {
    const ALL: [Self; 7] = [
        Self::Project,
        Self::Host,
        Self::Profile,
        Self::Model,
        Self::Effort,
        Self::Worktree,
        Self::Prompt,
    ];

    fn step(self, backwards: bool) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0);
        let next = if backwards {
            (index + Self::ALL.len() - 1) % Self::ALL.len()
        } else {
            (index + 1) % Self::ALL.len()
        };
        Self::ALL[next]
    }

    fn picker(self) -> Option<HomePicker> {
        match self {
            Self::Project => Some(HomePicker::Project),
            Self::Profile => Some(HomePicker::Agent),
            Self::Model => Some(HomePicker::Model),
            Self::Effort => Some(HomePicker::Effort),
            Self::Worktree => Some(HomePicker::Workspace),
            Self::Host | Self::Prompt => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnDockState {
    pub(crate) home: HomeState,
    pub(crate) auto_host: bool,
    pub(crate) focus: SpawnDockField,
    pub(crate) picker: Option<HomePicker>,
    pub(crate) filter: String,
    pub(crate) restored_age: Option<String>,
    pub(crate) project_agent_counts: Vec<usize>,
}

impl SpawnDockState {
    pub(crate) fn new(home: HomeState) -> Self {
        Self {
            home,
            auto_host: true,
            focus: SpawnDockField::Project,
            picker: None,
            filter: String::new(),
            restored_age: None,
            project_agent_counts: Vec::new(),
        }
    }

    pub(crate) fn from_draft(
        mut home: HomeState,
        draft: crate::client::presentation::SpawnDockDraft,
    ) -> Self {
        home.prompt = draft.prompt;
        if home
            .projects()
            .iter()
            .any(|project| project.id == draft.project)
        {
            home.set_project(&draft.project);
            if let Some(path) = home.repo_options().first().map(|repo| repo.path.clone()) {
                home.set_directory(path);
            }
        }
        let auto_host = draft.host == "auto";
        if !auto_host
            && home
                .machines()
                .iter()
                .any(|machine| machine.name == draft.host)
        {
            home.set_machine(&draft.host);
        }
        if home
            .profiles()
            .iter()
            .any(|profile| profile.id == draft.profile)
        {
            home.set_profile(&draft.profile);
        }
        if home
            .model_options()
            .iter()
            .any(|model| model.id == draft.model)
        {
            home.set_model(&draft.model);
        }
        if draft
            .effort
            .as_ref()
            .is_some_and(|effort| home.effort_options().contains(effort))
        {
            home.set_effort(draft.effort);
        }
        home.workspace = match draft.worktree.as_str() {
            "new" => HomeWorkspace::NewWorktree,
            "previous" => draft
                .worktree_path
                .map(HomeWorkspace::PreviousWorktree)
                .unwrap_or(HomeWorkspace::CurrentCheckout),
            _ => HomeWorkspace::CurrentCheckout,
        };
        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|now| now.as_secs().saturating_sub(draft.saved_at_unix))
            .unwrap_or_default();
        Self {
            home,
            auto_host,
            focus: SpawnDockField::Project,
            picker: None,
            filter: String::new(),
            restored_age: Some(format_age(age)),
            project_agent_counts: Vec::new(),
        }
    }

    pub(crate) fn draft(&self) -> crate::client::presentation::SpawnDockDraft {
        let (worktree, worktree_path) = match &self.home.workspace {
            HomeWorkspace::CurrentCheckout => ("current".into(), None),
            HomeWorkspace::NewWorktree => ("new".into(), None),
            HomeWorkspace::PreviousWorktree(path) => ("previous".into(), Some(path.clone())),
        };
        crate::client::presentation::SpawnDockDraft {
            prompt: self.home.prompt.clone(),
            project: self.home.project_id().to_string(),
            host: if self.auto_host {
                "auto".to_string()
            } else {
                self.home
                    .machine()
                    .map(|machine| machine.name.clone())
                    .unwrap_or_default()
            },
            profile: self.home.profile_id().to_string(),
            model: self.home.model.clone(),
            effort: self.home.effort.clone(),
            worktree,
            worktree_path,
            saved_at_unix: crate::provider_usage::now_unix().unwrap_or_default().max(0) as u64,
        }
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        if self.picker.is_some() {
            let text: String = text
                .chars()
                .filter(|character| !character.is_control())
                .collect();
            if text.is_empty() {
                return;
            }
            self.filter.push_str(&text);
            self.home.picker_selected = 0;
        } else if self.focus == SpawnDockField::Prompt {
            let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            let text: String = normalized
                .chars()
                .filter(|character| *character == '\n' || !character.is_control())
                .collect();
            if text.is_empty() {
                return;
            }
            self.home.prompt.push_str(&text);
        }
    }

    pub(crate) fn handle_key(
        &mut self,
        event: KeyEvent,
        _auto_host: Option<&str>,
    ) -> SpawnDockAction {
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);
        if event.code == KeyCode::Backspace && event.modifiers == KeyModifiers::CONTROL {
            return SpawnDockAction::Clear;
        }
        if self.picker.is_some() {
            return self.handle_picker(event);
        }
        match event.code {
            KeyCode::Esc => SpawnDockAction::Close,
            KeyCode::Tab => {
                self.focus = self.focus.step(false);
                SpawnDockAction::Consumed
            }
            KeyCode::BackTab => {
                self.focus = self.focus.step(true);
                SpawnDockAction::Consumed
            }
            KeyCode::Enter if self.focus == SpawnDockField::Prompt && shift => {
                SpawnDockAction::Spawn
            }
            KeyCode::Enter if self.focus == SpawnDockField::Prompt => {
                self.home.append_prompt('\n');
                SpawnDockAction::Consumed
            }
            KeyCode::Enter => {
                if let Some(picker) = self.focus.picker() {
                    self.picker = Some(picker);
                    self.filter.clear();
                    self.home.picker_selected = 0;
                }
                SpawnDockAction::Consumed
            }
            KeyCode::Left | KeyCode::Right if self.focus == SpawnDockField::Host => {
                let backwards = event.code == KeyCode::Left;
                if let Some(current) = self.home.machine().map(|machine| machine.name.clone()) {
                    let machines = self.home.machines();
                    let next_name = self
                        .home
                        .machines()
                        .iter()
                        .position(|machine| machine.name == current)
                        .map(|index| {
                            if backwards {
                                (index + machines.len() - 1) % machines.len()
                            } else {
                                (index + 1) % machines.len()
                            }
                        })
                        .and_then(|index| machines.get(index))
                        .map(|machine| machine.name.clone());
                    if let Some(next_name) = next_name {
                        self.home.set_machine(&next_name);
                        self.auto_host = false;
                    }
                }
                SpawnDockAction::Consumed
            }
            KeyCode::Left | KeyCode::Right => {
                self.cycle_value(event.code == KeyCode::Left);
                SpawnDockAction::Consumed
            }
            KeyCode::Backspace if self.focus == SpawnDockField::Prompt => {
                self.home.backspace_prompt();
                SpawnDockAction::Consumed
            }
            KeyCode::Char(ch)
                if self.focus == SpawnDockField::Prompt
                    && !event.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                self.home.append_prompt(ch);
                SpawnDockAction::Consumed
            }
            _ => SpawnDockAction::Consumed,
        }
    }

    fn cycle_value(&mut self, backwards: bool) {
        let (picker, len, current) = match self.focus {
            SpawnDockField::Project => (
                HomePicker::Project,
                self.home.projects().len(),
                self.home
                    .projects()
                    .iter()
                    .position(|p| p.id == self.home.project_id())
                    .unwrap_or(0),
            ),
            SpawnDockField::Profile => (
                HomePicker::Agent,
                self.home.profiles().len(),
                self.home
                    .profiles()
                    .iter()
                    .position(|p| p.id == self.home.profile_id())
                    .unwrap_or(0),
            ),
            SpawnDockField::Model => (
                HomePicker::Model,
                self.home.model_options().len(),
                self.home
                    .model_options()
                    .iter()
                    .position(|m| m.id == self.home.model)
                    .unwrap_or(0),
            ),
            SpawnDockField::Effort => (
                HomePicker::Effort,
                self.home.effort_options().len(),
                self.home
                    .effort_options()
                    .iter()
                    .position(|e| Some(e) == self.home.effort.as_ref())
                    .unwrap_or(0),
            ),
            SpawnDockField::Worktree => (
                HomePicker::Workspace,
                self.home.workspace_options().len(),
                self.home
                    .workspace_options()
                    .iter()
                    .position(|w| w == &self.home.workspace)
                    .unwrap_or(0),
            ),
            SpawnDockField::Host | SpawnDockField::Prompt => return,
        };
        if len > 0 {
            self.picker = Some(picker);
            self.home.picker_selected = if backwards {
                (current + len - 1) % len
            } else {
                (current + 1) % len
            };
            self.accept_picker();
        }
    }

    fn handle_picker(&mut self, event: KeyEvent) -> SpawnDockAction {
        match event.code {
            KeyCode::Esc => {
                self.picker = None;
                self.filter.clear();
            }
            KeyCode::Up if event.modifiers.is_empty() => {
                self.move_picker_selection(-1);
            }
            KeyCode::Down if event.modifiers.is_empty() => {
                self.move_picker_selection(1);
            }
            KeyCode::Backspace if event.modifiers.is_empty() => {
                self.filter.pop();
                self.home.picker_selected = 0;
            }
            KeyCode::Char(ch)
                if !event.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.filter.push(ch);
                self.home.picker_selected = 0;
            }
            KeyCode::Enter if event.modifiers.is_empty() => self.accept_picker(),
            KeyCode::Tab => {
                self.picker = None;
                self.focus = self.focus.step(false);
            }
            KeyCode::BackTab => {
                self.picker = None;
                self.focus = self.focus.step(true);
            }
            _ => {}
        }
        SpawnDockAction::Consumed
    }

    fn accept_picker(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let selected = self
            .filtered_indices(picker)
            .get(self.home.picker_selected)
            .copied()
            .unwrap_or(self.home.picker_selected);
        match picker {
            HomePicker::Project => {
                if let Some(project) = self.home.projects().get(selected) {
                    let id = project.id.clone();
                    self.home.set_project(&id);
                    if let Some(path) = self
                        .home
                        .repo_options()
                        .first()
                        .map(|repo| repo.path.clone())
                    {
                        self.home.set_directory(path);
                    }
                }
            }
            HomePicker::Agent => {
                if let Some(profile) = self.home.profiles().get(selected) {
                    let id = profile.id.clone();
                    self.home.set_profile(&id);
                }
            }
            HomePicker::Model => {
                if let Some(model) = self.home.model_options().get(selected) {
                    let id = model.id.clone();
                    self.home.set_model(&id);
                }
            }
            HomePicker::Effort => {
                if let Some(effort) = self.home.effort_options().get(selected) {
                    let effort = effort.clone();
                    self.home.set_effort(Some(effort));
                }
            }
            HomePicker::Workspace => {
                if let Some(workspace) = self.home.workspace_options().get(selected) {
                    let workspace = workspace.clone();
                    self.home.workspace = workspace;
                }
            }
            _ => {}
        }
        self.filter.clear();
    }

    fn move_picker_selection(&mut self, delta: i32) {
        let Some(picker) = self.picker else {
            return;
        };
        let len = self.filtered_indices(picker).len();
        if len == 0 {
            self.home.picker_selected = 0;
            return;
        }
        self.home.picker_selected = if delta < 0 {
            self.home
                .picker_selected
                .saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.home.picker_selected.saturating_add(delta as usize)
        }
        .min(len - 1);
    }

    fn filtered_indices(&self, picker: HomePicker) -> Vec<usize> {
        let query = self.filter.to_lowercase();
        let labels: Vec<String> = match picker {
            HomePicker::Project => self
                .home
                .projects()
                .iter()
                .enumerate()
                .map(|(index, p)| {
                    let path = p
                        .repos
                        .first()
                        .map(|repo| repo.path.display().to_string())
                        .unwrap_or_default();
                    let count = self
                        .project_agent_counts
                        .get(index)
                        .copied()
                        .unwrap_or_default();
                    format!("{} {path} agents:{count}", p.label)
                })
                .collect(),
            HomePicker::Agent => self
                .home
                .profiles()
                .iter()
                .map(|p| format!("{} {} {}", p.label, p.id, self.home.profile_models(p.agent)))
                .collect(),
            HomePicker::Model => self
                .home
                .model_options()
                .iter()
                .map(|m| m.display_name.clone())
                .collect(),
            HomePicker::Effort => self.home.effort_options().to_vec(),
            HomePicker::Workspace => self
                .home
                .workspace_options()
                .iter()
                .map(|w| w.label())
                .collect(),
            _ => Vec::new(),
        };
        labels
            .iter()
            .enumerate()
            .filter_map(|(i, label)| label.to_lowercase().contains(&query).then_some(i))
            .collect()
    }
}

fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnDockAction {
    Consumed,
    Close,
    Clear,
    Spawn,
}

impl AppState {
    pub(crate) fn open_spawn_dock(&mut self) {
        if self.spawn_dock.is_none() {
            let mut home = self.new_home_state();
            if let Some(path) = home.repo_options().first().map(|repo| repo.path.clone()) {
                home.set_directory(path);
            }
            let entries = super::worktrees::worktree_repo_root(&home.directory)
                .and_then(|root| super::worktrees::worktree_entries_for_repo(&root, |_| None).ok())
                .unwrap_or_default();
            home.set_workspace_options(super::home::home_workspace_options_from_entries(&entries));
            let auto_host = self.least_loaded_spawn_host();
            if let Some(draft) = self.sidebar_presentation.load_spawn_dock_draft() {
                if draft.host == "auto" {
                    if let Some(name) = auto_host {
                        home.set_machine(&name);
                    }
                }
                self.spawn_dock = Some(SpawnDockState::from_draft(home, draft));
            } else {
                if let Some(name) = auto_host {
                    home.set_machine(&name);
                }
                self.spawn_dock = Some(SpawnDockState::new(home));
            }
            if let Some(dock) = self.spawn_dock.as_mut() {
                dock.project_agent_counts = dock
                    .home
                    .projects()
                    .iter()
                    .map(|project| {
                        self.workspaces
                            .iter()
                            .filter(|workspace| {
                                project
                                    .repos
                                    .iter()
                                    .any(|repo| workspace.identity_cwd.starts_with(&repo.path))
                            })
                            .map(|workspace| {
                                workspace
                                    .tabs
                                    .iter()
                                    .flat_map(|tab| tab.panes.values())
                                    .filter(|pane| {
                                        self.terminals.get(&pane.attached_terminal_id).is_some_and(
                                            |terminal| terminal.effective_agent_label().is_some(),
                                        )
                                    })
                                    .count()
                            })
                            .sum()
                    })
                    .collect();
            }
        }
        self.home = None;
        self.inbox = None;
    }

    pub(crate) fn least_loaded_spawn_host(&self) -> Option<String> {
        ["ub1", "ub2"]
            .into_iter()
            .filter_map(|name| {
                let configured = self
                    .machines
                    .iter()
                    .find(|machine| machine.name == name && !machine.is_local())?;
                let count = self
                    .fleet_snapshot
                    .hosts
                    .iter()
                    .find(|host| host.name == configured.name)
                    .map(|host| {
                        let now =
                            crate::provider_usage::now_unix().unwrap_or_default().max(0) as u64;
                        host.entries
                            .iter()
                            .filter(|entry| entry.source != crate::fleet::EvidenceSource::Host)
                            .filter(|entry| entry.counts_as_live_agent(host.state, now))
                            .count()
                    })
                    .unwrap_or(usize::MAX);
                Some((configured.name.clone(), count))
            })
            .min_by_key(|(_, count)| *count)
            .map(|(name, _)| name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn dock_open_preserves_pane_surface_and_uses_home_dispatch_settings() {
        let mut app = AppState::test_new();
        app.open_spawn_dock();
        assert!(app.spawn_dock.is_some());
        assert!(app.home.is_none());
        assert!(matches!(
            app.terminal_area_surface(),
            crate::app::state::TerminalAreaSurface::Empty
        ));
    }

    #[test]
    fn draft_round_trips_launch_settings_and_prompt() {
        let app = AppState::test_new();
        let mut home = app.new_home_state();
        home.prompt = "review this change".into();
        let original = SpawnDockState::new(home);
        let saved = original.draft();
        let restored = SpawnDockState::from_draft(app.new_home_state(), saved);
        assert_eq!(restored.home.prompt, "review this change");
        assert_eq!(restored.home.profile_id(), original.home.profile_id());
        assert!(restored.restored_age.is_some());
    }

    #[test]
    fn prompt_uses_enter_for_newline_and_shift_enter_for_spawn() {
        let app = AppState::test_new();
        let mut dock = SpawnDockState::new(app.new_home_state());
        dock.focus = SpawnDockField::Prompt;
        assert_eq!(
            dock.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()), None),
            SpawnDockAction::Consumed
        );
        assert_eq!(dock.home.prompt, "\n");
        assert_eq!(
            dock.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), None),
            SpawnDockAction::Spawn
        );
    }

    #[test]
    fn pasted_prompt_preserves_normalized_newlines_and_picker_stays_single_line() {
        let app = AppState::test_new();
        let mut dock = SpawnDockState::new(app.new_home_state());
        dock.focus = SpawnDockField::Prompt;
        dock.insert_text("first\r\nsecond\rthird\n");
        assert_eq!(dock.home.prompt, "first\nsecond\nthird\n");

        dock.picker = Some(HomePicker::Project);
        dock.insert_text("one\r\ntwo\rthree");
        assert_eq!(dock.filter, "onetwothree");
    }
}
