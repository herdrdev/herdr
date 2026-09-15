use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{App, PULL_REQUEST_REFRESH_INTERVAL};
use crate::events::AppEvent;
use crate::workspace::{PullRequestInfo, PullRequestState};

#[derive(Debug, Clone)]
pub(crate) struct WorkspacePullRequest {
    pub workspace_id: String,
    pub cwd: PathBuf,
    pub branch: String,
    pub pull_request: Result<PullRequestLookup, ()>,
}

#[derive(Debug, Clone)]
pub(crate) struct PullRequestLookup {
    pub pull_request: Option<PullRequestInfo>,
    pub repository: Option<String>,
}

#[derive(Deserialize)]
struct GhPullRequest {
    number: u64,
    state: String,
    #[serde(rename = "draft")]
    is_draft: bool,
    #[serde(default)]
    merged_at: Option<String>,
    head: GhPullRequestHead,
}

#[derive(Deserialize)]
struct GhPullRequestHead {
    repo: Option<GhRepositoryName>,
}

#[derive(Deserialize)]
struct GhRepositoryName {
    #[serde(rename = "full_name")]
    name_with_owner: String,
}

#[derive(Deserialize)]
struct GhRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
    url: String,
    parent: Option<GhRepositoryParent>,
}

#[derive(Deserialize)]
struct GhRepositoryParent {
    name: String,
    owner: GhRepositoryOwner,
}

#[derive(Deserialize)]
struct GhRepositoryOwner {
    login: String,
}

impl App {
    pub(crate) fn pull_request_refresh_deadline(&self) -> Option<Instant> {
        (!self.pull_request_refresh_in_flight
            && self
                .state
                .workspaces
                .iter()
                .any(|workspace| workspace.cached_git_branch.is_some()))
        .then_some(self.last_pull_request_refresh + PULL_REQUEST_REFRESH_INTERVAL)
    }

    pub(crate) fn start_pull_request_refresh_if_due(&mut self, now: Instant) {
        let Some(deadline) = self.pull_request_refresh_deadline() else {
            return;
        };
        if now < deadline {
            return;
        }
        let targets = self
            .state
            .workspaces
            .iter()
            .filter_map(|workspace| {
                Some((
                    workspace.id.clone(),
                    workspace.cached_identity_cwd.clone(),
                    workspace.cached_git_branch.clone()?,
                    workspace.cached_pull_request_repository.clone(),
                ))
            })
            .collect::<Vec<_>>();
        self.pull_request_refresh_in_flight = true;
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            let target_count = targets.len();
            let concurrency = Arc::new(tokio::sync::Semaphore::new(4));
            let mut tasks = tokio::task::JoinSet::new();
            for (workspace_id, cwd, branch, cached_repository) in targets {
                let concurrency = Arc::clone(&concurrency);
                tasks.spawn(async move {
                    let _permit = concurrency.acquire_owned().await.ok()?;
                    Some(WorkspacePullRequest {
                        workspace_id,
                        pull_request: query_pull_request(
                            &cwd,
                            &branch,
                            cached_repository.as_deref(),
                        )
                        .await,
                        cwd,
                        branch,
                    })
                });
            }
            let mut results = Vec::with_capacity(target_count);
            while let Some(result) = tasks.join_next().await {
                if let Ok(Some(result)) = result {
                    results.push(result);
                }
            }
            let _ = event_tx
                .send(AppEvent::PullRequestsRefreshed(results))
                .await;
        });
    }

    pub(crate) fn handle_pull_requests_refreshed(
        &mut self,
        results: Vec<WorkspacePullRequest>,
    ) -> bool {
        self.pull_request_refresh_in_flight = false;
        let now = Instant::now();
        if self.pull_request_refresh_due_after_in_flight {
            self.last_pull_request_refresh = now
                .checked_sub(PULL_REQUEST_REFRESH_INTERVAL)
                .unwrap_or(now);
            self.pull_request_refresh_due_after_in_flight = false;
        } else {
            self.last_pull_request_refresh = now;
        }
        let mut changed = false;
        for result in results {
            let Some(workspace) = self.state.workspaces.iter_mut().find(|workspace| {
                workspace.id == result.workspace_id
                    && workspace.cached_identity_cwd == result.cwd
                    && workspace.cached_git_branch.as_deref() == Some(result.branch.as_str())
            }) else {
                continue;
            };
            let Ok(lookup) = result.pull_request else {
                continue;
            };
            workspace.cached_pull_request_repository = lookup.repository;
            if workspace.cached_pull_request != lookup.pull_request {
                workspace.cached_pull_request = lookup.pull_request;
                changed = true;
            }
        }
        changed
    }
}

