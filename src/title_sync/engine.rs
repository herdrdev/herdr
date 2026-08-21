use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::mpsc as tokio_mpsc;
use tracing::warn;

use super::{
    agent_identity, clear_ownership, has_agent_identity, read_ownership, sanitize_title,
    session_key, title_for_pane, write_ownership, Ownership, PaneInput, ReaderPaths,
};

pub(crate) const TITLE_SYNC_PLUGIN_ID: &str = "herdr-agent-title-sync";

#[derive(Clone, Debug)]
pub(crate) struct ResolvedPane {
    pub(crate) pane_id: String,
    pub(crate) expected_session: Option<String>,
    pub(crate) expected_agent: Option<String>,
    pub(crate) expected_label: Option<String>,
    pub(crate) had_agent: bool,
    pub(crate) desired_title: Option<String>,
    pub(crate) previous: Option<Ownership>,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipMutation {
    pub(crate) pane_id: String,
    pub(crate) state: Option<Ownership>,
}

enum Work {
    Resolve {
        generation: u64,
        panes: Vec<PaneInput>,
    },
    Persist(Vec<OwnershipMutation>),
    Shutdown,
}

pub(crate) struct TitleSyncEngine {
    sender: Option<mpsc::Sender<Work>>,
    worker: Option<thread::JoinHandle<()>>,
    interval: Duration,
    next_tick: Instant,
    observed_generation: u64,
    in_flight: bool,
    pending: bool,
}

impl TitleSyncEngine {
    pub(crate) fn start(
        enabled: bool,
        interval: Duration,
        event_tx: tokio_mpsc::Sender<crate::events::AppEvent>,
    ) -> Self {
        Self::start_with(
            enabled,
            interval,
            crate::plugin_paths::plugin_state_dir(TITLE_SYNC_PLUGIN_ID),
            ReaderPaths::default(),
            event_tx,
        )
    }

    fn start_with(
        enabled: bool,
        interval: Duration,
        state_root: PathBuf,
        paths: ReaderPaths,
        event_tx: tokio_mpsc::Sender<crate::events::AppEvent>,
    ) -> Self {
        let (sender, worker) = if enabled {
            let (sender, receiver) = mpsc::channel();
            let worker = thread::spawn(move || worker_loop(receiver, event_tx, state_root, paths));
            (Some(sender), Some(worker))
        } else {
            (None, None)
        };
        Self {
            sender,
            worker,
            interval,
            next_tick: Instant::now(),
            observed_generation: 0,
            in_flight: false,
            pending: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled(event_tx: tokio_mpsc::Sender<crate::events::AppEvent>) -> Self {
        Self::start(false, Duration::from_secs(1), event_tx)
    }

    pub(crate) fn poll(&mut self, app: &crate::app::App, now: Instant) {
        if self.sender.is_none() {
            return;
        }
        let generation = app.state.title_sync_generation;
        let due = now >= self.next_tick || generation != self.observed_generation;
        if !due {
            return;
        }
        self.next_tick = now + self.interval;
        self.observed_generation = generation;
        if self.in_flight {
            self.pending = true;
            return;
        }
        self.start_resolution(app, generation);
    }

    fn start_resolution(&mut self, app: &crate::app::App, generation: u64) {
        let Some(sender) = &self.sender else {
            return;
        };
        if sender
            .send(Work::Resolve {
                generation,
                panes: app.title_sync_inputs(),
            })
            .is_ok()
        {
            self.in_flight = true;
            self.pending = false;
        }
    }

    pub(crate) fn complete(
        &mut self,
        app: &crate::app::App,
        generation: u64,
        mutations: Vec<OwnershipMutation>,
    ) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Work::Persist(mutations));
        }
        self.in_flight = false;
        if self.pending || app.state.title_sync_generation != generation {
            let generation = app.state.title_sync_generation;
            self.observed_generation = generation;
            self.start_resolution(app, generation);
        }
    }
}

impl Drop for TitleSyncEngine {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Work::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                warn!("title-sync worker panicked during shutdown");
            }
        }
    }
}

