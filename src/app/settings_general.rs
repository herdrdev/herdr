//! The General section of the settings screen.
//!
//! Each row is exactly one config key. The row list is pure data so the
//! renderer, the hit test and the tests all read the same thing, and so a row
//! can never drift away from the key it claims to edit.

use crate::app::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeneralRow {
    ProjectGrouping,
    WorkingRowOpacity,
    AutoSettleFinished,
    AutoSettleInactive,
    SettleAfterDays,
    AutoSettleDone,
    SettleDoneAfterMinutes,
    NudgeResumedAgents,
    AutoNudgeStalledAgents,
    MaxNudgesWithoutHuman,
    HideWhitespace,
    NewThreadWorkspace,
    AddProjectStartDir,
    DefaultPanelSurfaces,
    DeleteConfirmation,
    Notepad,
    NotepadHeight,
    BreakTimer,
    BreakTimerWorkMinutes,
    BreakTimerShortMinutes,
    BreakTimerLongMinutes,
}

/// The inactivity thresholds `enter` cycles through on the days row. A settings
/// screen has no number field, and the useful values are few.
const SETTLE_DAY_LADDER: [u64; 5] = [1, 3, 7, 14, 30];

/// How long a Done thread may sit before it settles. Shorter than the
/// inactivity ladder, because Done is already an answer.
const SETTLE_DONE_MINUTE_LADDER: [u64; 5] = [5, 15, 30, 60, 240];

/// Sidebar rows the notepad may occupy. Small enough to stay a list, large
/// enough to hold a short to-do list without scrolling.
const NOTEPAD_HEIGHT_LADDER: [u64; 6] = [6, 8, 12, 16, 24, 40];

/// Minutes per focus interval, per short break, and per long break.
const WORK_MINUTE_LADDER: [u64; 5] = [20, 25, 30, 45, 50];
const SHORT_BREAK_MINUTE_LADDER: [u64; 3] = [3, 5, 10];
const LONG_BREAK_MINUTE_LADDER: [u64; 3] = [15, 20, 30];
const WORKING_OPACITY_LADDER: [u8; 4] = [100, 75, 50, 25];
const NUDGES_WITHOUT_HUMAN_LADDER: [u32; 4] = [3, 5, 10, 0];

fn next_nudges_without_human(current: u32) -> u32 {
    let next = NUDGES_WITHOUT_HUMAN_LADDER
        .iter()
        .position(|value| *value == current)
        .map_or(0, |index| (index + 1) % NUDGES_WITHOUT_HUMAN_LADDER.len());
    NUDGES_WITHOUT_HUMAN_LADDER[next]
}

/// The next entry above `current`, wrapping to the first one at the top.
fn next_in_ladder(ladder: &[u64], current: u64) -> u64 {
    ladder
        .iter()
        .copied()
        .find(|value| *value > current)
        .unwrap_or(ladder[0])
}

