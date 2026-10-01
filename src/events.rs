//! Internal app events delivered via channel.
//!
//! Background tasks (PTY child watchers, future hook listeners, etc.) send
//! events to the main loop through this channel. No polling needed.

use std::time::Instant;

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::workspace::{GitStatusCacheEntry, WorkspaceGitStatus};

#[derive(Debug)]
pub struct ClosingBlockReport {
    pub gates: Vec<crate::api::schema::ClosingBlockItem>,
    pub items: Vec<crate::api::schema::ClosingBlockItem>,
    pub decisions: Vec<crate::api::schema::ClosingBlockDecision>,
    pub agents: Option<u32>,
    pub completion: Option<crate::api::schema::ClosingCompletion>,
    pub external_wait: Option<String>,
    pub parse_status: Option<crate::api::schema::ClosingParseStatus>,
    pub workers_unknown: Option<bool>,
    pub settle_ready: bool,
    pub dependencies_authoritative: bool,
    pub session_id: Option<String>,
    pub last_turn_at: Option<String>,
}

#[derive(Debug)]
pub struct ApiWorktreeAddRequest {
    pub client_id: Option<u64>,
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
    pub work_context: Option<crate::work_context::PaneWorkContext>,
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
    pub client_id: Option<u64>,
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
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

/// An event from a background task to the main loop.
#[derive(Debug)]
pub enum AppEvent {
    PlanningLockRemoteSnapshot(Option<crate::planning_lock::Snapshot>),
    PlanningLockFileChanged(crate::planning_lock::PlanningLock),
    /// A read-only fleet host inventory poll completed.
    FleetRefreshed {
        snapshot: crate::fleet::Snapshot,
    },
    /// A remote owner pushed its current agent inventory over the host event stream.
    FleetAgentInventoryChanged {
        host: crate::config::FleetHostConfig,
        config_generation: u64,
        agents: Vec<crate::api::schema::AgentInfo>,
    },
    /// A host published a revisioned session inventory through session.changed.
    FleetSessionInventoryChanged {
        host: crate::config::FleetHostConfig,
        config_generation: u64,
        snapshot: Box<crate::api::schema::SessionSnapshot>,
    },
    /// Latest lines for only the remote terminals linked on the open board.
    BoardRemoteLinesFetched {
        note_path: std::path::PathBuf,
        fleet_generation: u64,
        request_id: u64,
        complete: bool,
        lines: Vec<(crate::board::AgentLink, String)>,
    },
    /// A one-shot request to the host owning a remote pane completed. Fleet
    /// polling remains the only source of rendered remote lifecycle state.
    RemoteApiRequestFinished {
        agent_ref: crate::api::schema::AgentRef,
        response: String,
    },
    /// An authority admission advance finished its durable ledger write.
    AuthorityAcceptanceLedgerPersisted {
        ledger: crate::fleet::AuthorityAcceptanceLedger,
        snapshot: Box<crate::fleet::Snapshot>,
        result: Result<(), String>,
    },
    /// An unchanged poll reloaded and merged durable authority history off the app loop.
    AuthorityAcceptanceLedgerReconciled {
        ledger: crate::fleet::AuthorityAcceptanceLedger,
        snapshot: Box<crate::fleet::Snapshot>,
        result: Result<(), String>,
    },
    /// A remote focus transport advanced a server-owned operation. The wire
    /// transport will construct this event when the next slice lands.
    #[allow(dead_code)]
    RemoteFocusTransition {
        operation_id: String,
        transition: Box<crate::app::remote_focus::RemoteFocusTransition>,
    },
    /// One complete remote terminal frame for a remote focus proxy pane.
    /// Frames are an ordered diff stream; they must be delivered reliably, so
    /// the transport blocks on a full channel rather than dropping one.
    #[allow(dead_code)] // Constructed by the wire transport, which is not yet the default.
    RemoteFocusFrame {
        operation_id: String,
        frame: Box<crate::protocol::TerminalFrame>,
    },
    /// A PTY user-write gate was poisoned. Remote control must be disabled,
    /// but the pane and its child process remain alive.
    #[cfg(unix)]
    RemoteControlGatePoisoned {
        pane_id: PaneId,
    },
    /// A read-only Symphony Temporal workflow poll completed.
    SymphonyWorkflowsRefreshed {
        snapshot: crate::symphony::Snapshot,
    },
    /// The focused repository's scratchpad file changed on disk; reload it.
    ScratchpadChanged,
    /// A note in the notepad directory changed on disk; rescan and reload.
    NotepadChanged,
    /// The focused session's goals file was checked off the render thread.
    GoalsRefreshed {
        generation: u64,
        refresh: crate::goals::GoalsRefresh,
    },
    /// The append-only loop receipt source changed; refresh its cursor.
    LoopRunHistoryChanged,
    /// A bounded background native-metric sample completed.
    StatusMetricsRefreshed {
        snapshot: Option<Box<crate::platform::status_metrics::StatusMetricsSnapshot>>,
    },
    /// A bounded background provider-quota read completed.
    ProviderUsageRefreshed {
        snapshot: Box<crate::provider_usage::ProviderUsageSnapshot>,
    },
    /// A generation-bound local history scan completed off the render thread.
    UsageScanFinished {
        generation: u64,
        result: Result<Box<crate::provider_usage::UsageSnapshot>, String>,
    },
    /// A bounded background reachability probe completed.
    ConnectivityProbed {
        reachable: bool,
    },
    /// The local Codex CLI returned an updated account-aware model catalog.
    HomeCatalogRefreshed {
        catalog: crate::app::home_catalog::HomeProviderCatalog,
    },
    /// Background ref discovery completed for one repository.
    HomeRefsRefreshed {
        repo_root: std::path::PathBuf,
        result: Result<crate::app::home_refs::HomeRefCacheEntry, String>,
    },
    /// GitHub repositories for the active add-project owner finished loading.
    HomeGithubReposRefreshed {
        owner: String,
        result: Result<Vec<String>, String>,
    },
    /// Background provider/integration probes for settings finished.
    ToolProbesFinished {
        probes: Vec<crate::app::probes::ToolProbe>,
    },
    /// A Home dispatch to another machine finished; carries that machine's
    /// pane id on success.
    HomeRemoteSpawnFinished {
        machine: String,
        result: Result<String, String>,
    },
    /// A requested Home checkout completed before agent launch.
    HomeCheckoutFinished {
        plan: Box<crate::app::home::HomeDispatchPlan>,
        result: Result<(), String>,
    },
    /// A pane's child process exited.
    PaneDied {
        pane_id: PaneId,
    },
    /// The pane exit must checkpoint the session before normal removal.
    PaneExitCheckpoint {
        pane_id: PaneId,
    },
    /// Process detection identified an agent before its screen state was confirmed.
    AgentProcessDetected {
        pane_id: PaneId,
        agent: Agent,
        observed_at: Instant,
    },
    /// Fallback detector state changed in a pane.
    StateChanged {
        pane_id: PaneId,
        agent: Option<Agent>,
        state: AgentState,
        visible_blocker: bool,
        visible_working: bool,
        usage_limited: bool,
        process_exited: bool,
        observed_at: Instant,
    },
    /// URLs collected off the PTY parser path on a detection snapshot.
    #[cfg(test)]
    AgentLinksDetected {
        pane_id: PaneId,
        output_urls: Vec<String>,
        osc8_urls: Vec<String>,
        observed_at: std::time::SystemTime,
    },
    /// Arrival-ordered URLs collected off the PTY parser path.
    OrderedAgentLinksDetected {
        pane_id: PaneId,
        links: Vec<crate::agent_state::DetectedAgentLink>,
        observed_at: std::time::SystemTime,
    },
    /// The process composition below a pane changed.
    PaneProcessStateChanged {
        pane_id: PaneId,
        holds_shell: bool,
        stale_resolution: Option<(AgentState, bool)>,
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
        wait: Option<String>,
        eta_s: Option<u64>,
        reported_at: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        closing_block: Option<Box<ClosingBlockReport>>,
    },
    /// Agent session identity was reported without state authority.
    AgentSessionReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        claude_transcript_path: Option<std::path::PathBuf>,
        session_name_write_target: Option<crate::work_title::SessionNameWriteTarget>,
        session_name_target_evaluated: bool,
        session_start_source: Option<String>,
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
    /// Activity after a blocked full-lifecycle report made that report stale.
    HookAuthorityRetired {
        pane_id: PaneId,
        observed_at: Instant,
        suppress_completion: bool,
        answers_human_gate: bool,
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
    TerminalBell {
        pane_id: PaneId,
        count: u16,
    },
    /// A pane child emitted a valid OSC 52 clipboard write. The main loop
    /// re-emits it through herdr's own clipboard writer.
    ClipboardWrite {
        content: Vec<u8>,
    },
    /// Prefix-mode ASCII input-source request, emitted on entering/leaving the ASCII input
    /// realm. `client_id` binds queued server-mode transitions to the client that produced
    /// them; monolithic events leave it empty and apply the switch in-process.
    PrefixInputSource {
        client_id: Option<crate::app::InputSourceId>,
        active: bool,
    },
    /// A pane child reported its shell current directory through terminal
    /// metadata such as OSC 7.
    TerminalCwdReported {
        pane_id: PaneId,
        cwd: std::path::PathBuf,
    },
    /// Background git status refresh completed for workspaces.
    GitStatusRefreshed {
        generation: u64,
        results: Vec<WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, GitStatusCacheEntry)>,
        file_fingerprints: Vec<(std::path::PathBuf, u64)>,
    },
    /// A focused pane's file tree finished loading outside the render thread.
    DockFilesRefreshed {
        generation: u64,
        snapshot: crate::files::FileTreeSnapshot,
    },
    /// A dock diff summary or expanded file finished loading from git.
    DiffRefreshed {
        generation: u64,
        result: Box<crate::app::diff::DiffRefreshResult>,
    },
    /// A configured tab bar status command finished.
    TabBarCommandFinished {
        generation: u64,
        segment_index: usize,
        result: Result<Option<String>, String>,
    },
    /// Background git/gh work-context observations completed for live panes.
    GitWorkContextRefreshed {
        generation: u64,
        observations: Vec<crate::app::work_context_git::GitWorkContextObservation>,
        cache_updates: Vec<(
            crate::app::work_context_git::GitWorkContextCacheKey,
            crate::app::work_context_git::GitWorkContextCacheEntry,
        )>,
    },
    /// Background repo-wide GitHub, Linear, and pane work observations completed.
    WorkIndexRefreshed {
        generation: u64,
        snapshot: Box<crate::work_index::Snapshot>,
        session: crate::work_index::WorkIndexSession,
    },
    /// One bounded batch of GitHub detail observations completed.
    WorkItemDetailRefreshed {
        generation: u64,
        details: Vec<(
            crate::app::state::WorkItemKey,
            crate::work_index::WorkItemDetail,
        )>,
    },
    /// Background foreground-process observations completed for live panes.
    ForegroundProcessesRefreshed {
        generation: u64,
        observations: Vec<crate::app::foreground_process::ForegroundProcessObservation>,
    },
    /// Bounded incremental observations from Claude session transcripts.
    ClaudeSubagentsRefreshed {
        generation: u64,
        observations: Vec<crate::app::claude_subagents::RefreshObservation>,
        stats: crate::app::claude_subagents::BatchStats,
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
}
