use serde::{Deserialize, Serialize};

pub mod accounts;
pub mod agents;
pub mod commands;
pub mod common;
pub mod events;
pub mod fs;
pub mod gram;
pub mod guest;
pub mod integrations;
pub mod machines;
pub(crate) mod operations;
pub mod panes;
pub mod plugins;
pub mod response;
pub mod server;
pub mod session;
pub mod tabs;
pub mod transcript;
pub mod workspaces;
pub mod worktrees;

pub use accounts::*;
pub use agents::*;
pub use commands::*;
pub use common::*;
pub use events::*;
pub use fs::*;
pub use gram::*;
pub use guest::*;
pub use integrations::*;
pub use machines::*;
pub use panes::*;
pub use plugins::*;
pub use response::*;
pub use server::*;
pub use session::*;
pub use tabs::*;
pub use transcript::*;
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
    MachineStatus(EmptyParams) => "machine.status", false, false;
    ServerStagedUpdate(EmptyParams) => "server.staged_update", false, false;
    ServerApplyStagedUpdate(EmptyParams) => "server.apply_staged_update", false, false;
    ServerSshAgentRegister(ServerSshAgentRegisterParams) => "server.ssh_agent.register", false, false;
    ServerAgentManifests(EmptyParams) => "server.agent_manifests", false, false;
    ServerReloadAgentManifests(EmptyParams) => "server.reload_agent_manifests", true, false;
    NotificationShow(NotificationShowParams) => "notification.show", true, false;
    ProductAnnouncementDismiss(ProductAnnouncementDismissParams) => "product_announcement.dismiss", true, true;
    ReleaseNotesDismiss(ReleaseNotesDismissParams) => "release_notes.dismiss", true, true;
    CommandInvoke(CommandInvokeParams) => "command.invoke", true, true;
    NotificationsRegisterDevice(NotificationsRegisterDeviceParams) => "notifications.register_device", false, false;
    NotificationsUnregisterDevice(NotificationsUnregisterDeviceParams) => "notifications.unregister_device", false, false;
    NotificationsRegisterActivity(NotificationsRegisterActivityParams) => "notifications.register_activity", false, false;
    NotificationsUnregisterActivity(NotificationsRegisterActivityParams) => "notifications.unregister_activity", false, false;
    NotificationsStatus(EmptyParams) => "notifications.status", false, false;
    GramSend(GramSendParams) => "gram.send", false, false;
    GramPost(GramPostParams) => "gram.post", false, false;
    GramList(GramListParams) => "gram.list", false, false;
    GramGrab(GramGrabParams) => "gram.grab", false, false;
    GramMarkRead(GramMarkReadParams) => "gram.mark_read", false, false;
    GramDelete(GramDeleteParams) => "gram.delete", false, false;
    GramUploadChunk(GramUploadChunkParams) => "gram.upload_chunk", false, false;
    GramUploadDiscard(GramUploadDiscardParams) => "gram.upload.discard", false, false;
    GramGetFile(GramGetFileParams) => "gram.get_file", false, false;
    GramGetFileChunk(GramGetFileChunkParams) => "gram.get_file_chunk", false, false;
    /// Internal only: the restricted reverse SSH gateway inserts the machine alias.
    GramRelay(GramRelayParams) => "gram.relay", false, false;
    /// Effective Gram relay consent, its source and errors, and gateway state. Local only.
    GramRelayStatus(EmptyParams) => "gram.relay_status", false, false;
    /// Owner only: invite one guest to one live agent. Never reachable by
    /// federation peers or guests.
    GuestInviteCreate(GuestInviteCreateParams) => "guest.invite.create", false, false;
    GuestList(GuestListParams) => "guest.list", false, false;
    GuestRevoke(GuestRevokeParams) => "guest.revoke", false, false;
    GuestUpdate(GuestUpdateParams) => "guest.update", false, false;
    GuestAudit(GuestAuditParams) => "guest.audit", false, false;
    /// Internal only: resolve an agent and its live-agent check for the guest gate.
    #[serde(skip)]
    #[schemars(skip)]
    GuestAgentProbe(GuestAgentProbeParams) => "guest.agent_probe", false, false, cfg(unix);
    ClientWindowTitleSet(ClientWindowTitleSetParams) => "client.window_title.set", false, false;
    ClientWindowTitleClear(EmptyParams) => "client.window_title.clear", false, false;
    ClientShellSurfaceSet(ClientShellSurfaceSetParams) => "client_shell.surface.set", false, true;
    SessionSnapshot(EmptyParams) => "session.snapshot", false, false;
    WorkspaceCreate(WorkspaceCreateParams) => "workspace.create", true, true;
    WorkspaceList(WorkspaceListParams) => "workspace.list", false, false;
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
    AgentList(AgentListParams) => "agent.list", false, false;
    AgentGet(AgentTarget) => "agent.get", false, false;
    AgentRead(AgentReadParams) => "agent.read", false, false;
    AgentTranscript(AgentTranscriptParams) => "agent.transcript", false, false;
    AgentExplain(AgentTarget) => "agent.explain", false, false;
    AgentSendKeys(AgentSendKeysParams) => "agent.send_keys", true, false;
    AgentRename(AgentRenameParams) => "agent.rename", true, false;
    AgentArchive(AgentArchiveParams) => "agent.archive", false, false;
    AgentUnarchive(AgentUnarchiveParams) => "agent.unarchive", false, false;
    AgentForget(AgentForgetParams) => "agent.forget", false, false;
    AgentViewSet(AgentViewSetParams) => "agent.view.set", true, false;
    AgentViewClear(AgentViewClearParams) => "agent.view.clear", true, false;
    AgentFocus(AgentTarget) => "agent.focus", true, false;
    AgentStart(AgentStartParams) => "agent.start", true, false;
    AgentPrompt(AgentPromptParams) => "agent.prompt", true, false;
    AgentWait(AgentWaitParams) => "agent.wait", false, false;
    AgentAnswer(AgentAnswerParams) => "agent.answer", true, false;
    AgentRestart(AgentRestartParams) => "agent.restart", false, false;
    AgentTransferSession(AgentTransferSessionParams) => "agent.transfer_session", false, false;
    AccountsList(EmptyParams) => "accounts.list", false, false;
    AccountsCreate(AccountsCreateParams) => "accounts.create", false, false;
    AccountsRemove(AccountsRemoveParams) => "accounts.remove", false, false;
    AgentKinds(EmptyParams) => "agent.kinds", false, false;
    FsListDir(FsListDirParams) => "fs.list_dir", false, false;
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
    PaneSetPtySize(PaneSetPtySizeParams) => "pane.set_pty_size", true, false;
    PaneList(PaneListParams) => "pane.list", false, false;
    PaneCurrent(PaneCurrentParams) => "pane.current", false, false;
    PaneGet(PaneTarget) => "pane.get", false, false;
    PaneTurns(PaneTurnsParams) => "pane.turns", false, false;
    PaneFocus(PaneTarget) => "pane.focus", true, true;
    PaneInputSet(PaneInputSetParams) => "pane.input.set", true, true;
    PaneLinkActivate(PaneLinkActivateParams) => "pane.link.activate", false, true;
    PaneLinkResolve(PaneLinkActivateParams) => "pane.link.resolve", false, true;
    PaneRename(PaneRenameParams) => "pane.rename", true, true;
    PaneSendText(PaneSendTextParams) => "pane.send_text", false, false;
    PaneSendKeys(PaneSendKeysParams) => "pane.send_keys", false, false;
    PaneSendInput(PaneSendInputParams) => "pane.send_input", false, false;
    PaneRead(PaneReadParams) => "pane.read", false, false;
    PaneHistory(PaneHistoryParams) => "pane.history", false, false;
    #[schemars(skip)]
    PaneStream(PaneStreamParams) => "pane.stream", false, false;
    #[serde(skip)]
    #[schemars(skip)]
    PaneStreamOpen(PaneStreamParams) => "pane.stream.open", false, false;
    #[serde(skip)]
    #[schemars(skip)]
    PaneStreamClose(PaneStreamParams) => "pane.stream.close", false, false;
    /// Internal only: drop one viewer's width lease on every pane, as its
    /// `pane.stream` closing would (the guest gate, on revoke). Unix-only, like
    /// the guest gate that is its only sender.
    #[serde(skip)]
    #[schemars(skip)]
    PanePtyLeaseRelease(PanePtyLeaseReleaseParams) => "pane.pty_lease.release", false, false, cfg(unix);
    #[schemars(skip)]
    PaneInputStream(PaneInputStreamParams) => "pane.input.stream", false, false;
    #[serde(skip)]
    #[schemars(skip)]
    PaneInputStreamOpen(PaneInputStreamParams) => "pane.input.stream.open", false, false;
    // Dotted third segment follows the STREAMING convention (`pane.input.stream`),
    // not the `gram.upload_chunk` snake_case one: it is a streaming channel, and the
    // wire name is what `api_method_name` and the federation audit key on.
    #[schemars(skip)]
    GramUploadStream(GramUploadStreamParams) => "gram.upload.stream", false, false;
    #[serde(skip)]
    #[schemars(skip)]
    GramUploadStreamOpen(GramUploadStreamParams) => "gram.upload.stream.open", false, false;
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
