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
    Ping(PingParams) => "ping", false, false, true;
    ServerStop(EmptyParams) => "server.stop", false, false, false;
    ServerLiveHandoff(ServerLiveHandoffParams) => "server.live_handoff", false, false, false;
    ServerReloadConfig(EmptyParams) => "server.reload_config", true, true, false;
    ServerSshAgentRegister(ServerSshAgentRegisterParams) => "server.ssh_agent.register", false, false, false;
    ServerAgentManifests(EmptyParams) => "server.agent_manifests", false, false, false;
    ServerReloadAgentManifests(EmptyParams) => "server.reload_agent_manifests", true, false, false;
    NotificationShow(NotificationShowParams) => "notification.show", true, false, false;
    ProductAnnouncementDismiss(ProductAnnouncementDismissParams) => "product_announcement.dismiss", true, true, false;
    ReleaseNotesDismiss(ReleaseNotesDismissParams) => "release_notes.dismiss", true, true, false;
    CommandInvoke(CommandInvokeParams) => "command.invoke", true, true, false;
    ClientWindowTitleSet(ClientWindowTitleSetParams) => "client.window_title.set", false, false, false;
    ClientWindowTitleClear(EmptyParams) => "client.window_title.clear", false, false, false;
    ClientShellSurfaceSet(ClientShellSurfaceSetParams) => "client_shell.surface.set", false, true, false;
    SessionSnapshot(EmptyParams) => "session.snapshot", false, false, false;
    WorkspaceCreate(WorkspaceCreateParams) => "workspace.create", true, true, false;
    WorkspaceList(EmptyParams) => "workspace.list", false, false, false;
    WorkspaceGet(WorkspaceTarget) => "workspace.get", false, false, false;
    WorkspaceFocus(WorkspaceTarget) => "workspace.focus", true, true, false;
    WorkspaceRename(WorkspaceRenameParams) => "workspace.rename", true, true, false;
    WorkspaceMove(WorkspaceMoveParams) => "workspace.move", true, true, false;
    WorkspaceMoveBlock(WorkspaceMoveBlockParams) => "workspace.move_block", true, true, false;
    WorkspaceReportMetadata(WorkspaceReportMetadataParams) => "workspace.report_metadata", true, false, false;
    WorkspaceClose(WorkspaceCloseParams) => "workspace.close", true, true, false;
    WorktreeList(WorktreeListParams) => "worktree.list", false, true, false;
    WorktreeCreate(WorktreeCreateParams) => "worktree.create", true, true, false;
    WorktreeOpen(WorktreeOpenParams) => "worktree.open", true, true, false;
    WorktreeRemove(WorktreeRemoveParams) => "worktree.remove", true, true, false;
    TabCreate(TabCreateParams) => "tab.create", true, true, false;
    TabList(TabListParams) => "tab.list", false, false, false;
    TabGet(TabTarget) => "tab.get", false, false, false;
    TabFocus(TabTarget) => "tab.focus", true, true, false;
    TabRename(TabRenameParams) => "tab.rename", true, true, false;
    TabMove(TabMoveParams) => "tab.move", true, true, false;
    TabClose(TabTarget) => "tab.close", true, true, false;
    AgentList(EmptyParams) => "agent.list", false, false, false;
    AgentGet(AgentTarget) => "agent.get", false, false, false;
    AgentRead(AgentReadParams) => "agent.read", false, false, false;
    AgentExplain(AgentTarget) => "agent.explain", false, false, false;
    AgentSendKeys(AgentSendKeysParams) => "agent.send_keys", true, false, false;
    AgentRename(AgentRenameParams) => "agent.rename", true, false, false;
    AgentViewSet(AgentViewSetParams) => "agent.view.set", true, false, false;
    AgentViewClear(AgentViewClearParams) => "agent.view.clear", true, false, false;
    AgentFocus(AgentTarget) => "agent.focus", true, false, false;
    AgentStart(AgentStartParams) => "agent.start", true, false, false;
    AgentPrompt(AgentPromptParams) => "agent.prompt", true, false, false;
    AgentWait(AgentWaitParams) => "agent.wait", false, false, false;
    PaneSplit(PaneSplitParams) => "pane.split", true, true, false;
    PaneSwap(PaneSwapParams) => "pane.swap", true, true, false;
    PaneMove(PaneMoveParams) => "pane.move", true, false, false;
    PaneZoom(PaneZoomParams) => "pane.zoom", true, true, false;
    PaneLayout(PaneLayoutParams) => "pane.layout", false, false, false;
    PaneProcessInfo(PaneProcessInfoParams) => "pane.process_info", false, false, false;
    LayoutExport(LayoutExportParams) => "layout.export", false, false, false;
    LayoutApply(LayoutApplyParams) => "layout.apply", true, false, false;
    LayoutSetSplitRatio(LayoutSetSplitRatioParams) => "layout.set_split_ratio", true, true, false;
    PaneNeighbor(PaneNeighborParams) => "pane.neighbor", false, false, false;
    PaneEdges(PaneEdgesParams) => "pane.edges", false, false, false;
    PaneFocusDirection(PaneFocusDirectionParams) => "pane.focus_direction", true, true, false;
    PaneResize(PaneResizeParams) => "pane.resize", true, true, false;
    PaneScroll(PaneScrollParams) => "pane.scroll", true, true, false;
    PaneClear(PaneTarget) => "pane.clear", true, true, false;
    PaneEditScrollback(PaneTarget) => "pane.edit_scrollback", true, true, false;
    PaneSelectionRead(PaneSelectionReadParams) => "pane.selection.read", false, true, false;
    PaneCopyMotion(PaneCopyMotionParams) => "pane.copy_motion", false, true, false;
    PaneCopySearch(PaneCopySearchParams) => "pane.copy_search", false, true, false;
    PaneList(PaneListParams) => "pane.list", false, false, false;
    PaneCurrent(PaneCurrentParams) => "pane.current", false, false, false;
    PaneGet(PaneTarget) => "pane.get", false, false, false;
    PaneFocus(PaneTarget) => "pane.focus", true, true, false;
    PaneInputSet(PaneInputSetParams) => "pane.input.set", true, true, false;
    PaneLinkActivate(PaneLinkActivateParams) => "pane.link.activate", false, true, false;
    PaneLinkResolve(PaneLinkActivateParams) => "pane.link.resolve", false, true, false;
    PaneRename(PaneRenameParams) => "pane.rename", true, true, false;
    PaneSendText(PaneSendTextParams) => "pane.send_text", false, false, false;
    PaneSendKeys(PaneSendKeysParams) => "pane.send_keys", false, false, false;
    PaneSendInput(PaneSendInputParams) => "pane.send_input", false, false, false;
    PaneRead(PaneReadParams) => "pane.read", false, false, false;
    PaneReportAgent(PaneReportAgentParams) => "pane.report_agent", true, false, true;
    PaneReportAgentSession(PaneReportAgentSessionParams) => "pane.report_agent_session", true, false, true;
    PaneReportMetadata(PaneReportMetadataParams) => "pane.report_metadata", true, false, true;
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams) => "pane.clear_agent_authority", true, false, true;
    PaneReleaseAgent(PaneReleaseAgentParams) => "pane.release_agent", true, false, true;
    PaneClose(PaneTarget) => "pane.close", true, true, false;
    PopupClose(EmptyParams) => "popup.close", true, false, false;
    EventsSubscribe(EventsSubscribeParams) => "events.subscribe", false, false, false;
    EventsWait(EventsWaitParams) => "events.wait", false, false, false;
    PaneWaitForOutput(PaneWaitForOutputParams) => "pane.wait_for_output", false, false, false;
    IntegrationList(EmptyParams) => "integration.list", false, true, false;
    IntegrationInstall(IntegrationInstallParams) => "integration.install", false, true, false;
    IntegrationUninstall(IntegrationUninstallParams) => "integration.uninstall", false, false, false;
    PluginLink(PluginLinkParams) => "plugin.link", false, false, false;
    PluginList(PluginListParams) => "plugin.list", false, false, false;
    PluginUnlink(PluginUnlinkParams) => "plugin.unlink", true, false, false;
    PluginEnable(PluginSetEnabledParams) => "plugin.enable", false, false, false;
    PluginDisable(PluginSetEnabledParams) => "plugin.disable", true, false, false;
    PluginActionList(PluginActionListParams) => "plugin.action.list", false, false, false;
    PluginActionInvoke(PluginActionInvokeParams) => "plugin.action.invoke", true, false, false;
    PluginLogList(PluginLogListParams) => "plugin.log.list", false, false, false;
    PluginPaneOpen(PluginPaneOpenParams) => "plugin.pane.open", true, false, false;
    PluginPaneFocus(PluginPaneFocusParams) => "plugin.pane.focus", true, false, false;
    PluginPaneClose(PluginPaneCloseParams) => "plugin.pane.close", true, false, false;
}

#[cfg(test)]
mod tests;
