//! Push notifications for agents on federation peers (#246).
//!
//! The coordinator learns remote agent state two ways: the event relay patches
//! the [`FederationStore`] within about a second, and the 5 s `agent.list` poll
//! refreshes it for every peer (the only source for a peer without
//! `events_v2`). Both writes bump the store's revision and wake the app loop,
//! which compares each reachable remote pane with the state it last saw and
//! pushes each real transition once through the same builder and delivery as a
//! local agent: needs you, finished, and (from a relayed `pane.exited`) died.
//!
//! Because the comparison is against state, not events, a duplicate relayed
//! event, a reconnect resync, a poll repeating the relay's answer, or a peer
//! that drops out and comes back unchanged sends nothing.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::api::federation_store::{FederationStore, Reachability, RemotePaneEvent};
use crate::api::schema::{AgentInfo, AgentStatus, WorkspaceInfo};
use crate::app::App;
use crate::push::{PushKind, PushNotification};

/// How long a remote agent's finish that came with a release waits for the
/// pane's exit. A peer sends release, status and exit back to back, so an exit
/// arrives well within this; locally a dying agent pushes only "exited", and
/// the wait keeps a remote one from also pushing "finished".
const RELEASE_EXIT_GRACE: Duration = Duration::from_secs(1);

/// What the app loop last saw of the remote agents.
#[derive(Debug, Default)]
pub(crate) struct RemotePushTracker {
    /// Store revision last compared; `None` before the first read.
    revision: Option<u64>,
    /// Peers seen reachable at least once. Their first reachable view is the
    /// silent baseline; a pane that appears on one of them later notifies like
    /// any other transition.
    peers: HashSet<String>,
    /// Last seen state per remote pane (alias-qualified id).
    panes: HashMap<String, SeenPane>,
    /// Finishes held back until [`RELEASE_EXIT_GRACE`] passes without an exit.
    held: Vec<(Instant, RemoteTransition)>,
}

#[derive(Debug)]
struct SeenPane {
    alias: String,
    status: AgentStatus,
    agent: Option<String>,
    /// The agent left the pane since its status last changed.
    released: bool,
    /// The agent left the pane (a relayed release) and no new run has
    /// started, so the pane dropping off the peer's list is not a death.
    left: bool,
    /// The pane's process exited and its died alert went out. Until a new
    /// agent run shows up, whatever the peer still reports for it (a late
    /// status, a poll that predates the exit, a repeated exit) sends nothing.
    exited: bool,
    alert: AlertContext,
}

/// Everything an alert about one remote pane names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AlertContext {
    /// Agent name for the title; `None` for a pane no agent has held.
    title_agent: Option<String>,
    machine: String,
    body: String,
    workspace_id: String,
}

/// One reachable remote agent, as read from the store.
struct RemoteAgent {
    pane_id: String,
    status: AgentStatus,
    agent: Option<String>,
    alert: AlertContext,
}

/// The remote state one store revision shows: per peer alias, its agents when
/// it is reachable (`None` while it is not), and the relayed pane lifecycle
/// events since the last read.
struct RemoteView {
    peers: Vec<(String, Option<Vec<RemoteAgent>>)>,
    pane_events: Vec<(String, RemotePaneEvent)>,
}

impl RemoteView {
    fn read(store: &mut FederationStore) -> Self {
        let peers = store
            .peers()
            .map(|(alias, entry)| {
                let agents = (entry.reachability == Reachability::Reachable).then(|| {
                    entry
                        .agents
                        .iter()
                        .filter(|agent| agent.archived.is_none() && !agent.pane_id.is_empty())
                        .map(|agent| RemoteAgent::new(alias, agent, &entry.workspaces))
                        .collect()
                });
                (alias.to_owned(), agents)
            })
            .collect();
        Self {
            peers,
            pane_events: store.take_pane_events(),
        }
    }
}

