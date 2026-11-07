//! Git provenance for bundle folders, and the import-side clone.
//!
//! All git access is best-effort and goes through the `git` executable so it
//! behaves the same on Linux, macOS, and Windows. A missing `git` or a
//! non-repository folder simply yields no provenance.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::redact_remote_url;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GitInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) toplevel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<String>,
    /// `git status --porcelain` lines, capped. Uncommitted changes are
    /// listed for the user, never packed into the bundle.
    #[serde(default)]
    pub(crate) dirty: Vec<String>,
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn provenance(cwd: &Path) -> Option<GitInfo> {
    let toplevel = git_output(cwd, &["rev-parse", "--show-toplevel"])?;
    let dirty = git_output(cwd, &["status", "--porcelain"])
        .map(|out| {
            out.lines()
                .filter(|line| !line.is_empty())
                .take(200)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(GitInfo {
        toplevel: Some(toplevel),
        head: git_output(cwd, &["rev-parse", "HEAD"]),
        branch: git_output(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]),
        origin: git_output(cwd, &["remote", "get-url", "origin"])
            .map(|url| redact_remote_url(&url)),
        dirty,
    })
}

/// Clone `origin` into `target` and check out the recorded commit. Returns a
/// human-readable failure instead of panicking; import reports it and moves
/// on so one unreachable remote cannot block the whole restore.
pub(crate) fn clone_at(origin: &str, target: &Path, head: Option<&str>) -> Result<(), String> {
    let status = std::process::Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg(origin)
        .arg(target)
        .status()
        .map_err(|error| format!("could not run git clone: {error}"))?;
    if !status.success() {
        return Err(format!("git clone {origin} failed"));
    }
    if let Some(head) = head.filter(|head| !head.is_empty()) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(target)
            .args(["checkout", "--quiet", head])
            .status()
            .map_err(|error| format!("could not run git checkout: {error}"))?;
        if !status.success() {
            return Err(format!(
                "cloned {} but checking out {head} failed; the default branch is checked out",
                target.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_of_non_repository_is_none() {
        let dir = super::super::test_temp_dir("git-info");
        assert_eq!(provenance(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
