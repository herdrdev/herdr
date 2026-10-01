use crate::api::schema::{
    AuthorityCatalogInfo, AuthorityCatalogStateInfo, FleetAgentInfo, FleetHostInfo,
    FleetHostStateInfo, FleetSnapshotInfo, ResponseResult,
};
use crate::app::App;
use crate::fleet::{EvidenceSource, HostState};

use super::responses::encode_success;

impl App {
    pub(crate) fn authority_catalog_infos(&self) -> Vec<AuthorityCatalogInfo> {
        self.state
            .fleet_snapshot
            .group_catalogs
            .iter()
            .map(|catalog| AuthorityCatalogInfo {
                connection: catalog.host.clone(),
                authority_id: catalog.authority_id().cloned(),
                state: match catalog.state {
                    crate::fleet::GroupCatalogState::Fresh => AuthorityCatalogStateInfo::Fresh,
                    crate::fleet::GroupCatalogState::Stale => AuthorityCatalogStateInfo::Stale,
                    crate::fleet::GroupCatalogState::IdentityConflict => {
                        AuthorityCatalogStateInfo::IdentityConflict
                    }
                    crate::fleet::GroupCatalogState::Unavailable => {
                        AuthorityCatalogStateInfo::Unavailable
                    }
                },
                snapshot: catalog.snapshot.clone(),
                error: catalog.error.clone(),
            })
            .collect()
    }

    pub(super) fn handle_fleet_list(&self, id: String) -> String {
        let snapshot = &self.state.fleet_snapshot;
        let now_unix_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let hosts = snapshot
            .hosts
            .iter()
            .map(|host| {
                let mut agent_count = 0;
                let agents = host
                    .entries
                    .iter()
                    .filter(|entry| entry.source != EvidenceSource::Host)
                    .map(|entry| {
                        let lifecycle = entry.effective_remote_lifecycle(host.state, now_unix_s);
                        agent_count +=
                            usize::from(entry.counts_as_live_agent(host.state, now_unix_s));
                        FleetAgentInfo {
                            agent_ref: entry.agent_ref.clone(),
                            name: entry.name.clone().unwrap_or_else(|| entry.handle.clone()),
                            title: entry.title.clone(),
                            last_turn_at: entry.last_turn_at.clone(),
                            agent: entry.agent.clone(),
                            state: lifecycle.state_label.to_string(),
                            snoozed_until: lifecycle.snoozed_until,
                            source: entry.source.table_label().to_string(),
                        }
                    })
                    .collect::<Vec<_>>();
                FleetHostInfo {
                    name: host.name.clone(),
                    target: host.target.clone(),
                    local: host.local,
                    session: host.session.clone(),
                    state: match host.state {
                        HostState::Reachable => FleetHostStateInfo::Reachable,
                        HostState::Unreachable => FleetHostStateInfo::Unreachable,
                        HostState::VersionSkew => FleetHostStateInfo::VersionSkew,
                    },
                    agent_count,
                    version: host.version.clone(),
                    protocol: host.protocol,
                    error: host.error.clone(),
                    agents,
                }
            })
            .collect();
        encode_success(
            id,
            ResponseResult::FleetList {
                snapshot: FleetSnapshotInfo {
                    polled: snapshot.polled,
                    refreshed_at_unix_ms: snapshot.refreshed_at_unix_ms,
                    hosts,
                    authority_catalogs: self.authority_catalog_infos(),
                },
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};

    fn app_with_local_agent(self_name: &str) -> App {
        let mut config = crate::config::Config::default();
        config.remote.fleet.self_name = Some(self_name.to_string());
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Codex),
                crate::detect::AgentState::Idle,
            );
        app
    }

