use regex::Regex;

use crate::api::event_hub::EventHistoryError;
use crate::api::schema::{
    EmptyParams, ErrorBody, ErrorResponse, EventKind, Method, PaneAgentStatusChangedEvent,
    PaneOutputMatchedEvent, PaneScrollChangedEvent, PaneScrollInfo, Request, Subscription,
    SubscriptionEventData, SubscriptionEventEnvelope, SubscriptionEventKind, SuccessResponse,
};
use crate::api::server::{dispatch_to_app_with_timeout, APP_RESPONSE_TIMEOUT};
use crate::api::{ApiRequestSender, EventHub};

pub(super) fn output_match_read_source(
    source: &crate::api::schema::ReadSource,
) -> crate::api::schema::ReadSource {
    match source {
        crate::api::schema::ReadSource::Recent => crate::api::schema::ReadSource::RecentUnwrapped,
        other => *other,
    }
}

pub(super) fn match_output(
    text: &str,
    matcher: &crate::api::schema::OutputMatch,
    regex: Option<&Regex>,
) -> Option<String> {
    match matcher {
        crate::api::schema::OutputMatch::Substring { value } => text
            .lines()
            .find(|line| line.contains(value))
            .map(|line| line.to_string()),
        crate::api::schema::OutputMatch::Regex { .. } => regex.and_then(|re| {
            text.lines()
                .find(|line| re.is_match(line))
                .map(|line| line.to_string())
        }),
    }
}

pub(super) struct ActiveOutputMatchedSubscription {
    pane_id: String,
    source: crate::api::schema::ReadSource,
    lines: Option<u32>,
    matcher: crate::api::schema::OutputMatch,
    regex: Option<Regex>,
    strip_ansi: bool,
    currently_matching: bool,
    request_prefix: String,
}

pub(super) struct ActiveAgentStatusChangedSubscription {
    pane_id: String,
    status_filter: Option<crate::api::schema::AgentStatus>,
    last_status: Option<crate::api::schema::AgentStatus>,
    last_presentation: Option<PanePresentationSnapshot>,
    last_sequence: u64,
    initial_event: Option<PaneAgentStatusChangedEvent>,
    request_prefix: String,
}

pub(super) struct ActiveScrollChangedSubscription {
    pane_id: String,
    last_scroll: Option<PaneScrollInfo>,
    request_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PanePresentationSnapshot {
    title: Option<String>,
    display_agent: Option<String>,
    state_labels: std::collections::HashMap<String, String>,
    wait: Option<String>,
    eta_s: Option<u64>,
    reported_at: Option<String>,
    last_turn_at: Option<String>,
    waiting_on_agents: bool,
}

impl PanePresentationSnapshot {
    fn from(pane: &crate::api::schema::PaneInfo) -> Self {
        Self {
            title: pane.title.clone(),
            display_agent: pane.display_agent.clone(),
            state_labels: pane.state_labels.clone(),
            wait: pane.wait.clone(),
            eta_s: pane.eta_s,
            reported_at: pane.reported_at.clone(),
            last_turn_at: pane.last_turn_at.clone(),
            waiting_on_agents: pane.waiting_on_agents,
        }
    }

