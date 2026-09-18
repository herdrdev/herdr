//! Hook-free discovery of the Kiro CLI conversation running in a pane.
//!
//! Stock Kiro publishes the identity of its active conversation to the file
//! system: while a chat is open it holds a lock file whose body is
//! `{"pid": <owner>, "started_at": ...}`.
//!
//! * V2 engine: `~/.kiro/sessions/cli/<SESSION_ID>.lock`
//! * V3 engine: `~/.kiro/sessions/<cwd-hash>/sess_<SESSION_ID>/.lock`
//!
//! The owning pid is a descendant of the `kiro-cli` process Herdr already
//! tracks in the pane. Matching the lock owner against that process tree gives
//! the exact session id for the exact pane with no hook installed in Kiro.
//! The lock is removed when Kiro exits (including the SIGHUP it receives when
//! the Herdr server dies), so discovery must happen while Kiro is alive and the
//! result must be persisted through the normal agent-session path.

use std::path::{Path, PathBuf};

/// Session reports discovered this way use this hook-style source label.
pub const SOURCE: &str = "herdr:kiro";
pub const AGENT_LABEL: &str = "kiro";

/// Upper bound on the parent chain walked from a lock owner. Kiro's real
/// chain is several processes deep (kiro-cli wrapper -> ... -> chat process).
const MAX_ANCESTRY_DEPTH: usize = 32;

/// Root of the Kiro home directory, honoring `KIRO_HOME`.
pub fn kiro_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("KIRO_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".kiro"))
}

/// A lock file Kiro holds for a live conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KiroSessionLock {
    pub session_id: String,
    pub owner_pid: u32,
}

impl KiroSessionLock {
    /// V3 conversations carry a `sess_` prefix; they must be resumed on the V3
    /// engine, V2 ids on the default (V2) engine.
    #[cfg(test)]
    pub fn is_v3(&self) -> bool {
        Self::session_id_is_v3(&self.session_id)
    }

    pub fn session_id_is_v3(session_id: &str) -> bool {
        session_id.starts_with("sess_")
    }
}

/// Parse a lock body of the form `{"pid": 123, ...}`. Kiro writes compact
/// serde JSON, but this stays tolerant of whitespace and key order without
/// pulling in a JSON parser for a two-field file.
pub fn parse_lock_owner_pid(body: &str) -> Option<u32> {
    let key = body.find("\"pid\"")?;
    let rest = &body[key + "\"pid\"".len()..];
    let colon = rest.find(':')?;
    let digits: String = rest[colon + 1..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let pid: u32 = digits.parse().ok()?;
    (pid > 0).then_some(pid)
}

/// Enumerate every live Kiro lock under `sessions_root`
/// (normally `~/.kiro/sessions`). Bounded to two directory levels.
pub fn scan_locks(sessions_root: &Path) -> Vec<KiroSessionLock> {
    let mut locks = Vec::new();
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return locks;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(children) = std::fs::read_dir(&path) else {
            continue;
        };
        for child in children.flatten() {
            let child_path = child.path();
            let Some(name) = child_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // V2: sessions/cli/<id>.lock
            if let Some(id) = name.strip_suffix(".lock") {
                if !id.is_empty() && child_path.is_file() {
                    push_lock(&mut locks, id, &child_path);
                }
                continue;
            }
            // V3: sessions/<hash>/sess_<id>/.lock
            if name.starts_with("sess_") && child_path.is_dir() {
                let lock_path = child_path.join(".lock");
                if lock_path.is_file() {
                    push_lock(&mut locks, name, &lock_path);
                }
            }
        }
    }
    locks
}

fn push_lock(locks: &mut Vec<KiroSessionLock>, session_id: &str, lock_path: &Path) {
    let Ok(body) = std::fs::read_to_string(lock_path) else {
        return;
    };
    if let Some(owner_pid) = parse_lock_owner_pid(&body) {
        locks.push(KiroSessionLock {
            session_id: session_id.to_string(),
            owner_pid,
        });
    }
}

/// Whether `pid` is `ancestor` or has `ancestor` in its parent chain.
pub fn descends_from(pid: u32, ancestor: u32, parent_of: impl Fn(u32) -> Option<u32>) -> bool {
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if current == ancestor {
            return true;
        }
        match parent_of(current) {
            Some(parent) if parent > 1 && parent != current => current = parent,
            _ => return false,
        }
    }
    false
}