impl RemoteAgent {
    fn new(alias: &str, agent: &AgentInfo, workspaces: &[WorkspaceInfo]) -> Self {
        // The peer's own sidebar position, like a local alert's "label · N".
        let body = workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == agent.workspace_id)
            .map(|workspace| format!("{} · {}", workspace.label, workspace.number))
            .unwrap_or_default();
        Self {
            pane_id: agent.pane_id.clone(),
            status: agent.agent_status,
            agent: agent.agent.clone(),
            alert: AlertContext {
                // The poll stores remote names alias-qualified ("<machine-id>/llm-opt")
                // so they can be addressed; the title names the machine separately, so
                // it takes the agent's own name. Without this the title starts with a
                // 32-character machine id and iOS truncates everything after it.
                title_agent: agent.agent.as_deref().map(|label| {
                    let name = agent.name.as_deref().map(|name| {
                        name.strip_prefix(alias)
                            .and_then(|rest| rest.strip_prefix('/'))
                            .unwrap_or(name)
                    });
                    super::push_title_agent(name, label).to_owned()
                }),
                machine: agent
                    .machine_label
                    .clone()
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| alias.to_owned()),
                body,
                workspace_id: agent.workspace_id.clone(),
            },
        }
    }
}

/// The local detector's view of a remote status, so remote transitions follow
/// the same notification rules as local ones.
fn detect_state(status: AgentStatus) -> crate::detect::AgentState {
    match status {
        AgentStatus::Idle | AgentStatus::Done => crate::detect::AgentState::Idle,
        AgentStatus::Working => crate::detect::AgentState::Working,
        AgentStatus::Blocked => crate::detect::AgentState::Blocked,
        AgentStatus::Unknown => crate::detect::AgentState::Unknown,
    }
}

/// The alert a remote pane's status change calls for, if any; only agent
/// panes notify.
fn transition_kind(previous: AgentStatus, agent: &RemoteAgent) -> Option<PushKind> {
    agent.agent.as_ref()?;
    let toast = crate::app::actions::notification_toast_for_state_change(
        false,
        detect_state(previous),
        detect_state(agent.status),
    );
    super::push_kind_for(false, toast)
}

/// One transition to push.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteTransition {
    kind: PushKind,
    pane_id: String,
    alert: AlertContext,
}

impl RemoteTransition {
    /// The same alert shape a local agent produces, with the machine named:
    /// "llm-opt on Jerry's Mac Studio needs attention". `None` for a pane no
    /// agent has held, or a title that sanitizes away, like the local path.
    fn notification(self) -> Option<PushNotification> {
        let title_agent = self.alert.title_agent?;
        let title = super::sanitized_notification_text(
            &format!(
                "{title_agent} on {} {}",
                self.alert.machine,
                super::push_event_text(self.kind)
            ),
            80,
        )?;
        Some(PushNotification {
            title,
            body: super::sanitized_notification_text(&self.alert.body, 240).unwrap_or_default(),
            pane_id: self.pane_id,
            workspace_id: self.alert.workspace_id,
            kind: self.kind,
            #[cfg(unix)]
            guest_scope: None,
            // v1: only the daemon that runs an agent sends its question
            // (`herdr_ask`); a peer's ask arrives as a plain alert.
            ask_source: None,
            ask: None,
        })
    }
}

