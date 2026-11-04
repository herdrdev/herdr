use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::{Duration, Instant};

use crate::api::client::ApiClientError;
use crate::api::schema::{Request, ResponseResult};
use crate::protocol::ClientMessage;

use super::endpoint::{ClientEndpointId, EndpointRegistry, EndpointSendOutcome};
use super::shell::ClientShellEndpointError;

const ENDPOINT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RETIRED_REQUESTS_PER_ENDPOINT: usize = 128;

struct QueuedCommand {
    generation: u64,
    boot_id: String,
    request: Box<Request>,
}

struct InFlightCommand {
    generation: u64,
    boot_id: String,
    request_id: String,
    response: Vec<u8>,
    sent_at: Instant,
}

pub(super) struct EndpointCommandResult {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) generation: u64,
    pub(super) boot_id: String,
    pub(super) request_id: String,
    pub(super) result: Result<ResponseResult, ClientShellEndpointError>,
}

#[derive(Default)]
struct EndpointCommandLane {
    queued: VecDeque<QueuedCommand>,
    in_flight: Option<InFlightCommand>,
    retired: VecDeque<(u64, String, String)>,
}

impl EndpointCommandLane {
    fn retire(&mut self, request: (u64, String, String)) {
        if self.retired.contains(&request) {
            return;
        }
        if self.retired.len() == MAX_RETIRED_REQUESTS_PER_ENDPOINT {
            self.retired.pop_front();
        }
        self.retired.push_back(request);
    }

    fn consume_retired(&mut self, request: &(u64, String, String), final_chunk: bool) -> bool {
        let Some(index) = self.retired.iter().position(|retired| retired == request) else {
            return false;
        };
        if final_chunk {
            self.retired.remove(index);
        }
        true
    }
}

#[derive(Default)]
pub(super) struct EndpointCommands {
    lanes: HashMap<ClientEndpointId, EndpointCommandLane>,
}

impl EndpointCommands {
    pub(super) fn enqueue(
        &mut self,
        endpoint_id: ClientEndpointId,
        generation: u64,
        boot_id: String,
        request: Box<Request>,
    ) {
        self.lanes
            .entry(endpoint_id)
            .or_default()
            .queued
            .push_back(QueuedCommand {
                generation,
                boot_id,
                request,
            });
    }

    pub(super) fn send_next(
        &mut self,
        endpoint_id: &ClientEndpointId,
        endpoints: &mut EndpointRegistry,
    ) -> Vec<String> {
        let lane = self.lanes.entry(endpoint_id.clone()).or_default();
        let mut cancelled = Vec::new();
        if lane.in_flight.is_some() {
            return cancelled;
        }
        while let Some(queued) = lane.queued.pop_front() {
            let request_id = queued.request.id.clone();
            if !endpoints.accepts(endpoint_id, queued.generation) {
                cancelled.push(request_id);
                continue;
            }
            let request = match serde_json::to_string(&queued.request) {
                Ok(request) => request,
                Err(error) => {
                    tracing::warn!(%error, %request_id, "could not encode endpoint request");
                    cancelled.push(request_id);
                    continue;
                }
            };
            let message = ClientMessage::ClientShellEndpointRequest {
                boot_id: queued.boot_id.clone(),
                request,
            };
            if endpoints.send_to(endpoint_id, &message) != EndpointSendOutcome::Sent {
                cancelled.push(request_id);
                continue;
            }
            lane.in_flight = Some(InFlightCommand {
                generation: queued.generation,
                boot_id: queued.boot_id,
                request_id,
                response: Vec::new(),
                sent_at: Instant::now(),
            });
            break;
        }
        cancelled
    }

    pub(super) fn accepts_response(
        &self,
        endpoint_id: &ClientEndpointId,
        response_generation: u64,
        response_boot_id: &str,
        response_request_id: &str,
    ) -> bool {
        self.lanes
            .get(endpoint_id)
            .and_then(|lane| lane.in_flight.as_ref())
            .is_some_and(|command| {
                command.generation == response_generation
                    && command.boot_id == response_boot_id
                    && command.request_id == response_request_id
            })
    }

