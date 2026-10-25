use serde::{Deserialize, Serialize};

use super::agents::{AgentAnswerStatus, AgentInfo, AgentPromptDelivery};
use super::common::{ClientWindowTitleReason, NotificationShowReason, NotificationsStatusState};
use super::events::EventEnvelope;
use super::gram::GramMessageInfo;
use super::integrations::{
    IntegrationInstallResult, IntegrationTarget, IntegrationUninstallResult,
};
use super::panes::{
    LayoutDescription, PaneEdgesResult, PaneFocusDirectionResult, PaneInfo, PaneLayoutSnapshot,
    PaneMoveResult, PaneNeighborResult, PaneProcessInfo, PaneReadResult, PaneResizeResult,
    PaneSwapResult, PaneTextPoint, PaneTextRange, PaneTurnsResult, PaneZoomResult,
};
use super::plugins::{
    InstalledPluginInfo, PluginActionInfo, PluginCommandLogInfo, PluginInvocationContext,
    PluginPaneInfo,
};
use super::server::ServerCapabilities;
use super::session::SessionSnapshot;
use super::tabs::TabInfo;
use super::transcript::{
    PendingAsk, TranscriptEntry, TranscriptModel, TranscriptPlanItem, TranscriptRecap,
    TranscriptWorking,
};
use super::workspaces::WorkspaceInfo;
use super::worktrees::{WorktreeInfo, WorktreeSourceInfo};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuccessResponse {
    pub id: String,
    pub result: ResponseResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorResponse {
    pub id: String,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseResult {
    Pong {
        version: String,
        protocol: u32,
        #[serde(default)]
        capabilities: Option<ServerCapabilities>,
    },
    SessionSnapshot {
        snapshot: Box<SessionSnapshot>,
    },
    WorkspaceInfo {
        workspace: WorkspaceInfo,
    },
    WorkspaceCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    WorkspaceList {
        workspaces: Vec<WorkspaceInfo>,
    },
    WorktreeList {
        source: WorktreeSourceInfo,
        worktrees: Vec<WorktreeInfo>,
    },
    WorktreeCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
    },
    WorktreeOpened {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
        already_open: bool,
    },
    WorktreeRemoved {
        workspace_id: String,
        path: String,
        forced: bool,
    },
    TabInfo {
        tab: TabInfo,
    },
    TabCreated {
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    TabList {
        tabs: Vec<TabInfo>,
    },
    AgentInfo {
        agent: AgentInfo,
    },
    AgentStarted {
        agent: AgentInfo,
        argv: Vec<String>,
    },
    AgentPrompted {
        agent: AgentInfo,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<AgentPromptDelivery>,
    },
    AgentAnswer {
        status: AgentAnswerStatus,
    },
    AgentList {
        agents: Vec<AgentInfo>,
        /// Install-stable identity of the daemon that produced this response.
        /// Older peers omit it. It is a pinning value, not an authenticator.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_machine_id: Option<String>,
        /// Opaque identity of the daemon process that produced this response.
        /// Changes on restart and fences replies from an earlier remote boot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_boot_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_protocol: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_capabilities: Option<ServerCapabilities>,
    },
    MachineStatus {
        machines: std::collections::BTreeMap<String, super::CoordinatorMachineStatus>,
    },
    AccountsList {
        accounts: Vec<super::accounts::AccountInfo>,
    },
    AgentKinds {
        kinds: Vec<AgentKindInfo>,
    },
    DirList {
        path: String,
        entries: Vec<DirEntryInfo>,
    },
    AgentView {
        active: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    PaneInfo {
        pane: PaneInfo,
    },
    PaneList {
        panes: Vec<PaneInfo>,
    },
    PaneCurrent {
        pane: PaneInfo,
    },
    PaneTurns {
        turns: PaneTurnsResult,
    },
    PaneSwap {
        swap: PaneSwapResult,
    },
    PaneMove {
        move_result: PaneMoveResult,
    },
    PaneZoom {
        zoom: PaneZoomResult,
    },
    PaneLayout {
        layout: PaneLayoutSnapshot,
    },
    PaneProcessInfo {
        process_info: PaneProcessInfo,
    },
    LayoutExport {
        layout: LayoutDescription,
    },
    LayoutApply {
        layout: LayoutDescription,
    },
    LayoutSplitRatioSet {
        layout: LayoutDescription,
    },
    PaneNeighbor {
        neighbor: PaneNeighborResult,
    },
    PaneEdges {
        edges: PaneEdgesResult,
    },
    PaneFocusDirection {
        focus: PaneFocusDirectionResult,
    },
    PaneResize {
        resize: PaneResizeResult,
    },
    PanePtySize {
        pane_id: String,
        cols: u16,
        rows: u16,
        locked: bool,
    },
    /// Rows directly above a `pane.history` cursor's row, oldest first, as
    /// base64 ANSI rendered like a keyframe's `history_b64`. `cursor` pages
    /// further back and is `null` once the start of retained history is reached.
    PaneHistory {
        pane_id: String,
        history_b64: String,
        rows: u32,
        cursor: Option<String>,
    },
    /// One page of an omp agent's conversation (`agent.transcript`), entries
    /// oldest first. `before` pages older turns and is `null` at the start;
    /// `after` returns what was appended or changed since this page.
    AgentTranscript {
        agent: String,
        pane_id: String,
        session_id: String,
        entries: Vec<TranscriptEntry>,
        before: Option<String>,
        after: String,
        pending_ask: Option<PendingAsk>,
        working: Option<TranscriptWorking>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<TranscriptModel>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<Vec<TranscriptPlanItem>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recap: Option<TranscriptRecap>,
    },
    StreamStarted {
        pane_id: String,
        epoch: u64,
        cols: u16,
        rows: u16,
        base_seq: u64,
        resync: bool,
    },
    PaneRead {
        read: PaneReadResult,
    },
    PaneSelection {
        pane_id: String,
        text: String,
    },
    PaneCopyMotion {
        pane_id: String,
        cursor: PaneTextPoint,
        content_revision: u64,
    },
    PaneCopySearch {
        pane_id: String,
        content_revision: u64,
        matches: Vec<PaneTextRange>,
        total: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_global: Option<u64>,
    },
    AgentExplain {
        explain: serde_json::Value,
    },
    SubscriptionStarted {
        /// Entries an `events_v2` request skipped because their pane does not
        /// exist. Omitted when every entry was subscribed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rejected: Vec<super::events::SubscriptionRejection>,
    },
    WaitMatched {
        event: EventEnvelope,
    },
    OutputMatched {
        pane_id: String,
        revision: u64,
        matched_line: Option<String>,
        read: PaneReadResult,
    },
    NotificationShow {
        shown: bool,
        reason: NotificationShowReason,
    },
    /// A new guest invite. The secret exists only inside the two links.
    GuestInviteCreated {
        invite: super::guest::GuestInviteInfo,
        url: String,
        web_url: String,
    },
    /// Guests, invites and relay link state. Never includes secrets or keys.
    GuestList {
        guests: Vec<super::guest::GuestInfo>,
        invites: Vec<super::guest::GuestInviteInfo>,
        link: super::guest::GuestLinkInfo,
    },
    GuestRevoked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guest_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invite_id: Option<String>,
        /// Live guest streams closed by this revoke.
        closed_streams: usize,
    },
    /// The guest after `guest.update`.
    GuestUpdated {
        guest: super::guest::GuestInfo,
    },
    GuestAudit {
        entries: Vec<super::guest::GuestAuditEntry>,
    },
    /// Internal: the guest gate's agent lookup plus its live-agent check.
    #[cfg(unix)]
    #[schemars(skip)]
    GuestAgentProbed {
        /// Public pane id currently showing the terminal.
        pane_id: String,
        /// `None` when the pane no longer hosts an agent.
        agent: Option<AgentInfo>,
        running: bool,
    },
    /// Effective Gram relay policy for both roles. Never includes secrets.
    GramRelayStatus {
        coordinator: super::gram::GramRelayCoordinatorStatus,
        remote: super::gram::GramRelayRemoteStatus,
    },
    /// Remote push readiness. Counts only; never tokens, capabilities, or key material.
    NotificationsStatus {
        state: NotificationsStatusState,
        mode: crate::config::PushMode,
        relay_url: String,
        /// Registered push devices.
        devices: u64,
        /// Registered devices that carry a relay capability.
        relay_devices: u64,
    },
    ClientWindowTitle {
        changed: bool,
        reason: ClientWindowTitleReason,
    },
    IntegrationList {
        integrations: Vec<super::integrations::IntegrationInfo>,
    },
    IntegrationInstall {
        target: IntegrationTarget,
        details: IntegrationInstallResult,
    },
    IntegrationUninstall {
        target: IntegrationTarget,
        details: IntegrationUninstallResult,
    },
    AgentManifestReload {
        manifests: Vec<AgentManifestInfo>,
    },
    AgentManifestStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_check_unix: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_result: Option<String>,
        manifests: Vec<AgentManifestInfo>,
    },
    PluginLinked {
        plugin: InstalledPluginInfo,
    },
    PluginList {
        plugins: Vec<InstalledPluginInfo>,
    },
    PluginUnlinked {
        plugin_id: String,
        removed: bool,
    },
    PluginEnabled {
        plugin: InstalledPluginInfo,
    },
    PluginDisabled {
        plugin: InstalledPluginInfo,
    },
    PluginActionList {
        actions: Vec<PluginActionInfo>,
    },
    PluginActionInvoked {
        action: PluginActionInfo,
        context: PluginInvocationContext,
        log: PluginCommandLogInfo,
    },
    PaneLinkResolved {
        regions: Vec<super::panes::PaneLinkRegion>,
    },
    PaneLinkActivated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        handled: bool,
    },
    PluginLogList {
        logs: Vec<PluginCommandLogInfo>,
    },
    PluginPaneOpened {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneFocused {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneClosed {
        pane_id: String,
    },
    ConfigReload {
        status: crate::config::ConfigReloadStatus,
        diagnostics: Vec<String>,
    },
    /// Acknowledgement for the client-shell surface interest lease. This method is new on the
    /// endpoint protocol, so its revision-bearing result can establish an activation floor.
    ClientShellSurfaceSet {
        active: bool,
        projection_revision: u64,
    },
    GramSent {
        message: GramMessageInfo,
        /// Install-stable identity of the store this send landed in (the responding
        /// daemon's `machine` id). Lets a sender see WHICH store its gram was
        /// written to, so a send that lands where the owner never reads becomes
        /// visible instead of silent. See issue #98.
        store_id: String,
    },
    GramList {
        messages: Vec<GramMessageInfo>,
        /// Install-stable identity of the store these messages were read from (the
        /// responding daemon's `machine` id). Present even when `messages` is empty,
        /// so a reader can tell which store it is looking at when it appears empty.
        store_id: String,
        /// Fingerprint of the whole filtered list this answer was cut from — NOT of
        /// the page. Send it back as `if_unchanged_digest` to make the next head poll
        /// conditional. Computed over the serialized messages themselves, so it
        /// changes exactly when the answer would differ and there is no field list to
        /// keep in step as `GramMessageInfo` grows.
        digest: String,
        /// Whether messages older than this page remain. Always `false` for an
        /// unpaged answer, which by definition already reaches the oldest message.
        has_more: bool,
        /// Unread count over the WHOLE filtered list, never just the page: the app's
        /// badge and its Read-all affordance must stay correct while the reader holds
        /// only the newest window.
        unread_count: usize,
    },
    /// `gram.list` with an `if_unchanged_digest` that still matches: nothing has
    /// changed, so the messages are omitted entirely. A client holding that digest
    /// already has the list; one that never sent the parameter never receives this.
    GramListUnchanged {
        store_id: String,
        digest: String,
    },
    GramGrabbed {
        message: GramMessageInfo,
    },
    GramFileContent {
        name: String,
        mime: String,
        size: u64,
        data_base64: String,
    },
    /// One bounded range of an attachment. `sha256` and `size` describe the
    /// complete committed file, so a downloader can reject truncation/corruption.
    GramFileChunk {
        name: String,
        mime: String,
        size: u64,
        sha256: String,
        offset: u64,
        data_base64: String,
    },
    /// `gram.upload.discard` — whether a staged upload with that id existed and
    /// was deleted. `false` (unknown, already discarded, or already attached to a
    /// message) is a normal answer, not an error.
    GramUploadDiscarded {
        discarded: bool,
    },
    /// `server.staged_update` — the running daemon version/protocol/commit, plus the staged (built
    /// but not-yet-running) build if one is available. `staged` present with a `sha` different from
    /// `running_sha` means an update is ready to apply. `running_sha` is the running binary's short
    /// git commit, absent on a build with no git context.
    StagedUpdate {
        running_version: String,
        running_protocol: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        running_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        staged: Option<StagedBuildInfo>,
    },
    Ok {},
}

/// A staged (built, not-yet-running) daemon build, as returned to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StagedBuildInfo {
    pub version: String,
    pub sha: String,
    pub built_at: String,
}

/// One known agent kind and whether its interactive harness binary is installed
/// on the daemon's `$PATH`, as returned by `agent.kinds`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentKindInfo {
    pub kind: String,
    pub installed: bool,
}

/// One entry in a directory listing returned by `fs.list_dir`. `name` is the
/// entry's file name only (not a full path); `is_dir` is resolved through
/// symlinks, so a symlink to a directory reports `true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DirEntryInfo {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentManifestInfo {
    pub agent: String,
    pub source: String,
    pub source_kind: String,
    /// The active manifest declares a composer observation region; runtime
    /// visibility can still prevent confirmation of a particular submission.
    #[serde(default)]
    pub submission_verification_supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_remote_version: Option<String>,
    pub local_override_shadowing_remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_last_checked_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
