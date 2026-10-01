use std::collections::BTreeMap;

use crate::app::AppState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceGroup<T> {
    pub(crate) host: String,
    pub(crate) local: bool,
    pub(crate) reachable: bool,
    pub(crate) items: Vec<T>,
}

/// Build the section's device groups once, omitting empty hosts and keeping
/// the local device first regardless of its name.
pub(crate) fn group_items<T>(
    local_host: &str,
    items: impl IntoIterator<Item = (String, bool, bool, T)>,
) -> Vec<DeviceGroup<T>> {
    let mut groups = BTreeMap::<String, (bool, bool, Vec<T>)>::new();
    for (host, local, reachable, item) in items {
        let group = groups
            .entry(host)
            .or_insert_with(|| (local, reachable, Vec::new()));
        group.0 |= local;
        group.1 &= reachable;
        group.2.push(item);
    }
    let mut groups = groups
        .into_iter()
        .filter_map(|(host, (local, reachable, items))| {
            (!items.is_empty()).then_some(DeviceGroup {
                local: local || host == local_host,
                host,
                reachable,
                items,
            })
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .local
            .cmp(&left.local)
            .then_with(|| left.host.to_lowercase().cmp(&right.host.to_lowercase()))
            .then_with(|| left.host.cmp(&right.host))
    });
    groups
}

pub(crate) fn group_key(section: &str, host: &str) -> String {
    format!("device:{section}/{host}")
}

pub(crate) fn group_is_collapsed(
    app: &AppState,
    section: &str,
    host: &str,
    local: bool,
    reachable: bool,
) -> bool {
    let key = group_key(section, host);
    if app.collapsed_sidebar_groups.contains(&key) {
        return true;
    }
    if app
        .collapsed_sidebar_groups
        .contains(&format!("expanded:{key}"))
    {
        return false;
    }
    if reachable && app.view.focused_remote_host.as_deref() == Some(host) {
        return false;
    }
    !reachable || !local
}

pub(crate) fn host_reachable(app: &AppState, host: &str) -> bool {
    host == app.agent_host_name
        || app
            .fleet_snapshot
            .hosts
            .iter()
            .find(|snapshot| snapshot.name == host)
            .is_some_and(|snapshot| snapshot.reachable)
}

pub(crate) fn reset_offline_expansions(
    collapsed: &mut std::collections::HashSet<String>,
    section: &str,
    host: &str,
) {
    let key = group_key(section, host);
    collapsed.remove(&key);
    collapsed.remove(&format!("expanded:{key}"));
}

pub(super) fn append_remote_entry_groups(
    app: &AppState,
    rows: &mut Vec<super::SidebarRow>,
    section: &str,
    entries: Vec<super::AgentPanelEntry>,
) {
    let fallback_activity = app.remote_device_activity.is_none().then(|| {
        let activity_entries = app
            .remote_agent_panel_entries
            .iter()
            .map(super::remote_agent_as_panel_entry)
            .collect::<Vec<_>>();
        super::sidebar_remote_activity(app, &activity_entries)
    });
    let remote_activity = app
        .remote_device_activity
        .as_ref()
        .or(fallback_activity.as_ref())
        .expect("remote activity fallback is present when no snapshot cache exists");
    if let Some(groups_by_section) = app.remote_agent_device_groups.as_ref() {
        let Some(groups) = groups_by_section.get(section) else {
            return;
        };
        let remote_terms = super::sidebar_query_parts(&app.sidebar_work_filter.query).0;
        for group in groups {
            if remote_terms.is_empty() && !app.blocked_filter {
                append_device_group(
                    app,
                    rows,
                    section,
                    group,
                    group.items.len(),
                    group.items.iter().map(|entry| (**entry).clone()),
                    remote_activity,
                );
            } else {
                let is_visible = |entry: &&std::sync::Arc<super::AgentPanelEntry>| {
                    let Some(remote) = entry.remote_entry.as_ref() else {
                        return false;
                    };
                    super::remote_sidebar_entry_matches_query(remote, &remote_terms)
                        && (!app.blocked_filter || super::entry_has_red_dot(entry.as_ref()))
                };
                let entry_count = group.items.iter().filter(is_visible).count();
                append_device_group(
                    app,
                    rows,
                    section,
                    group,
                    entry_count,
                    group
                        .items
                        .iter()
                        .filter(is_visible)
                        .map(|entry| (**entry).clone()),
                    remote_activity,
                );
            }
        }
        return;
    }
    let groups = group_items(
        &app.agent_host_name,
        entries.into_iter().map(|entry| {
            let host = entry
                .remote_entry
                .as_ref()
                .map(|remote| remote.agent_ref.host.clone())
                .unwrap_or_else(|| app.agent_host_name.clone());
            let reachable = entry
                .remote_entry
                .as_ref()
                .is_none_or(|remote| remote.host_fresh);
            (host, false, reachable, entry)
        }),
    );
    for group in &groups {
        append_device_group(
            app,
            rows,
            section,
            group,
            group.items.len(),
            group.items.iter().cloned(),
            remote_activity,
        );
    }
}

