use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

const PRESENTATION_FILE_NAME: &str = "client-presentation.json";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct SpawnDockDraft {
    pub(crate) prompt: String,
    pub(crate) project: String,
    pub(crate) host: String,
    pub(crate) profile: String,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) worktree: String,
    pub(crate) worktree_path: Option<PathBuf>,
    pub(crate) saved_at_unix: u64,
}

#[cfg(test)]
thread_local! {
    static TEST_SPAWN_DOCK_DRAFTS: std::cell::RefCell<std::collections::HashMap<String, Option<SpawnDockDraft>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ClientPresentationFile {
    #[serde(default)]
    dock_width: Option<u16>,
    #[serde(default)]
    notepad_height: Option<u16>,
    #[serde(default)]
    sidebar_group_mode: Option<crate::app::state::SidebarGroupMode>,
    #[serde(default)]
    sidebar_work_filter: Option<crate::app::state::SidebarWorkFilter>,
    #[serde(default)]
    sidebar_group_sorts:
        Option<std::collections::HashMap<String, crate::app::state::SidebarSortMode>>,
    #[serde(default)]
    sidebar_group_collapsed: Option<std::collections::HashMap<String, bool>>,
    #[serde(default, rename = "spawn_dock_draft")]
    _legacy_spawn_dock_draft: Option<SpawnDockDraft>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct SpawnDockDraftFile {
    #[serde(default)]
    draft: Option<SpawnDockDraft>,
}

fn presentation_path() -> PathBuf {
    crate::config::state_dir().join(PRESENTATION_FILE_NAME)
}

#[cfg(not(test))]
fn spawn_dock_draft_path(client_id: &str) -> PathBuf {
    spawn_dock_draft_path_in(&crate::config::state_dir(), client_id)
}

fn spawn_dock_draft_path_in(state_dir: &Path, client_id: &str) -> PathBuf {
    let hash = client_id
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    state_dir.join(format!("spawn-dock-draft-{hash:016x}.json"))
}

pub(crate) fn load_spawn_dock_draft(client_id: &str) -> Option<SpawnDockDraft> {
    #[cfg(test)]
    {
        TEST_SPAWN_DOCK_DRAFTS.with(|drafts| drafts.borrow().get(client_id).cloned().flatten())
    }
    #[cfg(not(test))]
    {
        let path = spawn_dock_draft_path(client_id);
        match load_spawn_dock_draft_from_path(&path) {
            Ok(state) => state.draft,
            Err(err) => {
                warn!(path = %path.display(), err = %err, "failed to load spawn dock draft");
                None
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn saved_spawn_dock_draft_for_test(client_id: &str) -> Option<SpawnDockDraft> {
    load_spawn_dock_draft(client_id)
}

pub(crate) fn save_spawn_dock_draft(client_id: &str, draft: Option<SpawnDockDraft>) {
    #[cfg(test)]
    {
        TEST_SPAWN_DOCK_DRAFTS.with(|saved| {
            saved.borrow_mut().insert(client_id.to_owned(), draft);
        });
    }
    #[cfg(not(test))]
    {
        let path = spawn_dock_draft_path(client_id);
        if let Err(err) = save_spawn_dock_draft_to_path(&path, draft) {
            warn!(path = %path.display(), err = %err, "failed to save spawn dock draft");
        }
    }
}

fn save_spawn_dock_draft_to_path(
    path: &Path,
    draft: Option<SpawnDockDraft>,
) -> std::io::Result<()> {
    save_json_to_path(path, &SpawnDockDraftFile { draft })
}

fn load_spawn_dock_draft_from_path(path: &Path) -> std::io::Result<SpawnDockDraftFile> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SpawnDockDraftFile::default());
        }
        Err(err) => return Err(err),
    };
    serde_json::from_str(&content).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid spawn dock draft: {err}"),
        )
    })
}

fn clamp_dock_width(width: u16) -> u16 {
    width.clamp(crate::ui::DOCK_MIN_WIDTH, crate::ui::DOCK_MAX_WIDTH)
}

pub(crate) fn load_dock_width() -> u16 {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state
            .dock_width
            .map(clamp_dock_width)
            .unwrap_or(crate::ui::DOCK_DEFAULT_WIDTH),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            crate::ui::DOCK_DEFAULT_WIDTH
        }
    }
}

