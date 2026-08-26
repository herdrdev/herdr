use crate::api::schema::{
    ResponseResult, Task, TaskActivity, TaskActivityKind, TaskAttachParams, TaskCreateParams,
    TaskDispatchParams, TaskListParams, TaskReportParams, TaskStatus, TaskTarget, TaskUpdateParams,
};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_task_list(&mut self, id: String, params: TaskListParams) -> String {
        let mut tasks = self.state.tasks.clone();
        tasks.sort_by_key(|task| (task.status as u8, task.priority, task.created_at));
        if let Some(status) = params.status {
            tasks.retain(|task| task.status == status);
        }
        encode_success(id, ResponseResult::TaskList { tasks })
    }

    pub(super) fn handle_task_get(&mut self, id: String, target: TaskTarget) -> String {
        let Some(task) = self
            .state
            .tasks
            .iter()
            .find(|task| task.id == target.task_id)
        else {
            return task_not_found(id, &target.task_id);
        };
        encode_success(id, ResponseResult::TaskInfo { task: task.clone() })
    }

    pub(super) fn handle_task_create(&mut self, id: String, params: TaskCreateParams) -> String {
        let title = params.title.trim();
        if title.is_empty() {
            return encode_error(id, "invalid_task_title", "task title must not be empty");
        }
        let agent_session_id = params
            .agent_session_id
            .map(|agent_session_id| agent_session_id.trim().to_string());
        if agent_session_id.as_deref() == Some("") {
            return encode_error(
                id,
                "invalid_agent_session_id",
                "agent session id must not be empty",
            );
        }
        let unique_dependencies = params
            .dependencies
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if unique_dependencies.len() != params.dependencies.len() {
            return encode_error(
                id,
                "invalid_task_dependency",
                "task dependencies must be unique",
            );
        }
        for dependency in &params.dependencies {
            if self.state.tasks.iter().all(|task| task.id != *dependency) {
                return encode_error(
                    id,
                    "task_dependency_not_found",
                    format!("task dependency {dependency} was not found"),
                );
            }
        }
        if has_dependency_cycle(&self.state.tasks, &params.dependencies) {
            return encode_error(
                id,
                "task_dependency_cycle",
                "task dependencies must be acyclic",
            );
        }

        let now = crate::task::unix_timestamp();
        let mut task = Task::new(
            next_task_id(&self.state.tasks),
            title.to_string(),
            params.description,
            params.priority,
            params.dependencies,
            params.cwd,
            now,
        );
        task.agent_session_id = agent_session_id;
        self.state.tasks.push(task);
        refresh_ready_tasks(&mut self.state.tasks, now);
        let Some(task) = self.state.tasks.last().cloned() else {
            return encode_error(id, "task_create_failed", "task was not retained");
        };
        self.state.mark_session_dirty();
        self.schedule_session_save();
        encode_success(id, ResponseResult::TaskCreated { task })
    }

    pub(super) fn handle_task_update(&mut self, id: String, params: TaskUpdateParams) -> String {
        let Some(index) = self
            .state
            .tasks
            .iter()
            .position(|task| task.id == params.task_id)
        else {
            return task_not_found(id, &params.task_id);
        };
        let now = crate::task::unix_timestamp();
        if params.status == Some(TaskStatus::Ready)
            && !dependencies_satisfied(&self.state.tasks[index], &self.state.tasks)
        {
            return encode_error(id, "task_blocked", "task dependencies are not complete");
        }
        let task = &mut self.state.tasks[index];
        if let Some(title) = params.title {
            let title = title.trim();
            if title.is_empty() {
                return encode_error(id, "invalid_task_title", "task title must not be empty");
            }
            task.title = title.to_string();
        }
        if let Some(description) = params.description {
            task.description = description;
        }
        if let Some(priority) = params.priority {
            task.priority = priority;
        }
        let message = params.message.filter(|message| !message.is_empty());
        if let Some(message) = &message {
            task.last_message = Some(message.clone());
        }
        if let Some(status) = params.status {
            if let Err(error) = task.transition(status, now) {
                return encode_error(
                    id,
                    "invalid_task_transition",
                    format!(
                        "cannot transition task from {:?} to {:?}",
                        error.from, error.to
                    ),
                );
            }
        } else {
            task.updated_at = now;
        }
        if let Some(message) = message {
            task.record_activity(TaskActivity {
                timestamp: now,
                kind: TaskActivityKind::Progress,
                status: Some(task.status),
                message: Some(message),
                agent: task.agent.clone(),
                pane_id: task.pane_id.clone(),
            });
        }
        let task = task.clone();
        refresh_ready_tasks(&mut self.state.tasks, now);
        self.state.mark_session_dirty();
        self.schedule_session_save();
        encode_success(id, ResponseResult::TaskUpdated { task })
    }

    pub(super) fn handle_task_attach(&mut self, id: String, params: TaskAttachParams) -> String {
        match self.attach_task(&params.task_id, &params.target) {
            Ok(task) => encode_success(id, ResponseResult::TaskAttached { task }),
            Err((code, message)) => encode_error(id, code, message),
        }
    }

    pub(super) fn handle_task_dispatch(
        &mut self,
        id: String,
        params: TaskDispatchParams,
    ) -> String {
        let task = match self.attach_task(&params.task_id, &params.target) {
            Ok(task) => task,
            Err((code, message)) => return encode_error(id, code, message),
        };
        let prompt = params
            .prompt
            .unwrap_or_else(|| default_dispatch_prompt(&task));
        let response = self.handle_agent_prompt(
            "task:dispatch:prompt".into(),
            crate::api::schema::AgentPromptParams {
                target: params.target,
                text: prompt,
                wait: None,
            },
        );
        let parsed = serde_json::from_str::<serde_json::Value>(&response).ok();
        if parsed
            .as_ref()
            .is_some_and(|value| value.get("error").is_some())
        {
            return response;
        }
        let now = crate::task::unix_timestamp();
        let Some(task) = self
            .state
            .tasks
            .iter_mut()
            .find(|candidate| candidate.id == task.id)
        else {
            return encode_error(id, "task_not_found", "dispatched task was not retained");
        };
        task.record_activity(TaskActivity {
            timestamp: now,
            kind: TaskActivityKind::Dispatched,
            status: Some(task.status),
            message: Some("task prompt submitted to the attached agent".into()),
            agent: task.agent.clone(),
            pane_id: task.pane_id.clone(),
        });
        task.updated_at = now;
        let task = task.clone();
        self.state.mark_session_dirty();
        self.schedule_session_save();
        encode_success(id, ResponseResult::TaskDispatched { task })
    }

    pub(super) fn handle_task_report(&mut self, id: String, params: TaskReportParams) -> String {
        self.handle_task_update(
            id,
            TaskUpdateParams {
                task_id: params.task_id,
                title: None,
                description: None,
                priority: None,
                status: Some(params.status),
                message: params.message,
            },
        )
    }

    fn attach_task(&mut self, task_id: &str, target: &str) -> Result<Task, (&'static str, String)> {
        let agent = self.agent_info_for_target(target).map_err(|_| {
            (
                "agent_not_found",
                format!("agent target {target} was not found"),
            )
        })?;
        let index = self
            .state
            .tasks
            .iter()
            .position(|task| task.id == task_id)
            .ok_or_else(|| ("task_not_found", format!("task {task_id} was not found")))?;
        let now = crate::task::unix_timestamp();
        let dependencies_ready =
            dependencies_satisfied(&self.state.tasks[index], &self.state.tasks);
        if !dependencies_ready {
            return Err((
                "task_blocked",
                "task dependencies are not complete".to_string(),
            ));
        }
        let task = &mut self.state.tasks[index];
        if task.status == TaskStatus::Backlog {
            task.transition(TaskStatus::Ready, now).map_err(|_| {
                (
                    "invalid_task_transition",
                    "task cannot be made ready".to_string(),
                )
            })?;
        }
        if task.status != TaskStatus::Running {
            task.transition(TaskStatus::Running, now).map_err(|error| {
                (
                    "invalid_task_transition",
                    format!("cannot dispatch task from {:?} to running", error.from),
                )
            })?;
        }
        task.workspace_id = Some(agent.workspace_id);
        task.pane_id = Some(agent.pane_id);
        task.agent = agent.name.or(agent.agent);
        if task.cwd.is_none() {
            task.cwd = agent.cwd;
        }
        task.attempts = task.attempts.saturating_add(1);
        task.updated_at = now;
        task.record_activity(TaskActivity {
            timestamp: now,
            kind: TaskActivityKind::Attached,
            status: Some(task.status),
            message: Some("task attached to a live agent".into()),
            agent: task.agent.clone(),
            pane_id: task.pane_id.clone(),
        });
        let task = task.clone();
        self.state.mark_session_dirty();
        self.schedule_session_save();
        Ok(task)
    }
}

