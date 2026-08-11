use regex::Regex;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneAgentStatusChangedEvent, PaneOutputMatchedEvent,
    PaneScrollChangedEvent, PaneScrollInfo, Request, Subscription, SubscriptionEventData,
    SubscriptionEventEnvelope, SubscriptionEventKind,
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
}

impl PanePresentationSnapshot {
    fn from(pane: &crate::api::schema::PaneInfo) -> Self {
        Self {
            title: pane.title.clone(),
            display_agent: pane.display_agent.clone(),
            state_labels: pane.state_labels.clone(),
        }
    }

    fn from_event(
        title: &Option<String>,
        display_agent: &Option<String>,
        state_labels: &std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            title: title.clone(),
            display_agent: display_agent.clone(),
            state_labels: state_labels.clone(),
        }
    }
}

pub(super) struct ActiveEventSubscription {
    event_kind: crate::api::schema::EventKind,
    last_sequence: u64,
}

pub(super) enum ActiveSubscription {
    Event(ActiveEventSubscription),
    OutputMatched(ActiveOutputMatchedSubscription),
    AgentStatusChanged(Box<ActiveAgentStatusChangedSubscription>),
    ScrollChanged(ActiveScrollChangedSubscription),
    ConversationChanged(ActiveConversationChangedSubscription),
}

/// Tracks one pane-scoped conversation subscription. Only metadata is polled;
/// payloads remain behind the conversation read API.
pub(super) struct ActiveConversationChangedSubscription {
    pub(super) pane_id: String,
    pub(super) workspace_id: String,
    pub(super) session: Option<crate::api::schema::ConversationSessionIdentity>,
    pub(super) reader_generation: Option<String>,
    pub(super) last_revision: u64,
    pub(super) request_prefix: String,
}

impl ActiveSubscription {
    pub(super) fn new(
        subscription: Subscription,
        request_id: &str,
        index: usize,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Self, ErrorResponse> {
        match subscription {
            Subscription::WorkspaceCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceCreated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceUpdated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceMetadataUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceMetadataUpdated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceRenamed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceRenamed,
                last_sequence: 0,
            })),
            Subscription::WorkspaceMoved {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceMoved,
                last_sequence: 0,
            })),
            Subscription::WorkspaceReordered {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceReordered,
                last_sequence: 0,
            })),
            Subscription::WorkspaceClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceClosed,
                last_sequence: 0,
            })),
            Subscription::WorkspaceFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceFocused,
                last_sequence: 0,
            })),
            Subscription::WorktreeCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorktreeCreated,
                last_sequence: 0,
            })),
            Subscription::WorktreeOpened {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorktreeOpened,
                last_sequence: 0,
            })),
            Subscription::WorktreeRemoved {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorktreeRemoved,
                last_sequence: 0,
            })),
            Subscription::TabCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabCreated,
                last_sequence: 0,
            })),
            Subscription::TabClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabClosed,
                last_sequence: 0,
            })),
            Subscription::TabFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabFocused,
                last_sequence: 0,
            })),
            Subscription::TabRenamed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabRenamed,
                last_sequence: 0,
            })),
            Subscription::TabMoved {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabMoved,
                last_sequence: 0,
            })),
            Subscription::PaneCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneCreated,
                last_sequence: 0,
            })),
            Subscription::PaneClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneClosed,
                last_sequence: 0,
            })),
            Subscription::PaneUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneUpdated,
                last_sequence: 0,
            })),
            Subscription::PaneFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneFocused,
                last_sequence: 0,
            })),
            Subscription::PaneMoved {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneMoved,
                last_sequence: 0,
            })),
            Subscription::PaneExited {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneExited,
                last_sequence: 0,
            })),
            Subscription::PaneAgentDetected {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneAgentDetected,
                last_sequence: 0,
            })),
            Subscription::LayoutUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::LayoutUpdated,
                last_sequence: 0,
            })),
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
            Subscription::AgentConversationChanged { pane_id } => {
                let probe = pane_get(format!("{request_id}:sub:{index}:probe"), &pane_id, api_tx)?;
                let request_prefix = format!("{request_id}:sub:{index}");
                let metadata = conversation_metadata(
                    format!("{request_prefix}:read"),
                    &probe.pane_id,
                    api_tx,
                )?;
                let (session, reader_generation, last_revision) = metadata
                    .map(|metadata| {
                        (
                            Some(metadata.session),
                            Some(metadata.reader_generation),
                            metadata.revision,
                        )
                    })
                    .unwrap_or((None, None, 0));
                Ok(Self::ConversationChanged(
                    ActiveConversationChangedSubscription {
                        pane_id: probe.pane_id,
                        workspace_id: probe.workspace_id,
                        session,
                        reader_generation,
                        last_revision,
                        request_prefix,
                    },
                ))
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
            Self::ConversationChanged(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
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
        let mut saw_status_event = false;
        for (sequence, event) in event_hub.events_after(self.last_sequence) {
            self.last_sequence = sequence;
            let crate::api::schema::EventData::PaneAgentStatusChanged {
                pane_id,
                workspace_id,
                agent_status,
                agent,
                title,
                display_agent,
                state_labels,
            } = event.data
            else {
                continue;
            };
            if event.event != crate::api::schema::EventKind::PaneAgentStatusChanged {
                continue;
            }
            if pane_id != self.pane_id {
                continue;
            }
            saw_status_event = true;

            let current_presentation =
                PanePresentationSnapshot::from_event(&title, &display_agent, &state_labels);
            self.last_status = Some(agent_status);
            self.last_presentation = Some(current_presentation);
            if self
                .status_filter
                .is_some_and(|wanted| wanted != agent_status)
            {
                continue;
            }

            self.initial_event = None;
            return Ok(Some(SubscriptionEventEnvelope {
                event: SubscriptionEventKind::PaneAgentStatusChanged,
                data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                    pane_id,
                    workspace_id,
                    agent_status,
                    agent,
                    title,
                    display_agent,
                    state_labels,
                }),
            }));
        }

        if saw_status_event {
            self.initial_event = None;
        } else if event_hub.current_sequence() != self.last_sequence {
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

#[derive(Debug)]
struct ConversationMetadata {
    session: crate::api::schema::ConversationSessionIdentity,
    reader_generation: String,
    revision: u64,
    reset_required: bool,
}

impl ActiveConversationChangedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let metadata = conversation_metadata(
            format!("{}:read", self.request_prefix),
            &self.pane_id,
            api_tx,
        )
        .ok()??;
        self.event_from_metadata(
            metadata.session,
            metadata.reader_generation,
            metadata.revision,
            metadata.reset_required,
        )
    }

    fn event_from_metadata(
        &mut self,
        session: crate::api::schema::ConversationSessionIdentity,
        reader_generation: String,
        revision: u64,
        reset_required: bool,
    ) -> Option<SubscriptionEventEnvelope> {
        let session_changed = self.session.as_ref() != Some(&session);
        let generation_changed = self.reader_generation.as_deref() != Some(&reader_generation);
        let revision_changed = self.last_revision != revision;
        let changed = session_changed || generation_changed || revision_changed || reset_required;
        self.session = Some(session.clone());
        self.reader_generation = Some(reader_generation.clone());
        self.last_revision = revision;
        if !changed {
            return None;
        }
        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::AgentConversationChanged,
            data: SubscriptionEventData::AgentConversationChanged(
                crate::api::schema::AgentConversationChangedEvent {
                    pane_id: self.pane_id.clone(),
                    workspace_id: self.workspace_id.clone(),
                    session,
                    reader_generation,
                    revision,
                    reset_required: reset_required || session_changed || generation_changed,
                },
            ),
        })
    }
}

