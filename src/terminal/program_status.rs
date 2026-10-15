//! Program status records reported through OSC 7501.
//!
//! Herdr stores valid reports as sent and does not check them against
//! processes, hooks, or screen contents. `TerminalState` decides how the
//! records rank against inferred status.

use crate::detect::AgentState;
use crate::ghostty::{ProgramStatusEvent, ProgramStatusReport, ProgramStatusState};

/// The protocol's cap. The least recently updated record makes room.
const MAX_RECORDS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusRecord {
    pub report: ProgramStatusReport,
    /// A result whose program has since returned to the shell prompt.
    left_behind: bool,
    /// Orders reports within a terminal, so a result can be told apart from
    /// the ones the user has already seen.
    sequence: u64,
    /// The app a left-behind result was shown under. Presentation only: the
    /// prompt removes the records it inherited from, and the pane's name must
    /// not change under a result the user has not seen. The reported fields
    /// stay as reported.
    shown_app: Option<String>,
}

impl ProgramStatusRecord {
    fn is_result(&self) -> bool {
        matches!(
            self.report.state,
            ProgramStatusState::Done | ProgramStatusState::Error
        )
    }
}

/// One terminal's records, least recently updated first. Applying events is
/// the whole protocol lifecycle except for what the user has seen, which
/// only the app knows; see `ProgramStatus`.
#[derive(Debug, Default, Clone)]
pub struct ProgramStatusRecords {
    records: Vec<ProgramStatusRecord>,
    last_sequence: u64,
}

impl ProgramStatusRecords {
    pub fn apply(&mut self, event: ProgramStatusEvent) {
        match event {
            ProgramStatusEvent::Report(report) => {
                let previous = self
                    .records
                    .iter()
                    .position(|record| record.report.id == report.id)
                    .map(|index| self.records.remove(index));
                // The same report again from a running program is the same
                // result, not a new one. After a prompt it is a new run's.
                let repeated =
                    previous.filter(|previous| !previous.left_behind && previous.report == report);
                let sequence = repeated.map_or_else(
                    || {
                        self.last_sequence += 1;
                        self.last_sequence
                    },
                    |previous| previous.sequence,
                );
                if self.records.len() == MAX_RECORDS {
                    self.records.remove(0);
                }
                self.records.push(ProgramStatusRecord {
                    report,
                    left_behind: false,
                    sequence,
                    shown_app: None,
                });
            }
            ProgramStatusEvent::Clear(id) if id.is_empty() => self.records.clear(),
            ProgramStatusEvent::Clear(id) => self.records.retain(|record| {
                record
                    .report
                    .id
                    .strip_prefix(id.as_str())
                    .is_none_or(|rest| !rest.is_empty() && !rest.starts_with('/'))
            }),
            // The reporting program returned to the shell prompt. Unfinished
            // records go; results stay.
            ProgramStatusEvent::PromptStart => {
                for index in 0..self.records.len() {
                    if self.records[index].is_result() {
                        if let Some(app) = self.app(&self.records[index]).map(str::to_string) {
                            self.records[index].shown_app = Some(app);
                        }
                    }
                }
                self.records.retain(ProgramStatusRecord::is_result);
                for record in &mut self.records {
                    record.left_behind = true;
                }
            }
            // The shell is running its next command, so results the last one
            // left behind are over, seen or not. Anything reported since the
            // prompt belongs to a program that is still running.
            ProgramStatusEvent::CommandStart => self.records.retain(|record| !record.left_behind),
        }
    }

    pub fn records(&self) -> &[ProgramStatusRecord] {
        &self.records
    }

    /// A record's `app`, or its nearest ancestor's when it has none.
    pub fn app(&self, record: &ProgramStatusRecord) -> Option<&str> {
        let mut id = record.report.id.as_str();
        loop {
            let app = self
                .records
                .iter()
                .find(|candidate| candidate.report.id == id)
                .and_then(|candidate| candidate.report.app.as_deref());
            if app.is_some() || id.is_empty() {
                return app;
            }
            id = id.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
}

/// What the records say about the terminal as a whole. Kept up to date on
/// every change so presentation never scans the records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusSummary {
    /// The existing pane status this presents as. `done` and `error` are
    /// finished work, which Herdr shows as idle and unseen.
    pub state: AgentState,
    /// The representative record's `app`, or its nearest ancestor's, or the
    /// one a left-behind result was last shown under.
    pub app: Option<String>,
    /// The representative record is a result the user has not seen.
    pub unseen_result: bool,
}

/// A terminal's records as the app holds them: the pane reader's latest
/// records, minus what the user has already seen.
#[derive(Debug, Default)]
pub struct ProgramStatus {
    records: ProgramStatusRecords,
    /// Results up to this sequence have been seen.
    seen_through: u64,
    /// Results up to this sequence have already counted as completed work.
    completed_through: u64,
    summary: Option<ProgramStatusSummary>,
}

impl ProgramStatus {
    /// Takes the pane reader's current records. Returns true when they hold a
    /// result reported since the last time, so one that was reported and
    /// cleared in between does not count.
    pub fn replace(&mut self, records: ProgramStatusRecords) -> bool {
        let newest_result = records
            .records
            .iter()
            .filter(|record| record.is_result())
            .map(|record| record.sequence)
            .max();
        let new_result = newest_result.is_some_and(|sequence| sequence > self.completed_through);
        self.completed_through = self.completed_through.max(newest_result.unwrap_or(0));
        self.records = records;
        self.refresh();
        new_result
    }

