use serde::{Deserialize, Serialize};

pub mod agent_ref;
pub mod agents;
pub mod common;
pub mod day;
pub mod events;
pub mod fleet;
pub mod groups;
pub mod integrations;
pub mod loops;
pub mod panes;
pub mod plugins;
pub mod response;
pub mod server;
pub mod session;
pub mod symphony;
pub mod tabs;
pub mod workspaces;
pub mod worktrees;

pub use agent_ref::*;
pub use agents::*;
pub use common::*;
pub use day::*;
pub use events::*;
pub use fleet::*;
pub use groups::*;
pub use integrations::*;
pub use loops::*;
pub use panes::*;
pub use plugins::*;
pub use response::*;
pub use server::*;
pub use session::*;
pub use symphony::*;
pub use tabs::*;
pub use workspaces::*;
pub use worktrees::*;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Request {
    pub id: String,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params")]
// Request enums are short-lived wire values; keeping variants direct preserves
// the simple serde shape and avoids boxing churn across every caller.
#[allow(clippy::large_enum_variant)]
pub enum Method {
    #[serde(rename = "ping")]
    Ping(PingParams),
    #[serde(rename = "server.stop")]
    ServerStop(EmptyParams),
    #[serde(rename = "server.live_handoff")]
    ServerLiveHandoff(ServerLiveHandoffParams),
    #[serde(rename = "server.reload_config")]
    ServerReloadConfig(EmptyParams),
    #[serde(rename = "theme.status")]
    ThemeStatus(EmptyParams),
    #[serde(rename = "theme.set")]
    ThemeSet(ThemeSetParams),
    #[serde(rename = "server.agent_manifests")]
    ServerAgentManifests(EmptyParams),
    #[serde(rename = "server.reload_agent_manifests")]
    ServerReloadAgentManifests(EmptyParams),
    #[serde(rename = "notification.show")]
    NotificationShow(NotificationShowParams),
    #[serde(rename = "client.window_title.set")]
    ClientWindowTitleSet(ClientWindowTitleSetParams),
    #[serde(rename = "client.window_title.clear")]
    ClientWindowTitleClear(EmptyParams),
    #[serde(rename = "session.snapshot")]
    SessionSnapshot(EmptyParams),
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreateParams),
    #[serde(rename = "workspace.list")]
    WorkspaceList(EmptyParams),
    #[serde(rename = "workspace.get")]
    WorkspaceGet(WorkspaceTarget),
    #[serde(rename = "workspace.focus")]
    WorkspaceFocus(WorkspaceTarget),
    #[serde(rename = "workspace.rename")]
    WorkspaceRename(WorkspaceRenameParams),
    #[serde(rename = "workspace.bind")]
    WorkspaceBind(WorkspaceBindParams),
    #[serde(rename = "workspace.move")]
    WorkspaceMove(WorkspaceMoveParams),
    #[serde(rename = "workspace.move_block")]
    WorkspaceMoveBlock(WorkspaceMoveBlockParams),
    #[serde(rename = "workspace.report_metadata")]
    WorkspaceReportMetadata(WorkspaceReportMetadataParams),
    #[serde(rename = "workspace.close")]
    WorkspaceClose(WorkspaceCloseParams),
    #[serde(rename = "loop.list")]
    LoopList(EmptyParams),
    #[serde(rename = "loop.run_history")]
    LoopRunHistory(LoopRunHistoryParams),
    #[serde(rename = "loop.findings")]
    LoopFindings(EmptyParams),
    #[serde(rename = "symphony.list")]
    SymphonyList(EmptyParams),
    #[serde(rename = "fleet.list")]
    FleetList(EmptyParams),
    #[serde(rename = "group.host_snapshot")]
    GroupHostSnapshot(EmptyParams),
    #[serde(rename = "group.create")]
    GroupCreate(GroupCreateParams),
    #[serde(rename = "group.rename")]
    GroupRename(GroupRenameParams),
    #[serde(rename = "group.delete")]
    GroupDelete(GroupDeleteParams),
    #[serde(rename = "group.authority_mutate")]
    GroupAuthorityMutate(AuthorityMutationParams),
    #[serde(rename = "worktree.list")]
    WorktreeList(WorktreeListParams),
    #[serde(rename = "worktree.create")]
    WorktreeCreate(WorktreeCreateParams),
    #[serde(rename = "worktree.open")]
    WorktreeOpen(WorktreeOpenParams),
    #[serde(rename = "worktree.remove")]
    WorktreeRemove(WorktreeRemoveParams),
    #[serde(rename = "tab.create")]
    TabCreate(TabCreateParams),
    #[serde(rename = "tab.list")]
    TabList(TabListParams),
    #[serde(rename = "tab.get")]
    TabGet(TabTarget),
    #[serde(rename = "tab.focus")]
    TabFocus(TabTarget),
    #[serde(rename = "tab.rename")]
    TabRename(TabRenameParams),
    #[serde(rename = "tab.prio")]
    TabPrio(TabPrioParams),
    #[serde(rename = "tab.pin")]
    TabPin(TabPinParams),
    #[serde(rename = "tab.park")]
    TabPark(TabParkParams),
    #[serde(rename = "tab.star")]
    TabStar(TabStarParams),
    #[serde(rename = "tab.move")]
    TabMove(TabMoveParams),
    #[serde(rename = "tab.close")]
    TabClose(TabTarget),
    #[serde(rename = "agent.list")]
    AgentList(EmptyParams),
    #[serde(rename = "agent.search")]
    AgentSearch(AgentSearchParams),
    #[serde(rename = "agent.get")]
    AgentGet(AgentTarget),
    #[serde(rename = "agent.state")]
    AgentState(AgentStateParams),
    #[serde(rename = "agent.report")]
    AgentReport(AgentReportParams),
    #[serde(rename = "day.add")]
    DayAdd(DayAddParams),
    #[serde(rename = "day.list")]
    DayList(DayListParams),
    #[serde(rename = "day.bind")]
    DayBind(DayBindParams),
    #[serde(rename = "day.link")]
    DayLink(DayLinkParams),
    #[serde(rename = "day.note")]
    DayNote(DayNoteParams),
    #[serde(rename = "day.done")]
    DayDone(DayItemTarget),
    #[serde(rename = "day.dismiss")]
    DayDismiss(DayItemTarget),
    #[serde(rename = "agent.read")]
    AgentRead(AgentReadParams),
    #[serde(rename = "agent.explain")]
    AgentExplain(AgentTarget),
    #[serde(rename = "agent.send_keys")]
    AgentSendKeys(AgentSendKeysParams),
    #[serde(rename = "agent.rename")]
    AgentRename(AgentRenameParams),
    #[serde(rename = "agent.view.set")]
    AgentViewSet(AgentViewSetParams),
    #[serde(rename = "agent.view.clear")]
    AgentViewClear(AgentViewClearParams),
    #[serde(rename = "agent.focus")]
    AgentFocus(AgentFocusParams),
    #[serde(rename = "agent.focus.status")]
    AgentFocusStatus(AgentFocusStatusParams),
    #[serde(rename = "agent.start")]
    AgentStart(AgentStartParams),
    #[serde(rename = "agent.prompt")]
    AgentPrompt(AgentPromptParams),
    #[serde(rename = "agent.wait")]
    AgentWait(AgentWaitParams),
    #[serde(rename = "pane.split")]
    PaneSplit(PaneSplitParams),
    #[serde(rename = "pane.split_companion")]
    PaneSplitCompanion(PaneSplitParams),
    #[serde(rename = "pane.swap")]
    PaneSwap(PaneSwapParams),
    #[serde(rename = "pane.move")]
    PaneMove(PaneMoveParams),
    #[serde(rename = "pane.zoom")]
    PaneZoom(PaneZoomParams),
    #[serde(rename = "pane.layout")]
    PaneLayout(PaneLayoutParams),
    #[serde(rename = "pane.process_info")]
    PaneProcessInfo(PaneProcessInfoParams),
    #[serde(rename = "layout.export")]
    LayoutExport(LayoutExportParams),
    #[serde(rename = "layout.apply")]
    LayoutApply(LayoutApplyParams),
    #[serde(rename = "layout.set_split_ratio")]
    LayoutSetSplitRatio(LayoutSetSplitRatioParams),
    #[serde(rename = "pane.neighbor")]
    PaneNeighbor(PaneNeighborParams),
    #[serde(rename = "pane.edges")]
    PaneEdges(PaneEdgesParams),
    #[serde(rename = "pane.focus_direction")]
    PaneFocusDirection(PaneFocusDirectionParams),
    #[serde(rename = "pane.resize")]
    PaneResize(PaneResizeParams),
    #[serde(rename = "pane.list")]
    PaneList(PaneListParams),
    #[serde(rename = "pane.current")]
    PaneCurrent(PaneCurrentParams),
    #[serde(rename = "pane.get")]
    PaneGet(PaneTarget),
    #[serde(rename = "pane.settle")]
    PaneSettle(PaneTarget),
    #[serde(rename = "pane.unsettle")]
    PaneUnsettle(PaneTarget),
    #[serde(rename = "pane.snooze")]
    PaneSnooze(PaneSnoozeParams),
    #[serde(rename = "pane.unsnooze")]
    PaneUnsnooze(PaneTarget),
    #[serde(rename = "pane.focus")]
    PaneFocus(PaneTarget),
    #[serde(rename = "pane.input.set")]
    PaneInputSet(PaneInputSetParams),
    #[serde(rename = "pane.rename")]
    PaneRename(PaneRenameParams),
    #[serde(rename = "pane.group.set")]
    PaneGroupSet(PaneGroupSetParams),
    #[serde(rename = "pane.work_context.set")]
    PaneWorkContextSet(PaneWorkContextSetParams),
    #[serde(rename = "pane.send_text")]
    PaneSendText(PaneSendTextParams),
    #[serde(rename = "pane.send_text_if")]
    PaneSendTextIf(PaneSendTextIfParams),
    #[serde(rename = "pane.send_keys")]
    PaneSendKeys(PaneSendKeysParams),
    #[serde(rename = "pane.send_input")]
    PaneSendInput(PaneSendInputParams),
    #[serde(rename = "pane.read")]
    PaneRead(PaneReadParams),
    #[serde(rename = "pane.graphics.set")]
    PaneGraphicsSet(PaneGraphicsSetParams),
    #[serde(rename = "pane.graphics.clear")]
    PaneGraphicsClear(PaneGraphicsClearParams),
    #[serde(rename = "pane.graphics.info")]
    PaneGraphicsInfo(PaneTarget),
    #[serde(rename = "pane.graphics.stream")]
    #[schemars(skip)]
    PaneGraphicsStream(PaneGraphicsStreamParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamSet(PaneGraphicsSetParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamDirect(PaneGraphicsDirectParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamOpen(PaneGraphicsStreamParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamClose(PaneGraphicsStreamParams),
    #[serde(rename = "pane.report_agent")]
    PaneReportAgent(PaneReportAgentParams),
    #[serde(rename = "pane.report_agent_session")]
    PaneReportAgentSession(PaneReportAgentSessionParams),
    #[serde(rename = "pane.report_metadata")]
    PaneReportMetadata(PaneReportMetadataParams),
    #[serde(rename = "pane.clear_agent_authority")]
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams),
    #[serde(rename = "pane.release_agent")]
    PaneReleaseAgent(PaneReleaseAgentParams),
    #[serde(rename = "pane.close")]
    PaneClose(PaneTarget),
    #[serde(rename = "popup.close")]
    PopupClose(EmptyParams),
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(EventsSubscribeParams),
    #[serde(rename = "events.wait")]
    EventsWait(EventsWaitParams),
    #[serde(rename = "pane.wait_for_output")]
    PaneWaitForOutput(PaneWaitForOutputParams),
    #[serde(rename = "integration.install")]
    IntegrationInstall(IntegrationInstallParams),
    #[serde(rename = "integration.uninstall")]
    IntegrationUninstall(IntegrationUninstallParams),
    #[serde(rename = "plugin.link")]
    PluginLink(PluginLinkParams),
    #[serde(rename = "plugin.list")]
    PluginList(PluginListParams),
    #[serde(rename = "plugin.unlink")]
    PluginUnlink(PluginUnlinkParams),
    #[serde(rename = "plugin.enable")]
    PluginEnable(PluginSetEnabledParams),
    #[serde(rename = "plugin.disable")]
    PluginDisable(PluginSetEnabledParams),
    #[serde(rename = "plugin.action.list")]
    PluginActionList(PluginActionListParams),
    #[serde(rename = "plugin.action.invoke")]
    PluginActionInvoke(PluginActionInvokeParams),
    #[serde(rename = "plugin.log.list")]
    PluginLogList(PluginLogListParams),
    #[serde(rename = "plugin.pane.open")]
    PluginPaneOpen(PluginPaneOpenParams),
    #[serde(rename = "plugin.pane.focus")]
    PluginPaneFocus(PluginPaneFocusParams),
    #[serde(rename = "plugin.pane.close")]
    PluginPaneClose(PluginPaneCloseParams),
}

#[cfg(test)]
mod tests;
