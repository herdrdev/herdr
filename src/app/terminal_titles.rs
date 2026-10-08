use std::collections::HashSet;

use super::App;
use crate::layout::PaneId;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalTitleChanges {
    pub(crate) raw_changed: bool,
    pub(crate) stripped_changed: bool,
}

impl App {
    pub(crate) fn terminal_title_sidebar_changed(&self, changes: &TerminalTitleChanges) -> bool {
        let config = &self.state.sidebar_agents;
        std::iter::once(&config.rows)
            .chain(config.rows_by_agent.values())
            .flatten()
            .flatten()
            .any(|token| match token.parts().0 {
                crate::config::AgentSidebarToken::TerminalTitle => changes.raw_changed,
                crate::config::AgentSidebarToken::TerminalTitleStripped => changes.stripped_changed,
                _ => false,
            })
    }

    pub(crate) fn sync_pending_terminal_titles(&mut self) -> TerminalTitleChanges {
        let sources = self.render_dirty.pending_terminal_title_sources();
        let changes = self.sync_terminal_titles(&sources);
        if self.terminal_title_sidebar_changed(&changes) {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changes
    }

    pub(crate) fn sync_terminal_titles(
        &mut self,
        sources: &HashSet<PaneId>,
    ) -> TerminalTitleChanges {
        if sources.is_empty() {
            return TerminalTitleChanges::default();
        }

        let mut observations = Vec::with_capacity(sources.len());
        for pane_id in sources {
            let Some((ws_idx, terminal_id)) = self
                .find_pane(*pane_id)
                .map(|(ws_idx, pane)| (ws_idx, pane.attached_terminal_id.clone()))
            else {
                continue;
            };
            let Some(runtime) = self.terminal_runtimes.get(&terminal_id) else {
                continue;
            };
            observations.push((
                ws_idx,
                *pane_id,
                terminal_id,
                runtime.terminal_title(),
                runtime.program_status(),
            ));
        }

        let mut changes = TerminalTitleChanges::default();
        let mut publish = Vec::new();
        for (ws_idx, pane_id, terminal_id, title, program_status) in observations {
            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            let status_changed = terminal.program_status != program_status;
            terminal.program_status = program_status;
            let change = terminal.set_terminal_title(title);
            changes.raw_changed |= change.raw_changed;
            changes.stripped_changed |= change.stripped_changed;
            if change.stripped_changed || status_changed {
                publish.push((ws_idx, pane_id));
            }
        }

        for (ws_idx, pane_id) in publish {
            self.emit_pane_updated(ws_idx, pane_id);
        }

        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    #[tokio::test]
    async fn program_status_pane_snapshot_and_updated_event_preserve_agent_authority() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(
                std::env::temp_dir()
                    .join("herdr-program-status-hook-session.jsonl")
                    .display()
                    .to_string(),
            )
            .unwrap(),
        });
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(2),
        );
        assert!(terminal.hook_authority.is_some());
        let hook_authority = terminal.hook_authority.clone();
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes(
            b"\x1b]7501;state=blocked:kind=permission:app=pi:msg=c2VjcmV0\x07",
        );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let sources = HashSet::from([pane_id]);
        app.sync_terminal_titles(&sources);
        let pane = app.pane_info(0, pane_id).unwrap();
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert_eq!(
            app.state.terminals[&terminal_id].hook_authority,
            hook_authority
        );
        assert_eq!(
            pane.program_status
                .as_ref()
                .unwrap()
                .record
                .as_ref()
                .unwrap()
                .kind,
            Some(crate::api::schema::ProgramStatusKind::Permission)
        );
        let json = serde_json::to_string(&pane).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("c2VjcmV0"));
        assert_eq!(pane_updated_events(&event_hub), 1);
        let events = event_hub.events_after(0);
        let event_pane = events
            .iter()
            .find_map(|(_, event)| {
                if let crate::api::schema::EventData::PaneUpdated { pane } = &event.data {
                    Some(pane)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(event_pane.program_status, pane.program_status);
        app.sync_terminal_titles(&sources);
        assert_eq!(pane_updated_events(&event_hub), 1);
        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]7501;state=clear\x07");
        app.sync_terminal_titles(&sources);
        let cleared = app.pane_info(0, pane_id).unwrap().program_status.unwrap();
        assert_eq!(cleared.revision, 2);
        assert_eq!(cleared.record, None);
        assert_eq!(pane_updated_events(&event_hub), 2);
        // Legacy pane JSON does not need the new optional field.
        let mut legacy = serde_json::to_value(&pane).unwrap();
        legacy.as_object_mut().unwrap().remove("program_status");
        assert_eq!(
            serde_json::from_value::<crate::api::schema::PaneInfo>(legacy)
                .unwrap()
                .program_status,
            None
        );
    }

    #[tokio::test]
    async fn sync_keeps_latest_raw_title_and_emits_only_for_stripped_changes() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.state = AgentState::Working;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes("\x1b]0;⠋ 修复🙂标题\x07".as_bytes());
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let sources = HashSet::from([pane_id]);

        assert_eq!(
            app.sync_terminal_titles(&sources),
            TerminalTitleChanges {
                raw_changed: true,
                stripped_changed: true,
            }
        );
        let pane = app.pane_info(0, pane_id).unwrap();
        assert_eq!(pane.terminal_title.as_deref(), Some("⠋ 修复🙂标题"));
        assert_eq!(pane.terminal_title_stripped.as_deref(), Some("修复🙂标题"));
        assert_eq!(pane.title, None);
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert_eq!(pane.revision, 1);
        let agent = app.collect_agent_infos().pop().unwrap();
        assert_eq!(agent.terminal_title.as_deref(), Some("⠋ 修复🙂标题"));
        assert_eq!(agent.terminal_title_stripped.as_deref(), Some("修复🙂标题"));

        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes("\x1b]2;⠙ 修复🙂标题\x1b\\".as_bytes());
        assert_eq!(
            app.sync_terminal_titles(&sources),
            TerminalTitleChanges {
                raw_changed: true,
                stripped_changed: false,
            }
        );
        let pane = app.pane_info(0, pane_id).unwrap();
        assert_eq!(pane.terminal_title.as_deref(), Some("⠙ 修复🙂标题"));
        assert_eq!(pane.terminal_title_stripped.as_deref(), Some("修复🙂标题"));
        assert_eq!(pane.revision, 1);
        assert_eq!(pane_updated_events(&event_hub), 1);

        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]0;Done reviewing\x07");
        assert!(app.sync_terminal_titles(&sources).stripped_changed);
        assert_eq!(pane_updated_events(&event_hub), 2);

        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]0;\x07");
        assert!(app.sync_terminal_titles(&sources).stripped_changed);
        let pane = app.pane_info(0, pane_id).unwrap();
        assert_eq!(pane.terminal_title, None);
        assert_eq!(pane.terminal_title_stripped, None);
        assert_eq!(pane.revision, 3);
        assert_eq!(pane_updated_events(&event_hub), 3);
    }

    #[tokio::test]
    async fn syncing_pending_titles_preserves_sidebar_render_impact() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub,
        );
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.sidebar_agents.rows = vec![vec![
            crate::config::AgentSidebarToken::TerminalTitleStripped,
        ]];
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes(b"\x1b]0;building\x07");
        app.terminal_runtimes.insert(terminal_id, runtime);
        app.render_dirty.request_terminal_title(pane_id);

        let changes = app.sync_pending_terminal_titles();

        assert!(changes.stripped_changed);
        let render_request = app.render_dirty.take();
        assert!(render_request.generic);
    }

    #[test]
    fn sidebar_redraws_only_for_the_configured_title_form() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub,
        );
        app.state.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![vec![
                crate::config::AgentSidebarToken::TerminalTitleStripped,
            ]],
        );

        let spinner_only = TerminalTitleChanges {
            raw_changed: true,
            ..TerminalTitleChanges::default()
        };
        assert!(!app.terminal_title_sidebar_changed(&spinner_only));
        assert!(app.terminal_title_sidebar_changed(&TerminalTitleChanges {
            stripped_changed: true,
            ..TerminalTitleChanges::default()
        }));

        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![vec![crate::config::AgentSidebarToken::TerminalTitle]],
        );
        assert!(app.terminal_title_sidebar_changed(&spinner_only));
    }

    fn pane_updated_events(event_hub: &crate::api::EventHub) -> usize {
        event_hub
            .events_after(0)
            .iter()
            .filter(|(_, event)| event.event == crate::api::schema::EventKind::PaneUpdated)
            .count()
    }
}