async fn query_pull_request(
    cwd: &std::path::Path,
    branch: &str,
    cached_repository: Option<&str>,
) -> Result<PullRequestLookup, ()> {
    let target = resolve_published_target(cwd, branch).await?;
    let (_, published_branch, repository_locator) = &target;
    let repository: GhRepository = serde_json::from_slice(
        &gh_output(
            cwd,
            &[
                "repo",
                "view",
                repository_locator,
                "--json",
                "nameWithOwner,parent,url",
            ],
        )
        .await?,
    )
    .map_err(|_| ())?;
    let host = github_host(&repository.url).ok_or(())?;
    let head_owner = repository.name_with_owner.split_once('/').ok_or(())?.0;
    let remote_config = command_output(
        cwd,
        "git",
        &["config", "--get-regexp", r"^remote\..*\.(pushurl|url)$"],
    )
    .await
    .ok()
    .and_then(|output| String::from_utf8(output).ok());
    let repositories =
        pull_request_repositories(&repository, remote_config.as_deref(), cached_repository);
    let head = format!("{head_owner}:{published_branch}");
    let mut found = None;
    'states: for state in ["open", "closed"] {
        let mut failed = false;
        for (base_repository, required) in &repositories {
            let endpoint = format!("repos/{base_repository}/pulls");
            let output = match gh_output(
                cwd,
                &[
                    "api",
                    "--method",
                    "GET",
                    "--hostname",
                    host,
                    &endpoint,
                    "-f",
                    &format!("state={state}"),
                    "-f",
                    &format!("head={head}"),
                    "-f",
                    "per_page=100",
                    "--paginate",
                    "--slurp",
                ],
            )
            .await
            {
                Ok(output) => output,
                Err(()) => {
                    failed |= *required;
                    continue;
                }
            };
            if let Some(pull_request) = pull_request_for_repository(
                serde_json::from_slice::<Vec<Vec<GhPullRequest>>>(&output)
                    .map_err(|_| ())?
                    .into_iter()
                    .flatten(),
                &repository.name_with_owner,
            ) {
                found = Some((pull_request, base_repository.clone()));
                break 'states;
            }
        }
        if failed {
            return Err(());
        }
    }
    let current_remote_config = command_output(
        cwd,
        "git",
        &["config", "--get-regexp", r"^remote\..*\.(pushurl|url)$"],
    )
    .await
    .ok()
    .and_then(|output| String::from_utf8(output).ok());
    if resolve_published_target(cwd, branch).await? != target
        || pull_request_repositories(
            &repository,
            current_remote_config.as_deref(),
            cached_repository,
        ) != repositories
    {
        return Err(());
    }
    let (pull_request, repository) = found.map_or((None, None), |(pull_request, repository)| {
        (Some(pull_request), Some(repository))
    });
    Ok(PullRequestLookup {
        pull_request,
        repository,
    })
}

async fn resolve_published_target(
    cwd: &std::path::Path,
    branch: &str,
) -> Result<(String, String, String), ()> {
    let local_ref = format!("refs/heads/{branch}");
    let published = String::from_utf8(command_output(
        cwd,
        "git",
        &[
            "for-each-ref",
            "--format=%(push:remotename)%09%(push:remoteref)%09%(upstream:remotename)%09%(upstream:remoteref)%09%(push)",
            &local_ref,
        ],
    )
    .await?)
    .map_err(|_| ())?;
    let (remote, published_branch) = published_target(Some(&published), branch);
    let remote_url = String::from_utf8(
        command_output(cwd, "git", &["remote", "get-url", "--push", &remote]).await?,
    )
    .map_err(|_| ())?;
    let repository = repository_locator(&remote_url).ok_or(())?;
    Ok((remote, published_branch, repository))
}

