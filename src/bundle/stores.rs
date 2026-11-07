//! Locating and installing agent session files.
//!
//! Herdr records *which* agent session a pane was running; the transcript
//! itself lives in the agent's own store. Export copies that one session
//! (never the whole store, which also holds credentials) and import installs
//! it into the same kind of store on the target machine. Layouts below are
//! the agents' documented on-disk conventions and are home-relative on
//! Linux, macOS, and Windows, so only the home directory differs per OS.

use std::path::{Path, PathBuf};

use super::{
    claude_project_key, copy_file, copy_tree_filtered, home_dir, sanitize_jsonl_file, BundleResult,
};

/// Session stores whose file layout export/import understand. Agents not
/// listed here keep their session reference in the bundle; they resume on
/// the target only when that session already exists there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreKind {
    Claude,
    Codex,
    Muse,
    /// An agent whose session reference is the session file's own path
    /// (for example Pi and OMP).
    SessionFile,
}

#[derive(Debug, Clone)]
pub(crate) struct StoreRoots {
    pub(crate) home: PathBuf,
    /// `CLAUDE_CONFIG_DIR` when set, else `<home>/.claude`.
    pub(crate) claude_dir: PathBuf,
    /// `CODEX_HOME` when set, else `<home>/.codex`.
    pub(crate) codex_dir: PathBuf,
    /// Candidate `muse/sessions` roots, most preferred first.
    pub(crate) muse_roots: Vec<PathBuf>,
}

impl StoreRoots {
    pub(crate) fn from_env() -> Option<Self> {
        let home = home_dir()?;
        let env_path = |key: &str| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
        };
        Some(Self {
            claude_dir: env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
            codex_dir: env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            muse_roots: default_muse_roots(&home, env_path("XDG_DATA_HOME")),
            home,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_home(home: PathBuf) -> Self {
        Self {
            claude_dir: home.join(".claude"),
            codex_dir: home.join(".codex"),
            muse_roots: vec![home.join(".local/share/muse/sessions")],
            home,
        }
    }

    fn claude_projects(&self) -> PathBuf {
        self.claude_dir.join("projects")
    }

    fn codex_sessions(&self) -> PathBuf {
        self.codex_dir.join("sessions")
    }

    /// Where a Muse session is installed when no existing root matches.
    fn primary_muse_root(&self) -> PathBuf {
        for root in &self.muse_roots {
            if root.parent().is_some_and(Path::is_dir) {
                return root.clone();
            }
        }
        self.muse_roots
            .first()
            .cloned()
            .unwrap_or_else(|| self.home.join(".local/share/muse/sessions"))
    }
}

fn default_muse_roots(home: &Path, xdg_data_home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(xdg) = xdg_data_home {
        roots.push(xdg.join("muse/sessions"));
    }
    roots.push(home.join(".local/share/muse/sessions"));
    if cfg!(target_os = "macos") {
        roots.push(home.join("Library/Application Support/muse/sessions"));
    }
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("muse/sessions"));
        }
        if let Some(roaming) = std::env::var_os("APPDATA") {
            roots.push(PathBuf::from(roaming).join("muse/sessions"));
        }
    }
    roots
}

/// The store for an official session reference, if export can copy its files.
pub(crate) fn kind_for(
    agent: &str,
    session_kind: &crate::agent_resume::AgentSessionRefKind,
) -> Option<StoreKind> {
    use crate::agent_resume::AgentSessionRefKind;
    match session_kind {
        AgentSessionRefKind::Path => Some(StoreKind::SessionFile),
        AgentSessionRefKind::Id => match agent {
            "claude" => Some(StoreKind::Claude),
            "codex" => Some(StoreKind::Codex),
            _ => None,
        },
    }
}

/// Muse has no official Herdr session reference; its panes report a resume
/// command (`muse resume <id>`) instead. Extract the session ID from it.
pub(crate) fn muse_session_id(resume_argv: &[String]) -> Option<String> {
    let [command, subcommand, id] = resume_argv else {
        return None;
    };
    let executable = Path::new(command).file_stem()?.to_str()?;
    (executable == "muse" && subcommand == "resume" && !id.starts_with('-')).then(|| id.clone())
}

/// Files located in a local store for one agent session.
pub(crate) enum Located {
    /// A single transcript file (Claude, Codex, path-reference sessions).
    File { path: PathBuf, store_root: PathBuf },
    /// A session directory (Muse).
    Dir { path: PathBuf, store_root: PathBuf },
}

