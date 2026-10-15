//! Internal app events delivered via channel.
//!
//! Background tasks (PTY child watchers, future hook listeners, etc.) send
//! events to the main loop through this channel. No polling needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::detect::{Agent, AgentState};
use crate::ghostty::ProgramStatusEvent;
use crate::layout::PaneId;
use crate::terminal::program_status::ProgramStatusRecords;
use crate::workspace::{GitStatusCacheEntry, WorkspaceGitStatus};

/// A pane reader's program status records, waiting for the app to take them.
///
/// A program reports each change once and nothing repeats it, so a full event
/// channel must not lose reports. The reader applies them here as they
/// arrive, which keeps what is retained to one terminal's records however
/// long the app takes, and one `ProgramStatusReported` wakeup at a time tells
/// the app to take the current records.
#[derive(Debug, Default)]
pub struct ProgramStatusMailbox {
    pending: Mutex<ProgramStatusRecords>,
    wakeup_pending: AtomicBool,
    /// The pane runtime that reported these is gone.
    retired: AtomicBool,
}

impl ProgramStatusMailbox {
    /// Called when the owning pane runtime goes away, so a wakeup still in
    /// flight cannot hand its records to the shell that replaces it.
    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    /// Applies events. Returns true when the caller must send the wakeup.
    pub fn push(&self, events: Vec<ProgramStatusEvent>) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        for event in events {
            pending.apply(event);
        }
        !self.wakeup_pending.swap(true, Ordering::AcqRel)
    }

    /// The current records. None once the pane runtime is gone.
    pub fn take(&self) -> Option<ProgramStatusRecords> {
        if self.retired.load(Ordering::Acquire) {
            return None;
        }
        // Cleared first, so a push racing with this take sends a new wakeup.
        self.wakeup_pending.store(false, Ordering::Release);
        Some(
            self.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
    }
}

#[derive(Debug)]
pub struct ApiWorktreeAddRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub source_workspace_id: Option<String>,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub label: Option<String>,
    pub focus: bool,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeAddResult {
    pub path: std::path::PathBuf,
    pub api_request: Option<ApiWorktreeAddRequest>,
    pub result: Result<(), String>,
}

#[derive(Debug)]
pub struct ApiWorktreeRemoveRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub shutdown_panes: Vec<crate::layout::PaneId>,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeRemoveResult {
    pub workspace_id: String,
    pub path: std::path::PathBuf,
    pub workspace: Option<Box<crate::api::schema::WorkspaceInfo>>,
    pub worktree: Option<Box<crate::api::schema::WorktreeInfo>>,
    pub forced: bool,
    pub api_request: Option<ApiWorktreeRemoveRequest>,
    pub result: Result<(), String>,
}

#[derive(Debug)]
pub struct WorktreeReadResult {
    // Keep the slot until completion is consumed, including time queued on the app loop.
    pub(crate) _permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) client_local: bool,
    pub(crate) request: crate::api::schema::Request,
    pub(crate) source_workspace_id: Option<String>,
    pub(crate) source_cwd: Option<std::path::PathBuf>,
    pub(crate) result: Result<WorktreeReadData, (String, String)>,
    pub(crate) respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub(crate) struct WorktreeReadData {
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub entries: Vec<crate::worktree::ExistingWorktree>,
}

