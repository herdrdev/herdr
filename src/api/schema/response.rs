use serde::{Deserialize, Serialize};

use super::agents::AgentInfo;
use super::common::{ClientWindowTitleReason, NotificationShowReason};
use super::events::EventEnvelope;
use super::integrations::{
    IntegrationInstallResult, IntegrationTarget, IntegrationUninstallResult,
};
use super::panes::{
    LayoutDescription, PaneEdgesResult, PaneFocusDirectionResult, PaneInfo, PaneLayoutSnapshot,
    PaneMoveResult, PaneNeighborResult, PaneProcessInfo, PaneReadResult, PaneResizeResult,
    PaneSwapResult, PaneTextPoint, PaneTextRange, PaneZoomResult,
};
use super::plugins::{
    InstalledPluginInfo, PluginActionInfo, PluginCommandLogInfo, PluginInvocationContext,
    PluginPaneInfo,
};
use super::server::ServerCapabilities;
use super::session::SessionSnapshot;
use super::tabs::TabInfo;
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
    },
    AgentList {
        agents: Vec<AgentInfo>,
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
    SubscriptionStarted {},
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
    ShepherdCall {
        data: serde_json::Value,
    },
    ShepherdAgentList {
        agents: Vec<shepherd_core::Agent>,
    },
    ShepherdAgentSpawned {
        agent: shepherd_core::Agent,
    },
    ShepherdAgentKilled {
        agent: shepherd_core::Agent,
    },
    ShepherdAgentCleaned {
        cleaned: Vec<String>,
    },
    ShepherdAgentFocused {
        agent: shepherd_core::Agent,
    },
    ShepherdAgentReconciled {
        agents: Vec<shepherd_core::Agent>,
    },
    ShepherdAgentResumed {
        agents: Vec<shepherd_core::Agent>,
    },
    ShepherdTaskList {
        tasks: Vec<shepherd_core::CreatedTask>,
    },
    ShepherdTaskCreated {
        task: shepherd_core::CreatedTask,
    },
    ShepherdTaskArchived {
        archived: bool,
        path: String,
    },
    ShepherdRunList {
        runs: Vec<shepherd_core::RunSummary>,
    },
    ShepherdRunStarted {
        accepted: bool,
        job: String,
        task: String,
    },
    ShepherdGateApproved {
        ok: bool,
        task: String,
        approved: String,
    },
    ShepherdGateRejected {
        ok: bool,
        task: String,
        rejected: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    ShepherdSkillList {
        skills: Vec<shepherd_core::skills::Skill>,
    },
    ShepherdGitStatus {
        status: shepherd_core::git::GitRepoStatus,
    },
    ShepherdSnapshotCaptured {
        result: shepherd_core::snapshot::CaptureResult,
    },
    ShepherdSnapshotList {
        snapshots: Vec<shepherd_core::snapshot::SnapshotInfo>,
    },
    ShepherdSnapshotShown {
        result: shepherd_core::snapshot::ShowResult,
    },
    ShepherdSnapshotDiffed {
        diff: shepherd_core::snapshot::DiffResult,
    },
    ShepherdSnapshotRestored {
        result: shepherd_core::snapshot::RestoreResult,
    },
    ShepherdSnapshotDeleted {
        result: shepherd_core::snapshot::DeleteResult,
    },
    ShepherdSnapshotPruned {
        result: shepherd_core::snapshot::PruneResult,
    },
    ShepherdPatternList {
        patterns: Vec<shepherd_core::patterns::PatternSummary>,
    },
    ShepherdPatternDetail {
        pattern: shepherd_core::patterns::PatternDetail,
    },
    ShepherdPatternSaved {
        ok: bool,
        name: String,
        path: String,
        source: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        problems: Vec<shepherd_core::patterns::Problem>,
    },
    ShepherdPatternDeleted {
        ok: bool,
        name: String,
        deleted_source: String,
    },
    ShepherdPatternValidated {
        valid: bool,
        problems: Vec<shepherd_core::patterns::Problem>,
    },
    ShepherdTopicList {
        topics: Vec<shepherd_core::bus::TopicSummary>,
    },
    ShepherdTopicCreated {
        topic: shepherd_core::bus::Topic,
    },
    ShepherdMessagePublished {
        message: shepherd_core::bus::Message,
    },
    ShepherdTopicMessages {
        messages: Vec<shepherd_core::bus::Message>,
    },
    ShepherdConsumerGroups {
        groups: Vec<shepherd_core::bus::ConsumerGroupSummary>,
    },
    ShepherdPatternRunStarted {
        run: shepherd_core::service::patterns_run::PatternRun,
    },
    ShepherdPatternRunList {
        runs: Vec<shepherd_core::service::patterns_run::PatternRunSummary>,
    },
    ShepherdPatternRunDetail {
        run: shepherd_core::service::patterns_run::PatternRun,
    },
    ShepherdPatternRunPaused {
        run: shepherd_core::service::patterns_run::PatternRun,
    },
    ShepherdPatternRunResumed {
        run: shepherd_core::service::patterns_run::PatternRun,
    },
    ShepherdPatternRunStopped {
        run: shepherd_core::service::patterns_run::PatternRun,
    },
    Ok {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentManifestInfo {
    pub agent: String,
    pub source: String,
    pub source_kind: String,
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
