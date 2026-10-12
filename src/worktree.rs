use std::ffi::OsString;
use std::path::{Path, PathBuf};

const DEFAULT_WORKTREE_PREFIX: &str = "worktree";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_prunable: bool,
}

pub(crate) fn generated_branch_slug(seed: u64) -> String {
    let adjectives = [
        "brave", "calm", "clear", "green", "lucky", "quiet", "rapid", "silver",
    ];
    let nouns = [
        "river", "cloud", "field", "forest", "harbor", "meadow", "stone", "valley",
    ];
    let adjective = adjectives[(seed as usize) % adjectives.len()];
    let noun = nouns[((seed / adjectives.len() as u64) as usize) % nouns.len()];
    format!("{DEFAULT_WORKTREE_PREFIX}/{adjective}-{noun}-{seed:016x}")
}

/// One server-owned create operation. Automatic names must never opt in to
/// existing-branch checkout, even when another process wins the same name.
#[derive(Debug)]
pub(crate) struct WorktreeCreation {
    pub(crate) branch: String,
    pub(crate) path: PathBuf,
    base: String,
    trust_repository: bool,
    automatic: Option<AutomaticBranch>,
}

#[derive(Debug)]
struct AutomaticBranch {
    seed: u64,
    remaining: usize,
    checkout_directory: Option<PathBuf>,
    base_resolved: bool,
}

#[derive(Debug)]
pub(crate) enum WorktreeCreateError {
    Collision(String),
    Failed(String),
}

impl WorktreeCreateError {
    pub(crate) fn message(self) -> String {
        match self {
            Self::Collision(message) | Self::Failed(message) => message,
        }
    }
}

impl From<String> for WorktreeCreateError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl WorktreeCreation {
    pub(crate) const MAX_ATTEMPTS: usize = 64;

    pub(crate) fn new(
        branch: Option<String>,
        base: String,
        path: Option<PathBuf>,
        checkout_root: PathBuf,
        repo_name: &str,
        trust_repository: bool,
        seed: u64,
    ) -> Self {
        let automatic = branch.is_none().then(|| AutomaticBranch {
            seed,
            remaining: Self::MAX_ATTEMPTS - 1,
            checkout_directory: path.is_none().then(|| checkout_root.join(repo_name)),
            base_resolved: false,
        });
        let branch = branch.unwrap_or_else(|| generated_branch_slug(seed));
        let path =
            path.unwrap_or_else(|| default_checkout_path(&checkout_root, repo_name, &branch));
        Self {
            branch,
            path,
            base,
            trust_repository,
            automatic,
        }
    }

    pub(crate) fn has_automatic_path(&self) -> bool {
        self.automatic
            .as_ref()
            .is_some_and(|auto| auto.checkout_directory.is_some())
    }

    pub(crate) fn next_candidate(&mut self) -> bool {
        let Some(auto) = self.automatic.as_mut() else {
            return false;
        };
        if auto.remaining == 0 {
            return false;
        }
        auto.remaining -= 1;
        auto.seed = auto.seed.wrapping_add(1);
        self.branch = generated_branch_slug(auto.seed);
        if let Some(directory) = &auto.checkout_directory {
            self.path = directory.join(branch_to_path_slug(&self.branch));
        }
        true
    }