    fn from_event(
        title: &Option<String>,
        display_agent: &Option<String>,
        state_labels: &std::collections::HashMap<String, String>,
        wait: &Option<String>,
        eta_s: Option<u64>,
        reported_at: &Option<String>,
        last_turn_at: &Option<String>,
        waiting_on_agents: bool,
    ) -> Self {
        Self {
            title: title.clone(),
            display_agent: display_agent.clone(),
            state_labels: state_labels.clone(),
            wait: wait.clone(),
            eta_s,
            reported_at: reported_at.clone(),
            last_turn_at: last_turn_at.clone(),
            waiting_on_agents,
        }
    }
}

pub(super) struct ActiveEventSubscription {
    event_kind: crate::api::schema::EventKind,
    last_sequence: u64,
}

pub(super) struct ActiveSessionChangedSubscription {
    last_sequence: u64,
}

pub(super) struct ActiveFleetAgentsSubscription {
    last_sequence: u64,
    initial: bool,
    request_prefix: String,
}

pub(super) enum ActiveSubscription {
    Event(ActiveEventSubscription),
    OutputMatched(ActiveOutputMatchedSubscription),
    AgentStatusChanged(Box<ActiveAgentStatusChangedSubscription>),
    ScrollChanged(ActiveScrollChangedSubscription),
    FleetAgentsChanged(ActiveFleetAgentsSubscription),
    SessionChanged(ActiveSessionChangedSubscription),
}

impl ActiveSubscription {
    pub(super) fn new(
        subscription: Subscription,
        request_id: &str,
        index: usize,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
        event_start_sequence: u64,
    ) -> Result<Self, ErrorResponse> {
        let event_subscription = |event_kind| {
            Self::Event(ActiveEventSubscription {
                event_kind,
                last_sequence: event_start_sequence,
            })
        };

        match subscription {
            Subscription::WorkspaceCreated {} => {
                Ok(event_subscription(EventKind::WorkspaceCreated))
            }
            Subscription::WorkspaceUpdated {} => {
                Ok(event_subscription(EventKind::WorkspaceUpdated))
            }
            Subscription::WorkspaceMetadataUpdated {} => {
                Ok(event_subscription(EventKind::WorkspaceMetadataUpdated))
            }
            Subscription::WorkspaceRenamed {} => {
                Ok(event_subscription(EventKind::WorkspaceRenamed))
            }
            Subscription::WorkspaceMoved {} => Ok(event_subscription(EventKind::WorkspaceMoved)),
            Subscription::WorkspaceReordered {} => {
                Ok(event_subscription(EventKind::WorkspaceReordered))
            }
            Subscription::WorkspaceClosed {} => Ok(event_subscription(EventKind::WorkspaceClosed)),
            Subscription::WorkspaceFocused {} => {
                Ok(event_subscription(EventKind::WorkspaceFocused))
            }
            Subscription::LoopRunHistoryUpdated {} => {
                Ok(event_subscription(EventKind::LoopRunHistoryUpdated))
            }
            Subscription::WorktreeCreated {} => Ok(event_subscription(EventKind::WorktreeCreated)),
            Subscription::WorktreeOpened {} => Ok(event_subscription(EventKind::WorktreeOpened)),
            Subscription::WorktreeRemoved {} => Ok(event_subscription(EventKind::WorktreeRemoved)),
            Subscription::TabCreated {} => Ok(event_subscription(EventKind::TabCreated)),
            Subscription::TabClosed {} => Ok(event_subscription(EventKind::TabClosed)),
            Subscription::TabFocused {} => Ok(event_subscription(EventKind::TabFocused)),
            Subscription::TabRenamed {} => Ok(event_subscription(EventKind::TabRenamed)),
            Subscription::TabMoved {} => Ok(event_subscription(EventKind::TabMoved)),
            Subscription::PaneCreated {} => Ok(event_subscription(EventKind::PaneCreated)),
            Subscription::PaneClosed {} => Ok(event_subscription(EventKind::PaneClosed)),
            Subscription::PaneUpdated {} => Ok(event_subscription(EventKind::PaneUpdated)),
            Subscription::PaneSettled {} => Ok(event_subscription(EventKind::PaneSettled)),
            Subscription::PaneUnsettled {} => Ok(event_subscription(EventKind::PaneUnsettled)),
            Subscription::PaneSnoozed {} => Ok(event_subscription(EventKind::PaneSnoozed)),
            Subscription::PaneUnsnoozed {} => Ok(event_subscription(EventKind::PaneUnsnoozed)),
            Subscription::PaneFocused {} => Ok(event_subscription(EventKind::PaneFocused)),
            Subscription::PaneMoved {} => Ok(event_subscription(EventKind::PaneMoved)),
            Subscription::PaneExited {} => Ok(event_subscription(EventKind::PaneExited)),
            Subscription::PaneAgentDetected {} => {
                Ok(event_subscription(EventKind::PaneAgentDetected))
            }
            Subscription::AuthorityCatalogsUpdated {} => {
                Ok(event_subscription(EventKind::AuthorityCatalogsUpdated))
            }
            Subscription::LayoutUpdated {} => Ok(event_subscription(EventKind::LayoutUpdated)),
            Subscription::FleetAgentsChanged {} => {
                Ok(Self::FleetAgentsChanged(ActiveFleetAgentsSubscription {
                    last_sequence: event_start_sequence,
                    initial: true,
                    request_prefix: format!("{request_id}:sub:{index}"),
                }))
            }
            Subscription::SessionChanged {} => {
                Ok(Self::SessionChanged(ActiveSessionChangedSubscription {
                    last_sequence: event_start_sequence,
                }))
            }
            Subscription::PaneOutputMatched {
                pane_id,
                source,
                lines,
                r#match,
                strip_ansi,
            } => {
                let regex = match &r#match {
                    crate::api::schema::OutputMatch::Regex { value } => match Regex::new(value) {
                        Ok(regex) => Some(regex),
                        Err(err) => {
                            return Err(ErrorResponse {
                                id: request_id.to_string(),
                                error: ErrorBody {
                                    code: "invalid_regex".into(),
                                    message: err.to_string(),
                                },
                            });
                        }
                    },
                    crate::api::schema::OutputMatch::Substring { .. } => None,
                };

                let probe = pane_read(
                    format!("{request_id}:sub:{index}:probe"),
                    &pane_id,
                    source,
                    lines,
                    strip_ansi,
                    api_tx,
                );
                probe?;

                Ok(Self::OutputMatched(ActiveOutputMatchedSubscription {
                    pane_id,
                    source,
                    lines,
                    matcher: r#match,
                    regex,
                    strip_ansi,
                    currently_matching: false,
                    request_prefix: format!("{request_id}:sub:{index}"),
                }))
            }
            Subscription::PaneAgentStatusChanged {
                pane_id,
                agent_status,
            } => {
                let last_sequence = event_hub.current_sequence();
                let probe = pane_get(format!("{request_id}:sub:{index}:probe"), &pane_id, api_tx)?;
                let last_status = probe.agent_status;
                let last_presentation = PanePresentationSnapshot::from(&probe);
                let initial_event = agent_status
                    .is_some_and(|wanted| wanted == probe.agent_status)
                    .then_some(PaneAgentStatusChangedEvent {
                        pane_id: probe.pane_id.clone(),
                        workspace_id: probe.workspace_id,
                        agent_status: probe.agent_status,
                        waiting_on_agents: probe.waiting_on_agents,
                        wait: probe.wait,
                        eta_s: probe.eta_s,
                        reported_at: probe.reported_at,
                        last_turn_at: probe.last_turn_at,
                        agent: probe.agent,
                        title: probe.title,
                        display_agent: probe.display_agent,
                        state_labels: probe.state_labels,
                    });

                Ok(Self::AgentStatusChanged(Box::new(
                    ActiveAgentStatusChangedSubscription {
                        pane_id: probe.pane_id,
                        status_filter: agent_status,
                        last_status: Some(last_status),
                        last_presentation: Some(last_presentation),
                        last_sequence,
                        initial_event,
                        request_prefix: format!("{request_id}:sub:{index}"),
                    },
                )))
            }
            Subscription::PaneScrollChanged { pane_id } => {
                let probe = pane_get(format!("{request_id}:sub:{index}:probe"), &pane_id, api_tx)?;

                Ok(Self::ScrollChanged(ActiveScrollChangedSubscription {
                    pane_id: probe.pane_id,
                    last_scroll: probe.scroll,
                    request_prefix: format!("{request_id}:sub:{index}"),
                }))
            }
        }
    }