    /// Retire the complete source lane at source-off. The in-flight request is tombstoned for a
    /// late endpoint-local response; every queued request is cancelled before it can run in a
    /// later presentation epoch. Other endpoint lanes are deliberately untouched.
    pub(super) fn retire_lane(&mut self, endpoint_id: &ClientEndpointId) -> Vec<String> {
        let Some(lane) = self.lanes.get_mut(endpoint_id) else {
            return Vec::new();
        };
        let mut request_ids = Vec::new();
        if let Some(command) = lane.in_flight.take() {
            lane.retire((
                command.generation,
                command.boot_id,
                command.request_id.clone(),
            ));
            request_ids.push(command.request_id);
        }
        request_ids.extend(lane.queued.drain(..).map(|command| command.request.id));
        request_ids
    }

    pub(super) fn expire(&mut self, now: Instant) -> Vec<EndpointCommandResult> {
        self.lanes
            .iter_mut()
            .filter_map(|(endpoint_id, lane)| {
                let command = lane.in_flight.as_ref()?;
                if now.saturating_duration_since(command.sent_at) < ENDPOINT_COMMAND_TIMEOUT {
                    return None;
                }
                let command = lane.in_flight.take().expect("checked in-flight command");
                lane.retire((
                    command.generation,
                    command.boot_id.clone(),
                    command.request_id.clone(),
                ));
                Some(EndpointCommandResult {
                    endpoint_id: endpoint_id.clone(),
                    generation: command.generation,
                    boot_id: command.boot_id,
                    request_id: command.request_id,
                    result: Err(ClientShellEndpointError {
                        code: Some("endpoint_timeout".into()),
                        message: "este servidor não respondeu à ação".into(),
                    }),
                })
            })
            .collect()
    }

    pub(super) fn receive_chunk(
        &mut self,
        endpoint_id: &ClientEndpointId,
        response_generation: u64,
        response_boot_id: &str,
        response_request_id: &str,
        final_chunk: bool,
        data: Vec<u8>,
    ) -> io::Result<Option<EndpointCommandResult>> {
        let Some(lane) = self.lanes.get_mut(endpoint_id) else {
            return Ok(None);
        };
        let retired = (
            response_generation,
            response_boot_id.to_owned(),
            response_request_id.to_owned(),
        );
        if lane.consume_retired(&retired, final_chunk) {
            return Ok(None);
        }
        let Some(in_flight) = lane.in_flight.as_mut() else {
            return Ok(None);
        };
        if response_generation != in_flight.generation
            || response_boot_id != in_flight.boot_id
            || response_request_id != in_flight.request_id
        {
            return Ok(None);
        }
        in_flight.response.extend(data);
        if !final_chunk {
            return Ok(None);
        }

        let in_flight = lane.in_flight.take().expect("checked in-flight command");
        let result = parse_response(&in_flight.request_id, &in_flight.response);
        Ok(Some(EndpointCommandResult {
            endpoint_id: endpoint_id.clone(),
            generation: in_flight.generation,
            boot_id: in_flight.boot_id,
            request_id: in_flight.request_id,
            result,
        }))
    }

    /// Disconnecting an endpoint also cancels its shell-pending requests. Connection generation
    /// rejection handles any late wire response after the lane itself is removed.
    pub(super) fn disconnect(&mut self, endpoint_id: &ClientEndpointId) -> Vec<String> {
        let Some(lane) = self.lanes.remove(endpoint_id) else {
            return Vec::new();
        };
        let mut request_ids = lane
            .queued
            .into_iter()
            .map(|command| command.request.id)
            .collect::<Vec<_>>();
        if let Some(command) = lane.in_flight {
            request_ids.push(command.request_id);
        }
        request_ids
    }
}

