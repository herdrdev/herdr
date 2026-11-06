//! Pure Git model, parsers, and collection for Shepherd (Phase F4b).
//!
//! Provides:
//! - Pure parsing functions for `git worktree list --porcelain`,
//!   `git status --porcelain=v1 -b`, and `gh pr list --json`.
//! - Read-only collection of repository worktrees, branch dirty/clean status,
//!   ahead/behind counts, associated Shepherd agents, and pull request info.
//! - Non-blocking execution with short timeouts and graceful fallback when `gh` is unavailable.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Agent;

/// Information about a parsed Git worktree entry from `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedWorktree {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
    pub is_detached: bool,
    pub is_bare: bool,
    pub is_locked: bool,
    pub prunable: bool,
}

/// Parses the output of `git worktree list --porcelain`.
/// True when `path` is inside a git work tree (an unborn repository counts).
pub fn is_git_work_tree(path: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"true"))
}

pub fn parse_git_worktree_porcelain(text: &str) -> Vec<ParsedWorktree> {
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch = None;
    let mut is_detached = false;
    let mut is_bare = false;
    let mut is_locked = false;
    let mut prunable = false;

    let flush = |worktrees: &mut Vec<ParsedWorktree>,
                 path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut Option<String>,
                 detached: &mut bool,
                 bare: &mut bool,
                 locked: &mut bool,
                 prune: &mut bool| {
        if let Some(p) = path.take() {
            worktrees.push(ParsedWorktree {
                path: p,
                head: std::mem::take(head),
                branch: branch.take(),
                is_detached: *detached,
                is_bare: *bare,
                is_locked: *locked,
                prunable: *prune,
            });
            *detached = false;
            *bare = false;
            *locked = false;
            *prune = false;
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush(
                &mut worktrees,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
                &mut is_detached,
                &mut is_bare,
                &mut is_locked,
                &mut prunable,
            );
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("worktree ") {
            flush(
                &mut worktrees,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
                &mut is_detached,
                &mut is_bare,
                &mut is_locked,
                &mut prunable,
            );
            current_path = Some(PathBuf::from(rest.trim()));
        } else if let Some(rest) = trimmed.strip_prefix("HEAD ") {
            current_head = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("branch ") {
            let b = rest.trim();
            let cleaned = if let Some(stripped) = b.strip_prefix("refs/heads/") {
                stripped
            } else {
                b
            };
            current_branch = Some(cleaned.to_string());
        } else if trimmed == "detached" {
            is_detached = true;
        } else if trimmed == "bare" {
            is_bare = true;
        } else if trimmed.starts_with("locked") {
            is_locked = true;
        } else if trimmed.starts_with("prunable") {
            prunable = true;
        }
    }
    flush(
        &mut worktrees,
        &mut current_path,
        &mut current_head,
        &mut current_branch,
        &mut is_detached,
        &mut is_bare,
        &mut is_locked,
        &mut prunable,
    );

    worktrees
}

/// Parsed status of a worktree from `git status --porcelain=v1 -b`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedGitStatus {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub is_dirty: bool,
    pub dirty_count: usize,
    pub is_detached: bool,
}

/// Parses the output of `git status --porcelain=v1 -b`.
pub fn parse_git_status_porcelain(text: &str) -> ParsedGitStatus {
    let mut status = ParsedGitStatus::default();
    let mut lines = text.lines();

    if let Some(first_line) = lines.next() {
        let trimmed = first_line.trim();
        if let Some(branch_line) = trimmed.strip_prefix("## ") {
            let b = branch_line.trim();
            if let Some(rest) = b.strip_prefix("No commits yet on ") {
                status.branch = rest.trim().to_string();
            } else if let Some(rest) = b.strip_prefix("Initial commit on ") {
                status.branch = rest.trim().to_string();
            } else if b.starts_with("HEAD (no branch)") {
                status.branch = "HEAD (no branch)".to_string();
                status.is_detached = true;
            } else {
                let (branch_part, rest_opt) = match b.find("...") {
                    Some(pos) => (&b[..pos], Some(&b[pos + 3..])),
                    None => match b.find(" [") {
                        Some(pos) => (&b[..pos], Some(&b[pos..])),
                        None => (b, None),
                    },
                };
                status.branch = branch_part.trim().to_string();

                if let Some(rest) = rest_opt {
                    let rest = rest.trim();
                    let (upstream_part, brackets_part) = match rest.find(" [") {
                        Some(pos) => (&rest[..pos], Some(&rest[pos + 2..])),
                        None => {
                            if let Some(stripped) = rest.strip_prefix('[') {
                                ("", Some(stripped))
                            } else {
                                (rest, None)
                            }
                        }
                    };
                    if !upstream_part.is_empty() {
                        status.upstream = Some(upstream_part.trim().to_string());
                    }
                    if let Some(brackets) = brackets_part {
                        let inner = brackets.trim_end_matches(']').trim();
                        for part in inner.split(',') {
                            let part = part.trim();
                            if let Some(ahead_str) = part.strip_prefix("ahead ") {
                                status.ahead = ahead_str.trim().parse::<u32>().unwrap_or(0);
                            } else if let Some(behind_str) = part.strip_prefix("behind ") {
                                status.behind = behind_str.trim().parse::<u32>().unwrap_or(0);
                            }
                        }
                    }
                }
            }
        } else if !trimmed.is_empty() {
            status.dirty_count += 1;
        }
    }

    for line in lines {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            status.dirty_count += 1;
        }
    }

    status.is_dirty = status.dirty_count > 0;
    status
}

