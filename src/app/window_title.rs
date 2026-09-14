//! Outer terminal window title.
//!
//! Herdr is a terminal emulator, so `OSC 0`/`OSC 2` written by a pane stops at
//! Herdr and never reaches the terminal Herdr itself runs in. Without this the
//! host window title keeps whatever the shell or `ssh` left behind, which is
//! what window managers show in tab and group bars.
//!
//! The title is rendered on the server so `{hostname}` names the host the panes
//! actually live on, not the machine a thin remote client runs on. The server
//! pushes the result to the foreground client, which writes the `OSC 0`.

use super::App;
use crate::config::{WindowTitlePart, WindowTitleTemplate, WindowTitleToken};

impl App {
    pub(crate) fn configure_window_title(&mut self, template: &str) {
        self.window_title_template =
            WindowTitleTemplate::parse(template)
                .ok()
                .flatten()
                .map(|template| {
                    // Resolve the hostname once here rather than per render.
                    let hostname = if template.uses(WindowTitleToken::Hostname) {
                        crate::platform::hostname().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    (template, hostname)
                });
    }

    /// Whether `ui.window_title` asks Herdr to own the outer terminal title at
    /// all. When it does not, Herdr leaves whatever the shell or `ssh` set.
    pub(crate) fn window_title_configured(&self) -> bool {
        self.window_title_template.is_some()
    }

    /// Whether the title depends on the focused pane's own terminal title, which
    /// is the one input that arrives through PTY parsing rather than app state.
    pub(crate) fn window_title_uses_terminal_title(&self) -> bool {
        self.window_title_template
            .as_ref()
            .is_some_and(|(template, _)| template.uses(WindowTitleToken::TerminalTitle))
    }

    /// Renders the configured outer window title, or `None` when window titles
    /// are disabled or every token resolved empty.
    pub(crate) fn window_title(&self) -> Option<String> {
        let target = self.state.active.and_then(|workspace_index| {
            self.state
                .workspaces
                .get(workspace_index)
                .map(|workspace| (workspace_index, workspace.active_tab_index()))
        });
        self.window_title_for_target(target)
    }

    pub(crate) fn window_title_for(
        &self,
        workspace_index: usize,
        tab_index: usize,
    ) -> Option<String> {
        self.window_title_for_target(Some((workspace_index, tab_index)))
    }

    fn window_title_for_target(&self, target: Option<(usize, usize)>) -> Option<String> {
        let (template, hostname) = self.window_title_template.as_ref()?;
        let workspace =
            target.and_then(|(workspace_index, _)| self.state.workspaces.get(workspace_index));
        let tab = target.and_then(|(workspace_index, tab_index)| {
            self.state
                .workspaces
                .get(workspace_index)?
                .tabs
                .get(tab_index)
        });
        let terminal = tab
            .and_then(|tab| tab.terminal_id(tab.layout.focused()))
            .and_then(|terminal_id| self.state.terminals.get(terminal_id));

        let mut title = String::new();
        for part in template.parts() {
            match part {
                WindowTitlePart::Literal(literal) => title.push_str(literal),
                WindowTitlePart::Token(WindowTitleToken::Hostname) => title.push_str(hostname),
                WindowTitlePart::Token(WindowTitleToken::Workspace) => {
                    if let Some(workspace) = workspace {
                        title.push_str(
                            &workspace.display_name_from_terminals(&self.state.terminals),
                        );
                    }
                }
                WindowTitlePart::Token(WindowTitleToken::Tab) => {
                    if let Some(name) = target.and_then(|(workspace_index, tab_index)| {
                        self.state
                            .workspaces
                            .get(workspace_index)?
                            .tab_display_name(tab_index)
                    }) {
                        title.push_str(&name);
                    }
                }
                WindowTitlePart::Token(WindowTitleToken::Pane) => {
                    if let Some(label) =
                        terminal.and_then(|terminal| terminal.manual_label.as_deref())
                    {
                        title.push_str(label);
                    }
                }
                WindowTitlePart::Token(WindowTitleToken::TerminalTitle) => {
                    if let Some(terminal_title) =
                        terminal.and_then(|terminal| terminal.terminal_title_stripped())
                    {
                        title.push_str(&terminal_title);
                    }
                }
            }
        }

        Some(title)
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::config::Config;
    use crate::workspace::Workspace;

    fn test_app() -> App {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub,
        );
        app.state.workspaces = vec![Workspace::test_new("herd")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app
    }

    #[test]
    fn renders_workspace_and_tab_names() {
        let mut app = test_app();
        app.configure_window_title("{workspace}/{tab}");

        assert_eq!(app.window_title().as_deref(), Some("herd/1"));

        app.state.workspaces[0].tabs[0].custom_name = Some("build".into());
        assert_eq!(app.window_title().as_deref(), Some("herd/build"));
    }

    #[test]
    fn renders_focused_pane_label_and_terminal_title() {
        let mut app = test_app();
        app.configure_window_title("{pane}|{terminal_title}");

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("focused terminal");
        terminal.manual_label = Some("api".into());
        terminal.set_terminal_title(Some("⠋ building".into()));

        assert_eq!(app.window_title().as_deref(), Some("api|building"));
    }

    #[test]
    fn empty_template_disables_window_titles() {
        let mut app = test_app();
        app.configure_window_title("");

        assert_eq!(app.window_title(), None);
    }

    #[test]
    fn invalid_template_disables_window_titles() {
        let mut app = test_app();
        app.configure_window_title("{nope}");

        assert_eq!(app.window_title(), None);
    }

    #[test]
    fn unset_tokens_render_empty() {
        let mut app = test_app();
        app.configure_window_title("[{pane}]");

        assert_eq!(app.window_title().as_deref(), Some("[]"));
    }
}