pub(crate) fn locate(
    kind: StoreKind,
    session_value: &str,
    cwd: &str,
    roots: &StoreRoots,
) -> Option<Located> {
    match kind {
        StoreKind::Claude => locate_claude(session_value, cwd, roots),
        StoreKind::Codex => locate_codex(session_value, roots),
        StoreKind::Muse => locate_muse(session_value, roots),
        StoreKind::SessionFile => {
            let path = PathBuf::from(session_value);
            path.is_file().then_some(Located::File {
                store_root: path.parent()?.to_path_buf(),
                path,
            })
        }
    }
}

fn locate_claude(id: &str, cwd: &str, roots: &StoreRoots) -> Option<Located> {
    let projects = roots.claude_projects();
    let direct = projects
        .join(claude_project_key(cwd))
        .join(format!("{id}.jsonl"));
    if direct.is_file() {
        return Some(Located::File {
            path: direct,
            store_root: projects,
        });
    }
    // The pane may have changed directory since the session started.
    find_file(&projects, 2, &|path| {
        path.file_name()
            .is_some_and(|name| name == format!("{id}.jsonl").as_str())
    })
    .map(|path| Located::File {
        path,
        store_root: projects,
    })
}

fn locate_codex(id: &str, roots: &StoreRoots) -> Option<Located> {
    let sessions = roots.codex_sessions();
    find_file(&sessions, 4, &|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("rollout-") && name.ends_with(".jsonl") && name.contains(id)
            })
    })
    .map(|path| Located::File {
        path,
        store_root: sessions,
    })
}

fn locate_muse(id: &str, roots: &StoreRoots) -> Option<Located> {
    for root in &roots.muse_roots {
        if let Some(dir) = find_dir(root, 4, &|path| {
            path.file_name().is_some_and(|name| name == id) && path.join("session.jsonl").is_file()
        }) {
            return Some(Located::Dir {
                path: dir,
                store_root: root.clone(),
            });
        }
    }
    None
}

/// Bounded directory walk returning the first matching file. Symlinks are
/// never followed, so a store cannot lead export outside itself.
fn find_file(root: &Path, max_depth: usize, matches: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_file() && matches(&path) {
                return Some(path);
            }
            if file_type.is_dir() && depth < max_depth {
                stack.push((path, depth + 1));
            }
        }
    }
    None
}

/// Join a `/`-separated bundle-relative path onto a base directory. Bundle
/// paths always use `/`, on every OS, so bundles cross between them.
pub(crate) fn bundle_path(base: &Path, relative: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for component in relative.split('/').filter(|part| !part.is_empty()) {
        path.push(component);
    }
    path
}

fn relative_slashes(path: &Path, root: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

const MUSE_EXCLUDE_NAMES: &[&str] = &["cron.db", "tool-outputs"];

fn muse_keep(name: &str, _is_dir: bool) -> bool {
    !MUSE_EXCLUDE_NAMES.contains(&name) && !name.ends_with(".lock") && !name.ends_with(".log")
}

fn sanitize_jsonl_tree(dir: &Path) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                let _ = sanitize_jsonl_file(&path);
            }
        }
    }
}

/// Copy a located session into the bundle. Returns its `/`-separated path
/// relative to the bundle root.
pub(crate) fn copy_into_bundle(
    kind: StoreKind,
    located: &Located,
    agent: &str,
    session_value: &str,
    bundle_dir: &Path,
) -> BundleResult<String> {
    match (kind, located) {
        (StoreKind::Muse, Located::Dir { path, store_root }) => {
            let relative =
                relative_slashes(path, store_root).unwrap_or_else(|| session_value.to_string());
            let bundle_relative = format!("agents/muse/{relative}");
            let target = bundle_path(bundle_dir, &bundle_relative);
            copy_tree_filtered(path, &target, &muse_keep)?;
            sanitize_jsonl_tree(&target);
            Ok(bundle_relative)
        }
        (StoreKind::SessionFile, Located::File { path, .. }) => {
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "session.jsonl".to_string());
            let bundle_relative = format!(
                "agents/session-files/{agent}/{}-{file_name}",
                short_digest(session_value)
            );
            let target = bundle_path(bundle_dir, &bundle_relative);
            copy_file(path, &target)?;
            sanitize_if_jsonl(&target);
            Ok(bundle_relative)
        }
        (_, Located::File { path, store_root }) => {
            let agent_dir = match kind {
                StoreKind::Claude => "claude",
                StoreKind::Codex => "codex",
                StoreKind::Muse => "muse",
                StoreKind::SessionFile => "session-files",
            };
            let relative = relative_slashes(path, store_root).unwrap_or_else(|| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let bundle_relative = format!("agents/{agent_dir}/{relative}");
            let target = bundle_path(bundle_dir, &bundle_relative);
            copy_file(path, &target)?;
            sanitize_if_jsonl(&target);
            if kind == StoreKind::Claude {
                // Claude keeps subagent data in a sibling `<id>/` directory.
                let side_dir = path.with_extension("");
                if side_dir.is_dir() {
                    let side_relative = relative
                        .strip_suffix(".jsonl")
                        .unwrap_or(&relative)
                        .to_string();
                    let side_target =
                        bundle_path(bundle_dir, &format!("agents/claude/{side_relative}"));
                    copy_tree_filtered(&side_dir, &side_target, &|_, _| true)?;
                }
            }
            Ok(bundle_relative)
        }
        (_, Located::Dir { .. }) => Err(super::message_error(
            "unexpected session directory for this agent store",
        )),
    }
}