    pub(super) fn poll(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Option<serde_json::Value> {
        match self {
            Self::Event(subscription) => subscription.poll(event_hub),
            Self::OutputMatched(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
            }
            Self::AgentStatusChanged(subscription) => {
                serde_json::to_value(subscription.poll(api_tx, event_hub)?).ok()
            }
            Self::ScrollChanged(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
            }
            Self::FleetAgentsChanged(subscription) => subscription
                .poll_batch(api_tx, event_hub)
                .ok()?
                .into_iter()
                .next(),
            Self::SessionChanged(subscription) => {
                // Each event carries a full snapshot, so the newest supersedes older events
                // drained in the same poll.
                subscription.poll(event_hub).ok()?.into_iter().last()
            }
        }
    }

    pub(super) fn poll_for_wait(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Option<serde_json::Value>, ErrorResponse> {
        match self {
            Self::AgentStatusChanged(subscription) => Ok(subscription
                .poll_result(api_tx, event_hub)?
                .and_then(|event| serde_json::to_value(event).ok())),
            _ => Ok(self.poll(api_tx, event_hub)),
        }
    }

    pub(super) fn poll_batch(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Vec<serde_json::Value>, ErrorBody> {
        match self {
            Self::Event(subscription) => {
                let events = subscription_events_after(event_hub, subscription.last_sequence)?;
                let mut matching = Vec::new();
                for (sequence, event) in events {
                    subscription.last_sequence = sequence;
                    if event.event == subscription.event_kind {
                        matching.push(serde_json::to_value(event).map_err(event_encoding_error)?);
                    }
                }
                Ok(matching)
            }
            Self::AgentStatusChanged(subscription) => {
                let events = subscription_events_after(event_hub, subscription.last_sequence)?;
                let mut matching = Vec::new();
                for (sequence, event) in events {
                    subscription.last_sequence = sequence;
                    if let Some(event) = subscription.event_from_history(event) {
                        matching.push(serde_json::to_value(event).map_err(event_encoding_error)?);
                    }
                }
                if matching.is_empty() {
                    if let Some(event) = subscription
                        .poll_snapshot(api_tx, event_hub)
                        .map_err(|response| response.error)?
                    {
                        matching.push(serde_json::to_value(event).map_err(event_encoding_error)?);
                    }
                }
                Ok(matching)
            }
            Self::OutputMatched(_) | Self::ScrollChanged(_) => {
                Ok(self.poll(api_tx, event_hub).into_iter().collect())
            }
            Self::FleetAgentsChanged(subscription) => subscription.poll_batch(api_tx, event_hub),
            Self::SessionChanged(subscription) => subscription.poll_batch(event_hub),
        }
    }
}

impl ActiveFleetAgentsSubscription {
    fn poll_batch(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Vec<serde_json::Value>, ErrorBody> {
        let events = subscription_events_after(event_hub, self.last_sequence)?;
        let changed = self.initial
            || events
                .iter()
                .any(|(_, event)| changes_fleet_agent_inventory(event.event));
        if let Some((sequence, _)) = events.last() {
            self.last_sequence = *sequence;
        }
        self.initial = false;
        if !changed {
            return Ok(Vec::new());
        }

        let response = dispatch_to_app_with_timeout(
            Request {
                id: format!("{}:{}", self.request_prefix, self.last_sequence),
                method: Method::AgentList(EmptyParams::default()),
            },
            api_tx,
            Some(APP_RESPONSE_TIMEOUT),
        );
        if let Ok(error) = serde_json::from_str::<ErrorResponse>(&response) {
            return Err(error.error);
        }
        let success =
            serde_json::from_str::<SuccessResponse>(&response).map_err(|error| ErrorBody {
                code: "invalid_response".into(),
                message: format!("invalid fleet agent response: {error}"),
            })?;
        let crate::api::schema::ResponseResult::AgentList { agents } = success.result else {
            return Err(ErrorBody {
                code: "invalid_response".into(),
                message: "fleet agent request returned an unexpected result".into(),
            });
        };
        let event = crate::api::schema::SubscriptionEventEnvelope {
            event: SubscriptionEventKind::FleetAgentsChanged,
            data: SubscriptionEventData::FleetAgentsChanged(
                crate::api::schema::FleetAgentsChangedEvent { agents },
            ),
        };
        serde_json::to_value(event)
            .map(|event| vec![event])
            .map_err(event_encoding_error)
    }
}

impl ActiveSessionChangedSubscription {
    fn poll(&mut self, event_hub: &EventHub) -> Result<Vec<serde_json::Value>, ErrorBody> {
        self.poll_batch(event_hub)
    }

    fn poll_batch(&mut self, event_hub: &EventHub) -> Result<Vec<serde_json::Value>, ErrorBody> {
        let events = subscription_events_after(event_hub, self.last_sequence)?;
        let mut matching = Vec::new();
        for (sequence, event) in events {
            self.last_sequence = sequence;
            if let crate::api::schema::EventData::SessionChanged {
                epoch,
                revision,
                snapshot,
            } = event.data
            {
                matching.push(
                    serde_json::to_value(SubscriptionEventEnvelope {
                        event: SubscriptionEventKind::SessionChanged,
                        data: SubscriptionEventData::SessionChanged(
                            crate::api::schema::SessionChangedEvent {
                                epoch,
                                revision,
                                snapshot,
                            },
                        ),
                    })
                    .map_err(event_encoding_error)?,
                );
            }
        }
        Ok(matching)
    }
}

fn changes_fleet_agent_inventory(event: EventKind) -> bool {
    matches!(
        event,
        EventKind::WorkspaceCreated
            | EventKind::WorkspaceUpdated
            | EventKind::WorkspaceMetadataUpdated
            | EventKind::WorkspaceClosed
            | EventKind::WorkspaceRenamed
            | EventKind::WorkspaceMoved
            | EventKind::WorkspaceFocused
            | EventKind::TabCreated
            | EventKind::TabClosed
            | EventKind::TabRenamed
            | EventKind::TabMoved
            | EventKind::TabFocused
            | EventKind::PaneCreated
            | EventKind::PaneClosed
            | EventKind::PaneUpdated
            | EventKind::PaneSettled
            | EventKind::PaneUnsettled
            | EventKind::PaneSnoozed
            | EventKind::PaneUnsnoozed
            | EventKind::PaneFocused
            | EventKind::PaneMoved
            | EventKind::PaneExited
            | EventKind::PaneAgentDetected
            | EventKind::PaneAgentStatusChanged
            | EventKind::LayoutUpdated
            | EventKind::SessionChanged
    )
}

fn subscription_events_after(
    event_hub: &EventHub,
    sequence: u64,
) -> Result<Vec<(u64, crate::api::schema::EventEnvelope)>, ErrorBody> {
    event_hub.events_after_checked(sequence).map_err(|error| match error {
        EventHistoryError::Lost => ErrorBody {
            code: "events_lost".into(),
            message: "event subscription fell behind retained history; resubscribe and resync with session.snapshot".into(),
        },
        EventHistoryError::Unavailable => ErrorBody {
            code: "server_unavailable".into(),
            message: "event history is unavailable".into(),
        },
    })
}

fn event_encoding_error(error: serde_json::Error) -> ErrorBody {
    ErrorBody {
        code: "internal_error".into(),
        message: format!("failed to encode subscription event: {error}"),
    }
}

impl ActiveEventSubscription {
    fn poll(&mut self, event_hub: &EventHub) -> Option<serde_json::Value> {
        for (sequence, event) in event_hub.events_after(self.last_sequence) {
            self.last_sequence = sequence;
            if event.event == self.event_kind {
                return serde_json::to_value(event).ok();
            }
        }
        None
    }
}

impl ActiveOutputMatchedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let read = pane_read(
            format!("{}:read", self.request_prefix),
            &self.pane_id,
            output_match_read_source(&self.source),
            self.lines,
            self.strip_ansi,
            api_tx,
        )
        .ok()?;

        let matched_line = match_output(&read.text, &self.matcher, self.regex.as_ref());
        match matched_line {
            Some(matched_line) => {
                if self.currently_matching {
                    return None;
                }
                self.currently_matching = true;
                Some(SubscriptionEventEnvelope {
                    event: SubscriptionEventKind::PaneOutputMatched,
                    data: SubscriptionEventData::PaneOutputMatched(PaneOutputMatchedEvent {
                        pane_id: read.pane_id.clone(),
                        matched_line,
                        read,
                    }),
                })
            }
            None => {
                self.currently_matching = false;
                None
            }
        }
    }
}

impl ActiveAgentStatusChangedSubscription {
    fn poll(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Option<SubscriptionEventEnvelope> {
        self.poll_result(api_tx, event_hub).ok().flatten()
    }

    fn poll_result(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Option<SubscriptionEventEnvelope>, ErrorResponse> {
        let events = subscription_events_after(event_hub, self.last_sequence).map_err(|error| {
            ErrorResponse {
                id: self.request_prefix.clone(),
                error,
            }
        })?;
        for (sequence, event) in events {
            self.last_sequence = sequence;
            if let Some(event) = self.event_from_history(event) {
                return Ok(Some(event));
            }
        }

        self.poll_snapshot(api_tx, event_hub)
    }

    fn event_from_history(
        &mut self,
        event: crate::api::schema::EventEnvelope,
    ) -> Option<SubscriptionEventEnvelope> {
        if event.event != EventKind::PaneAgentStatusChanged {
            return None;
        }
        let crate::api::schema::EventData::PaneAgentStatusChanged {
            pane_id,
            workspace_id,
            agent_status,
            waiting_on_agents,
            wait,
            eta_s,
            reported_at,
            last_turn_at,
            agent,
            title,
            display_agent,
            state_labels,
        } = event.data
        else {
            return None;
        };
        if pane_id != self.pane_id {
            return None;
        }

        self.last_status = Some(agent_status);
        self.last_presentation = Some(PanePresentationSnapshot::from_event(
            &title,
            &display_agent,
            &state_labels,
            &wait,
            eta_s,
            &reported_at,
            &last_turn_at,
            waiting_on_agents,
        ));
        self.initial_event = None;
        if self
            .status_filter
            .is_some_and(|wanted| wanted != agent_status)
        {
            return None;
        }

        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::PaneAgentStatusChanged,
            data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                pane_id,
                workspace_id,
                agent_status,
                waiting_on_agents,
                wait,
                eta_s,
                reported_at,
                last_turn_at,
                agent,
                title,
                display_agent,
                state_labels,
            }),
        })
    }

