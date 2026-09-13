//! Endpoint-qualified rows shared by aggregate navigation surfaces.

use super::*;
use crate::protocol::ClientShellAgent;

#[derive(Clone, Copy)]
pub(super) struct CachedEndpointSnapshot<'a> {
    pub(super) endpoint_index: usize,
    pub(super) endpoint_id: &'a ClientEndpointId,
    pub(super) label: &'a str,
    pub(super) status: ClientEndpointStatus,
    pub(super) snapshot: &'a ClientShellSnapshot,
    pub(super) agent_recency: &'a HashMap<String, u64>,
    pub(super) agent_presentation: &'a super::endpoint_agent_state::EndpointAgentPresentation,
}

impl CachedEndpointSnapshot<'_> {
    pub(super) fn stale(self) -> bool {
        self.status != ClientEndpointStatus::Online
    }
}

pub(super) fn cached_endpoint_snapshots(
    endpoints: &[ClientShellEndpoint],
) -> impl Iterator<Item = CachedEndpointSnapshot<'_>> {
    endpoints
        .iter()
        .enumerate()
        .filter_map(|(endpoint_index, endpoint)| {
            endpoint
                .snapshot
                .as_deref()
                .map(|snapshot| CachedEndpointSnapshot {
                    endpoint_index,
                    endpoint_id: &endpoint.endpoint_id,
                    label: &endpoint.label,
                    status: endpoint.status,
                    snapshot,
                    agent_recency: &endpoint.agent_recency,
                    agent_presentation: &endpoint.agent_presentation,
                })
        })
}

pub(super) struct AggregateAgentRow<'a> {
    pub(super) endpoint: CachedEndpointSnapshot<'a>,
    pub(super) agent: &'a ClientShellAgent,
    pub(super) recency: u64,
}

pub(super) struct AggregateAgentTarget {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) pane_id: String,
}

pub(super) fn aggregate_agent_rows<'a>(
    endpoints: &'a [ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<AggregateAgentRow<'a>> {
    let active_index = endpoints
        .iter()
        .position(|endpoint| &endpoint.endpoint_id == active_endpoint_id);
    let active_view = active_index.and_then(|index| {
        let endpoint = &endpoints[index];
        if !endpoint.agent_view_projection_supported {
            return None;
        }
        match ClientShellState::endpoint_agent_view(endpoint) {
            Some(Ok(view)) => Some(Ok(view.as_ref())),
            Some(Err(())) => Some(Err(())),
            None if endpoint
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.agent_view_label.is_none()) =>
            {
                Some(Ok(None))
            }
            None => Some(Err(())),
        }
    });

    if let Some(Ok(view)) = active_view {
        let mut rows = cached_endpoint_snapshots(endpoints)
            .flat_map(|endpoint| {
                endpoint
                    .snapshot
                    .agents
                    .iter()
                    .map(move |agent| AggregateAgentRow {
                        recency: endpoint
                            .agent_recency
                            .get(&agent.pane_id)
                            .copied()
                            .unwrap_or_default(),
                        endpoint,
                        agent,
                    })
            })
            .collect::<Vec<_>>();
        if let Some(view) = view {
            let context = active_index
                .and_then(|index| endpoints[index].snapshot.as_deref())
                .map(|snapshot| crate::agent_view_eval::AgentViewContext {
                    scope: active_index.unwrap_or_default(),
                    workspace_id: snapshot.focused_workspace_id.clone(),
                    tab_id: snapshot.focused_tab_id.clone(),
                });
            if let (Some(context), Some(filter)) = (context.as_ref(), view.filter.as_ref()) {
                rows.retain(|row| {
                    crate::agent_view_eval::matches_filter(
                        context,
                        &ClientAgentViewEntry::new(row),
                        filter,
                    )
                });
            }
            if !view.sort.is_empty() {
                rows.sort_by(|left, right| {
                    crate::agent_view_eval::compare_entries(
                        &ClientAgentViewEntry::new(left),
                        &ClientAgentViewEntry::new(right),
                        &view.sort,
                    )
                });
                return rows;
            }
        }
        sort_aggregate_rows(&mut rows, sort);
        return rows;
    }

    let mut rows = cached_endpoint_snapshots(endpoints)
        .flat_map(|endpoint| {
            super::agent_sidebar::ordered_agent_pane_ids(endpoint.snapshot, sort)
                .into_iter()
                .filter_map(move |pane_id| {
                    let agent = endpoint
                        .snapshot
                        .agents
                        .iter()
                        .find(|agent| agent.pane_id == pane_id)?;
                    Some(AggregateAgentRow {
                        recency: endpoint
                            .agent_recency
                            .get(&pane_id)
                            .copied()
                            .unwrap_or_default(),
                        endpoint,
                        agent,
                    })
                })
        })
        .collect::<Vec<_>>();
    sort_aggregate_rows(&mut rows, sort);
    rows
}

