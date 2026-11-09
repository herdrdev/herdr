//! What the Shepherd panels show: agents, pattern runs, tasks and patterns of the project. Everything is asked
//! of the server of the session being looked at (its JSON API), because the server is the one that knows where
//! it keeps things and every session keeps its own; only a task's text is read from the folder the server names.
//! Every change goes through [`crate::actions`], because the server is the only writer.

use serde_json::{json, Value};
use shepherd_core::bus::{Message, MessageBus};
use shepherd_core::config::{self, Role, Vendor};
use shepherd_core::patterns::PatternSummary;
use shepherd_core::service::patterns_run::PatternRun;
use shepherd_core::{read_task_section, Agent, CreatedTask, TaskStatus};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RunRow {
    pub run: PatternRun,
    /// Agents of the run that are still live in the registry.
    pub live_agents: usize,
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub status: TaskStatus,
    /// What the task asks for, as the panels show it (the spec once it was written, else the issue's body).
    pub description: String,
    pub criteria: Vec<String>,
    /// The body of `issue.md` under its title: what the edit form's description box holds.
    pub issue_body: String,
    /// The whole of `spec.md`.
    pub spec: String,
}

#[derive(Debug, Clone, Default)]
pub struct Models {
    pub vendors: BTreeMap<String, Vendor>,
    pub roles: BTreeMap<String, Role>,
}

#[derive(Debug, Clone, Default)]
pub struct Data {
    pub root: PathBuf,
    pub agents: Vec<Agent>,
    pub runs: Vec<RunRow>,
    pub tasks: Vec<TaskRow>,
    pub patterns: Vec<PatternSummary>,
    pub models: Models,
}

/// The project a folder belongs to: the one with the nearest `.shepherd/config.yaml`, else the folder.
pub fn project_root_at(start: &Path) -> PathBuf {
    config::project_config_path(start)
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| start.to_path_buf())
}

fn bullet_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- ") || l.starts_with("* "))
        .filter(|l| !l.contains("(add checkable criteria; the reviewer verifies each one)"))
        .map(str::to_string)
        .collect()
}

/// The spec unless it is still the untouched template, else the issue body.
fn task_description(spec: &str, issue: &str) -> String {
    const TEMPLATE: &str =
        "Design decisions, constraints, interfaces. Agents read this before coding.";
    let spec = spec.trim();
    if !(spec.is_empty()
        || spec.starts_with("# Spec")
            && spec.contains(TEMPLATE)
            && spec.len() <= TEMPLATE.len() + 20)
    {
        return spec.to_string();
    }
    let issue = issue.trim();
    match issue.strip_prefix("# ") {
        Some(rest) => rest
            .split_once('\n')
            .map(|(_, body)| body.trim().to_string())
            .unwrap_or_default(),
        None => issue.to_string(),
    }
}

/// `# Title\n\nbody` → `body`.
fn issue_body(issue: &str) -> String {
    let issue = issue.trim();
    match issue.strip_prefix("# ") {
        Some(rest) => rest
            .split_once('\n')
            .map(|(_, body)| body.trim().to_owned())
            .unwrap_or_default(),
        None => issue.to_owned(),
    }
}

