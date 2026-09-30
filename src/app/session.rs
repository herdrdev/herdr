use std::time::{Duration, Instant};

use super::{App, SESSION_SAVE_DEBOUNCE};

const SESSION_SAVE_COMPLETION_POLL: Duration = Duration::from_millis(100);
const SESSION_SAVE_RETRY_BASE: Duration = Duration::from_millis(250);
const SESSION_SAVE_RETRY_MAX: Duration = Duration::from_secs(30);
const SESSION_SAVE_STALL_WARN_AFTER: Duration = Duration::from_secs(60);
const SESSION_SAVE_STALL_WARN_INTERVAL: Duration = Duration::from_secs(60);

enum SessionSaveJob {
    Clear,
    Save {
        snapshot: Box<crate::persist::SessionSnapshot>,
        history: Option<crate::persist::SessionHistorySnapshot>,
    },
}

pub(crate) struct SessionSaveResult {
    pub(super) revision: u64,
    pub(super) result: std::io::Result<()>,
}

impl App {
    pub(super) fn persist_session_candidate(
        &mut self,
        snapshot: crate::persist::SessionSnapshot,
        history: Option<crate::persist::SessionHistorySnapshot>,
    ) -> std::io::Result<()> {
        if let Some(thread) = self.session_save_thread.take() {
            match thread.join() {
                Ok(result) => self.apply_session_save_result(result),
                Err(_) => self.apply_session_save_result(SessionSaveResult {
                    revision: self.state.session_dirty_revision,
                    result: Err(std::io::Error::other("session save thread panicked")),
                }),
            }
        }
        if self.no_session {
            return Err(std::io::Error::other("session persistence is disabled"));
        }

        let revision = self.state.session_dirty_revision;
        #[cfg(test)]
        let result = if let Some((session_path, history_path)) =
            self.group_session_paths_override.as_ref()
        {
            crate::persist::save_to_paths(session_path, history_path, &snapshot, history.as_ref())
        } else {
            run_session_save_job(
                SessionSaveJob::Save {
                    snapshot: Box::new(snapshot),
                    history,
                },
                revision,
                &self.session_writer,
            )
            .result
        };
        #[cfg(not(test))]
        let result = run_session_save_job(
            SessionSaveJob::Save {
                snapshot: Box::new(snapshot),
                history,
            },
            revision,
            &self.session_writer,
        )
        .result;

        match result {
            Ok(()) => {
                self.apply_session_save_result(SessionSaveResult {
                    revision,
                    result: Ok(()),
                });
                Ok(())
            }
            Err(error) => {
                let returned = std::io::Error::new(error.kind(), error.to_string());
                self.apply_session_save_result(SessionSaveResult {
                    revision,
                    result: Err(error),
                });
                Err(returned)
            }
        }
    }