fn conversation_metadata(
    request_id: String,
    pane_id: &str,
    api_tx: &ApiRequestSender,
) -> Result<Option<ConversationMetadata>, ErrorResponse> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::AgentConversationMetadata(
                crate::api::schema::AgentConversationReadParams {
                    target: pane_id.to_owned(),
                    cursor: None,
                    direction: crate::api::schema::ConversationPageDirection::Newest,
                    limit: 1,
                },
            ),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|_| ErrorResponse {
        id: request_id.clone(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode conversation metadata response".into(),
        },
    })?;
    if value.get("error").is_some() {
        return Ok(None);
    }
    let read: crate::api::schema::ConversationReadResult =
        serde_json::from_value(value["result"]["read"].clone()).map_err(|_| ErrorResponse {
            id: request_id,
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode conversation metadata".into(),
            },
        })?;
    Ok(Some(match read {
        crate::api::schema::ConversationReadResult::Page { page } => ConversationMetadata {
            session: page.session,
            reader_generation: page.reader_generation,
            revision: page.revision,
            reset_required: false,
        },
        crate::api::schema::ConversationReadResult::ResetRequired {
            session,
            reader_generation,
        } => ConversationMetadata {
            session,
            reader_generation,
            revision: 0,
            reset_required: true,
        },
    }))
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
    use crate::api::schema::{AgentStatus, EventData, EventEnvelope, EventKind, PaneInfo};

    fn presentation_event(title: Option<&str>) -> EventEnvelope {
        EventEnvelope {
            event: EventKind::PaneAgentStatusChanged,
            data: EventData::PaneAgentStatusChanged {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("pi".into()),
                title: title.map(str::to_string),
                display_agent: None,
                state_labels: HashMap::new(),
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
            cwd: None,
            foreground_cwd: None,
            label: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: AgentStatus::Unknown,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            conversation_session: None,
            conversation_capability: None,
            scroll,
            revision: 0,
        }
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
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: Some(PaneAgentStatusChangedEvent {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("pi".into()),
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
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
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: Some(PaneAgentStatusChangedEvent {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("pi".into()),
                title: Some("short lived".into()),
                display_agent: None,
                state_labels: HashMap::new(),
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
    #[test]
    fn conversation_subscription_emits_only_for_revision_or_identity_changes() {
        let mut subscription = ActiveConversationChangedSubscription {
            pane_id: "pane-1".into(),
            workspace_id: "workspace-1".into(),
            session: Some(crate::api::schema::ConversationSessionIdentity {
                id: "session-1".into(),
            }),
            reader_generation: Some("generation-1".into()),
            last_revision: 4,
            request_prefix: "test".into(),
        };

        assert!(subscription
            .event_from_metadata(
                crate::api::schema::ConversationSessionIdentity {
                    id: "session-1".into(),
                },
                "generation-1".into(),
                4,
                false,
            )
            .is_none());

        let event = subscription
            .event_from_metadata(
                crate::api::schema::ConversationSessionIdentity {
                    id: "session-1".into(),
                },
                "generation-1".into(),
                5,
                false,
            )
            .expect("revision event");
        assert_eq!(event.event, SubscriptionEventKind::AgentConversationChanged);
        let SubscriptionEventData::AgentConversationChanged(data) = event.data else {
            panic!("wrong event data");
        };
        assert_eq!(data.revision, 5);
        assert!(!data.reset_required);

        let reset = subscription
            .event_from_metadata(
                crate::api::schema::ConversationSessionIdentity {
                    id: "session-2".into(),
                },
                "generation-2".into(),
                1,
                true,
            )
            .expect("reset event");
        let SubscriptionEventData::AgentConversationChanged(data) = reset.data else {
            panic!("wrong reset data");
        };
        assert!(data.reset_required);
        assert_eq!(data.session.id, "session-2");
    }
}