    pub(crate) fn run(&mut self, source: &Path) -> Result<(), WorktreeCreateError> {
        let Some(automatic) = self.automatic.as_mut() else {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            return run_worktree_add_command(
                source,
                &self.path,
                &self.branch,
                &self.base,
                self.trust_repository,
            )
            .map_err(Into::into);
        };

        // Resolve once, before any attempt. A retry must not follow a moving HEAD.
        if !automatic.base_resolved {
            let output = repository_git_command(source, self.trust_repository)
                .args(["rev-parse", "--verify", "--end-of-options"])
                .arg(format!("{}^{{commit}}", self.base))
                .output()
                .map_err(|err| err.to_string())?;
            if !output.status.success() {
                return Err(WorktreeCreateError::Failed(format!(
                    "could not resolve worktree base '{}': {}",
                    self.base,
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            self.base = String::from_utf8_lossy(&output.stdout).trim().to_string();
            automatic.base_resolved = true;
        }

        // Check before add: Git can create a branch before rejecting the path.
        // Include empty directories, dangling symlinks and stale registrations.
        let occupied = match std::fs::symlink_metadata(&self.path) {
            Ok(_) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => return Err(WorktreeCreateError::Failed(err.to_string())),
        } || worktree_list_contains_path(source, &self.path, self.trust_repository)?;
        if occupied {
            let message = format!(
                "worktree checkout '{}' already exists or is registered",
                self.path.display()
            );
            return Err(if self.has_automatic_path() {
                WorktreeCreateError::Collision(message)
            } else {
                WorktreeCreateError::Failed(message)
            });
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let command = build_worktree_add_new_branch_command(
            source,
            &self.path,
            &self.branch,
            &self.base,
            self.trust_repository,
        );
        run_worktree_command(&command).map_err(|message| {
            // Only this definitive pre-creation failure permits another name.
            // A path/checkout failure may already have created our branch: do
            // not retry or delete anything on that ambiguous outcome. Hooks may
            // also print this diagnostic after creation, so require the checkout
            // to remain absent and unregistered before treating it as a conflict.
            let collision = format!("fatal: a branch named '{}' already exists", self.branch);
            if message.lines().any(|line| line == collision)
                && matches!(std::fs::symlink_metadata(&self.path),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound)
                && matches!(
                    worktree_list_contains_path(source, &self.path, self.trust_repository),
                    Ok(false)
                )
            {
                WorktreeCreateError::Collision(message)
            } else {
                WorktreeCreateError::Failed(format!(
                    "{message}; attempted branch '{}' at '{}'; inspect these before retrying",
                    self.branch,
                    self.path.display(),
                ))
            }
        })
    }
}

pub(crate) fn branch_to_path_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in branch.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        DEFAULT_WORKTREE_PREFIX.to_string()
    } else {
        trimmed
    }
}

pub(crate) fn expand_tilde_path(path: &str) -> PathBuf {
    expand_tilde_path_from_env(path, cfg!(windows), |key| std::env::var_os(key))
}

fn expand_tilde_path_from_env(
    path: &str,
    is_windows: bool,
    env: impl Fn(&str) -> Option<OsString> + Copy,
) -> PathBuf {
    if path == "~" {
        return home_dir_from_env(is_windows, env).unwrap_or_else(|_| PathBuf::from(path));
    }

    let tilde_rest = path.strip_prefix("~/").or_else(|| {
        if is_windows {
            path.strip_prefix("~\\")
        } else {
            None
        }
    });
    if let Some(rest) = tilde_rest {
        return home_dir_from_env(is_windows, env)
            .map(|home| join_tilde_rest(home, rest, is_windows))
            .unwrap_or_else(|_| PathBuf::from(path));
    }

    PathBuf::from(path)
}

fn join_tilde_rest(home: PathBuf, rest: &str, is_windows: bool) -> PathBuf {
    if is_windows {
        rest.split(['/', '\\'])
            .filter(|component| !component.is_empty())
            .fold(home, |path, component| path.join(component))
    } else {
        home.join(rest)
    }
}

fn home_dir_from_env(
    is_windows: bool,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, ()> {
    if !is_windows {
        return env("HOME").map(PathBuf::from).ok_or(());
    }

    if let Some(path) = usable_home_path(env("USERPROFILE")) {
        return Ok(path);
    }
    if let (Some(drive), Some(path)) = (
        usable_home_component(env("HOMEDRIVE")),
        usable_home_component(env("HOMEPATH")),
    ) {
        let path = path.to_string_lossy();
        if !path.starts_with(['\\', '/']) {
            return usable_home_path(env("HOME")).ok_or(());
        }
        let combined = format!("{}{}", drive.to_string_lossy(), path);
        if let Some(path) = usable_home_path(Some(OsString::from(combined))) {
            return Ok(path);
        }
    }

    usable_home_path(env("HOME")).ok_or(())
}

fn usable_home_path(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if value.is_empty() || value == "~" {
        return None;
    }
    Some(PathBuf::from(value))
}

fn usable_home_component(value: Option<OsString>) -> Option<OsString> {
    let value = value?;
    if value.is_empty() || value == "~" {
        return None;
    }
    Some(value)
}

pub(crate) fn expand_tilde_absolute_path(path: &str) -> PathBuf {
    let path = expand_tilde_path(path);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

pub(crate) fn canonical_or_original(path: &Path) -> PathBuf {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        let candidate = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        match std::fs::canonicalize(candidate) {
            Ok(mut canonical) => {
                // A missing checkout still has the identity of its resolved
                // parent. Git records that path, including symlinks such as
                // /tmp -> /private/tmp on macOS, before or after creation.
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return canonical;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return path.to_path_buf(),
        }
        let Some(name) = ancestor.file_name() else {
            return path.to_path_buf();
        };
        let Some(parent) = ancestor.parent() else {
            return path.to_path_buf();
        };
        missing.push(name.to_os_string());
        ancestor = parent;
    }
}

fn repository_git_command(repo_root: &Path, trust_repository: bool) -> std::process::Command {
    let mut command = crate::noninteractive_process::command("git");
    command.args(repository_git_args(repo_root, trust_repository));
    command
}

fn repository_git_args(repo_root: &Path, trust_repository: bool) -> Vec<String> {
    let mut args = Vec::new();
    if trust_repository {
        args.push("-c".to_string());
        args.push(format!("safe.directory={}", repo_root.display()));
    }
    args.push("-C".to_string());
    args.push(repo_root.display().to_string());
    args
}

pub(crate) fn default_checkout_path(root: &Path, repo_name: &str, branch: &str) -> PathBuf {
    root.join(repo_name).join(branch_to_path_slug(branch))
}

pub(crate) fn build_worktree_remove_command(
    repo_root: &Path,
    path: &Path,
    force: bool,
    trust_repository: bool,
) -> WorktreeCommand {
    let mut args = repository_git_args(repo_root, trust_repository);
    args.extend(["worktree".to_string(), "remove".to_string()]);
    if force {
        args.push("--force".to_string());
    }
    args.push(path.display().to_string());

    WorktreeCommand {
        program: "git".to_string(),
        args,
    }
}

pub(crate) fn is_dirty_worktree_remove_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    (lower.contains("contains modified or untracked files")
        && lower.contains("use --force to delete it"))
        || lower.contains("working trees containing submodules cannot be moved or removed")
}

pub(crate) fn is_not_working_tree_remove_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("is not a working tree") || lower.contains("is not a worktree")
}

#[cfg(windows)]
pub(crate) fn worktree_dirty_remove_message(path: &Path) -> String {
    format!(
        "fatal: '{}' contains modified or untracked files, use --force to delete it",
        path.display()
    )
}

#[cfg(any(windows, test))]
pub(crate) fn checkout_has_dirty_files(
    path: &Path,
    trust_repository: bool,
) -> Result<bool, String> {
    let output = repository_git_command(path, trust_repository)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        return Ok(!output.stdout.is_empty());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        Err(stderr)
    } else if !stdout.is_empty() {
        Err(stdout)
    } else {
        Err(format!("git status failed with status {}", output.status))
    }
}

pub(crate) fn build_worktree_add_new_branch_command(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    trust_repository: bool,
) -> WorktreeCommand {
    let mut args = repository_git_args(repo_root, trust_repository);
    args.extend([
        "worktree".to_string(),
        "add".to_string(),
        "-b".to_string(),
        branch.to_string(),
        path.display().to_string(),
        base.to_string(),
    ]);
    WorktreeCommand {
        program: "git".to_string(),
        args,
    }
}

pub(crate) fn build_worktree_add_existing_branch_command(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    trust_repository: bool,
) -> WorktreeCommand {
    let mut args = repository_git_args(repo_root, trust_repository);
    args.extend([
        "worktree".to_string(),
        "add".to_string(),
        path.display().to_string(),
        branch.to_string(),
    ]);
    WorktreeCommand {
        program: "git".to_string(),
        args,
    }
}

fn local_branch_exists(
    repo_root: &Path,
    branch: &str,
    trust_repository: bool,
) -> Result<bool, String> {
    let output = repository_git_command(repo_root, trust_repository)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        Err(stderr)
    } else if !stdout.is_empty() {
        Err(stdout)
    } else {
        Err(format!("git show-ref failed with status {}", output.status))
    }
}

pub(crate) fn run_worktree_add_command(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    trust_repository: bool,
) -> Result<(), String> {
    let command = if local_branch_exists(repo_root, branch, trust_repository)? {
        build_worktree_add_existing_branch_command(repo_root, path, branch, trust_repository)
    } else {
        build_worktree_add_new_branch_command(repo_root, path, branch, base, trust_repository)
    };
    run_worktree_command(&command)
}

pub(crate) fn run_worktree_command(command: &WorktreeCommand) -> Result<(), String> {
    let output = crate::noninteractive_process::command(&command.program)
        // Removal errors are classified by Git's English diagnostics.
        .env("LC_ALL", "C")
        .args(&command.args)
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(if message.is_empty() {
        format!("{} failed with status {}", command.program, output.status)
    } else {
        message
    })
}

pub(crate) fn run_worktree_remove_command_with_recovery(
    command: &WorktreeCommand,
    repo_root: &Path,
    path: &Path,
    force: bool,
    trust_repository: bool,
) -> Result<(), String> {
    match run_worktree_command(command) {
        Ok(()) => Ok(()),
        Err(err) if force && is_not_working_tree_remove_error(&err) => {
            if worktree_list_contains_path(repo_root, path, trust_repository)? {
                return Err(err);
            }
            if path.exists() {
                if !leftover_worktree_checkout_matches_repo(repo_root, path, trust_repository) {
                    return Err(err);
                }
                std::fs::remove_dir_all(path).map_err(|remove_err| {
                    format!(
                        "{err}; failed to remove leftover checkout {}: {remove_err}",
                        path.display()
                    )
                })?;
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn leftover_worktree_checkout_matches_repo(
    repo_root: &Path,
    path: &Path,
    trust_repository: bool,
) -> bool {
    let git_file = path.join(".git");
    let Ok(content) = std::fs::read_to_string(&git_file) else {
        return false;
    };
    let Some(gitdir) = content.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let gitdir = PathBuf::from(gitdir.trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        path.join(gitdir)
    };
    let Some(worktrees_dir) = git_common_worktrees_dir(repo_root, trust_repository) else {
        return false;
    };
    canonical_or_original(&gitdir).starts_with(canonical_or_original(&worktrees_dir))
}

fn git_common_worktrees_dir(repo_root: &Path, trust_repository: bool) -> Option<PathBuf> {
    let output = repository_git_command(repo_root, trust_repository)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let common_dir = stdout.trim();
    if common_dir.is_empty() {
        None
    } else {
        let common_dir = PathBuf::from(common_dir);
        let common_dir = if common_dir.is_absolute() {
            common_dir
        } else {
            repo_root.join(common_dir)
        };
        Some(common_dir.join("worktrees"))
    }
}

pub(crate) fn parse_worktree_list_porcelain(output: &str) -> Vec<ExistingWorktree> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = None;
    let mut is_bare = false;
    let mut is_detached = false;
    let mut is_prunable = false;

    let finish = |entries: &mut Vec<ExistingWorktree>,
                  path: &mut Option<PathBuf>,
                  branch: &mut Option<String>,
                  is_bare: &mut bool,
                  is_detached: &mut bool,
                  is_prunable: &mut bool| {
        if let Some(path) = path.take() {
            entries.push(ExistingWorktree {
                path,
                branch: branch.take(),
                is_bare: *is_bare,
                is_detached: *is_detached,
                is_prunable: *is_prunable,
            });
        }
        *is_bare = false;
        *is_detached = false;
        *is_prunable = false;
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            finish(
                &mut entries,
                &mut path,
                &mut branch,
                &mut is_bare,
                &mut is_detached,
                &mut is_prunable,
            );
            continue;
        }
        if let Some(value) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("branch ") {
            branch = Some(
                value
                    .strip_prefix("refs/heads/")
                    .unwrap_or(value)
                    .to_string(),
            );
        } else if line == "detached" {
            is_detached = true;
        } else if line == "bare" {
            is_bare = true;
        } else if line.starts_with("prunable") {
            is_prunable = true;
        }
    }

    finish(
        &mut entries,
        &mut path,
        &mut branch,
        &mut is_bare,
        &mut is_detached,
        &mut is_prunable,
    );
    entries
}

pub(crate) fn list_existing_worktrees(
    repo_root: &Path,
    trust_repository: bool,
) -> Result<Vec<ExistingWorktree>, String> {
    #[cfg(test)]
    test_list_gate::wait(repo_root);
    let output = repository_git_command(repo_root, trust_repository)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Ok(parse_worktree_list_porcelain(&stdout));
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("git worktree list failed with status {}", output.status)
    } else {
        stderr
    })
}

#[cfg(test)]
pub(crate) mod test_list_gate {
    use std::path::{Path, PathBuf};
    use std::sync::{mpsc, Mutex};