fn append_device_group<T>(
    app: &AppState,
    rows: &mut Vec<super::SidebarRow>,
    section: &str,
    group: &DeviceGroup<T>,
    entry_count: usize,
    entries: impl IntoIterator<Item = super::AgentPanelEntry>,
    remote_activity: &std::collections::HashMap<(String, String), super::SidebarActivityCount>,
) {
    if entry_count == 0 {
        return;
    }
    let key = group_key(section, &group.host);
    let collapsed = group_is_collapsed(app, section, &group.host, group.local, group.reachable);
    rows.push(super::SidebarRow::NestedHeader {
        key,
        action_key: None,
        sort_key: None,
        sort_mode: crate::app::state::SidebarSortMode::Default,
        title: device_title(app, &group.host, group.local),
        count: entry_count,
        activity_count: None,
        collapsed,
        dim: !group.reachable,
        status: None,
        spawn: false,
    });
    if !collapsed {
        super::append_space_tree_rows_from_iter(
            app,
            rows,
            entries,
            false,
            None,
            super::SidebarGroupMode::Spaces,
            false,
            true,
            None,
            Some(remote_activity),
            None,
        );
    }
}

pub(super) fn device_title(app: &AppState, host: &str, local: bool) -> String {
    let icon = if app.nerd_font { "󰍹" } else { "dev" };
    if local {
        format!("{icon} {host} · this device")
    } else {
        format!("{icon} {host}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_omit_empty_hosts_and_order_local_then_remote_alphabetically() {
        let groups = group_items(
            "ub2",
            [
                ("zeta".into(), false, true, "z1"),
                ("ub2".into(), true, true, "local"),
                ("alpha".into(), false, true, "a1"),
            ],
        );
        assert_eq!(
            groups
                .iter()
                .map(|group| group.host.as_str())
                .collect::<Vec<_>>(),
            ["ub2", "alpha", "zeta"]
        );
        assert!(groups.iter().all(|group| !group.items.is_empty()));
    }

    #[test]
    fn local_only_has_no_device_header_and_remotes_start_collapsed() {
        let app = AppState::test_new();
        let local = group_items("ub2", [("ub2".into(), true, true, 1)]);
        assert_eq!(local.len(), 1);
        assert!(!group_is_collapsed(&app, "main", "ub2", true, true));
        assert!(group_is_collapsed(&app, "main", "ub1", false, true));
    }

    #[test]
    fn offline_groups_collapse_and_clear_manual_expansions() {
        let mut app = AppState::test_new();
        let key = group_key("main", "ub1");
        app.collapsed_sidebar_groups
            .insert(format!("expanded:{key}"));
        assert!(!group_is_collapsed(&app, "main", "ub1", false, false));
        reset_offline_expansions(&mut app.collapsed_sidebar_groups, "main", "ub1");
        assert!(group_is_collapsed(&app, "main", "ub1", false, false));
        assert!(!app
            .collapsed_sidebar_groups
            .contains(&format!("expanded:{key}")));
    }
}