pub(crate) fn save_dock_width(width: u16) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| {
        state.dock_width = Some(clamp_dock_width(width));
    }) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

fn clamp_notepad_height(height: u16) -> u16 {
    height.clamp(crate::notepad::MIN_HEIGHT, crate::notepad::MAX_HEIGHT)
}

/// The dragged notepad panel height, when the operator ever dragged it. The
/// configured `[notepad] height` stays the default until then.
// The only caller is `#[cfg(not(test))]` in App::new, so tests would flag this
// as dead.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn load_notepad_height() -> Option<u16> {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state.notepad_height.map(clamp_notepad_height),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            None
        }
    }
}

pub(crate) fn save_notepad_height(height: u16) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| {
        state.notepad_height = Some(clamp_notepad_height(height));
    }) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

pub(crate) fn load_sidebar_group_mode() -> crate::app::state::SidebarGroupMode {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => match state.sidebar_group_mode.unwrap_or_default() {
            crate::app::state::SidebarGroupMode::RepoWorktree => {
                crate::app::state::SidebarGroupMode::Repo
            }
            mode => mode,
        },
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            crate::app::state::SidebarGroupMode::default()
        }
    }
}

pub(crate) fn save_sidebar_group_mode(mode: crate::app::state::SidebarGroupMode) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| state.sidebar_group_mode = Some(mode)) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

pub(crate) fn load_sidebar_work_filter() -> crate::app::state::SidebarWorkFilter {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state.sidebar_work_filter.unwrap_or_default(),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            crate::app::state::SidebarWorkFilter::default()
        }
    }
}

pub(crate) fn save_sidebar_work_filter(filter: crate::app::state::SidebarWorkFilter) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| state.sidebar_work_filter = Some(filter)) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

pub(crate) fn load_sidebar_group_sorts(
) -> std::collections::HashMap<String, crate::app::state::SidebarSortMode> {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state.sidebar_group_sorts.unwrap_or_default(),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            std::collections::HashMap::new()
        }
    }
}

/// A default sort is the absence of a choice, so it removes the key rather
/// than writing a value every reader must then treat as missing.
fn apply_sidebar_group_sort(
    state: &mut ClientPresentationFile,
    key: &str,
    mode: crate::app::state::SidebarSortMode,
) {
    let sorts = state
        .sidebar_group_sorts
        .get_or_insert_with(Default::default);
    if mode == crate::app::state::SidebarSortMode::Default {
        sorts.remove(key);
    } else {
        sorts.insert(key.to_string(), mode);
    }
}

pub(crate) fn save_sidebar_group_sort(key: &str, mode: crate::app::state::SidebarSortMode) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| apply_sidebar_group_sort(state, key, mode)) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

pub(crate) fn load_sidebar_group_collapsed() -> std::collections::HashMap<String, bool> {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state.sidebar_group_collapsed.unwrap_or_default(),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            std::collections::HashMap::new()
        }
    }
}

pub(crate) fn save_sidebar_group_collapsed(key: &str, collapsed: bool) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| {
        state
            .sidebar_group_collapsed
            .get_or_insert_with(Default::default)
            .insert(key.to_string(), collapsed);
    }) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

fn load_from_path(path: &Path) -> std::io::Result<ClientPresentationFile> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ClientPresentationFile::default());
        }
        Err(err) => return Err(err),
    };
    let state: ClientPresentationFile = serde_json::from_str(&content).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid state: {err}"),
        )
    })?;
    Ok(state)
}

fn update_path(
    path: &Path,
    update: impl FnOnce(&mut ClientPresentationFile),
) -> std::io::Result<()> {
    let mut state = load_from_path(path)?;
    update(&mut state);
    save_to_path(path, &state)
}

fn save_to_path(path: &Path, state: &ClientPresentationFile) -> std::io::Result<()> {
    save_json_to_path(path, state)
}

