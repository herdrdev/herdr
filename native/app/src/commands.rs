//! The command palette: what can be run from the keyboard, and how a typed query narrows it.
//! Anything destructive only *arms* its button in the panel, so the second click stays where it always is.

use crate::panel::{Armed, PanelMsg, Tab};
use crate::{Action, Message, Mode};
use shepherd_app_core::snapshot::Snapshot;

#[derive(Debug, Clone)]
pub struct Command {
    pub label: String,
    pub run: Vec<Message>,
}

fn cmd(label: impl Into<String>, run: Vec<Message>) -> Command {
    Command {
        label: label.into(),
        run,
    }
}

pub fn build(snapshot: &Snapshot) -> Vec<Command> {
    let panel = |tab: Tab| {
        vec![
            Message::SetMode(Mode::Shepherd),
            Message::Panel(PanelMsg::Tab(tab)),
        ]
    };
    let mut all = vec![
        cmd("Show the terminal", vec![Message::SetMode(Mode::Terminal)]),
        cmd(
            "Show the Shepherd panel",
            vec![Message::SetMode(Mode::Shepherd)],
        ),
        cmd("Runs", panel(Tab::Runs)),
        cmd("Agents", panel(Tab::Agents)),
        cmd("Tasks", panel(Tab::Tasks)),
        cmd("Patterns: see them as a graph", panel(Tab::Patterns)),
        cmd(
            "Monitor: what the agents say to each other",
            panel(Tab::Monitor),
        ),
        cmd("Skills", panel(Tab::Skills)),
        cmd("Git: worktrees and pull requests", panel(Tab::Git)),
        cmd("Events: topics and messages", panel(Tab::Events)),
        cmd("Models: vendors and roles", panel(Tab::Models)),
        cmd(
            "Run a pattern",
            vec![
                Message::SetMode(Mode::Shepherd),
                Message::Panel(PanelMsg::OpenLauncher(None)),
            ],
        ),
        cmd(
            "Clean finished runs and gone agents",
            vec![
                Message::SetMode(Mode::Shepherd),
                Message::Panel(PanelMsg::Tab(Tab::Runs)),
                Message::Panel(PanelMsg::Arm(Armed::CleanRuns)),
            ],
        ),
        cmd(
            "Clean the communication",
            vec![
                Message::SetMode(Mode::Shepherd),
                Message::Panel(PanelMsg::Tab(Tab::Monitor)),
                Message::Panel(PanelMsg::Arm(Armed::CleanBus)),
            ],
        ),
        cmd(
            "Archive done tasks",
            vec![
                Message::SetMode(Mode::Shepherd),
                Message::Panel(PanelMsg::Tab(Tab::Tasks)),
                Message::Panel(PanelMsg::Arm(Armed::ArchiveDone)),
            ],
        ),
        cmd("Reload the panels", vec![Message::Reload]),
        cmd("New space", vec![Message::Act(Action::NewSpace)]),
    ];
    if let Some(workspace) = snapshot.focused_workspace_id.clone() {
        all.push(cmd(
            "New tab",
            vec![Message::Act(Action::NewTab(workspace.clone()))],
        ));
        for tab in snapshot.tabs_of(&workspace) {
            all.push(cmd(
                format!("Tab: {}", tab.label),
                vec![
                    Message::SetMode(Mode::Terminal),
                    Message::Act(Action::FocusTab(tab.tab_id.clone())),
                ],
            ));
        }
    }
    for workspace in &snapshot.workspaces {
        all.push(cmd(
            format!("Space: {}", workspace.label),
            vec![
                Message::SetMode(Mode::Terminal),
                Message::Act(Action::FocusSpace(workspace.workspace_id.clone())),
            ],
        ));
    }
    for agent in &snapshot.agents {
        all.push(cmd(
            format!("Agent: {}", agent.display_name()),
            vec![Message::GoToPane(agent.pane_id.clone())],
        ));
    }
    all
}

/// Commands containing every word of `query` (any case, any order); all of them for an empty query.
pub fn filter<'a>(commands: &'a [Command], query: &str) -> Vec<&'a Command> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    commands
        .iter()
        .filter(|c| {
            let label = c.label.to_lowercase();
            words.iter().all(|w| label.contains(w))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot::parse(
            r#"{"focused_workspace_id":"w1","workspaces":[{"workspace_id":"w1","label":"alpha"},{"workspace_id":"w2","label":"beta"}],
                "tabs":[{"tab_id":"w1:t1","workspace_id":"w1","label":"logs"},{"tab_id":"w2:t1","workspace_id":"w2","label":"other"}],
                "agents":[{"pane_id":"w1:p2","name":"coder-1","agent_status":"idle"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn the_palette_lists_spaces_tabs_of_the_focused_space_and_agents() {
        let all = build(&snapshot());
        let labels: Vec<&str> = all.iter().map(|c| c.label.as_str()).collect();
        for expected in [
            "Space: alpha",
            "Space: beta",
            "Tab: logs",
            "Agent: coder-1",
            "Run a pattern",
            "New tab",
        ] {
            assert!(
                labels.contains(&expected),
                "{expected} missing in {labels:?}"
            );
        }
        assert!(
            !labels.contains(&"Tab: other"),
            "tabs of other spaces are not listed"
        );
    }

    #[test]
    fn a_query_needs_every_word_in_any_order_and_case() {
        let all = build(&snapshot());
        let hits = |q: &str| {
            filter(&all, q)
                .into_iter()
                .map(|c| c.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(hits("space BETA"), ["Space: beta"]);
        assert_eq!(hits("coder agent"), ["Agent: coder-1"]);
        assert!(hits("zzz").is_empty());
        assert_eq!(hits("").len(), all.len());
    }

    #[test]
    fn destructive_commands_only_arm_the_button() {
        let all = build(&snapshot());
        let clean = all
            .iter()
            .find(|c| c.label.starts_with("Clean finished"))
            .unwrap();
        assert!(clean
            .run
            .iter()
            .any(|m| matches!(m, Message::Panel(PanelMsg::Arm(Armed::CleanRuns)))));
        assert!(!clean
            .run
            .iter()
            .any(|m| matches!(m, Message::Panel(PanelMsg::Confirm))));
    }
}