/// The Kiro conversation owned by a process descending from `pane_child_pid`.
/// Returns `None` when no lock matches (Kiro not yet started, already exited,
/// or has not created its lock yet — Kiro writes it a few seconds after
/// launch, so callers poll while the agent is present).
pub fn discover_for_pane(pane_child_pid: u32) -> Option<KiroSessionLock> {
    let root = kiro_home()?.join("sessions");
    discover_in(&root, pane_child_pid, crate::platform::parent_process_id)
}

pub fn discover_in(
    sessions_root: &Path,
    pane_child_pid: u32,
    parent_of: impl Fn(u32) -> Option<u32> + Copy,
) -> Option<KiroSessionLock> {
    scan_locks(sessions_root)
        .into_iter()
        .find(|lock| descends_from(lock.owner_pid, pane_child_pid, parent_of))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn parses_kiro_lock_bodies() {
        assert_eq!(
            parse_lock_owner_pid(r#"{"pid":90458,"started_at":"2026-09-18T15:19:03.669459Z"}"#),
            Some(90458)
        );
        assert_eq!(parse_lock_owner_pid(r#"{ "started_at": "x", "pid" : 7 }"#), Some(7));
        assert_eq!(parse_lock_owner_pid(r#"{"pid":0}"#), None);
        assert_eq!(parse_lock_owner_pid(""), None);
        assert_eq!(parse_lock_owner_pid("not json"), None);
    }

    #[test]
    fn v3_ids_are_prefixed() {
        assert!(KiroSessionLock::session_id_is_v3("sess_ad83e258-4a35"));
        assert!(!KiroSessionLock::session_id_is_v3("b6d55962-0abc-4064"));
    }

    #[test]
    fn ancestry_walk_is_bounded_and_matches_descendants() {
        let mut parents: HashMap<u32, u32> = HashMap::new();
        // 500 <- 400 <- 300 <- 200 (pane child)
        parents.insert(500, 400);
        parents.insert(400, 300);
        parents.insert(300, 200);
        parents.insert(200, 100);
        let parent_of = |pid: u32| parents.get(&pid).copied();
        assert!(descends_from(500, 200, parent_of));
        assert!(descends_from(200, 200, parent_of));
        assert!(!descends_from(500, 999, parent_of));
        // self-loop must terminate
        let looping = |_pid: u32| Some(5u32);
        assert!(!descends_from(5, 6, looping));
    }

    #[test]
    fn scans_v2_and_v3_lock_layouts_and_matches_pane() {
        let root_buf = std::env::temp_dir().join(format!(
            "herdr-kiro-session-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root_buf);
        std::fs::create_dir_all(&root_buf).unwrap();
        let root = root_buf.as_path();
        std::fs::create_dir_all(root.join("cli")).unwrap();
        std::fs::write(
            root.join("cli").join("b6d55962-v2.lock"),
            r#"{"pid":901,"started_at":"t"}"#,
        )
        .unwrap();
        std::fs::write(root.join("cli").join("b6d55962-v2.history"), "x").unwrap();
        let v3 = root.join("3a3802dee60bffa2").join("sess_ad83e258-v3");
        std::fs::create_dir_all(&v3).unwrap();
        std::fs::write(v3.join(".lock"), r#"{"pid":902,"started_at":"t"}"#).unwrap();
        // an index lock without a pid must be ignored
        std::fs::create_dir_all(root.join("3a3802dee60bffa2").join(".index")).unwrap();
        std::fs::write(root.join("3a3802dee60bffa2").join(".index").join(".lock"), "").unwrap();

        let mut locks = scan_locks(root);
        locks.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(
            locks,
            vec![
                KiroSessionLock {
                    session_id: "b6d55962-v2".into(),
                    owner_pid: 901
                },
                KiroSessionLock {
                    session_id: "sess_ad83e258-v3".into(),
                    owner_pid: 902
                },
            ]
        );

        // pane A (child 10) owns 901; pane B (child 20) owns 902
        let mut parents: HashMap<u32, u32> = HashMap::new();
        parents.insert(901, 10);
        parents.insert(902, 20);
        let parent_of = |pid: u32| parents.get(&pid).copied();
        assert_eq!(
            discover_in(root, 10, parent_of).map(|l| l.session_id),
            Some("b6d55962-v2".into())
        );
        let found = discover_in(root, 20, parent_of).unwrap();
        assert_eq!(found.session_id, "sess_ad83e258-v3");
        assert!(found.is_v3());
        assert!(discover_in(root, 30, parent_of).is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