/// An event from a background task to the main loop.
#[derive(Debug)]
pub enum AppEvent {
    /// A pane's child process exited.
    PaneDied {
        pane_id: PaneId,
        exit_reason: crate::platform::ChildExitReason,
    },
    /// A worktree-removal runtime could not be restored normally.
    WorktreeRuntimeRestoreFailed { pane_id: PaneId, operation_id: u64 },
    /// Process detection identified an agent before its screen state was confirmed.
    AgentProcessDetected {
        pane_id: PaneId,
        agent: Agent,
        observed_at: Instant,
    },
    /// The current Codex input screen is visible during managed startup.
    CodexPromptObserved { pane_id: PaneId, ready: bool },
    /// Fallback detector state changed in a pane.
    StateChanged {
        pane_id: PaneId,
        agent: Option<Agent>,
        state: AgentState,
        visible_blocker: bool,
        visible_working: bool,
        process_exited: bool,
        observed_at: Instant,
    },
    /// Hook-authoritative agent state was reported for a pane.
    HookStateReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
    },
    /// Agent session identity was reported without state authority.
    AgentSessionReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        session_start_source: Option<String>,
    },
    /// A reporter supplied the command that resumes its own session.
    AgentResumeReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        seq: Option<u64>,
        argv: Vec<String>,
    },
    /// A pane held by a self-reported agent is back at its idle shell.
    ReportedAgentShellReturned {
        pane_id: PaneId,
        observed_at: std::time::Instant,
    },
    /// Display-only agent metadata was reported for a pane.
    HookMetadataReported {
        pane_id: PaneId,
        source: String,
        agent_label: Option<String>,
        applies_to_source: Option<String>,
        title: Option<String>,
        display_agent: Option<String>,
        state_labels: std::collections::HashMap<String, String>,
        clear_title: bool,
        clear_display_agent: bool,
        clear_state_labels: bool,
        seq: Option<u64>,
        ttl: Option<std::time::Duration>,
    },
    /// Hook authority was explicitly cleared for a pane.
    HookAuthorityCleared {
        pane_id: PaneId,
        source: Option<String>,
        seq: Option<u64>,
    },
    /// The current detected agent gracefully released this pane back to the shell.
    HookAgentReleased {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        known_agent: Option<Agent>,
        seq: Option<u64>,
    },
    /// A new version is available through the active installation manager.
    UpdateReady {
        version: String,
        install_command: String,
    },
    /// Remote agent detection manifest update check finished.
    AgentDetectionManifestsUpdated {
        updated: Vec<crate::detect::manifest_update::ManifestUpdateCommit>,
        activated: Vec<crate::detect::Agent>,
        status: crate::detect::manifest_update::ManifestUpdateStatus,
    },
    /// A pane child emitted one or more executable BEL characters.
    /// The host-facing process forwards them to its outer terminal.
    TerminalBell { pane_id: PaneId, count: u16 },
    /// A pane child emitted a valid OSC 52 clipboard write. The main loop
    /// re-emits it through herdr's own clipboard writer.
    ClipboardWrite { content: Vec<u8> },
    /// A pane child reported program status through OSC 7501, or its
    /// terminal reached a boundary that ends reported work.
    ProgramStatusReported {
        pane_id: PaneId,
        reports: Arc<ProgramStatusMailbox>,
    },
    /// A pane child reported its shell current directory through terminal
    /// metadata such as OSC 7.
    TerminalCwdReported {
        pane_id: PaneId,
        cwd: std::path::PathBuf,
    },
    /// Background git status refresh completed for workspaces.
    GitStatusRefreshed {
        results: Vec<WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, GitStatusCacheEntry)>,
    },
    /// Background validation of a saved membership after session restore.
    RestoredWorktreeSpaceChecked {
        workspace_id: String,
        expected: crate::workspace::WorktreeSpaceMembership,
        valid: bool,
    },
    /// A configured tab bar status command finished.
    TabBarCommandFinished {
        generation: u64,
        segment_index: usize,
        result: Result<Option<String>, String>,
    },
    /// A plugin action or event command finished.
    PluginCommandFinished {
        log_id: String,
        finished_unix_ms: u64,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
    /// Background `git worktree add` completed.
    WorktreeAddFinished(Box<WorktreeAddResult>),
    /// Background `git worktree remove` completed.
    WorktreeRemoveFinished(Box<WorktreeRemoveResult>),
    /// Background worktree discovery completed for an API list/open request.
    WorktreeReadFinished(Box<WorktreeReadResult>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghostty::{ProgramStatusReport, ProgramStatusState};

    fn report(id: &str, state: ProgramStatusState, message_bytes: usize) -> ProgramStatusEvent {
        ProgramStatusEvent::Report(ProgramStatusReport {
            id: id.into(),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: Some("x".repeat(message_bytes)),
        })
    }

    #[test]
    fn an_undrained_mailbox_keeps_bounded_records_and_the_final_state() {
        let mailbox = ProgramStatusMailbox::default();
        let ids = |records: &ProgramStatusRecords| -> Vec<String> {
            records
                .records()
                .iter()
                .map(|record| record.report.id.clone())
                .collect()
        };

        // A program floods distinct records with the largest message the
        // protocol allows while the app takes nothing.
        for index in 0..20_000 {
            mailbox.push(vec![report(
                &format!("job{index}"),
                ProgramStatusState::Working,
                2048,
            )]);
        }
        let records = mailbox.take().unwrap();
        let expected: Vec<_> = (19_744..20_000)
            .map(|index| format!("job{index}"))
            .collect();
        assert_eq!(ids(&records), expected);

        // Clears, prompt boundaries, and results keep their meaning.
        for round in 0..5_000 {
            mailbox.push(vec![
                report(&format!("round{round}"), ProgramStatusState::Working, 2048),
                ProgramStatusEvent::Clear(format!("job{}", 19_744 + round % 256)),
                ProgramStatusEvent::PromptStart,
            ]);
        }
        mailbox.push(vec![
            report("deploy", ProgramStatusState::Error, 2048),
            ProgramStatusEvent::PromptStart,
            report("", ProgramStatusState::Blocked, 0),
        ]);
        assert_eq!(ids(&mailbox.take().unwrap()), ["deploy", ""]);

        // Once its pane runtime is gone, nothing more is handed over.
        mailbox.retire();
        assert!(mailbox.take().is_none());
    }
}