impl RemotePushTracker {
    /// Held finishes whose grace ran out by `now`.
    fn take_due(&mut self, now: Instant) -> Vec<RemoteTransition> {
        let (due, held) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|(deadline, _)| *deadline <= now);
        self.held = held;
        due.into_iter().map(|(_, transition)| transition).collect()
    }

    /// Compare one store revision with what was last seen, update the
    /// baseline, and return the transitions to push now.
    fn observe(&mut self, view: RemoteView, now: Instant) -> Vec<RemoteTransition> {
        let mut transitions = Vec::new();
        // Lifecycle events first, against the state seen before this revision:
        // the status change a release causes may be in the same revision.
        for (pane_id, event) in view.pane_events {
            match event {
                RemotePaneEvent::AgentReleased => {
                    if let Some(seen) = self.panes.get_mut(&pane_id) {
                        seen.released = true;
                        seen.left = true;
                    }
                }
                RemotePaneEvent::Exited => {
                    self.held.retain(|(_, held)| held.pane_id != pane_id);
                    if let Some(seen) = self.panes.get_mut(&pane_id) {
                        if !std::mem::replace(&mut seen.exited, true) {
                            seen.released = false;
                            transitions.push(RemoteTransition {
                                kind: PushKind::Died,
                                pane_id,
                                alert: seen.alert.clone(),
                            });
                        }
                    }
                }
                // Closed on purpose, like closing a local pane: no alert when
                // it drops off the peer's list.
                RemotePaneEvent::Closed => {
                    self.panes.remove(&pane_id);
                }
            }
        }
        let cached: HashSet<&str> = view.peers.iter().map(|(alias, _)| alias.as_str()).collect();
        self.peers.retain(|alias| cached.contains(alias.as_str()));
        self.panes
            .retain(|_, seen| cached.contains(seen.alias.as_str()));
        for (alias, agents) in view.peers {
            // An unreachable peer's agents are stale: keep the baseline so the
            // peer coming back unchanged sends nothing.
            let Some(agents) = agents else {
                continue;
            };
            let seeding = self.peers.insert(alias.clone());
            let present: HashSet<&str> =
                agents.iter().map(|agent| agent.pane_id.as_str()).collect();
            // The peer's list is authoritative: a pane gone from it closed or
            // died, possibly with its pane.exited lost across a flap. An agent
            // that already left the pane, or whose exit was pushed, ends quietly.
            self.panes.retain(|pane_id, seen| {
                if seen.alias != alias || present.contains(pane_id.as_str()) {
                    return true;
                }
                if !seen.exited && !seen.left {
                    transitions.push(RemoteTransition {
                        kind: PushKind::Died,
                        pane_id: pane_id.clone(),
                        alert: seen.alert.clone(),
                    });
                }
                false
            });
            for agent in agents {
                self.observe_agent(&alias, seeding, agent, now, &mut transitions);
            }
        }
        transitions
    }

    fn observe_agent(
        &mut self,
        alias: &str,
        seeding: bool,
        agent: RemoteAgent,
        now: Instant,
        transitions: &mut Vec<RemoteTransition>,
    ) {
        let Some(seen) = self.panes.get_mut(&agent.pane_id) else {
            // A pane new on a known peer starts from nothing, so one that is
            // already blocked still needs you.
            let kind = if seeding {
                None
            } else {
                transition_kind(AgentStatus::Unknown, &agent)
            };
            if let Some(kind) = kind {
                transitions.push(RemoteTransition {
                    kind,
                    pane_id: agent.pane_id.clone(),
                    alert: agent.alert.clone(),
                });
            }
            self.panes.insert(
                agent.pane_id,
                SeenPane {
                    alias: alias.to_owned(),
                    status: agent.status,
                    agent: agent.agent,
                    released: false,
                    left: false,
                    exited: false,
                    alert: agent.alert,
                },
            );
            return;
        };
        // A new run of an agent (working again, or the pane back to a plain
        // shell) re-arms an exited pane; nothing it reports until then counts.
        let exited = seen.exited;
        if exited && (agent.status == AgentStatus::Working || agent.agent.is_none()) {
            seen.exited = false;
        }
        let kind = if exited {
            None
        } else {
            transition_kind(seen.status, &agent)
        };
        // A finish that comes with a release waits for a possible exit; the
        // release is spent by the status change it causes.
        let hold = kind == Some(PushKind::Finished) && seen.released;
        if seen.status != agent.status {
            seen.released = false;
        }
        if agent.status == AgentStatus::Working {
            seen.left = false;
        }
        // A held finish no longer stands once the agent is busy again.
        if matches!(agent.status, AgentStatus::Working | AgentStatus::Blocked) {
            self.held.retain(|(_, held)| held.pane_id != agent.pane_id);
        }
        seen.status = agent.status;
        seen.agent = agent.agent;
        // Keep naming the agent after it releases the pane, for its exit.
        let title_agent = agent
            .alert
            .title_agent
            .or_else(|| seen.alert.title_agent.take());
        seen.alert = AlertContext {
            title_agent,
            ..agent.alert
        };
        let Some(kind) = kind else {
            return;
        };
        let transition = RemoteTransition {
            kind,
            pane_id: agent.pane_id,
            alert: seen.alert.clone(),
        };
        if hold {
            self.held.push((now + RELEASE_EXIT_GRACE, transition));
        } else {
            transitions.push(transition);
        }
    }
}

