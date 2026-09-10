//! Endpoint-qualified rows shared by aggregate navigation surfaces.

use super::*;
use crate::protocol::ClientShellAgent;

#[derive(Clone, Copy)]
pub(super) struct CachedEndpointSnapshot<'a> {
    pub(super) endpoint_id: &'a ClientEndpointId,
    pub(super) label: &'a str,
    pub(super) status: ClientEndpointStatus,
    pub(super) snapshot: &'a ClientShellSnapshot,
    pub(super) agent_recency: &'a HashMap<String, u64>,
}

impl CachedEndpointSnapshot<'_> {
    pub(super) fn stale(self) -> bool {
        self.status != ClientEndpointStatus::Online
    }
}

pub(super) fn cached_endpoint_snapshots(
    endpoints: &[ClientShellEndpoint],
) -> impl Iterator<Item = CachedEndpointSnapshot<'_>> {
    endpoints.iter().filter_map(|endpoint| {
        endpoint
            .snapshot
            .as_deref()
            .map(|snapshot| CachedEndpointSnapshot {
                endpoint_id: &endpoint.endpoint_id,
                label: &endpoint.label,
                status: endpoint.status,
                snapshot,
                agent_recency: &endpoint.agent_recency,
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

pub(super) fn aggregate_agent_rows(
    endpoints: &[ClientShellEndpoint],
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<AggregateAgentRow<'_>> {
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
    if sort == crate::config::AgentPanelSortConfig::Priority {
        rows.sort_by_key(|row| {
            (
                row.endpoint.stale(),
                std::cmp::Reverse(status_priority(row.agent.agent_status)),
                std::cmp::Reverse(row.recency),
            )
        });
    }
    rows
}

pub(super) fn online_agent_targets(
    endpoints: &[ClientShellEndpoint],
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<AggregateAgentTarget> {
    aggregate_agent_rows(endpoints, sort)
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
                        let agent_label = agent
                            .and_then(|agent| agent.agent.as_deref())
                            .map(navigator_agent_label);
                        let meta = pane
                            .foreground_cwd
                            .clone()
                            .or_else(|| pane.cwd.clone())
                            .unwrap_or_default();
                        if !filtering
                            || filter(status)
                                && (endpoint_query_matches
                                    || text(&label)
                                    || text(&meta)
                                    || agent_label.as_deref().is_some_and(|agent| text(agent)))
                        {
                            panes.push(ClientNavigatorRow {
                                depth: 2 + depth_offset,
                                label,
                                agent: agent_label,
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
                            agent: None,
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
                        agent: None,
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
                    agent: None,
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

fn navigator_agent_label(agent: &str) -> String {
    match agent {
        "agy" => "Antigravity".to_owned(),
        "claude" => "Claude Code".to_owned(),
        "copilot" => "GitHub Copilot".to_owned(),
        "omp" => "OMP".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        value => {
            let mut label = value.to_owned();
            if let Some(first) = label.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            label
        }
    }
}

#[cfg(test)]
mod tests {
    use super::navigator_agent_label;

    #[test]
    fn navigator_agent_labels_are_readable() {
        assert_eq!(navigator_agent_label("codex"), "Codex");
        assert_eq!(navigator_agent_label("claude"), "Claude Code");
        assert_eq!(navigator_agent_label("omp"), "OMP");
        assert_eq!(navigator_agent_label("opencode"), "OpenCode");
    }
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
