use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::warn;

use super::snapshot::{
    parse_history_snapshot, parse_snapshot, snapshot_file_version, SessionHistorySnapshot,
    SessionSnapshot, SNAPSHOT_VERSION,
};

pub(super) fn session_path() -> PathBuf {
    crate::session::data_dir().join("session.json")
}

fn session_history_path() -> PathBuf {
    crate::session::data_dir().join("session-history.json")
}

static SAVE_GENERATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_JSON_COMMITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn fail_next_json_commits_for_test(count: usize) {
    FAIL_NEXT_JSON_COMMITS.with(|remaining| remaining.set(count));
}

fn next_save_generation() -> String {
    // Nanosecond time makes process-restart collisions impractical; PID and a
    // monotonic sequence disambiguate concurrent/same-tick saves.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = SAVE_GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{sequence:x}", std::process::id(), nanos)
}

// Writes follow a (possibly dangling) symlink to its target so a stow-managed
// session file keeps its link. The resolver is shared with the config writer.
fn resolve_write_target(path: &Path) -> std::io::Result<PathBuf> {
    crate::platform::resolve_write_target(path)
}

#[cfg(test)]
pub(super) fn save_to_path(path: &Path, snapshot: &SessionSnapshot) -> std::io::Result<()> {
    save_json_to_path(path, snapshot)
}

#[cfg(test)]
fn save_json_to_path<T: serde::Serialize>(path: &Path, snapshot: &T) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(snapshot)?;
    commit_json_to_path(path, &json)
}

pub(crate) fn commit_json_to_path(path: &Path, json: &str) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_JSON_COMMITS.with(|remaining| {
        let count = remaining.get();
        remaining.set(count.saturating_sub(1));
        count > 0
    }) {
        return Err(std::io::Error::other("injected JSON commit failure"));
    }

    let target = resolve_write_target(path)?;
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::other("session path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let tmp_path = target.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SAVE_GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)?;
    if let Err(err) = file
        .write_all(json.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    drop(file);
    if let Err(err) = crate::platform::replace_file_durably(&tmp_path, &target) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

fn json_with_generation<T: serde::Serialize>(
    snapshot: &T,
    generation: &str,
) -> std::io::Result<String> {
    let mut value = serde_json::to_value(snapshot)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("session snapshot is not an object"))?;
    object.insert(
        "generation".to_string(),
        serde_json::Value::String(generation.to_string()),
    );
    Ok(serde_json::to_string_pretty(&value)?)
}

pub(crate) fn save_to_paths(
    session_path: &Path,
    history_path: &Path,
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
) -> std::io::Result<()> {
    save_to_paths_with_hook(session_path, history_path, snapshot, history, || Ok(()))
}

fn save_to_paths_with_hook(
    session_path: &Path,
    history_path: &Path,
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    after_history_commit: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let generation = next_save_generation();
    let session_json = json_with_generation(snapshot, &generation)?;
    let history_result = if let Some(history) = history {
        let history_json = json_with_generation(history, &generation)?;
        commit_json_to_path(history_path, &history_json)
    } else {
        clear_path(history_path)
    };
    if history_result.is_ok() {
        after_history_commit()?;
    } else if let Err(err) = &history_result {
        tracing::warn!(
            event = "persist.history",
            outcome = "error",
            path = %history_path.display(),
            err = %err,
            "failed to persist optional session history"
        );
    }
    // The topology is the commit marker. If optional history failed, its old
    // generation no longer matches and restore safely ignores it.
    commit_json_to_path(session_path, &session_json)?;
    Ok(())
}

#[cfg(test)]
pub(super) fn save_history_to_path(
    path: &Path,
    history: Option<&SessionHistorySnapshot>,
) -> std::io::Result<()> {
    match history {
        Some(history) => save_json_to_path(path, history),
        None => clear_path(path),
    }
}

pub(super) fn clear_path(path: &Path) -> std::io::Result<()> {
    let target = resolve_write_target(path)?;
    let tombstone = target.with_extension(format!(
        "json.deleted.{}.{}.tmp",
        std::process::id(),
        SAVE_GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    crate::platform::remove_file_durably(&target, &tombstone)
}

pub fn clear_history() {
    let path = session_history_path();
    if let Err(err) = clear_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
    }
}

pub fn load() -> Option<SessionSnapshot> {
    let path = session_path();
    load_from_path(&path)
}

fn load_from_path(path: &Path) -> Option<SessionSnapshot> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                event = "persist.restore", subsystem = "persist", outcome = "missing",
                path = %path.display(), "session file is missing"
            );
            return None;
        }
        Err(err) => {
            warn!(
                event = "persist.restore", subsystem = "persist", outcome = "read_error",
                path = %path.display(), err = %err, "failed to read session file"
            );
            return None;
        }
    };
    match parse_snapshot(&content) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        event = "persist.restore", subsystem = "persist", outcome = "unsupported_version",
                        path = %path.display(), file_version = version, supported = SNAPSHOT_VERSION,
                        "session file is from a newer herdr version, ignoring"
                    );
                    return None;
                }
            }
            warn!(
                event = "persist.restore", subsystem = "persist", outcome = "parse_error",
                path = %path.display(), err = %err, "failed to parse session file, ignoring"
            );
            None
        }
    }
}

pub fn load_history() -> Option<SessionHistorySnapshot> {
    let path = session_history_path();
    load_history_from_path(&path)
}