    pub(super) fn schedule_session_save(&mut self) {
        if self.no_session {
            // Nothing is persisted, but connected clients still need the
            // structural change announced as a session revision.
            self.state.mark_session_dirty();
            return;
        }

        self.state.mark_session_dirty();
        if self.session_save_retry_deadline.is_none() {
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_DEBOUNCE);
            self.session_save_scheduled_revision = Some(self.state.session_dirty_revision);
        }
    }

    pub(crate) fn sync_session_save_schedule(&mut self) {
        if self.state.session_dirty_revision > self.state.session_event_revision {
            let revision = self.state.session_dirty_revision;
            let epoch = self.state.session_epoch.clone();
            let snapshot = self.session_snapshot();
            self.event_hub.push(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::SessionChanged,
                data: crate::api::schema::EventData::SessionChanged {
                    epoch,
                    revision,
                    snapshot: Box::new(snapshot),
                },
            });
            self.state.session_event_revision = revision;
        }
        self.reap_finished_session_save();
        self.warn_if_session_save_stalled(Instant::now());
        if let Some(retry_at) = self.session_save_retry_deadline {
            self.session_save_deadline = Some(retry_at);
            return;
        }
        if self.state.session_dirty
            && !self.no_session
            && self.session_save_thread.is_none()
            && (self.session_save_deadline.is_none()
                || self.session_save_scheduled_revision != Some(self.state.session_dirty_revision))
        {
            self.schedule_session_save();
        }
    }

    pub(super) fn reap_finished_session_save(&mut self) {
        if !self
            .session_save_thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return;
        }

        let Some(thread) = self.session_save_thread.take() else {
            return;
        };
        match thread.join() {
            Ok(result) => self.apply_session_save_result(result),
            Err(_) => self.apply_session_save_result(SessionSaveResult {
                revision: self.state.session_dirty_revision,
                result: Err(std::io::Error::other("session save thread panicked")),
            }),
        }
    }

    pub(super) fn apply_session_save_result(&mut self, result: SessionSaveResult) {
        match result.result {
            Ok(()) => {
                self.session_save_failures = 0;
                self.session_save_retry_deadline = None;
                if self.state.session_dirty_revision == result.revision {
                    self.state.session_dirty = false;
                    self.session_save_deadline = None;
                    self.session_dirty_since = None;
                    self.session_save_stall_warning_at = None;
                }
            }
            Err(err) => {
                self.state.session_dirty = true;
                self.session_save_failures = self.session_save_failures.saturating_add(1);
                let delay = session_save_retry_delay(self.session_save_failures);
                let retry_at = Instant::now() + delay;
                self.session_save_retry_deadline = Some(retry_at);
                self.session_save_deadline = Some(retry_at);
                tracing::warn!(
                    err = %err,
                    attempt = self.session_save_failures,
                    retry_ms = delay.as_millis(),
                    "session save failed; retry scheduled"
                );
            }
        }
    }

    fn capture_session_save_job(&self) -> SessionSaveJob {
        if self.state.workspaces.is_empty() {
            SessionSaveJob::Clear
        } else {
            let snapshot = crate::persist::capture(
                &self.state.workspaces,
                &self.state.terminals,
                &self.terminal_runtimes,
                self.state.active,
                self.state.selected,
                self.state.sidebar_width,
                self.state.sidebar_section_split,
                self.state.collapsed_space_keys.clone(),
                self.state.prio_panel_collapsed,
                self.state.window_cycle_mode,
                self.state.skip_collapsed_cycle,
            );
            let history = self.persist_pane_history.then(|| {
                crate::persist::capture_history(
                    &snapshot,
                    &self.state.workspaces,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            });
            SessionSaveJob::Save {
                snapshot: Box::new(snapshot),
                history,
            }
        }
    }

    pub(crate) fn start_background_session_save(&mut self) {
        if self.no_session {
            self.session_save_deadline = None;
            self.session_save_scheduled_revision = None;
            self.session_save_retry_deadline = None;
            return;
        }

        self.reap_finished_session_save();
        if self.session_save_thread.is_some() {
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_COMPLETION_POLL);
            return;
        }
        if self
            .session_save_retry_deadline
            .is_some_and(|retry_at| Instant::now() < retry_at)
        {
            self.session_save_deadline = self.session_save_retry_deadline;
            return;
        }
        if !self.state.session_dirty {
            self.session_save_deadline = None;
            self.session_dirty_since = None;
            self.session_save_stall_warning_at = None;
            return;
        }

        self.session_dirty_since.get_or_insert_with(Instant::now);

        self.pane_exit_checkpoint_pending = false;
        let job = self.capture_session_save_job();
        let revision = self.state.session_dirty_revision;
        self.session_save_scheduled_revision = Some(revision);
        self.session_save_retry_deadline = None;
        self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_COMPLETION_POLL);
        let writer = self.session_writer.clone();
        match std::thread::Builder::new()
            .name("herdr-session-save".into())
            .spawn(move || run_session_save_job(job, revision, &writer))
        {
            Ok(thread) => self.session_save_thread = Some(thread),
            Err(err) => {
                tracing::warn!(err = %err, "failed to spawn session save thread; saving inline");
                let writer = self.session_writer.clone();
                let job = self.capture_session_save_job();
                let result = run_session_save_job(job, revision, &writer);
                self.apply_session_save_result(result);
            }
        }
    }

    pub(crate) fn save_session_now(&mut self) {
        self.pane_exit_checkpoint_pending = false;
        if let Some(thread) = self.session_save_thread.take() {
            match thread.join() {
                Ok(result) => self.apply_session_save_result(result),
                Err(_) => self.apply_session_save_result(SessionSaveResult {
                    revision: self.state.session_dirty_revision,
                    result: Err(std::io::Error::other("session save thread panicked")),
                }),
            }
        }

        if self.no_session {
            self.session_save_deadline = None;
            self.session_save_scheduled_revision = None;
            self.session_save_retry_deadline = None;
            return;
        }

        let revision = self.state.session_dirty_revision;
        let writer = self.session_writer.clone();
        let job = self.capture_session_save_job();
        let result = run_session_save_job(job, revision, &writer);
        self.apply_session_save_result(result);
    }

    pub(crate) fn checkpoint_session_before_pane_exit(&mut self) {
        if self.no_session || (self.pane_exit_checkpoint_pending && !self.state.session_dirty) {
            return;
        }
        self.save_session_now();
        if self.session_save_retry_deadline.is_none() {
            self.pane_exit_checkpoint_pending = true;
            self.state.session_dirty = false;
        }
    }

    pub(crate) fn finish_checkpointed_pane_exit(&mut self) {
        if self.pane_exit_checkpoint_pending {
            self.state.session_dirty = false;
            self.session_save_deadline = None;
            self.session_save_scheduled_revision = None;
            self.session_save_retry_deadline = None;
        }
    }

    pub(crate) fn save_session_on_shutdown(&mut self) {
        if self.pane_exit_checkpoint_pending && !self.state.session_dirty {
            self.session_save_deadline = None;
            return;
        }
        self.save_session_now();
    }

    fn warn_if_session_save_stalled(&mut self, now: Instant) {
        if self.no_session || !self.state.session_dirty {
            self.session_dirty_since = None;
            self.session_save_stall_warning_at = None;
            return;
        }

        let dirty_since = *self.session_dirty_since.get_or_insert(now);
        if now.duration_since(dirty_since) < SESSION_SAVE_STALL_WARN_AFTER
            || self
                .session_save_stall_warning_at
                .is_some_and(|last| now.duration_since(last) < SESSION_SAVE_STALL_WARN_INTERVAL)
        {
            return;
        }

        let dirty_for = now.duration_since(dirty_since);
        tracing::warn!(
            dirty_for_secs = dirty_for.as_secs(),
            revision = self.state.session_dirty_revision,
            writer_running = self.session_save_thread.is_some(),
            save_deadline_set = self.session_save_deadline.is_some(),
            "session has remained dirty without a successful save"
        );
        self.session_save_stall_warning_at = Some(now);
    }
}