impl App {
    /// Push each remote agent transition the federation store shows since the
    /// last call, and each held finish due by `now`, then refresh the Live
    /// Activity. Cheap when nothing changed: one lock and a revision compare.
    /// Run by the app loop every iteration, which the store also wakes after
    /// each change.
    pub(crate) fn sync_remote_agent_notifications(&mut self, now: Instant) {
        let mut transitions = self.remote_push.take_due(now);
        let view = {
            let mut store = self
                .federation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let revision = store.revision();
            (self.remote_push.revision != Some(revision)).then(|| {
                self.remote_push.revision = Some(revision);
                RemoteView::read(&mut store)
            })
        };
        let changed = view.is_some();
        if let Some(view) = view {
            // The baseline follows the store even when nothing can be sent,
            // so turning push on later does not replay old transitions.
            transitions.extend(self.remote_push.observe(view, now));
        }
        if self.no_session || !crate::push::may_deliver(&self.state.push_config) {
            return;
        }
        let notifications = transitions
            .into_iter()
            .filter_map(RemoteTransition::notification)
            .collect();
        crate::push::dispatch(self.state.push_config.clone(), notifications);
        if changed {
            self.emit_live_activity_updates();
        }
    }

    /// Remote agents on reachable peers, for the Live Activity. A peer that
    /// stopped answering drops out, like a dead local pane, rather than
    /// showing its stale agents as needing you.
    pub(super) fn reachable_remote_agents(&self) -> Vec<AgentInfo> {
        let store = self
            .federation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        store
            .peers()
            .filter(|(_, entry)| entry.reachability == Reachability::Reachable)
            .flat_map(|(_, entry)| entry.agents.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::api::federation_store::{
        agent_status_event, PeerCacheEntry, Reachability, RelayResync, RemotePaneEvent,
    };
    use crate::api::schema::{AgentInfo, AgentStatus, WorkspaceInfo};
    use crate::app::App;
    use crate::config::{PushConfig, PushMode};
    use crate::persist::devices::RegisteredDevice;
    use crate::push::test_sink::Capture;
    use crate::push::{plan_alerts, PushKind, PushNotification};

    const PEER: &str = "mac";
    const PANE: &str = "mac/w1:p2";

    fn direct_push() -> PushConfig {
        PushConfig {
            mode: PushMode::Direct,
            enabled: true,
            key_path: Some("/tmp/AuthKey.p8".to_string()),
            key_id: Some("ABC123DEFG".to_string()),
            team_id: Some("TEAM123456".to_string()),
            topic: Some("app.herdr.ios".to_string()),
            ..PushConfig::default()
        }
    }

    fn relay_push() -> PushConfig {
        PushConfig {
            mode: PushMode::Relay,
            ..PushConfig::default()
        }
    }

    fn app(push: PushConfig) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.push_config = push;
        app
    }

    /// A remote agent as the poll stores it (`prefix_remote_agent`): ids and the
    /// name alias-qualified, machine fields stamped by the coordinator. `name` is
    /// the agent's own name; the alias is the pane id's prefix.
    fn remote_agent(pane_id: &str, name: &str, status: AgentStatus) -> AgentInfo {
        let alias = pane_id.split_once('/').map_or(PEER, |(alias, _)| alias);
        serde_json::from_value(serde_json::json!({
            "terminal_id": format!("{PEER}/t1"),
            "name": format!("{alias}/{name}"),
            "agent": "claude",
            "agent_status": status,
            "workspace_id": format!("{PEER}/w1"),
            "tab_id": format!("{PEER}/w1:t1"),
            "pane_id": pane_id,
            "focused": false,
            "revision": 1,
            "machine_id": PEER,
            "machine_label": "Jerry's Mac Studio",
            "reachability": "reachable",
        }))
        .expect("agent info deserializes")
    }

    fn remote_workspace() -> WorkspaceInfo {
        serde_json::from_value(serde_json::json!({
            "workspace_id": format!("{PEER}/w1"),
            "number": 1,
            "label": "api",
            "focused": false,
            "pane_count": 1,
            "tab_count": 1,
            "active_tab_id": format!("{PEER}/w1:t1"),
            "agent_status": "working",
        }))
        .expect("workspace info deserializes")
    }

    /// A poll answer from the peer.
    fn poll(app: &App, agents: Vec<AgentInfo>) {
        let mut store = app.federation.lock().unwrap();
        store.set_peer(PEER, PeerCacheEntry::reachable(agents, Instant::now()));
        store.set_peer_workspaces(PEER, vec![remote_workspace()]);
    }

    /// A status event the relay received from the peer.
    fn relay(app: &App, agent: &AgentInfo, status: AgentStatus) -> bool {
        let mut event = agent_status_event(agent);
        event.agent_status = status;
        app.federation
            .lock()
            .unwrap()
            .relay_status(PEER, &mut event, Instant::now())
    }

    fn sync(app: &mut App, capture: &Capture) -> Vec<PushNotification> {
        app.sync_remote_agent_notifications(Instant::now());
        capture.take().alerts
    }

    /// A peer that reports `agent` working, already seen by the app.
    fn seeded(push: PushConfig, name: &str) -> (App, Capture, AgentInfo) {
        let mut app = app(push);
        let capture = Capture::install();
        let agent = remote_agent(PANE, name, AgentStatus::Working);
        poll(&app, vec![agent.clone()]);
        assert!(
            sync(&mut app, &capture).is_empty(),
            "first sight is the baseline"
        );
        (app, capture, agent)
    }

    fn device(token: &str, muted: &[&str], relay_capability: Option<&str>) -> RegisteredDevice {
        RegisteredDevice {
            device_token: token.to_string(),
            platform: "ios".to_string(),
            notify_needs_input: true,
            notify_dies: true,
            notify_finishes: true,
            notify_gram: false,
            muted_panes: muted.iter().map(|pane| pane.to_string()).collect(),
            registered_unix_ms: 0,
            relay_capability: relay_capability.map(str::to_owned),
        }
    }

    #[test]
    fn relayed_remote_needs_you_pushes_once_with_the_machine_label() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");

        assert!(relay(&app, &agent, AgentStatus::Blocked));
        let alerts = sync(&mut app, &capture);

        assert_eq!(alerts.len(), 1, "{alerts:?}");
        let alert = &alerts[0];
        assert_eq!(alert.kind, PushKind::NeedsInput);
        assert_eq!(alert.title, "llm-opt on Jerry's Mac Studio needs attention");
        assert_eq!(alert.body, "api · 1");
        assert_eq!(alert.pane_id, PANE);
        assert_eq!(alert.workspace_id, "mac/w1");
        assert!(sync(&mut app, &capture).is_empty());
    }

