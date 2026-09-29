pub mod client;
mod event_hub;
pub mod schema;
mod server;
mod status;
mod subscriptions;
mod wait;

pub use event_hub::EventHub;
pub(crate) use server::start_server_with_stop_control;
pub use server::{start_server_with_capabilities, ServerHandle};
pub use status::{read_runtime_status_at, RuntimeStatus};

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::api::schema::{Method, Request};

pub const SOCKET_PATH_ENV_VAR: &str = "HERDR_SOCKET_PATH";

pub(crate) fn request_changes_ui(request: &Request) -> bool {
    matches!(
        &request.method,
        Method::ServerReloadConfig(_)
            | Method::ThemeSet(_)
            | Method::ServerReloadAgentManifests(_)
            | Method::NotificationShow(_)
            | Method::WorkspaceCreate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
            | Method::WorkspaceBind(_)
            | Method::WorkspaceMove(_)
            | Method::WorkspaceMoveBlock(_)
            | Method::WorkspaceReportMetadata(_)
            | Method::WorkspaceClose(_)
            | Method::WorktreeCreate(_)
            | Method::WorktreeOpen(_)
            | Method::WorktreeRemove(_)
            | Method::TabCreate(_)
            | Method::TabFocus(_)
            | Method::TabRename(_)
            | Method::TabPrio(_)
            | Method::TabPin(_)
            | Method::TabStar(_)
            | Method::TabMove(_)
            | Method::TabClose(_)
            | Method::LayoutApply(_)
            | Method::LayoutSetSplitRatio(_)
            | Method::AgentRename(_)
            | Method::AgentViewSet(_)
            | Method::AgentViewClear(_)
            | Method::AgentFocus(_)
            | Method::AgentStart(_)
            | Method::AgentPrompt(_)
            | Method::AgentSendKeys(_)
            | Method::PaneSplit(_)
            | Method::PaneSplitCompanion(_)
            | Method::PaneSwap(_)
            | Method::PaneMove(_)
            | Method::PaneZoom(_)
            | Method::PaneFocusDirection(_)
            | Method::PaneResize(_)
            | Method::PaneSnooze(_)
            | Method::PaneUnsnooze(_)
            | Method::PaneFocus(_)
            | Method::PaneInputSet(_)
            | Method::PaneRename(_)
            | Method::PaneGraphicsSet(_)
            | Method::PaneGraphicsClear(_)
            | Method::PaneGraphicsStream(_)
            | Method::PaneGraphicsStreamSet(_)
            | Method::PaneGraphicsStreamDirect(_)
            | Method::PaneGraphicsStreamOpen(_)
            | Method::PaneGraphicsStreamClose(_)
            | Method::PaneReportAgent(_)
            | Method::PaneReportAgentSession(_)
            | Method::PaneReportMetadata(_)
            | Method::PaneClearAgentAuthority(_)
            | Method::PaneReleaseAgent(_)
            | Method::PaneClose(_)
            | Method::GroupCreate(_)
            | Method::GroupRename(_)
            | Method::GroupDelete(_)
            | Method::PaneGroupSet(_)
            | Method::GroupAuthorityMutate(_)
            | Method::PopupClose(_)
            | Method::PluginUnlink(_)
            | Method::PluginDisable(_)
            | Method::PluginActionInvoke(_)
            | Method::PluginPaneOpen(_)
            | Method::PluginPaneFocus(_)
            | Method::PluginPaneClose(_)
    )
}

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
    pub response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
    pub stream_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        AuthorityMutation, AuthorityMutationParams, GroupCreateParams, GroupDeleteParams,
        GroupRenameParams, PaneGroupSetParams, PaneSnoozeParams, PaneTarget,
    };

    #[test]
    fn snooze_transitions_are_classified_as_ui_changes() {
        let snooze = Request {
            id: "snooze".into(),
            method: Method::PaneSnooze(PaneSnoozeParams {
                pane_id: "w1:p1".into(),
                duration_s: Some(60),
                snoozed_until: None,
            }),
        };
        let unsnooze = Request {
            id: "unsnooze".into(),
            method: Method::PaneUnsnooze(PaneTarget {
                pane_id: "w1:p1".into(),
            }),
        };

        assert!(request_changes_ui(&snooze));
        assert!(request_changes_ui(&unsnooze));
    }

    #[test]
    fn group_mutations_are_classified_as_ui_changes() {
        let authority = crate::groups::AuthorityId::from_random_bytes([7; 16]);
        let group_id = crate::groups::GroupId {
            owner: authority.clone(),
            local: 1,
        };
        let pane_set = PaneGroupSetParams {
            pane_id: "w1:p1".into(),
            group_id: Some(group_id.clone()),
            expected_revision: 0,
            expected_pane_authority: Some(authority.clone()),
            expected_pane_incarnation: Some("pane-1".into()),
        };
        let requests = [
            Method::GroupCreate(GroupCreateParams {
                name: "Pod".into(),
                expected_revision: 0,
            }),
            Method::GroupRename(GroupRenameParams {
                group_id: group_id.clone(),
                name: "Renamed".into(),
                expected_revision: 1,
            }),
            Method::GroupDelete(GroupDeleteParams {
                group_id: group_id.clone(),
                expected_revision: 1,
            }),
            Method::PaneGroupSet(pane_set.clone()),
            Method::GroupAuthorityMutate(AuthorityMutationParams {
                expected_authority: authority,
                forwarded: true,
                mutation: AuthorityMutation::PaneGroupSet(pane_set),
            }),
        ];

        for method in requests {
            assert!(request_changes_ui(&Request {
                id: "group-mutation".into(),
                method,
            }));
        }
    }

    #[test]
    fn day_mutations_do_not_trigger_client_rendering() {
        let request = Request {
            id: "day-done".into(),
            method: Method::DayDone(crate::api::schema::DayItemTarget {
                id: "01K5DAYITEM".into(),
            }),
        };

        assert!(!request_changes_ui(&request));
    }
}