fn sort_aggregate_rows(
    rows: &mut [AggregateAgentRow<'_>],
    sort: crate::config::AgentPanelSortConfig,
) {
    if sort == crate::config::AgentPanelSortConfig::Priority {
        rows.sort_by_key(|row| {
            (
                row.endpoint.stale(),
                std::cmp::Reverse(status_priority(row.agent.agent_status)),
                std::cmp::Reverse(row.recency),
            )
        });
    }
}

struct ClientAgentViewEntry<'a> {
    endpoint_index: usize,
    snapshot: &'a ClientShellSnapshot,
    agent: &'a ClientShellAgent,
    seen: bool,
}

impl<'a> ClientAgentViewEntry<'a> {
    fn new(row: &AggregateAgentRow<'a>) -> Self {
        Self {
            endpoint_index: row.endpoint.endpoint_index,
            snapshot: row.endpoint.snapshot,
            agent: row.agent,
            seen: row.endpoint.agent_presentation.seen(row.agent),
        }
    }
}

impl crate::agent_view_eval::AgentViewEntry for ClientAgentViewEntry<'_> {
    fn scope(&self) -> usize {
        self.endpoint_index
    }

    fn status(&self) -> &'static str {
        status_text(self.agent.agent_status)
    }

    fn workspace_id(&self) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(&self.agent.workspace_id))
    }

    fn tab_id(&self) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(&self.agent.tab_id))
    }

    fn pane_id(&self) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(&self.agent.pane_id))
    }

    fn agent(&self) -> Option<&str> {
        self.agent.agent.as_deref()
    }

    fn seen(&self) -> bool {
        self.seen
    }

    fn state_change_seq(&self) -> Option<u64> {
        Some(self.agent.state_change_seq)
    }

    fn token(&self, token: &str) -> Option<&str> {
        self.agent
            .tokens
            .iter()
            .find(|(name, _)| name == token)
            .map(|(_, value)| value.as_str())
    }

    fn workspace_order(&self) -> Option<u64> {
        self.snapshot
            .workspaces
            .iter()
            .position(|workspace| workspace.workspace_id == self.agent.workspace_id)
            .map(|index| index as u64)
    }

    fn tab_order(&self) -> Option<u64> {
        self.snapshot
            .tabs
            .iter()
            .find(|tab| tab.tab_id == self.agent.tab_id)
            .map(|tab| tab.number as u64)
    }

    fn pane_order(&self) -> Option<u64> {
        let suffix = self
            .agent
            .pane_id
            .strip_prefix(&self.agent.workspace_id)?
            .strip_prefix(":p")?;
        crate::workspace::decode_public_number(suffix).map(|number| number as u64)
    }

    fn attention(&self) -> u64 {
        u64::from(status_priority(self.agent.agent_status))
    }
}