    fn app_with_entries(entries: Vec<crate::fleet::FleetRow>) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            aloop: None,
            inbox: None,
            polled: true,
            refreshed_at: None,
            refreshed_at_unix_ms: Some(42),
            config_generation: 0,
            configured_hosts: vec!["ub2".to_string()],
            hosts: vec![crate::fleet::HostSnapshot {
                name: "ub2".to_string(),
                target: "ub2".to_string(),
                local: false,
                session: Some("agents".to_string()),
                socket: None,
                state: HostState::Reachable,
                version: Some("0.8.2".to_string()),
                protocol: Some(crate::protocol::PROTOCOL_VERSION),
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries,
            }],
            group_catalogs: Vec::new(),
        };
        app
    }

    fn render_hosts(app: &App) -> String {
        let area = Rect::new(0, 0, 80, 16);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::dock::hosts::render_hosts(&app.state, frame, area))
            .expect("render hosts");
        let buffer = terminal.backend().buffer();
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn fleet_api_returns_cached_host_and_agent_inventory() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            aloop: None,
            inbox: None,
            polled: true,
            refreshed_at: None,
            refreshed_at_unix_ms: Some(42),
            config_generation: 0,
            configured_hosts: vec!["ub1".to_string(), "ub2".to_string()],
            hosts: ["ub1", "ub2"]
                .into_iter()
                .map(|host| {
                    let mut entry =
                        crate::fleet::FleetRow::test_agent_row_with_id(host, "reviewer", "w1:p1");
                    entry.title = Some(format!("{host} review title"));
                    crate::fleet::HostSnapshot {
                        name: host.to_string(),
                        target: host.to_string(),
                        local: false,
                        session: Some("agents".to_string()),
                        socket: None,
                        state: HostState::Reachable,
                        version: Some("0.8.2".to_string()),
                        protocol: Some(crate::protocol::PROTOCOL_VERSION),
                        error: None,
                        remote_identity: None,
                        sessions: None,
                        reachable: true,
                        last_seen_unix_ms: None,
                        entries: vec![entry],
                    }
                })
                .collect(),
            group_catalogs: Vec::new(),
        };

        let value: serde_json::Value =
            serde_json::from_str(&app.handle_fleet_list("x".into())).expect("fleet response");
        let snapshot = &value["result"]["snapshot"];
        assert_eq!(snapshot["refreshed_at_unix_ms"], 42);
        assert_eq!(snapshot["hosts"][0]["agent_count"], 1);
        assert_eq!(snapshot["hosts"][0]["agents"][0]["agent_ref"], "ub1::w1:p1");
        assert_eq!(
            snapshot["hosts"][0]["agents"][0]["title"],
            "ub1 review title"
        );
        assert_eq!(snapshot["hosts"][1]["agents"][0]["agent_ref"], "ub2::w1:p1");
        assert_ne!(
            snapshot["hosts"][0]["agents"][0]["agent_ref"],
            snapshot["hosts"][1]["agents"][0]["agent_ref"]
        );
    }

    #[test]
    fn local_fleet_alias_keeps_agent_list_identity() {
        let mut app = app_with_local_agent("laptop");
        let agent_response: crate::api::schema::SuccessResponse =
            serde_json::from_str(&app.handle_agent_list("agents".into())).expect("agent list");
        let crate::api::schema::ResponseResult::AgentList { agents } = agent_response.result else {
            panic!("expected agent list response");
        };
        let agent_ref = agents[0].agent_ref.clone().expect("agent list identity");
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            aloop: None,
            polled: true,
            hosts: vec![crate::fleet::HostSnapshot {
                name: "local".into(),
                target: String::new(),
                local: true,
                session: None,
                socket: None,
                state: HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: vec![crate::fleet::FleetRow::test_local_agent_info_row(
                    "local",
                    agents[0].clone(),
                )],
            }],
            ..crate::fleet::Snapshot::default()
        };

        let fleet_response: crate::api::schema::SuccessResponse =
            serde_json::from_str(&app.handle_fleet_list("fleet".into())).expect("fleet list");
        let crate::api::schema::ResponseResult::FleetList { snapshot } = fleet_response.result
        else {
            panic!("expected fleet list response");
        };
        assert_eq!(snapshot.hosts[0].agents[0].agent_ref, agent_ref);
    }

    #[test]
    fn fleet_api_lists_unknown_agents_with_zero_live_count() {
        let app = app_with_entries(vec![
            crate::fleet::FleetRow::test_agent_row_with_state("ub2", "stale-one", "status_unknown"),
            crate::fleet::FleetRow::test_agent_row_with_state("ub2", "stale-two", "status_unknown"),
        ]);

        let value: serde_json::Value =
            serde_json::from_str(&app.handle_fleet_list("x".into())).expect("fleet response");
        let host = &value["result"]["snapshot"]["hosts"][0];
        assert_eq!(host["agent_count"], 0);
        assert_eq!(host["agents"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn fleet_api_and_hosts_dock_agree_on_live_count() {
        let entries = [
            ("idle", "idle"),
            ("working", "working"),
            ("blocked", "blocked"),
            ("blocked-unknown", "blocked_liveness_unknown"),
            ("done", "done"),
            ("stale", "status_unknown"),
        ]
        .into_iter()
        .map(|(name, state)| crate::fleet::FleetRow::test_agent_row_with_state("ub2", name, state))
        .collect();
        let app = app_with_entries(entries);

        let value: serde_json::Value =
            serde_json::from_str(&app.handle_fleet_list("x".into())).expect("fleet response");
        let host = &value["result"]["snapshot"]["hosts"][0];
        let dock = render_hosts(&app);
        assert_eq!(host["agent_count"], 5);
        assert_eq!(host["agents"].as_array().map(Vec::len), Some(6));
        assert!(dock.contains("5 agents"), "{dock}");
        assert!(dock.contains("stale"), "{dock}");
    }

    #[test]
    fn unreachable_host_projects_unknown_without_stale_live_count_in_api_and_dock() {
        let agent: crate::api::schema::AgentInfo = serde_json::from_value(serde_json::json!({
            "terminal_id": "terminal-blocked",
            "name": "blocked",
            "agent": "codex",
            "agent_status": "blocked",
            "gates": [{"n": 1, "label": "Gate", "text": "Answer"}],
            "snoozed_until": u64::MAX,
            "workspace_id": "workspace",
            "tab_id": "tab",
            "pane_id": "pane-blocked",
            "focused": false,
            "revision": 1
        }))
        .expect("valid remote agent");
        let mut app = app_with_entries(vec![crate::fleet::FleetRow::test_agent_info_row(
            "ub2", agent,
        )]);
        app.state.fleet_snapshot.hosts[0].state = HostState::Unreachable;

        let value: serde_json::Value =
            serde_json::from_str(&app.handle_fleet_list("x".into())).expect("fleet response");
        let host = &value["result"]["snapshot"]["hosts"][0];
        assert_eq!(host["agent_count"], 0);
        assert_eq!(host["agents"][0]["state"], "status_unknown");
        assert_eq!(host["agents"][0]["snoozed_until"], u64::MAX);

        let dock = render_hosts(&app);
        assert!(dock.contains("0 agents"), "{dock}");
        assert!(dock.contains("status_unknown"), "{dock}");
        assert!(!dock.contains("  blocked"), "{dock}");
    }
}
