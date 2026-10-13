use serde::{Deserialize, Serialize};

pub mod agents;
pub mod commands;
pub mod common;
pub mod events;
pub mod integrations;
pub(crate) mod operations;
pub mod panes;
pub mod plugins;
pub mod response;
pub mod server;
pub mod session;
pub mod tabs;
pub mod workspaces;
pub mod worktrees;

pub use agents::*;
pub use commands::*;
pub use common::*;
pub use events::*;
pub use integrations::*;
pub use panes::*;
pub use plugins::*;
pub use response::*;
pub use server::*;
pub use session::*;
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

// Variant order and parameter types preserve the existing wire contract.
// The two flags are independent: presentation impact, then client-shell admission.
operations::define_operations! {
    Ping(PingParams) => "ping", false, false;
    ServerStop(EmptyParams) => "server.stop", false, false;
    ServerLiveHandoff(ServerLiveHandoffParams) => "server.live_handoff", false, false;
    ServerReloadConfig(EmptyParams) => "server.reload_config", true, true;
    ServerSshAgentRegister(ServerSshAgentRegisterParams) => "server.ssh_agent.register", false, false;
    ServerAgentManifests(EmptyParams) => "server.agent_manifests", false, false;
    ServerReloadAgentManifests(EmptyParams) => "server.reload_agent_manifests", true, false;
    NotificationShow(NotificationShowParams) => "notification.show", true, false;
    ProductAnnouncementDismiss(ProductAnnouncementDismissParams) => "product_announcement.dismiss", true, true;
    ReleaseNotesDismiss(ReleaseNotesDismissParams) => "release_notes.dismiss", true, true;
    CommandInvoke(CommandInvokeParams) => "command.invoke", true, true;
    ClientWindowTitleSet(ClientWindowTitleSetParams) => "client.window_title.set", false, false;
    ClientWindowTitleClear(EmptyParams) => "client.window_title.clear", false, false;
    ClientShellSurfaceSet(ClientShellSurfaceSetParams) => "client_shell.surface.set", false, true;
    SessionSnapshot(EmptyParams) => "session.snapshot", false, false;
    WorkspaceCreate(WorkspaceCreateParams) => "workspace.create", true, true;
    WorkspaceList(EmptyParams) => "workspace.list", false, false;
    WorkspaceGet(WorkspaceTarget) => "workspace.get", false, false;
    WorkspaceFocus(WorkspaceTarget) => "workspace.focus", true, true;
    WorkspaceRename(WorkspaceRenameParams) => "workspace.rename", true, true;
    WorkspaceMove(WorkspaceMoveParams) => "workspace.move", true, true;
    WorkspaceMoveBlock(WorkspaceMoveBlockParams) => "workspace.move_block", true, true;
    WorkspaceReportMetadata(WorkspaceReportMetadataParams) => "workspace.report_metadata", true, false;
    WorkspaceClose(WorkspaceCloseParams) => "workspace.close", true, true;
    WorktreeList(WorktreeListParams) => "worktree.list", false, true;
    WorktreeCreate(WorktreeCreateParams) => "worktree.create", true, true;
    WorktreeOpen(WorktreeOpenParams) => "worktree.open", true, true;
    WorktreeRemove(WorktreeRemoveParams) => "worktree.remove", true, true;
    TabCreate(TabCreateParams) => "tab.create", true, true;
    TabList(TabListParams) => "tab.list", false, false;
    TabGet(TabTarget) => "tab.get", false, false;
    TabFocus(TabTarget) => "tab.focus", true, true;
    TabRename(TabRenameParams) => "tab.rename", true, true;
    TabMove(TabMoveParams) => "tab.move", true, true;
    TabClose(TabTarget) => "tab.close", true, true;
    AgentList(EmptyParams) => "agent.list", false, false;
    AgentGet(AgentTarget) => "agent.get", false, false;
    AgentRead(AgentReadParams) => "agent.read", false, false;
    AgentExplain(AgentTarget) => "agent.explain", false, false;
    AgentSendKeys(AgentSendKeysParams) => "agent.send_keys", true, false;
    AgentRename(AgentRenameParams) => "agent.rename", true, false;
    AgentViewSet(AgentViewSetParams) => "agent.view.set", true, false;
    AgentViewClear(AgentViewClearParams) => "agent.view.clear", true, false;
    AgentFocus(AgentTarget) => "agent.focus", true, false;
    AgentStart(AgentStartParams) => "agent.start", true, false;
    AgentPrompt(AgentPromptParams) => "agent.prompt", true, false;
    AgentWait(AgentWaitParams) => "agent.wait", false, false;
    PaneSplit(PaneSplitParams) => "pane.split", true, true;
    PaneSwap(PaneSwapParams) => "pane.swap", true, true;
    PaneMove(PaneMoveParams) => "pane.move", true, false;
    PaneZoom(PaneZoomParams) => "pane.zoom", true, true;
    PaneLayout(PaneLayoutParams) => "pane.layout", false, false;
    PaneProcessInfo(PaneProcessInfoParams) => "pane.process_info", false, false;
    LayoutExport(LayoutExportParams) => "layout.export", false, false;
    LayoutApply(LayoutApplyParams) => "layout.apply", true, false;
    LayoutSetSplitRatio(LayoutSetSplitRatioParams) => "layout.set_split_ratio", true, true;
    PaneNeighbor(PaneNeighborParams) => "pane.neighbor", false, false;
    PaneEdges(PaneEdgesParams) => "pane.edges", false, false;
    PaneFocusDirection(PaneFocusDirectionParams) => "pane.focus_direction", true, true;
    PaneResize(PaneResizeParams) => "pane.resize", true, true;
    PaneScroll(PaneScrollParams) => "pane.scroll", true, true;
    PaneClear(PaneTarget) => "pane.clear", true, true;
    PaneEditScrollback(PaneTarget) => "pane.edit_scrollback", true, true;
    PaneSelectionRead(PaneSelectionReadParams) => "pane.selection.read", false, true;
    PaneCopyMotion(PaneCopyMotionParams) => "pane.copy_motion", false, true;
    PaneCopySearch(PaneCopySearchParams) => "pane.copy_search", false, true;
    PaneList(PaneListParams) => "pane.list", false, false;
    PaneCurrent(PaneCurrentParams) => "pane.current", false, false;
    PaneGet(PaneTarget) => "pane.get", false, false;
    PaneFocus(PaneTarget) => "pane.focus", true, true;
    PaneInputSet(PaneInputSetParams) => "pane.input.set", true, true;
    PaneLinkActivate(PaneLinkActivateParams) => "pane.link.activate", false, true;
    PaneLinkResolve(PaneLinkActivateParams) => "pane.link.resolve", false, true;
    PaneRename(PaneRenameParams) => "pane.rename", true, true;
    PaneSendText(PaneSendTextParams) => "pane.send_text", false, false;
    PaneSendKeys(PaneSendKeysParams) => "pane.send_keys", false, false;
    PaneSendInput(PaneSendInputParams) => "pane.send_input", false, false;
    PaneRead(PaneReadParams) => "pane.read", false, false;
    PaneReportAgent(PaneReportAgentParams) => "pane.report_agent", true, false;
    PaneReportAgentSession(PaneReportAgentSessionParams) => "pane.report_agent_session", true, false;
    PaneReportMetadata(PaneReportMetadataParams) => "pane.report_metadata", true, false;
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams) => "pane.clear_agent_authority", true, false;
    PaneReleaseAgent(PaneReleaseAgentParams) => "pane.release_agent", true, false;
    PaneClose(PaneTarget) => "pane.close", true, true;
    PopupClose(EmptyParams) => "popup.close", true, false;
    EventsSubscribe(EventsSubscribeParams) => "events.subscribe", false, false;
    EventsWait(EventsWaitParams) => "events.wait", false, false;
    PaneWaitForOutput(PaneWaitForOutputParams) => "pane.wait_for_output", false, false;
    IntegrationList(EmptyParams) => "integration.list", false, true;
    IntegrationInstall(IntegrationInstallParams) => "integration.install", false, true;
    IntegrationUninstall(IntegrationUninstallParams) => "integration.uninstall", false, false;
    PluginLink(PluginLinkParams) => "plugin.link", false, false;
    PluginList(PluginListParams) => "plugin.list", false, false;
    PluginUnlink(PluginUnlinkParams) => "plugin.unlink", true, false;
    PluginEnable(PluginSetEnabledParams) => "plugin.enable", false, false;
    PluginDisable(PluginSetEnabledParams) => "plugin.disable", true, false;
    PluginActionList(PluginActionListParams) => "plugin.action.list", false, false;
    PluginActionInvoke(PluginActionInvokeParams) => "plugin.action.invoke", true, false;
    PluginLogList(PluginLogListParams) => "plugin.log.list", false, false;
    PluginPaneOpen(PluginPaneOpenParams) => "plugin.pane.open", true, false;
    PluginPaneFocus(PluginPaneFocusParams) => "plugin.pane.focus", true, false;
    PluginPaneClose(PluginPaneCloseParams) => "plugin.pane.close", true, false;
}

#[cfg(test)]
mod tests;