pub(super) fn online_agent_targets(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<AggregateAgentTarget> {
    aggregate_agent_rows(endpoints, active_endpoint_id, sort)
        .into_iter()
        .filter(|row| !row.endpoint.stale())
        .map(|row| AggregateAgentTarget {
            endpoint_id: row.endpoint.endpoint_id.clone(),
            pane_id: row.agent.pane_id.clone(),
        })
        .collect()
}

pub(super) fn navigator_rows(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    navigator: &ClientNavigatorOverlay,
) -> Vec<ClientNavigatorRow> {
    let query = navigator.query.trim().to_lowercase();
    let filter = |status| match navigator.filter {
        Some(ClientNavigatorFilter::Blocked) => status == crate::api::schema::AgentStatus::Blocked,
        Some(ClientNavigatorFilter::Working) => status == crate::api::schema::AgentStatus::Working,
        Some(ClientNavigatorFilter::Idle) => status == crate::api::schema::AgentStatus::Idle,
        Some(ClientNavigatorFilter::Done) => status == crate::api::schema::AgentStatus::Done,
        None => true,
    };
    let text = |value: &str| query.is_empty() || value.to_lowercase().contains(&query);
    let filtering = navigator.filter.is_some() || !query.is_empty();
    let federated = endpoints.len() > 1;
    let depth_offset = u8::from(federated);
    let mut rows = Vec::new();

    for endpoint in endpoints {
        let stale = endpoint.status != ClientEndpointStatus::Online;
        let endpoint_query_matches = !query.is_empty() && text(&endpoint.label);
        let mut endpoint_rows = Vec::new();
        if let Some(snapshot) = endpoint.snapshot.as_deref() {
            for workspace in &snapshot.workspaces {
                let workspace_meta = workspace.branch.clone().unwrap_or_default();
                let mut children = Vec::new();
                for tab in snapshot
                    .tabs
                    .iter()
                    .filter(|tab| tab.workspace_id == workspace.workspace_id)
                {
                    let mut panes = Vec::new();
                    for (index, pane) in snapshot
                        .panes
                        .iter()
                        .filter(|pane| pane.tab_id == tab.tab_id)
                        .enumerate()
                    {
                        let agent = snapshot
                            .agents
                            .iter()
                            .find(|agent| agent.pane_id == pane.pane_id);
                        let status = agent
                            .map_or(crate::api::schema::AgentStatus::Unknown, |agent| {
                                agent.agent_status
                            });
                        let label = pane
                            .label
                            .clone()
                            .or_else(|| agent.and_then(|agent| agent.name.clone()))
                            .or_else(|| agent.and_then(|agent| agent.display_agent.clone()))
                            .or_else(|| agent.and_then(|agent| agent.title.clone()))
                            .unwrap_or_else(|| format!("pane {}", index + 1));
                        let meta = pane
                            .foreground_cwd
                            .clone()
                            .or_else(|| pane.cwd.clone())
                            .unwrap_or_default();
                        if !filtering
                            || filter(status)
                                && (endpoint_query_matches || text(&label) || text(&meta))
                        {
                            panes.push(ClientNavigatorRow {
                                depth: 2 + depth_offset,
                                label,
                                meta,
                                status: Some(status),
                                stale,
                                current: endpoint.endpoint_id == *active_endpoint_id
                                    && snapshot.focused_pane_id.as_deref() == Some(&pane.pane_id),
                                target: ClientNavigatorTarget::Pane {
                                    endpoint_id: endpoint.endpoint_id.clone(),
                                    pane_id: pane.pane_id.clone(),
                                },
                            });
                        }
                    }
                    if !filtering
                        || filter(tab.agent_status) && (endpoint_query_matches || text(&tab.label))
                        || !panes.is_empty()
                    {
                        children.push(ClientNavigatorRow {
                            depth: 1 + depth_offset,
                            label: tab.label.clone(),
                            meta: format!(
                                "{} panes",
                                snapshot
                                    .panes
                                    .iter()
                                    .filter(|pane| pane.tab_id == tab.tab_id)
                                    .count()
                            ),
                            status: None,
                            stale,
                            current: false,
                            target: ClientNavigatorTarget::Tab {
                                endpoint_id: endpoint.endpoint_id.clone(),
                                tab_id: tab.tab_id.clone(),
                            },
                        });
                        children.extend(panes);
                    }
                }
                let workspace_matches = filter(workspace.agent_status)
                    && (endpoint_query_matches || text(&workspace.label) || text(&workspace_meta));
                if !filtering || workspace_matches || !children.is_empty() {
                    let key = (endpoint.endpoint_id.clone(), workspace.workspace_id.clone());
                    endpoint_rows.push(ClientNavigatorRow {
                        depth: depth_offset,
                        label: workspace.label.clone(),
                        meta: workspace_meta,
                        status: None,
                        stale,
                        current: false,
                        target: ClientNavigatorTarget::Workspace {
                            endpoint_id: endpoint.endpoint_id.clone(),
                            workspace_id: workspace.workspace_id.clone(),
                        },
                    });
                    if navigator.expanded_workspaces.contains(&key) || filtering {
                        endpoint_rows.extend(children);
                    }
                }
            }
        }
        if !filtering || endpoint_query_matches || !endpoint_rows.is_empty() {
            if federated {
                rows.push(ClientNavigatorRow {
                    depth: 0,
                    label: endpoint.label.to_owned(),
                    meta: String::new(),
                    status: None,
                    stale,
                    current: false,
                    target: ClientNavigatorTarget::Machine {
                        endpoint_id: endpoint.endpoint_id.clone(),
                    },
                });
            }
            rows.extend(endpoint_rows);
        }
    }
    rows
}

pub(super) fn navigator_selected_index(
    rows: &[ClientNavigatorRow],
    navigator: &ClientNavigatorOverlay,
) -> Option<usize> {
    match navigator.selected.as_ref() {
        Some(target) => rows.iter().position(|row| row.target == *target),
        None => (!rows.is_empty()).then_some(0),
    }
}

pub(super) fn selected_navigator_target(
    rows: &[ClientNavigatorRow],
    navigator: &ClientNavigatorOverlay,
) -> Option<ClientNavigatorTarget> {
    navigator_selected_index(rows, navigator).map(|index| rows[index].target.clone())
}