    fn poll_snapshot(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Option<SubscriptionEventEnvelope>, ErrorResponse> {
        if event_hub.current_sequence() != self.last_sequence {
            return Ok(None);
        } else if let Some(event) = self.initial_event.take() {
            return Ok(Some(SubscriptionEventEnvelope {
                event: SubscriptionEventKind::PaneAgentStatusChanged,
                data: SubscriptionEventData::PaneAgentStatusChanged(event),
            }));
        }

        let before_snapshot_sequence = self.last_sequence;
        let pane = pane_get(
            format!("{}:pane", self.request_prefix),
            &self.pane_id,
            api_tx,
        );
        let after_snapshot_sequence = event_hub.current_sequence();
        if after_snapshot_sequence != before_snapshot_sequence {
            return Ok(None);
        }
        let pane = pane?;

        let event = self.event_from_snapshot(pane);
        if event.is_some() {
            self.last_sequence = after_snapshot_sequence;
        }
        Ok(event)
    }

    fn event_from_snapshot(
        &mut self,
        pane: crate::api::schema::PaneInfo,
    ) -> Option<SubscriptionEventEnvelope> {
        let current_status = pane.agent_status;
        let current_presentation = PanePresentationSnapshot::from(&pane);
        let previous_status = self.last_status.replace(current_status);
        let previous_presentation = self.last_presentation.replace(current_presentation.clone());
        let presentation_changed = previous_presentation
            .as_ref()
            .is_some_and(|previous| previous != &current_presentation);
        let status_changed = previous_status.is_some_and(|previous| previous != current_status);
        if !(status_changed || presentation_changed) {
            return None;
        }
        if self
            .status_filter
            .is_some_and(|wanted| wanted != current_status)
        {
            return None;
        }

        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::PaneAgentStatusChanged,
            data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                agent_status: current_status,
                waiting_on_agents: pane.waiting_on_agents,
                wait: pane.wait,
                eta_s: pane.eta_s,
                reported_at: pane.reported_at,
                last_turn_at: pane.last_turn_at,
                agent: pane.agent,
                title: pane.title,
                display_agent: pane.display_agent,
                state_labels: pane.state_labels,
            }),
        })
    }
}

