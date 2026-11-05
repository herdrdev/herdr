//! Local per-agent snapshots (F11a).
//!
//! A snapshot is a git commit that records the full (non-ignored) state of an agent's
//! worktree, stored under the hidden ref `refs/shepherd/snapshots/<agent>/<n>`. Objects are
//! built with a temporary index, so the user's index, branch, stash and commits are never
//! touched. Everything is local: only the `git` CLI is used, nothing leaves the machine.
//!
//! Refs live in the shared `refs/` of the repository, so they travel with the repo (and
//! with `git clone --mirror`), are never collected by `git gc`, and are **not** pushed by a
//! normal `git push` (only `git push origin 'refs/shepherd/*'` would publish them).
//!
//! Design note: snapshots are *not* chained through git parents. The previous snapshot of
//! the same agent is recorded as `prev:` metadata instead, so that deleting/pruning a
//! snapshot really releases its objects (a parent chain would keep every pruned ancestor
//! reachable from the newest snapshot forever).

pub mod capture;
pub mod diff;
pub mod limits;
pub mod render;
pub mod restore;

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

pub use capture::{capture, CaptureResult};
pub use diff::{diff, show, DiffFile, DiffResult, ShowResult};
pub use limits::SnapshotLimits;
pub use restore::{restore, RestoreResult};

pub(crate) const REF_PREFIX: &str = "refs/shepherd/snapshots/";
/// The well-known id of git's empty tree (valid for SHA-1 repositories).
pub(crate) const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Why a snapshot was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotReason {
    Manual,
    Idle,
    Done,
    PreRestore,
    PreClean,
    PreKill,
}

impl SnapshotReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::PreRestore => "pre-restore",
            Self::PreClean => "pre-clean",
            Self::PreKill => "pre-kill",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "manual" => Self::Manual,
            "idle" => Self::Idle,
            "done" => Self::Done,
            "pre-restore" => Self::PreRestore,
            "pre-clean" => Self::PreClean,
            "pre-kill" => Self::PreKill,
            other => {
                return Err(format!(
                    "unknown snapshot reason {other:?} (expected manual|idle|done|pre-restore|pre-clean|pre-kill)"
                ))
            }
        })
    }
}

/// A file left out of a capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OmittedFile {
    pub path: String,
    pub reason: String,
    pub size: u64,
}

/// One snapshot (metadata is stored in the commit message).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SnapshotInfo {
    /// `<agent>/<n>`.
    pub id: String,
    pub agent: String,
    pub n: u64,
    pub commit: String,
    pub reason: SnapshotReason,
    /// Unix seconds.
    pub created: u64,
    /// Worktree the snapshot was taken in.
    pub worktree: String,
    /// Id of the previous snapshot of the same agent, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    /// Files changed, and lines added/removed, relative to `prev` (automatic, local summary).
    pub files: u32,
    pub added: u64,
    pub removed: u64,
    /// Files left out (too large). Capped at 100 entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<OmittedFile>,
}

/// An opened git worktree.
pub(crate) struct Repo {
    pub top: PathBuf,
    pub git_dir: PathBuf,
}

