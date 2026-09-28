use std::time::{Duration, Instant};

use super::App;

pub(super) const GOALS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

impl App {
    pub(crate) fn schedule_goals_refresh(&mut self, now: Instant) -> bool {
        if !self.state.goals.enabled {
            return false;
        }

        let workspace_index = self.state.active.unwrap_or(self.state.selected);
        let cwd = self
            .state
            .workspaces
            .get(workspace_index)
            .and_then(|workspace| {
                workspace.focused_cwd_from(&self.state.terminals, &self.terminal_runtimes)
            });
        let focus_changed = self.goals_focused_cwd != cwd;
        // Only a reset of already-visible goals needs a redraw.
        let mut visible_reset = false;
        if focus_changed {
            visible_reset = !matches!(self.state.goals.load, crate::goals::GoalsLoad::Missing);
            self.goals_focused_cwd = cwd.clone();
            self.goals_observation = None;
            self.goals_refresh_in_flight = false;
            self.goals_refresh_generation = self.goals_refresh_generation.wrapping_add(1);
            self.next_goals_refresh = now;
            self.state.goals.load = crate::goals::GoalsLoad::Missing;
        }
        let Some(cwd) = cwd else {
            return visible_reset;
        };
        if self.goals_refresh_in_flight || now < self.next_goals_refresh {
            return visible_reset;
        }
        self.next_goals_refresh = now + GOALS_REFRESH_INTERVAL;

        self.goals_refresh_in_flight = true;
        self.goals_refresh_generation = self.goals_refresh_generation.wrapping_add(1);
        let generation = self.goals_refresh_generation;
        let previous = self.goals_observation.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let refresh = crate::goals::refresh_from_cwd(&cwd, previous.as_ref());
            let _ = tx.blocking_send(crate::events::AppEvent::GoalsRefreshed {
                generation,
                refresh,
            });
        });
        visible_reset
    }

    pub(super) fn finish_goals_refresh(
        &mut self,
        generation: u64,
        refresh: crate::goals::GoalsRefresh,
    ) -> bool {
        if generation != self.goals_refresh_generation {
            return false;
        }
        self.goals_refresh_in_flight = false;
        self.goals_observation = Some(refresh.observation);
        let Some(load) = refresh.load else {
            return false;
        };
        if self.state.goals.load == load {
            return false;
        }
        self.state.goals.load = load;
        true
    }

    pub(super) fn apply_goals_config(&mut self, config: &crate::config::GoalsPanelConfig) {
        if self.state.goals.enabled == config.enabled {
            return;
        }
        self.state.goals.enabled = config.enabled;
        self.state.goals.load = crate::goals::GoalsLoad::Missing;
        self.goals_observation = None;
        self.goals_focused_cwd = None;
        self.goals_refresh_in_flight = false;
        self.goals_refresh_generation = self.goals_refresh_generation.wrapping_add(1);
        self.next_goals_refresh = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn losing_focused_cwd_clears_cached_goals_and_cancels_refresh() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.goals.enabled = true;
        app.goals_focused_cwd = Some("/previous".into());
        app.goals_refresh_in_flight = true;
        app.state.goals.load = crate::goals::GoalsLoad::Malformed("previous".into());
        let generation = app.goals_refresh_generation;

        assert!(app.schedule_goals_refresh(Instant::now()));
        assert_eq!(app.state.goals.load, crate::goals::GoalsLoad::Missing);
        assert_eq!(app.goals_focused_cwd, None);
        assert_eq!(app.goals_observation, None);
        assert!(!app.goals_refresh_in_flight);
        assert_ne!(app.goals_refresh_generation, generation);
    }
}
