use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crate::api::client::ApiClientError;
use crate::api::schema::{Request, ResponseResult};
use crate::ipc::LocalStream;
use crate::protocol::ClientMessage;

use super::shell::ClientShellEndpointError;

const ENDPOINT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

struct InFlightCommand {
    boot_id: String,
    request_id: String,
    response: Vec<u8>,
    sent_at: Instant,
    timed_out: bool,
}

pub(super) struct EndpointCommandResult {
    pub(super) boot_id: String,
    pub(super) request_id: String,
    pub(super) result: Result<ResponseResult, ClientShellEndpointError>,
}

#[derive(Default)]
pub(super) struct EndpointCommands {
    queued: VecDeque<(String, Box<Request>)>,
    in_flight: Option<InFlightCommand>,
}

impl EndpointCommands {
    pub(super) fn enqueue(&mut self, boot_id: String, request: Box<Request>) {
        self.queued.push_back((boot_id, request));
    }

    pub(super) fn send_next(&mut self, stream: &mut LocalStream) -> io::Result<()> {
        if self.in_flight.is_some() {
            return Ok(());
        }
        let Some((boot_id, request)) = self.queued.pop_front() else {
            return Ok(());
        };
        let request_id = request.id.clone();
        let request = serde_json::to_string(&request)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        super::write_to_server(
            stream,
            &ClientMessage::ClientShellEndpointRequest {
                boot_id: boot_id.clone(),
                request,
            },
        )?;
        self.in_flight = Some(InFlightCommand {
            boot_id,
            request_id,
            response: Vec::new(),
            sent_at: Instant::now(),
            timed_out: false,
        });
        Ok(())
    }

    pub(super) fn expire(&mut self, now: Instant) -> Option<EndpointCommandResult> {
        let command = self.in_flight.as_mut()?;
        if command.timed_out
            || now.saturating_duration_since(command.sent_at) < ENDPOINT_COMMAND_TIMEOUT
        {
            return None;
        }
        command.timed_out = true;
        Some(EndpointCommandResult {
            boot_id: command.boot_id.clone(),
            request_id: command.request_id.clone(),
            result: Err(ClientShellEndpointError {
                code: Some("endpoint_timeout".into()),
                message: "this server did not respond to the action".into(),
            }),
        })
    }

    pub(super) fn receive_chunk(
        &mut self,
        response_boot_id: &str,
        response_request_id: &str,
        final_chunk: bool,
        data: Vec<u8>,
    ) -> io::Result<Option<EndpointCommandResult>> {
        let Some(in_flight) = self.in_flight.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "endpoint response arrived without an in-flight command",
            ));
        };
        if response_boot_id != in_flight.boot_id || response_request_id != in_flight.request_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "endpoint response correlation did not match the in-flight command",
            ));
        }
        in_flight.response.extend(data);
        if !final_chunk {
            return Ok(None);
        }

        let in_flight = self.in_flight.take().expect("checked in-flight command");
        let response = String::from_utf8(in_flight.response)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let result = parse_response(&in_flight.request_id, &response);
        Ok(Some(EndpointCommandResult {
            boot_id: in_flight.boot_id,
            request_id: in_flight.request_id,
            result,
        }))
    }
}

