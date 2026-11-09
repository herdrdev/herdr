//! Diffs between snapshots, or between a snapshot and the current worktree.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    capture::worktree_tree, limits::SnapshotLimits, read_one, tree_of, Repo, SnapshotInfo,
    EMPTY_TREE,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiffFile {
    pub path: String,
    /// `added`, `modified`, `deleted` or `type-changed`.
    pub status: String,
    pub added: u64,
    pub removed: u64,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiffResult {
    /// Snapshot id, or `-` for "nothing" (the empty tree).
    pub from: String,
    /// Snapshot id, or `current` for the live worktree.
    pub to: String,
    pub files: Vec<DiffFile>,
    pub added: u64,
    pub removed: u64,
    /// Full unified diff; only when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShowResult {
    pub snapshot: SnapshotInfo,
    /// Changes the snapshot introduced over the agent's previous one.
    pub diff: DiffResult,
}

fn diff_trees(
    repo: &Repo,
    from: (&str, &str),
    to: (&str, &str),
    full: bool,
) -> Result<DiffResult, String> {
    let (ta, tb) = (from.1, to.1);
    let raw = repo.run(&[
        "diff-tree",
        "-r",
        "--no-renames",
        "--name-status",
        "-z",
        ta,
        tb,
    ])?;
    let mut files: BTreeMap<String, DiffFile> = BTreeMap::new();
    let mut parts = raw.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
        let path = String::from_utf8_lossy(path).into_owned();
        let status = match status {
            b"A" => "added",
            b"D" => "deleted",
            b"T" => "type-changed",
            _ => "modified",
        };
        files.insert(
            path.clone(),
            DiffFile {
                path,
                status: status.into(),
                added: 0,
                removed: 0,
                binary: false,
            },
        );
    }
    let nums = repo.run(&[
        "diff",
        "--numstat",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        ta,
        tb,
    ])?;
    for rec in nums.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let text = String::from_utf8_lossy(rec);
        let mut cols = text.splitn(3, '\t');
        let (a, r, path) = (cols.next(), cols.next(), cols.next());
        if let (Some(a), Some(r), Some(path)) = (a, r, path) {
            if let Some(f) = files.get_mut(path) {
                f.binary = a == "-";
                f.added = a.parse().unwrap_or(0);
                f.removed = r.parse().unwrap_or(0);
            }
        }
    }
    let files: Vec<DiffFile> = files.into_values().collect();
    let patch = if full {
        let out = repo.run(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            ta,
            tb,
        ])?;
        Some(String::from_utf8_lossy(&out).into_owned())
    } else {
        None
    };
    Ok(DiffResult {
        from: from.0.to_owned(),
        to: to.0.to_owned(),
        added: files.iter().map(|f| f.added).sum(),
        removed: files.iter().map(|f| f.removed).sum(),
        files,
        patch,
    })
}

/// Diff snapshot `a` against snapshot `b`, or against the current worktree when `b` is `None`.
pub fn diff(
    root: &Path,
    a: &str,
    b: Option<&str>,
    full: bool,
    limits: &SnapshotLimits,
) -> Result<DiffResult, String> {
    let repo = Repo::open(root)?;
    let sa = read_one(&repo, a)?;
    let ta = tree_of(&repo, &sa.commit)?;
    match b {
        Some(b) => {
            let sb = read_one(&repo, b)?;
            let tb = tree_of(&repo, &sb.commit)?;
            diff_trees(&repo, (&sa.id, &ta), (&sb.id, &tb), full)
        }
        None => {
            repo.ensure_idle()?;
            let now = worktree_tree(&repo, limits)?;
            diff_trees(&repo, (&sa.id, &ta), ("current", &now.tree), full)
        }
    }
}

/// A snapshot with what it changed over the agent's previous snapshot.
pub fn show(root: &Path, id: &str, full: bool) -> Result<ShowResult, String> {
    let repo = Repo::open(root)?;
    let snap = read_one(&repo, id)?;
    let tree = tree_of(&repo, &snap.commit)?;
    let prev = match snap.prev.as_deref().map(|p| read_one(&repo, p)) {
        Some(Ok(p)) => Some(p),
        // Previous one pruned/deleted: compare against nothing.
        _ => None,
    };
    let diff = match prev {
        Some(p) => {
            let pt = tree_of(&repo, &p.commit)?;
            diff_trees(&repo, (&p.id, &pt), (&snap.id, &tree), full)?
        }
        None => diff_trees(&repo, ("-", EMPTY_TREE), (&snap.id, &tree), full)?,
    };
    Ok(ShowResult {
        snapshot: snap,
        diff,
    })
}