pub(super) fn parse_response(
    expected_id: &str,
    response: &[u8],
) -> Result<ResponseResult, ClientShellEndpointError> {
    let value = serde_json::from_slice(response).map_err(|error| ClientShellEndpointError {
        code: None,
        message: format!("resposta inválida da conexão: {error}"),
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

    fn endpoint() -> ClientEndpointId {
        ClientEndpointId::Local
    }

    fn commands_with_in_flight() -> EndpointCommands {
        EndpointCommands {
            lanes: HashMap::from([(
                endpoint(),
                EndpointCommandLane {
                    in_flight: Some(InFlightCommand {
                        generation: 1,
                        boot_id: "boot-a".into(),
                        request_id: "request-a".into(),
                        response: Vec::new(),
                        sent_at: Instant::now(),
                    }),
                    ..EndpointCommandLane::default()
                },
            )]),
        }
    }

    fn has_in_flight(commands: &EndpointCommands) -> bool {
        commands
            .lanes
            .get(&endpoint())
            .is_some_and(|lane| lane.in_flight.is_some())
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
                &endpoint(),
                1,
                "boot-a",
                "request-a",
                false,
                response.as_bytes()[..split].to_vec(),
            )
            .unwrap()
            .is_none());
        let completed = commands
            .receive_chunk(
                &endpoint(),
                1,
                "boot-a",
                "request-a",
                true,
                response.as_bytes()[split..].to_vec(),
            )
            .unwrap()
            .unwrap();

        assert_eq!(completed.endpoint_id, endpoint());
        assert_eq!(completed.generation, 1);
        assert_eq!(completed.boot_id, "boot-a");
        assert_eq!(completed.request_id, "request-a");
        assert!(matches!(completed.result, Ok(ResponseResult::Ok {})));
        assert!(!has_in_flight(&commands));
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
                    &endpoint(),
                    1,
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
            .pop()
            .expect("expired endpoint command");

        assert_eq!(expired.endpoint_id, endpoint());
        assert_eq!(expired.boot_id, "boot-a");
        assert_eq!(expired.request_id, "request-a");
        assert!(matches!(
            expired.result,
            Err(ClientShellEndpointError {
                code: Some(code),
                ..
            }) if code == "endpoint_timeout"
        ));
        assert!(!has_in_flight(&commands));
        assert!(commands
            .expire(std::time::Instant::now() + ENDPOINT_COMMAND_TIMEOUT)
            .is_empty());
        let late_response = serde_json::to_vec(&SuccessResponse {
            id: "request-a".into(),
            result: ResponseResult::Ok {},
        })
        .unwrap();
        assert!(commands
            .receive_chunk(&endpoint(), 1, "boot-a", "request-a", true, late_response)
            .expect("late retired response is ignored")
            .is_none());
        assert!(!has_in_flight(&commands));
    }

    #[test]
    fn endpoint_lanes_complete_independently() {
        let remote = ClientEndpointId::Ssh(
            crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        );
        let mut commands = commands_with_in_flight();
        commands.lanes.insert(
            remote.clone(),
            EndpointCommandLane {
                in_flight: Some(InFlightCommand {
                    generation: 2,
                    boot_id: "boot-b".into(),
                    request_id: "request-b".into(),
                    response: Vec::new(),
                    sent_at: Instant::now(),
                }),
                ..EndpointCommandLane::default()
            },
        );
        let response = serde_json::to_vec(&SuccessResponse {
            id: "request-b".into(),
            result: ResponseResult::Ok {},
        })
        .unwrap();

        let completed = commands
            .receive_chunk(&remote, 2, "boot-b", "request-b", true, response)
            .unwrap()
            .expect("remote response");

        assert_eq!(completed.endpoint_id, remote);
        assert!(has_in_flight(&commands));
        assert!(commands
            .lanes
            .get(&completed.endpoint_id)
            .is_some_and(|lane| lane.in_flight.is_none()));
    }

    #[test]
    fn retiring_complete_source_lane_cancels_queued_ids_and_keeps_other_lanes() {
        let remote = ClientEndpointId::Ssh(
            crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        );
        let mut commands = commands_with_in_flight();
        commands
            .lanes
            .get_mut(&endpoint())
            .unwrap()
            .queued
            .push_back(QueuedCommand {
                generation: 1,
                boot_id: "boot-a".into(),
                request: Box::new(Request {
                    id: "queued-source".into(),
                    method: crate::api::schema::Method::WorkspaceList(
                        crate::api::schema::EmptyParams::default(),
                    ),
                }),
            });
        commands.lanes.insert(
            remote.clone(),
            EndpointCommandLane {
                queued: VecDeque::from([QueuedCommand {
                    generation: 2,
                    boot_id: "boot-b".into(),
                    request: Box::new(Request {
                        id: "request-b".into(),
                        method: crate::api::schema::Method::WorkspaceList(
                            crate::api::schema::EmptyParams::default(),
                        ),
                    }),
                }]),
                ..EndpointCommandLane::default()
            },
        );

        assert_eq!(
            commands.retire_lane(&endpoint()),
            vec!["request-a", "queued-source"]
        );
        assert!(!has_in_flight(&commands));
        assert!(commands
            .lanes
            .get(&endpoint())
            .is_some_and(|lane| lane.queued.is_empty()));
        assert!(commands
            .lanes
            .get(&remote)
            .is_some_and(|lane| !lane.queued.is_empty()));

        let late_response = serde_json::to_vec(&SuccessResponse {
            id: "request-a".into(),
            result: ResponseResult::Ok {},
        })
        .unwrap();
        assert!(commands
            .receive_chunk(&endpoint(), 1, "boot-a", "request-a", true, late_response)
            .unwrap()
            .is_none());
    }

    #[test]
    fn disconnect_returns_every_request_that_must_be_discarded() {
        let mut commands = commands_with_in_flight();
        commands
            .lanes
            .get_mut(&endpoint())
            .unwrap()
            .queued
            .push_back(QueuedCommand {
                generation: 1,
                boot_id: "boot-a".into(),
                request: Box::new(Request {
                    id: "queued-a".into(),
                    method: crate::api::schema::Method::WorkspaceList(
                        crate::api::schema::EmptyParams::default(),
                    ),
                }),
            });
        assert_eq!(
            commands.disconnect(&endpoint()),
            vec!["queued-a", "request-a"]
        );
        assert!(!commands.lanes.contains_key(&endpoint()));
    }

    #[test]
    fn retired_request_tombstones_are_bounded() {
        let mut lane = EndpointCommandLane::default();
        for serial in 0..MAX_RETIRED_REQUESTS_PER_ENDPOINT + 10 {
            lane.retire((1, "boot".into(), format!("request-{serial}")));
        }
        assert_eq!(lane.retired.len(), MAX_RETIRED_REQUESTS_PER_ENDPOINT);
        assert!(!lane
            .retired
            .contains(&(1, "boot".into(), "request-0".into())));
        assert!(lane.retired.contains(&(
            1,
            "boot".into(),
            format!("request-{}", MAX_RETIRED_REQUESTS_PER_ENDPOINT + 9)
        )));
    }

    #[test]
    fn stale_or_unknown_responses_do_not_damage_the_live_lane() {
        let mut commands = commands_with_in_flight();
        let unknown = ClientEndpointId::Ssh(
            crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        );

        assert!(commands
            .receive_chunk(&endpoint(), 2, "boot-a", "request-a", true, b"{}".to_vec())
            .unwrap()
            .is_none());
        assert!(commands
            .receive_chunk(&endpoint(), 1, "boot-b", "request-a", true, b"{}".to_vec())
            .unwrap()
            .is_none());
        assert!(commands
            .receive_chunk(&unknown, 1, "boot-a", "request-a", true, b"{}".to_vec())
            .unwrap()
            .is_none());
        assert!(has_in_flight(&commands));
    }
}