impl ActiveScrollChangedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let pane = pane_get(
            format!("{}:pane", self.request_prefix),
            &self.pane_id,
            api_tx,
        )
        .ok()?;
        self.event_from_snapshot(pane)
    }

    fn event_from_snapshot(
        &mut self,
        pane: crate::api::schema::PaneInfo,
    ) -> Option<SubscriptionEventEnvelope> {
        let scroll = pane.scroll;
        if self.last_scroll == scroll {
            return None;
        }
        self.last_scroll = scroll;
        let scroll = scroll?;

        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::ScrollChanged,
            data: SubscriptionEventData::ScrollChanged(PaneScrollChangedEvent {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                scroll,
            }),
        })
    }
}

fn pane_read(
    request_id: String,
    pane_id: &str,
    source: crate::api::schema::ReadSource,
    lines: Option<u32>,
    strip_ansi: bool,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::PaneReadResult, ErrorResponse> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: pane_id.to_string(),
                source,
                lines,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi,
                intent: crate::api::schema::ReadIntent::Passive,
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|_| ErrorResponse {
        id: request_id.clone(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane read response".into(),
        },
    })?;
    if value.get("error").is_some() {
        return serde_json::from_value(value).map_err(|_| ErrorResponse {
            id: request_id,
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode pane read error".into(),
            },
        });
    }
    serde_json::from_value(value["result"]["read"].clone()).map_err(|_| ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane read result".into(),
        },
    })
}

