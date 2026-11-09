//! The shell snapshot (`shell.snapshot.v1`): spaces, tabs, panes and agents as the server sees them.
//! Lenient on purpose: every field has a default and unknown ones are ignored, so a newer server
//! (which may only add optional fields) never breaks the sidebar.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub boot_id: String,
    pub revision: u64,
    pub focused_workspace_id: Option<String>,
    pub focused_tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<Workspace>,
    pub tabs: Vec<Tab>,
    pub panes: Vec<Pane>,
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub workspace_id: String,
    pub active_tab_id: String,
    pub number: usize,
    pub label: String,
    pub branch: Option<String>,
    pub worktree: Option<Worktree>,
    pub focused: bool,
    agent_status: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Worktree {
    pub key: String,
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Tab {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub zoomed: bool,
    agent_status: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Pane {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub focused: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Agent {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub name: Option<String>,
    pub display_agent: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub focused: bool,
    agent_status: Value,
}

/// What an agent (or a space or tab, as the busiest of its agents) is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Working,
    Blocked,
    Done,
    Idle,
    Unknown,
}

impl Status {
    fn from_value(value: &Value) -> Self {
        match value.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("working") => Status::Working,
            Some("blocked") => Status::Blocked,
            Some("done") => Status::Done,
            Some("idle") => Status::Idle,
            _ => Status::Unknown,
        }
    }
}

impl Workspace {
    pub fn status(&self) -> Status {
        Status::from_value(&self.agent_status)
    }
}
impl Tab {
    pub fn status(&self) -> Status {
        Status::from_value(&self.agent_status)
    }
}
impl Agent {
    pub fn status(&self) -> Status {
        Status::from_value(&self.agent_status)
    }

    /// The name to show: the agent's own name, else its kind, else the pane.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.display_agent.clone())
            .or_else(|| self.agent.clone())
            .unwrap_or_else(|| self.pane_id.clone())
    }
}

impl Snapshot {
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    pub fn tabs_of(&self, workspace_id: &str) -> impl Iterator<Item = &Tab> {
        let id = workspace_id.to_owned();
        self.tabs.iter().filter(move |t| t.workspace_id == id)
    }

    /// Agents grouped like Herdr's sidebar: working first, then blocked, done, idle.
    pub fn agents_by_state(&self) -> Vec<(Status, Vec<&Agent>)> {
        [
            Status::Working,
            Status::Blocked,
            Status::Done,
            Status::Idle,
            Status::Unknown,
        ]
        .into_iter()
        .map(|status| {
            (
                status,
                self.agents
                    .iter()
                    .filter(|a| a.status() == status)
                    .collect::<Vec<_>>(),
            )
        })
        .filter(|(_, agents)| !agents.is_empty())
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "boot_id": "b1", "revision": 7, "future_field": {"x": 1},
        "focused_workspace_id": "w1", "focused_tab_id": "w1:t1", "focused_pane_id": "w1:p1",
        "workspaces": [{"workspace_id": "w1", "active_tab_id": "w1:t1", "number": 1, "label": "proj", "branch": "main", "focused": true, "agent_status": "working", "worktree": null}],
        "tabs": [{"tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "1", "focused": true, "zoomed": false, "agent_status": "idle"},
                 {"tab_id": "w2:t1", "workspace_id": "w2", "number": 1, "label": "1", "focused": false, "zoomed": false, "agent_status": "done"}],
        "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1", "label": null, "cwd": "/tmp", "focused": true, "right_click_passthrough": false}],
        "agents": [
          {"pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1", "name": "coder-1", "agent": "claude", "agent_status": "working", "focused": true, "tokens": []},
          {"pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t2", "name": null, "display_agent": "codex", "agent_status": "idle", "focused": false},
          {"pane_id": "w1:p3", "workspace_id": "w1", "tab_id": "w1:t3", "agent_status": {"odd": true}}
        ]
    }"#;

    #[test]
    fn parses_leniently_and_groups_agents_like_the_sidebar() {
        let snapshot = Snapshot::parse(SAMPLE).unwrap();
        assert_eq!(snapshot.revision, 7);
        assert_eq!(snapshot.workspaces[0].label, "proj");
        assert_eq!(snapshot.workspaces[0].status(), Status::Working);
        assert_eq!(snapshot.tabs_of("w1").count(), 1);
        let groups = snapshot.agents_by_state();
        assert_eq!(
            groups
                .iter()
                .map(|(s, a)| (*s, a.len()))
                .collect::<Vec<_>>(),
            vec![
                (Status::Working, 1),
                (Status::Idle, 1),
                (Status::Unknown, 1)
            ]
        );
        assert_eq!(snapshot.agents[0].display_name(), "coder-1");
        assert_eq!(snapshot.agents[1].display_name(), "codex");
        assert_eq!(snapshot.agents[2].display_name(), "w1:p3");
    }

    #[test]
    fn an_empty_object_is_a_valid_snapshot() {
        assert!(Snapshot::parse("{}").unwrap().workspaces.is_empty());
    }
}