impl Repo {
    /// Resolve the worktree that contains `root`.
    pub fn open(root: &Path) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("not a directory: {}", root.display()));
        }
        let probe = Repo {
            top: root.to_path_buf(),
            git_dir: PathBuf::new(),
        };
        let out = probe
            .run(&["rev-parse", "--show-toplevel", "--absolute-git-dir"])
            .map_err(|_| format!("not a git worktree: {}", root.display()))?;
        let text = String::from_utf8_lossy(&out);
        let mut lines = text.lines();
        let (Some(top), Some(git_dir)) = (lines.next(), lines.next()) else {
            return Err(format!("not a git worktree: {}", root.display()));
        };
        Ok(Repo {
            top: PathBuf::from(top),
            git_dir: PathBuf::from(git_dir),
        })
    }

    /// Refuse to touch a worktree that is in the middle of a merge, rebase, etc.
    pub fn ensure_idle(&self) -> Result<(), String> {
        for (name, what) in [
            ("MERGE_HEAD", "merge"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase/am"),
        ] {
            if self.git_dir.join(name).exists() {
                return Err(format!(
                    "a {what} is in progress in {}; finish or abort it first",
                    self.top.display()
                ));
            }
        }
        Ok(())
    }

    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        self.run_with(&[], args, None)
    }

    pub fn text(&self, args: &[&str]) -> Result<String, String> {
        Ok(String::from_utf8_lossy(&self.run(args)?).trim().to_owned())
    }

    /// Run git in the worktree with extra environment and optional stdin.
    pub fn run_with(
        &self,
        env: &[(&str, &OsStr)],
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.top)
            .args(["-c", "core.quotepath=off", "-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_COMMON_DIR",
        ] {
            cmd.env_remove(var);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| format!("failed to run git: {e}"))?;
        let writer = stdin.map(|bytes| {
            let mut pipe = child.stdin.take();
            let bytes = bytes.to_vec();
            std::thread::spawn(move || {
                if let Some(p) = pipe.as_mut() {
                    let _ = p.write_all(&bytes);
                }
            })
        });
        let out = child
            .wait_with_output()
            .map_err(|e| format!("failed to run git: {e}"))?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// A scratch index inside the git dir, removed on drop. Never the user's index.
pub(crate) struct TempIndex {
    pub path: PathBuf,
}

impl TempIndex {
    /// Create a scratch index, seeded with a copy of the real index when `seed` is true
    /// (keeps tracked-but-ignored files and reuses the stat cache).
    pub fn new(repo: &Repo, seed: bool) -> Result<Self, String> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = repo.git_dir.join(format!(
            "shepherd-snapshot-{}-{n}.index",
            std::process::id()
        ));
        let real = repo.git_dir.join("index");
        if seed && real.is_file() {
            std::fs::copy(&real, &path).map_err(|e| format!("cannot copy index: {e}"))?;
        }
        Ok(Self { path })
    }

    pub fn env(&self) -> [(&'static str, &OsStr); 1] {
        [("GIT_INDEX_FILE", self.path.as_os_str())]
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Agent names become ref components: keep them boring.
pub(crate) fn validate_agent(agent: &str) -> Result<(), String> {
    let ok = !agent.is_empty()
        && agent
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !agent.starts_with(['.', '-'])
        && !agent.ends_with('.')
        && !agent.ends_with(".lock")
        && !agent.contains("..");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid agent name {agent:?} (use letters, digits, '.', '_' and '-')"
        ))
    }
}

pub(crate) fn make_id(agent: &str, n: u64) -> String {
    format!("{agent}/{n}")
}

pub(crate) fn ref_name(agent: &str, n: u64) -> String {
    format!("{REF_PREFIX}{agent}/{n}")
}

/// Parse `<agent>/<n>`.
pub fn parse_id(id: &str) -> Result<(String, u64), String> {
    let bad = || format!("invalid snapshot id {id:?} (expected <agent>/<n>)");
    let (agent, n) = id.rsplit_once('/').ok_or_else(bad)?;
    validate_agent(agent).map_err(|_| bad())?;
    let n = n.parse::<u64>().map_err(|_| bad())?;
    Ok((agent.to_owned(), n))
}

fn parse_commit(refname: &str, commit: &str, message: &str) -> Option<SnapshotInfo> {
    let rest = refname.strip_prefix(REF_PREFIX)?;
    let (agent, n) = rest.rsplit_once('/')?;
    let n = n.parse::<u64>().ok()?;
    let mut info = SnapshotInfo {
        id: make_id(agent, n),
        agent: agent.to_owned(),
        n,
        commit: commit.to_owned(),
        reason: SnapshotReason::Manual,
        created: 0,
        worktree: String::new(),
        prev: None,
        files: 0,
        added: 0,
        removed: 0,
        omitted: Vec::new(),
    };
    for line in message.lines() {
        let Some((k, v)) = line.split_once(": ") else {
            continue;
        };
        match k {
            "reason" => info.reason = SnapshotReason::parse(v).unwrap_or(SnapshotReason::Manual),
            "created" => info.created = v.parse().unwrap_or(0),
            "worktree" => info.worktree = serde_json::from_str(v).unwrap_or_default(),
            "prev" if v != "-" => info.prev = Some(v.to_owned()),
            "files" => info.files = v.parse().unwrap_or(0),
            "added" => info.added = v.parse().unwrap_or(0),
            "removed" => info.removed = v.parse().unwrap_or(0),
            "omitted" => info.omitted = serde_json::from_str(v).unwrap_or_default(),
            _ => {}
        }
    }
    Some(info)
}