    struct Gate {
        path: PathBuf,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    static GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());

    pub(crate) fn block(path: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, entry_rx) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        GATES.lock().unwrap().push(Gate {
            path: super::canonical_or_original(path),
            entered,
            release,
        });
        (entry_rx, release_tx)
    }

    pub(super) fn wait(path: &Path) {
        let gate = {
            let mut gates = GATES.lock().unwrap();
            gates
                .iter()
                .position(|gate| gate.path == super::canonical_or_original(path))
                .map(|index| gates.remove(index))
        };
        if let Some(gate) = gate {
            gate.entered.send(()).unwrap();
            gate.release
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("test must release worktree discovery without blocking the server loop");
        }
    }
}

fn worktree_list_contains_path(
    repo_root: &Path,
    path: &Path,
    trust_repository: bool,
) -> Result<bool, String> {
    let expected = canonical_or_original(path);
    Ok(list_existing_worktrees(repo_root, trust_repository)?
        .into_iter()
        .any(|entry| canonical_or_original(&entry.path) == expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "git command failed: git -C {} {}",
            repo.display(),
            args.join(" ")
        );
    }

    fn create_committed_repo(name: &str) -> PathBuf {
        let repo = unique_temp_path(name);
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "--quiet"]);
        run_git(&repo, &["config", "user.email", "herdr@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Herdr Test"]);
        std::fs::write(repo.join("README.md"), "test\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "--quiet", "-m", "initial"]);
        repo
    }

    fn automatic_creation(root: &Path, seed: u64) -> WorktreeCreation {
        WorktreeCreation::new(
            None,
            "HEAD".into(),
            None,
            root.to_path_buf(),
            "",
            false,
            seed,
        )
    }

    fn git_output(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn automatic_worktree_collision_pins_base_and_preserves_old_branch() {
        let repo = create_committed_repo("auto-base-pin");
        let root = unique_temp_path("auto-base-pin-checkouts");
        let mut creation = automatic_creation(&root, 7);
        let old = git_output(&repo, &["rev-parse", "HEAD"]);
        run_git(&repo, &["branch", &creation.branch]);
        let retained_branch = creation.branch.clone();
        std::fs::write(repo.join("README.md"), "requested\n").unwrap();
        run_git(&repo, &["commit", "--quiet", "-am", "requested source"]);
        let expected = git_output(&repo, &["rev-parse", "HEAD"]);
        assert!(matches!(
            creation.run(&repo),
            Err(WorktreeCreateError::Collision(_))
        ));
        std::fs::write(repo.join("README.md"), "moved again\n").unwrap();
        run_git(
            &repo,
            &["commit", "--quiet", "-am", "source moved during retry"],
        );
        assert!(creation.next_candidate());
        creation.run(&repo).unwrap();
        assert_eq!(git_output(&creation.path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(git_output(&repo, &["rev-parse", &retained_branch]), old);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn automatic_worktree_skips_occupied_and_registered_paths_before_creating_branch() {
        for registered in [false, true] {
            let repo = create_committed_repo("auto-occupied");
            let root = unique_temp_path("auto-occupied-checkouts");
            let mut creation = automatic_creation(&root, 10);
            if registered {
                run_git(
                    &repo,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        creation.path.to_str().unwrap(),
                        "HEAD",
                    ],
                );
                std::fs::remove_dir_all(&creation.path).unwrap();
            } else {
                std::fs::create_dir_all(&creation.path).unwrap();
            }
            assert!(matches!(
                creation.run(&repo),
                Err(WorktreeCreateError::Collision(_))
            ));
            assert!(!local_branch_exists(&repo, &creation.branch, false).unwrap());
            let original = creation.path.clone();
            assert!(creation.next_candidate());
            creation.run(&repo).unwrap();
            assert_ne!(creation.path, original);
            let _ = std::fs::remove_dir_all(root);
            let _ = std::fs::remove_dir_all(repo);
        }
    }

    #[cfg(unix)]
    #[test]
    fn automatic_worktree_skips_registered_missing_checkout_under_symlink_parent() {
        let repo = create_committed_repo("auto-symlink-registration");
        let root = unique_temp_path("auto-symlink-checkouts");
        let actual = root.join("actual");
        let alias = root.join("alias");
        std::fs::create_dir_all(&actual).unwrap();
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        let mut creation = automatic_creation(&alias, 15);
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                creation.path.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::remove_dir_all(&creation.path).unwrap();
        let skipped_branch = creation.branch.clone();
        assert!(matches!(
            creation.run(&repo),
            Err(WorktreeCreateError::Collision(_))
        ));
        assert!(!local_branch_exists(&repo, &skipped_branch, false).unwrap());
        assert!(creation.next_candidate());
        creation.run(&repo).unwrap();
        assert_eq!(
            git_output(&creation.path, &["rev-parse", "HEAD"]),
            git_output(&repo, &["rev-parse", "HEAD"])
        );
        assert!(!local_branch_exists(&repo, &skipped_branch, false).unwrap());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn automatic_worktree_explicit_path_is_not_replaced_on_collision() {
        let repo = create_committed_repo("auto-explicit-path");
        let path = unique_temp_path("auto-explicit-path-checkout");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("sentinel"), "keep").unwrap();
        let mut creation = WorktreeCreation::new(
            None,
            "HEAD".into(),
            Some(path.clone()),
            PathBuf::new(),
            "",
            false,
            42,
        );
        assert!(matches!(
            creation.run(&repo),
            Err(WorktreeCreateError::Failed(_))
        ));
        assert_eq!(creation.path, path);
        assert_eq!(
            std::fs::read_to_string(path.join("sentinel")).unwrap(),
            "keep"
        );
        assert!(!local_branch_exists(&repo, &creation.branch, false).unwrap());
        let _ = std::fs::remove_dir_all(path);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn automatic_worktree_invalid_base_is_terminal_and_creates_nothing() {
        let repo = create_committed_repo("auto-invalid-base");
        let root = unique_temp_path("auto-invalid-base-checkouts");
        let mut creation = automatic_creation(&root, 11);
        creation.base = "missing-ref".into();
        assert!(matches!(
            creation.run(&repo),
            Err(WorktreeCreateError::Failed(_))
        ));
        assert!(!root.exists());
        assert!(!local_branch_exists(&repo, &creation.branch, false).unwrap());
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn automatic_worktree_branch_race_retries_without_reusing_competing_branch() {
        let repo = create_committed_repo("auto-branch-race");
        let root = unique_temp_path("auto-branch-race-checkouts");
        let creation = automatic_creation(&root, 12);
        let competing_branch = creation.branch.clone();
        let (entered, release) = test_list_gate::block(&repo);
        let worker_repo = repo.clone();
        let worker = std::thread::spawn(move || {
            let mut creation = creation;
            let result = creation.run(&worker_repo);
            (creation, result)
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        run_git(&repo, &["branch", &competing_branch]);
        release.send(()).unwrap();
        let (mut creation, result) = worker.join().unwrap();
        assert!(matches!(result, Err(WorktreeCreateError::Collision(_))));
        assert!(creation.next_candidate());
        creation.run(&repo).unwrap();
        assert_ne!(creation.branch, competing_branch);
        assert!(local_branch_exists(&repo, &competing_branch, false).unwrap());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn automatic_worktree_path_race_is_terminal_and_preserves_partial_state() {
        let repo = create_committed_repo("auto-path-race");
        let root = unique_temp_path("auto-path-race-checkouts");
        let creation = automatic_creation(&root, 13);
        let path = creation.path.clone();
        let (entered, release) = test_list_gate::block(&repo);
        let worker_repo = repo.clone();
        let worker = std::thread::spawn(move || {
            let mut creation = creation;
            let result = creation.run(&worker_repo);
            (creation, result)
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("sentinel"), "another actor").unwrap();
        release.send(()).unwrap();
        let (creation, result) = worker.join().unwrap();
        let Err(WorktreeCreateError::Failed(message)) = result else {
            panic!("post-add path race must not be retried");
        };
        assert!(message.contains(&creation.branch));
        assert!(message.contains(path.to_str().unwrap()));
        assert!(local_branch_exists(&repo, &creation.branch, false).unwrap());
        assert_eq!(
            std::fs::read_to_string(path.join("sentinel")).unwrap(),
            "another actor"
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[cfg(unix)]
    #[test]
    fn automatic_worktree_hook_collision_message_after_creation_is_terminal() {
        use std::os::unix::fs::PermissionsExt;

        let repo = create_committed_repo("auto-hook-failure");
        let root = unique_temp_path("auto-hook-failure-checkouts");
        let mut creation = automatic_creation(&root, 14);
        let hook = repo.join(".git/hooks/post-checkout");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\nprintf \"fatal: a branch named '{}' already exists\\n\" >&2\nexit 1\n",
                creation.branch,
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let Err(WorktreeCreateError::Failed(message)) = creation.run(&repo) else {
            panic!("a post-checkout hook failure must not be retried as a name collision");
        };
        assert!(message.contains(&creation.branch));
        assert!(creation.path.join(".git").is_file());
        assert!(local_branch_exists(&repo, &creation.branch, false).unwrap());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn automatic_worktree_candidate_budget_is_bounded_and_uses_high_seed_bits() {
        let mut creation = automatic_creation(Path::new("/checkouts"), u64::MAX);
        let mut names = std::collections::HashSet::from([creation.branch.clone()]);
        for _ in 1..WorktreeCreation::MAX_ATTEMPTS {
            assert!(creation.next_candidate());
            assert!(names.insert(creation.branch.clone()));
        }
        assert!(!creation.next_candidate());
        assert_eq!(names.len(), WorktreeCreation::MAX_ATTEMPTS);
        assert_ne!(generated_branch_slug(0), generated_branch_slug(1 << 16));
    }

    #[test]
    fn trusted_repository_git_args_are_request_scoped() {
        assert_eq!(
            repository_git_args(Path::new("/repo/herdr"), false),
            ["-C", "/repo/herdr"]
        );
        assert_eq!(
            repository_git_args(Path::new("/repo/herdr"), true),
            ["-c", "safe.directory=/repo/herdr", "-C", "/repo/herdr",]
        );
    }

    #[test]
    fn generated_branch_slug_is_worktree_namespaced_and_stable() {
        assert_eq!(
            generated_branch_slug(0),
            "worktree/brave-river-0000000000000000"
        );
        assert_eq!(
            generated_branch_slug(9),
            "worktree/calm-cloud-0000000000000009"
        );
    }

    #[test]
    fn parses_git_worktree_list_porcelain() {
        let output = "\
worktree /repo/main
HEAD abc
branch refs/heads/main

worktree /repo/issue
HEAD def
branch refs/heads/worktree/issue

worktree /repo/detached
HEAD fed
detached
prunable stale

";

        assert_eq!(
            parse_worktree_list_porcelain(output),
            vec![
                ExistingWorktree {
                    path: PathBuf::from("/repo/main"),
                    branch: Some("main".into()),
                    is_bare: false,
                    is_detached: false,
                    is_prunable: false,
                },
                ExistingWorktree {
                    path: PathBuf::from("/repo/issue"),
                    branch: Some("worktree/issue".into()),
                    is_bare: false,
                    is_detached: false,
                    is_prunable: false,
                },
                ExistingWorktree {
                    path: PathBuf::from("/repo/detached"),
                    branch: None,
                    is_bare: false,
                    is_detached: true,
                    is_prunable: true,
                },
            ]
        );
    }

    #[test]
    fn branch_to_path_slug_makes_branch_safe_folder_name() {
        assert_eq!(
            branch_to_path_slug("worktree/brave-river"),
            "worktree-brave-river"
        );
        assert_eq!(
            branch_to_path_slug("issue/137 Worktree Spaces"),
            "issue-137-worktree-spaces"
        );
        assert_eq!(branch_to_path_slug("///"), "worktree");
    }

    #[test]
    fn expand_tilde_path_uses_home_when_available() {
        assert_eq!(
            expand_tilde_path_from_env("~/.herdr/worktrees", false, |key| match key {
                "HOME" => Some("/home/me".into()),
                _ => None,
            }),
            PathBuf::from("/home/me/.herdr/worktrees")
        );
        assert_eq!(
            expand_tilde_path_from_env("/tmp/worktrees", false, |_| None),
            PathBuf::from("/tmp/worktrees")
        );
    }

    #[test]
    fn home_dir_uses_windows_profile_before_literal_home() {
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOME" => Some("~".into()),
                "USERPROFILE" => Some(r"C:\Users\herdr".into()),
                _ => None,
            }),
            Ok(PathBuf::from(r"C:\Users\herdr"))
        );
    }

    #[test]
    fn home_dir_uses_windows_drive_and_path_when_profile_is_missing() {
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOMEDRIVE" => Some("C:".into()),
                "HOMEPATH" => Some(r"\Users\herdr".into()),
                _ => None,
            }),
            Ok(PathBuf::from(r"C:\Users\herdr"))
        );
    }

    #[test]
    fn home_dir_rejects_incomplete_windows_drive_and_path() {
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOMEDRIVE" => Some("C:".into()),
                "HOMEPATH" => Some("".into()),
                _ => None,
            }),
            Err(())
        );
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOMEDRIVE" => Some("C:".into()),
                "HOMEPATH" => Some("Users\\herdr".into()),
                _ => None,
            }),
            Err(())
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_tilde_expansion_keeps_windows_separator_literal() {
        assert_eq!(
            expand_tilde_path_from_env(r"~\.herdr\worktrees", false, |key| match key {
                "HOME" => Some("/home/me".into()),
                _ => None,
            }),
            PathBuf::from(r"~\.herdr\worktrees")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_tilde_expansion_normalizes_separators() {
        fn env(key: &str) -> Option<OsString> {
            match key {
                "HOME" => Some("~".into()),
                "USERPROFILE" => Some(r"C:\Users\herdr".into()),
                _ => None,
            }
        }

        let default_path = expand_tilde_path_from_env("~/.herdr/worktrees", true, env);
        assert_eq!(
            default_path,
            PathBuf::from(r"C:\Users\herdr\.herdr\worktrees")
        );
        assert_eq!(
            default_path.display().to_string(),
            r"C:\Users\herdr\.herdr\worktrees"
        );
        assert_eq!(
            expand_tilde_path_from_env(r"~\.herdr\worktrees", true, env),
            PathBuf::from(r"C:\Users\herdr\.herdr\worktrees")
        );
    }

    #[test]
    fn default_checkout_path_appends_repo_and_branch_slug() {
        assert_eq!(
            default_checkout_path(
                Path::new("/home/me/.herdr/worktrees"),
                "herdr",
                "worktree/brave-river",
            ),
            PathBuf::from("/home/me/.herdr/worktrees/herdr/worktree-brave-river")
        );
    }

    #[test]
    fn checkout_dirty_detection_reports_clean_and_dirty_worktrees() {
        let repo = create_committed_repo("worktree-dirty-detection-repo");
        let checkout = unique_temp_path("worktree-dirty-detection-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/dirty-detection",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        assert_eq!(checkout_has_dirty_files(&checkout, false), Ok(false));
        std::fs::write(checkout.join("README.md"), "dirty\n").unwrap();
        assert_eq!(checkout_has_dirty_files(&checkout, false), Ok(true));

        let remove = build_worktree_remove_command(&repo, &checkout, true, false);
        run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn worktree_remove_command_preserves_branch_by_not_deleting_it() {
        let command = build_worktree_remove_command(
            Path::new("/repo/herdr"),
            Path::new("/w/herdr/issue-137"),
            false,
            false,
        );
        assert_eq!(command.program, "git");
        assert_eq!(
            command.args,
            vec![
                "-C",
                "/repo/herdr",
                "worktree",
                "remove",
                "/w/herdr/issue-137"
            ]
        );
    }

    #[test]
    fn forced_worktree_remove_command_uses_git_force_flag() {
        let command = build_worktree_remove_command(
            Path::new("/repo/herdr"),
            Path::new("/w/herdr/issue-137"),
            true,
            false,
        );
        assert_eq!(
            command.args,
            vec![
                "-C",
                "/repo/herdr",
                "worktree",
                "remove",
                "--force",
                "/w/herdr/issue-137"
            ]
        );
    }

    #[test]
    fn dirty_remove_error_detection_matches_git_force_hint() {
        assert!(is_dirty_worktree_remove_error(
            "fatal: '/w/herdr' contains modified or untracked files, use --force to delete it"
        ));
        assert!(!is_dirty_worktree_remove_error(
            "fatal: '/w/herdr' is a missing but already registered worktree"
        ));
        assert!(!is_dirty_worktree_remove_error(
            "fatal: '/w/herdr' contains a locked worktree, use --force only if you know why"
        ));
    }

    #[test]
    fn submodule_remove_error_requires_force_confirmation() {
        assert!(is_dirty_worktree_remove_error(
            "fatal: working trees containing submodules cannot be moved or removed"
        ));
    }

    #[test]
    fn submodule_worktree_removal_requires_explicit_force() {
        let repo = create_committed_repo("submodule-remove-repo");
        let submodule = create_committed_repo("submodule-remove-source");
        let checkout = unique_temp_path("submodule-remove-checkout");
        run_git(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                submodule.to_str().unwrap(),
                "sub",
            ],
        );
        run_git(&repo, &["commit", "--quiet", "-am", "add submodule"]);
        let add = build_worktree_add_new_branch_command(
            &repo,
            &checkout,
            "worktree/submodule-remove",
            "HEAD",
            false,
        );
        run_worktree_command(&add).unwrap();
        run_git(
            &checkout,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "--quiet",
            ],
        );
        assert!(!checkout_has_dirty_files(&checkout, false).unwrap());

        let remove = build_worktree_remove_command(&repo, &checkout, false, false);
        let error = run_worktree_command(&remove).unwrap_err();
        assert!(is_dirty_worktree_remove_error(&error), "{error}");
        assert!(checkout.join("sub/README.md").exists());

        std::fs::write(checkout.join("sub/README.md"), "uncommitted change\n").unwrap();
        let error = run_worktree_command(&remove).unwrap_err();
        assert!(is_dirty_worktree_remove_error(&error), "{error}");
        assert!(checkout.join("sub/README.md").exists());

        let forced = build_worktree_remove_command(&repo, &checkout, true, false);
        run_worktree_remove_command_with_recovery(&forced, &repo, &checkout, true, false).unwrap();
        assert!(!checkout.exists());
        assert!(!worktree_list_contains_path(&repo, &checkout, false).unwrap());
        assert!(repo.join("sub/README.md").exists());

        std::fs::remove_dir_all(repo).unwrap();
        std::fs::remove_dir_all(submodule).unwrap();
    }

    #[test]
    fn worktree_add_command_creates_new_branch_from_base() {
        let command = build_worktree_add_new_branch_command(
            Path::new("/repo/herdr"),
            Path::new("/w/herdr/worktree-brave-river"),
            "worktree/brave-river",
            "HEAD",
            false,
        );
        assert_eq!(command.program, "git");
        assert_eq!(
            command.args,
            vec![
                "-C",
                "/repo/herdr",
                "worktree",
                "add",
                "-b",
                "worktree/brave-river",
                "/w/herdr/worktree-brave-river",
                "HEAD"
            ]
        );
    }

    #[test]
    fn worktree_add_command_checks_out_existing_branch() {
        let command = build_worktree_add_existing_branch_command(
            Path::new("/repo/herdr"),
            Path::new("/w/herdr/worktree-brave-river"),
            "worktree/brave-river",
            false,
        );
        assert_eq!(command.program, "git");
        assert_eq!(
            command.args,
            vec![
                "-C",
                "/repo/herdr",
                "worktree",
                "add",
                "/w/herdr/worktree-brave-river",
                "worktree/brave-river"
            ]
        );
    }

    #[test]
    fn run_worktree_add_and_remove_create_and_delete_checkout() {
        let repo = create_committed_repo("worktree-run-repo");
        let checkout = unique_temp_path("worktree-run-checkout");
        let branch = "worktree/test-create-remove";

        let add = build_worktree_add_new_branch_command(&repo, &checkout, branch, "HEAD", false);
        run_worktree_command(&add).unwrap();

        assert!(checkout.join("README.md").exists());
        let branch_name = std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert!(branch_name.status.success());
        assert_eq!(
            String::from_utf8(branch_name.stdout).unwrap().trim(),
            branch
        );

        let remove = build_worktree_remove_command(&repo, &checkout, false, false);
        run_worktree_command(&remove).unwrap();
        assert!(!checkout.exists());

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn forced_worktree_remove_recovers_leftover_unregistered_checkout() {
        let repo = create_committed_repo("worktree-recovery-repo");
        let checkout = unique_temp_path("worktree-recovery-checkout");
        let branch = "worktree/recovery";

        let add = build_worktree_add_new_branch_command(&repo, &checkout, branch, "HEAD", false);
        run_worktree_command(&add).unwrap();
        let remove = build_worktree_remove_command(&repo, &checkout, true, false);
        run_worktree_command(&remove).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        let stale_admin_dir = git_common_worktrees_dir(&repo, false)
            .unwrap()
            .join("stale");
        std::fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", stale_admin_dir.display()),
        )
        .unwrap();
        std::fs::write(checkout.join("leftover"), "leftover\n").unwrap();

        run_worktree_remove_command_with_recovery(&remove, &repo, &checkout, true, false).unwrap();

        assert!(!checkout.exists());
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn forced_worktree_remove_recovery_keeps_unrelated_replacement_directory() {
        let repo = create_committed_repo("worktree-recovery-unrelated-repo");
        let checkout = unique_temp_path("worktree-recovery-unrelated-checkout");
        let branch = "worktree/recovery-unrelated";

        let add = build_worktree_add_new_branch_command(&repo, &checkout, branch, "HEAD", false);
        run_worktree_command(&add).unwrap();
        let remove = build_worktree_remove_command(&repo, &checkout, true, false);
        run_worktree_command(&remove).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("unrelated"), "do not delete\n").unwrap();

        let err = run_worktree_remove_command_with_recovery(&remove, &repo, &checkout, true, false)
            .expect_err("unrelated replacement directory should not be removed");

        assert!(is_not_working_tree_remove_error(&err));
        assert!(checkout.join("unrelated").exists());
        let _ = std::fs::remove_dir_all(checkout);
        let _ = std::fs::remove_dir_all(repo);
    }
}
