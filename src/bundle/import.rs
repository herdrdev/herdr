//! Bundle import: install agent sessions, optionally clone missing project
//! folders, and write a Herdr session that the normal snapshot restore can
//! open. Import never touches a running session and never overwrites an
//! existing saved session without `--force` (which keeps a backup).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::git_info::{self, GitInfo};
use super::manifest::{load_bundle, LoadedBundle, Manifest, ManifestAgent, ManifestV1};
use super::stores::{self, InstallPlan, StoreKind, StoreRoots};
use super::{
    copy_file, map_path, message_error, path_is_absolute_any, BundleResult, PathMap, HISTORY_FILE,
    SESSION_FILE,
};

#[derive(Debug, Clone)]
pub(crate) struct ImportOptions {
    pub(crate) maps: Vec<PathMap>,
    pub(crate) clone_missing: bool,
    pub(crate) dry_run: bool,
    pub(crate) force: bool,
}

/// The session import writes to, resolved by the CLI layer.
#[derive(Debug, Clone)]
pub(crate) struct ImportTarget {
    /// Display name (`default` for the default session).
    pub(crate) name: String,
    pub(crate) data_dir: PathBuf,
    pub(crate) running: bool,
}

#[derive(Debug)]
pub(crate) struct ImportOutcome {
    pub(crate) session_name: String,
    pub(crate) panes: usize,
    pub(crate) agents_installed: usize,
    pub(crate) agents_already_present: usize,
    /// Every planned or performed step, in order. The CLI prints these;
    /// under `--dry-run` they are the whole result.
    pub(crate) actions: Vec<String>,
    pub(crate) notes: Vec<String>,
}

struct UnifiedPane {
    workspace: String,
    tab: String,
    cwd: String,
    agent: Option<ManifestAgent>,
}

fn safe_bundle_relative(relative: &str) -> bool {
    !relative.is_empty()
        && !relative.starts_with('/')
        && !relative.starts_with('\\')
        && !path_is_absolute_any(relative)
        && !relative
            .split(['/', '\\'])
            .any(|part| part == ".." || part.is_empty())
}

fn store_for_manifest_agent(agent: &ManifestAgent) -> Option<(StoreKind, String)> {
    let value = agent.session_value.clone().or_else(|| {
        agent
            .resume_argv
            .as_ref()
            .and_then(|argv| stores::muse_session_id(argv))
    })?;
    let kind = if agent.session_kind.as_deref() == Some("path") {
        StoreKind::SessionFile
    } else {
        match agent.agent.as_str() {
            "claude" => StoreKind::Claude,
            "codex" => StoreKind::Codex,
            "muse" => StoreKind::Muse,
            _ => return None,
        }
    };
    Some((kind, value))
}

fn unified_from_v2(manifest: &Manifest) -> Vec<UnifiedPane> {
    manifest
        .panes
        .iter()
        .map(|pane| UnifiedPane {
            workspace: pane.workspace.clone(),
            tab: pane.tab.clone(),
            cwd: pane.cwd.clone(),
            agent: pane.agent.clone(),
        })
        .collect()
}

fn unified_from_v1(manifest: &ManifestV1) -> Vec<UnifiedPane> {
    manifest
        .panes
        .iter()
        .map(|pane| {
            let agent = pane.agent.as_ref().map(|agent| ManifestAgent {
                source: match agent.name.as_str() {
                    "claude" => Some("herdr:claude".to_string()),
                    "codex" => Some("herdr:codex".to_string()),
                    _ => None,
                },
                agent: agent.name.clone(),
                session_kind: Some("id".to_string()),
                session_value: Some(agent.session_id.clone()),
                resume_argv: agent.resume.clone(),
                bundle_path: agent.bundle_path.clone(),
                files_missing: None,
            });
            UnifiedPane {
                workspace: pane
                    .space
                    .clone()
                    .unwrap_or_else(|| "workspace".to_string()),
                tab: pane.tab.clone().unwrap_or_else(|| "tab".to_string()),
                cwd: pane.cwd.clone(),
                agent,
            }
        })
        .collect()
}