/// A parsed pull request entry from `gh pr list --json ...`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedPr {
    pub number: u64,
    pub title: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: String,
    pub url: String,
    pub is_draft: bool,
}

/// Parses the output of `gh pr list --json ...`.
pub fn parse_gh_pr_list_json(text: &str) -> Result<Vec<ParsedPr>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| format!("invalid JSON from gh: {e}"))?;
    let array = value
        .as_array()
        .ok_or_else(|| "expected JSON array from gh pr list".to_string())?;

    let mut prs = Vec::new();
    for item in array {
        let number = item.get("number").and_then(Value::as_u64).unwrap_or(0);
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let head_branch = item
            .get("headRefName")
            .or_else(|| item.get("head_branch"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let base_branch = item
            .get("baseRefName")
            .or_else(|| item.get("base_branch"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let state = item
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string();
        let url = item
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let is_draft = item
            .get("isDraft")
            .or_else(|| item.get("is_draft"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        prs.push(ParsedPr {
            number,
            title,
            head_branch,
            base_branch,
            state,
            url,
            is_draft,
        });
    }
    Ok(prs)
}

/// Parses `<behind>\t<ahead>` output from `git rev-list --left-right --count`.
pub fn parse_rev_list_left_right(text: &str) -> (u32, u32) {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() >= 2 {
        let behind = parts[0].parse::<u32>().unwrap_or(0);
        let ahead = parts[1].parse::<u32>().unwrap_or(0);
        (behind, ahead)
    } else {
        (0, 0)
    }
}

/// Summary of a Pull Request associated with a branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitPrSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub is_draft: bool,
}

/// Information about a single worktree in the repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitWorktreeInfo {
    pub path: String,
    pub branch: String,
    pub is_main: bool,
    pub is_clean: bool,
    pub dirty_count: usize,
    pub ahead: u32,
    pub behind: u32,
    pub upstream: Option<String>,
    pub agent_id: Option<String>,
    pub pr: Option<GitPrSummary>,
}

/// Full repository git status returned by `git_status` API call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitRepoStatus {
    pub root: String,
    pub worktrees: Vec<GitWorktreeInfo>,
    pub gh_available: bool,
    pub gh_note: Option<String>,
    pub error: Option<String>,
}

/// Executes a process with a bounded timeout, returning its Output.
pub fn run_cmd_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let (tx, rx) = std::sync::mpsc::channel();
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("cannot spawn {program}: {e}")),
    };

    std::thread::spawn(move || {
        let res = child.wait_with_output();
        let _ = tx.send(res);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(format!("failed waiting for {program}: {e}")),
        Err(_) => Err(format!("{program} timed out after {}s", timeout.as_secs())),
    }
}

