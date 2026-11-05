//! First executable workflow slice: schedule persisted subtasks, run coders through Shepherd,
//! receive MCP results, and checkpoint Go-compatible task status for review-stage continuation.

pub mod pr;
pub mod runner;
pub mod verification;

pub use pr::{
    approve_plan, approve_subtask, approve_subtask_with_registry, approve_subtask_with_runner,
    reject_plan, reject_subtask,
};
pub use runner::{run_coders, run_coders_with_terminals};

use crate::{load_agent_registry, save_task_status};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct RunRequest {
    pub root: PathBuf,
    pub task: String,
    pub mcp_url: String,
}

#[derive(Default)]
pub struct RunReport {
    pub started: Vec<String>,
    pub completed_coding: Vec<u32>,
    pub waiting_for_human: Vec<u32>,
    pub failed: Vec<u32>,
    pub plan_waiting: bool,
}

pub(crate) struct ReviewOutcome {
    pub(crate) reviewer: Option<String>,
    pub(crate) state: String,
    pub(crate) note: String,
}

pub(crate) fn has_content(plan: &str) -> bool {
    let trimmed = plan.trim();
    !trimmed.is_empty() && trimmed != "# Plan"
}

pub(crate) fn validate_mcp_url(url: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(authority) = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/mcp"))
    else {
        return Err("MCP URL must be a local HTTP endpoint ending in /mcp (for example http://127.0.0.1:4317/mcp)".into());
    };
    let host = authority.split(':').next().unwrap_or("");
    if !matches!(host, "127.0.0.1" | "localhost")
        || authority
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .is_none()
    {
        return Err("MCP endpoint must use localhost or 127.0.0.1 and include a valid port".into());
    }
    Ok(())
}

