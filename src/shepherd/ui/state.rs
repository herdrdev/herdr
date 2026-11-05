use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::agents::{ShepherdAgentsAction, ShepherdAgentsHits, ShepherdAgentsSectionState};
use super::components::EmptyStateHits;
use super::git::{ShepherdGitAction, ShepherdGitHits, ShepherdGitSectionState};
use super::patterns::{ShepherdPatternsAction, ShepherdPatternsHits, ShepherdPatternsSectionState};
use super::runs::{ShepherdRunsAction, ShepherdRunsHits, ShepherdRunsSectionState};
use super::skills::{ShepherdSkillsAction, ShepherdSkillsHits, ShepherdSkillsSectionState};
use super::tasks::{ShepherdTasksAction, ShepherdTasksHits, ShepherdTasksSectionState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShepherdSectionTab {
    Agents,
    Tasks,
    Runs,
    Patterns,
    Skills,
    Git,
    Events,
}

impl ShepherdSectionTab {
    pub const ALL: [Self; 7] = [
        Self::Agents,
        Self::Tasks,
        Self::Runs,
        Self::Patterns,
        Self::Skills,
        Self::Git,
        Self::Events,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agentes",
            Self::Tasks => "Tareas",
            Self::Runs => "Runs",
            Self::Patterns => "Patrones",
            Self::Skills => "Skills",
            Self::Git => "Git",
            Self::Events => "Eventos",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Agents => "1",
            Self::Tasks => "2",
            Self::Runs => "3",
            Self::Patterns => "4",
            Self::Skills => "5",
            Self::Git => "6",
            Self::Events => "7",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Agents => 0,
            Self::Tasks => 1,
            Self::Runs => 2,
            Self::Patterns => 3,
            Self::Skills => 4,
            Self::Git => 5,
            Self::Events => 6,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index % 7 {
            0 => Self::Agents,
            1 => Self::Tasks,
            2 => Self::Runs,
            3 => Self::Patterns,
            4 => Self::Skills,
            5 => Self::Git,
            _ => Self::Events,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShepherdFocusedPanel {
    #[default]
    List,
    Detail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdSectionAction {
    Agents(ShepherdAgentsAction),
    Tasks(ShepherdTasksAction),
    Runs(ShepherdRunsAction),
    Patterns(ShepherdPatternsAction),
    Skills(ShepherdSkillsAction),
    Git(ShepherdGitAction),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdSectionOutcome {
    Handled,
    Action(ShepherdSectionAction),
    Close,
    Ignored,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdSectionHits {
    pub tabs: Vec<(Rect, ShepherdSectionTab)>,
    pub close_button: Option<Rect>,
    pub agents_hits: Option<ShepherdAgentsHits>,
    pub tasks_hits: Option<ShepherdTasksHits>,
    pub runs_hits: Option<ShepherdRunsHits>,
    pub patterns_hits: Option<ShepherdPatternsHits>,
    pub skills_hits: Option<ShepherdSkillsHits>,
    pub git_hits: Option<ShepherdGitHits>,
    pub empty_state_hits: Option<EmptyStateHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdSectionState {
    pub active_tab: ShepherdSectionTab,
    pub focused_panel: ShepherdFocusedPanel,
    pub agents: ShepherdAgentsSectionState,
    pub tasks: ShepherdTasksSectionState,
    pub runs: ShepherdRunsSectionState,
    pub patterns: ShepherdPatternsSectionState,
    pub skills: ShepherdSkillsSectionState,
    pub git: ShepherdGitSectionState,
    pub status_message: Option<String>,
    pub loading: bool,
}

impl Default for ShepherdSectionState {
    fn default() -> Self {
        Self {
            active_tab: ShepherdSectionTab::Agents,
            focused_panel: ShepherdFocusedPanel::List,
            agents: ShepherdAgentsSectionState::default(),
            tasks: ShepherdTasksSectionState::default(),
            runs: ShepherdRunsSectionState::default(),
            patterns: ShepherdPatternsSectionState::default(),
            skills: ShepherdSkillsSectionState::default(),
            git: ShepherdGitSectionState::default(),
            status_message: None,
            loading: false,
        }
    }
}

impl ShepherdSectionState {
    pub fn select_tab(&mut self, tab: ShepherdSectionTab) {
        self.active_tab = tab;
        self.status_message = None;
    }

    pub fn next_tab(&mut self) {
        let next = (self.active_tab.index() + 1) % 7;
        self.select_tab(ShepherdSectionTab::from_index(next));
    }

    pub fn prev_tab(&mut self) {
        let prev = (self.active_tab.index() + 6) % 7;
        self.select_tab(ShepherdSectionTab::from_index(prev));
    }

    pub fn has_active_sub_modal_or_stripe(&self) -> bool {
        match self.active_tab {
            ShepherdSectionTab::Agents => self.agents.has_sub_action(),
            ShepherdSectionTab::Tasks => {
                self.tasks.form.is_some()
                    || self.tasks.import_gh.is_some()
                    || self.tasks.confirm_archive.is_some()
            }
            ShepherdSectionTab::Runs => {
                self.runs.launch_form.is_some() || self.runs.confirm_cancel.is_some()
            }
            ShepherdSectionTab::Patterns => self.patterns.has_sub_action(),
            ShepherdSectionTab::Skills => self.skills.has_sub_action(),
            ShepherdSectionTab::Git => self.git.has_sub_action(),
            _ => false,
        }
    }

    pub fn cancel_active_sub_modal_or_stripe(&mut self) -> bool {
        match self.active_tab {
            ShepherdSectionTab::Agents => self.agents.cancel_sub_action(),
            ShepherdSectionTab::Tasks => self.tasks.cancel_sub_action(),
            ShepherdSectionTab::Runs => self.runs.cancel_sub_action(),
            ShepherdSectionTab::Patterns => self.patterns.cancel_sub_action(),
            ShepherdSectionTab::Skills => self.skills.cancel_sub_action(),
            ShepherdSectionTab::Git => self.git.cancel_sub_action(),
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> ShepherdSectionOutcome {
        // Esc: if sub-modal or stripe active, cancel it. Otherwise close section.
        if key.code == KeyCode::Esc {
            if self.cancel_active_sub_modal_or_stripe() {
                return ShepherdSectionOutcome::Handled;
            }
            return ShepherdSectionOutcome::Close;
        }

        // Direct tab shortcut keys 1-7 (when no form is typing text)
        if !self.has_active_sub_modal_or_stripe() {
            match key.code {
                KeyCode::Char('1') => {
                    self.select_tab(ShepherdSectionTab::Agents);
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Char('2') => {
                    self.select_tab(ShepherdSectionTab::Tasks);
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Char('3') => {
                    self.select_tab(ShepherdSectionTab::Runs);
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Char('4') => {
                    self.select_tab(ShepherdSectionTab::Patterns);
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Char('5') => {
                    self.select_tab(ShepherdSectionTab::Skills);
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Char('6') => {
                    self.select_tab(ShepherdSectionTab::Git);
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Char('7') => {
                    self.select_tab(ShepherdSectionTab::Events);
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.prev_tab();
                    return ShepherdSectionOutcome::Handled;
                }
                KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.next_tab();
                    return ShepherdSectionOutcome::Handled;
                }
                _ => {}
            }
        }

        // Forward to active tab
        match self.active_tab {
            ShepherdSectionTab::Agents => {
                let (handled, act) = self.agents.handle_key(key);
                if let Some(a) = act {
                    ShepherdSectionOutcome::Action(ShepherdSectionAction::Agents(a))
                } else if handled {
                    ShepherdSectionOutcome::Handled
                } else {
                    ShepherdSectionOutcome::Ignored
                }
            }
            ShepherdSectionTab::Tasks => {
                let (handled, act) = self.tasks.handle_key(key);
                if let Some(a) = act {
                    ShepherdSectionOutcome::Action(ShepherdSectionAction::Tasks(a))
                } else if handled {
                    ShepherdSectionOutcome::Handled
                } else {
                    ShepherdSectionOutcome::Ignored
                }
            }
            ShepherdSectionTab::Runs => {
                let (handled, act) = self.runs.handle_key(key);
                if let Some(a) = act {
                    ShepherdSectionOutcome::Action(ShepherdSectionAction::Runs(a))
                } else if handled {
                    ShepherdSectionOutcome::Handled
                } else {
                    ShepherdSectionOutcome::Ignored
                }
            }
            ShepherdSectionTab::Patterns => {
                let (handled, act) = self.patterns.handle_key(key);
                if let Some(a) = act {
                    ShepherdSectionOutcome::Action(ShepherdSectionAction::Patterns(a))
                } else if handled {
                    ShepherdSectionOutcome::Handled
                } else {
                    ShepherdSectionOutcome::Ignored
                }
            }
            ShepherdSectionTab::Skills => {
                let (handled, act) = self.skills.handle_key(key);
                if let Some(a) = act {
                    ShepherdSectionOutcome::Action(ShepherdSectionAction::Skills(a))
                } else if handled {
                    ShepherdSectionOutcome::Handled
                } else {
                    ShepherdSectionOutcome::Ignored
                }
            }
            ShepherdSectionTab::Git => {
                let (handled, act) = self.git.handle_key(key);
                if let Some(a) = act {
                    ShepherdSectionOutcome::Action(ShepherdSectionAction::Git(a))
                } else if handled {
                    ShepherdSectionOutcome::Handled
                } else {
                    ShepherdSectionOutcome::Ignored
                }
            }
            _ => ShepherdSectionOutcome::Ignored,
        }
    }
}
