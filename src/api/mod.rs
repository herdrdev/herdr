pub mod client;
mod event_hub;
pub mod schema;
mod server;
mod status;
mod subscriptions;
mod wait;

pub use event_hub::EventHub;
pub use server::ServerHandle;
pub(crate) use server::{api_method_name, start_server_with_stop_control};
pub use status::{read_runtime_status_at, RuntimeStatus};

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::api::schema::{Method, Request};

pub const SOCKET_PATH_ENV_VAR: &str = "HERDR_SOCKET_PATH";

pub(crate) fn request_changes_ui(request: &Request) -> bool {
    matches!(
        &request.method,
        Method::ServerReloadConfig(_)
            | Method::ServerReloadAgentManifests(_)
            | Method::RegistryPresentationRefresh(_)
            | Method::NotificationShow(_)
            | Method::ProductAnnouncementDismiss(_)
            | Method::ReleaseNotesDismiss(_)
            | Method::CommandInvoke(_)
            | Method::WorkspaceCreate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
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
            | Method::PaneSwap(_)
            | Method::PaneMove(_)
            | Method::PaneZoom(_)
            | Method::PaneFocusDirection(_)
            | Method::PaneResize(_)
            | Method::PaneScroll(_)
            | Method::PaneEditScrollback(_)
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

/// At most one maintenance wakeup may be queued while publications race ahead
/// of the App loop. The handler reads the latest generation, not an old payload.
#[derive(Default)]
pub(crate) struct RegistryPublicationWakeup(std::sync::atomic::AtomicBool);

pub(crate) static REGISTRY_PUBLICATION_WAKEUP: RegistryPublicationWakeup =
    RegistryPublicationWakeup(std::sync::atomic::AtomicBool::new(false));

impl RegistryPublicationWakeup {
    pub(crate) fn notify(&self, before: u64, after: u64, api_tx: &ApiRequestSender) {
        use std::sync::atomic::Ordering;
        if before == after || self.0.swap(true, Ordering::AcqRel) {
            return;
        }
        let (respond_to, _) = std::sync::mpsc::channel();
        if api_tx
            .send(ApiRequestMessage {
                request: Request {
                    id: "internal:registry:published".into(),
                    method: Method::RegistryPresentationRefresh(schema::EmptyParams::default()),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
            .is_err()
        {
            self.begin_refresh();
        }
    }

    pub(crate) fn begin_refresh(&self) {
        // Clear before sampling generation: a publication during the refresh
        // must enqueue another wakeup rather than becoming a lost update.
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}

#[cfg(test)]
mod registry_publication_tests {
    use super::*;

    #[test]
    fn publication_wakeups_coalesce_and_unchanged_reloads_do_not_queue() {
        let wakeup = RegistryPublicationWakeup::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        wakeup.notify(1, 1, &tx);
        assert!(rx.try_recv().is_err());
        wakeup.notify(1, 2, &tx);
        wakeup.notify(2, 3, &tx);
        let message = rx.try_recv().unwrap();
        assert!(matches!(
            message.request.method,
            Method::RegistryPresentationRefresh(_)
        ));
        assert!(request_changes_ui(&message.request));
        assert!(serde_json::to_value(&message.request).is_err());
        assert!(serde_json::from_value::<Request>(serde_json::json!({
            "id": "external", "method": "RegistryPresentationRefresh", "params": {}
        }))
        .is_err());
        assert!(rx.try_recv().is_err());
        wakeup.begin_refresh();
        wakeup.notify(3, 4, &tx);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }
}