fn sanitize_if_jsonl(path: &Path) {
    if path.extension().is_some_and(|ext| ext == "jsonl") {
        let _ = sanitize_jsonl_file(path);
    }
}

fn short_digest(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value.as_bytes());
    digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn find_dir(root: &Path, max_depth: usize, matches: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if dir != root && matches(&dir) {
            return Some(dir);
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .file_type()
                .is_ok_and(|file_type| file_type.is_dir() && !file_type.is_symlink())
            {
                stack.push((entry.path(), depth + 1));
            }
        }
    }
    None
}

/// What import should do with one bundled agent session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InstallPlan {
    /// Copy `bundle_relative` (file or directory) to `target`.
    Install {
        bundle_relative: String,
        target: PathBuf,
        is_dir: bool,
    },
    /// A copy at least as new already exists on this machine.
    AlreadyPresent { target: PathBuf },
}

/// Decide where a bundled session belongs on this machine, without writing.
/// `mapped_cwd` is the pane directory after `--map` rewrites; `mapped_value`
/// is the session reference after the same rewrites (only differs for
/// path-reference sessions).
pub(crate) fn install_plan(
    kind: StoreKind,
    bundle_relative: &str,
    session_value: &str,
    mapped_cwd: &str,
    mapped_value: &str,
    roots: &StoreRoots,
    bundle_dir: &Path,
) -> Option<InstallPlan> {
    let source = bundle_path(bundle_dir, bundle_relative);
    if !source.exists() {
        return None;
    }
    let (target, is_dir) = match kind {
        StoreKind::Claude => (
            roots
                .claude_projects()
                .join(claude_project_key(mapped_cwd))
                .join(format!("{session_value}.jsonl")),
            false,
        ),
        StoreKind::Codex => {
            let relative = bundle_relative.strip_prefix("agents/codex/")?;
            (bundle_path(&roots.codex_sessions(), relative), false)
        }
        StoreKind::Muse => {
            let relative = bundle_relative.strip_prefix("agents/muse/")?;
            (bundle_path(&roots.primary_muse_root(), relative), true)
        }
        StoreKind::SessionFile => (PathBuf::from(mapped_value), false),
    };
    if already_present(&source, &target, is_dir) {
        return Some(InstallPlan::AlreadyPresent { target });
    }
    Some(InstallPlan::Install {
        bundle_relative: bundle_relative.to_string(),
        target,
        is_dir,
    })
}

/// Never overwrite a local session that is already at least as large: it may
/// be the live, further-advanced copy of the same conversation.
fn already_present(source: &Path, target: &Path, is_dir: bool) -> bool {
    let (source_marker, target_marker) = if is_dir {
        (source.join("session.jsonl"), target.join("session.jsonl"))
    } else {
        (source.to_path_buf(), target.to_path_buf())
    };
    let (Ok(source_meta), Ok(target_meta)) = (
        std::fs::metadata(&source_marker),
        std::fs::metadata(&target_marker),
    ) else {
        return false;
    };
    target_meta.len() >= source_meta.len()
}