pub(crate) fn wait_for_result(
    path: &std::path::Path,
    id: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let registry = load_agent_registry(path).map_err(|error| error.to_string())?;
        let agent = registry
            .result(id)
            .ok_or_else(|| format!("agent {id} disappeared from agents.json"))?;
        if agent.has_result {
            return Ok(agent.result.clone());
        }
        if agent.terminal() {
            return Err(if agent.error.is_empty() {
                format!("agent ended in state {} without a result", agent.state)
            } else {
                agent.error.clone()
            });
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {} seconds", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(crate) fn checkpoint(
    status: &mut crate::TaskStatus,
    directory: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if status.waiting == "plan" {
        status.state = "planning".into();
    } else {
        status.state = crate::aggregate_task_state(&status.subtasks, &status.state);
    }
    status.updated = super::now_rfc3339();
    save_task_status(directory.join("status.json"), status)
}

pub(crate) fn parse_timeout(value: &str) -> Option<Duration> {
    let (number, unit) = value.split_at(value.find(|ch: char| !ch.is_ascii_digit())?);
    let number = number.parse::<u64>().ok()?;
    match unit {
        "s" => Some(Duration::from_secs(number)),
        "m" => Some(Duration::from_secs(number * 60)),
        "h" => Some(Duration::from_secs(number * 3600)),
        _ => None,
    }
}

pub(crate) fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::pr::pr_body;
    use super::verification::{
        mcp_role_assignment_from, reviewer_assignment, revise_coder, verify_coder,
    };
    use super::*;
    use crate::terminals::Terminals;
    use std::process::Command;

    #[test]
    fn requires_loopback_mcp_endpoint_and_parses_timeouts() {
        assert!(validate_mcp_url("http://127.0.0.1:4317/mcp").is_ok());
        assert!(validate_mcp_url("https://example.test/mcp").is_err());
        assert!(validate_mcp_url("http://0.0.0.0:4317/mcp").is_err());
        assert_eq!(parse_timeout("30m"), Some(Duration::from_secs(1800)));
        assert_eq!(parse_timeout("bad"), None);
    }

    #[test]
    fn routes_reviewers_to_a_different_mcp_vendor() {
        let config = crate::config::Config::default();
        let active = vec!["claude".into(), "codex".into(), "gemini".into()];
        assert_eq!(
            reviewer_assignment(&config, &active, "codex", "gpt-5").unwrap(),
            ("claude".into(), "opus".into())
        );
    }

    #[test]
    fn routes_brain_off_a_vendor_without_mcp_and_approves_persisted_plan() {
        let mut config = crate::config::Config::default();
        config.roles.get_mut("brain").unwrap().vendor = "gemini".into();
        let active = vec!["claude".into(), "gemini".into()];
        assert_eq!(
            mcp_role_assignment_from(&config, "brain", &active).unwrap(),
            ("claude".into(), "opus".into())
        );

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "shepherd-plan-approval-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let task = crate::create_task(
            &root,
            crate::NewTask {
                id: 1,
                title: "Plan approval".into(),
                description: String::new(),
                criteria: Vec::new(),
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();
        crate::write_task_section(
            &task,
            "plan",
            "# Plan\n\n## Subtask 1: Do work\nVerify: true\n",
        )
        .unwrap();
        let mut status = task.status.clone();
        status.waiting = "plan".into();
        status.state = "planning".into();
        crate::save_task_status(task.directory.join("status.json"), &status).unwrap();
        approve_plan(root.clone(), "1").unwrap();
        let approved = crate::find_task(&root, "1").unwrap();
        assert!(approved.status.waiting.is_empty());
        assert_eq!(approved.status.state, "coding");
        assert_eq!(approved.status.subtasks.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verifies_clean_commits_and_configured_checks_in_a_worktree() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("shepherd-verify-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-b", "main"]).status.success());
        assert!(git(&["config", "user.email", "test@example.test"])
            .status
            .success());
        assert!(git(&["config", "user.name", "Shepherd Test"])
            .status
            .success());
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(git(&["add", "base.txt"]).status.success());
        assert!(git(&["commit", "-m", "base"]).status.success());
        assert!(git(&["checkout", "-b", "feature"]).status.success());
        std::fs::write(root.join("change.txt"), "change\n").unwrap();
        assert!(git(&["add", "change.txt"]).status.success());
        assert!(git(&["commit", "-m", "change"]).status.success());
        let coder = crate::Agent {
            root: root.display().to_string(),
            worktree: root.display().to_string(),
            branch: "feature".into(),
            ..crate::Agent::default()
        };
        let verified = verify_coder(&coder, "main", ["true".to_owned()].into_iter());
        assert!(verified.is_none(), "{verified:?}");
        std::fs::write(root.join("uncommitted.txt"), "unfinished\n").unwrap();
        assert!(verify_coder(&coder, "main", std::iter::empty())
            .unwrap()
            .contains("uncommitted"));
        std::fs::remove_file(root.join("uncommitted.txt")).unwrap();
        assert!(
            verify_coder(&coder, "main", ["false".to_owned()].into_iter())
                .unwrap()
                .contains("Check `false` failed")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pull_request_body_links_issue_and_records_review_and_checks() {
        let task = crate::CreatedTask {
            status: crate::TaskStatus {
                id: 7,
                slug: "oauth".into(),
                title: "OAuth".into(),
                state: "pr".into(),
                issue: Some(crate::IssueRef {
                    number: 42,
                    url: "https://example.test/issues/42".into(),
                    repo: String::new(),
                }),
                branch: String::new(),
                pr: 0,
                subtasks: Vec::new(),
                waiting: String::new(),
                note: String::new(),
                agents: Vec::new(),
                created: "now".into(),
                updated: "now".into(),
            },
            directory: PathBuf::new(),
        };
        let subtask = crate::Subtask {
            number: 1,
            title: "OAuth API".into(),
            body: "Implement the endpoint".into(),
            depends_on: Vec::new(),
            verify: Vec::new(),
            state: "awaiting-approval".into(),
            note: None,
            coder: "coder-1".into(),
            reviewer: "reviewer-1".into(),
            round: 1,
            branch: "feature".into(),
            pr: String::new(),
        };
        let body = pr_body(&task, &subtask, &["cargo test".into()]);
        assert!(body.contains("Closes #42"));
        assert!(body.contains("Approved by reviewer-1"));
        assert!(body.contains("`cargo test`"));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_approval_pushes_and_opens_pr_then_persists_url() {
        use std::os::unix::process::ExitStatusExt;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("shepherd-approve-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(root.join(".shepherd")).unwrap();
        std::fs::write(
            root.join(".shepherd/config.yaml"),
            "workflow:\n  base_branch: main\n  checks: [\"true\"]\n  draft: true\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-b", "main"]).status.success());
        assert!(git(&["config", "user.email", "test@example.test"])
            .status
            .success());
        assert!(git(&["config", "user.name", "Shepherd Test"])
            .status
            .success());
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(git(&["add", "base.txt"]).status.success());
        assert!(git(&["commit", "-m", "base"]).status.success());
        assert!(git(&["checkout", "-b", "feature"]).status.success());
        std::fs::write(root.join("change.txt"), "done\n").unwrap();
        assert!(git(&["add", "change.txt"]).status.success());
        assert!(git(&["commit", "-m", "feat: change"]).status.success());

        let task = crate::create_task(
            &root,
            crate::NewTask {
                id: 9,
                title: "Publish change".into(),
                description: String::new(),
                criteria: Vec::new(),
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();
        let mut status = task.status.clone();
        status.subtasks.push(crate::Subtask {
            number: 1,
            title: "Change".into(),
            body: "Add change".into(),
            depends_on: Vec::new(),
            verify: vec!["true".into()],
            state: "awaiting-approval".into(),
            note: None,
            coder: "coder-1".into(),
            reviewer: "reviewer-1".into(),
            round: 1,
            branch: "feature".into(),
            pr: String::new(),
        });
        crate::save_task_status(task.directory.join("status.json"), &status).unwrap();
        assert!(git(&["add", ".shepherd"]).status.success());
        assert!(git(&["commit", "-m", "test: persist workflow fixture"])
            .status
            .success());
        let mut registry = crate::AgentRegistryFile::default();
        registry.create(
            crate::Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                root: root.display().to_string(),
                worktree: root.display().to_string(),
                branch: "feature".into(),
                ..crate::Agent::default()
            },
            "coder-token".into(),
            "now",
        );
        let registry_path = std::env::temp_dir().join(format!(
            "shepherd-approve-registry-{}-{nonce}.json",
            std::process::id()
        ));
        crate::save_agent_registry(&registry_path, &registry).unwrap();
        let mut calls = Vec::<Vec<String>>::new();
        let url = approve_subtask_with_runner(root.clone(), "9", 1, &registry_path, |command| {
            calls.push(
                std::iter::once(command.get_program().to_string_lossy().into_owned())
                    .chain(
                        command
                            .get_args()
                            .map(|arg| arg.to_string_lossy().into_owned()),
                    )
                    .collect(),
            );
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: b"https://example.test/pr/12\n".to_vec(),
                stderr: Vec::new(),
            })
        })
        .unwrap();
        assert_eq!(url, "https://example.test/pr/12");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            &calls[0][..5],
            ["git", "push", "--set-upstream", "origin", "feature"]
        );
        assert!(calls[1].iter().any(|argument| argument == "--draft"));
        let updated = crate::find_task(&root, "9").unwrap();
        assert_eq!(updated.status.subtasks[0].state, "pr");
        assert_eq!(updated.status.subtasks[0].pr, "https://example.test/pr/12");
        std::fs::remove_file(registry_path).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revise_coder_clears_previous_result_and_prompts_through_terminals_trait() {
        use crate::terminals::{InMemoryTerminals, OpenAgentRequest};
        use std::collections::HashMap;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let registry_path = std::env::temp_dir().join(format!(
            "shepherd-revise-registry-{}-{nonce}.json",
            std::process::id()
        ));
        let mut registry = crate::AgentRegistryFile::default();
        registry.create(
            crate::Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                ..crate::Agent::default()
            },
            "tok".into(),
            "now",
        );
        registry
            .submit_result("coder-1", "initial summary", "now")
            .unwrap();
        crate::save_agent_registry(&registry_path, &registry).unwrap();

        let fake = InMemoryTerminals::new();
        fake.open_agent(&OpenAgentRequest {
            root: PathBuf::from("/tmp"),
            role: "coder".into(),
            agent_id: "coder-1".into(),
            vendor_name: "codex".into(),
            command: "codex".into(),
            shepherd_agent: "codex".into(),
            args: Vec::new(),
            label: "coder".into(),
            env: HashMap::new(),
            worktree: None,
        })
        .unwrap();

        revise_coder(&registry_path, "coder-1", "Please fix unit tests", &fake).unwrap();
        let updated = crate::load_agent_registry(&registry_path).unwrap();
        let coder = updated.result("coder-1").unwrap();
        assert!(!coder.has_result);
        assert_eq!(coder.state, "working");
        let prompts = fake.prompts();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].0, "coder-1");
        assert!(prompts[0].1.contains("Please fix unit tests"));
        std::fs::remove_file(registry_path).unwrap();
    }
}