pub(crate) fn render_message(info: &SnapshotInfo) -> String {
    format!(
        "shepherd snapshot {}\n\nagent: {}\nn: {}\nreason: {}\ncreated: {}\nworktree: {}\nprev: {}\nfiles: {}\nadded: {}\nremoved: {}\nomitted: {}\n",
        info.id,
        info.agent,
        info.n,
        info.reason.as_str(),
        info.created,
        serde_json::to_string(&info.worktree).unwrap_or_else(|_| "\"\"".into()),
        info.prev.as_deref().unwrap_or("-"),
        info.files,
        info.added,
        info.removed,
        serde_json::to_string(&info.omitted).unwrap_or_else(|_| "[]".into()),
    )
}

/// All snapshots of the repository (or of one agent), ordered by agent then `n`.
pub(crate) fn read_all(repo: &Repo, agent: Option<&str>) -> Result<Vec<SnapshotInfo>, String> {
    let pattern = match agent {
        Some(a) => format!("{REF_PREFIX}{a}/"),
        None => REF_PREFIX.to_owned(),
    };
    let out = repo.run(&[
        "for-each-ref",
        "--format=%(refname)%1f%(objectname)%1f%(contents)%1e",
        &pattern,
    ])?;
    let text = String::from_utf8_lossy(&out);
    let mut all: Vec<SnapshotInfo> = text
        .split('\u{1e}')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            let mut parts = rec.splitn(3, '\u{1f}');
            parse_commit(parts.next()?, parts.next()?, parts.next()?)
        })
        .collect();
    all.sort_by(|a, b| a.agent.cmp(&b.agent).then(a.n.cmp(&b.n)));
    Ok(all)
}

pub(crate) fn read_one(repo: &Repo, id: &str) -> Result<SnapshotInfo, String> {
    let (agent, n) = parse_id(id)?;
    read_all(repo, Some(&agent))?
        .into_iter()
        .find(|s| s.n == n)
        .ok_or_else(|| format!("snapshot not found: {id}"))
}

pub(crate) fn tree_of(repo: &Repo, commit: &str) -> Result<String, String> {
    repo.text(&["rev-parse", &format!("{commit}^{{tree}}")])
}

/// List snapshots. With `agent`: that agent's; otherwise those taken in this worktree, or
/// every snapshot of the repository when `all` is set.
pub fn list(root: &Path, agent: Option<&str>, all: bool) -> Result<Vec<SnapshotInfo>, String> {
    let repo = Repo::open(root)?;
    if let Some(a) = agent {
        validate_agent(a)?;
    }
    let mut snaps = read_all(&repo, agent)?;
    if agent.is_none() && !all {
        let here = repo.top.to_string_lossy();
        snaps.retain(|s| s.worktree == here);
    }
    Ok(snaps)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeleteResult {
    pub deleted: String,
}

/// Delete one snapshot ref (its objects are released by the next `git gc`).
pub fn delete(root: &Path, id: &str) -> Result<DeleteResult, String> {
    let repo = Repo::open(root)?;
    let snap = read_one(&repo, id)?;
    repo.run(&["update-ref", "-d", &ref_name(&snap.agent, snap.n)])?;
    Ok(DeleteResult { deleted: snap.id })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneResult {
    pub pruned: Vec<String>,
    pub kept: usize,
}

/// Keep the newest `keep` snapshots of each agent (or of `agent` only); delete the rest.
pub fn prune(
    root: &Path,
    agent: Option<&str>,
    keep: Option<usize>,
    limits: &SnapshotLimits,
) -> Result<PruneResult, String> {
    let repo = Repo::open(root)?;
    if let Some(a) = agent {
        validate_agent(a)?;
    }
    let keep = keep.unwrap_or(limits.max_per_agent);
    prune_repo(&repo, agent, keep)
}

pub(crate) fn prune_repo(
    repo: &Repo,
    agent: Option<&str>,
    keep: usize,
) -> Result<PruneResult, String> {
    let snaps = read_all(repo, agent)?;
    let mut pruned = Vec::new();
    let mut kept = 0;
    let mut by_agent: Vec<(&str, Vec<&SnapshotInfo>)> = Vec::new();
    for s in &snaps {
        match by_agent.last_mut() {
            Some((a, v)) if *a == s.agent => v.push(s),
            _ => by_agent.push((&s.agent, vec![s])),
        }
    }
    for (_, v) in by_agent {
        let cut = v.len().saturating_sub(keep);
        for s in &v[..cut] {
            repo.run(&["update-ref", "-d", &ref_name(&s.agent, s.n)])?;
            pruned.push(s.id.clone());
        }
        kept += v.len() - cut;
    }
    Ok(PruneResult { pruned, kept })
}

/// `YYYY-MM-DD HH:MM:SS` (UTC) for unix seconds.
pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests;