fn github_host(url: &str) -> Option<&str> {
    url.split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .filter(|host| !host.is_empty())
}

fn repository_locator(remote_url: &str) -> Option<String> {
    let remote_url = remote_url.trim();
    let (authority, path) = if let Some((_, rest)) = remote_url.split_once("://") {
        rest.split_once('/')?
    } else {
        remote_url.split_once(':')?
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && path.split_once('/').is_some()).then(|| format!("{host}/{path}"))
}

fn pull_request_repositories(
    repository: &GhRepository,
    remote_config: Option<&str>,
    cached_repository: Option<&str>,
) -> Vec<(String, bool)> {
    let host = github_host(&repository.url).unwrap_or_default();
    let mut repositories = Vec::new();
    let mut push = |repository: String, required: bool| {
        if !repositories
            .iter()
            .any(|(existing, _): &(String, bool)| existing.eq_ignore_ascii_case(&repository))
        {
            repositories.push((repository, required));
        }
    };
    if let Some(parent) = &repository.parent {
        push(format!("{}/{}", parent.owner.login, parent.name), true);
    }
    push(repository.name_with_owner.clone(), true);
    for locator in remote_config
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|line| line.split_once(' ').map(|(_, url)| url))
        .filter_map(repository_locator)
    {
        let Some((candidate_host, repository)) = locator.split_once('/') else {
            continue;
        };
        if candidate_host.eq_ignore_ascii_case(host) {
            push(repository.to_owned(), false);
        }
    }
    if let Some(index) = cached_repository.and_then(|cached| {
        repositories
            .iter()
            .position(|(repository, _)| repository.eq_ignore_ascii_case(cached))
    }) {
        let (repository, _) = repositories.remove(index);
        repositories.insert(0, (repository, true));
    }
    repositories
}

async fn gh_output(cwd: &std::path::Path, args: &[&str]) -> Result<Vec<u8>, ()> {
    command_output(cwd, "gh", args).await
}

fn published_target(metadata: Option<&str>, local_branch: &str) -> (String, String) {
    let mut fields = metadata.unwrap_or_default().trim_end().split('\t');
    let push_remote = fields.next().unwrap_or_default();
    let push_branch = fields
        .next()
        .unwrap_or_default()
        .strip_prefix("refs/heads/");
    let upstream_remote = fields.next().unwrap_or_default();
    let upstream_branch = fields
        .next()
        .unwrap_or_default()
        .strip_prefix("refs/heads/");
    let push_tracking_branch = fields
        .next()
        .unwrap_or_default()
        .strip_prefix(&format!("refs/remotes/{push_remote}/"));

    if !push_remote.is_empty() {
        let branch = push_branch
            .or(push_tracking_branch)
            .or_else(|| {
                (upstream_remote == push_remote)
                    .then_some(upstream_branch)
                    .flatten()
            })
            .unwrap_or(local_branch);
        return (push_remote.to_owned(), branch.to_owned());
    }
    if !upstream_remote.is_empty() {
        return (
            upstream_remote.to_owned(),
            upstream_branch.unwrap_or(local_branch).to_owned(),
        );
    }
    ("origin".to_owned(), local_branch.to_owned())
}

async fn command_output(
    cwd: &std::path::Path,
    program: &str,
    args: &[&str],
) -> Result<Vec<u8>, ()> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::configure_background_command(&mut command);
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    Ok(output.stdout)
}

#[cfg(test)]
fn parse_pull_request(json: &[u8]) -> Option<PullRequestInfo> {
    let value: GhPullRequest = serde_json::from_slice(json).ok()?;
    pull_request_info(value)
}

fn pull_request_info(value: GhPullRequest) -> Option<PullRequestInfo> {
    let state = match value.state.as_str() {
        "open" if value.is_draft => PullRequestState::Draft,
        "open" => PullRequestState::Open,
        "closed" if value.merged_at.is_some() => PullRequestState::Merged,
        "closed" => PullRequestState::Closed,
        _ => return None,
    };
    Some(PullRequestInfo {
        number: value.number,
        state,
    })
}