fn worker_loop(
    receiver: mpsc::Receiver<Work>,
    event_tx: tokio_mpsc::Sender<crate::events::AppEvent>,
    state_root: PathBuf,
    paths: ReaderPaths,
) {
    while let Ok(work) = receiver.recv() {
        match work {
            Work::Resolve { generation, panes } => {
                let panes = panes
                    .into_iter()
                    .map(|pane| ResolvedPane {
                        previous: read_ownership(&state_root, &pane.pane_id),
                        desired_title: has_agent_identity(&pane)
                            .then(|| title_for_pane(&pane, &paths))
                            .flatten(),
                        expected_session: session_key(&pane),
                        expected_agent: agent_identity(&pane).map(str::to_string),
                        expected_label: sanitize_title(pane.label.as_deref(), 200),
                        had_agent: has_agent_identity(&pane),
                        pane_id: pane.pane_id,
                    })
                    .collect();
                if event_tx
                    .blocking_send(crate::events::AppEvent::TitleSyncResolved { generation, panes })
                    .is_err()
                {
                    break;
                }
            }
            Work::Persist(mutations) => {
                for mutation in mutations {
                    let result = match mutation.state {
                        Some(state) => write_ownership(&state_root, &mutation.pane_id, &state),
                        None => clear_ownership(&state_root, &mutation.pane_id),
                    };
                    if let Err(error) = result {
                        warn!(pane_id = mutation.pane_id, %error, "failed to persist title ownership");
                    }
                }
            }
            Work::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_resume::{AgentSessionRef, PersistedAgentSession};
    use crate::detect::Agent;
    use crate::events::AppEvent;
    use crate::workspace::Workspace;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture_dir() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "vimeflow-title-engine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("fixture root");
        root
    }

    fn test_app() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("title-sync")];
        app.state.ensure_test_terminals();
        app
    }

    fn set_claude_session(app: &mut crate::app::App, session_id: Option<&str>) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal")
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).expect("terminal");
        terminal.detected_agent = session_id.map(|_| Agent::Claude);
        terminal.persisted_agent_session = session_id.map(|value| PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(value).expect("valid session"),
        });
        app.state.mark_title_sync_dirty();
    }

    fn pane_label(app: &crate::app::App, pane_id: crate::layout::PaneId) -> Option<&str> {
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal");
        app.state.terminals[terminal_id].manual_label.as_deref()
    }

    fn settle(engine: &mut TitleSyncEngine, app: &mut crate::app::App, now: Instant) {
        engine.poll(app, now);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match app.event_rx.try_recv() {
                Ok(AppEvent::TitleSyncResolved { generation, panes }) => {
                    let mutations = app.apply_title_sync_results(panes);
                    engine.complete(app, generation, mutations);
                }
                Ok(other) => panic!("unexpected event: {other:?}"),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) if !engine.in_flight => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "title-sync worker timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    panic!("title-sync event channel disconnected")
                }
            }
        }
    }

    #[test]
    fn engine_preserves_manual_labels_clears_owned_exit_and_recomputes_session_only_changes() {
        let root = fixture_dir();
        let claude_root = root.join("claude");
        let project = claude_root.join("projects/project");
        let state_root = root.join("state");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            project.join("session-1.jsonl"),
            "{\"type\":\"ai-title\",\"sessionId\":\"session-1\",\"aiTitle\":\"Agent task\"}\n",
        )
        .expect("session one");
        std::fs::write(
            project.join("session-2.jsonl"),
            "{\"type\":\"ai-title\",\"sessionId\":\"session-2\",\"aiTitle\":\"Second task\"}\n",
        )
        .expect("session two");

        let mut app = test_app();
        set_claude_session(&mut app, Some("session-1"));
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let mut engine = TitleSyncEngine::start_with(
            true,
            Duration::from_secs(60),
            state_root,
            ReaderPaths {
                claude_root: Some(claude_root),
                ..ReaderPaths::default()
            },
            app.event_tx.clone(),
        );

        settle(&mut engine, &mut app, Instant::now());
        assert_eq!(pane_label(&app, pane_id), Some("Agent task"));

        std::fs::write(
            project.join("session-1.jsonl"),
            "{\"type\":\"ai-title\",\"sessionId\":\"session-1\",\"aiTitle\":\"Updated task\"}\n",
        )
        .expect("updated session one");
        settle(
            &mut engine,
            &mut app,
            Instant::now() + Duration::from_secs(120),
        );
        assert_eq!(pane_label(&app, pane_id), Some("Updated task"));

        app.mutate_pane_label(0, pane_id, Some("Manual title".into()))
            .expect("manual rename");
        settle(
            &mut engine,
            &mut app,
            Instant::now() + Duration::from_secs(120),
        );
        assert_eq!(pane_label(&app, pane_id), Some("Manual title"));

        set_claude_session(&mut app, None);
        settle(&mut engine, &mut app, Instant::now());
        assert_eq!(pane_label(&app, pane_id), Some("Manual title"));

        app.mutate_pane_label(0, pane_id, None)
            .expect("clear manual label");
        set_claude_session(&mut app, Some("session-1"));
        settle(&mut engine, &mut app, Instant::now());
        assert_eq!(pane_label(&app, pane_id), Some("Updated task"));
        set_claude_session(&mut app, None);
        settle(&mut engine, &mut app, Instant::now());
        assert_eq!(pane_label(&app, pane_id), None);

        set_claude_session(&mut app, Some("session-1"));
        settle(&mut engine, &mut app, Instant::now());
        let generation = app.state.title_sync_generation;
        let updates = app.state.handle_app_event(AppEvent::AgentSessionReported {
            pane_id,
            source: "herdr:claude".into(),
            agent_label: "claude".into(),
            seq: Some(1),
            session_ref: AgentSessionRef::id("session-2"),
            session_start_source: Some("resume".into()),
        });
        assert!(
            updates.is_empty(),
            "session-only mutation stays visually silent"
        );
        assert!(app.state.title_sync_generation > generation);
        settle(&mut engine, &mut app, Instant::now());
        assert_eq!(pane_label(&app, pane_id), Some("Second task"));

        drop(engine);
        let _ = std::fs::remove_dir_all(root);
    }
}
