use crate::api::schema::{ResponseResult, SuccessResponse, TabInfo, TabListParams, TabTarget};

pub(super) fn run_window_command(args: &[String]) -> std::io::Result<i32> {
    let Some(direction) = args.first().map(String::as_str) else {
        print_window_help();
        return Ok(2);
    };
    let forward = match (direction, args.len()) {
        ("next", 1) => true,
        ("previous", 1) => false,
        ("help" | "--help" | "-h", 1) => {
            print_window_help();
            return Ok(0);
        }
        _ => {
            print_window_help();
            return Ok(2);
        }
    };

    // `tab.list` is already server-owned, canonical workspace/vector/tab
    // order, and includes tabs without agents. Compose it with `tab.focus`
    // instead of creating a UI-specific protocol method.
    let list_response = super::send_request(&crate::api::schema::Request {
        id: "cli:window:list".into(),
        method: crate::api::schema::Method::TabList(TabListParams::default()),
    })?;
    let tabs = match tabs_from_response(list_response) {
        Ok(tabs) => tabs,
        Err(response) => return super::print_response(&response),
    };
    let Some(tab) = relative_window(&tabs, forward) else {
        eprintln!("no Herdr windows are available to focus");
        return Ok(1);
    };
    let response = super::send_request(&crate::api::schema::Request {
        id: "cli:window:focus".into(),
        method: crate::api::schema::Method::TabFocus(TabTarget {
            tab_id: tab.tab_id.clone(),
        }),
    })?;
    super::print_response(&response)
}

fn tabs_from_response(response: serde_json::Value) -> Result<Vec<TabInfo>, serde_json::Value> {
    if response.get("error").is_some() {
        return Err(response);
    }
    match serde_json::from_value::<SuccessResponse>(response.clone()) {
        Ok(SuccessResponse {
            result: ResponseResult::TabList { tabs },
            ..
        }) => Ok(tabs),
        _ => Err(response),
    }
}

fn relative_window(tabs: &[TabInfo], forward: bool) -> Option<&TabInfo> {
    let current = tabs.iter().position(|tab| tab.focused)?;
    crate::workspace::relative_window_index(tabs.len(), current, forward)
        .and_then(|index| tabs.get(index))
}

fn print_window_help() {
    eprintln!("herdr window commands:");
    eprintln!("  herdr window next      focus the next tab across all workspaces");
    eprintln!("  herdr window previous  focus the previous tab across all workspaces");
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{AgentStatus, TabInfo};

    fn tab(id: &str, focused: bool) -> TabInfo {
        TabInfo {
            tab_id: id.into(),
            workspace_id: "workspace".into(),
            number: 1,
            label: id.into(),
            prio: false,
            starred: false,
            parked: false,
            focused,
            pane_count: 1,
            agent_status: AgentStatus::Idle,
        }
    }

    #[test]
    fn window_cycle_keeps_single_active_agentless_tab_selectable() {
        assert!(super::relative_window(&[tab("one", true)], true).is_some());
        assert!(super::relative_window(&[tab("one", true)], false).is_some());
        assert_eq!(
            super::relative_window(&[tab("agentless", true)], true)
                .unwrap()
                .tab_id,
            "agentless"
        );
    }

    #[test]
    fn window_cycle_handles_empty_single_and_both_wrap_directions() {
        assert!(super::relative_window(&[], true).is_none());
        let one = vec![tab("one", true)];
        assert_eq!(super::relative_window(&one, true).unwrap().tab_id, "one");
        let tabs = vec![tab("one", true), tab("two", false), tab("three", false)];
        assert_eq!(super::relative_window(&tabs, true).unwrap().tab_id, "two");
        assert_eq!(
            super::relative_window(&tabs, false).unwrap().tab_id,
            "three"
        );
        let tabs = vec![tab("one", false), tab("two", false), tab("three", true)];
        assert_eq!(super::relative_window(&tabs, true).unwrap().tab_id, "one");
    }

    #[test]
    fn window_cycle_requires_an_active_tab() {
        assert!(super::relative_window(&[tab("agentless", false)], true).is_none());
    }
}
