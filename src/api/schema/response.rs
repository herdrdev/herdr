use serde::{Deserialize, Serialize};

use super::agents::{AgentInfo, AgentStateSnapshot};
use super::common::{ClientWindowTitleReason, NotificationShowReason};
use super::events::EventEnvelope;
use super::fleet::FleetSnapshotInfo;
use super::integrations::{
    IntegrationInstallResult, IntegrationTarget, IntegrationUninstallResult,
};
use super::loops::{LoopFindingInfo, LoopInfo, LoopRunInfo};
use super::panes::{
    LayoutDescription, PaneEdgesResult, PaneFocusDirectionResult, PaneInfo, PaneLayoutSnapshot,
    PaneMoveResult, PaneNeighborResult, PaneProcessInfo, PaneReadResult, PaneResizeResult,
    PaneSendTextIfOutcome, PaneSwapResult, PaneTextPoint, PaneTextRange, PaneZoomResult,
};
use super::plugins::{
    InstalledPluginInfo, PluginActionInfo, PluginCommandLogInfo, PluginInvocationContext,
    PluginPaneInfo,
};
use super::server::ServerCapabilities;
use super::session::SessionSnapshot;
use super::symphony::SymphonyWorkflowInfo;
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;
use super::worktrees::{WorktreeInfo, WorktreeSourceInfo};

fn is_false(value: &bool) -> bool {
    !value
}

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
    SidebarCoverage {
        panes: Vec<super::sidebar::PaneCoverage>,
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
    LoopList {
        loops: Vec<LoopInfo>,
    },
    LoopRunHistory {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_id: Option<String>,
        runs: Vec<LoopRunInfo>,
        skipped_lines: u64,
    },
    LoopFindings {
        /// The producer host the findings were read from.
        host: String,
        /// False when the producer host could not be read; findings is empty.
        reachable: bool,
        /// Pending findings, newest first, capped (MAT-159 AC2).
        findings: Vec<LoopFindingInfo>,
    },
    SymphonyList {
        workflows: Vec<SymphonyWorkflowInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unavailable: Option<String>,
    },
    FleetList {
        snapshot: FleetSnapshotInfo,
    },
    GroupHostSnapshot {
        snapshot: crate::groups::GroupAuthoritySnapshot,
    },
    GroupMutation {
        record: crate::groups::GroupRecord,
        revision: u64,
    },
    PaneGroupSet {
        pane_id: String,
        membership: crate::groups::PaneGroupMembership,
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
    TabPrio {
        prio: super::tabs::TabPrioResult,
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
    AgentState {
        state: AgentStateSnapshot,
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
    AgentSearch {
        hits: Vec<super::agents::AgentSearchHit>,
        #[serde(default, skip_serializing_if = "is_false")]
        partial: bool,
    },
    DayItem {
        item: super::day::DerivedDayItem,
    },
    DayList {
        items: Vec<super::day::DerivedDayItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        load_errors: Vec<super::day::DayLoadError>,
    },
    AgentFocusStarted {
        operation_id: String,
        agent_ref: super::AgentRef,
        state: super::RemoteFocusState,
        proxy_pane_id: String,
    },
    #[schemars(transform = require_remote_focus_status_details)]
    AgentFocusStatus {
        operation_id: String,
        agent_ref: super::AgentRef,
        state: super::RemoteFocusState,
        proxy_pane_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<super::RemoteControlContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ErrorBody>,
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
    PaneTextSend {
        outcome: PaneSendTextIfOutcome,
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
    PaneGraphicsFrameAck {
        sequence: u64,
        revision: u64,
    },
    PaneGraphicsInfo {
        cell_width_px: u32,
        cell_height_px: u32,
        /// True only when this pane is on the currently rendered terminal surface.
        pane_visible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        file_frame_formats: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_max_bytes: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_direct_max_bytes: Option<usize>,
        /// Accepts damage metadata while still consuming a complete canonical file.
        #[serde(default)]
        file_frame_damage: bool,
        #[serde(default)]
        max_layers_per_pane: usize,
        #[serde(default)]
        pixel_mouse: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_transport: Option<String>,
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
    ThemeStatus {
        #[serde(default)]
        host_reported: Option<String>,
        #[serde(rename = "override")]
        appearance_override: crate::config::HostAppearanceOverride,
        effective_appearance: String,
        theme_name: String,
    },
    /// Acknowledgement for the client-shell surface interest lease. This method is new on the
    /// endpoint protocol, so its revision-bearing result can establish an activation floor.
    ClientShellSurfaceSet {
        active: bool,
        projection_revision: u64,
    },
    LiveHandoff {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skipped_panes: Vec<LiveHandoffSkippedPane>,
    },
    Ok {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LiveHandoffSkippedPane {
    pub pane_id: String,
    pub terminal_id: String,
    pub name: String,
}

fn require_remote_focus_status_details(schema: &mut schemars::Schema) {
    let _ = schema.insert(
        "allOf".into(),
        serde_json::json!([
            {
                "if": {"properties": {"state": {"const": "active"}}},
                "then": {
                    "properties": {"context": {"not": {"type": "null"}}},
                    "required": ["context"]
                }
            },
            {
                "if": {"properties": {"state": {"const": "failed"}}},
                "then": {
                    "properties": {"error": {"not": {"type": "null"}}},
                    "required": ["error"]
                }
            }
        ]),
    );
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
    /// Bundled rule ids appended to the active remote manifest because the
    /// upstream catalog does not define them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlaid_rule_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_last_checked_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
