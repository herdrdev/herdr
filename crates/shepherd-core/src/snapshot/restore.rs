//! Restore files (or the whole worktree) from a snapshot.
//!
//! Safety: the current state is always captured first as a `pre-restore` snapshot, the
//! user's index/branch are never touched (files are written through a scratch index), and
//! files that exist now but not in the snapshot are only removed on request.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use super::{
    capture::capture_in, limits::SnapshotLimits, read_one, tree_of, Repo, SnapshotInfo,
    SnapshotReason, TempIndex,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RestoreResult {
    /// The snapshot that was restored.
    pub restored: SnapshotInfo,
    /// The `pre-restore` snapshot of the state before restoring.
    pub pre_restore: SnapshotInfo,
    /// False when the previous state was already identical to the agent's latest snapshot.
    pub pre_restore_created: bool,
    pub restored_files: Vec<String>,
    /// Files present now but absent from the snapshot (full restore only). Kept unless
    /// `delete_extra` was set, in which case they are in `deleted_files`.
    pub extra_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

fn nul_list(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// A repo-relative path with no `..`, root or prefix components.
fn clean_relative(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let mut parts = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return Err(format!("path must be relative to the worktree: {path:?}")),
        }
    }
    if parts.is_empty() {
        return Err(format!("empty path: {path:?}"));
    }
    Ok(parts.join("/"))
}

/// Restore `paths` (files or directories) from `id`, or the whole worktree when `paths` is
/// empty (which requires `confirm`).
pub fn restore(
    root: &Path,
    id: &str,
    paths: &[String],
    confirm: bool,
    delete_extra: bool,
    limits: &SnapshotLimits,
) -> Result<RestoreResult, String> {
    let repo = Repo::open(root)?;
    repo.ensure_idle()?;
    let snap = read_one(&repo, id)?;
    if paths.is_empty() && !confirm {
        return Err(
            "restoring the whole worktree overwrites local changes; pass --confirm (the current state is saved first as a pre-restore snapshot)"
                .into(),
        );
    }
    if !paths.is_empty() && delete_extra {
        return Err("--delete-extra only applies to a full restore (no --path)".into());
    }

    // Resolve what to write before touching anything.
    let snap_tree = tree_of(&repo, &snap.commit)?;
    let mut targets: Vec<String> = Vec::new();
    for p in paths {
        let rel = clean_relative(p)?;
        // Literal pathspecs: `:colon.txt` and `[glob]*.txt` are file names, not patterns.
        let found = nul_list(&repo.run_with(
            &[("GIT_LITERAL_PATHSPECS", "1".as_ref())],
            &[
                "ls-tree",
                "-r",
                "-z",
                "--name-only",
                &snap.commit,
                "--",
                &rel,
            ],
            None,
        )?);
        if found.is_empty() {
            return Err(format!("path not in snapshot {}: {p}", snap.id));
        }
        targets.extend(found);
    }

    // Always save the current state first.
    let pre = capture_in(&repo, &snap.agent, SnapshotReason::PreRestore, limits)?;
    let pre_tree = tree_of(&repo, &pre.snapshot.commit)?;

    let idx = TempIndex::new(&repo, false)?;
    let env = idx.env();
    repo.run_with(&env, &["read-tree", &snap.commit], None)?;
    let restored_files = if paths.is_empty() {
        repo.run_with(&env, &["checkout-index", "-f", "-a"], None)?;
        nul_list(&repo.run(&["ls-tree", "-r", "-z", "--name-only", &snap.commit])?)
    } else {
        let mut stdin = Vec::new();
        for t in &targets {
            stdin.extend_from_slice(t.as_bytes());
            stdin.push(0);
        }
        repo.run_with(
            &env,
            &["checkout-index", "-f", "-z", "--stdin"],
            Some(&stdin),
        )?;
        targets
    };

    let (mut extra_files, mut deleted_files) = (Vec::new(), Vec::new());
    if paths.is_empty() {
        extra_files = nul_list(&repo.run(&[
            "diff-tree",
            "-r",
            "--no-renames",
            "--name-only",
            "--diff-filter=A",
            "-z",
            &snap_tree,
            &pre_tree,
        ])?);
        if delete_extra {
            for f in std::mem::take(&mut extra_files) {
                let abs = repo.top.join(&f);
                if std::fs::remove_file(&abs).is_ok() {
                    // Tidy now-empty parent directories (never above the worktree).
                    let mut dir = abs.parent();
                    while let Some(d) = dir {
                        if d == repo.top || std::fs::remove_dir(d).is_err() {
                            break;
                        }
                        dir = d.parent();
                    }
                    deleted_files.push(f);
                } else {
                    extra_files.push(f);
                }
            }
        }
    }

    Ok(RestoreResult {
        restored: snap,
        pre_restore: pre.snapshot,
        pre_restore_created: pre.created,
        restored_files,
        extra_files,
        deleted_files,
    })
}
