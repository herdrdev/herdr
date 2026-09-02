use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ClientChromePreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_width: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_section_split: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_collapsed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) agent_panel_sort: Option<crate::config::AgentPanelSortConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) collapsed_groups: Vec<String>,
}

pub(super) fn path_for_local_endpoint(socket_path: &Path) -> PathBuf {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in socket_path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("local-{hash:016x}.json"))
}

pub(super) fn load(path: &Path) -> Option<ClientChromePreferences> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(super) fn store(path: &Path, preferences: ClientChromePreferences) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create client shell state directory: {error}"))?;
    let content = serde_json::to_vec_pretty(&preferences)
        .map_err(|error| format!("failed to encode client shell state: {error}"))?;
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = path
        .file_name()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?
        .to_os_string();
    temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    let temp_path = parent.join(temp_name);
    std::fs::write(&temp_path, content)
        .map_err(|error| format!("failed to write client shell state: {error}"))?;
    crate::platform::replace_file(&temp_path, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp_path);
        format!("failed to replace client shell state: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_paths_are_stable_and_distinct() {
        let first = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let again = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let second = path_for_local_endpoint(Path::new("/run/herdr/two.sock"));
        assert_eq!(first, again);
        assert_ne!(first, second);
    }

    #[test]
    fn concurrent_stores_leave_complete_preferences() {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-concurrent-preferences-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let writers = (20..28)
            .map(|width| {
                let path = path.clone();
                std::thread::spawn(move || {
                    store(
                        &path,
                        ClientChromePreferences {
                            sidebar_width: Some(width),
                            ..ClientChromePreferences::default()
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("preference writer").expect("store");
        }
        assert!(load(&path)
            .and_then(|saved| saved.sidebar_width)
            .is_some_and(|width| (20..28).contains(&width)));
        std::fs::remove_file(path).expect("remove preferences");
    }

    #[test]
    fn repeated_store_replaces_existing_preferences() {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-preferences-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(24),
                ..ClientChromePreferences::default()
            },
        )
        .expect("first preference store");
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(32),
                ..ClientChromePreferences::default()
            },
        )
        .expect("replacement preference store");
        assert_eq!(load(&path).and_then(|saved| saved.sidebar_width), Some(32));
        std::fs::remove_file(path).expect("remove preferences");
    }
}