pub(crate) fn execute_install(plan: &InstallPlan, bundle_dir: &Path) -> BundleResult<()> {
    let InstallPlan::Install {
        bundle_relative,
        target,
        is_dir,
    } = plan
    else {
        return Ok(());
    };
    let source = bundle_path(bundle_dir, bundle_relative);
    if *is_dir {
        copy_tree_filtered(&source, target, &|_, _| true)?;
    } else {
        copy_file(&source, target)?;
        // Claude subagent side directory travels beside the transcript.
        let side_source = source.with_extension("");
        if side_source.is_dir() {
            copy_tree_filtered(&side_source, &target.with_extension(""), &|_, _| true)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn muse_id_comes_from_reported_resume_only() {
        let argv = vec!["muse".into(), "resume".into(), "id-1".into()];
        assert_eq!(muse_session_id(&argv).as_deref(), Some("id-1"));
        let argv = vec!["claude".into(), "--resume".into(), "id-1".into()];
        assert_eq!(muse_session_id(&argv), None);
        let argv = vec!["muse".into(), "resume".into(), "--last".into()];
        assert_eq!(muse_session_id(&argv), None);
    }

    #[test]
    fn claude_round_trip_through_bundle() {
        let home = super::super::test_temp_dir("stores-claude");
        let roots = StoreRoots::for_home(home.clone());
        let cwd = home.join("app");
        let cwd_str = cwd.display().to_string();
        let transcript = roots
            .claude_projects()
            .join(claude_project_key(&cwd_str))
            .join("session-1.jsonl");
        write(&transcript, "{\"a\":1}\n{\"b\":2}\n{\"torn\":");

        let located = locate(StoreKind::Claude, "session-1", &cwd_str, &roots).unwrap();
        let bundle = super::super::test_temp_dir("stores-claude-bundle");
        let relative =
            copy_into_bundle(StoreKind::Claude, &located, "claude", "session-1", &bundle).unwrap();
        assert!(relative.starts_with("agents/claude/"), "{relative}");
        let copied = std::fs::read_to_string(bundle_path(&bundle, &relative)).unwrap();
        assert_eq!(copied, "{\"a\":1}\n{\"b\":2}\n", "torn tail is dropped");

        // Install into a fresh home under a mapped directory.
        let other_home = super::super::test_temp_dir("stores-claude-other");
        let other_roots = StoreRoots::for_home(other_home.clone());
        let mapped_cwd = other_home.join("app").display().to_string();
        let plan = install_plan(
            StoreKind::Claude,
            &relative,
            "session-1",
            &mapped_cwd,
            "session-1",
            &other_roots,
            &bundle,
        )
        .unwrap();
        execute_install(&plan, &bundle).unwrap();
        let installed = other_roots
            .claude_projects()
            .join(claude_project_key(&mapped_cwd))
            .join("session-1.jsonl");
        assert_eq!(
            std::fs::read_to_string(installed).unwrap(),
            "{\"a\":1}\n{\"b\":2}\n"
        );
        // A second import must not overwrite the equal copy.
        let plan = install_plan(
            StoreKind::Claude,
            &relative,
            "session-1",
            &mapped_cwd,
            "session-1",
            &other_roots,
            &bundle,
        )
        .unwrap();
        assert!(matches!(plan, InstallPlan::AlreadyPresent { .. }));
        for dir in [home, bundle, other_home] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn muse_copy_excludes_locks_databases_and_spools() {
        let home = super::super::test_temp_dir("stores-muse");
        let roots = StoreRoots::for_home(home.clone());
        let session = roots.muse_roots[0].join("2026/11/07/id-9");
        write(&session.join("session.jsonl"), "{\"a\":1}\n");
        write(&session.join("cron.db"), "secret-db");
        write(&session.join("run.lock"), "lock");
        write(&session.join("debug.log"), "log");
        write(&session.join("tool-outputs/out.txt"), "spool");

        let located = locate(StoreKind::Muse, "id-9", "/anywhere", &roots).unwrap();
        let bundle = super::super::test_temp_dir("stores-muse-bundle");
        let relative =
            copy_into_bundle(StoreKind::Muse, &located, "muse", "id-9", &bundle).unwrap();
        assert_eq!(relative, "agents/muse/2026/11/07/id-9");
        let copied = bundle_path(&bundle, &relative);
        assert!(copied.join("session.jsonl").is_file());
        assert!(!copied.join("cron.db").exists());
        assert!(!copied.join("run.lock").exists());
        assert!(!copied.join("debug.log").exists());
        assert!(!copied.join("tool-outputs").exists());
        for dir in [home, bundle] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn codex_locate_matches_rollout_name() {
        let home = super::super::test_temp_dir("stores-codex");
        let roots = StoreRoots::for_home(home.clone());
        let rollout = roots
            .codex_sessions()
            .join("2026/11/07/rollout-2026-11-07T10-00-00-codex-id-1.jsonl");
        write(&rollout, "{\"a\":1}\n");
        let located = locate(StoreKind::Codex, "codex-id-1", "/anywhere", &roots).unwrap();
        let bundle = super::super::test_temp_dir("stores-codex-bundle");
        let relative =
            copy_into_bundle(StoreKind::Codex, &located, "codex", "codex-id-1", &bundle).unwrap();
        assert_eq!(
            relative,
            "agents/codex/2026/11/07/rollout-2026-11-07T10-00-00-codex-id-1.jsonl"
        );
        for dir in [home, bundle] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
