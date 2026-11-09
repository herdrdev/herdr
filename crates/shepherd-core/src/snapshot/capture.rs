//! Capture the state of a worktree as a hidden snapshot ref.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{
    limits::SnapshotLimits, make_id, read_all, ref_name, render_message, tree_of, validate_agent,
    OmittedFile, Repo, SnapshotInfo, SnapshotReason, TempIndex, EMPTY_TREE,
};

const MAX_OMITTED_RECORDED: usize = 100;

/// Result of a capture. `created == false` means the state was identical to the agent's
/// latest snapshot, which is returned instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureResult {
    pub created: bool,
    pub snapshot: SnapshotInfo,
    /// Snapshots removed by the per-agent limit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pruned: Vec<String>,
}

/// Tree object of the current worktree state (honouring `.gitignore` and the size limits).
pub(crate) struct WorktreeTree {
    pub tree: String,
    pub omitted: Vec<OmittedFile>,
}

/// Write the current state of the worktree to the object database without touching the
/// user's index: stage everything into a scratch index and `write-tree` it.
pub(crate) fn worktree_tree(repo: &Repo, limits: &SnapshotLimits) -> Result<WorktreeTree, String> {
    let idx = TempIndex::new(repo, true)?;
    let env = idx.env();

    // Candidates: tracked + untracked, not ignored.
    let listed = repo.run_with(
        &env,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        None,
    )?;
    let mut omitted = Vec::new();
    let mut total: u64 = 0;
    let mut seen = std::collections::HashSet::new();
    for raw in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(raw).into_owned();
        if !seen.insert(path.clone()) {
            continue;
        }
        // Missing/unreadable files are handled by `git add -A` (deletions).
        let Ok(meta) = std::fs::symlink_metadata(repo.top.join(&path)) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > limits.max_file_bytes {
            omitted.push(OmittedFile {
                path,
                reason: format!("larger than max_file_bytes ({})", limits.max_file_bytes),
                size: meta.len(),
            });
        } else {
            total += meta.len();
        }
    }
    if total > limits.max_capture_bytes {
        return Err(format!(
            "capture is {total} bytes, over max_capture_bytes ({}); raise [shepherd.snapshots] max_capture_bytes or ignore the large files",
            limits.max_capture_bytes
        ));
    }

    // `git add -A` over everything except the omitted files (so oversized files are never
    // hashed), then drop any stale index entry for them.
    if omitted.is_empty() {
        repo.run_with(&env, &["add", "-A"], None)?;
    } else {
        let mut spec = b".\0".to_vec();
        for o in &omitted {
            spec.extend_from_slice(b":(exclude,literal)");
            spec.extend_from_slice(o.path.as_bytes());
            spec.push(0);
        }
        repo.run_with(
            &env,
            &["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"],
            Some(&spec),
        )?;
        let mut spec = Vec::new();
        for o in &omitted {
            spec.extend_from_slice(b":(literal)");
            spec.extend_from_slice(o.path.as_bytes());
            spec.push(0);
        }
        repo.run_with(
            &env,
            &[
                "rm",
                "--cached",
                "-q",
                "-r",
                "--ignore-unmatch",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            Some(&spec),
        )?;
    }
    let tree = String::from_utf8_lossy(&repo.run_with(&env, &["write-tree"], None)?)
        .trim()
        .to_owned();
    Ok(WorktreeTree { tree, omitted })
}

/// `(files changed, lines added, lines removed)` between two trees.
pub(crate) fn summarize(repo: &Repo, from: &str, to: &str) -> Result<(u32, u64, u64), String> {
    let out = repo.run(&[
        "diff",
        "--numstat",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        from,
        to,
    ])?;
    let (mut files, mut added, mut removed) = (0u32, 0u64, 0u64);
    for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let text = String::from_utf8_lossy(rec);
        let mut cols = text.splitn(3, '\t');
        let (a, r) = (cols.next().unwrap_or("-"), cols.next().unwrap_or("-"));
        files += 1;
        added += a.parse::<u64>().unwrap_or(0);
        removed += r.parse::<u64>().unwrap_or(0);
    }
    Ok((files, added, removed))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Capture `root`'s worktree for `agent`.
pub fn capture(
    root: &Path,
    agent: &str,
    reason: SnapshotReason,
    limits: &SnapshotLimits,
) -> Result<CaptureResult, String> {
    validate_agent(agent)?;
    let repo = Repo::open(root)?;
    repo.ensure_idle()?;
    capture_in(&repo, agent, reason, limits)
}

pub(crate) fn capture_in(
    repo: &Repo,
    agent: &str,
    reason: SnapshotReason,
    limits: &SnapshotLimits,
) -> Result<CaptureResult, String> {
    let wt = worktree_tree(repo, limits)?;

    for _ in 0..5 {
        let existing = read_all(repo, Some(agent))?;
        let latest = existing.last();
        let prev_tree = match latest {
            Some(l) => tree_of(repo, &l.commit)?,
            None => EMPTY_TREE.to_owned(),
        };
        if let Some(l) = latest {
            if prev_tree == wt.tree {
                return Ok(CaptureResult {
                    created: false,
                    snapshot: l.clone(),
                    pruned: Vec::new(),
                });
            }
        }
        let n = latest.map_or(1, |l| l.n + 1);
        let (files, added, removed) = summarize(repo, &prev_tree, &wt.tree)?;
        let mut info = SnapshotInfo {
            id: make_id(agent, n),
            agent: agent.to_owned(),
            n,
            commit: String::new(),
            reason,
            created: now_secs(),
            worktree: repo.top.to_string_lossy().into_owned(),
            prev: latest.map(|l| l.id.clone()),
            files,
            added,
            removed,
            omitted: wt
                .omitted
                .iter()
                .take(MAX_OMITTED_RECORDED)
                .cloned()
                .collect(),
        };
        let message = render_message(&info);
        let identity: [(&str, &std::ffi::OsStr); 4] = [
            ("GIT_AUTHOR_NAME", "shepherd".as_ref()),
            ("GIT_AUTHOR_EMAIL", "shepherd@localhost".as_ref()),
            ("GIT_COMMITTER_NAME", "shepherd".as_ref()),
            ("GIT_COMMITTER_EMAIL", "shepherd@localhost".as_ref()),
        ];
        let commit = String::from_utf8_lossy(&repo.run_with(
            &identity,
            &["commit-tree", &wt.tree, "-F", "-"],
            Some(message.as_bytes()),
        )?)
        .trim()
        .to_owned();
        // Create-only: an empty old value fails if a concurrent capture took this `n`.
        if repo
            .run(&["update-ref", &ref_name(agent, n), &commit, ""])
            .is_err()
        {
            continue;
        }
        info.commit = commit;
        let pruned = super::prune_repo(repo, Some(agent), limits.max_per_agent)?.pruned;
        return Ok(CaptureResult {
            created: true,
            snapshot: info,
            pruned,
        });
    }
    Err(format!(
        "could not allocate a snapshot number for {agent} (concurrent captures)"
    ))
}
