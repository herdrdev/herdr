use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

use crate::planning_lock::{Dialog, DialogFlow, PlanningLock, UNLOCK_MINUTES};

use super::App;

impl App {
    fn planning_lock_now() -> u64 {
        crate::app::settled::unix_seconds(std::time::SystemTime::now())
    }

    fn planning_lock_tabs(&self) -> Vec<(String, String)> {
        let mut tabs = Vec::new();
        for (workspace_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                let Some(id) = self.public_tab_id(workspace_idx, tab_idx) else {
                    continue;
                };
                let label = format!(
                    "{} · {}",
                    workspace.custom_name.as_deref().unwrap_or("Workspace"),
                    tab.custom_name
                        .clone()
                        .unwrap_or_else(|| (tab_idx + 1).to_string())
                );
                tabs.push((id, label));
            }
        }
        tabs
    }

    fn focused_planning_lock_tab(&self) -> Option<String> {
        let workspace_idx = self.state.active?;
        let workspace = self.state.workspaces.get(workspace_idx)?;
        self.public_tab_id(workspace_idx, workspace.active_tab_index())
    }

    pub(super) fn open_planning_lock_menu(&mut self) {
        let flow = if self.state.planning_lock.configured() {
            DialogFlow::Manage
        } else {
            DialogFlow::SetupChooseDiscussionTab
        };
        let mut dialog = Dialog::new(flow);
        let tabs = self.planning_lock_tabs();
        dialog.selected_tab = self
            .focused_planning_lock_tab()
            .and_then(|focused| tabs.iter().position(|(id, _)| id == &focused))
            .unwrap_or(0);
        self.state.planning_lock_dialog = Some(dialog);
    }

    fn persist_planning_lock(&mut self, candidate: PlanningLock) -> Result<(), String> {
        let path = crate::config::config_dir().join(crate::planning_lock::CONFIG_FILE_NAME);
        candidate
            .persist(&path)
            .map_err(|error| error.to_string())?;
        self.state.planning_lock = candidate;
        self.state.mark_session_dirty();
        Ok(())
    }

    pub(super) fn handle_planning_lock_text(&mut self, text: &str, paste: bool) -> bool {
        let now = Self::planning_lock_now();
        let locked = self.state.planning_lock.is_locked(now);
        if !locked && self.state.planning_lock_dialog.is_none() {
            return false;
        }
        if locked
            && self.state.planning_lock_dialog.is_none()
            && self
                .focused_planning_lock_tab()
                .is_some_and(|tab| self.state.planning_lock.permits_tab(&tab, now))
        {
            return false;
        }
        if let Some(dialog) = self.state.planning_lock_dialog.as_mut() {
            if paste || text.chars().count() != 1 || text.chars().any(char::is_control) {
                dialog.error = Some("Paste is disabled — type it".into());
            } else {
                dialog.input.push_str(text);
                dialog.error = None;
            }
        }
        true
    }

    pub(super) fn handle_planning_lock_key(&mut self, key: KeyEvent) -> bool {
        let now = Self::planning_lock_now();
        let locked = self.state.planning_lock.is_locked(now);
        if !locked && self.state.planning_lock_dialog.is_none() {
            return false;
        }
        let ctrl_alt_u = matches!(key.code, KeyCode::Char('u' | 'U'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::ALT);
        if self.state.planning_lock_dialog.is_none() {
            if ctrl_alt_u {
                self.state.planning_lock_dialog = Some(Dialog::new(DialogFlow::UnlockPassword));
                return true;
            }
            return !self
                .focused_planning_lock_tab()
                .is_some_and(|tab| self.state.planning_lock.permits_tab(&tab, now));
        }

        if !matches!(key.code, KeyCode::Char(_)) {
            self.planning_lock_key_burst.clear();
        }

        let tabs = self.planning_lock_tabs();
        let Some(mut dialog) = self.state.planning_lock_dialog.take() else {
            return true;
        };
        let mut keep_dialog = true;
        match key.code {
            KeyCode::Esc if !locked => keep_dialog = false,
            KeyCode::Esc => {
                keep_dialog = false;
                if let Some(discussion_tab) = self.state.planning_lock.discussion_tab_id() {
                    if let Some((workspace_idx, tab_idx)) =
                        self.state.workspaces.iter().enumerate().find_map(
                            |(workspace_idx, workspace)| {
                                workspace.tabs.iter().enumerate().find_map(|(tab_idx, _)| {
                                    (self.public_tab_id(workspace_idx, tab_idx).as_deref()
                                        == Some(discussion_tab))
                                    .then_some((workspace_idx, tab_idx))
                                })
                            },
                        )
                    {
                        self.state.active = Some(workspace_idx);
                        self.state.workspaces[workspace_idx].switch_tab(tab_idx);
                        self.state.selected = workspace_idx;
                    }
                }
            }
            KeyCode::Backspace => {
                dialog.input.pop();
                dialog.error = None;
            }
            KeyCode::Up | KeyCode::Char('k')
                if matches!(
                    dialog.flow,
                    DialogFlow::SetupChooseDiscussionTab | DialogFlow::ChooseDiscussionTab
                ) =>
            {
                dialog.selected_tab = dialog.selected_tab.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j')
                if matches!(
                    dialog.flow,
                    DialogFlow::SetupChooseDiscussionTab | DialogFlow::ChooseDiscussionTab
                ) =>
            {
                dialog.selected_tab = dialog
                    .selected_tab
                    .saturating_add(1)
                    .min(tabs.len().saturating_sub(1));
            }
            KeyCode::Left | KeyCode::Up if dialog.flow == DialogFlow::ChooseDuration => {
                dialog.selected_tab = dialog.selected_tab.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Down if dialog.flow == DialogFlow::ChooseDuration => {
                dialog.selected_tab = dialog
                    .selected_tab
                    .saturating_add(1)
                    .min(UNLOCK_MINUTES.len() - 1);
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                dialog.input.push(ch);
                dialog.error = None;
                let now = Instant::now();
                self.planning_lock_key_burst
                    .retain(|at| now.saturating_duration_since(*at) <= Duration::from_millis(10));
                self.planning_lock_key_burst.push_back(now);
                if self.planning_lock_key_burst.len() >= 3 {
                    for _ in 0..3 {
                        dialog.input.pop();
                    }
                    dialog.error = Some("Paste is disabled — type it".into());
                    self.planning_lock_key_burst.clear();
                }
            }
            KeyCode::Enter => {
                let flow = dialog.flow;
                let input = std::mem::take(&mut dialog.input);
                let selected_tab = dialog.selected_tab;
                let saved_password = dialog.password.clone();
                match flow {
                    DialogFlow::SetupChooseDiscussionTab => {
                        if tabs.is_empty() {
                            dialog.error =
                                Some("Create a tab before enabling planning lock".into());
                        } else {
                            dialog.flow = DialogFlow::SetupPassword;
                        }
                    }
                    DialogFlow::SetupPassword => {
                        if input.chars().count() < crate::planning_lock::MIN_PASSWORD_CHARS {
                            dialog.error = Some("Use at least 24 characters".into());
                        } else {
                            dialog.password = Some(input);
                            dialog.flow = DialogFlow::SetupConfirmPassword;
                        }
                    }
                    DialogFlow::SetupConfirmPassword => {
                        if saved_password.as_deref() != Some(input.as_str()) {
                            dialog.error = Some("Passwords do not match".into());
                            dialog.password = None;
                            dialog.flow = DialogFlow::SetupPassword;
                        } else if let Some((tab_id, _)) = tabs.get(selected_tab) {
                            let mut candidate = self.state.planning_lock.clone();
                            match candidate.configure(&input, tab_id) {
                                Ok(()) => match self.persist_planning_lock(candidate) {
                                    Ok(()) => keep_dialog = false,
                                    Err(error) => dialog.error = Some(error),
                                },
                                Err(error) => dialog.error = Some(error.to_string()),
                            }
                        }
                    }
                    DialogFlow::UnlockPassword | DialogFlow::DisablePassword => {
                        let candidate = self.state.planning_lock.clone();
                        match candidate.verify_password(&input) {
                            Ok(()) if flow == DialogFlow::UnlockPassword => {
                                dialog.password = Some(input);
                                dialog.flow = DialogFlow::ChooseDuration;
                                dialog.selected_tab = 0;
                            }
                            Ok(()) => {
                                dialog.password = Some(input);
                                dialog.flow = DialogFlow::DisableConfirmation;
                            }
                            Err(error) => dialog.error = Some(error.to_string()),
                        }
                    }
                    DialogFlow::ChooseDuration => {
                        if let Some(minutes) = UNLOCK_MINUTES.get(selected_tab).copied() {
                            let mut candidate = self.state.planning_lock.clone();
                            match candidate.unlock(
                                dialog.password.as_deref().unwrap_or_default(),
                                minutes,
                                now,
                            ) {
                                Ok(()) => match self.persist_planning_lock(candidate) {
                                    Ok(()) => dialog.flow = DialogFlow::Manage,
                                    Err(error) => dialog.error = Some(error),
                                },
                                Err(error) => dialog.error = Some(error.to_string()),
                            }
                        }
                    }
                    DialogFlow::Manage => match input.as_str() {
                        "u" => dialog.flow = DialogFlow::UnlockPassword,
                        "d" => dialog.flow = DialogFlow::ChooseDiscussionTab,
                        "x" => dialog.flow = DialogFlow::DisablePassword,
                        _ => {
                            dialog.error =
                                Some("u unlock · d change discussion tab · x turn off".into())
                        }
                    },
                    DialogFlow::ChooseDiscussionTab => {
                        if let Some((tab_id, _)) = tabs.get(selected_tab) {
                            let mut candidate = self.state.planning_lock.clone();
                            match candidate.set_discussion_session(tab_id, now) {
                                Ok(()) => match self.persist_planning_lock(candidate) {
                                    Ok(()) => dialog.flow = DialogFlow::Manage,
                                    Err(error) => dialog.error = Some(error),
                                },
                                Err(error) => dialog.error = Some(error.to_string()),
                            }
                        }
                    }
                    DialogFlow::DisableConfirmation => {
                        let mut candidate = self.state.planning_lock.clone();
                        match candidate.disable(&saved_password.unwrap_or_default(), &input) {
                            Ok(()) => match self.persist_planning_lock(candidate) {
                                Ok(()) => keep_dialog = false,
                                Err(error) => dialog.error = Some(error),
                            },
                            Err(error) => dialog.error = Some(error.to_string()),
                        }
                    }
                }
            }
            _ => self.planning_lock_key_burst.clear(),
        }
        if keep_dialog {
            self.state.planning_lock_dialog = Some(dialog);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn planning_lock_footer_opens_setup_flow_when_disabled() {
        let mut app = app();
        app.open_planning_lock_menu();
        assert_eq!(
            app.state
                .planning_lock_dialog
                .as_ref()
                .map(|dialog| dialog.flow),
            Some(DialogFlow::SetupChooseDiscussionTab)
        );
    }

    #[test]
    fn unlock_dialog_rejects_paste_and_multicharacter_text_commits() {
        let mut app = app();
        app.state.planning_lock_dialog = Some(Dialog::new(DialogFlow::UnlockPassword));
        assert!(app.handle_planning_lock_text("pasted password", true));
        assert!(app
            .state
            .planning_lock_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.input.is_empty()
                && dialog.error.as_deref() == Some("Paste is disabled — type it")));
        assert!(app.handle_planning_lock_text("burst", false));
        assert!(app
            .state
            .planning_lock_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.input.is_empty()));
    }

    #[test]
    fn unlock_dialog_rejects_a_wrong_typed_password() {
        let mut app = app();
        app.state
            .planning_lock
            .configure("a planning password longer than twenty four", "tab-1")
            .expect("configure lock");
        app.state.planning_lock_dialog = Some(Dialog::new(DialogFlow::UnlockPassword));
        for character in "wrong".chars() {
            app.handle_planning_lock_key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            ));
        }
        app.handle_planning_lock_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(app
            .state
            .planning_lock_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.error.as_deref() == Some("incorrect password")));
    }

    #[test]
    fn unlock_dialog_rejects_a_three_key_burst_as_paste() {
        let mut app = app();
        app.state.planning_lock_dialog = Some(Dialog::new(DialogFlow::UnlockPassword));
        for character in ['a', 'b', 'c'] {
            app.handle_planning_lock_key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            ));
        }
        let dialog = app
            .state
            .planning_lock_dialog
            .as_ref()
            .expect("dialog remains open");
        assert!(dialog.input.is_empty());
        assert_eq!(dialog.error.as_deref(), Some("Paste is disabled — type it"));
    }

    #[test]
    fn escape_closes_the_locked_dialog() {
        let mut app = app();
        let mut workspace = crate::workspace::Workspace::test_new("planning-lock");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        let discussion_tab = app.public_tab_id(0, 0).expect("discussion tab");
        app.state.workspaces[0].switch_tab(1);
        app.state
            .planning_lock
            .configure(
                "a planning password longer than twenty four",
                &discussion_tab,
            )
            .expect("configure lock");
        app.state.planning_lock_dialog = Some(Dialog::new(DialogFlow::UnlockPassword));
        assert!(app.handle_planning_lock_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.state.planning_lock_dialog.is_none());
        assert_eq!(app.state.workspaces[0].active_tab_index(), 0);
    }
}