fn save_json_to_path<T: Serialize>(path: &Path, state: &T) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("client presentation path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let json = serde_json::to_string_pretty(state)?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_path = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), unique));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    if let Err(err) = file
        .write_all(json.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    drop(file);
    if let Err(err) = crate::platform::replace_file_durably(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-client-presentation-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn dock_width_round_trips_through_client_local_state() {
        let path = temp_path();
        update_path(&path, |state| state.dock_width = Some(25))
            .expect("save client presentation state");
        assert_eq!(
            load_from_path(&path)
                .expect("load client presentation state")
                .dock_width
                .map(clamp_dock_width),
            Some(25)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_or_missing_dock_width_uses_default() {
        let path = temp_path();
        assert_eq!(
            load_from_path(&path).expect("missing state").dock_width,
            None,
        );
        std::fs::write(&path, "not json").expect("write invalid state");
        assert!(load_from_path(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notepad_height_round_trips_without_erasing_dock_width() {
        let path = temp_path();
        update_path(&path, |state| state.dock_width = Some(25)).expect("save dock width");
        update_path(&path, |state| state.notepad_height = Some(12)).expect("save notepad height");
        let state = load_from_path(&path).expect("load client presentation state");
        assert_eq!(state.dock_width, Some(25));
        assert_eq!(state.notepad_height.map(clamp_notepad_height), Some(12));
        // A dragged height outside the config range clamps to it.
        update_path(&path, |state| state.notepad_height = Some(1)).expect("save");
        assert_eq!(
            load_from_path(&path)
                .expect("load")
                .notepad_height
                .map(clamp_notepad_height),
            Some(crate::notepad::MIN_HEIGHT)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_presentation_files_have_no_notepad_height() {
        let state: ClientPresentationFile =
            serde_json::from_str(r#"{"dock_width":31}"#).expect("legacy presentation state");
        assert!(state.notepad_height.is_none());
    }

    #[test]
    fn per_view_sidebar_filters_round_trip_with_the_group_mode() {
        let path = temp_path();
        let mut filters = crate::app::state::SidebarWorkFilter {
            query: "label:billing pricing".into(),
            team: Some("ENG".into()),
            assignee: None,
            ..Default::default()
        };
        filters
            .linear_statuses
            .remove(&crate::app::state::LinearStatusFilter::Done);
        filters.github.assignee = Some("grace".into());
        filters.github.show_drafts = true;
        filters.github.state = crate::app::state::GithubStateFilter::Merged;
        filters.missive.team = Some("Support".into());
        filters.missive.assignee = None;
        filters.missive.show_closed = true;
        update_path(&path, |state| {
            state.sidebar_group_mode = Some(crate::app::state::SidebarGroupMode::LinearTeam);
            state.sidebar_work_filter = Some(filters.clone());
        })
        .expect("save sidebar work filter");
        let state = load_from_path(&path).expect("load client presentation state");
        assert_eq!(
            state.sidebar_group_mode,
            Some(crate::app::state::SidebarGroupMode::LinearTeam)
        );
        assert_eq!(state.sidebar_work_filter, Some(filters));
        // A filter that narrows nothing round trips as the default, not as a
        // missing key that would resurrect an older narrowing.
        update_path(&path, |state| {
            state.sidebar_work_filter = Some(crate::app::state::SidebarWorkFilter::default());
        })
        .expect("clear sidebar work filter");
        assert_eq!(
            load_from_path(&path)
                .expect("load client presentation state")
                .sidebar_work_filter,
            Some(crate::app::state::SidebarWorkFilter::default())
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_linear_filter_gains_per_view_defaults() {
        let state: ClientPresentationFile =
            serde_json::from_str(r#"{"sidebar_work_filter":{"team":"OPS","assignee":"Ada"}}"#)
                .expect("legacy presentation state");
        let filters = state.sidebar_work_filter.expect("sidebar filters");

        assert_eq!(filters.team.as_deref(), Some("OPS"));
        assert_eq!(filters.assignee.as_deref(), Some("Ada"));
        assert!(filters.query.is_empty());
        assert_eq!(
            filters.github,
            crate::app::state::GithubSidebarFilter::default()
        );
        assert_eq!(
            filters.missive,
            crate::app::state::MissiveSidebarFilter::default()
        );
    }

    #[test]
    fn sidebar_group_mode_round_trips_without_erasing_dock_width() {
        let path = temp_path();
        update_path(&path, |state| state.dock_width = Some(25)).expect("save dock width");
        update_path(&path, |state| {
            state.sidebar_group_mode = Some(crate::app::state::SidebarGroupMode::RepoPr);
        })
        .expect("save sidebar group mode");
        let state = load_from_path(&path).expect("load client presentation state");
        assert_eq!(state.dock_width, Some(25));
        assert_eq!(
            state.sidebar_group_mode,
            Some(crate::app::state::SidebarGroupMode::RepoPr)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sidebar_group_sorts_round_trip_and_default_clears_the_key() {
        let path = temp_path();
        update_path(&path, |state| {
            apply_sidebar_group_sort(
                state,
                "repo:acme/alpha",
                crate::app::state::SidebarSortMode::Status,
            )
        })
        .expect("save sidebar group sort");
        update_path(&path, |state| state.dock_width = Some(31)).expect("save dock width");
        let state = load_from_path(&path).expect("load client presentation state");
        assert_eq!(state.dock_width, Some(31));
        assert_eq!(
            state
                .sidebar_group_sorts
                .as_ref()
                .and_then(|sorts| sorts.get("repo:acme/alpha")),
            Some(&crate::app::state::SidebarSortMode::Status),
            "the sort survives an unrelated presentation write"
        );
        // A group set back to Default drops its key instead of accumulating
        // entries every reader must special-case.
        update_path(&path, |state| {
            apply_sidebar_group_sort(
                state,
                "repo:acme/alpha",
                crate::app::state::SidebarSortMode::Default,
            )
        })
        .expect("clear sidebar group sort");
        let state = load_from_path(&path).expect("load client presentation state");
        assert!(state
            .sidebar_group_sorts
            .as_ref()
            .is_none_or(|sorts| !sorts.contains_key("repo:acme/alpha")));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn presentation_files_without_group_sorts_load_as_empty() {
        let state: ClientPresentationFile =
            serde_json::from_str(r#"{"sidebar_group_mode":"repo_pr"}"#)
                .expect("legacy presentation state");
        assert!(state.sidebar_group_sorts.is_none());
    }

    #[test]
    fn sidebar_collapse_overrides_round_trip_true_and_false_without_erasing_width() {
        let path = temp_path();
        update_path(&path, |state| {
            state.dock_width = Some(29);
            state.sidebar_group_collapsed = Some(std::collections::HashMap::from([
                ("repo:Runs".to_string(), true),
                ("repo:Settled".to_string(), false),
                ("device:main/ub2".to_string(), true),
                ("device:main/mbpro".to_string(), false),
            ]));
        })
        .expect("save sidebar collapse overrides");
        update_path(&path, |state| {
            state.sidebar_group_mode = Some(crate::app::state::SidebarGroupMode::Missive);
        })
        .expect("save unrelated presentation field");

        let state = load_from_path(&path).expect("load client presentation state");
        assert_eq!(state.dock_width, Some(29));
        assert_eq!(
            state.sidebar_group_collapsed,
            Some(std::collections::HashMap::from([
                ("repo:Runs".to_string(), true),
                ("repo:Settled".to_string(), false),
                ("device:main/ub2".to_string(), true),
                ("device:main/mbpro".to_string(), false),
            ]))
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_presentation_files_have_no_collapse_overrides() {
        let state: ClientPresentationFile =
            serde_json::from_str(r#"{"dock_width":31}"#).expect("legacy presentation state");
        assert!(state.sidebar_group_collapsed.is_none());
    }

    #[test]
    fn spawn_dock_drafts_are_isolated_by_client_and_survive_reload() {
        let root = std::env::temp_dir().join(format!(
            "herdr-spawn-dock-drafts-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let first = SpawnDockDraft {
            prompt: "first client".into(),
            project: "project-a".into(),
            ..Default::default()
        };
        let second = SpawnDockDraft {
            prompt: "second client".into(),
            project: "project-b".into(),
            ..Default::default()
        };
        let first_path = spawn_dock_draft_path_in(&root, "tty:first");
        let second_path = spawn_dock_draft_path_in(&root, "tty:second");

        save_spawn_dock_draft_to_path(&first_path, Some(first.clone()))
            .expect("save first client's draft");
        save_spawn_dock_draft_to_path(&second_path, Some(second.clone()))
            .expect("save second client's draft");

        assert_ne!(first_path, second_path);
        assert_eq!(
            load_spawn_dock_draft_from_path(&first_path)
                .expect("reload first client")
                .draft,
            Some(first)
        );
        assert_eq!(
            load_spawn_dock_draft_from_path(&second_path)
                .expect("reload second client")
                .draft,
            Some(second)
        );
        std::fs::remove_dir_all(root).expect("remove test drafts");
    }
}
