use std::time::{Duration, Instant};

use super::App;
use crate::terminal_theme::HostAppearance;

#[cfg(test)]
mod tests;

impl App {
    #[cfg(test)]
    pub(crate) fn set_host_terminal_appearance_state(
        &mut self,
        appearance: Option<HostAppearance>,
        explicit: bool,
    ) -> bool {
        self.set_host_terminal_state(self.state.host_terminal_theme, appearance, explicit)
    }

    #[cfg(test)]
    pub(crate) fn set_host_terminal_theme(
        &mut self,
        theme: crate::terminal_theme::TerminalTheme,
    ) -> bool {
        self.set_host_terminal_state(
            theme,
            self.state.host_terminal_appearance,
            self.state.host_terminal_appearance_explicit,
        )
    }

    pub(crate) fn set_host_terminal_state(
        &mut self,
        theme: crate::terminal_theme::TerminalTheme,
        appearance: Option<HostAppearance>,
        explicit: bool,
    ) -> bool {
        let theme_changed = theme != self.state.host_terminal_theme;
        if theme_changed {
            self.state.host_terminal_theme = theme;
            self.apply_host_terminal_theme_to_panes();
        }

        let previous = self.state.host_terminal_appearance;
        let appearance_changed =
            previous != appearance || self.state.host_terminal_appearance_explicit != explicit;
        if appearance_changed {
            self.state.host_terminal_appearance = appearance;
            self.state.host_terminal_appearance_explicit = explicit;
            if previous.is_some() && appearance.is_some() {
                if previous != appearance {
                    // The matching OSC 11 arrives after the appearance report's round trip.
                    self.pending_host_appearance_deadline
                        .get_or_insert(Instant::now() + Duration::from_millis(750));
                }
            } else {
                self.pending_host_appearance_deadline = None;
                self.apply_host_terminal_appearance_to_panes();
            }
        }

        // A foreground switch must install both facts before releasing the outgoing hold.
        let released = self.release_host_appearance_if_ready(Instant::now());
        let repainted = appearance_changed && self.refresh_effective_app_theme();
        theme_changed || released || repainted
    }

    pub(crate) fn pane_host_terminal_appearance(&self) -> Option<HostAppearance> {
        if self.pending_host_appearance_deadline.is_some() {
            // New panes must describe the background available during the hold.
            self.state
                .host_terminal_theme
                .background
                .map(|color| color.inferred_appearance())
                .or(self.pane_host_appearance)
        } else {
            self.state.host_terminal_appearance
        }
    }

    pub(crate) fn release_host_appearance_if_ready(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.pending_host_appearance_deadline else {
            return false;
        };
        let matches = self
            .state
            .host_terminal_theme
            .background
            .is_some_and(|color| {
                Some(color.inferred_appearance()) == self.state.host_terminal_appearance
            });
        if !matches && now < deadline {
            return false;
        }
        self.pending_host_appearance_deadline = None;
        self.apply_host_terminal_appearance_to_panes();
        true
    }

    pub(super) fn refresh_effective_app_theme(&mut self) -> bool {
        let (palette, theme_name) = super::resolve_effective_theme(
            &self.state.theme_runtime,
            self.state.host_terminal_appearance,
        );
        if self.state.theme_name == theme_name && self.state.palette == palette {
            return false;
        }
        self.state.theme_name = theme_name;
        self.state.palette = palette;
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
        true
    }

    fn apply_host_terminal_appearance_to_panes(&mut self) {
        self.pane_host_appearance = self.state.host_terminal_appearance;
        for runtime in self.terminal_runtimes.values() {
            runtime.apply_host_terminal_appearance(self.state.host_terminal_appearance);
        }
    }

    fn apply_host_terminal_theme_to_panes(&self) {
        for runtime in self.terminal_runtimes.values() {
            runtime.apply_host_terminal_theme(self.state.host_terminal_theme);
        }

        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }
}