fn load_history_from_path(path: &Path) -> Option<SessionHistorySnapshot> {
    if !path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            warn!(err = %err, "failed to read session history file");
            return None;
        }
    };
    match parse_history_snapshot(&content) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = version,
                        supported = SNAPSHOT_VERSION,
                        "session history file is from a newer herdr version, ignoring"
                    );
                    return None;
                }
            }
            warn!(err = %err, "failed to parse session history file, ignoring");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::snapshot::{
        PaneHistorySnapshot, TabHistorySnapshot, WorkspaceHistorySnapshot,
    };

    fn temp_session_path(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-session-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("session.json")
    }

    fn temp_session_paths(name: &str) -> (PathBuf, PathBuf) {
        let session = temp_session_path(name);
        let history = session.with_file_name("session-history.json");
        (session, history)
    }

    fn empty_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            version: SNAPSHOT_VERSION,
            generation: None,
            workspaces: vec![],
            active: None,
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: std::collections::HashSet::new(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        }
    }

    fn history_snapshot(secret: &str) -> SessionHistorySnapshot {
        SessionHistorySnapshot {
            version: SNAPSHOT_VERSION,
            generation: None,
            layout_fingerprint: None,
            workspaces: vec![WorkspaceHistorySnapshot {
                tabs: vec![TabHistorySnapshot {
                    panes: std::collections::HashMap::from([(
                        0,
                        PaneHistorySnapshot {
                            pane_id: Some("workspace:p1".into()),
                            ansi: secret.to_string(),
                            lines: 1,
                        },
                    )]),
                }],
            }],
        }
    }

    #[test]
    fn save_to_paths_writes_pane_history_only_to_history_file() {
        let (session_path, history_path) = temp_session_paths("split-history");

        save_to_path(&session_path, &empty_snapshot()).unwrap();
        save_history_to_path(&history_path, Some(&history_snapshot("split-secret"))).unwrap();

        let session = std::fs::read_to_string(&session_path).unwrap();
        let history = std::fs::read_to_string(&history_path).unwrap();
        assert!(!session.contains("split-secret"));
        assert!(!session.contains("history"));
        assert!(history.contains("split-secret"));
    }

    #[test]
    fn save_to_paths_removes_stale_history_when_history_is_disabled() {
        let (session_path, history_path) = temp_session_paths("clear-history");
        save_to_path(&session_path, &empty_snapshot()).unwrap();
        save_history_to_path(&history_path, Some(&history_snapshot("stale-secret"))).unwrap();

        save_history_to_path(&history_path, None).unwrap();

        assert!(session_path.exists());
        assert!(!history_path.exists());
    }

    #[test]
    fn ac2_crash_after_history_commit_leaves_mismatched_generation() {
        let (session_path, history_path) = temp_session_paths("generation-crash");
        save_to_paths(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("old-history")),
        )
        .unwrap();
        let old_session = load_from_path(&session_path).unwrap();

        let err = save_to_paths_with_hook(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("new-history")),
            || Err(std::io::Error::other("simulated crash")),
        )
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::Other);
        let current_session = load_from_path(&session_path).unwrap();
        let current_history = load_history_from_path(&history_path).unwrap();
        assert_eq!(current_session.generation, old_session.generation);
        assert_ne!(current_session.generation, current_history.generation);
        assert!(crate::persist::restore::compatible_history(
            &current_session,
            Some(&current_history)
        )
        .is_none());
    }

    #[test]
    fn ac2_crash_after_disabling_history_removes_secret_before_topology_commit() {
        let (session_path, history_path) = temp_session_paths("disable-history-crash");
        save_to_paths(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("stale-secret")),
        )
        .unwrap();
        let old_session = load_from_path(&session_path).unwrap();

        save_to_paths_with_hook(
            &session_path,
            &history_path,
            &empty_snapshot(),
            None,
            || Err(std::io::Error::other("simulated crash")),
        )
        .unwrap_err();

        let current_session = load_from_path(&session_path).unwrap();
        assert_eq!(current_session.generation, old_session.generation);
        assert!(!history_path.exists());
    }

    #[test]
    fn ac4_legacy_v3_without_generation_still_parses() {
        let session =
            parse_snapshot(r#"{"version":3,"workspaces":[],"active":null,"selected":0}"#).unwrap();
        let history = parse_history_snapshot(r#"{"version":3,"workspaces":[]}"#).unwrap();

        assert_eq!(session.generation, None);
        assert_eq!(history.generation, None);
    }

    #[test]
    fn ac2_save_generations_are_unique_within_a_process() {
        assert_ne!(next_save_generation(), next_save_generation());
    }

    #[test]
    fn clear_path_removes_existing_session_file() {
        let path = temp_session_path("clear-existing");
        save_to_path(&path, &empty_snapshot()).unwrap();

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn clear_path_ignores_missing_session_file() {
        let path = temp_session_path("clear-missing");

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_preserves_existing_symlink() {
        let target = temp_session_path("symlink-target");
        let link = target.with_file_name("link.json");
        save_to_path(&target, &empty_snapshot()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut snap = empty_snapshot();
        snap.selected = 7;
        save_to_path(&link, &snap).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let parsed = parse_snapshot(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(parsed.selected, 7);
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_writes_through_dangling_symlink() {
        let target = temp_session_path("dangling-target");
        let link = target.with_file_name("link.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        save_to_path(&link, &empty_snapshot()).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_resolves_relative_symlink() {
        let session = temp_session_path("relative-symlink");
        let dir = session.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let target = dir.join("real.json");
        let link = dir.join("link.json");
        std::os::unix::fs::symlink("real.json", &link).unwrap();

        save_to_path(&link, &empty_snapshot()).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn ac2_clear_path_preserves_symlink_and_durably_removes_target() {
        let target = temp_session_path("clear-symlink-target");
        let link = target.with_file_name("clear-link.json");
        save_to_path(&target, &empty_snapshot()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        clear_path(&link).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!target.exists());
    }
}