fn session_save_retry_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    SESSION_SAVE_RETRY_BASE
        .saturating_mul(1_u32 << shift)
        .min(SESSION_SAVE_RETRY_MAX)
}

fn run_session_save_job(
    job: SessionSaveJob,
    revision: u64,
    writer: &std::sync::Mutex<crate::persist::SessionWriter>,
) -> SessionSaveResult {
    let result = match writer.lock() {
        Ok(mut writer) => match job {
            SessionSaveJob::Clear => writer.clear(),
            SessionSaveJob::Save { snapshot, history } => writer.save(&snapshot, history.as_ref()),
        },
        Err(err) => Err(std::io::Error::other(format!(
            "session writer mutex poisoned: {err}"
        ))),
    };
    SessionSaveResult { revision, result }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = crate::config::Config::default();
        config.work_index.enabled = false;
        App::new(&config, true, None, api_rx, crate::api::EventHub::default())
    }

    #[test]
    fn dirty_session_publishes_one_revisioned_snapshot_event_per_sync() {
        let mut app = test_app();
        app.state.mark_session_dirty();
        let revision = app.state.session_dirty_revision;

        app.sync_session_save_schedule();
        app.sync_session_save_schedule();

        let events = app.event_hub.events_after(0);
        assert_eq!(events.len(), 1);
        let event = &events[0].1;
        let crate::api::schema::EventData::SessionChanged {
            epoch,
            revision: event_revision,
            snapshot,
        } = &event.data
        else {
            panic!("expected session change event");
        };
        assert_eq!(*event_revision, revision);
        assert_eq!(snapshot.epoch.as_deref(), Some(epoch.as_str()));
        assert_eq!(snapshot.revision, Some(revision));
        assert_eq!(snapshot.as_ref(), &app.session_snapshot());
        assert_eq!(app.state.session_event_revision, revision);
    }

    #[test]
    fn dirty_session_warns_when_no_save_completes_for_sixty_seconds() {
        let mut app = test_app();
        app.no_session = false;
        app.state.session_dirty = true;
        let now = Instant::now();
        app.session_dirty_since = Some(now - SESSION_SAVE_STALL_WARN_AFTER);

        app.warn_if_session_save_stalled(now);

        assert_eq!(app.session_save_stall_warning_at, Some(now));
        app.warn_if_session_save_stalled(now + SESSION_SAVE_STALL_WARN_INTERVAL);
        assert_eq!(
            app.session_save_stall_warning_at,
            Some(now + SESSION_SAVE_STALL_WARN_INTERVAL)
        );
    }

    #[test]
    fn ac1_retry_backoff_is_bounded() {
        assert_eq!(session_save_retry_delay(1), Duration::from_millis(250));
        assert_eq!(session_save_retry_delay(2), Duration::from_millis(500));
        assert_eq!(session_save_retry_delay(u32::MAX), SESSION_SAVE_RETRY_MAX);
    }
}