    #[test]
    fn muted_remote_pane_is_sent_to_no_device() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        relay(&app, &agent, AgentStatus::Blocked);
        let alerts = sync(&mut app, &capture);

        let muted = [device("muted", &[PANE], None)];
        assert!(plan_alerts(&direct_push(), &alerts, &muted)
            .direct
            .is_empty());
        let other_pane_muted = [device("other", &["mac/w1:p9"], None)];
        assert_eq!(
            plan_alerts(&direct_push(), &alerts, &other_pane_muted)
                .direct
                .len(),
            1
        );
    }

    #[test]
    fn reconnect_resync_and_repeated_state_push_nothing_more() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        relay(&app, &agent, AgentStatus::Blocked);
        assert_eq!(sync(&mut app, &capture).len(), 1);
        let mut blocked = agent.clone();
        blocked.agent_status = AgentStatus::Blocked;

        // The same event relayed again.
        assert!(!relay(&app, &agent, AgentStatus::Blocked));
        // A reconnect and a peer restart resynchronize the same state.
        for mode in [RelayResync::Diff, RelayResync::Reset] {
            app.federation.lock().unwrap().relay_resync(
                PEER,
                vec![agent_status_event(&blocked)],
                mode,
                Instant::now(),
            );
            assert!(sync(&mut app, &capture).is_empty(), "{mode:?}");
        }
        // The 5 s poll repeats it; the peer drops out and comes back unchanged.
        poll(&app, vec![blocked.clone()]);
        assert!(sync(&mut app, &capture).is_empty());
        app.federation
            .lock()
            .unwrap()
            .degrade_peer(PEER, Reachability::Unreachable);
        assert!(sync(&mut app, &capture).is_empty());
        poll(&app, vec![blocked]);
        assert!(sync(&mut app, &capture).is_empty());

        // A new transition still pushes.
        relay(&app, &agent, AgentStatus::Working);
        assert!(sync(&mut app, &capture).is_empty());
        relay(&app, &agent, AgentStatus::Blocked);
        assert_eq!(sync(&mut app, &capture).len(), 1);
    }

    #[test]
    fn poll_only_peer_pushes_once() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        let mut blocked = agent;
        blocked.agent_status = AgentStatus::Blocked;

        poll(&app, vec![blocked.clone()]);
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts[0].title,
            "llm-opt on Jerry's Mac Studio needs attention"
        );

        poll(&app, vec![blocked]);
        assert!(sync(&mut app, &capture).is_empty());
    }

    #[test]
    fn new_blocked_pane_on_known_peer_pushes_but_first_sight_does_not() {
        let mut app = app(direct_push());
        let capture = Capture::install();
        let first = remote_agent(PANE, "llm-opt", AgentStatus::Blocked);
        poll(&app, vec![first.clone()]);
        assert!(sync(&mut app, &capture).is_empty());

        let second = remote_agent("mac/w1:p3", "reviewer", AgentStatus::Blocked);
        poll(&app, vec![first, second]);
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].pane_id, "mac/w1:p3");
    }

    #[test]
    fn remote_finish_pushes_finished() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        relay(&app, &agent, AgentStatus::Done);
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, PushKind::Finished);
        assert_eq!(alerts[0].title, "llm-opt on Jerry's Mac Studio finished");
    }

    #[test]
    fn remote_exit_pushes_died_only() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        let event = |app: &App, event| {
            app.federation
                .lock()
                .unwrap()
                .relay_pane_event(PEER, PANE, event);
        };
        // The peer sends release, status and exit in this order; the app loop
        // may run between any two of them.
        event(&app, RemotePaneEvent::AgentReleased);
        assert!(sync(&mut app, &capture).is_empty());
        relay(&app, &agent, AgentStatus::Done);
        assert!(sync(&mut app, &capture).is_empty());
        event(&app, RemotePaneEvent::Exited);
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, PushKind::Died);
        assert_eq!(alerts[0].title, "llm-opt on Jerry's Mac Studio exited");

        app.sync_remote_agent_notifications(Instant::now() + Duration::from_secs(5));
        assert!(
            capture.take().alerts.is_empty(),
            "the held finish is dropped"
        );
    }

    fn pane_event(app: &App, event: RemotePaneEvent) {
        app.federation
            .lock()
            .unwrap()
            .relay_pane_event(PEER, PANE, event);
    }

    #[test]
    fn blocked_remote_exit_pushes_died_once_and_nothing_else() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        relay(&app, &agent, AgentStatus::Blocked);
        assert_eq!(sync(&mut app, &capture).len(), 1);
        let mut blocked = agent.clone();
        blocked.agent_status = AgentStatus::Blocked;

        // Killed while blocked: the store still lists the blocked agent.
        pane_event(&app, RemotePaneEvent::Exited);
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert_eq!(alerts[0].kind, PushKind::Died);

        // A repeated exit, a poll still listing the dead agent, and its final
        // status arriving late send nothing.
        pane_event(&app, RemotePaneEvent::Exited);
        assert!(sync(&mut app, &capture).is_empty());
        poll(&app, vec![blocked]);
        assert!(sync(&mut app, &capture).is_empty());
        relay(&app, &agent, AgentStatus::Done);
        assert!(sync(&mut app, &capture).is_empty());

        // A new agent run in the same pane notifies again.
        relay(&app, &agent, AgentStatus::Working);
        assert!(sync(&mut app, &capture).is_empty());
        relay(&app, &agent, AgentStatus::Blocked);
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, PushKind::NeedsInput);
    }

    #[test]
    fn remote_release_without_exit_finishes_after_the_grace() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        app.federation
            .lock()
            .unwrap()
            .relay_pane_event(PEER, PANE, RemotePaneEvent::AgentReleased);
        relay(&app, &agent, AgentStatus::Done);
        assert!(sync(&mut app, &capture).is_empty());

        app.sync_remote_agent_notifications(Instant::now() + Duration::from_secs(5));
        let alerts = capture.take().alerts;
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, PushKind::Finished);
    }

    #[test]
    fn held_finish_is_cancelled_when_the_agent_resumes() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        pane_event(&app, RemotePaneEvent::AgentReleased);
        relay(&app, &agent, AgentStatus::Done);
        assert!(sync(&mut app, &capture).is_empty());
        relay(&app, &agent, AgentStatus::Working);
        assert!(sync(&mut app, &capture).is_empty());

        app.sync_remote_agent_notifications(Instant::now() + Duration::from_secs(5));
        let alerts = capture.take().alerts;
        assert!(alerts.is_empty(), "{alerts:?}");
    }

    #[test]
    fn remote_pane_lost_across_a_flap_pushes_died_once() {
        // The stream flaps and the pane.exited is lost: the reconnect
        // resync no longer lists the pane.
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        relay(&app, &agent, AgentStatus::Blocked);
        assert_eq!(sync(&mut app, &capture).len(), 1);
        let mut blocked = agent.clone();
        blocked.agent_status = AgentStatus::Blocked;
        let poll_started = Instant::now();
        app.federation.lock().unwrap().relay_resync(
            PEER,
            Vec::new(),
            RelayResync::Diff,
            Instant::now(),
        );
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert_eq!(alerts[0].kind, PushKind::Died);
        assert_eq!(alerts[0].title, "llm-opt on Jerry's Mac Studio exited");

        // A poll that started before the resync still lists the pane; it
        // must not bring it back, and a later one without it sends nothing.
        app.federation.lock().unwrap().set_polled_peer(
            PEER,
            PeerCacheEntry::reachable(vec![blocked], poll_started),
            poll_started,
        );
        assert!(sync(&mut app, &capture).is_empty());
        poll(&app, Vec::new());
        assert!(sync(&mut app, &capture).is_empty());
    }

    #[test]
    fn remote_pane_missing_after_the_peer_comes_back_pushes_died() {
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        app.federation
            .lock()
            .unwrap()
            .degrade_peer(PEER, Reachability::Unreachable);
        assert!(sync(&mut app, &capture).is_empty());
        let other = remote_agent("mac/w1:p3", "reviewer", AgentStatus::Working);
        poll(&app, vec![other]);
        let alerts = sync(&mut app, &capture);
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert_eq!(alerts[0].kind, PushKind::Died);
        assert_eq!(alerts[0].pane_id, agent.pane_id);
    }

    #[test]
    fn agent_that_left_its_pane_is_not_a_death_when_the_pane_drops_off() {
        // An unnamed agent quits to its shell: released, finished, and the
        // pane leaves agent.list on the next poll.
        let (mut app, capture, agent) = seeded(direct_push(), "llm-opt");
        pane_event(&app, RemotePaneEvent::AgentReleased);
        relay(&app, &agent, AgentStatus::Done);
        assert!(sync(&mut app, &capture).is_empty());
        poll(&app, Vec::new());
        assert!(sync(&mut app, &capture).is_empty());
        app.sync_remote_agent_notifications(Instant::now() + Duration::from_secs(5));
        let alerts = capture.take().alerts;
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert_eq!(alerts[0].kind, PushKind::Finished);
    }

    #[test]
    fn closed_remote_pane_leaves_quietly() {
        // Closing a pane is not a death, like closing a local one.
        let (mut app, capture, _) = seeded(direct_push(), "llm-opt");
        app.federation
            .lock()
            .unwrap()
            .relay_pane_closed(PEER, PANE, Instant::now());
        assert!(sync(&mut app, &capture).is_empty());
        assert!(app.reachable_remote_agents().is_empty());
        poll(&app, Vec::new());
        assert!(sync(&mut app, &capture).is_empty());
    }

    #[test]
    fn live_activity_counts_reachable_remote_agents() {
        // Unique names: the Live Activity dedup hash is process-wide.
        let (mut app, capture, agent) = seeded(direct_push(), "la-remote-agent");
        let mut other = remote_agent("lab/w1:p1", "la-lab-agent", AgentStatus::Working);
        other.machine_id = Some("lab".to_string());
        app.federation.lock().unwrap().set_peer(
            "lab",
            PeerCacheEntry::reachable(vec![other], Instant::now()),
        );
        app.sync_remote_agent_notifications(Instant::now());
        capture.take();

        relay(&app, &agent, AgentStatus::Blocked);
        app.sync_remote_agent_notifications(Instant::now());
        let state = capture
            .take()
            .live_activities
            .pop()
            .expect("live activity update");
        assert_eq!(state["headline"], "la-remote-agent");
        assert_eq!(state["status"], "needsYou");
        assert_eq!(state["needsYouCount"], 1);
        assert_eq!(state["workingCount"], 1);
        assert_eq!(state["totalCount"], 2);

        app.federation
            .lock()
            .unwrap()
            .degrade_peer(PEER, Reachability::Unreachable);
        app.sync_remote_agent_notifications(Instant::now());
        let state = capture
            .take()
            .live_activities
            .pop()
            .expect("live activity update");
        assert_eq!(state["headline"], "la-lab-agent");
        assert_eq!(
            state["totalCount"], 1,
            "an offline machine's agents drop off"
        );
    }

    #[test]
    fn direct_and_relay_modes_send_the_same_remote_alert() {
        let mut sent = Vec::new();
        for push in [direct_push(), relay_push()] {
            let (mut app, capture, agent) = seeded(push, "llm-opt");
            relay(&app, &agent, AgentStatus::Blocked);
            sent.push(sync(&mut app, &capture));
        }
        let [direct, relayed] = <[_; 2]>::try_from(sent).unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(relayed.len(), 1);
        assert_eq!(
            (&direct[0].title, &direct[0].body, &direct[0].pane_id),
            (&relayed[0].title, &relayed[0].body, &relayed[0].pane_id),
        );

        let devices = [
            device("plain", &[], None),
            device("cap", &[], Some("hpr1.AbC")),
            device("cap-muted", &[PANE], Some("hpr1.AbC")),
        ];
        let direct_plan = plan_alerts(&direct_push(), &direct, &devices);
        let relay_plan = plan_alerts(&relay_push(), &relayed, &devices);
        let tokens = |sends: &[(usize, &RegisteredDevice)]| {
            sends
                .iter()
                .map(|(_, device)| device.device_token.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(tokens(&direct_plan.direct), ["plain", "cap"]);
        assert!(direct_plan.relayed.is_empty());
        assert_eq!(tokens(&relay_plan.relayed), ["cap"]);
        assert!(relay_plan.direct.is_empty());
        assert_eq!(direct_plan.payloads, relay_plan.payloads);
    }
}