    /// The user has seen this terminal's results. A finished program's
    /// results go; a running program's stop outranking its ongoing work.
    pub fn acknowledge_results(&mut self) {
        self.seen_through = self.records.last_sequence;
        self.refresh();
    }

    pub fn has_unseen_result(&self) -> bool {
        self.records
            .records
            .iter()
            .any(|record| self.is_unseen_result(record))
    }

    /// The shell was respawned; its reader starts over.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn records(&self) -> &ProgramStatusRecords {
        &self.records
    }

    pub fn summary(&self) -> Option<&ProgramStatusSummary> {
        self.summary.as_ref()
    }

    fn is_unseen_result(&self, record: &ProgramStatusRecord) -> bool {
        record.is_result() && record.sequence > self.seen_through
    }

    /// Blocked, then working, then a result the user has not seen, then idle
    /// or a result already seen. Work going on now always shows: a program
    /// that finishes, waits on background work, and continues is working, and
    /// a finished record never hides one that is still working.
    fn attention(&self, record: &ProgramStatusRecord) -> u8 {
        match record.report.state {
            ProgramStatusState::Blocked => 4,
            ProgramStatusState::Working => 3,
            _ if self.is_unseen_result(record) => 2,
            ProgramStatusState::Idle | ProgramStatusState::Done | ProgramStatusState::Error => 1,
        }
    }