fn pane_get(
    request_id: String,
    pane_id: &str,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::PaneInfo, ErrorResponse> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneGet(crate::api::schema::PaneTarget {
                pane_id: pane_id.to_string(),
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|_| ErrorResponse {
        id: request_id.clone(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane get response".into(),
        },
    })?;
    if value.get("error").is_some() {
        let response =
            serde_json::from_value::<ErrorResponse>(value).map_err(|_| ErrorResponse {
                id: request_id,
                error: ErrorBody {
                    code: "internal_error".into(),
                    message: "failed to decode pane get error".into(),
                },
            })?;
        return Err(response);
    }
    serde_json::from_value(value["result"]["pane"].clone()).map_err(|_| ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane get result".into(),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn fleet_inventory_refreshes_for_lifecycle_and_focus_changes() {
        for event in [
            EventKind::WorkspaceFocused,
            EventKind::TabFocused,
            EventKind::PaneFocused,
            EventKind::PaneCreated,
            EventKind::PaneClosed,
            EventKind::PaneAgentStatusChanged,
        ] {
            assert!(changes_fleet_agent_inventory(event), "{event:?}");
        }
        assert!(!changes_fleet_agent_inventory(EventKind::PaneOutputChanged));
    }
    use crate::api::schema::{AgentStatus, EventData, EventEnvelope, EventKind, PaneInfo};

    fn presentation_event(title: Option<&str>) -> EventEnvelope {
        EventEnvelope {
            event: EventKind::PaneAgentStatusChanged,
            data: EventData::PaneAgentStatusChanged {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                waiting_on_agents: false,
                wait: None,
                eta_s: None,
                reported_at: None,
                agent: Some("pi".into()),
                title: title.map(str::to_string),
                display_agent: None,
                state_labels: HashMap::new(),
                last_turn_at: None,
            },
        }
    }

    fn workspace_focused_event(workspace_id: &str) -> EventEnvelope {
        EventEnvelope {
            event: EventKind::WorkspaceFocused,
            data: EventData::WorkspaceFocused {
                workspace_id: workspace_id.into(),
            },
        }
    }

    fn pane_info_with_scroll(scroll: Option<PaneScrollInfo>) -> PaneInfo {
        PaneInfo {
            pane_id: "pane_1".into(),
            terminal_id: "terminal_1".into(),
            workspace_id: "workspace_1".into(),
            tab_id: "tab_1".into(),
            focused: true,
            settled_at: None,
            snoozed_until: None,
            restore_error: None,
            work_context: Default::default(),
            cwd: None,
            foreground_cwd: None,
            label: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: AgentStatus::Unknown,
            waiting_on_agents: false,
            wait: None,
            eta_s: None,
            reported_at: None,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            gates: Vec::new(),
            items: Vec::new(),
            decisions: Vec::new(),
            agent_session: None,
            scroll,
            revision: 0,
            last_turn_at: None,
        }
    }

    #[test]
    fn lifecycle_subscription_skips_history_but_keeps_setup_window_events() {
        let event_hub = EventHub::default();
        event_hub.push(workspace_focused_event("before_subscription"));
        let event_start_sequence = event_hub.current_sequence();
        event_hub.push(workspace_focused_event("during_setup"));

        let (api_tx, _api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut subscription = ActiveSubscription::new(
            Subscription::WorkspaceFocused {},
            "test",
            0,
            &api_tx,
            &event_hub,
            event_start_sequence,
        )
        .expect("workspace focus subscription");

        let setup_event = subscription
            .poll(&api_tx, &event_hub)
            .expect("setup-window event");
        assert_eq!(setup_event["data"]["workspace_id"], "during_setup");
        assert!(subscription.poll(&api_tx, &event_hub).is_none());

        event_hub.push(workspace_focused_event("after_setup"));
        let live_event = subscription.poll(&api_tx, &event_hub).expect("live event");
        assert_eq!(live_event["data"]["workspace_id"], "after_setup");
    }

    #[test]
    fn workspace_metadata_subscription_uses_dedicated_event_kind() {
        let event_hub = EventHub::default();
        let (api_tx, _api_rx) = tokio::sync::mpsc::unbounded_channel();
        let subscription = ActiveSubscription::new(
            Subscription::WorkspaceMetadataUpdated {},
            "test",
            0,
            &api_tx,
            &event_hub,
            event_hub.current_sequence(),
        )
        .expect("workspace metadata subscription");

        assert!(matches!(
            subscription,
            ActiveSubscription::Event(ActiveEventSubscription {
                event_kind: EventKind::WorkspaceMetadataUpdated,
                ..
            })
        ));
    }

    #[test]
    fn session_changed_subscription_delivers_revision_and_snapshot() {
        let event_hub = EventHub::default();
        let (api_tx, _api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut subscription = ActiveSubscription::new(
            Subscription::SessionChanged {},
            "test",
            0,
            &api_tx,
            &event_hub,
            event_hub.current_sequence(),
        )
        .expect("session changed subscription");
        event_hub.push(crate::api::schema::EventEnvelope {
            event: EventKind::SessionChanged,
            data: crate::api::schema::EventData::SessionChanged {
                epoch: "epoch-1".into(),
                revision: 7,
                snapshot: Box::new(crate::api::schema::SessionSnapshot {
                    epoch: Some("epoch-1".into()),
                    revision: Some(7),
                    version: "0.9.1".into(),
                    protocol: 16,
                    focused_workspace_id: None,
                    focused_tab_id: None,
                    focused_pane_id: None,
                    planning_lock: None,
                    workspaces: Vec::new(),
                    tabs: Vec::new(),
                    panes: Vec::new(),
                    layouts: Vec::new(),
                    agents: Vec::new(),
                }),
            },
        });

        let event = subscription
            .poll(&api_tx, &event_hub)
            .expect("session change event");

        assert_eq!(event["event"], "session.changed");
        assert_eq!(event["data"]["revision"], 7);
        assert_eq!(event["data"]["epoch"], "epoch-1");
        assert_eq!(event["data"]["snapshot"]["revision"], 7);
    }

    #[test]
    fn session_changed_poll_returns_newest_of_queued_snapshots() {
        let event_hub = EventHub::default();
        let (api_tx, _api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut subscription = ActiveSubscription::new(
            Subscription::SessionChanged {},
            "test",
            0,
            &api_tx,
            &event_hub,
            event_hub.current_sequence(),
        )
        .expect("session changed subscription");

        for revision in [3, 4, 5] {
            event_hub.push(crate::api::schema::EventEnvelope {
                event: EventKind::SessionChanged,
                data: crate::api::schema::EventData::SessionChanged {
                    epoch: "epoch-1".into(),
                    revision,
                    snapshot: Box::new(crate::api::schema::SessionSnapshot {
                        epoch: Some("epoch-1".into()),
                        revision: Some(revision),
                        version: "0.9.1".into(),
                        protocol: 16,
                        focused_workspace_id: None,
                        focused_tab_id: None,
                        focused_pane_id: None,
                        planning_lock: None,
                        workspaces: Vec::new(),
                        tabs: Vec::new(),
                        panes: Vec::new(),
                        layouts: Vec::new(),
                        agents: Vec::new(),
                    }),
                },
            });
        }

        let event = subscription
            .poll(&api_tx, &event_hub)
            .expect("newest session change event");
        assert_eq!(event["data"]["revision"], 5);
        assert_eq!(event["data"]["snapshot"]["revision"], 5);
        assert!(subscription.poll(&api_tx, &event_hub).is_none());
    }

    #[test]
    fn scroll_subscription_emits_when_scroll_snapshot_changes() {
        let at_bottom = PaneScrollInfo {
            offset_from_bottom: 0,
            max_offset_from_bottom: 40,
            viewport_rows: 20,
        };
        let scrolled_back = PaneScrollInfo {
            offset_from_bottom: 8,
            max_offset_from_bottom: 40,
            viewport_rows: 20,
        };
        let mut subscription = ActiveScrollChangedSubscription {
            pane_id: "pane_1".into(),
            last_scroll: Some(at_bottom),
            request_prefix: "test".into(),
        };

        assert!(subscription
            .event_from_snapshot(pane_info_with_scroll(Some(at_bottom)))
            .is_none());

        let event = subscription
            .event_from_snapshot(pane_info_with_scroll(Some(scrolled_back)))
            .expect("scroll event");
        assert_eq!(event.event, SubscriptionEventKind::ScrollChanged);
        let SubscriptionEventData::ScrollChanged(data) = event.data else {
            panic!("wrong event data");
        };
        assert_eq!(data.pane_id, "pane_1");
        assert_eq!(data.workspace_id, "workspace_1");
        assert_eq!(data.scroll, scrolled_back);
    }

    #[test]
    fn agent_status_subscription_replays_queued_metadata_set_and_expiry_events() {
        let event_hub = EventHub::default();
        let mut subscription = ActiveAgentStatusChangedSubscription {
            pane_id: "pane_1".into(),
            status_filter: None,
            last_status: Some(AgentStatus::Working),
            last_presentation: Some(PanePresentationSnapshot {
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
                wait: None,
                eta_s: None,
                reported_at: None,
                waiting_on_agents: false,
                last_turn_at: None,
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: None,
            request_prefix: "test".into(),
        };

        event_hub.push(presentation_event(Some("short lived")));
        event_hub.push(presentation_event(None));

        let set_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("set event");
        let SubscriptionEventData::PaneAgentStatusChanged(set_data) = set_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(set_data.title.as_deref(), Some("short lived"));

        let expiry_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("expiry event");
        let SubscriptionEventData::PaneAgentStatusChanged(expiry_data) = expiry_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(expiry_data.title, None);
    }

    #[test]
    fn agent_status_subscription_forwards_a_waiting_projection_change() {
        let event_hub = EventHub::default();
        let mut subscription = ActiveAgentStatusChangedSubscription {
            pane_id: "pane_1".into(),
            status_filter: None,
            last_status: Some(AgentStatus::Working),
            last_presentation: Some(PanePresentationSnapshot {
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
                wait: None,
                eta_s: None,
                reported_at: None,
                waiting_on_agents: false,
                last_turn_at: None,
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: None,
            request_prefix: "test".into(),
        };
        let mut event = presentation_event(None);
        let EventData::PaneAgentStatusChanged {
            waiting_on_agents, ..
        } = &mut event.data
        else {
            panic!("waiting event")
        };
        *waiting_on_agents = true;
        event_hub.push(event);

        let event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("waiting projection event");
        let SubscriptionEventData::PaneAgentStatusChanged(data) = event.data else {
            panic!("wrong event data");
        };
        assert_eq!(data.agent_status, AgentStatus::Working);
        assert!(data.waiting_on_agents);
    }

    #[test]
    fn agent_status_subscription_prefers_setup_window_events_over_initial_snapshot() {
        let event_hub = EventHub::default();
        let mut subscription = ActiveAgentStatusChangedSubscription {
            pane_id: "pane_1".into(),
            status_filter: Some(AgentStatus::Working),
            last_status: Some(AgentStatus::Working),
            last_presentation: Some(PanePresentationSnapshot {
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
                wait: None,
                eta_s: None,
                reported_at: None,
                waiting_on_agents: false,
                last_turn_at: None,
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: Some(PaneAgentStatusChangedEvent {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                waiting_on_agents: false,
                wait: None,
                eta_s: None,
                reported_at: None,
                agent: Some("pi".into()),
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
                last_turn_at: None,
            }),
            request_prefix: "test".into(),
        };

        event_hub.push(presentation_event(Some("short lived")));
        event_hub.push(presentation_event(None));

        let set_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("set event");
        let SubscriptionEventData::PaneAgentStatusChanged(set_data) = set_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(set_data.title.as_deref(), Some("short lived"));

        let expiry_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("expiry event");
        let SubscriptionEventData::PaneAgentStatusChanged(expiry_data) = expiry_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(expiry_data.title, None);
    }

    #[test]
    fn agent_status_subscription_emits_setup_window_event_already_reflected_by_probe() {
        let event_hub = EventHub::default();
        let mut subscription = ActiveAgentStatusChangedSubscription {
            pane_id: "pane_1".into(),
            status_filter: Some(AgentStatus::Working),
            last_status: Some(AgentStatus::Working),
            last_presentation: Some(PanePresentationSnapshot {
                title: Some("short lived".into()),
                display_agent: None,
                state_labels: HashMap::new(),
                wait: None,
                eta_s: None,
                reported_at: None,
                waiting_on_agents: false,
                last_turn_at: None,
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: Some(PaneAgentStatusChangedEvent {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                waiting_on_agents: false,
                wait: None,
                eta_s: None,
                reported_at: None,
                agent: Some("pi".into()),
                title: Some("short lived".into()),
                display_agent: None,
                state_labels: HashMap::new(),
                last_turn_at: None,
            }),
            request_prefix: "test".into(),
        };

        event_hub.push(presentation_event(Some("short lived")));

        let event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("setup-window event");
        let SubscriptionEventData::PaneAgentStatusChanged(data) = event.data else {
            panic!("wrong event data");
        };
        assert_eq!(data.title.as_deref(), Some("short lived"));
        assert!(subscription.initial_event.is_none());
    }
}
