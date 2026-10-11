use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SidebarCoverageKey {
    ws_idx: usize,
    tab_idx: usize,
    pane_id: crate::layout::PaneId,
}

struct TrackedPaneCoverage {
    key: SidebarCoverageKey,
    report: crate::api::schema::PaneCoverage,
    entry: Option<AgentPanelEntry>,
}

pub(super) struct SidebarCoverageRecorder {
    records: Vec<TrackedPaneCoverage>,
    index: std::collections::HashMap<SidebarCoverageKey, usize>,
}

impl SidebarCoverageRecorder {
    pub(super) fn new(app: &AppState) -> Self {
        let mut records = Vec::new();
        let mut index = std::collections::HashMap::new();
        for (ws_idx, workspace) in app.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                let tab_number = workspace.public_tab_number(tab_idx).unwrap_or(tab_idx + 1);
                let tab_id = crate::workspace::public_tab_id_for_number(&workspace.id, tab_number);
                let mut pane_ids = tab.panes.keys().copied().collect::<Vec<_>>();
                pane_ids.sort_by_key(|pane_id| {
                    workspace.public_pane_number(*pane_id).unwrap_or(usize::MAX)
                });
                for (pane_idx, pane_id) in pane_ids.into_iter().enumerate() {
                    let pane_number = workspace.public_pane_number(pane_id);
                    let report_pane_id = pane_number.map_or_else(
                        || {
                            format!(
                                "{}:unknown-pane-{}-{}",
                                workspace.id,
                                tab_number,
                                pane_idx + 1
                            )
                        },
                        |number| crate::workspace::public_pane_id_for_number(&workspace.id, number),
                    );
                    let key = SidebarCoverageKey {
                        ws_idx,
                        tab_idx,
                        pane_id,
                    };
                    let report = crate::api::schema::PaneCoverage {
                        workspace_id: workspace.id.clone(),
                        tab_id: tab_id.clone(),
                        pane_id: report_pane_id,
                        placement: None,
                        dropped_by: pane_number
                            .is_none()
                            .then(|| "missing_public_id".to_string()),
                    };
                    index.insert(key, records.len());
                    records.push(TrackedPaneCoverage {
                        key,
                        report,
                        entry: None,
                    });
                }
            }
        }
        Self { records, index }
    }

    pub(super) fn bind_entries_with_fallbacks(
        &mut self,
        app: &AppState,
        entries: &[AgentPanelEntry],
    ) {
        for entry in entries {
            let Some(target) = entry.local_target() else {
                continue;
            };
            let key = SidebarCoverageKey {
                ws_idx: target.ws_idx,
                tab_idx: target.tab_idx,
                pane_id: target.pane_id,
            };
            if let Some(index) = self.index.get(&key).copied() {
                self.records[index].entry = Some(entry.clone());
            }
        }
        for record in &mut self.records {
            if record.entry.is_some() {
                continue;
            }
            let Some((workspace, tab, pane)) = app
                .workspaces
                .get(record.key.ws_idx)
                .and_then(|workspace| {
                    workspace
                        .tabs
                        .get(record.key.tab_idx)
                        .map(|tab| (workspace, tab))
                })
                .and_then(|(workspace, tab)| {
                    tab.panes
                        .get(&record.key.pane_id)
                        .map(|pane| (workspace, tab, pane))
                })
            else {
                record
                    .report
                    .dropped_by
                    .get_or_insert_with(|| "missing_pane_state".into());
                continue;
            };
            let pane_label = app
                .workspaces
                .get(record.key.ws_idx)
                .and_then(|workspace| workspace.public_pane_number(record.key.pane_id))
                .map_or_else(
                    || "Unknown pane".to_string(),
                    |number| format!("Pane {number}"),
                );
            let missing_terminal = !app.terminals.contains_key(&pane.attached_terminal_id);
            record.report.dropped_by.get_or_insert_with(|| {
                if missing_terminal {
                    "no_terminal_state".into()
                } else {
                    "no_sidebar_entry".into()
                }
            });
            let mut entry = AgentPanelEntry::new(
                AgentPanelIdentity::Local(AgentPanelLocalTarget {
                    ws_idx: record.key.ws_idx,
                    tab_idx: record.key.tab_idx,
                    pane_id: record.key.pane_id,
                }),
                AgentPanelEntryData {
                    primary_label: workspace.id.clone(),
                    space_label: workspace.id.clone(),
                    primary_tab_label: tab
                        .custom_name
                        .clone()
                        .or_else(|| Some("Unknown session".into())),
                    tab_has_custom_name: tab.custom_name.is_some(),
                    tab_label_leads_with_agent: false,
                    tab_has_live_agent_title: false,
                    pane_label: Some(pane_label),
                    pane_label_is_agent_identity: false,
                    terminal_title: None,
                    terminal_title_stripped: None,
                    agent_label: None,
                    agent_kind_label: None,
                    agent: None,
                    foreground_process_name: None,
                    agent_context: None,
                    has_agent: false,
                    prio: tab.prio,
                    starred: tab.starred,
                    state: AgentState::Unknown,
                    attention_tier: Some(crate::terminal::state::AttentionTier::None),
                    open_blockers: false,
                    completion_tier: None,
                    usage_limited: false,
                    active_subagents: None,
                    model_letter: None,
                    waiting_on_agents: false,
                    working_while_blocked: false,
                    holds_shell: false,
                    gate_count: 0,
                    seen: pane.seen,
                    done_since: pane.done_since,
                    stale: false,
                    reported_at: None,
                    last_agent_state_change_seq: None,
                    activity_at: None,
                    agent_activity_at: None,
                    human_activity_at: None,
                    state_labels: std::collections::HashMap::new(),
                    tokens: std::collections::HashMap::new(),
                    tab_first_pane: true,
                    remote_host: None,
                },
            );
            entry.pinned = tab.pinned;
            entry.parked = tab.parked;
            record.entry = Some(entry);
        }
    }

    pub(super) fn mark_dropped(&mut self, entry: &AgentPanelEntry, reason: &'static str) {
        let Some(target) = entry.local_target() else {
            return;
        };
        let key = SidebarCoverageKey {
            ws_idx: target.ws_idx,
            tab_idx: target.tab_idx,
            pane_id: target.pane_id,
        };
        let Some(index) = self.index.get(&key).copied() else {
            return;
        };
        let record = &mut self.records[index].report;
        if record.placement.is_none() {
            record.dropped_by.get_or_insert_with(|| reason.to_string());
        }
    }

    pub(super) fn record_placement(
        &mut self,
        entry: &AgentPanelEntry,
        section: &str,
        group: &str,
        collapsed: bool,
    ) {
        let Some(target) = entry.local_target() else {
            return;
        };
        let key = SidebarCoverageKey {
            ws_idx: target.ws_idx,
            tab_idx: target.tab_idx,
            pane_id: target.pane_id,
        };
        let Some(index) = self.index.get(&key).copied() else {
            return;
        };
        let report = &mut self.records[index].report;
        report.placement = Some(crate::api::schema::Placement {
            section: section.to_string(),
            group: group.to_string(),
            collapsed,
        });
        report.dropped_by = None;
    }

    pub(super) fn finish(&mut self, app: &AppState, rows: &[SidebarRow]) {
        let main_key = devices::group_key("main", &app.agent_host_name);
        let working_key = devices::group_key("working", &app.agent_host_name);
        let mut section = "main".to_string();
        let mut group = devices::device_title(app, &app.agent_host_name, true);
        for row in rows {
            match row {
                SidebarRow::SectionHeader { title, .. } => section = (*title).to_string(),
                SidebarRow::NestedHeader { key, title, .. } => {
                    if key == &main_key {
                        section = "main".to_string();
                    } else if key == &working_key {
                        section = WORKING_SECTION_TITLE.to_string();
                    }
                    group.clone_from(title);
                }
                SidebarRow::Workspace { ws_idx, .. } => {
                    if let Some(workspace) = app.workspaces.get(*ws_idx) {
                        group.clone_from(&workspace.id);
                    }
                }
                SidebarRow::Tab { entry, .. } | SidebarRow::Agent { entry, .. }
                    if entry.local_target().is_some() =>
                {
                    self.record_placement(entry, &section, &group, false);
                }
                // Needs-you rows duplicate pane targets from the main tree.
                _ => {}
            }
        }

        let mut tab_status = std::collections::HashMap::new();
        for record in &self.records {
            if record.report.placement.is_some() {
                tab_status.insert(
                    (record.key.ws_idx, record.key.tab_idx),
                    (record.report.placement.clone(), None),
                );
            }
        }
        for record in &self.records {
            if record.report.dropped_by.is_some() {
                tab_status
                    .entry((record.key.ws_idx, record.key.tab_idx))
                    .or_insert_with(|| (None, record.report.dropped_by.clone()));
            }
        }

        for index in 0..self.records.len() {
            if self.records[index].report.placement.is_some()
                || self.records[index].report.dropped_by.is_some()
            {
                continue;
            }
            if let Some((placement, dropped_by)) = tab_status.get(&(
                self.records[index].key.ws_idx,
                self.records[index].key.tab_idx,
            )) {
                self.records[index].report.placement.clone_from(placement);
                self.records[index].report.dropped_by.clone_from(dropped_by);
                continue;
            }
            let Some(entry) = self.records[index].entry.clone() else {
                self.records[index].report.dropped_by = Some("no_sidebar_entry".to_string());
                continue;
            };
            let section = if entry.parked {
                INBOX_NEW_SECTION_TITLE
            } else if entry.pinned {
                PINNED_SECTION_TITLE
            } else if entry_is_past_done_hide_threshold(app, &entry) {
                SETTLED_SECTION_TITLE
            } else {
                match sidebar_entry_lifecycle(app, &entry) {
                    SidebarEntryLifecycle::Active => "main",
                    SidebarEntryLifecycle::Snoozed => SNOOZED_SECTION_TITLE,
                    SidebarEntryLifecycle::Settled => SETTLED_SECTION_TITLE,
                }
            };
            if let Some((placement_section, group)) =
                collapsed_sidebar_placement(app, &entry, section)
            {
                self.record_placement(&entry, &placement_section, &group, true);
            } else {
                self.records[index].report.dropped_by = Some("sidebar_projection".to_string());
            }
        }
    }

    pub(super) fn append_guard_rows(&self, app: &AppState, rows: &mut Vec<SidebarRow>) {
        let hidden = self
            .records
            .iter()
            .filter(|record| {
                record.report.dropped_by.is_some() || record.report.placement.is_none()
            })
            .filter_map(|record| {
                record.entry.as_ref().map(|entry| {
                    (
                        entry.clone(),
                        record.report.dropped_by.as_deref().unwrap_or("unknown"),
                    )
                })
            })
            .collect::<Vec<_>>();
        if hidden.is_empty() {
            return;
        }

        let main_key = devices::group_key("main", &app.agent_host_name);
        let root_index = rows.iter().position(
            |row| matches!(row, SidebarRow::NestedHeader { key, .. } if key == &main_key),
        );
        let insert_at = match root_index {
            Some(index) => index + 1,
            None => {
                let index = rows
                    .iter()
                    .position(|row| {
                        matches!(
                            row,
                            SidebarRow::SectionHeader {
                                title: WORKING_SECTION_TITLE,
                                ..
                            }
                        )
                    })
                    .unwrap_or(rows.len());
                rows.insert(
                    index,
                    SidebarRow::NestedHeader {
                        key: main_key,
                        action_key: None,
                        sort_key: None,
                        sort_mode: SidebarSortMode::Default,
                        title: devices::device_title(app, &app.agent_host_name, true),
                        count: 0,
                        activity_count: None,
                        collapsed: devices::group_is_collapsed(
                            app,
                            "main",
                            &app.agent_host_name,
                            true,
                            true,
                        ),
                        dim: false,
                        status: None,
                        spawn: false,
                    },
                );
                index + 1
            }
        };
        let coverage_key = sidebar_coverage_group_key(&app.agent_host_name);
        let collapsed = !app
            .collapsed_sidebar_groups
            .contains(&format!("expanded:{coverage_key}"));
        let count = hidden.len();
        rows.insert(
            insert_at,
            SidebarRow::NestedHeader {
                key: coverage_key,
                action_key: None,
                sort_key: None,
                sort_mode: SidebarSortMode::Default,
                title: format!("⚠ {count} sessions hidden"),
                count,
                activity_count: None,
                collapsed,
                dim: false,
                status: None,
                spawn: false,
            },
        );
        if !collapsed {
            for (offset, (mut entry, reason)) in hidden.into_iter().enumerate() {
                let label = compact_row_title(&entry, true).trim().to_string();
                let label = if label.is_empty() || label == DEFAULT_THREAD_TITLE {
                    entry
                        .pane_label
                        .as_deref()
                        .filter(|label| !label.trim().is_empty())
                        .unwrap_or("Untitled session")
                        .to_string()
                } else {
                    label
                };
                entry.terminal_title = None;
                entry.terminal_title_stripped = None;
                entry.tab_has_live_agent_title = false;
                entry.tab_has_custom_name = true;
                entry.tab_label_leads_with_agent = false;
                entry.primary_tab_label = Some(format!("{label} · {reason}"));
                rows.insert(
                    insert_at + 1 + offset,
                    SidebarRow::Tab {
                        entry: Box::new(entry),
                        depth: 1,
                    },
                );
            }
        }
    }

    pub(super) fn into_records(self) -> Vec<crate::api::schema::PaneCoverage> {
        self.records
            .into_iter()
            .map(|record| record.report)
            .collect()
    }
}

pub fn sidebar_coverage(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<crate::api::schema::PaneCoverage> {
    let mut coverage = SidebarCoverageRecorder::new(app);
    let rows = super::compact_sidebar_rows_inner(
        app,
        Some(terminal_runtimes),
        false,
        true,
        false,
        Some(&mut coverage),
    );
    coverage.finish(app, &rows);
    coverage.into_records()
}
