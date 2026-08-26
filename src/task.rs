use serde::{Deserialize, Serialize};

const MAX_TASK_ACTIVITIES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskActivityKind {
    Created,
    StatusChanged,
    Progress,
    Attached,
    Dispatched,
    AgentStatus,
    AgentReleased,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskActivity {
    pub timestamp: u64,
    pub kind: TaskActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// A durable unit of work tracked by Herdr's control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activities: Vec<TaskActivity>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Backlog,
    Ready,
    Running,
    Blocked,
    Review,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (Self::Backlog, Self::Ready | Self::Cancelled)
                    | (Self::Ready, Self::Running | Self::Blocked | Self::Cancelled)
                    | (
                        Self::Running,
                        Self::Blocked | Self::Review | Self::Failed | Self::Cancelled
                    )
                    | (
                        Self::Blocked,
                        Self::Running | Self::Failed | Self::Cancelled
                    )
                    | (
                        Self::Review,
                        Self::Running | Self::Done | Self::Failed | Self::Cancelled
                    )
                    | (Self::Failed, Self::Ready | Self::Cancelled)
            )
    }
}

impl Task {
    pub fn new(
        id: String,
        title: String,
        description: String,
        priority: u8,
        dependencies: Vec<String>,
        cwd: Option<String>,
        now: u64,
    ) -> Self {
        let status = if dependencies.is_empty() {
            TaskStatus::Ready
        } else {
            TaskStatus::Backlog
        };
        let mut task = Self {
            id,
            title,
            description,
            status,
            priority,
            dependencies,
            cwd,
            workspace_id: None,
            pane_id: None,
            agent: None,
            agent_session_id: None,
            attempts: 0,
            created_at: now,
            updated_at: now,
            last_message: None,
            activities: Vec::new(),
        };
        task.record_activity(TaskActivity {
            timestamp: now,
            kind: TaskActivityKind::Created,
            status: Some(status),
            message: None,
            agent: None,
            pane_id: None,
        });
        task
    }

    pub fn transition(&mut self, next: TaskStatus, now: u64) -> Result<(), TaskTransitionError> {
        if !self.status.can_transition_to(next) {
            return Err(TaskTransitionError {
                from: self.status,
                to: next,
            });
        }
        let changed = self.status != next;
        self.status = next;
        self.updated_at = now;
        if changed {
            self.record_activity(TaskActivity {
                timestamp: now,
                kind: TaskActivityKind::StatusChanged,
                status: Some(next),
                message: None,
                agent: self.agent.clone(),
                pane_id: self.pane_id.clone(),
            });
        }
        Ok(())
    }

    pub fn record_activity(&mut self, activity: TaskActivity) {
        let overflow = self
            .activities
            .len()
            .saturating_add(1)
            .saturating_sub(MAX_TASK_ACTIVITIES);
        if overflow > 0 {
            self.activities.drain(..overflow);
        }
        self.activities.push(activity);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskTransitionError {
    pub from: TaskStatus,
    pub to: TaskStatus,
}

fn default_priority() -> u8 {
    100
}

pub(crate) fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tasks_with_dependencies_wait_in_backlog() {
        let task = Task::new(
            "task-1".into(),
            "blocked until build".into(),
            String::new(),
            100,
            vec!["task-0".into()],
            None,
            1,
        );
        assert_eq!(task.status, TaskStatus::Backlog);
    }

    #[test]
    fn lifecycle_rejects_skipping_review() {
        assert!(!TaskStatus::Running.can_transition_to(TaskStatus::Done));
        assert!(TaskStatus::Review.can_transition_to(TaskStatus::Done));
        assert!(TaskStatus::Failed.can_transition_to(TaskStatus::Ready));
    }

    #[test]
    fn records_creation_and_status_activity() {
        let mut task = Task::new(
            "task-1".into(),
            "activity".into(),
            String::new(),
            100,
            Vec::new(),
            None,
            10,
        );

        task.transition(TaskStatus::Running, 20).unwrap();

        assert_eq!(task.activities.len(), 2);
        assert_eq!(task.activities[0].kind, TaskActivityKind::Created);
        assert_eq!(task.activities[1].status, Some(TaskStatus::Running));
    }

    #[test]
    fn activity_history_keeps_only_the_newest_records() {
        let mut task = Task::new(
            "task-1".into(),
            "bounded activity".into(),
            String::new(),
            100,
            Vec::new(),
            None,
            1,
        );
        for timestamp in 0..300 {
            task.record_activity(TaskActivity {
                timestamp,
                kind: TaskActivityKind::Progress,
                status: Some(TaskStatus::Ready),
                message: None,
                agent: None,
                pane_id: None,
            });
        }

        assert_eq!(task.activities.len(), MAX_TASK_ACTIVITIES);
        assert_eq!(task.activities[0].timestamp, 44);
        assert_eq!(task.activities.last().unwrap().timestamp, 299);
    }

    #[test]
    fn older_task_json_defaults_new_optional_fields() {
        let mut task = Task::new(
            "task-1".into(),
            "legacy task".into(),
            String::new(),
            100,
            Vec::new(),
            None,
            1,
        );
        task.agent_session_id = Some("session-newer-than-snapshot".into());
        let mut value = serde_json::to_value(task).unwrap();
        let object = value
            .as_object_mut()
            .expect("task should serialize as an object");
        object.remove("activities");
        object.remove("agent_session_id");

        let restored: Task = serde_json::from_value(value).unwrap();

        assert!(restored.activities.is_empty());
        assert_eq!(restored.agent_session_id, None);
    }
}