fn task_not_found(id: String, task_id: &str) -> String {
    encode_error(
        id,
        "task_not_found",
        format!("task {task_id} was not found"),
    )
}

fn next_task_id(tasks: &[Task]) -> String {
    let next = tasks
        .iter()
        .filter_map(|task| task.id.strip_prefix("task-"))
        .filter_map(|number| number.parse::<u128>().ok())
        .max()
        .unwrap_or_default()
        .saturating_add(1);
    format!("task-{next}")
}

fn default_dispatch_prompt(task: &Task) -> String {
    let description = if task.description.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", task.description)
    };
    format!(
        "Herdr task {}: {}{}\n\nRecord meaningful visible progress with:\n  herdr task report {} running --message \"<milestone summary>\"\nWhen implementation and verification are complete, report review. If blocked or failed, report that state with a concise reason. Do not expose private chain-of-thought; report only useful work summaries.",
        task.id, task.title, description, task.id
    )
}

fn dependencies_satisfied(task: &Task, tasks: &[Task]) -> bool {
    task.dependencies.iter().all(|dependency| {
        tasks
            .iter()
            .find(|candidate| candidate.id == *dependency)
            .is_some_and(|candidate| candidate.status == TaskStatus::Done)
    })
}

fn refresh_ready_tasks(tasks: &mut [Task], now: u64) {
    let completed = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Done)
        .map(|task| task.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for task in tasks.iter_mut() {
        if task.status == TaskStatus::Backlog
            && task
                .dependencies
                .iter()
                .all(|dependency| completed.contains(dependency))
        {
            let _ = task.transition(TaskStatus::Ready, now);
        }
    }
}

