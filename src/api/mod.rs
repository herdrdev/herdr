pub mod client;
mod event_hub;
pub(crate) mod federation;
pub mod federation_manager;
mod federation_relay;
pub mod federation_store;
#[cfg(unix)]
pub(crate) mod gram_gateway;
pub(crate) mod gram_relay;
pub(crate) mod output_registry;
pub(crate) mod peer_channel;
#[cfg(unix)]
pub(crate) mod reverse;
pub(crate) mod reverse_agents;
pub mod schema;
mod server;
pub(crate) use server::{default_capabilities, UploadClaim};
mod status;
mod subscriptions;
mod transport;
mod wait;

pub use event_hub::EventHub;
#[cfg(all(test, unix))]
pub(crate) use server::prefix_remote_agent;
#[cfg(unix)]
pub(crate) use server::serve_guest_stream;
pub use server::ServerHandle;
pub(crate) use server::{api_method_name, start_server_with_stop_control};
pub use status::{read_runtime_status_at, RuntimeStatus};
pub(crate) use transport::{ApiStream, ApiStreamRead};

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use crate::api::schema::Request;

pub const SOCKET_PATH_ENV_VAR: &str = "HERDR_SOCKET_PATH";

/// Opaque identity for this server process lifetime. Changes on every daemon
/// start and is shared by local and federation API responses.
pub(crate) fn server_boot_id() -> &'static str {
    static BOOT_ID: OnceLock<String> = OnceLock::new();
    BOOT_ID.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        format!(
            "{}-{:x}-{nanos:x}",
            crate::persist::machine::get_or_create(),
            std::process::id()
        )
    })
}

pub(crate) fn request_changes_ui(request: &Request) -> bool {
    request.method.operation().changes_ui
}

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
    pub response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}
