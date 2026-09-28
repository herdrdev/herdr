use std::time::{Duration, Instant};

use super::{App, SESSION_SAVE_DEBOUNCE};

const SESSION_SAVE_COMPLETION_POLL: Duration = Duration::from_millis(100);
const SESSION_SAVE_RETRY_BASE: Duration = Duration::from_millis(250);
const SESSION_SAVE_RETRY_MAX: Duration = Duration::from_secs(30);

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
            return;
        }

        self.state.mark_session_dirty();
        if self.session_save_retry_deadline.is_none() {
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_DEBOUNCE);
            self.session_save_scheduled_revision = Some(self.state.session_dirty_revision);
        }
    }

    pub(crate) fn sync_session_save_schedule(&mut self) {
        self.reap_finished_session_save();
        if let Some(retry_at) = self.session_save_retry_deadline {
            self.session_save_deadline = Some(retry_at);
            return;
        }
        if self.state.session_dirty
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
            return;
        }

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

    #[test]
    fn ac1_retry_backoff_is_bounded() {
        assert_eq!(session_save_retry_delay(1), Duration::from_millis(250));
        assert_eq!(session_save_retry_delay(2), Duration::from_millis(500));
        assert_eq!(session_save_retry_delay(u32::MAX), SESSION_SAVE_RETRY_MAX);
    }
}