    /// The representative record is the one needing the most attention, and
    /// the most recently updated among equals. A left-behind result the user
    /// has seen is done with.
    fn refresh(&mut self) {
        let seen_through = self.seen_through;
        self.records
            .records
            .retain(|record| !record.left_behind || record.sequence > seen_through);
        self.summary = self
            .records
            .records
            .iter()
            .max_by_key(|record| self.attention(record))
            .map(|record| ProgramStatusSummary {
                state: match record.report.state {
                    ProgramStatusState::Working => AgentState::Working,
                    ProgramStatusState::Blocked => AgentState::Blocked,
                    ProgramStatusState::Idle
                    | ProgramStatusState::Done
                    | ProgramStatusState::Error => AgentState::Idle,
                },
                app: self
                    .records
                    .app(record)
                    .or(record.shown_app.as_deref())
                    .map(str::to_string),
                unseen_result: self.is_unseen_result(record),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: &str, state: ProgramStatusState) -> ProgramStatusReport {
        ProgramStatusReport {
            id: id.into(),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: None,
        }
    }

    fn report(id: &str, state: ProgramStatusState) -> ProgramStatusEvent {
        ProgramStatusEvent::Report(status(id, state))
    }

    fn ids(records: &ProgramStatusRecords) -> Vec<&str> {
        records
            .records()
            .iter()
            .map(|record| record.report.id.as_str())
            .collect()
    }

    /// What the app holds after the reader's records reach it.
    fn seen_by_app(status: &mut ProgramStatus, records: &ProgramStatusRecords) -> bool {
        status.replace(records.clone())
    }

    #[test]
    fn records_replace_clear_descendants_and_inherit_app() {
        let mut records = ProgramStatusRecords::default();
        let mut status_view = ProgramStatus::default();
        records.apply(ProgramStatusEvent::Report(ProgramStatusReport {
            app: Some("deploy".into()),
            message: Some("starting".into()),
            ..status("", ProgramStatusState::Working)
        }));
        records.apply(report("build", ProgramStatusState::Working));
        records.apply(report("build/test", ProgramStatusState::Blocked));
        records.apply(report("builder", ProgramStatusState::Idle));

        // The blocked grandchild stands for the terminal, under the root's app.
        seen_by_app(&mut status_view, &records);
        let summary = status_view.summary().unwrap();
        assert_eq!(summary.state, AgentState::Blocked);
        assert_eq!(summary.app.as_deref(), Some("deploy"));

        // A report replaces its whole record: the message does not carry over.
        records.apply(report("", ProgramStatusState::Idle));
        let root = records.records().last().unwrap();
        assert_eq!((root.report.id.as_str(), &root.report.message), ("", &None));

        records.apply(ProgramStatusEvent::Clear("build".into()));
        assert_eq!(ids(&records), ["builder", ""]);
        records.apply(ProgramStatusEvent::Clear(String::new()));
        seen_by_app(&mut status_view, &records);
        assert!(records.records().is_empty() && status_view.summary().is_none());
    }

    #[test]
    fn current_work_shows_first_and_results_last_until_seen() {
        let mut records = ProgramStatusRecords::default();
        let mut status_view = ProgramStatus::default();
        let root = |state| {
            ProgramStatusEvent::Report(ProgramStatusReport {
                app: Some("deploy".into()),
                ..status("", state)
            })
        };
        let shown = |status_view: &ProgramStatus| status_view.summary().unwrap().state;

        // A finished child does not hide the root that is still working.
        records.apply(root(ProgramStatusState::Working));
        records.apply(report("job", ProgramStatusState::Done));
        assert!(seen_by_app(&mut status_view, &records));
        assert_eq!(shown(&status_view), AgentState::Working);

        // The same report again is the same result. So is a result that was
        // reported and cleared before the app saw it.
        records.apply(report("job", ProgramStatusState::Done));
        records.apply(report("other", ProgramStatusState::Error));
        records.apply(ProgramStatusEvent::Clear("other".into()));
        assert!(!seen_by_app(&mut status_view, &records));

        // Nor does a finished root hide a child still working in the
        // background. A program that finishes and then continues is working
        // again, and a blocked record outranks working.
        records.apply(root(ProgramStatusState::Done));
        records.apply(report("job", ProgramStatusState::Working));
        assert!(seen_by_app(&mut status_view, &records));
        assert_eq!(shown(&status_view), AgentState::Working);
        records.apply(root(ProgramStatusState::Working));
        records.apply(report("job", ProgramStatusState::Done));
        records.apply(report("ask", ProgramStatusState::Blocked));
        seen_by_app(&mut status_view, &records);
        assert_eq!(shown(&status_view), AgentState::Blocked);
        records.apply(ProgramStatusEvent::Clear("ask".into()));
        seen_by_app(&mut status_view, &records);
        assert_eq!(shown(&status_view), AgentState::Working);

        // When the work ends, the unseen result is what the terminal shows.
        // Once seen it is an ordinary record, kept with its details.
        records.apply(root(ProgramStatusState::Idle));
        seen_by_app(&mut status_view, &records);
        assert!(status_view.summary().unwrap().unseen_result);
        status_view.acknowledge_results();
        assert_eq!(ids(status_view.records()), ["job", ""]);
        assert!(!status_view.summary().unwrap().unseen_result);

        // The prompt drops unfinished work. An unseen result survives any
        // number of prompts, under the app it inherited, until it is seen.
        records.apply(report("idle", ProgramStatusState::Idle));
        records.apply(report("blocked", ProgramStatusState::Blocked));
        records.apply(report("failed", ProgramStatusState::Error));
        for _ in 0..3 {
            records.apply(ProgramStatusEvent::PromptStart);
            seen_by_app(&mut status_view, &records);
            assert_eq!(ids(status_view.records()), ["failed"]);
        }
        let summary = status_view.summary().unwrap();
        assert!(summary.unseen_result);

        // Its parent is gone, so the record itself has no app, but the pane
        // keeps the name the result was shown under. A new parent's app is
        // inherited like any other.
        let failed = |records: &ProgramStatusRecords| {
            let mut all = records.records().iter();
            all.find(|record| record.report.id == "failed")
                .cloned()
                .unwrap()
        };
        assert_eq!(failed(&records).report.app, None);
        assert_eq!(records.app(&failed(&records)), None);
        assert_eq!(summary.app.as_deref(), Some("deploy"));
        records.apply(ProgramStatusEvent::Report(ProgramStatusReport {
            app: Some("release".into()),
            ..status("", ProgramStatusState::Idle)
        }));
        assert_eq!(records.app(&failed(&records)), Some("release"));
        records.apply(ProgramStatusEvent::PromptStart);
        seen_by_app(&mut status_view, &records);
        assert_eq!(ids(status_view.records()), ["failed"]);

        // Seeing it removes it, and it does not come back with later records.
        status_view.acknowledge_results();
        assert!(status_view.summary().is_none());
        seen_by_app(&mut status_view, &records);
        assert!(status_view.records().records().is_empty());

        // The same result from the next run is a new one.
        records.apply(report("failed", ProgramStatusState::Error));
        records.apply(ProgramStatusEvent::PromptStart);
        assert!(seen_by_app(&mut status_view, &records));
        assert!(status_view.summary().unwrap().unseen_result);

        // The next marked command ends results left behind before it, seen
        // or not. A record reported since the prompt belongs to a program
        // that is still running and stays, as does everything reported
        // afterwards. The app's older copy does not bring the result back.
        records.apply(report("fresh", ProgramStatusState::Done));
        records.apply(ProgramStatusEvent::CommandStart);
        records.apply(report("", ProgramStatusState::Working));
        assert_eq!(ids(&records), ["fresh", ""]);
        seen_by_app(&mut status_view, &records);
        assert_eq!(ids(status_view.records()), ["fresh", ""]);
    }

    #[test]
    fn the_least_recently_updated_record_makes_room_at_the_cap() {
        let mut records = ProgramStatusRecords::default();
        for index in 0..MAX_RECORDS {
            records.apply(report(&format!("r{index}"), ProgramStatusState::Working));
        }
        records.apply(report("r0", ProgramStatusState::Working));
        records.apply(report("new", ProgramStatusState::Working));

        assert_eq!(records.records().len(), MAX_RECORDS);
        assert!(!ids(&records).contains(&"r1"));
        assert!(ids(&records).contains(&"r0"));
    }
}