/// The criteria of a task as the edit form shows them: one per line, without the list marker.
pub fn criteria_text(task: &TaskRow) -> String {
    task.criteria
        .iter()
        .map(|c| strip_bullet(c))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the edit form's criteria box holds back into a list: a line each, markers and blanks dropped.
pub fn parse_criteria(text: &str) -> Vec<String> {
    text.lines()
        .map(strip_bullet)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .collect()
}

fn task_row(task: CreatedTask) -> TaskRow {
    let section = |name: &str| read_task_section(&task, name).unwrap_or_default();
    let (spec, issue) = (section("spec"), section("issue"));
    TaskRow {
        description: task_description(&spec, &issue),
        criteria: bullet_lines(&section("acceptance")),
        issue_body: issue_body(&issue),
        spec,
        status: task.status,
    }
}

impl Data {
    /// Loads everything for the project that `folder` belongs to, from the session behind `api`.
    /// What the server cannot be asked (it is down, say) comes back empty.
    pub fn load(api: &Path, folder: &Path) -> Self {
        let root = project_root_at(folder);
        let root_text = root.to_string_lossy().into_owned();
        let ask = |method: &str, params: Value| {
            crate::api::call(api, method, params).unwrap_or(Value::Null)
        };
        let mut agents = parse_agents(&ask("shepherd.agent.list", json!({})));
        agents.sort_by_key(|a| a.seq);
        let runs = parse_runs(
            &ask("shepherd.pattern.run.list", json!({ "root": root_text })),
            |id| {
                ask(
                    "shepherd.pattern.run.show",
                    json!({ "id": id, "root": root_text }),
                )
            },
        );
        let cfg = config::load_config(&root).unwrap_or_default();
        Self {
            runs,
            tasks: parse_tasks(&ask("shepherd.task.list", json!({ "root": root_text }))),
            patterns: parse_list(
                &ask("shepherd.pattern.list", json!({ "root": root_text })),
                "patterns",
            ),
            agents,
            models: Models {
                vendors: cfg.vendors.into_iter().collect(),
                roles: cfg.roles.into_iter().collect(),
            },
            root,
        }
    }

    pub fn task(&self, id: u32) -> Option<&TaskRow> {
        self.tasks.iter().find(|t| t.status.id == id)
    }
}

/// The items of `value[key]` that read as `T` (what does not is skipped, not an error).
fn parse_list<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Vec<T> {
    value[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| serde_json::from_value(i.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// `shepherd.agent.list`.
pub fn parse_agents(reply: &Value) -> Vec<Agent> {
    parse_list(reply, "agents")
}

/// `shepherd.pattern.run.list` (a summary per run, with how many agents are alive) plus the detail of each run
/// from `show`, newest first.
pub fn parse_runs(list: &Value, mut show: impl FnMut(&str) -> Value) -> Vec<RunRow> {
    let mut rows: Vec<RunRow> = list["runs"]
        .as_array()
        .map(|runs| {
            runs.iter()
                .filter_map(|summary| {
                    let id = summary["id"].as_str()?;
                    let run: PatternRun = serde_json::from_value(show(id)["run"].clone()).ok()?;
                    Some(RunRow {
                        live_agents: summary["live_agents"].as_u64().unwrap_or(0) as usize,
                        run,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b.run.created_at.cmp(&a.run.created_at));
    rows
}

/// `shepherd.task.list`: each task has its status and the folder the server keeps its text in.
pub fn parse_tasks(reply: &Value) -> Vec<TaskRow> {
    parse_list::<CreatedTask>(reply, "tasks")
        .into_iter()
        .map(task_row)
        .collect()
}

/// Messages sent by or to any of `agents` (topic publications and direct mail), oldest first, at most
/// `limit` (the latest ones). Topic names are shared by every run, so who sends decides what belongs to a
/// group of agents, not the topic.
pub fn bus_feed(agents: &[String], limit: usize) -> Vec<Message> {
    let Ok(bus) = MessageBus::open(config::state_dir().join("messages.jsonl")) else {
        return Vec::new();
    };
    feed_from(&bus, agents, limit)
}

fn feed_from(bus: &MessageBus, agents: &[String], limit: usize) -> Vec<Message> {
    let mine: HashSet<&str> = agents.iter().map(String::as_str).collect();
    let mut messages: Vec<Message> = bus
        .since(0)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| {
            mine.contains(m.from.as_str()) || (!m.to.is_empty() && mine.contains(m.to.as_str()))
        })
        .collect();
    messages.sort_by_key(|m| m.id);
    let skip = messages.len().saturating_sub(limit);
    messages.split_off(skip)
}

/// The task a run was launched for: its objective starts with "Task #N:" (or the legacy "Tarea #N:").
pub fn task_of_run(objective: &str) -> Option<u32> {
    let head = objective.trim_start();
    let rest = ["Tarea #", "Task #", "tarea #", "task #"]
        .iter()
        .find_map(|p| head.strip_prefix(p))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    rest[digits.len()..]
        .starts_with(':')
        .then(|| digits.parse().ok())
        .flatten()
}

/// `- [ ] text`, `* [x] text` or `- text` → `text`.
fn strip_bullet(line: &str) -> &str {
    let rest = line.trim().trim_start_matches(['-', '*']).trim_start();
    match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((" " | "x" | "X", after)) => after.trim_start(),
        _ => rest,
    }
}

/// The objective for a run launched for a task: title, description and criteria.
pub fn task_prompt(task: &TaskRow) -> String {
    let mut parts = vec![format!("Task #{}: {}", task.status.id, task.status.title)];
    if !task.description.trim().is_empty() {
        parts.push(task.description.trim().to_owned());
    }
    let criteria: Vec<String> = task
        .criteria
        .iter()
        .map(|c| strip_bullet(c).to_owned())
        .filter(|c| !c.is_empty())
        .collect();
    if !criteria.is_empty() {
        parts.push(format!(
            "Acceptance criteria:\n{}",
            criteria
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    parts.join("\n\n")
}

/// Keeps the "Task #N:" prefix that links a run to its task, adding it when the text lost it.
pub fn ensure_task_prefix(text: &str, task: &TaskRow) -> String {
    let text = text.trim();
    if task_of_run(text).is_some() {
        text.to_owned()
    } else {
        format!(
            "Task #{}: {}\n\n{}",
            task.status.id, task.status.title, text
        )
        .trim()
        .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u32, title: &str, description: &str, criteria: &[&str]) -> TaskRow {
        let task = CreatedTask {
            status: TaskStatus {
                id,
                slug: "t".into(),
                title: title.into(),
                state: "new".into(),
                issue: None,
                branch: String::new(),
                pr: 0,
                subtasks: vec![],
                waiting: String::new(),
                note: String::new(),
                agents: vec![],
                created: String::new(),
                updated: String::new(),
            },
            directory: PathBuf::new(),
        };
        TaskRow {
            status: task.status,
            description: description.into(),
            criteria: criteria.iter().map(|c| c.to_string()).collect(),
            issue_body: description.into(),
            spec: String::new(),
        }
    }

    #[test]
    fn the_task_link_survives_both_languages_and_editing() {
        assert_eq!(task_of_run("Tarea #7: legacy"), Some(7));
        assert_eq!(task_of_run("Task #12: x\n\ny"), Some(12));
        assert_eq!(task_of_run("Task #12 x"), None);
        assert_eq!(task_of_run("nothing"), None);
        let task = row(
            3,
            "Login",
            "To log in",
            &["- [ ] works", "- [x] xml is read", "* no checkbox"],
        );
        assert_eq!(
            task_prompt(&task),
            "Task #3: Login\n\nTo log in\n\nAcceptance criteria:\n- works\n- xml is read\n- no checkbox"
        );
        assert_eq!(
            ensure_task_prefix("only my objective", &task),
            "Task #3: Login\n\nonly my objective"
        );
        assert_eq!(
            ensure_task_prefix("Task #3: already there", &task),
            "Task #3: already there"
        );
    }

    #[test]
    fn the_feed_belongs_to_the_agents_that_send_or_receive() {
        let dir = std::env::temp_dir().join(format!("native-feed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bus = MessageBus::open(dir.join("messages.jsonl")).unwrap();
        bus.publish("orders", "from a", None, Some(0), "a-1")
            .unwrap();
        bus.publish("orders", "from another run", None, Some(0), "z-9")
            .unwrap();
        bus.send(Message::directed("b-1", "a-1", "msg", "for a"))
            .unwrap();
        let mine = ["a-1".to_owned()];
        let feed = feed_from(&bus, &mine, 10);
        assert_eq!(
            feed.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            ["from a", "for a"]
        );
        assert_eq!(
            feed_from(&bus, &mine, 1).len(),
            1,
            "only the latest are kept"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn runs_come_newest_first() {
        let list = json!({"runs":[{"id":"run-1","live_agents":0},{"id":"run-2","live_agents":1},{"id":"run-3","live_agents":0}]});
        let run = |id: &str, at: &str| {
            json!({"run": {"id": id, "pattern": "p", "kind": "blackboard", "task": "t",
                "state": "running", "created_at": at, "updated_at": at}})
        };
        let runs = parse_runs(&list, |id| match id {
            "run-1" => run(id, "2026-01-01T00:00:01Z"),
            "run-2" => run(id, "2026-01-01T00:00:03Z"),
            _ => run(id, "2026-01-01T00:00:02Z"),
        });
        assert_eq!(
            runs.iter().map(|r| r.run.id.as_str()).collect::<Vec<_>>(),
            ["run-2", "run-3", "run-1"]
        );
        assert_eq!(runs[0].live_agents, 1);
    }

    // Answers of a real server (a `duo` pattern run for a task), trimmed.
    const AGENTS: &str = r#"{"type": "shepherd_agent_list", "agents": [{"id": "specialist-1", "seq": 1, "role": "specialist", "vendor": "codex", "task_id": "run-duo-3760", "task_text": "Task #1: Sample task", "depth": 0, "root": "/var/tmp/shep-probe.T2zBSP/proj", "mcp": true, "skills": {}, "active_skills": ["shepherd-comms", "shepherd-control", "shepherd-events", "shepherd-orchestrate"], "workspace": "w1", "tab": "w1:t2", "pane": "w1:p2", "label": "specialist·codex·run-duo-3760", "state": "starting", "has_result": false, "delivery_cursor": 0, "started": "2026-11-09T15:19:22Z", "updated": "2026-11-09T15:19:22Z"}]}"#;
    const RUN_LIST: &str = r#"{"type": "shepherd_pattern_run_list", "runs": [{"id": "run-duo-3760", "pattern": "duo", "kind": "blackboard", "state": "running", "agents_count": 2, "live_agents": 2, "topics_count": 1, "created_at": "2026-11-09T15:19:22Z", "updated_at": "2026-11-09T15:19:22Z"}]}"#;
    const RUN_SHOW: &str = r#"{"type": "shepherd_pattern_run_detail", "run": {"id": "run-duo-3760", "pattern": "duo", "kind": "blackboard", "task": "Task #1: Sample task", "topics": ["work"], "agents": [{"id": "specialist-1", "role": "specialist", "vendor": "codex", "model": "", "state": "starting", "start": true, "publishes": ["work"], "consumes": ["work"]}, {"id": "specialist-2", "role": "specialist", "vendor": "codex", "model": "", "state": "starting", "publishes": ["work"], "consumes": ["work"]}], "state": "running", "created_at": "2026-11-09T15:19:22Z", "updated_at": "2026-11-09T15:19:22Z"}}"#;
    const TASKS: &str = r#"{"type": "shepherd_task_list", "tasks": [{"status": {"id": 1, "slug": "sample-task", "title": "Sample task", "state": "new", "created": "2026-11-09T15:19:22Z", "updated": "2026-11-09T15:19:22Z"}, "directory": "/var/tmp/shep-probe.T2zBSP/proj/.shepherd/tasks/1-sample-task"}]}"#;
    const PATTERNS: &str = r#"{"type": "shepherd_pattern_list", "patterns": [{"name": "blackboard", "kind": "blackboard", "description": "Shared knowledge base where specialist agents read hypotheses and post discoveries. Starts: researcher (posts the first hypothesis to `blackboard`). Analyst and synthesizer wait for entries on `blackboard`, then post validations and the final synthesis there.", "source": "builtin", "shadowed": false, "agents_count": 3, "topics_count": 1}, {"name": "duo", "kind": "blackboard", "description": "two harmless agents", "source": "project", "shadowed": false, "agents_count": 2, "topics_count": 1, "path": "/var/tmp/shep-probe.T2zBSP/proj/.shepherd/patterns/duo.yaml"}]}"#;

    #[test]
    fn what_the_server_answers_becomes_the_panels_data() {
        let agents = parse_agents(&serde_json::from_str(AGENTS).unwrap());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "specialist-1");
        assert_eq!(agents[0].state, "starting");

        let list: Value = serde_json::from_str(RUN_LIST).unwrap();
        let show: Value = serde_json::from_str(RUN_SHOW).unwrap();
        let mut asked = Vec::new();
        let runs = parse_runs(&list, |id| {
            asked.push(id.to_owned());
            show.clone()
        });
        assert_eq!(asked.len(), 1, "the detail of each run is asked once");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run.pattern, "duo");
        assert_eq!(runs[0].run.agents.len(), 2);
        assert_eq!(task_of_run(&runs[0].run.task), Some(1));
        assert_eq!(
            runs[0].live_agents,
            list["runs"][0]["live_agents"].as_u64().unwrap() as usize
        );

        let tasks = parse_tasks(&serde_json::from_str(TASKS).unwrap());
        assert_eq!(
            (
                tasks.len(),
                tasks[0].status.id,
                tasks[0].status.title.as_str()
            ),
            (1, 1, "Sample task")
        );

        let patterns: Vec<PatternSummary> =
            parse_list(&serde_json::from_str(PATTERNS).unwrap(), "patterns");
        assert_eq!(patterns.len(), 2);
        assert!(patterns.iter().any(|p| p.name == "duo"));
    }

    #[test]
    fn a_server_that_does_not_answer_gives_empty_panels() {
        assert!(parse_agents(&Value::Null).is_empty());
        assert!(parse_runs(&Value::Null, |_| Value::Null).is_empty());
        assert!(parse_tasks(&json!({})).is_empty());
        // a run whose detail cannot be read is left out, the others stay
        let list = json!({"runs":[{"id":"x","live_agents":1}]});
        assert!(parse_runs(&list, |_| Value::Null).is_empty());
    }

    #[test]
    fn criteria_go_to_the_form_and_back_without_losing_their_text() {
        let task = row(
            1,
            "T",
            "d",
            &["- [ ] it works", "- [x] it is fast", "* plain"],
        );
        assert_eq!(criteria_text(&task), "it works\nit is fast\nplain");
        assert_eq!(
            parse_criteria("it works\n\n  - [ ] it is fast  \n* plain\n"),
            ["it works", "it is fast", "plain"]
        );
        assert!(parse_criteria("\n \n").is_empty());
        assert_eq!(issue_body("# Title\n\nthe body\nmore\n"), "the body\nmore");
        assert_eq!(issue_body("# Only a title"), "");
        assert_eq!(issue_body("no heading"), "no heading");
    }
}