/// Collects repository worktrees and git status using the default command runner.
pub fn collect_git_status(root: &Path, agents: &[Agent]) -> Result<GitRepoStatus, String> {
    collect_git_status_internal(root, agents, run_cmd_with_timeout)
}

/// Collects repository worktrees and git status with an optional base worktrees list
/// (for example, provided by Shepherd's `worktree.list` API) using the default command runner.
pub fn collect_git_status_with_base(
    root: &Path,
    agents: &[Agent],
    base_worktrees: Option<Vec<ParsedWorktree>>,
) -> Result<GitRepoStatus, String> {
    collect_git_status_with_base_internal(root, agents, base_worktrees, run_cmd_with_timeout)
}

/// Collects repository worktrees and git status using an injectable command runner.
pub fn collect_git_status_internal<F>(
    root: &Path,
    agents: &[Agent],
    runner: F,
) -> Result<GitRepoStatus, String>
where
    F: FnMut(&str, &[&str], &Path, Duration) -> Result<std::process::Output, String>,
{
    collect_git_status_with_base_internal(root, agents, None, runner)
}

/// Collects repository worktrees and git status with optional base worktrees using an injectable runner.
pub fn collect_git_status_with_base_internal<F>(
    root: &Path,
    agents: &[Agent],
    base_worktrees: Option<Vec<ParsedWorktree>>,
    mut runner: F,
) -> Result<GitRepoStatus, String>
where
    F: FnMut(&str, &[&str], &Path, Duration) -> Result<std::process::Output, String>,
{
    let cmd_timeout = Duration::from_secs(3);

    let parsed_wts = match base_worktrees {
        Some(wts) => wts,
        None => {
            let wt_output = match runner(
                "git",
                &["worktree", "list", "--porcelain"],
                root,
                cmd_timeout,
            ) {
                Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Ok(GitRepoStatus {
                        root: root.display().to_string(),
                        worktrees: Vec::new(),
                        gh_available: false,
                        gh_note: None,
                        error: Some(format!("git worktree list failed: {}", stderr.trim())),
                    });
                }
                Err(e) => {
                    return Ok(GitRepoStatus {
                        root: root.display().to_string(),
                        worktrees: Vec::new(),
                        gh_available: false,
                        gh_note: None,
                        error: Some(format!("cannot execute git: {e}")),
                    });
                }
            };
            parse_git_worktree_porcelain(&wt_output)
        }
    };
    if parsed_wts.is_empty() {
        return Ok(GitRepoStatus {
            root: root.display().to_string(),
            worktrees: Vec::new(),
            gh_available: false,
            gh_note: None,
            error: None,
        });
    }

    let (prs, gh_available, gh_note) = match runner(
        "gh",
        &[
            "pr",
            "list",
            "--json",
            "number,title,headRefName,baseRefName,state,url,isDraft",
            "--limit",
            "50",
        ],
        root,
        cmd_timeout,
    ) {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            match parse_gh_pr_list_json(&text) {
                Ok(prs) => (prs, true, None),
                Err(_) => (Vec::new(), false, Some("gh no disponible".to_string())),
            }
        }
        _ => (Vec::new(), false, Some("gh no disponible".to_string())),
    };

    let mut worktrees = Vec::new();

    for (idx, wt) in parsed_wts.iter().enumerate() {
        let is_main = idx == 0;
        let branch_name = wt.branch.clone().unwrap_or_else(|| {
            if wt.is_detached {
                "(detached)".to_string()
            } else {
                "HEAD".to_string()
            }
        });

        let (status, ahead, behind) = match runner(
            "git",
            &["status", "--porcelain=v1", "-b"],
            &wt.path,
            cmd_timeout,
        ) {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                let st = parse_git_status_porcelain(&text);
                let mut ahead = st.ahead;
                let mut behind = st.behind;

                if st.upstream.is_none() && !st.is_detached && ahead == 0 && behind == 0 {
                    let base = crate::mcp::default_base(&root.display().to_string(), "");
                    if base != branch_name && base != "HEAD" {
                        let rev_arg = format!("{base}...HEAD");
                        if let Ok(rev_out) = runner(
                            "git",
                            &["rev-list", "--left-right", "--count", &rev_arg],
                            &wt.path,
                            cmd_timeout,
                        ) {
                            if rev_out.status.success() {
                                let rev_text = String::from_utf8_lossy(&rev_out.stdout);
                                let (b, a) = parse_rev_list_left_right(&rev_text);
                                ahead = a;
                                behind = b;
                            }
                        }
                    }
                }
                (st, ahead, behind)
            }
            _ => (ParsedGitStatus::default(), 0, 0),
        };

        let agent_id = agents.iter().find_map(|ag| {
            if !ag.worktree.is_empty() {
                if ag.worktree == wt.path.display().to_string() {
                    return Some(ag.id.clone());
                }
                if let (Ok(ag_can), Ok(wt_can)) = (
                    Path::new(&ag.worktree).canonicalize(),
                    wt.path.canonicalize(),
                ) {
                    if ag_can == wt_can {
                        return Some(ag.id.clone());
                    }
                }
            }
            if !ag.branch.is_empty() && ag.branch == branch_name {
                return Some(ag.id.clone());
            }
            None
        });

        let pr = prs
            .iter()
            .find(|p| p.head_branch == branch_name)
            .map(|p| GitPrSummary {
                number: p.number,
                title: p.title.clone(),
                state: p.state.clone(),
                url: p.url.clone(),
                is_draft: p.is_draft,
            });

        worktrees.push(GitWorktreeInfo {
            path: wt.path.display().to_string(),
            branch: branch_name,
            is_main,
            is_clean: !status.is_dirty,
            dirty_count: status.dirty_count,
            ahead,
            behind,
            upstream: status.upstream,
            agent_id,
            pr,
        });
    }

    Ok(GitRepoStatus {
        root: root.display().to_string(),
        worktrees,
        gh_available,
        gh_note,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_worktree_porcelain_fixtures() {
        let fixture = "\
worktree /path/to/main
HEAD 1234567890abcdef1234567890abcdef12345678
branch refs/heads/master

worktree /path/to/wt-coder
HEAD abcdef1234567890abcdef1234567890abcdef12
branch refs/heads/shep/1-coder

worktree /path/to/wt-detached
HEAD fedcba0987654321fedcba0987654321fedcba09
detached

worktree /path/to/wt-locked
HEAD 1111111111111111111111111111111111111111
branch refs/heads/locked-branch
locked work in progress
";
        let wts = parse_git_worktree_porcelain(fixture);
        assert_eq!(wts.len(), 4);

        assert_eq!(wts[0].path, PathBuf::from("/path/to/main"));
        assert_eq!(wts[0].branch.as_deref(), Some("master"));
        assert!(!wts[0].is_detached);
        assert!(!wts[0].is_locked);

        assert_eq!(wts[1].path, PathBuf::from("/path/to/wt-coder"));
        assert_eq!(wts[1].branch.as_deref(), Some("shep/1-coder"));
        assert!(!wts[1].is_detached);

        assert_eq!(wts[2].path, PathBuf::from("/path/to/wt-detached"));
        assert!(wts[2].branch.is_none());
        assert!(wts[2].is_detached);

        assert_eq!(wts[3].path, PathBuf::from("/path/to/wt-locked"));
        assert_eq!(wts[3].branch.as_deref(), Some("locked-branch"));
        assert!(wts[3].is_locked);
    }

    #[test]
    fn test_parse_git_status_porcelain_fixtures() {
        // 1. Clean branch with upstream
        let clean_upstream = "## master...origin/master\n";
        let st1 = parse_git_status_porcelain(clean_upstream);
        assert_eq!(st1.branch, "master");
        assert_eq!(st1.upstream.as_deref(), Some("origin/master"));
        assert_eq!(st1.ahead, 0);
        assert_eq!(st1.behind, 0);
        assert!(!st1.is_dirty);
        assert_eq!(st1.dirty_count, 0);

        // 2. Dirty branch with ahead & behind
        let dirty_ahead_behind = "\
## feature...origin/feature [ahead 3, behind 1]
 M src/git.rs
?? untracked.txt
 D deleted.txt
";
        let st2 = parse_git_status_porcelain(dirty_ahead_behind);
        assert_eq!(st2.branch, "feature");
        assert_eq!(st2.upstream.as_deref(), Some("origin/feature"));
        assert_eq!(st2.ahead, 3);
        assert_eq!(st2.behind, 1);
        assert!(st2.is_dirty);
        assert_eq!(st2.dirty_count, 3);

        // 3. Local branch without upstream
        let local_dirty = "\
## shep/1-auth
 M Cargo.toml
";
        let st3 = parse_git_status_porcelain(local_dirty);
        assert_eq!(st3.branch, "shep/1-auth");
        assert!(st3.upstream.is_none());
        assert!(st3.is_dirty);
        assert_eq!(st3.dirty_count, 1);

        // 4. Detached HEAD
        let detached = "## HEAD (no branch)\n";
        let st4 = parse_git_status_porcelain(detached);
        assert_eq!(st4.branch, "HEAD (no branch)");
        assert!(st4.is_detached);
        assert!(!st4.is_dirty);

        // 5. Initial commit / unborn branch
        let initial = "## No commits yet on develop\n?? README.md\n";
        let st5 = parse_git_status_porcelain(initial);
        assert_eq!(st5.branch, "develop");
        assert!(st5.is_dirty);
        assert_eq!(st5.dirty_count, 1);
    }

    #[test]
    fn test_parse_gh_pr_list_json_fixtures() {
        let fixture = r#"[
  {
    "number": 42,
    "title": "Add Git view to Shepherd overlay",
    "headRefName": "shep/1-git-view",
    "baseRefName": "master",
    "state": "OPEN",
    "url": "https://github.com/example/repo/pull/42",
    "isDraft": false
  },
  {
    "number": 43,
    "title": "WIP: Experimental branch",
    "headRefName": "shep/2-experimental",
    "baseRefName": "master",
    "state": "OPEN",
    "url": "https://github.com/example/repo/pull/43",
    "isDraft": true
  }
]"#;
        let prs = parse_gh_pr_list_json(fixture).unwrap();
        assert_eq!(prs.len(), 2);

        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[0].title, "Add Git view to Shepherd overlay");
        assert_eq!(prs[0].head_branch, "shep/1-git-view");
        assert_eq!(prs[0].base_branch, "master");
        assert_eq!(prs[0].state, "OPEN");
        assert!(!prs[0].is_draft);

        assert_eq!(prs[1].number, 43);
        assert_eq!(prs[1].head_branch, "shep/2-experimental");
        assert!(prs[1].is_draft);

        // Empty list
        assert_eq!(parse_gh_pr_list_json("[]").unwrap().len(), 0);
        assert_eq!(parse_gh_pr_list_json("").unwrap().len(), 0);

        // Malformed json
        assert!(parse_gh_pr_list_json("invalid").is_err());
    }

    #[test]
    fn test_parse_rev_list_left_right() {
        assert_eq!(parse_rev_list_left_right("0\t2\n"), (0, 2));
        assert_eq!(parse_rev_list_left_right("3 5"), (3, 5));
        assert_eq!(parse_rev_list_left_right(""), (0, 0));
    }

    #[test]
    fn test_collect_git_status_internal_with_synthetic_runner() {
        let root = Path::new("/virtual/repo");
        let agents = vec![Agent {
            id: "coder-1".into(),
            worktree: "/virtual/repo/worktrees/wt-1".into(),
            branch: "shep/1-auth".into(),
            ..Agent::default()
        }];

        let runner = |cmd: &str,
                      args: &[&str],
                      cwd: &Path,
                      _t: Duration|
         -> Result<std::process::Output, String> {
            use std::os::unix::process::ExitStatusExt;
            if cmd == "git" && args == ["worktree", "list", "--porcelain"] {
                let out = "\
worktree /virtual/repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/master

worktree /virtual/repo/worktrees/wt-1
HEAD 2222222222222222222222222222222222222222
branch refs/heads/shep/1-auth
";
                return Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: out.as_bytes().to_vec(),
                    stderr: Vec::new(),
                });
            }
            if cmd == "git" && args == ["status", "--porcelain=v1", "-b"] {
                if cwd == Path::new("/virtual/repo") {
                    let out = "## master...origin/master\n";
                    return Ok(std::process::Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: out.as_bytes().to_vec(),
                        stderr: Vec::new(),
                    });
                } else if cwd == Path::new("/virtual/repo/worktrees/wt-1") {
                    let out = "## shep/1-auth\n M src/lib.rs\n";
                    return Ok(std::process::Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: out.as_bytes().to_vec(),
                        stderr: Vec::new(),
                    });
                }
            }
            if cmd == "gh" {
                let out = r#"[{"number":10,"title":"Auth","headRefName":"shep/1-auth","baseRefName":"master","state":"OPEN","url":"https://github.com/pr/10","isDraft":false}]"#;
                return Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: out.as_bytes().to_vec(),
                    stderr: Vec::new(),
                });
            }
            Err("command failed".into())
        };

        let status = collect_git_status_internal(root, &agents, runner).unwrap();
        assert_eq!(status.worktrees.len(), 2);
        assert!(status.gh_available);
        assert!(status.gh_note.is_none());

        // Worktree 0 (main)
        assert_eq!(status.worktrees[0].branch, "master");
        assert!(status.worktrees[0].is_main);
        assert!(status.worktrees[0].is_clean);
        assert!(status.worktrees[0].agent_id.is_none());

        // Worktree 1 (agent coder-1)
        assert_eq!(status.worktrees[1].branch, "shep/1-auth");
        assert!(!status.worktrees[1].is_main);
        assert!(!status.worktrees[1].is_clean);
        assert_eq!(status.worktrees[1].dirty_count, 1);
        assert_eq!(status.worktrees[1].agent_id.as_deref(), Some("coder-1"));
        assert!(status.worktrees[1].pr.is_some());
        assert_eq!(status.worktrees[1].pr.as_ref().unwrap().number, 10);
    }

    #[test]
    fn test_git_status_does_not_associate_old_worktree_with_unrelated_agent() {
        let root = Path::new("/virtual/repo");
        // Agent brain-1 has task_id "1" but no worktree or branch registered
        let agents = vec![Agent {
            id: "brain-1".into(),
            task_id: "1".into(),
            worktree: "".into(),
            branch: "".into(),
            ..Agent::default()
        }];

        let runner = |cmd: &str,
                      args: &[&str],
                      _cwd: &Path,
                      _t: Duration|
         -> Result<std::process::Output, String> {
            use std::os::unix::process::ExitStatusExt;
            if cmd == "git" && args == ["worktree", "list", "--porcelain"] {
                let out = "\
worktree /virtual/repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/master

worktree /virtual/repo/worktrees/wt-coder
HEAD 2222222222222222222222222222222222222222
branch refs/heads/shep/1-coder
";
                return Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: out.as_bytes().to_vec(),
                    stderr: Vec::new(),
                });
            }
            if cmd == "git" && args == ["status", "--porcelain=v1", "-b"] {
                let out = "## shep/1-coder\n";
                return Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: out.as_bytes().to_vec(),
                    stderr: Vec::new(),
                });
            }
            if cmd == "gh" {
                return Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: b"[]".to_vec(),
                    stderr: Vec::new(),
                });
            }
            Err("command failed".into())
        };

        let status = collect_git_status_internal(root, &agents, runner).unwrap();
        assert_eq!(status.worktrees.len(), 2);
        // Worktree shep/1-coder should NOT be associated with brain-1
        assert_eq!(status.worktrees[1].branch, "shep/1-coder");
        assert_eq!(status.worktrees[1].agent_id, None);
    }

    #[test]
    fn test_collect_git_status_real_tempdir_repo() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("shepherd-git-real-{}-{nonce}", std::process::id()));
        let wt_path =
            std::env::temp_dir().join(format!("shepherd-wt-real-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
        };

        assert!(git(&["init", "-b", "master"]).status.success());
        assert!(git(&["config", "user.email", "test@example.test"])
            .status
            .success());
        assert!(git(&["config", "user.name", "Shepherd Test"])
            .status
            .success());
        std::fs::write(root.join("init.txt"), "hello\n").unwrap();
        assert!(git(&["add", "init.txt"]).status.success());
        assert!(git(&["commit", "-m", "initial commit"]).status.success());

        // Create a worktree
        assert!(git(&[
            "worktree",
            "add",
            "-b",
            "shep/42-test",
            wt_path.to_str().unwrap()
        ])
        .status
        .success());

        // Make the worktree dirty
        std::fs::write(wt_path.join("dirty.txt"), "dirty content\n").unwrap();

        let agents = vec![Agent {
            id: "coder-42".into(),
            worktree: wt_path.display().to_string(),
            branch: "shep/42-test".into(),
            ..Agent::default()
        }];

        let status = collect_git_status(&root, &agents).unwrap();
        assert_eq!(status.worktrees.len(), 2);

        // Main worktree
        assert_eq!(status.worktrees[0].branch, "master");
        assert!(status.worktrees[0].is_main);
        assert!(status.worktrees[0].is_clean);

        // Secondary worktree
        assert_eq!(status.worktrees[1].branch, "shep/42-test");
        assert!(!status.worktrees[1].is_main);
        assert!(!status.worktrees[1].is_clean);
        assert_eq!(status.worktrees[1].dirty_count, 1);
        assert_eq!(status.worktrees[1].agent_id.as_deref(), Some("coder-42"));

        // gh is unavailable or no network PRs
        assert!(!status.gh_available || status.worktrees[1].pr.is_none());

        // Clean up
        let _ = git(&["worktree", "remove", "--force", wt_path.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(&wt_path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_collect_git_status_with_base_synthetic() {
        let root = Path::new("/mock/repo");
        let base_worktrees = vec![
            ParsedWorktree {
                path: PathBuf::from("/mock/repo"),
                head: "commit1".into(),
                branch: Some("master".into()),
                is_bare: false,
                is_detached: false,
                is_locked: false,
                prunable: false,
            },
            ParsedWorktree {
                path: PathBuf::from("/mock/repo/.worktrees/shep-1"),
                head: "commit2".into(),
                branch: Some("shep/1-task".into()),
                is_bare: false,
                is_detached: false,
                is_locked: false,
                prunable: false,
            },
        ];
        let agents = vec![Agent {
            id: "coder-1".into(),
            worktree: "/mock/repo/.worktrees/shep-1".into(),
            branch: "shep/1-task".into(),
            ..Agent::default()
        }];
        let runner = |_cmd: &str,
                      args: &[&str],
                      cwd: &Path,
                      _timeout: Duration|
         -> Result<std::process::Output, String> {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            if args.contains(&"status") {
                let stdout = if cwd == Path::new("/mock/repo") {
                    b"## master...origin/master\n".to_vec()
                } else {
                    b"## shep/1-task\n M file.rs\n".to_vec()
                };
                return Ok(std::process::Output {
                    #[cfg(unix)]
                    status: std::process::ExitStatus::from_raw(0),
                    #[cfg(not(unix))]
                    status: std::process::Command::new("true").status().unwrap(),
                    stdout,
                    stderr: Vec::new(),
                });
            }
            if args.contains(&"rev-list") {
                return Ok(std::process::Output {
                    #[cfg(unix)]
                    status: std::process::ExitStatus::from_raw(0),
                    #[cfg(not(unix))]
                    status: std::process::Command::new("true").status().unwrap(),
                    stdout: b"0\t1\n".to_vec(),
                    stderr: Vec::new(),
                });
            }
            Err("failed".into())
        };

        let res =
            collect_git_status_with_base_internal(root, &agents, Some(base_worktrees), runner)
                .unwrap();
        assert_eq!(res.worktrees.len(), 2);
        assert_eq!(res.worktrees[0].branch, "master");
        assert!(res.worktrees[0].is_main);
        assert!(res.worktrees[0].is_clean);

        assert_eq!(res.worktrees[1].branch, "shep/1-task");
        assert!(!res.worktrees[1].is_main);
        assert!(!res.worktrees[1].is_clean);
        assert_eq!(res.worktrees[1].agent_id.as_deref(), Some("coder-1"));
    }
}