fn pull_request_for_repository(
    values: impl IntoIterator<Item = GhPullRequest>,
    repository: &str,
) -> Option<PullRequestInfo> {
    values
        .into_iter()
        .find(|pull_request| {
            pull_request
                .head
                .repo
                .as_ref()
                .is_some_and(|head| head.name_with_owner.eq_ignore_ascii_case(repository))
        })
        .and_then(pull_request_info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_github_pull_request_states() {
        for (number, github_state, draft, state) in [
            (1, "open", false, PullRequestState::Open),
            (2, "open", true, PullRequestState::Draft),
            (3, "closed", false, PullRequestState::Closed),
            (4, "merged", false, PullRequestState::Merged),
            (5, "closed", true, PullRequestState::Closed),
        ] {
            let merged_at = (github_state == "merged").then_some(r#""2026-09-14T00:00:00Z""#);
            let json = format!(
                r#"{{"number":{number},"state":"{}","draft":{draft},"merged_at":{},"head":{{"repo":{{"full_name":"me/repo"}}}}}}"#,
                if github_state == "merged" {
                    "closed"
                } else {
                    github_state
                },
                merged_at.unwrap_or("null"),
            );
            assert_eq!(
                parse_pull_request(json.as_bytes()).map(|value| value.state),
                Some(state)
            );
        }
    }

    #[test]
    fn published_target_uses_push_remote_and_remote_branch() {
        assert_eq!(
            published_target(
                Some("fork\trefs/heads/published-name\torigin\trefs/heads/upstream-name\trefs/remotes/fork/published-name\n"),
                "local-name"
            ),
            ("fork".into(), "published-name".into())
        );
        assert_eq!(
            published_target(
                Some("fork\t\tfork\trefs/heads/upstream-name\trefs/remotes/fork/published-name\n"),
                "local-name"
            ),
            ("fork".into(), "published-name".into())
        );
    }

    #[test]
    fn repository_locator_removes_credentials() {
        assert_eq!(
            repository_locator("https://user:secret@github.example.com/me/herdr.git\n"),
            Some("github.example.com/me/herdr".into())
        );
        assert_eq!(
            repository_locator("git@github.com:me/herdr.git"),
            Some("github.com/me/herdr".into())
        );
    }

    #[test]
    fn ignores_same_owner_branch_from_another_repository() {
        let pages = serde_json::from_slice::<Vec<Vec<GhPullRequest>>>(br#"[[
            {"number":1,"state":"open","draft":false,"merged_at":null,"head":{"repo":{"full_name":"me/tools"}}}
        ],[
            {"number":2,"state":"open","draft":false,"merged_at":null,"head":{"repo":{"full_name":"me/app"}}}
        ]]"#).unwrap();

        assert_eq!(
            pull_request_for_repository(pages.into_iter().flatten(), "me/app")
                .map(|pull_request| pull_request.number),
            Some(2)
        );
    }

    #[test]
    fn configured_repositories_cover_fork_network_bases() {
        let repository = serde_json::from_slice::<GhRepository>(
            br#"
            {
              "nameWithOwner":"me/herdr",
              "url":"https://github.com/me/herdr",
              "parent":{"name":"herdr","owner":{"login":"upstream"}}
            }
        "#,
        )
        .unwrap();
        let remotes = "remote.origin.url https://user:secret@github.com/me/herdr.git
remote.upstream.url git@github.com:top/herdr.git
remote.other.url https://git.example.com/other/herdr.git
";
        assert_eq!(
            pull_request_repositories(&repository, Some(remotes), None),
            vec![
                ("upstream/herdr".into(), true),
                ("me/herdr".into(), true),
                ("top/herdr".into(), false),
            ]
        );
        assert_eq!(
            pull_request_repositories(&repository, Some(remotes), Some("top/herdr"))[0],
            ("top/herdr".into(), true)
        );
        assert_eq!(
            pull_request_repositories(&repository, Some(remotes), Some("removed/herdr")),
            pull_request_repositories(&repository, Some(remotes), None)
        );
    }
}