fn parse_response(
    expected_id: &str,
    response: &str,
) -> Result<ResponseResult, ClientShellEndpointError> {
    let value = serde_json::from_str(response).map_err(|error| ClientShellEndpointError {
        code: None,
        message: format!("invalid endpoint response: {error}"),
    })?;
    match crate::api::client::parse_response_value(value) {
        Ok(response) if response.id == expected_id => Ok(response.result),
        Ok(response) => Err(ClientShellEndpointError {
            code: None,
            message: format!(
                "endpoint response id {:?} did not match {expected_id:?}",
                response.id
            ),
        }),
        Err(ApiClientError::ErrorResponse(response)) if response.id == expected_id => {
            Err(ClientShellEndpointError {
                code: Some(response.error.code),
                message: response.error.message,
            })
        }
        Err(ApiClientError::ErrorResponse(response)) => Err(ClientShellEndpointError {
            code: None,
            message: format!(
                "endpoint error id {:?} did not match {expected_id:?}",
                response.id
            ),
        }),
        Err(error) => Err(ClientShellEndpointError {
            code: None,
            message: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ResponseResult, SuccessResponse};

    fn commands_with_in_flight() -> EndpointCommands {
        EndpointCommands {
            in_flight: Some(InFlightCommand {
                boot_id: "boot-a".into(),
                request_id: "request-a".into(),
                response: Vec::new(),
                sent_at: Instant::now(),
                timed_out: false,
            }),
            ..EndpointCommands::default()
        }
    }

    #[test]
    fn chunked_response_completion_is_correlated_and_clears_the_lane() {
        let mut commands = commands_with_in_flight();
        let response = serde_json::to_string(&SuccessResponse {
            id: "request-a".into(),
            result: ResponseResult::Ok {},
        })
        .unwrap();
        let split = response.len() / 2;

        assert!(commands
            .receive_chunk(
                "boot-a",
                "request-a",
                false,
                response.as_bytes()[..split].to_vec(),
            )
            .unwrap()
            .is_none());
        let completed = commands
            .receive_chunk(
                "boot-a",
                "request-a",
                true,
                response.as_bytes()[split..].to_vec(),
            )
            .unwrap()
            .unwrap();

        assert_eq!(completed.boot_id, "boot-a");
        assert_eq!(completed.request_id, "request-a");
        assert!(matches!(completed.result, Ok(ResponseResult::Ok {})));
        assert!(commands.in_flight.is_none());
    }

    #[test]
    fn large_selection_response_reassembles_without_truncation() {
        let mut commands = commands_with_in_flight();
        let selection = "selected".repeat(160_000);
        let response = serde_json::to_vec(&SuccessResponse {
            id: "request-a".into(),
            result: ResponseResult::PaneSelection {
                pane_id: "w1:p1".into(),
                text: selection.clone(),
            },
        })
        .unwrap();
        let chunk_count = response.len().div_ceil(128 * 1024);
        let mut completed = None;
        for (index, chunk) in response.chunks(128 * 1024).enumerate() {
            completed = commands
                .receive_chunk(
                    "boot-a",
                    "request-a",
                    index + 1 == chunk_count,
                    chunk.to_vec(),
                )
                .unwrap();
        }

        assert!(matches!(
            completed.expect("final selection response").result,
            Ok(ResponseResult::PaneSelection { text, .. }) if text == selection
        ));
    }

    #[test]
    fn in_flight_endpoint_command_expires_and_releases_the_lane() {
        let mut commands = commands_with_in_flight();
        let expired = commands
            .expire(std::time::Instant::now() + ENDPOINT_COMMAND_TIMEOUT)
            .expect("expired endpoint command");

        assert_eq!(expired.boot_id, "boot-a");
        assert_eq!(expired.request_id, "request-a");
        assert!(matches!(
            expired.result,
            Err(ClientShellEndpointError {
                code: Some(code),
                ..
            }) if code == "endpoint_timeout"
        ));
        assert!(commands.in_flight.is_some());
        assert!(commands
            .expire(std::time::Instant::now() + ENDPOINT_COMMAND_TIMEOUT)
            .is_none());
        let late_response = serde_json::to_vec(&SuccessResponse {
            id: "request-a".into(),
            result: ResponseResult::Ok {},
        })
        .unwrap();
        assert!(commands
            .receive_chunk("boot-a", "request-a", true, late_response)
            .expect("late response releases the synchronized lane")
            .is_some());
        assert!(commands.in_flight.is_none());
    }

    #[test]
    fn response_from_another_boot_is_rejected() {
        let mut commands = commands_with_in_flight();

        let Err(error) = commands.receive_chunk("boot-b", "request-a", true, b"{}".to_vec()) else {
            panic!("mismatched boot should fail");
        };

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