pub(crate) fn import_bundle(
    bundle_dir: &Path,
    target: &ImportTarget,
    roots: Option<&StoreRoots>,
    options: &ImportOptions,
) -> BundleResult<ImportOutcome> {
    if target.running {
        return Err(message_error(format!(
            "session '{}' is running; stop it before importing (import writes its saved layout)",
            target.name
        )));
    }
    let target_session = target.data_dir.join(SESSION_FILE);
    if target_session.exists() && !options.force && !options.dry_run {
        return Err(message_error(format!(
            "session '{}' already has a saved layout at {}; import into a new session with --name, or use --force to replace it (a backup is kept)",
            target.name,
            target_session.display()
        )));
    }

    let loaded = load_bundle(bundle_dir)?;
    let (panes, folders, has_persisted_session) = match &loaded {
        LoadedBundle::V2(manifest) => {
            let folders: BTreeMap<String, Option<GitInfo>> = manifest
                .folders
                .iter()
                .map(|(cwd, info)| (cwd.clone(), info.git.clone()))
                .collect();
            (
                unified_from_v2(manifest),
                folders,
                bundle_dir.join(SESSION_FILE).is_file(),
            )
        }
        LoadedBundle::V1(manifest) => {
            let folders: BTreeMap<String, Option<GitInfo>> = manifest
                .folders
                .iter()
                .map(|(cwd, info)| (cwd.clone(), info.git.clone()))
                .collect();
            (unified_from_v1(manifest), folders, false)
        }
    };

    let mut outcome = ImportOutcome {
        session_name: target.name.clone(),
        panes: panes.len(),
        agents_installed: 0,
        agents_already_present: 0,
        actions: Vec::new(),
        notes: Vec::new(),
    };
    if matches!(loaded, LoadedBundle::V1(_)) {
        outcome.notes.push(
            "schema 1 bundle: the layout is rebuilt from its pane list (one tab per original tab, panes side by side); exact split ratios were not recorded in a form this Herdr can replay"
                .to_string(),
        );
    }

    install_agents(bundle_dir, &panes, roots, options, &mut outcome);
    prepare_folders(&panes, &folders, options, &mut outcome);

    let (session_bytes, history_kept) = if has_persisted_session {
        transform_persisted_session(bundle_dir, options)?
    } else {
        (generate_session(&panes, options)?, false)
    };

    if target_session.exists() && options.force {
        let backup = target
            .data_dir
            .join(format!("session.json.bak-bundle-{}", unix_seconds()));
        outcome.actions.push(format!(
            "back up existing saved layout -> {}",
            backup.display()
        ));
        if !options.dry_run {
            copy_file(&target_session, &backup)?;
        }
    }
    outcome.actions.push(format!(
        "write session '{}' layout -> {}",
        target.name,
        target_session.display()
    ));
    if history_kept {
        outcome.actions.push(format!(
            "copy pane screen history -> {}",
            target.data_dir.join(HISTORY_FILE).display()
        ));
    }
    if !options.dry_run {
        std::fs::create_dir_all(&target.data_dir)?;
        write_atomic(&target_session, &session_bytes)?;
        if history_kept {
            copy_file(
                &bundle_dir.join(HISTORY_FILE),
                &target.data_dir.join(HISTORY_FILE),
            )?;
        }
    }
    Ok(outcome)
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> BundleResult<()> {
    let tmp = path.with_extension("json.bundle-tmp");
    std::fs::write(&tmp, bytes)?;
    if let Err(error) = crate::platform::replace_file(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

fn install_agents(
    bundle_dir: &Path,
    panes: &[UnifiedPane],
    roots: Option<&StoreRoots>,
    options: &ImportOptions,
    outcome: &mut ImportOutcome,
) {
    // Two panes can reference one session (the bundle holds one copy);
    // install each bundled copy once.
    let mut installed_paths: HashSet<String> = HashSet::new();
    for pane in panes {
        let Some(agent) = &pane.agent else {
            continue;
        };
        let Some(bundle_relative) = &agent.bundle_path else {
            continue;
        };
        if !installed_paths.insert(bundle_relative.clone()) {
            continue;
        }
        if !safe_bundle_relative(bundle_relative) {
            outcome.notes.push(format!(
                "skipped unsafe bundle path for {} session: {bundle_relative}",
                agent.agent
            ));
            continue;
        }
        let Some((kind, value)) = store_for_manifest_agent(agent) else {
            outcome.notes.push(format!(
                "{}: no local store is known for {}; session files in the bundle were not installed",
                pane.cwd, agent.agent
            ));
            continue;
        };
        let Some(roots) = roots else {
            outcome.notes.push(
                "no home directory found; agent session files were not installed".to_string(),
            );
            return;
        };
        let mapped_cwd = map_path(&pane.cwd, &options.maps);
        let mapped_value = if kind == StoreKind::SessionFile {
            map_path(&value, &options.maps)
        } else {
            value.clone()
        };
        if kind == StoreKind::SessionFile && !Path::new(&mapped_value).is_absolute() {
            outcome.notes.push(format!(
                "{}: session path '{mapped_value}' is not a path on this OS; re-run with --map to place it",
                agent.agent
            ));
            continue;
        }
        let Some(plan) = stores::install_plan(
            kind,
            bundle_relative,
            &value,
            &mapped_cwd,
            &mapped_value,
            roots,
            bundle_dir,
        ) else {
            outcome.notes.push(format!(
                "{}: bundled session files missing at {bundle_relative}",
                agent.agent
            ));
            continue;
        };
        match &plan {
            InstallPlan::AlreadyPresent { target } => {
                outcome.agents_already_present += 1;
                outcome.actions.push(format!(
                    "skip {} session {value} (already present at {})",
                    agent.agent,
                    target.display()
                ));
            }
            InstallPlan::Install { target, .. } => {
                outcome.actions.push(format!(
                    "install {} session {value} -> {}",
                    agent.agent,
                    target.display()
                ));
                if !options.dry_run {
                    match stores::execute_install(&plan, bundle_dir) {
                        Ok(()) => outcome.agents_installed += 1,
                        Err(error) => outcome.notes.push(format!(
                            "{}: installing session files failed: {error}",
                            agent.agent
                        )),
                    }
                } else {
                    outcome.agents_installed += 1;
                }
            }
        }
    }
}

fn prepare_folders(
    panes: &[UnifiedPane],
    folders: &BTreeMap<String, Option<GitInfo>>,
    options: &ImportOptions,
    outcome: &mut ImportOutcome,
) {
    let mut seen: HashSet<String> = HashSet::new();
    for pane in panes {
        if pane.cwd.is_empty() || !seen.insert(pane.cwd.clone()) {
            continue;
        }
        let mapped = map_path(&pane.cwd, &options.maps);
        if Path::new(&mapped).is_dir() {
            continue;
        }
        let git = folders.get(&pane.cwd).cloned().flatten();
        match (options.clone_missing, git.as_ref().and_then(|git| git.origin.as_ref())) {
            (true, Some(origin)) => {
                let head = git.as_ref().and_then(|git| git.head.as_deref());
                outcome.actions.push(format!(
                    "clone {origin} -> {mapped}{}",
                    head.map(|head| format!(" @ {}", &head[..head.len().min(12)]))
                        .unwrap_or_default()
                ));
                if !options.dry_run {
                    if let Err(error) = git_info::clone_at(origin, Path::new(&mapped), head) {
                        outcome.notes.push(format!("{mapped}: {error}"));
                    }
                }
            }
            _ => outcome.notes.push(format!(
                "folder missing: {mapped} (use --map OLD=NEW to point at its location here, or --clone-missing to clone the recorded remote)"
            )),
        }
        if let Some(git) = &git {
            if !git.dirty.is_empty() {
                outcome.notes.push(format!(
                    "{} had {} uncommitted change(s) on the source machine; bundles list them in manifest.json but do not pack them",
                    pane.cwd,
                    git.dirty.len()
                ));
            }
        }
    }
}

/// Read the bundle's saved layout, rewrite its paths, and re-serialize it.
/// Returns the bytes to write and whether the bundled pane history still
/// matches the (possibly rewritten) layout.
fn transform_persisted_session(
    bundle_dir: &Path,
    options: &ImportOptions,
) -> BundleResult<(Vec<u8>, bool)> {
    let content = std::fs::read_to_string(bundle_dir.join(SESSION_FILE))?;
    let mut snapshot = crate::persist::parse_snapshot(&content)
        .map_err(|error| message_error(format!("bundled session is unreadable: {error}")))?;
    let mut changed = false;
    let rewrite = |value: &str, changed: &mut bool| -> String {
        let mapped = map_path(value, &options.maps);
        if mapped != value {
            *changed = true;
        }
        mapped
    };
    for workspace in &mut snapshot.workspaces {
        let identity = workspace.identity_cwd.display().to_string();
        workspace.identity_cwd = PathBuf::from(rewrite(&identity, &mut changed));
        if let Some(space) = &mut workspace.worktree_space {
            let root = space.repo_root.display().to_string();
            space.repo_root = PathBuf::from(rewrite(&root, &mut changed));
            let checkout = space.checkout_path.display().to_string();
            space.checkout_path = PathBuf::from(rewrite(&checkout, &mut changed));
        }
        for tab in &mut workspace.tabs {
            for pane in tab.panes.values_mut() {
                let cwd = pane.cwd.display().to_string();
                pane.cwd = PathBuf::from(rewrite(&cwd, &mut changed));
                if let Some(session) = &mut pane.agent_session {
                    if session.kind == crate::agent_resume::AgentSessionRefKind::Path {
                        session.value = rewrite(&session.value.clone(), &mut changed);
                    }
                }
                if let Some(resume) = &mut pane.agent_resume {
                    for arg in &mut resume.argv {
                        if path_is_absolute_any(arg) {
                            *arg = rewrite(&arg.clone(), &mut changed);
                        }
                    }
                }
            }
        }
    }
    let bytes = if changed {
        serde_json::to_string_pretty(&snapshot)
            .map_err(|error| super::BundleError::Io(std::io::Error::other(error)))?
            .into_bytes()
    } else {
        content.into_bytes()
    };
    let history_kept = !changed && bundle_dir.join(HISTORY_FILE).is_file();
    Ok((bytes, history_kept))
}

/// Build a fresh saved layout from a flat pane list (schema 1 bundles and
/// read-only remote captures). Workspaces and tabs follow first-seen order;
/// panes in one tab sit side by side in a balanced split tree.
fn generate_session(panes: &[UnifiedPane], options: &ImportOptions) -> BundleResult<Vec<u8>> {
    use crate::persist::{
        PaneSnapshot, SessionSnapshot, TabSnapshot, WorkspaceSnapshot, SNAPSHOT_VERSION,
    };

    let mut workspace_order: Vec<String> = Vec::new();
    let mut grouped: BTreeMap<String, Vec<(String, Vec<&UnifiedPane>)>> = BTreeMap::new();
    let mut by_workspace: HashMap<String, Vec<String>> = HashMap::new();
    for pane in panes {
        if !by_workspace.contains_key(&pane.workspace) {
            workspace_order.push(pane.workspace.clone());
            by_workspace.insert(pane.workspace.clone(), Vec::new());
            grouped.insert(pane.workspace.clone(), Vec::new());
        }
        let tabs = grouped
            .get_mut(&pane.workspace)
            .ok_or_else(|| message_error("internal grouping error"))?;
        if !by_workspace
            .get(&pane.workspace)
            .is_some_and(|known| known.contains(&pane.tab))
        {
            by_workspace
                .get_mut(&pane.workspace)
                .ok_or_else(|| message_error("internal grouping error"))?
                .push(pane.tab.clone());
            tabs.push((pane.tab.clone(), Vec::new()));
        }
        let tab = tabs
            .iter_mut()
            .find(|(name, _)| name == &pane.tab)
            .ok_or_else(|| message_error("internal grouping error"))?;
        tab.1.push(pane);
    }

    let mut workspaces = Vec::new();
    for workspace_key in &workspace_order {
        let tabs_grouped = grouped
            .remove(workspace_key)
            .ok_or_else(|| message_error("internal grouping error"))?;
        let mut public_pane_numbers: HashMap<u32, usize> = HashMap::new();
        let mut next_pane_number = 1usize;
        let mut next_pane_id = 1u32;
        let mut tabs = Vec::new();
        let mut identity_cwd: Option<PathBuf> = None;
        for (_, tab_panes) in &tabs_grouped {
            let mut pane_map: HashMap<u32, PaneSnapshot> = HashMap::new();
            let mut ids = Vec::new();
            for pane in tab_panes {
                let id = next_pane_id;
                next_pane_id += 1;
                public_pane_numbers.insert(id, next_pane_number);
                next_pane_number += 1;
                ids.push(id);
                let mapped_cwd = map_path(&pane.cwd, &options.maps);
                if identity_cwd.is_none() {
                    identity_cwd = Some(PathBuf::from(&mapped_cwd));
                }
                let (agent_session, agent_resume) = generated_agent_refs(pane, options);
                pane_map.insert(
                    id,
                    PaneSnapshot {
                        cwd: PathBuf::from(mapped_cwd),
                        label: None,
                        agent_name: None,
                        managed_agent_kind: None,
                        agent_session,
                        agent_resume,
                        launch_argv: None,
                    },
                );
            }
            let first = ids.first().copied();
            tabs.push(TabSnapshot {
                custom_name: None,
                layout: build_layout(&ids).ok_or_else(|| message_error("tab without panes"))?,
                panes: pane_map,
                zoomed: false,
                focused: first,
                root_pane: first,
            });
        }
        let tab_count = tabs.len();
        workspaces.push(WorkspaceSnapshot {
            id: None,
            custom_name: None,
            identity_cwd: identity_cwd.unwrap_or_else(|| PathBuf::from(".")),
            worktree_space: None,
            public_pane_numbers,
            next_public_pane_number: next_pane_number,
            public_tab_numbers: (1..=tab_count).collect(),
            next_public_tab_number: tab_count + 1,
            tabs,
            active_tab: 0,
        });
    }

    let snapshot = SessionSnapshot {
        version: SNAPSHOT_VERSION,
        active: (!workspaces.is_empty()).then_some(0),
        selected: 0,
        workspaces,
        sidebar_width: None,
        sidebar_section_split: None,
        collapsed_space_keys: HashSet::new(),
    };
    serde_json::to_string_pretty(&snapshot)
        .map(String::into_bytes)
        .map_err(|error| super::BundleError::Io(std::io::Error::other(error)))
}

fn build_layout(ids: &[u32]) -> Option<crate::persist::LayoutSnapshot> {
    use crate::persist::{DirectionSnapshot, LayoutSnapshot};
    match ids {
        [] => None,
        [only] => Some(LayoutSnapshot::Pane(*only)),
        _ => {
            let mid = ids.len() / 2;
            Some(LayoutSnapshot::Split {
                direction: DirectionSnapshot::Horizontal,
                ratio: mid as f32 / ids.len() as f32,
                first: Box::new(build_layout(&ids[..mid])?),
                second: Box::new(build_layout(&ids[mid..])?),
            })
        }
    }
}

/// Official session references win over reported resume commands because
/// restore de-duplicates and validates them; a reported command is the
/// fallback for agents such as Muse that have no official reference.
fn generated_agent_refs(
    pane: &UnifiedPane,
    options: &ImportOptions,
) -> (
    Option<crate::persist::PaneAgentSessionSnapshot>,
    Option<crate::persist::PaneAgentResumeSnapshot>,
) {
    let Some(agent) = &pane.agent else {
        return (None, None);
    };
    if let (Some(source), Some(kind_name), Some(value)) = (
        agent.source.as_ref(),
        agent.session_kind.as_ref(),
        agent.session_value.as_ref(),
    ) {
        let kind = match kind_name.as_str() {
            "id" => crate::agent_resume::AgentSessionRefKind::Id,
            "path" => crate::agent_resume::AgentSessionRefKind::Path,
            _ => return (None, generated_resume(agent, options)),
        };
        let value = if kind == crate::agent_resume::AgentSessionRefKind::Path {
            map_path(value, &options.maps)
        } else {
            value.clone()
        };
        if crate::agent_resume::session_ref_from_snapshot(source, &agent.agent, kind, &value)
            .is_some()
        {
            return (
                Some(crate::persist::PaneAgentSessionSnapshot {
                    source: source.clone(),
                    agent: agent.agent.clone(),
                    kind,
                    value,
                }),
                None,
            );
        }
    }
    (None, generated_resume(agent, options))
}

fn generated_resume(
    agent: &ManifestAgent,
    options: &ImportOptions,
) -> Option<crate::persist::PaneAgentResumeSnapshot> {
    let argv: Vec<String> = agent
        .resume_argv
        .as_ref()?
        .iter()
        .map(|arg| {
            if path_is_absolute_any(arg) {
                map_path(arg, &options.maps)
            } else {
                arg.clone()
            }
        })
        .collect();
    crate::agent_resume::validate_resume_argv(&argv).ok()?;
    Some(crate::persist::PaneAgentResumeSnapshot {
        source: agent
            .source
            .clone()
            .unwrap_or_else(|| "herdr-bundle".to_string()),
        agent: agent.agent.clone(),
        argv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(maps: Vec<PathMap>) -> ImportOptions {
        ImportOptions {
            maps,
            clone_missing: false,
            dry_run: false,
            force: false,
        }
    }

    fn target(dir: PathBuf) -> ImportTarget {
        ImportTarget {
            name: "imported".to_string(),
            data_dir: dir,
            running: false,
        }
    }

    fn write_v1_bundle(bundle: &Path, source_cwd: &str) {
        let manifest = serde_json::json!({
            "kind": "herdr-bundle",
            "schema_version": 1,
            "home": "/home/source",
            "panes": [{
                "address": "t1", "space": "sp1", "tab": "tb1", "kind": "agent",
                "cwd": source_cwd,
                "agent": {
                    "name": "muse", "session_id": "muse-1",
                    "store_relpath": "2026/11/07",
                    "bundle_path": "agents/muse/2026/11/07/muse-1",
                    "resume": ["muse", "resume", "muse-1"]
                }
            }, {
                "address": "t2", "space": "sp1", "tab": "tb1", "kind": "shell",
                "cwd": source_cwd, "agent": null
            }],
            "folders": {}
        });
        std::fs::write(
            bundle.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let session = bundle.join("agents/muse/2026/11/07/muse-1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("session.jsonl"), "{\"a\":1}\n").unwrap();
    }

    #[test]
    fn v1_import_installs_muse_and_generates_restorable_session() {
        let bundle = super::super::test_temp_dir("import-v1-bundle");
        let home = super::super::test_temp_dir("import-v1-home");
        let app = home.join("app");
        std::fs::create_dir_all(&app).unwrap();
        write_v1_bundle(&bundle, "/home/source/app");
        let roots = StoreRoots::for_home(home.clone());
        let target_dir = super::super::test_temp_dir("import-v1-target");
        let maps = vec![PathMap {
            old: "/home/source".to_string(),
            new: home.display().to_string(),
        }];
        let outcome = import_bundle(
            &bundle,
            &target(target_dir.clone()),
            Some(&roots),
            &options(maps),
        )
        .unwrap();
        assert_eq!(outcome.panes, 2);
        assert_eq!(outcome.agents_installed, 1);
        assert!(roots.muse_roots[0]
            .join("2026/11/07/muse-1/session.jsonl")
            .is_file());
        let written = std::fs::read_to_string(target_dir.join(SESSION_FILE)).unwrap();
        let snapshot = crate::persist::parse_snapshot(&written).unwrap();
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.workspaces[0].tabs.len(), 1);
        let panes = &snapshot.workspaces[0].tabs[0].panes;
        assert_eq!(panes.len(), 2);
        let agent_pane = panes
            .values()
            .find(|pane| pane.agent_resume.is_some())
            .expect("muse pane keeps its reported resume command");
        assert_eq!(
            agent_pane.agent_resume.as_ref().unwrap().argv,
            vec!["muse", "resume", "muse-1"]
        );
        assert_eq!(agent_pane.cwd, app);
        for dir in [bundle, home, target_dir] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn dry_run_writes_nothing() {
        let bundle = super::super::test_temp_dir("import-dry-bundle");
        write_v1_bundle(&bundle, "/home/source/app");
        let home = super::super::test_temp_dir("import-dry-home");
        let roots = StoreRoots::for_home(home.clone());
        let target_dir = home.join("session-target");
        let mut opts = options(Vec::new());
        opts.dry_run = true;
        let outcome =
            import_bundle(&bundle, &target(target_dir.clone()), Some(&roots), &opts).unwrap();
        assert!(!outcome.actions.is_empty());
        assert!(!target_dir.exists());
        assert!(!roots.muse_roots[0].join("2026/11/07/muse-1").exists());
        for dir in [bundle, home] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn import_refuses_running_or_existing_session() {
        let bundle = super::super::test_temp_dir("import-guard-bundle");
        write_v1_bundle(&bundle, "/home/source/app");
        let target_dir = super::super::test_temp_dir("import-guard-target");
        let mut running = target(target_dir.clone());
        running.running = true;
        assert!(import_bundle(&bundle, &running, None, &options(Vec::new())).is_err());
        std::fs::write(target_dir.join(SESSION_FILE), "{}").unwrap();
        assert!(import_bundle(
            &bundle,
            &target(target_dir.clone()),
            None,
            &options(Vec::new())
        )
        .is_err());
        let mut forced = options(Vec::new());
        forced.force = true;
        // With --force the placeholder is backed up, then replaced.
        let outcome = import_bundle(&bundle, &target(target_dir.clone()), None, &forced).unwrap();
        assert!(outcome
            .actions
            .iter()
            .any(|action| action.contains("back up existing")));
        assert!(std::fs::read_dir(&target_dir)
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("session.json.bak-bundle-")));
        for dir in [bundle, target_dir] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn unsafe_bundle_paths_are_rejected() {
        assert!(!safe_bundle_relative("../escape.jsonl"));
        assert!(!safe_bundle_relative("/absolute.jsonl"));
        assert!(!safe_bundle_relative("agents//claude/x.jsonl"));
        assert!(safe_bundle_relative("agents/claude/-home-app/id.jsonl"));
    }
}
