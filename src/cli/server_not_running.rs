use std::fmt;
use std::path::Path;

use crate::api::schema::{ErrorBody, ErrorResponse};

/// Marker error signalling a dead API socket. Carries the `ErrorResponse` that
/// should be printed at the edge that finally surfaces the error, so callers
/// that recover (e.g. plugin offline fallback) print nothing. Mirrors
/// `ProtocolMismatchReported`, except printing is deferred because several CLI
/// commands recover from a dead server instead of reporting it.
#[derive(Debug)]
pub(super) struct ServerNotRunningReported {
    pub(super) response: ErrorResponse,
}

impl fmt::Display for ServerNotRunningReported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Delegate to the carried message so pre-existing paths that
        // stringify transport errors still show the actionable text.
        f.write_str(&self.response.error.message)
    }
}

impl std::error::Error for ServerNotRunningReported {}

/// Builds the friendly `server_not_running` ErrorResponse shown when no
/// server is listening on the resolved API socket.
pub(super) fn response(request_id: &str, socket_path: &Path) -> ErrorResponse {
    ErrorResponse {
        id: request_id.to_string(),
        error: ErrorBody {
            code: "server_not_running".into(),
            message: format!(
                "no herdr server is running at {}; run `herdr` to start one",
                socket_path.display()
            ),
        },
    }
}

/// Wraps the response in the recognizable marker WITHOUT printing. The caller
/// that ultimately surfaces the error prints the carried response (see
/// `reported_response`); recovering callers simply drop it.
pub(super) fn reported_error(response: ErrorResponse) -> std::io::Error {
    std::io::Error::other(ServerNotRunningReported { response })
}

pub(super) fn was_reported(err: &std::io::Error) -> bool {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<ServerNotRunningReported>())
        .is_some()
}

/// Returns the `ErrorResponse` carried by a `server_not_running` marker, if any,
/// so the surfacing edge can print it exactly once.
pub(super) fn reported_response(err: &std::io::Error) -> Option<&ErrorResponse> {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<ServerNotRunningReported>())
        .map(|reported| &reported.response)
}