fn has_dependency_cycle(tasks: &[Task], dependencies: &[String]) -> bool {
    fn visit(
        id: &str,
        graph: &std::collections::HashMap<&str, &[String]>,
        visiting: &mut std::collections::HashSet<String>,
        visited: &mut std::collections::HashSet<String>,
    ) -> bool {
        if !visiting.insert(id.to_string()) {
            return true;
        }
        if visited.contains(id) {
            visiting.remove(id);
            return false;
        }
        if let Some(dependencies) = graph.get(id) {
            for dependency in *dependencies {
                if visit(dependency, graph, visiting, visited) {
                    return true;
                }
            }
        }
        visiting.remove(id);
        visited.insert(id.to_string());
        false
    }

    let mut graph = std::collections::HashMap::new();
    for task in tasks {
        graph.insert(task.id.as_str(), task.dependencies.as_slice());
    }
    let new_id = "__new_task__";
    graph.insert(new_id, dependencies);
    visit(
        new_id,
        &graph,
        &mut std::collections::HashSet::new(),
        &mut std::collections::HashSet::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{SuccessResponse, TaskCreateParams};
    use crate::config::Config;

    fn app() -> App {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            true,
            None,
            rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn creates_and_lists_ready_task() {
        let mut app = app();
        let response = app.handle_task_create(
            "create".into(),
            TaskCreateParams {
                title: "write tests".into(),
                description: "cover the control plane".into(),
                priority: 10,
                dependencies: Vec::new(),
                cwd: Some("/tmp/project".into()),
                agent_session_id: Some("  01a03dda-cfca-7c62-87a0-17bf3d49a96c  ".into()),
            },
        );
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(parsed.result, ResponseResult::TaskCreated { .. }));

        let response = app.handle_task_list("list".into(), TaskListParams { status: None });
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TaskList { tasks } = parsed.result else {
            panic!("expected task list");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Ready);
        assert_eq!(
            tasks[0].agent_session_id.as_deref(),
            Some("01a03dda-cfca-7c62-87a0-17bf3d49a96c")
        );
        assert_eq!(tasks[0].activities.len(), 1);
        assert_eq!(tasks[0].activities[0].kind, TaskActivityKind::Created);
    }

    #[test]
    fn create_rejects_blank_agent_session_id() {
        let mut app = app();
        let response = app.handle_task_create(
            "create".into(),
            TaskCreateParams {
                title: "write tests".into(),
                description: String::new(),
                priority: 100,
                dependencies: Vec::new(),
                cwd: None,
                agent_session_id: Some("   ".into()),
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(parsed["error"]["code"], "invalid_agent_session_id");
        assert!(app.state.tasks.is_empty());
    }

    #[test]
    fn task_ids_increment_from_highest_persisted_number() {
        let tasks = vec![
            Task::new(
                "task-1".into(),
                "first".into(),
                String::new(),
                100,
                Vec::new(),
                None,
                1,
            ),
            Task::new(
                "task-3".into(),
                "third".into(),
                String::new(),
                100,
                Vec::new(),
                None,
                1,
            ),
            Task::new(
                "imported-task".into(),
                "custom".into(),
                String::new(),
                100,
                Vec::new(),
                None,
                1,
            ),
        ];

        assert_eq!(next_task_id(&tasks), "task-4");
    }

    #[test]
    fn report_done_unblocks_dependents() {
        let mut app = app();
        app.state.tasks = vec![
            Task {
                id: "task-1".into(),
                title: "first".into(),
                description: String::new(),
                status: TaskStatus::Review,
                priority: 100,
                dependencies: Vec::new(),
                cwd: None,
                workspace_id: None,
                pane_id: None,
                agent: None,
                agent_session_id: None,
                attempts: 1,
                created_at: 1,
                updated_at: 1,
                last_message: None,
                activities: Vec::new(),
            },
            Task {
                id: "task-2".into(),
                title: "second".into(),
                description: String::new(),
                status: TaskStatus::Backlog,
                priority: 100,
                dependencies: vec!["task-1".into()],
                cwd: None,
                workspace_id: None,
                pane_id: None,
                agent: None,
                agent_session_id: None,
                attempts: 0,
                created_at: 1,
                updated_at: 1,
                last_message: None,
                activities: Vec::new(),
            },
        ];
        let response = app.handle_task_report(
            "report".into(),
            TaskReportParams {
                task_id: "task-1".into(),
                status: TaskStatus::Done,
                message: Some("verified".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(app.state.tasks[1].status, TaskStatus::Ready);
        assert!(app.state.tasks[0].activities.iter().any(|activity| {
            activity.kind == TaskActivityKind::Progress
                && activity.message.as_deref() == Some("verified")
        }));
        assert!(app.state.tasks[1]
            .activities
            .iter()
            .any(|activity| activity.status == Some(TaskStatus::Ready)));
    }

    #[test]
    fn repeated_running_report_appends_visible_progress() {
        let mut app = app();
        let mut task = Task::new(
            "task-1".into(),
            "implement activity".into(),
            String::new(),
            100,
            Vec::new(),
            None,
            1,
        );
        task.transition(TaskStatus::Running, 2).unwrap();
        app.state.tasks.push(task);

        let response = app.handle_task_report(
            "report".into(),
            TaskReportParams {
                task_id: "task-1".into(),
                status: TaskStatus::Running,
                message: Some("tests are compiling".into()),
            },
        );

        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let activity = app.state.tasks[0].activities.last().unwrap();
        assert_eq!(activity.kind, TaskActivityKind::Progress);
        assert_eq!(activity.message.as_deref(), Some("tests are compiling"));
    }

    #[test]
    fn default_dispatch_prompt_requests_safe_progress_reports() {
        let task = Task::new(
            "task-7".into(),
            "implement scheduler".into(),
            "run focused tests".into(),
            100,
            Vec::new(),
            None,
            1,
        );

        let prompt = default_dispatch_prompt(&task);

        assert!(prompt.contains("herdr task report task-7 running"));
        assert!(prompt.contains("Do not expose private chain-of-thought"));
    }
}