impl GeneralRow {
    pub(crate) const ALL: &'static [Self] = &[
        Self::ProjectGrouping,
        Self::WorkingRowOpacity,
        Self::AutoSettleFinished,
        Self::AutoSettleInactive,
        Self::SettleAfterDays,
        Self::AutoSettleDone,
        Self::SettleDoneAfterMinutes,
        Self::NudgeResumedAgents,
        Self::AutoNudgeStalledAgents,
        Self::MaxNudgesWithoutHuman,
        Self::HideWhitespace,
        Self::NewThreadWorkspace,
        Self::AddProjectStartDir,
        Self::DefaultPanelSurfaces,
        Self::DeleteConfirmation,
        Self::Notepad,
        Self::NotepadHeight,
        Self::BreakTimer,
        Self::BreakTimerWorkMinutes,
        Self::BreakTimerShortMinutes,
        Self::BreakTimerLongMinutes,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ProjectGrouping => "Project grouping",
            Self::WorkingRowOpacity => "Working row opacity",
            Self::AutoSettleFinished => "Auto-settle finished threads",
            Self::AutoSettleInactive => "Auto-settle inactive threads",
            Self::SettleAfterDays => "Days of inactivity before auto-settle",
            Self::AutoSettleDone => "Auto-settle quiet threads",
            Self::SettleDoneAfterMinutes => "Quiet minutes before auto-settle",
            Self::NudgeResumedAgents => "Continue resumed agents",
            Self::AutoNudgeStalledAgents => "Nudge agents after the stale timer",
            Self::MaxNudgesWithoutHuman => "Nudge limit without human input",
            Self::HideWhitespace => "Hide whitespace changes in diff",
            Self::NewThreadWorkspace => "New threads default workspace",
            Self::AddProjectStartDir => "Add project starts in",
            Self::DefaultPanelSurfaces => "Default panel surfaces",
            Self::DeleteConfirmation => "Delete confirmation",
            Self::Notepad => "Sidebar notepad",
            Self::NotepadHeight => "Notepad height",
            Self::BreakTimer => "Break timer",
            Self::BreakTimerWorkMinutes => "Minutes per focus interval",
            Self::BreakTimerShortMinutes => "Minutes per short break",
            Self::BreakTimerLongMinutes => "Minutes per long break",
        }
    }

    /// The dim second line, when the label alone does not say what the key does.
    pub(crate) fn hint(self) -> Option<&'static str> {
        match self {
            Self::ProjectGrouping => Some("combine matching repos across hosts"),
            Self::WorkingRowOpacity => Some("fade blue-dot sessions on every device"),
            Self::AutoSettleDone => Some("file idle agents with no pending work under Settled"),
            Self::NudgeResumedAgents => {
                Some("after a restart, tell an idle resumed agent to carry on")
            }
            Self::AutoNudgeStalledAgents => {
                Some("after both its status report and pane activity go quiet")
            }
            Self::MaxNudgesWithoutHuman => {
                Some("after this many, wait until you type in the pane; off means no limit")
            }
            Self::Notepad => Some("markdown notes under the workspace list"),
            Self::BreakTimer => Some("countdown in the sidebar footer, break prompt on expiry"),
            _ => None,
        }
    }

    /// `(section, key)` in the config file.
    pub(crate) fn config_key(self) -> (&'static str, &'static str) {
        match self {
            Self::ProjectGrouping => ("ui", "combine_repos_across_hosts"),
            Self::WorkingRowOpacity => ("ui", "working_row_opacity_percent"),
            Self::AutoSettleFinished => ("session", "auto_settle_finished"),
            Self::AutoSettleInactive => ("session", "auto_settle_inactive"),
            Self::SettleAfterDays => ("session", "settle_after_days"),
            Self::AutoSettleDone => ("session", "auto_settle_done"),
            Self::SettleDoneAfterMinutes => ("session", "settle_done_after_minutes"),
            Self::NudgeResumedAgents => ("session", "nudge_resumed_agents"),
            Self::AutoNudgeStalledAgents => ("session", "auto_nudge_stalled_agents"),
            Self::MaxNudgesWithoutHuman => ("session", "max_nudges_without_human"),
            Self::HideWhitespace => ("ui", "hide_whitespace_in_diff"),
            Self::NewThreadWorkspace => ("ui", "new_thread_workspace"),
            Self::AddProjectStartDir => ("ui", "add_project_start_dir"),
            Self::DefaultPanelSurfaces => ("panel", "default_surfaces"),
            Self::DeleteConfirmation => ("ui", "confirm_close"),
            Self::Notepad => ("notepad", "enabled"),
            Self::NotepadHeight => ("notepad", "height"),
            Self::BreakTimer => ("pomodoro", "enabled"),
            Self::BreakTimerWorkMinutes => ("pomodoro", "work_minutes"),
            Self::BreakTimerShortMinutes => ("pomodoro", "short_break_minutes"),
            Self::BreakTimerLongMinutes => ("pomodoro", "long_break_minutes"),
        }
    }

    /// The bracketed value on the right of the row.
    pub(crate) fn value(self, state: &AppState) -> String {
        let on_off = |value: bool| if value { "on" } else { "off" }.to_string();
        let minutes = |value: std::time::Duration| value.as_secs().div_ceil(60).to_string();
        match self {
            Self::ProjectGrouping => on_off(state.combine_repos_across_hosts),
            Self::WorkingRowOpacity => format!("{}%", state.working_row_opacity_percent),
            Self::AutoSettleFinished => on_off(state.auto_settle_finished),
            Self::AutoSettleInactive => on_off(state.auto_settle_inactive),
            Self::SettleAfterDays => state
                .settle_after
                .as_secs()
                .div_ceil(24 * 60 * 60)
                .to_string(),
            Self::AutoSettleDone => on_off(state.auto_settle_done),
            Self::SettleDoneAfterMinutes => minutes(state.settle_done_after),
            Self::NudgeResumedAgents => on_off(state.nudge_resumed_agents),
            Self::AutoNudgeStalledAgents => on_off(state.auto_nudge_stalled_agents),
            Self::MaxNudgesWithoutHuman => {
                if state.max_nudges_without_human == 0 {
                    "off".to_string()
                } else {
                    state.max_nudges_without_human.to_string()
                }
            }
            Self::HideWhitespace => on_off(state.dock_diff_ignore_whitespace),
            Self::NewThreadWorkspace => state.new_thread_workspace.label().to_string(),
            Self::AddProjectStartDir => {
                if state.add_project_start_dir.trim().is_empty() {
                    "last used".to_string()
                } else {
                    state.add_project_start_dir.clone()
                }
            }
            Self::DefaultPanelSurfaces => {
                if state.dock_default_surfaces.is_empty() {
                    "none".to_string()
                } else {
                    state
                        .dock_default_surfaces
                        .iter()
                        .map(|surface| surface.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
            Self::DeleteConfirmation => on_off(state.confirm_close),
            Self::Notepad => on_off(state.notepad.enabled),
            Self::NotepadHeight => state.notepad.height.to_string(),
            Self::BreakTimer => on_off(state.pomodoro.enabled),
            Self::BreakTimerWorkMinutes => minutes(state.pomodoro.work),
            Self::BreakTimerShortMinutes => minutes(state.pomodoro.short_break),
            Self::BreakTimerLongMinutes => minutes(state.pomodoro.long_break),
        }
    }

    /// A path is not something a two-column list can edit; the row still names
    /// the key so the operator knows what to change.
    pub(crate) fn is_editable(self) -> bool {
        !matches!(self, Self::AddProjectStartDir | Self::DefaultPanelSurfaces)
    }
}

/// Config edits shared by settings rows and compact UI toggles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigEdit {
    Notifications {
        delivery: crate::config::ToastDelivery,
        sound_enabled: bool,
    },
    Bool {
        section: &'static str,
        key: &'static str,
        value: bool,
    },
    Integer {
        section: &'static str,
        key: &'static str,
        value: u64,
    },
    Text {
        section: &'static str,
        key: &'static str,
        value: String,
    },
}

/// The next value for `row`, or `None` when the row is not editable here.
pub(crate) fn cycle_general_row(state: &AppState, row: GeneralRow) -> Option<ConfigEdit> {
    let (section, key) = row.config_key();
    let toggle = |value: bool| {
        Some(ConfigEdit::Bool {
            section,
            key,
            value: !value,
        })
    };
    match row {
        GeneralRow::ProjectGrouping => toggle(state.combine_repos_across_hosts),
        GeneralRow::WorkingRowOpacity => {
            let current = state.working_row_opacity_percent;
            let index = WORKING_OPACITY_LADDER
                .iter()
                .position(|value| *value == current)
                .unwrap_or(0);
            Some(ConfigEdit::Integer {
                section,
                key,
                value: u64::from(
                    WORKING_OPACITY_LADDER[(index + 1) % WORKING_OPACITY_LADDER.len()],
                ),
            })
        }
        GeneralRow::AutoSettleFinished => toggle(state.auto_settle_finished),
        GeneralRow::AutoSettleInactive => toggle(state.auto_settle_inactive),
        GeneralRow::AutoSettleDone => toggle(state.auto_settle_done),
        GeneralRow::SettleDoneAfterMinutes => Some(ConfigEdit::Integer {
            section,
            key,
            value: next_in_ladder(
                &SETTLE_DONE_MINUTE_LADDER,
                state.settle_done_after.as_secs().div_ceil(60),
            ),
        }),
        GeneralRow::NudgeResumedAgents => toggle(state.nudge_resumed_agents),
        GeneralRow::AutoNudgeStalledAgents => toggle(state.auto_nudge_stalled_agents),
        GeneralRow::MaxNudgesWithoutHuman => Some(ConfigEdit::Integer {
            section,
            key,
            value: u64::from(next_nudges_without_human(state.max_nudges_without_human)),
        }),
        GeneralRow::HideWhitespace => toggle(state.dock_diff_ignore_whitespace),
        GeneralRow::DeleteConfirmation => toggle(state.confirm_close),
        GeneralRow::Notepad => toggle(state.notepad.enabled),
        GeneralRow::BreakTimer => toggle(state.pomodoro.enabled),
        GeneralRow::NotepadHeight => Some(ConfigEdit::Integer {
            section,
            key,
            value: next_in_ladder(&NOTEPAD_HEIGHT_LADDER, u64::from(state.notepad.height)),
        }),
        GeneralRow::BreakTimerWorkMinutes => Some(ConfigEdit::Integer {
            section,
            key,
            value: next_in_ladder(
                &WORK_MINUTE_LADDER,
                state.pomodoro.work.as_secs().div_ceil(60),
            ),
        }),
        GeneralRow::BreakTimerShortMinutes => Some(ConfigEdit::Integer {
            section,
            key,
            value: next_in_ladder(
                &SHORT_BREAK_MINUTE_LADDER,
                state.pomodoro.short_break.as_secs().div_ceil(60),
            ),
        }),
        GeneralRow::BreakTimerLongMinutes => Some(ConfigEdit::Integer {
            section,
            key,
            value: next_in_ladder(
                &LONG_BREAK_MINUTE_LADDER,
                state.pomodoro.long_break.as_secs().div_ceil(60),
            ),
        }),
        GeneralRow::SettleAfterDays => {
            let current = state.settle_after.as_secs().div_ceil(24 * 60 * 60);
            Some(ConfigEdit::Integer {
                section,
                key,
                value: next_in_ladder(&SETTLE_DAY_LADDER, current),
            })
        }
        GeneralRow::NewThreadWorkspace => {
            let next = match state.new_thread_workspace {
                crate::config::NewThreadWorkspaceConfig::CurrentCheckout => {
                    crate::config::NewThreadWorkspaceConfig::NewWorktree
                }
                crate::config::NewThreadWorkspaceConfig::NewWorktree => {
                    crate::config::NewThreadWorkspaceConfig::CurrentCheckout
                }
            };
            Some(ConfigEdit::Text {
                section,
                key,
                value: next.as_str().to_string(),
            })
        }
        GeneralRow::AddProjectStartDir | GeneralRow::DefaultPanelSurfaces => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_general_row_names_a_distinct_config_key() {
        let mut keys = GeneralRow::ALL
            .iter()
            .map(|row| row.config_key())
            .collect::<Vec<_>>();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(total, 21);
        assert_eq!(keys.len(), total);
    }

    #[test]
    fn general_row_values_read_live_state() {
        let mut state = AppState::test_new();
        assert_eq!(GeneralRow::ProjectGrouping.value(&state), "off");
        assert_eq!(GeneralRow::DeleteConfirmation.value(&state), "on");
        assert_eq!(
            GeneralRow::NewThreadWorkspace.value(&state),
            "current checkout"
        );
        assert_eq!(GeneralRow::AddProjectStartDir.value(&state), "last used");
        assert_eq!(GeneralRow::DefaultPanelSurfaces.value(&state), "none");
        assert_eq!(GeneralRow::WorkingRowOpacity.value(&state), "100%");

        state.combine_repos_across_hosts = true;
        state.add_project_start_dir = "~/Repos".into();
        assert_eq!(GeneralRow::ProjectGrouping.value(&state), "on");
        assert_eq!(GeneralRow::AddProjectStartDir.value(&state), "~/Repos");
    }

    #[test]
    fn working_row_opacity_cycles_in_decreasing_steps_and_wraps() {
        let mut state = AppState::test_new();
        for (current, next) in [(100, 75), (75, 50), (50, 25), (25, 100)] {
            state.working_row_opacity_percent = current;
            assert_eq!(
                cycle_general_row(&state, GeneralRow::WorkingRowOpacity),
                Some(ConfigEdit::Integer {
                    section: "ui",
                    key: "working_row_opacity_percent",
                    value: next,
                })
            );
        }
    }

    #[test]
    fn toggles_flip_and_the_day_ladder_wraps() {
        let mut state = AppState::test_new();
        assert_eq!(
            cycle_general_row(&state, GeneralRow::ProjectGrouping),
            Some(ConfigEdit::Bool {
                section: "ui",
                key: "combine_repos_across_hosts",
                value: true
            })
        );

        state.settle_after = std::time::Duration::from_secs(3 * 24 * 60 * 60);
        assert_eq!(
            cycle_general_row(&state, GeneralRow::SettleAfterDays),
            Some(ConfigEdit::Integer {
                section: "session",
                key: "settle_after_days",
                value: 7
            })
        );
        state.settle_after = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        assert_eq!(
            cycle_general_row(&state, GeneralRow::SettleAfterDays),
            Some(ConfigEdit::Integer {
                section: "session",
                key: "settle_after_days",
                value: 1
            })
        );
    }

    #[test]
    fn the_notepad_and_break_timer_rows_edit_their_own_sections() {
        let mut state = AppState::test_new();
        assert_eq!(GeneralRow::Notepad.value(&state), "off");
        assert_eq!(GeneralRow::BreakTimer.value(&state), "off");
        assert_eq!(GeneralRow::BreakTimerWorkMinutes.value(&state), "25");
        assert_eq!(GeneralRow::BreakTimerShortMinutes.value(&state), "5");
        assert_eq!(GeneralRow::BreakTimerLongMinutes.value(&state), "20");

        assert_eq!(
            cycle_general_row(&state, GeneralRow::Notepad),
            Some(ConfigEdit::Bool {
                section: "notepad",
                key: "enabled",
                value: true
            })
        );
        assert_eq!(
            cycle_general_row(&state, GeneralRow::BreakTimer),
            Some(ConfigEdit::Bool {
                section: "pomodoro",
                key: "enabled",
                value: true
            })
        );

        state.notepad.enabled = true;
        assert_eq!(GeneralRow::Notepad.value(&state), "on");
    }

    #[test]
    fn the_minute_and_height_ladders_wrap() {
        let mut state = AppState::test_new();
        assert_eq!(
            cycle_general_row(&state, GeneralRow::BreakTimerWorkMinutes),
            Some(ConfigEdit::Integer {
                section: "pomodoro",
                key: "work_minutes",
                value: 30
            })
        );

        state.pomodoro.work = std::time::Duration::from_secs(50 * 60);
        assert_eq!(
            cycle_general_row(&state, GeneralRow::BreakTimerWorkMinutes),
            Some(ConfigEdit::Integer {
                section: "pomodoro",
                key: "work_minutes",
                value: 20
            })
        );

        state.notepad.height = 16;
        assert_eq!(
            cycle_general_row(&state, GeneralRow::NotepadHeight),
            Some(ConfigEdit::Integer {
                section: "notepad",
                key: "height",
                value: 24
            })
        );

        // The ladder wraps at its tallest step, not at the old 16-row end.
        state.notepad.height = 40;
        assert_eq!(
            cycle_general_row(&state, GeneralRow::NotepadHeight),
            Some(ConfigEdit::Integer {
                section: "notepad",
                key: "height",
                value: 6
            })
        );
    }

    #[test]
    fn the_resume_nudge_row_toggles_the_session_key() {
        let mut state = AppState::test_new();
        assert_eq!(GeneralRow::NudgeResumedAgents.value(&state), "on");
        assert_eq!(
            cycle_general_row(&state, GeneralRow::NudgeResumedAgents),
            Some(ConfigEdit::Bool {
                section: "session",
                key: "nudge_resumed_agents",
                value: false
            })
        );

        state.nudge_resumed_agents = false;
        assert_eq!(GeneralRow::NudgeResumedAgents.value(&state), "off");
    }

    #[test]
    fn the_stalled_agent_nudge_row_toggles_the_session_key() {
        let mut state = AppState::test_new();
        assert_eq!(GeneralRow::AutoNudgeStalledAgents.value(&state), "off");
        assert_eq!(
            cycle_general_row(&state, GeneralRow::AutoNudgeStalledAgents),
            Some(ConfigEdit::Bool {
                section: "session",
                key: "auto_nudge_stalled_agents",
                value: true
            })
        );

        state.auto_nudge_stalled_agents = true;
        assert_eq!(GeneralRow::AutoNudgeStalledAgents.value(&state), "on");
    }

    #[test]
    fn the_nudges_without_human_row_cycles_the_session_key() {
        let mut state = AppState::test_new();
        assert_eq!(GeneralRow::MaxNudgesWithoutHuman.value(&state), "5");
        for value in [10, 0, 3, 5] {
            assert_eq!(
                cycle_general_row(&state, GeneralRow::MaxNudgesWithoutHuman),
                Some(ConfigEdit::Integer {
                    section: "session",
                    key: "max_nudges_without_human",
                    value,
                })
            );
            state.max_nudges_without_human = value as u32;
            assert_eq!(
                GeneralRow::MaxNudgesWithoutHuman.value(&state),
                if value == 0 {
                    "off".to_string()
                } else {
                    value.to_string()
                }
            );
        }
    }

    #[test]
    fn array_and_path_rows_are_not_editable_in_place() {
        let state = AppState::test_new();
        for row in [
            GeneralRow::AddProjectStartDir,
            GeneralRow::DefaultPanelSurfaces,
        ] {
            assert!(!row.is_editable());
            assert_eq!(cycle_general_row(&state, row), None);
        }
    }
}
